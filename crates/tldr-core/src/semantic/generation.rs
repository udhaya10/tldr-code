//! redb-authoritative publication of immutable usearch generations.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use fs2::FileExt;
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};

use super::redb_store::{
    GenerationState, GenerationVectorWrite, RedbStore, StoredGeneration, DEFAULT_REDB_CACHE_BYTES,
};
use super::vector_store::{
    activate_current, decode_binary, encode_binary, next_generation, ChunkMeta, FileRecord,
    ManifestId, VectorStore,
};
use crate::{TldrError, TldrResult};

const LEDGER_FILE: &str = "generations.redb";
const RECORD_BATCH: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublicationFault {
    Stage,
    Records,
    Artifact,
    Activation,
}

#[derive(Archive, RkyvDeserialize, RkyvSerialize)]
struct GenerationFiles {
    records: HashMap<String, FileRecord>,
}

struct OwnedVector {
    key: u64,
    vector: Vec<f32>,
    metadata: Vec<u8>,
}

/// Coordinates authoritative redb state and its derived usearch artifact.
pub struct GenerationManager {
    directory: PathBuf,
    ledger: RedbStore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PublicationTimings {
    pub stage_and_records_ms: u64,
    pub verification_ms: u64,
    pub activation_ms: u64,
}

impl GenerationManager {
    /// Open the ledger stored beside the derived usearch files.
    pub fn open(directory: &Path) -> TldrResult<Self> {
        std::fs::create_dir_all(directory)?;
        Ok(Self {
            directory: directory.to_path_buf(),
            ledger: RedbStore::open(&directory.join(LEDGER_FILE), DEFAULT_REDB_CACHE_BYTES)?,
        })
    }

    /// Active complete semantic generation, if one has published.
    pub fn active_generation(&self) -> TldrResult<Option<u64>> {
        self.ledger.active_generation()
    }

    /// Stage, verify, and atomically publish a new generation.
    pub fn publish(&self, store: &VectorStore, identity: &ManifestId) -> TldrResult<u64> {
        self.publish_measured(store, identity)
            .map(|(generation, _)| generation)
    }

    pub(crate) fn publish_measured(
        &self,
        store: &VectorStore,
        identity: &ManifestId,
    ) -> TldrResult<(u64, PublicationTimings)> {
        self.publish_with_fault_measured(store, identity, None)
    }

    #[allow(dead_code)]
    pub(crate) fn publish_with_fault(
        &self,
        store: &VectorStore,
        identity: &ManifestId,
        fault: Option<PublicationFault>,
    ) -> TldrResult<u64> {
        self.publish_with_fault_measured(store, identity, fault)
            .map(|(generation, _)| generation)
    }

    fn publish_with_fault_measured(
        &self,
        store: &VectorStore,
        identity: &ManifestId,
        fault: Option<PublicationFault>,
    ) -> TldrResult<(u64, PublicationTimings)> {
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(self.directory.join("lock"))?;
        lock.lock_exclusive()?;
        let stage_started = Instant::now();
        let mut generation = next_generation(&self.directory)?;
        while self.ledger.generation(generation)?.is_some() {
            generation = generation
                .checked_add(1)
                .ok_or_else(|| generation_error("generation counter overflow"))?;
        }
        let manifest_identity = encode_binary(identity)?;
        let files = encode_binary(&GenerationFiles {
            records: store.files_snapshot(),
        })?;
        self.ledger.stage_generation(&StoredGeneration {
            generation,
            state: GenerationState::Staged,
            chunk_count: store.len() as u64,
            dimensions: store.dimensions() as u32,
            manifest_identity,
            corpus_digest: store.corpus_digest(),
            files,
        })?;
        inject(fault, PublicationFault::Stage)?;

        let mut batch = Vec::with_capacity(RECORD_BATCH);
        store.visit_records(|key, vector, metadata| {
            batch.push(OwnedVector {
                key,
                vector: vector.to_vec(),
                metadata: encode_binary(metadata)?,
            });
            if batch.len() == RECORD_BATCH {
                self.flush(generation, store.dimensions(), &batch)?;
                batch.clear();
            }
            Ok(())
        })?;
        self.flush(generation, store.dimensions(), &batch)?;
        inject(fault, PublicationFault::Records)?;
        let stage_and_records_ms = stage_started.elapsed().as_millis() as u64;

        let verification_started = Instant::now();
        self.write_artifact(store, identity, generation)?;
        inject(fault, PublicationFault::Artifact)?;
        let verification_ms = verification_started.elapsed().as_millis() as u64;
        let activation_started = Instant::now();
        self.ledger.complete_and_activate_generation(generation)?;
        inject(fault, PublicationFault::Activation)?;

        // Backward-compatible mirror only. Readers in this module trust redb.
        activate_current(&self.directory, generation)?;
        let activation_ms = activation_started.elapsed().as_millis() as u64;
        Ok((
            generation,
            PublicationTimings {
                stage_and_records_ms,
                verification_ms,
                activation_ms,
            },
        ))
    }

    fn flush(&self, generation: u64, dimensions: usize, batch: &[OwnedVector]) -> TldrResult<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let writes: Vec<_> = batch
            .iter()
            .map(|record| GenerationVectorWrite {
                key: record.key,
                vector: &record.vector,
                metadata: &record.metadata,
            })
            .collect();
        self.ledger
            .put_generation_vectors(generation, dimensions, &writes)
    }

    fn write_artifact(
        &self,
        store: &VectorStore,
        identity: &ManifestId,
        generation: u64,
    ) -> TldrResult<()> {
        store.write_generation(&self.directory, identity, generation)?;
        VectorStore::load_specific_generation(&self.directory, generation, identity).map(|_| ())
    }

    /// Load exactly the redb-active generation. Rebuild a missing/corrupt
    /// usearch artifact deterministically from authoritative redb records.
    pub fn load(&self, identity: &ManifestId) -> TldrResult<VectorStore> {
        let Some(generation) = self.ledger.active_generation()? else {
            // One-time compatibility path for stores created before TLDR-9bxa.9.
            let store = VectorStore::load(&self.directory, identity)?;
            self.publish(&store, identity)?;
            return Ok(store);
        };
        let record = self
            .ledger
            .generation(generation)?
            .ok_or_else(|| generation_error("active generation metadata is missing"))?;
        if record.state != GenerationState::Complete
            || record.dimensions != identity.dimensions
            || record.manifest_identity != encode_binary(identity)?
        {
            return Err(generation_error(
                "active generation model, metric, dimensions, or pipeline identity mismatch",
            ));
        }
        match VectorStore::load_specific_generation(&self.directory, generation, identity) {
            Ok(store) if store.len() as u64 == record.chunk_count => Ok(store),
            _ => self.rebuild_artifact(record, identity),
        }
    }

    fn rebuild_artifact(
        &self,
        record: StoredGeneration,
        identity: &ManifestId,
    ) -> TldrResult<VectorStore> {
        let vectors = self
            .ledger
            .generation_vectors(record.generation, record.dimensions as usize)?;
        if vectors.len() as u64 != record.chunk_count {
            return Err(generation_error(
                "authoritative generation record count mismatch",
            ));
        }
        let records = vectors
            .into_iter()
            .map(|vector| {
                let metadata = decode_binary::<ChunkMeta>(&vector.metadata)?;
                Ok((vector.key, vector.vector, metadata))
            })
            .collect::<TldrResult<Vec<_>>>()?;
        let files: GenerationFiles = decode_binary(&record.files)?;
        let store = VectorStore::from_generation_records(
            record.dimensions as usize,
            records,
            files.records,
            record.corpus_digest,
        )?;
        self.write_artifact(&store, identity, record.generation)?;
        Ok(store)
    }

    /// Switch back to the retained previous complete generation.
    pub fn rollback(&self, identity: &ManifestId) -> TldrResult<Option<VectorStore>> {
        self.select_previous(identity)
    }

    /// Select a retained complete generation by number.
    pub fn select(&self, generation: u64, identity: &ManifestId) -> TldrResult<VectorStore> {
        let record = self
            .ledger
            .generation(generation)?
            .ok_or_else(|| generation_error(format!("generation {generation} is missing")))?;
        if record.state != GenerationState::Complete
            || record.dimensions != identity.dimensions
            || record.manifest_identity != encode_binary(identity)?
        {
            return Err(generation_error(
                "selected generation is incomplete or incompatible",
            ));
        }
        self.ledger.select_complete_generation(generation)?;
        activate_current(&self.directory, generation)?;
        self.load(identity)
    }

    /// Select the retained previous generation without discarding rollback.
    pub fn select_previous(&self, identity: &ManifestId) -> TldrResult<Option<VectorStore>> {
        let Some(generation) = self.ledger.previous_generation()? else {
            return Ok(None);
        };
        self.select(generation, identity).map(Some)
    }
}

fn inject(actual: Option<PublicationFault>, expected: PublicationFault) -> TldrResult<()> {
    if actual == Some(expected) {
        Err(generation_error(format!(
            "injected publication failure at {expected:?}"
        )))
    } else {
        Ok(())
    }
}

fn generation_error(error: impl std::fmt::Display) -> TldrError {
    TldrError::Embedding(format!("semantic generation: {error}"))
}

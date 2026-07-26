//! redb-authoritative publication of immutable usearch generations.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::redb_store::{
    GenerationState, GenerationVectorWrite, RedbStore, StoredGeneration, DEFAULT_REDB_CACHE_BYTES,
};
use super::vector_store::{
    activate_current, next_generation, ChunkMeta, FileRecord, ManifestId, VectorStore,
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

#[derive(Serialize, Deserialize)]
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

impl GenerationManager {
    /// Open the ledger stored beside the derived usearch files.
    pub fn open(directory: &Path) -> TldrResult<Self> {
        std::fs::create_dir_all(directory)?;
        Ok(Self {
            directory: directory.to_path_buf(),
            ledger: RedbStore::open(&directory.join(LEDGER_FILE), DEFAULT_REDB_CACHE_BYTES)?,
        })
    }

    /// Stage, verify, and atomically publish a new generation.
    pub fn publish(&self, store: &VectorStore, identity: &ManifestId) -> TldrResult<u64> {
        self.publish_with_fault(store, identity, None)
    }

    pub(crate) fn publish_with_fault(
        &self,
        store: &VectorStore,
        identity: &ManifestId,
        fault: Option<PublicationFault>,
    ) -> TldrResult<u64> {
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(self.directory.join("lock"))?;
        lock.lock_exclusive()?;
        let mut generation = next_generation(&self.directory)?;
        while self.ledger.generation(generation)?.is_some() {
            generation = generation
                .checked_add(1)
                .ok_or_else(|| generation_error("generation counter overflow"))?;
        }
        let manifest_identity = serde_json::to_vec(identity).map_err(generation_error)?;
        let files = serde_json::to_vec(&GenerationFiles {
            records: store.files_snapshot(),
        })
        .map_err(generation_error)?;
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
                metadata: serde_json::to_vec(metadata).map_err(generation_error)?,
            });
            if batch.len() == RECORD_BATCH {
                self.flush(generation, store.dimensions(), &batch)?;
                batch.clear();
            }
            Ok(())
        })?;
        self.flush(generation, store.dimensions(), &batch)?;
        inject(fault, PublicationFault::Records)?;

        self.write_artifact(store, identity, generation)?;
        inject(fault, PublicationFault::Artifact)?;
        self.ledger.complete_and_activate_generation(generation)?;
        inject(fault, PublicationFault::Activation)?;

        // Backward-compatible mirror only. Readers in this module trust redb.
        activate_current(&self.directory, generation)?;
        Ok(generation)
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
            || record.manifest_identity != serde_json::to_vec(identity).map_err(generation_error)?
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
                let metadata = serde_json::from_slice::<ChunkMeta>(&vector.metadata)
                    .map_err(generation_error)?;
                Ok((vector.key, vector.vector, metadata))
            })
            .collect::<TldrResult<Vec<_>>>()?;
        let files: GenerationFiles =
            serde_json::from_slice(&record.files).map_err(generation_error)?;
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
        let Some(generation) = self.ledger.rollback_generation()? else {
            return Ok(None);
        };
        activate_current(&self.directory, generation)?;
        self.load(identity).map(Some)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::lineage::{ChunkId, ChunkRevision, StructuralAnchor};
    use crate::semantic::types::ChunkStructure;

    fn identity(root: &Path, dimensions: u32) -> ManifestId {
        ManifestId {
            embedding_model: "test".into(),
            model_revision: "1".into(),
            dimensions,
            metric: "cos".into(),
            scalar_kind: "f32".into(),
            search_mode: "exact".into(),
            embed_schema: "raw".into(),
            chunk_params: "test".into(),
            walker_version: "test".into(),
            root: root.display().to_string(),
        }
    }

    fn store(seed: u8) -> VectorStore {
        let mut store = VectorStore::new(4, 1).unwrap();
        store
            .add(
                seed as u64,
                &[seed as f32, 0.0, 0.0, 0.0],
                ChunkMeta {
                    identity: format!("{seed:02x}"),
                    chunk_id: ChunkId(seed as u128),
                    revision: ChunkRevision([seed; 32]),
                    anchor: StructuralAnchor::default(),
                    file_rel_path: "src/lib.rs".into(),
                    function_name: Some("f".into()),
                    class_name: None,
                    line_start: 1,
                    line_end: 1,
                    content_hash: format!("{seed:02x}"),
                    structure: ChunkStructure::default(),
                },
            )
            .unwrap();
        store
    }

    #[test]
    fn publication_failures_never_expose_a_mixed_generation() {
        let directory = tempfile::tempdir().unwrap();
        let mut manager = GenerationManager::open(directory.path()).unwrap();
        let id = identity(directory.path(), 4);
        manager.publish(&store(1), &id).unwrap();
        for fault in [
            PublicationFault::Stage,
            PublicationFault::Records,
            PublicationFault::Artifact,
            PublicationFault::Activation,
        ] {
            assert!(manager
                .publish_with_fault(&store(2), &id, Some(fault))
                .is_err());
            drop(manager);
            manager = GenerationManager::open(directory.path()).unwrap();
            let loaded = manager.load(&id).unwrap();
            assert!(loaded.contains(1) || loaded.contains(2));
            assert_eq!(loaded.len(), 1);
        }
    }

    #[test]
    fn corrupt_derived_index_rebuilds_from_redb_and_rollback_is_explicit() {
        let directory = tempfile::tempdir().unwrap();
        let manager = GenerationManager::open(directory.path()).unwrap();
        let id = identity(directory.path(), 4);
        let first = manager.publish(&store(1), &id).unwrap();
        let second = manager.publish(&store(2), &id).unwrap();
        std::fs::write(
            directory.path().join(format!("index.{second}.usearch")),
            b"corrupt",
        )
        .unwrap();
        assert!(manager.load(&id).unwrap().contains(2));
        assert!(manager.rollback(&id).unwrap().unwrap().contains(1));
        assert_ne!(first, second);
        std::fs::write(directory.path().join("unrelated.keep"), b"keep").unwrap();
        assert!(directory.path().join("unrelated.keep").exists());
    }
}

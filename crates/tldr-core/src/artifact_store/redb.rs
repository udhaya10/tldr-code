//! Transactional redb implementation of the shared artifact store.

use std::path::{Path, PathBuf};

use redb::{Database, Durability, ReadableDatabase, ReadableTable};
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};

use crate::{TldrError, TldrResult};

use super::schema::{
    ACTIVE_GENERATION_KEY, ARTIFACTS, ARTIFACT_DEPS, DEFAULT_CACHE_BYTES, GENERATIONS,
    GENERATION_ARTIFACTS, JOBS, METADATA, PREVIOUS_GENERATION_KEY, SCHEMA_KEY, STORE_SCHEMA,
};
use super::{
    ArtifactBatch, ArtifactEnvelope, ArtifactKey, ArtifactKind, GenerationManifest, IngestionJob,
};

/// Durable operations used by ingestion and query projections.
pub trait ArtifactStore: Send + Sync {
    /// Atomically selected generation.
    fn active_generation(&self) -> TldrResult<Option<u64>>;
    /// Previous complete generation retained for rollback.
    fn previous_generation(&self) -> TldrResult<Option<u64>>;
    /// Read one artifact by complete key.
    fn artifact(&self, key: &ArtifactKey) -> TldrResult<Option<ArtifactEnvelope>>;
    /// Read artifacts of one kind from a generation.
    fn artifacts(&self, generation: u64, kind: ArtifactKind) -> TldrResult<Vec<ArtifactEnvelope>>;
    /// Commit one bounded batch and its resumable checkpoint atomically.
    fn commit_batch(&self, batch: &ArtifactBatch, job: &IngestionJob) -> TldrResult<()>;
    /// Atomically attach a demand-driven artifact to the active generation.
    fn commit_optional(&self, artifact: &ArtifactEnvelope) -> TldrResult<()>;
    /// Atomically publish a validated generation.
    fn publish(&self, manifest: &GenerationManifest) -> TldrResult<()>;
    /// Read a durable generation manifest.
    fn generation(&self, generation: u64) -> TldrResult<Option<GenerationManifest>>;
    /// Read a resumable job.
    fn job(&self, id: &str) -> TldrResult<Option<IngestionJob>>;
    /// Find artifacts that directly depend on `key`.
    fn reverse_dependencies(&self, key: &ArtifactKey) -> TldrResult<Vec<ArtifactKey>>;
}

/// Authoritative project artifact database.
pub struct RedbArtifactStore {
    database: Database,
    path: PathBuf,
    cache_size_bytes: usize,
}

impl RedbArtifactStore {
    /// Open or create a shared artifact database with the default cache bound.
    pub fn open(path: &Path) -> TldrResult<Self> {
        Self::open_with_cache(path, DEFAULT_CACHE_BYTES)
    }

    /// Open or create a shared artifact database with an explicit cache bound.
    pub fn open_with_cache(path: &Path, cache_size_bytes: usize) -> TldrResult<Self> {
        if cache_size_bytes == 0 {
            return Err(store_error("redb cache size must be positive"));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut builder = Database::builder();
        builder.set_cache_size(cache_size_bytes);
        let database = builder.create(path).map_err(redb_error)?;
        let store = Self {
            database,
            path: path.to_path_buf(),
            cache_size_bytes,
        };
        store.initialize()?;
        Ok(store)
    }

    fn initialize(&self) -> TldrResult<()> {
        let mut tx = self.database.begin_write().map_err(redb_error)?;
        tx.set_durability(Durability::Immediate)
            .map_err(redb_error)?;
        {
            let mut metadata = tx.open_table(METADATA).map_err(redb_error)?;
            let existing = metadata
                .get(SCHEMA_KEY)
                .map_err(redb_error)?
                .map(|value| value.value().to_vec());
            match existing {
                Some(value) if value != STORE_SCHEMA.as_bytes() => {
                    return Err(store_error(format!(
                        "incompatible artifact store at {}; rebuild required",
                        self.path.display()
                    )));
                }
                Some(_) => {}
                None => {
                    metadata
                        .insert(SCHEMA_KEY, STORE_SCHEMA.as_bytes())
                        .map_err(redb_error)?;
                }
            }
        }
        tx.open_table(GENERATIONS).map_err(redb_error)?;
        tx.open_table(ARTIFACTS).map_err(redb_error)?;
        tx.open_multimap_table(ARTIFACT_DEPS).map_err(redb_error)?;
        tx.open_multimap_table(GENERATION_ARTIFACTS)
            .map_err(redb_error)?;
        tx.open_table(JOBS).map_err(redb_error)?;
        tx.commit().map_err(redb_error)
    }

    /// Database path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Configured page-cache budget.
    pub fn cache_size_bytes(&self) -> usize {
        self.cache_size_bytes
    }
}

impl ArtifactStore for RedbArtifactStore {
    fn active_generation(&self) -> TldrResult<Option<u64>> {
        metadata_u64(&self.database, ACTIVE_GENERATION_KEY)
    }

    fn previous_generation(&self) -> TldrResult<Option<u64>> {
        metadata_u64(&self.database, PREVIOUS_GENERATION_KEY)
    }

    fn artifact(&self, key: &ArtifactKey) -> TldrResult<Option<ArtifactEnvelope>> {
        let key = encode(key)?;
        let tx = self.database.begin_read().map_err(redb_error)?;
        let table = tx.open_table(ARTIFACTS).map_err(redb_error)?;
        table
            .get(key.as_slice())
            .map_err(redb_error)?
            .map(|value| decode(value.value()))
            .transpose()
    }

    fn artifacts(&self, generation: u64, kind: ArtifactKind) -> TldrResult<Vec<ArtifactEnvelope>> {
        let tx = self.database.begin_read().map_err(redb_error)?;
        let links = tx
            .open_multimap_table(GENERATION_ARTIFACTS)
            .map_err(redb_error)?;
        let records = tx.open_table(ARTIFACTS).map_err(redb_error)?;
        let mut result = Vec::new();
        for value in links.get(generation).map_err(redb_error)? {
            let key = value.map_err(redb_error)?;
            let Some(record) = records.get(key.value()).map_err(redb_error)? else {
                return Err(store_error("generation references a missing artifact"));
            };
            let envelope: ArtifactEnvelope = decode(record.value())?;
            if envelope.key.kind == kind {
                result.push(envelope);
            }
        }
        Ok(result)
    }

    fn commit_batch(&self, batch: &ArtifactBatch, job: &IngestionJob) -> TldrResult<()> {
        if batch.generation == 0 || batch.generation != job.target_generation {
            return Err(store_error("artifact batch and job generation differ"));
        }
        if batch
            .artifacts
            .iter()
            .any(|artifact| artifact.generation != batch.generation || !artifact.is_valid())
        {
            return Err(store_error("artifact batch contains an invalid record"));
        }
        let records = batch
            .artifacts
            .iter()
            .map(|artifact| Ok((encode(&artifact.key)?, encode(artifact)?)))
            .collect::<TldrResult<Vec<_>>>()?;
        let dependencies = batch
            .artifacts
            .iter()
            .map(|artifact| {
                Ok((
                    encode(&artifact.key)?,
                    artifact
                        .dependencies
                        .iter()
                        .map(encode)
                        .collect::<TldrResult<Vec<_>>>()?,
                ))
            })
            .collect::<TldrResult<Vec<_>>>()?;
        let job_bytes = encode(job)?;

        let mut tx = self.database.begin_write().map_err(redb_error)?;
        tx.set_durability(Durability::Immediate)
            .map_err(redb_error)?;
        {
            let jobs = tx.open_table(JOBS).map_err(redb_error)?;
            let existing = jobs
                .get(job.id.as_str())
                .map_err(redb_error)?
                .map(|value| value.value().to_vec());
            if let Some(existing) = existing {
                let existing: IngestionJob = decode(&existing)?;
                if existing.target_generation != job.target_generation
                    || existing.source_revision != job.source_revision
                    || job.next_batch < existing.next_batch
                    || job.next_batch > existing.next_batch.saturating_add(1)
                {
                    return Err(store_error("invalid ingestion checkpoint transition"));
                }
            }
        }
        {
            let mut table = tx.open_table(ARTIFACTS).map_err(redb_error)?;
            for (key, record) in &records {
                table
                    .insert(key.as_slice(), record.as_slice())
                    .map_err(redb_error)?;
            }
        }
        {
            let mut links = tx
                .open_multimap_table(GENERATION_ARTIFACTS)
                .map_err(redb_error)?;
            for (key, _) in &records {
                links
                    .insert(batch.generation, key.as_slice())
                    .map_err(redb_error)?;
            }
        }
        {
            let mut deps = tx.open_multimap_table(ARTIFACT_DEPS).map_err(redb_error)?;
            for (dependent, inputs) in &dependencies {
                for input in inputs {
                    deps.insert(input.as_slice(), dependent.as_slice())
                        .map_err(redb_error)?;
                }
            }
        }
        {
            let mut jobs = tx.open_table(JOBS).map_err(redb_error)?;
            jobs.insert(job.id.as_str(), job_bytes.as_slice())
                .map_err(redb_error)?;
        }
        tx.commit().map_err(redb_error)
    }

    fn commit_optional(&self, artifact: &ArtifactEnvelope) -> TldrResult<()> {
        if !artifact.is_valid() {
            return Err(store_error("optional artifact is invalid"));
        }
        let key = encode(&artifact.key)?;
        let record = encode(artifact)?;
        let dependencies = artifact
            .dependencies
            .iter()
            .map(encode)
            .collect::<TldrResult<Vec<_>>>()?;

        let mut tx = self.database.begin_write().map_err(redb_error)?;
        tx.set_durability(Durability::Immediate)
            .map_err(redb_error)?;
        let active = {
            let metadata = tx.open_table(METADATA).map_err(redb_error)?;
            let active = metadata
                .get(ACTIVE_GENERATION_KEY)
                .map_err(redb_error)?
                .and_then(|value| value.value().try_into().ok().map(u64::from_le_bytes))
                .ok_or_else(|| store_error("no active generation for optional artifact"))?;
            active
        };
        if active != artifact.generation {
            return Err(store_error(
                "optional artifact does not target the active generation",
            ));
        }
        {
            let records = tx.open_table(ARTIFACTS).map_err(redb_error)?;
            for dependency in &dependencies {
                if records
                    .get(dependency.as_slice())
                    .map_err(redb_error)?
                    .is_none()
                {
                    return Err(store_error("optional artifact dependency is missing"));
                }
            }
        }
        {
            let mut records = tx.open_table(ARTIFACTS).map_err(redb_error)?;
            records
                .insert(key.as_slice(), record.as_slice())
                .map_err(redb_error)?;
        }
        {
            let mut links = tx
                .open_multimap_table(GENERATION_ARTIFACTS)
                .map_err(redb_error)?;
            links.insert(active, key.as_slice()).map_err(redb_error)?;
        }
        {
            let mut deps = tx.open_multimap_table(ARTIFACT_DEPS).map_err(redb_error)?;
            for dependency in &dependencies {
                deps.insert(dependency.as_slice(), key.as_slice())
                    .map_err(redb_error)?;
            }
        }
        {
            let mut generations = tx.open_table(GENERATIONS).map_err(redb_error)?;
            let bytes = generations
                .get(active)
                .map_err(redb_error)?
                .map(|value| value.value().to_vec())
                .ok_or_else(|| store_error("active generation manifest is missing"))?;
            let mut manifest: GenerationManifest = decode(&bytes)?;
            if !manifest.artifacts.contains(&artifact.key) {
                manifest.artifacts.push(artifact.key.clone());
            }
            let bytes = encode(&manifest)?;
            generations
                .insert(active, bytes.as_slice())
                .map_err(redb_error)?;
        }
        tx.commit().map_err(redb_error)
    }

    fn publish(&self, manifest: &GenerationManifest) -> TldrResult<()> {
        if manifest.generation == 0 {
            return Err(store_error("generation zero cannot be published"));
        }
        let keys = manifest
            .artifacts
            .iter()
            .map(encode)
            .collect::<TldrResult<Vec<_>>>()?;
        let manifest_bytes = encode(manifest)?;
        let mut tx = self.database.begin_write().map_err(redb_error)?;
        tx.set_durability(Durability::Immediate)
            .map_err(redb_error)?;
        {
            let artifacts = tx.open_table(ARTIFACTS).map_err(redb_error)?;
            for key in &keys {
                if artifacts.get(key.as_slice()).map_err(redb_error)?.is_none() {
                    return Err(store_error(
                        "cannot publish a generation with missing artifacts",
                    ));
                }
            }
        }
        {
            let generations = tx.open_table(GENERATIONS).map_err(redb_error)?;
            if generations
                .get(manifest.generation)
                .map_err(redb_error)?
                .is_some()
            {
                return Err(store_error("generation already exists"));
            }
        }
        {
            let mut generations = tx.open_table(GENERATIONS).map_err(redb_error)?;
            generations
                .insert(manifest.generation, manifest_bytes.as_slice())
                .map_err(redb_error)?;
        }
        {
            let mut links = tx
                .open_multimap_table(GENERATION_ARTIFACTS)
                .map_err(redb_error)?;
            for key in &keys {
                links
                    .insert(manifest.generation, key.as_slice())
                    .map_err(redb_error)?;
            }
        }
        {
            let mut metadata = tx.open_table(METADATA).map_err(redb_error)?;
            let active = metadata
                .get(ACTIVE_GENERATION_KEY)
                .map_err(redb_error)?
                .map(|value| value.value().to_vec());
            if let Some(active) = active {
                metadata
                    .insert(PREVIOUS_GENERATION_KEY, active.as_slice())
                    .map_err(redb_error)?;
            }
            metadata
                .insert(
                    ACTIVE_GENERATION_KEY,
                    manifest.generation.to_le_bytes().as_slice(),
                )
                .map_err(redb_error)?;
        }
        tx.commit().map_err(redb_error)
    }

    fn generation(&self, generation: u64) -> TldrResult<Option<GenerationManifest>> {
        let tx = self.database.begin_read().map_err(redb_error)?;
        let table = tx.open_table(GENERATIONS).map_err(redb_error)?;
        table
            .get(generation)
            .map_err(redb_error)?
            .map(|value| decode(value.value()))
            .transpose()
    }

    fn job(&self, id: &str) -> TldrResult<Option<IngestionJob>> {
        let tx = self.database.begin_read().map_err(redb_error)?;
        let table = tx.open_table(JOBS).map_err(redb_error)?;
        table
            .get(id)
            .map_err(redb_error)?
            .map(|value| decode(value.value()))
            .transpose()
    }

    fn reverse_dependencies(&self, key: &ArtifactKey) -> TldrResult<Vec<ArtifactKey>> {
        let key = encode(key)?;
        let tx = self.database.begin_read().map_err(redb_error)?;
        let table = tx.open_multimap_table(ARTIFACT_DEPS).map_err(redb_error)?;
        table
            .get(key.as_slice())
            .map_err(redb_error)?
            .map(|value| {
                let value = value.map_err(redb_error)?;
                decode(value.value())
            })
            .collect()
    }
}

pub(crate) fn encode<T>(value: &T) -> TldrResult<Vec<u8>>
where
    T: for<'a> RkyvSerialize<
        rkyv::api::high::HighSerializer<
            rkyv::util::AlignedVec,
            rkyv::ser::allocator::ArenaHandle<'a>,
            rkyv::rancor::Error,
        >,
    >,
{
    rkyv::to_bytes::<rkyv::rancor::Error>(value)
        .map(|bytes| bytes.into_vec())
        .map_err(binary_error)
}

pub(crate) fn decode<T>(bytes: &[u8]) -> TldrResult<T>
where
    T: Archive,
    T::Archived: for<'a> rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>
        + RkyvDeserialize<T, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>,
{
    let mut aligned: rkyv::util::AlignedVec<16> =
        rkyv::util::AlignedVec::with_capacity(bytes.len());
    aligned.extend_from_slice(bytes);
    rkyv::from_bytes::<T, rkyv::rancor::Error>(&aligned).map_err(binary_error)
}

fn metadata_u64(database: &Database, key: &str) -> TldrResult<Option<u64>> {
    let tx = database.begin_read().map_err(redb_error)?;
    let metadata = tx.open_table(METADATA).map_err(redb_error)?;
    Ok(metadata
        .get(key)
        .map_err(redb_error)?
        .and_then(|value| value.value().try_into().ok().map(u64::from_le_bytes)))
}

fn redb_error(error: impl std::fmt::Display) -> TldrError {
    store_error(format!("redb artifact store: {error}"))
}

fn binary_error(error: impl std::fmt::Display) -> TldrError {
    store_error(format!("artifact binary codec: {error}"))
}

fn store_error(message: impl Into<String>) -> TldrError {
    TldrError::ParseError {
        file: PathBuf::from("<artifact-store>"),
        line: None,
        message: message.into(),
    }
}

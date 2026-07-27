//! Versioned redb storage for semantic records, embeddings, and resumable jobs.

use std::path::{Path, PathBuf};

use redb::{
    Database, Durability, MultimapTableDefinition, ReadableDatabase, ReadableTable,
    ReadableTableMetadata, TableDefinition,
};
use serde::{Deserialize, Serialize};

use crate::{TldrError, TldrResult};

const STORE_SCHEMA_VERSION: u32 = 1;
const EMBEDDING_RECORD_VERSION: u32 = 1;
const EMBEDDING_HEADER_BYTES: usize = 4 + 4 + 8 + 8 + 32;
const NO_FILE_MTIME: u64 = u64::MAX;

const METADATA: TableDefinition<&str, &[u8]> = TableDefinition::new("metadata");
const FILES: TableDefinition<&str, &[u8]> = TableDefinition::new("files");
const CHUNKS: TableDefinition<&str, &[u8]> = TableDefinition::new("chunks");
const FILE_CHUNKS: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("file_chunks");
const EMBEDDINGS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("embeddings");
const JOBS: TableDefinition<&str, &[u8]> = TableDefinition::new("jobs");
const GENERATIONS: TableDefinition<u64, &[u8]> = TableDefinition::new("generations");
const GENERATION_VECTORS: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("generation_vectors");
const SCHEMA_KEY: &str = "schema_version";
const ACTIVE_GENERATION_KEY: &str = "active_generation";
const PREVIOUS_GENERATION_KEY: &str = "previous_generation";

/// Default redb page-cache budget. This replaces redb's 1 GiB default.
pub const DEFAULT_REDB_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// Durable file metadata used by incremental generation construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredFileRecord {
    /// Root-relative source path.
    pub path: String,
    /// Last observed modification time.
    pub mtime: u64,
    /// Last observed byte size.
    pub size: u64,
    /// Generation that owns this record.
    pub generation: u64,
}

/// Durable chunk metadata. Embedding bytes live in the separate embedding table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredChunkRecord {
    /// Stable logical chunk ID.
    pub chunk_id: String,
    /// Owning root-relative source path.
    pub file_path: String,
    /// Fingerprint of the complete embedding recipe.
    pub recipe: [u8; 32],
    /// Revision of the exact composed document.
    pub revision: [u8; 32],
    /// Generation that owns this record.
    pub generation: u64,
}

/// State of one resumable bulk embedding job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Work exists but has not started.
    Pending,
    /// A worker owns the next batch.
    Running,
    /// Every batch is durably committed.
    Completed,
    /// Retry budget was exhausted.
    Failed,
    /// Cancellation was durably acknowledged.
    Cancelled,
}

/// Durable checkpoint for a resumable bulk embedding job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRecord {
    /// Caller-stable job identifier.
    pub id: String,
    /// Protocol version required by the worker.
    pub protocol_version: u32,
    /// Full recipe fingerprint required by the job.
    pub recipe: [u8; 32],
    /// Number of the next uncommitted batch.
    pub next_batch: u64,
    /// Total batches in the job.
    pub total_batches: u64,
    /// Number of failed worker attempts.
    pub retries: u32,
    /// Maximum permitted failed worker attempts.
    pub max_retries: u32,
    /// Current durable state.
    pub state: JobState,
    /// Last update time as Unix seconds.
    pub updated_at: u64,
}

impl JobRecord {
    fn validate(&self) -> TldrResult<()> {
        if self.next_batch > self.total_batches {
            return Err(store_error(format!(
                "job {} checkpoint {} exceeds total {}",
                self.id, self.next_batch, self.total_batches
            )));
        }
        if self.retries > self.max_retries {
            return Err(store_error(format!(
                "job {} retries {} exceed limit {}",
                self.id, self.retries, self.max_retries
            )));
        }
        if self.state == JobState::Completed && self.next_batch != self.total_batches {
            return Err(store_error(format!(
                "job {} is complete with an incomplete checkpoint",
                self.id
            )));
        }
        Ok(())
    }
}

/// Borrowed embedding write used by atomic batch commits.
pub struct EmbeddingWrite<'a> {
    /// Content-addressed cache key.
    pub key: &'a [u8],
    /// Complete recipe fingerprint.
    pub recipe: [u8; 32],
    /// Normalized f32 vector.
    pub vector: &'a [f32],
    /// Cache insertion time as Unix seconds.
    pub cached_at: u64,
    /// Optional source-file modification time.
    pub file_mtime: Option<u64>,
}

/// Decoded and validated embedding record.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEmbedding {
    /// Complete recipe fingerprint stored alongside the vector.
    pub recipe: [u8; 32],
    /// Normalized vector decoded from little-endian bytes.
    pub vector: Vec<f32>,
    /// Cache insertion time as Unix seconds.
    pub cached_at: u64,
    /// Optional source-file modification time.
    pub file_mtime: Option<u64>,
}

/// Publication state for one immutable semantic generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationState {
    /// Authoritative records are being staged and must not be served.
    Staged,
    /// Every record and derived artifact has been verified.
    Complete,
}

/// Authoritative identity and integrity metadata for one generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredGeneration {
    /// Monotonic generation number.
    pub generation: u64,
    /// Whether the generation may be served.
    pub state: GenerationState,
    /// Expected number of vector records.
    pub chunk_count: u64,
    /// Vector width required by every record.
    pub dimensions: u32,
    /// Serialized complete vector-store manifest identity.
    pub manifest_identity: Vec<u8>,
    /// Source corpus digest captured during construction.
    pub corpus_digest: u64,
    /// Serialized per-file records needed for reconstruction.
    pub files: Vec<u8>,
}

/// One vector record written into an immutable generation.
pub struct GenerationVectorWrite<'a> {
    /// Stable usearch key.
    pub key: u64,
    /// Normalized vector values.
    pub vector: &'a [f32],
    /// Serialized chunk metadata.
    pub metadata: &'a [u8],
}

/// Decoded vector record read from an immutable generation.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredGenerationVector {
    /// Stable usearch key.
    pub key: u64,
    /// Normalized vector values.
    pub vector: Vec<f32>,
    /// Serialized chunk metadata.
    pub metadata: Vec<u8>,
}

/// Observable storage counts and configured cache bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedbStoreStats {
    /// Number of embedding records.
    pub embeddings: usize,
    /// Sum of decoded f32 payload bytes.
    pub embedding_bytes: usize,
    /// Configured redb page-cache limit.
    pub cache_size_bytes: usize,
}

/// Authoritative semantic record store.
pub struct RedbStore {
    database: Database,
    path: PathBuf,
    cache_size_bytes: usize,
}

impl RedbStore {
    /// Open or create a versioned database with an explicit page-cache bound.
    pub fn open(path: &Path, cache_size_bytes: usize) -> TldrResult<Self> {
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
        let mut transaction = self.database.begin_write().map_err(redb_error)?;
        transaction
            .set_durability(Durability::Immediate)
            .map_err(redb_error)?;
        {
            let mut metadata = transaction.open_table(METADATA).map_err(redb_error)?;
            let existing = metadata
                .get(SCHEMA_KEY)
                .map_err(redb_error)?
                .map(|version| version.value().to_vec());
            match existing {
                Some(bytes) => {
                    if bytes != STORE_SCHEMA_VERSION.to_le_bytes() {
                        return Err(store_error(format!(
                            "incompatible redb schema in {}: expected {}, found {}",
                            self.path.display(),
                            STORE_SCHEMA_VERSION,
                            decode_u32(&bytes).unwrap_or_default()
                        )));
                    }
                }
                None => {
                    metadata
                        .insert(SCHEMA_KEY, STORE_SCHEMA_VERSION.to_le_bytes().as_slice())
                        .map_err(redb_error)?;
                }
            }
        }
        {
            transaction.open_table(FILES).map_err(redb_error)?;
            transaction.open_table(CHUNKS).map_err(redb_error)?;
            transaction
                .open_multimap_table(FILE_CHUNKS)
                .map_err(redb_error)?;
            transaction.open_table(EMBEDDINGS).map_err(redb_error)?;
            transaction.open_table(JOBS).map_err(redb_error)?;
            transaction.open_table(GENERATIONS).map_err(redb_error)?;
            transaction
                .open_table(GENERATION_VECTORS)
                .map_err(redb_error)?;
        }
        transaction.commit().map_err(redb_error)
    }

    /// Configured redb page-cache budget.
    pub fn cache_size_bytes(&self) -> usize {
        self.cache_size_bytes
    }

    /// Store or replace one embedding without rewriting unrelated records.
    pub fn put_embedding(&self, write: EmbeddingWrite<'_>) -> TldrResult<()> {
        validate_cache_key(write.key)?;
        let encoded = encode_embedding(
            write.recipe,
            write.vector,
            write.cached_at,
            write.file_mtime,
        )?;
        let mut transaction = self.database.begin_write().map_err(redb_error)?;
        transaction
            .set_durability(Durability::Immediate)
            .map_err(redb_error)?;
        {
            let mut table = transaction.open_table(EMBEDDINGS).map_err(redb_error)?;
            table
                .insert(write.key, encoded.as_slice())
                .map_err(redb_error)?;
        }
        transaction.commit().map_err(redb_error)
    }

    /// Read one embedding and reject recipe, dimension, or byte corruption.
    pub fn get_embedding(
        &self,
        key: &[u8],
        expected_recipe: [u8; 32],
        expected_dimensions: usize,
    ) -> TldrResult<Option<StoredEmbedding>> {
        validate_cache_key(key)?;
        let transaction = self.database.begin_read().map_err(redb_error)?;
        let table = transaction.open_table(EMBEDDINGS).map_err(redb_error)?;
        let Some(value) = table.get(key).map_err(redb_error)? else {
            return Ok(None);
        };
        decode_embedding(value.value(), expected_recipe, expected_dimensions).map(Some)
    }

    /// Remove one content-addressed embedding record.
    pub fn remove_embedding(&self, key: &[u8]) -> TldrResult<bool> {
        validate_cache_key(key)?;
        let transaction = self.database.begin_write().map_err(redb_error)?;
        let removed = {
            let mut table = transaction.open_table(EMBEDDINGS).map_err(redb_error)?;
            let removed = table.remove(key).map_err(redb_error)?.is_some();
            removed
        };
        transaction.commit().map_err(redb_error)?;
        Ok(removed)
    }

    /// Remove and return the oldest embedding record.
    pub fn remove_oldest_embedding(&self) -> TldrResult<Option<(Vec<u8>, usize)>> {
        let transaction = self.database.begin_write().map_err(redb_error)?;
        let oldest = {
            let table = transaction.open_table(EMBEDDINGS).map_err(redb_error)?;
            let mut oldest: Option<(Vec<u8>, u64, usize)> = None;
            for item in table.iter().map_err(redb_error)? {
                let (key, value) = item.map_err(redb_error)?;
                let bytes = value.value();
                let cached_at = embedding_cached_at(bytes)?;
                let vector_bytes = bytes.len().saturating_sub(EMBEDDING_HEADER_BYTES);
                if oldest
                    .as_ref()
                    .is_none_or(|(_, candidate, _)| cached_at < *candidate)
                {
                    oldest = Some((key.value().to_vec(), cached_at, vector_bytes));
                }
            }
            oldest
        };
        let Some((key, _, vector_bytes)) = oldest else {
            transaction.abort().map_err(redb_error)?;
            return Ok(None);
        };
        {
            let mut table = transaction.open_table(EMBEDDINGS).map_err(redb_error)?;
            table.remove(key.as_slice()).map_err(redb_error)?;
        }
        transaction.commit().map_err(redb_error)?;
        Ok(Some((key, vector_bytes)))
    }

    /// Return keys whose insertion timestamp is at least `ttl_seconds` old.
    pub fn embedding_keys_older_than(
        &self,
        now: u64,
        ttl_seconds: u64,
    ) -> TldrResult<Vec<Vec<u8>>> {
        let transaction = self.database.begin_read().map_err(redb_error)?;
        let table = transaction.open_table(EMBEDDINGS).map_err(redb_error)?;
        let mut keys = Vec::new();
        for item in table.iter().map_err(redb_error)? {
            let (key, value) = item.map_err(redb_error)?;
            if now.saturating_sub(embedding_cached_at(value.value())?) >= ttl_seconds {
                keys.push(key.value().to_vec());
            }
        }
        Ok(keys)
    }

    /// Return storage counts without loading vector payloads into an owned map.
    pub fn stats(&self) -> TldrResult<RedbStoreStats> {
        let transaction = self.database.begin_read().map_err(redb_error)?;
        let table = transaction.open_table(EMBEDDINGS).map_err(redb_error)?;
        let embeddings = table.len().map_err(redb_error)? as usize;
        let mut embedding_bytes = 0usize;
        for item in table.iter().map_err(redb_error)? {
            let (_, value) = item.map_err(redb_error)?;
            embedding_bytes = embedding_bytes
                .saturating_add(value.value().len().saturating_sub(EMBEDDING_HEADER_BYTES));
        }
        Ok(RedbStoreStats {
            embeddings,
            embedding_bytes,
            cache_size_bytes: self.cache_size_bytes,
        })
    }

    /// Atomically replace one file and its complete chunk membership.
    pub fn put_file_with_chunks(
        &self,
        file: &StoredFileRecord,
        chunks: &[StoredChunkRecord],
    ) -> TldrResult<()> {
        if chunks.iter().any(|chunk| chunk.file_path != file.path) {
            return Err(store_error("chunk belongs to a different file"));
        }
        let file_bytes = serde_json::to_vec(file).map_err(serialization_error)?;
        let chunk_bytes = chunks
            .iter()
            .map(|chunk| serde_json::to_vec(chunk).map_err(serialization_error))
            .collect::<TldrResult<Vec<_>>>()?;
        let mut transaction = self.database.begin_write().map_err(redb_error)?;
        transaction
            .set_durability(Durability::Immediate)
            .map_err(redb_error)?;
        {
            let mut files = transaction.open_table(FILES).map_err(redb_error)?;
            files
                .insert(file.path.as_str(), file_bytes.as_slice())
                .map_err(redb_error)?;
        }
        {
            let mut links = transaction
                .open_multimap_table(FILE_CHUNKS)
                .map_err(redb_error)?;
            let removed = links
                .remove_all(file.path.as_str())
                .map_err(redb_error)?
                .map(|value| {
                    value
                        .map(|value| value.value().to_string())
                        .map_err(redb_error)
                })
                .collect::<TldrResult<Vec<_>>>()?;
            drop(links);
            let mut records = transaction.open_table(CHUNKS).map_err(redb_error)?;
            for chunk_id in removed {
                records.remove(chunk_id.as_str()).map_err(redb_error)?;
            }
        }
        {
            let mut links = transaction
                .open_multimap_table(FILE_CHUNKS)
                .map_err(redb_error)?;
            for chunk in chunks {
                links
                    .insert(file.path.as_str(), chunk.chunk_id.as_str())
                    .map_err(redb_error)?;
            }
        }
        {
            let mut records = transaction.open_table(CHUNKS).map_err(redb_error)?;
            for (chunk, bytes) in chunks.iter().zip(&chunk_bytes) {
                records
                    .insert(chunk.chunk_id.as_str(), bytes.as_slice())
                    .map_err(redb_error)?;
            }
        }
        transaction.commit().map_err(redb_error)
    }

    /// Read one durable job checkpoint.
    pub fn get_job(&self, id: &str) -> TldrResult<Option<JobRecord>> {
        let transaction = self.database.begin_read().map_err(redb_error)?;
        let table = transaction.open_table(JOBS).map_err(redb_error)?;
        let Some(value) = table.get(id).map_err(redb_error)? else {
            return Ok(None);
        };
        let job = serde_json::from_slice(value.value()).map_err(serialization_error)?;
        Ok(Some(job))
    }

    /// Persist a job transition atomically with all completed batch embeddings.
    pub fn commit_job_batch(
        &self,
        job: &JobRecord,
        embeddings: &[EmbeddingWrite<'_>],
    ) -> TldrResult<()> {
        job.validate()?;
        let encoded = embeddings
            .iter()
            .map(|write| {
                validate_cache_key(write.key)?;
                encode_embedding(
                    write.recipe,
                    write.vector,
                    write.cached_at,
                    write.file_mtime,
                )
                .map(|bytes| (write.key, bytes))
            })
            .collect::<TldrResult<Vec<_>>>()?;
        let job_bytes = serde_json::to_vec(job).map_err(serialization_error)?;
        let mut transaction = self.database.begin_write().map_err(redb_error)?;
        transaction
            .set_durability(Durability::Immediate)
            .map_err(redb_error)?;
        {
            let jobs = transaction.open_table(JOBS).map_err(redb_error)?;
            let existing = jobs
                .get(job.id.as_str())
                .map_err(redb_error)?
                .map(|value| value.value().to_vec());
            if let Some(existing) = existing {
                let existing: JobRecord =
                    serde_json::from_slice(&existing).map_err(serialization_error)?;
                if existing.protocol_version != job.protocol_version
                    || existing.recipe != job.recipe
                {
                    return Err(store_error(format!(
                        "job {} protocol or recipe changed during resume",
                        job.id
                    )));
                }
                if job.next_batch < existing.next_batch
                    || job.next_batch > existing.next_batch.saturating_add(1)
                {
                    return Err(store_error(format!(
                        "job {} checkpoint transition {} -> {} has a duplicate or gap",
                        job.id, existing.next_batch, job.next_batch
                    )));
                }
            }
        }
        {
            let mut table = transaction.open_table(EMBEDDINGS).map_err(redb_error)?;
            for (key, bytes) in &encoded {
                table.insert(*key, bytes.as_slice()).map_err(redb_error)?;
            }
        }
        {
            let mut jobs = transaction.open_table(JOBS).map_err(redb_error)?;
            jobs.insert(job.id.as_str(), job_bytes.as_slice())
                .map_err(redb_error)?;
        }
        transaction.commit().map_err(redb_error)
    }

    /// Create an unpublished generation. Reusing an existing generation is rejected.
    pub fn stage_generation(&self, generation: &StoredGeneration) -> TldrResult<()> {
        if generation.generation == 0 || generation.state != GenerationState::Staged {
            return Err(store_error("new generation must be non-zero and staged"));
        }
        let bytes = serde_json::to_vec(generation).map_err(serialization_error)?;
        let mut transaction = self.database.begin_write().map_err(redb_error)?;
        transaction
            .set_durability(Durability::Immediate)
            .map_err(redb_error)?;
        {
            let mut table = transaction.open_table(GENERATIONS).map_err(redb_error)?;
            if table
                .get(generation.generation)
                .map_err(redb_error)?
                .is_some()
            {
                return Err(store_error(format!(
                    "generation {} already exists",
                    generation.generation
                )));
            }
            table
                .insert(generation.generation, bytes.as_slice())
                .map_err(redb_error)?;
        }
        transaction.commit().map_err(redb_error)
    }

    /// Durably append a bounded batch of vectors to a staged generation.
    pub fn put_generation_vectors(
        &self,
        generation: u64,
        dimensions: usize,
        writes: &[GenerationVectorWrite<'_>],
    ) -> TldrResult<()> {
        let encoded = writes
            .iter()
            .map(|write| {
                encode_generation_vector(write.vector, write.metadata, dimensions)
                    .map(|value| (generation_vector_key(generation, write.key), value))
            })
            .collect::<TldrResult<Vec<_>>>()?;
        let mut transaction = self.database.begin_write().map_err(redb_error)?;
        transaction
            .set_durability(Durability::Immediate)
            .map_err(redb_error)?;
        {
            let generations = transaction.open_table(GENERATIONS).map_err(redb_error)?;
            let value = generations
                .get(generation)
                .map_err(redb_error)?
                .ok_or_else(|| store_error(format!("generation {generation} is not staged")))?;
            let record: StoredGeneration =
                serde_json::from_slice(value.value()).map_err(serialization_error)?;
            if record.state != GenerationState::Staged || record.dimensions as usize != dimensions {
                return Err(store_error(format!(
                    "generation {generation} is not a compatible staged generation"
                )));
            }
        }
        {
            let mut table = transaction
                .open_table(GENERATION_VECTORS)
                .map_err(redb_error)?;
            for (key, value) in &encoded {
                table
                    .insert(key.as_slice(), value.as_slice())
                    .map_err(redb_error)?;
            }
        }
        transaction.commit().map_err(redb_error)
    }

    /// Mark a fully staged generation complete and atomically publish it.
    pub fn complete_and_activate_generation(&self, generation: u64) -> TldrResult<()> {
        let mut transaction = self.database.begin_write().map_err(redb_error)?;
        transaction
            .set_durability(Durability::Immediate)
            .map_err(redb_error)?;
        let mut record = {
            let generations = transaction.open_table(GENERATIONS).map_err(redb_error)?;
            let value = generations
                .get(generation)
                .map_err(redb_error)?
                .ok_or_else(|| store_error(format!("generation {generation} is missing")))?;
            serde_json::from_slice::<StoredGeneration>(value.value())
                .map_err(serialization_error)?
        };
        let prefix = generation.to_be_bytes();
        let actual = {
            let vectors = transaction
                .open_table(GENERATION_VECTORS)
                .map_err(redb_error)?;
            vectors
                .range(prefix.as_slice()..=generation_vector_key(generation, u64::MAX).as_slice())
                .map_err(redb_error)?
                .count() as u64
        };
        if actual != record.chunk_count {
            return Err(store_error(format!(
                "generation {generation} expected {} vectors, found {actual}",
                record.chunk_count
            )));
        }
        record.state = GenerationState::Complete;
        let bytes = serde_json::to_vec(&record).map_err(serialization_error)?;
        {
            let mut generations = transaction.open_table(GENERATIONS).map_err(redb_error)?;
            generations
                .insert(generation, bytes.as_slice())
                .map_err(redb_error)?;
        }
        {
            let mut metadata = transaction.open_table(METADATA).map_err(redb_error)?;
            let active = {
                let value = metadata.get(ACTIVE_GENERATION_KEY).map_err(redb_error)?;
                value.map(|value| value.value().to_vec())
            };
            if let Some(active) = active {
                metadata
                    .insert(PREVIOUS_GENERATION_KEY, active.as_slice())
                    .map_err(redb_error)?;
            }
            metadata
                .insert(ACTIVE_GENERATION_KEY, generation.to_le_bytes().as_slice())
                .map_err(redb_error)?;
        }
        transaction.commit().map_err(redb_error)
    }

    /// Return the generation atomically selected for serving.
    pub fn active_generation(&self) -> TldrResult<Option<u64>> {
        let transaction = self.database.begin_read().map_err(redb_error)?;
        let metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        Ok(metadata
            .get(ACTIVE_GENERATION_KEY)
            .map_err(redb_error)?
            .and_then(|value| decode_u64(value.value())))
    }

    /// Return the retained rollback generation.
    pub fn previous_generation(&self) -> TldrResult<Option<u64>> {
        let transaction = self.database.begin_read().map_err(redb_error)?;
        let metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        Ok(metadata
            .get(PREVIOUS_GENERATION_KEY)
            .map_err(redb_error)?
            .and_then(|value| decode_u64(value.value())))
    }

    /// Select any retained complete generation and preserve the former active
    /// generation as the next rollback target.
    pub fn select_complete_generation(&self, generation: u64) -> TldrResult<()> {
        let mut transaction = self.database.begin_write().map_err(redb_error)?;
        transaction
            .set_durability(Durability::Immediate)
            .map_err(redb_error)?;
        {
            let generations = transaction.open_table(GENERATIONS).map_err(redb_error)?;
            let value = generations
                .get(generation)
                .map_err(redb_error)?
                .ok_or_else(|| store_error(format!("generation {generation} is missing")))?;
            let record: StoredGeneration =
                serde_json::from_slice(value.value()).map_err(serialization_error)?;
            if record.state != GenerationState::Complete {
                return Err(store_error(format!(
                    "generation {generation} is not complete"
                )));
            }
        }
        {
            let mut metadata = transaction.open_table(METADATA).map_err(redb_error)?;
            let active = {
                let value = metadata.get(ACTIVE_GENERATION_KEY).map_err(redb_error)?;
                value.map(|value| value.value().to_vec())
            };
            if let Some(active) = active {
                metadata
                    .insert(PREVIOUS_GENERATION_KEY, active.as_slice())
                    .map_err(redb_error)?;
            }
            metadata
                .insert(ACTIVE_GENERATION_KEY, generation.to_le_bytes().as_slice())
                .map_err(redb_error)?;
        }
        transaction.commit().map_err(redb_error)
    }

    /// Read one generation's identity and publication state.
    pub fn generation(&self, generation: u64) -> TldrResult<Option<StoredGeneration>> {
        let transaction = self.database.begin_read().map_err(redb_error)?;
        let table = transaction.open_table(GENERATIONS).map_err(redb_error)?;
        table
            .get(generation)
            .map_err(redb_error)?
            .map(|value| serde_json::from_slice(value.value()).map_err(serialization_error))
            .transpose()
    }

    /// Read one generation's vectors in deterministic key order.
    pub fn generation_vectors(
        &self,
        generation: u64,
        dimensions: usize,
    ) -> TldrResult<Vec<StoredGenerationVector>> {
        let transaction = self.database.begin_read().map_err(redb_error)?;
        let table = transaction
            .open_table(GENERATION_VECTORS)
            .map_err(redb_error)?;
        let first = generation_vector_key(generation, 0);
        let last = generation_vector_key(generation, u64::MAX);
        table
            .range(first.as_slice()..=last.as_slice())
            .map_err(redb_error)?
            .map(|row| {
                let (key, value) = row.map_err(redb_error)?;
                let key = decode_generation_vector_key(key.value())?;
                let (vector, metadata) = decode_generation_vector(value.value(), dimensions)?;
                Ok(StoredGenerationVector {
                    key,
                    vector,
                    metadata,
                })
            })
            .collect()
    }
}

fn generation_vector_key(generation: u64, key: u64) -> [u8; 16] {
    let mut encoded = [0_u8; 16];
    encoded[..8].copy_from_slice(&generation.to_be_bytes());
    encoded[8..].copy_from_slice(&key.to_be_bytes());
    encoded
}

fn decode_generation_vector_key(bytes: &[u8]) -> TldrResult<u64> {
    decode_u64_be(
        bytes
            .get(8..16)
            .ok_or_else(|| store_error("invalid generation vector key"))?,
    )
    .ok_or_else(|| store_error("invalid generation vector key"))
}

fn encode_generation_vector(
    vector: &[f32],
    metadata: &[u8],
    dimensions: usize,
) -> TldrResult<Vec<u8>> {
    if vector.len() != dimensions || vector.iter().any(|value| !value.is_finite()) {
        return Err(store_error(
            "generation vector dimensions or values are invalid",
        ));
    }
    let metadata_len =
        u32::try_from(metadata.len()).map_err(|_| store_error("generation metadata too large"))?;
    let mut bytes = Vec::with_capacity(4 + metadata.len() + vector.len() * 4);
    bytes.extend_from_slice(&metadata_len.to_le_bytes());
    bytes.extend_from_slice(metadata);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    Ok(bytes)
}

fn decode_generation_vector(bytes: &[u8], dimensions: usize) -> TldrResult<(Vec<f32>, Vec<u8>)> {
    let metadata_len = decode_u32(
        bytes
            .get(..4)
            .ok_or_else(|| store_error("truncated generation vector"))?,
    )
    .ok_or_else(|| store_error("invalid generation metadata length"))?
        as usize;
    let vector_start = 4_usize
        .checked_add(metadata_len)
        .ok_or_else(|| store_error("generation vector length overflow"))?;
    let expected = vector_start
        .checked_add(dimensions.saturating_mul(4))
        .ok_or_else(|| store_error("generation vector length overflow"))?;
    if bytes.len() != expected {
        return Err(store_error("generation vector byte length mismatch"));
    }
    let metadata = bytes[4..vector_start].to_vec();
    let mut vector = Vec::with_capacity(dimensions);
    for encoded in bytes[vector_start..].chunks_exact(4) {
        let value = f32::from_le_bytes(encoded.try_into().expect("four-byte chunk"));
        if !value.is_finite() {
            return Err(store_error("generation vector contains a non-finite value"));
        }
        vector.push(value);
    }
    Ok((vector, metadata))
}

fn validate_cache_key(key: &[u8]) -> TldrResult<()> {
    if key.len() != 64 {
        return Err(store_error(format!(
            "embedding cache key must be 64 bytes, found {}",
            key.len()
        )));
    }
    Ok(())
}

fn encode_embedding(
    recipe: [u8; 32],
    vector: &[f32],
    cached_at: u64,
    file_mtime: Option<u64>,
) -> TldrResult<Vec<u8>> {
    let dimensions =
        u32::try_from(vector.len()).map_err(|_| store_error("embedding dimensions exceed u32"))?;
    if dimensions == 0 || vector.iter().any(|value| !value.is_finite()) {
        return Err(store_error(
            "embedding must contain finite values and at least one dimension",
        ));
    }
    let mut bytes = Vec::with_capacity(EMBEDDING_HEADER_BYTES + vector.len() * 4);
    bytes.extend_from_slice(&EMBEDDING_RECORD_VERSION.to_le_bytes());
    bytes.extend_from_slice(&dimensions.to_le_bytes());
    bytes.extend_from_slice(&cached_at.to_le_bytes());
    bytes.extend_from_slice(&file_mtime.unwrap_or(NO_FILE_MTIME).to_le_bytes());
    bytes.extend_from_slice(&recipe);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    Ok(bytes)
}

fn decode_embedding(
    bytes: &[u8],
    expected_recipe: [u8; 32],
    expected_dimensions: usize,
) -> TldrResult<StoredEmbedding> {
    if bytes.len() < EMBEDDING_HEADER_BYTES {
        return Err(store_error("truncated embedding record"));
    }
    let version = decode_u32(&bytes[0..4]).ok_or_else(|| store_error("invalid record version"))?;
    if version != EMBEDDING_RECORD_VERSION {
        return Err(store_error(format!(
            "incompatible embedding record version {version}"
        )));
    }
    let dimensions =
        decode_u32(&bytes[4..8]).ok_or_else(|| store_error("invalid dimension field"))? as usize;
    if dimensions != expected_dimensions {
        return Err(store_error(format!(
            "embedding dimension mismatch: expected {expected_dimensions}, found {dimensions}"
        )));
    }
    let expected_bytes = EMBEDDING_HEADER_BYTES
        .checked_add(dimensions.saturating_mul(4))
        .ok_or_else(|| store_error("embedding byte length overflow"))?;
    if bytes.len() != expected_bytes {
        return Err(store_error(format!(
            "embedding byte length mismatch: expected {expected_bytes}, found {}",
            bytes.len()
        )));
    }
    let cached_at = decode_u64(&bytes[8..16]).ok_or_else(|| store_error("invalid timestamp"))?;
    let encoded_mtime =
        decode_u64(&bytes[16..24]).ok_or_else(|| store_error("invalid file mtime"))?;
    let recipe: [u8; 32] = bytes[24..56]
        .try_into()
        .map_err(|_| store_error("invalid recipe fingerprint"))?;
    if recipe != expected_recipe {
        return Err(store_error("embedding recipe mismatch"));
    }
    let mut vector = Vec::with_capacity(dimensions);
    for encoded in bytes[EMBEDDING_HEADER_BYTES..].chunks_exact(4) {
        let value = f32::from_le_bytes(encoded.try_into().expect("four-byte chunk"));
        if !value.is_finite() {
            return Err(store_error("embedding contains a non-finite value"));
        }
        vector.push(value);
    }
    Ok(StoredEmbedding {
        recipe,
        vector,
        cached_at,
        file_mtime: (encoded_mtime != NO_FILE_MTIME).then_some(encoded_mtime),
    })
}

fn embedding_cached_at(bytes: &[u8]) -> TldrResult<u64> {
    if bytes.len() < EMBEDDING_HEADER_BYTES {
        return Err(store_error("truncated embedding record"));
    }
    decode_u64(&bytes[8..16]).ok_or_else(|| store_error("invalid embedding timestamp"))
}

fn decode_u32(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn decode_u64(bytes: &[u8]) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

fn decode_u64_be(bytes: &[u8]) -> Option<u64> {
    Some(u64::from_be_bytes(bytes.try_into().ok()?))
}

fn redb_error(error: impl std::fmt::Display) -> TldrError {
    store_error(error.to_string())
}

fn serialization_error(error: impl std::fmt::Display) -> TldrError {
    store_error(format!("record serialization failed: {error}"))
}

fn store_error(message: impl Into<String>) -> TldrError {
    TldrError::Embedding(format!("semantic redb store: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> [u8; 64] {
        [seed; 64]
    }

    fn write<'a>(key: &'a [u8], recipe: [u8; 32], vector: &'a [f32]) -> EmbeddingWrite<'a> {
        EmbeddingWrite {
            key,
            recipe,
            vector,
            cached_at: 10,
            file_mtime: Some(9),
        }
    }

    #[test]
    fn little_endian_embedding_roundtrip_validates_identity_and_dimensions() {
        let directory = tempfile::tempdir().unwrap();
        let store = RedbStore::open(&directory.path().join("semantic.redb"), 1024 * 1024).unwrap();
        let key = key(7);
        let recipe = [3; 32];
        let vector = [0.25, -0.5, 1.0];
        store.put_embedding(write(&key, recipe, &vector)).unwrap();

        let loaded = store.get_embedding(&key, recipe, 3).unwrap().unwrap();
        assert_eq!(loaded.vector, vector);
        assert_eq!(loaded.file_mtime, Some(9));
        assert!(store.get_embedding(&key, [4; 32], 3).is_err());
        assert!(store.get_embedding(&key, recipe, 2).is_err());
        assert_eq!(store.cache_size_bytes(), 1024 * 1024);
    }

    #[test]
    fn one_embedding_update_does_not_replace_other_records() {
        let directory = tempfile::tempdir().unwrap();
        let store = RedbStore::open(&directory.path().join("semantic.redb"), 1024 * 1024).unwrap();
        let first = key(1);
        let second = key(2);
        store.put_embedding(write(&first, [1; 32], &[1.0])).unwrap();
        store
            .put_embedding(write(&second, [2; 32], &[2.0]))
            .unwrap();
        store.put_embedding(write(&first, [1; 32], &[3.0])).unwrap();

        assert_eq!(
            store
                .get_embedding(&second, [2; 32], 1)
                .unwrap()
                .unwrap()
                .vector,
            [2.0]
        );
        assert_eq!(store.stats().unwrap().embeddings, 2);
    }

    #[test]
    fn file_and_chunk_records_commit_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let store = RedbStore::open(&directory.path().join("semantic.redb"), 1024 * 1024).unwrap();
        let file = StoredFileRecord {
            path: "src/lib.rs".into(),
            mtime: 1,
            size: 2,
            generation: 3,
        };
        let chunks = [StoredChunkRecord {
            chunk_id: "chunk-a".into(),
            file_path: file.path.clone(),
            recipe: [1; 32],
            revision: [2; 32],
            generation: 3,
        }];
        store.put_file_with_chunks(&file, &chunks).unwrap();

        let transaction = store.database.begin_read().unwrap();
        assert_eq!(transaction.open_table(FILES).unwrap().len().unwrap(), 1);
        assert_eq!(transaction.open_table(CHUNKS).unwrap().len().unwrap(), 1);
        assert_eq!(
            transaction
                .open_multimap_table(FILE_CHUNKS)
                .unwrap()
                .len()
                .unwrap(),
            1
        );
    }

    #[test]
    fn job_checkpoint_and_embeddings_share_one_commit() {
        let directory = tempfile::tempdir().unwrap();
        let store = RedbStore::open(&directory.path().join("semantic.redb"), 1024 * 1024).unwrap();
        let key = key(8);
        let job = JobRecord {
            id: "job-1".into(),
            protocol_version: 1,
            recipe: [8; 32],
            next_batch: 1,
            total_batches: 2,
            retries: 0,
            max_retries: 3,
            state: JobState::Running,
            updated_at: 12,
        };
        store
            .commit_job_batch(&job, &[write(&key, job.recipe, &[1.0, 2.0])])
            .unwrap();

        assert_eq!(store.get_job("job-1").unwrap(), Some(job));
        assert!(store.get_embedding(&key, [8; 32], 2).unwrap().is_some());
    }

    #[test]
    fn invalid_job_transition_aborts_without_embedding() {
        let directory = tempfile::tempdir().unwrap();
        let store = RedbStore::open(&directory.path().join("semantic.redb"), 1024 * 1024).unwrap();
        let key = key(9);
        let invalid = JobRecord {
            id: "bad".into(),
            protocol_version: 1,
            recipe: [9; 32],
            next_batch: 2,
            total_batches: 1,
            retries: 0,
            max_retries: 1,
            state: JobState::Running,
            updated_at: 0,
        };
        assert!(store
            .commit_job_batch(&invalid, &[write(&key, invalid.recipe, &[1.0])])
            .is_err());
        assert!(store
            .get_embedding(&key, invalid.recipe, 1)
            .unwrap()
            .is_none());
    }

    #[test]
    fn committed_records_survive_reopen_and_uncommitted_records_do_not() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("semantic.redb");
        let committed = key(1);
        let aborted = key(2);
        {
            let store = RedbStore::open(&path, 1024 * 1024).unwrap();
            store
                .put_embedding(write(&committed, [1; 32], &[1.0]))
                .unwrap();
            let transaction = store.database.begin_write().unwrap();
            {
                let mut table = transaction.open_table(EMBEDDINGS).unwrap();
                let encoded = encode_embedding([2; 32], &[2.0], 10, None).unwrap();
                table
                    .insert(aborted.as_slice(), encoded.as_slice())
                    .unwrap();
            }
            drop(transaction);
        }
        let reopened = RedbStore::open(&path, 1024 * 1024).unwrap();
        assert!(reopened
            .get_embedding(&committed, [1; 32], 1)
            .unwrap()
            .is_some());
        assert!(reopened
            .get_embedding(&aborted, [2; 32], 1)
            .unwrap()
            .is_none());
    }

    #[test]
    fn incompatible_schema_has_a_clear_rebuild_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("semantic.redb");
        {
            let store = RedbStore::open(&path, 1024 * 1024).unwrap();
            let transaction = store.database.begin_write().unwrap();
            {
                let mut metadata = transaction.open_table(METADATA).unwrap();
                metadata
                    .insert(SCHEMA_KEY, 999_u32.to_le_bytes().as_slice())
                    .unwrap();
            }
            transaction.commit().unwrap();
        }
        let error = match RedbStore::open(&path, 1024 * 1024) {
            Ok(_) => panic!("incompatible schema unexpectedly opened"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("incompatible redb schema"));
    }

    #[test]
    fn job_checkpoint_rejects_gaps_and_recipe_changes() {
        let directory = tempfile::tempdir().unwrap();
        let store = RedbStore::open(&directory.path().join("semantic.redb"), 1024 * 1024).unwrap();
        let initial = JobRecord {
            id: "resume".into(),
            protocol_version: 1,
            recipe: [1; 32],
            next_batch: 0,
            total_batches: 3,
            retries: 0,
            max_retries: 2,
            state: JobState::Pending,
            updated_at: 1,
        };
        store.commit_job_batch(&initial, &[]).unwrap();
        let mut gap = initial.clone();
        gap.next_batch = 2;
        assert!(store.commit_job_batch(&gap, &[]).is_err());
        let mut mismatch = initial;
        mismatch.recipe = [2; 32];
        assert!(store.commit_job_batch(&mismatch, &[]).is_err());
    }

    #[test]
    #[ignore = "microbenchmark; run at the epic acceptance boundary"]
    fn redb_cache_hit_microbenchmark() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            RedbStore::open(&directory.path().join("semantic.redb"), 8 * 1024 * 1024).unwrap();
        let key = key(4);
        store
            .put_embedding(write(&key, [4; 32], &[0.25; 384]))
            .unwrap();
        let rss_before = crate::util::current_rss_bytes();
        let started = std::time::Instant::now();
        for _ in 0..10_000 {
            assert!(store.get_embedding(&key, [4; 32], 384).unwrap().is_some());
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "10k redb cache hits took {:?}",
            started.elapsed()
        );
        if let (Some(before), Some(after)) = (rss_before, crate::util::current_rss_bytes()) {
            assert!(
                after.saturating_sub(before) < 64 * 1024 * 1024,
                "cache-hit RSS grew by {} bytes",
                after.saturating_sub(before)
            );
        }
    }
}

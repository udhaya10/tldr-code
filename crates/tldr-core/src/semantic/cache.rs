//! Per-record redb embedding cache with one-time rkyv migration.
//!
//! Each `put` is an independent ACID transaction. `flush` remains as an API
//! compatibility boundary, but no longer serializes or rewrites a corpus-sized
//! map.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::semantic::redb_migration::MigratedEmbedding;
use crate::semantic::redb_store::{EmbeddingWrite, RedbStore, DEFAULT_REDB_CACHE_BYTES};
use crate::semantic::types::{CacheConfig, CacheStats, CodeChunk, EmbeddingModel};
use crate::semantic::{EmbeddingCacheIdentity, EmbeddingRecipeId};
use crate::{TldrError, TldrResult};

const REDB_FILE: &str = "cache.redb";
const MIN_REDB_CACHE_BYTES: usize = 1024 * 1024;

fn embed_schema_version() -> &'static str {
    let enrich = std::env::var("TLDR_ENRICH")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if enrich {
        "enriched-v3-structural"
    } else {
        "raw-v3-structural"
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
struct CacheKey {
    recipe: [u8; 32],
    revision: [u8; 32],
}

impl CacheKey {
    fn from_document(document: &str, recipe: &EmbeddingRecipeId) -> Self {
        let identity = EmbeddingCacheIdentity::new(recipe, document);
        Self {
            recipe: identity.recipe,
            revision: identity.revision.0,
        }
    }

    fn to_bytes(self) -> [u8; 64] {
        let mut bytes = [0; 64];
        bytes[..32].copy_from_slice(&self.recipe);
        bytes[32..].copy_from_slice(&self.revision);
        bytes
    }

    fn from_legacy_string(value: &str) -> TldrResult<Self> {
        let (recipe, revision) = value
            .split_once(':')
            .ok_or_else(|| cache_error("legacy cache key has no recipe/revision separator"))?;
        Ok(Self {
            recipe: decode_hex_32(recipe)?,
            revision: decode_hex_32(revision)?,
        })
    }
}

/// Embedding cache backed by direct redb record access.
pub struct EmbeddingCache {
    config: CacheConfig,
    store: RedbStore,
    stats: CacheStats,
    write_error: Option<String>,
    database_path: PathBuf,
}

impl EmbeddingCache {
    /// Open or create the redb cache, recovering incompatible/corrupt database
    /// files to a timestamped rebuild artifact and importing a valid legacy
    /// rkyv generation exactly once (idempotently after interrupted imports).
    pub fn open(config: CacheConfig) -> TldrResult<Self> {
        std::fs::create_dir_all(&config.cache_dir)?;
        let database_path = config.cache_dir.join(REDB_FILE);
        let configured_max = config.max_size_mb.saturating_mul(1024 * 1024);
        let redb_cache_bytes = configured_max.clamp(MIN_REDB_CACHE_BYTES, DEFAULT_REDB_CACHE_BYTES);
        let store = match RedbStore::open(&database_path, redb_cache_bytes) {
            Ok(store) => store,
            Err(error) if database_path.exists() => {
                let recovered = recovered_database_path(&database_path);
                std::fs::rename(&database_path, &recovered).map_err(|rename_error| {
                    cache_error(format!(
                        "{error}; failed to preserve incompatible database as {}: {rename_error}",
                        recovered.display()
                    ))
                })?;
                RedbStore::open(&database_path, redb_cache_bytes)?
            }
            Err(error) => return Err(error),
        };
        // Always retry while a legacy generation remains. A crash may occur
        // after some idempotent redb upserts but before the legacy file is
        // removed; gating on an empty redb table would strand that migration.
        crate::semantic::redb_migration::migrate_latest(&config.cache_dir, |entry| {
            migrate_entry(&store, entry)
        })?;
        let store_stats = store.stats()?;
        Ok(Self {
            config,
            stats: CacheStats {
                entries: store_stats.embeddings,
                size_bytes: store_stats.embedding_bytes,
                hit_rate: 0.0,
            },
            store,
            write_error: None,
            database_path,
        })
    }

    /// Retained compatibility hook; content-addressed keys are path-free.
    pub fn set_key_root(&mut self, _root: &Path) {}

    /// Get an embedding for raw chunk content.
    pub fn get(&mut self, chunk: &CodeChunk, model: EmbeddingModel) -> Option<Vec<f32>> {
        let recipe = EmbeddingRecipeId::for_document(model, embed_schema_version());
        self.get_document(chunk, &chunk.content, &recipe)
    }

    /// Get an embedding by exact composed input and complete recipe.
    pub fn get_document(
        &mut self,
        _chunk: &CodeChunk,
        document: &str,
        recipe: &EmbeddingRecipeId,
    ) -> Option<Vec<f32>> {
        let dimensions = dimensions_for_recipe(recipe)?;
        let key = CacheKey::from_document(document, recipe).to_bytes();
        let record = match self
            .store
            .get_embedding(&key, recipe.fingerprint(), dimensions)
        {
            Ok(record) => record,
            Err(error) => {
                // A corrupt/incompatible record is a rebuildable cache miss.
                let _ = self.store.remove_embedding(&key);
                self.refresh_counts();
                eprintln!("[tldr-warn] removed invalid embedding cache record: {error}");
                None
            }
        };
        let result = record.and_then(|record| {
            if entry_is_valid(record.cached_at, self.config.ttl_days) {
                Some(record.vector)
            } else {
                let _ = self.store.remove_embedding(&key);
                self.refresh_counts();
                None
            }
        });
        self.stats.hit_rate = self.calculate_hit_rate(result.is_some());
        result
    }

    /// Store an embedding for raw chunk content.
    pub fn put(&mut self, chunk: &CodeChunk, embedding: Vec<f32>, model: EmbeddingModel) {
        let recipe = EmbeddingRecipeId::for_document(model, embed_schema_version());
        self.put_document(chunk, &chunk.content, embedding, &recipe);
    }

    /// Store or replace one embedding record immediately.
    pub fn put_document(
        &mut self,
        chunk: &CodeChunk,
        document: &str,
        embedding: Vec<f32>,
        recipe: &EmbeddingRecipeId,
    ) {
        if self.write_error.is_some() {
            return;
        }
        let expected_dimensions = match dimensions_for_recipe(recipe) {
            Some(dimensions) => dimensions,
            None => {
                self.write_error = Some("unknown model in embedding recipe".into());
                return;
            }
        };
        if embedding.len() != expected_dimensions {
            self.write_error = Some(format!(
                "embedding dimension mismatch: expected {expected_dimensions}, found {}",
                embedding.len()
            ));
            return;
        }
        let max_bytes = self.config.max_size_mb.saturating_mul(1024 * 1024);
        let embedding_bytes = embedding.len().saturating_mul(std::mem::size_of::<f32>());
        if embedding_bytes > max_bytes {
            self.write_error = Some(format!(
                "one embedding requires {embedding_bytes} bytes, exceeding configured cache maximum of {}MB",
                self.config.max_size_mb
            ));
            return;
        }
        let key = CacheKey::from_document(document, recipe).to_bytes();
        let now = unix_seconds();
        let file_mtime = std::fs::metadata(&chunk.file_path)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs());
        let previous_bytes = self
            .store
            .get_embedding(&key, recipe.fingerprint(), expected_dimensions)
            .ok()
            .flatten()
            .map(|record| record.vector.len() * std::mem::size_of::<f32>());
        if let Err(error) = self.store.put_embedding(EmbeddingWrite {
            key: &key,
            recipe: recipe.fingerprint(),
            vector: &embedding,
            cached_at: now,
            file_mtime,
        }) {
            self.write_error = Some(error.to_string());
            return;
        }
        match previous_bytes {
            Some(bytes) => {
                self.stats.size_bytes = self.stats.size_bytes.saturating_sub(bytes);
            }
            None => self.stats.entries += 1,
        }
        self.stats.size_bytes = self.stats.size_bytes.saturating_add(embedding_bytes);
        while self.stats.size_bytes > max_bytes {
            match self.store.remove_oldest_embedding() {
                Ok(Some((_, bytes))) => {
                    self.stats.entries = self.stats.entries.saturating_sub(1);
                    self.stats.size_bytes = self.stats.size_bytes.saturating_sub(bytes);
                }
                Ok(None) => break,
                Err(error) => {
                    self.write_error = Some(error.to_string());
                    break;
                }
            }
        }
    }

    /// Compatibility commit boundary. Writes are already durable per record;
    /// this surfaces any deferred error from the infallible legacy `put` API.
    pub fn flush(&mut self) -> TldrResult<()> {
        match self.write_error.take() {
            Some(error) => Err(cache_error(error)),
            None => Ok(()),
        }
    }

    /// Evict all records whose TTL has elapsed.
    pub fn evict_stale(&mut self) -> usize {
        let now = unix_seconds();
        let ttl_seconds = self.config.ttl_days as u64 * 24 * 60 * 60;
        let keys = match self.store.embedding_keys_older_than(now, ttl_seconds) {
            Ok(keys) => keys,
            Err(error) => {
                self.write_error = Some(error.to_string());
                return 0;
            }
        };
        let mut removed = 0;
        for key in keys {
            match self.store.remove_embedding(&key) {
                Ok(true) => removed += 1,
                Ok(false) => {}
                Err(error) => {
                    self.write_error = Some(error.to_string());
                    break;
                }
            }
        }
        self.refresh_counts();
        removed
    }

    /// Current entry, payload-size, and hit-rate counters.
    pub fn stats(&self) -> &CacheStats {
        &self.stats
    }

    /// Number of embedding records.
    pub fn len(&self) -> usize {
        self.stats.entries
    }

    /// Whether the embedding table is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Configured redb page-cache budget.
    pub fn redb_cache_size_bytes(&self) -> usize {
        self.store.cache_size_bytes()
    }

    /// Active redb database path.
    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    fn calculate_hit_rate(&self, hit: bool) -> f64 {
        let alpha = 0.1;
        if hit {
            self.stats.hit_rate * (1.0 - alpha) + alpha
        } else {
            self.stats.hit_rate * (1.0 - alpha)
        }
    }

    fn refresh_counts(&mut self) {
        match self.store.stats() {
            Ok(stats) => {
                self.stats.entries = stats.embeddings;
                self.stats.size_bytes = stats.embedding_bytes;
            }
            Err(error) => self.write_error = Some(error.to_string()),
        }
    }
}

impl Drop for EmbeddingCache {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

fn migrate_entry(store: &RedbStore, entry: MigratedEmbedding) -> TldrResult<()> {
    let key = CacheKey::from_legacy_string(&entry.key)?.to_bytes();
    let recipe: [u8; 32] = key[..32]
        .try_into()
        .map_err(|_| cache_error("legacy recipe fingerprint is not 32 bytes"))?;
    store.put_embedding(EmbeddingWrite {
        key: &key,
        recipe,
        vector: &entry.embedding,
        cached_at: entry.cached_at,
        file_mtime: entry.file_mtime,
    })
}

fn dimensions_for_recipe(recipe: &EmbeddingRecipeId) -> Option<usize> {
    [
        EmbeddingModel::ArcticXS,
        EmbeddingModel::ArcticS,
        EmbeddingModel::ArcticM,
        EmbeddingModel::ArcticMLong,
        EmbeddingModel::ArcticL,
    ]
    .into_iter()
    .find(|model| model.model_name() == recipe.model_id)
    .map(|model| model.dimensions())
}

fn entry_is_valid(cached_at: u64, ttl_days: u32) -> bool {
    let ttl_seconds = ttl_days as u64 * 24 * 60 * 60;
    unix_seconds().saturating_sub(cached_at) < ttl_seconds
}

fn recovered_database_path(path: &Path) -> PathBuf {
    let timestamp = unix_seconds();
    path.with_extension(format!("redb.rebuild-{timestamp}"))
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn decode_hex_32(value: &str) -> TldrResult<[u8; 32]> {
    if value.len() != 64 {
        return Err(cache_error("legacy key component is not 64 hex digits"));
    }
    let mut output = [0; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_nibble(pair[0])?;
        let low = decode_nibble(pair[1])?;
        output[index] = (high << 4) | low;
    }
    Ok(output)
}

fn decode_nibble(value: u8) -> TldrResult<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(cache_error("legacy key contains non-hex data")),
    }
}

fn cache_error(message: impl Into<String>) -> TldrError {
    TldrError::Embedding(format!("embedding cache: {}", message.into()))
}

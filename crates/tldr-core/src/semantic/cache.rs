//! Embedding cache with rkyv persistence and file locking
//!
//! This module provides persistent caching of embeddings to avoid
//! re-computing embeddings for unchanged code. Key features:
//!
//! - Compact rkyv binary persistence for fast loading and flushing
//
// TLDR-AUDIT(TLDR-k4q): REGRESSION + wrong tool for the vectors. llm-tldr
//   persisted a binary `.faiss` index (semantic.py:1072,1134); this rewrite
//   stores embeddings as JSON — floats as ASCII text, the whole file parsed into
//   RAM on every cold run. JSON is the worst format for dense f32 arrays.
//   DIRECTION (see TLDR-7kf): once `usearch` owns the vectors, its binary
//   `save`/mmap `view` REPLACES vector persistence entirely — delete the
//   embedding-blob half of this cache. What remains is a small METADATA SIDECAR
//   (key -> {path, lines, snippet, content_hash, file_mtime}) that usearch does
//   NOT store. For that sidecar, JSON is actually fine: it's metadata-only (no
//   float arrays) and the daemon parses it once into memory (matches the
//   documented daemon-LRU caching model in ARCHITECTURE.md), so format has zero
//   query-time cost. The invalidation logic below (content-hash + mtime) is good
//   and should be preserved in the sidecar. See epic TLDR-blm.
//! - File locking via `fs2` for concurrent access safety
//! - TTL-based expiration checked on every read (P0 mitigation)
//! - Content hash + function identity in cache key (P0 mitigation)
//! - Atomic writes with temp file + rename pattern (P1 mitigation)
//! - File mtime validation for change detection (P1 mitigation)
//!
//! # Cache Key Structure
//!
//! Cache keys combine multiple factors to ensure correct invalidation:
//! - File path (relative to project root)
//! - Function name (if function-level chunk)
//! - Content hash (MD5 of source code)
//! - Embedding model name
//!
//! # Example
//!
//! ```rust,ignore
//! use tldr_core::semantic::{EmbeddingCache, CacheConfig, CodeChunk, EmbeddingModel};
//!
//! let config = CacheConfig::default();
//! let mut cache = EmbeddingCache::open(config)?;
//!
//! // Check cache
//! if let Some(embedding) = cache.get(&chunk, EmbeddingModel::ArcticM) {
//!     println!("Cache hit!");
//! } else {
//!     // Compute embedding...
//!     cache.put(&chunk, embedding, EmbeddingModel::ArcticM);
//! }
//!
//! // Flush to disk
//! cache.flush()?;
//! ```

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use fs2::FileExt;
use memmap2::{Mmap, MmapOptions};
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};

use crate::semantic::types::{CacheConfig, CacheStats, CodeChunk, EmbeddingModel};
use crate::semantic::{EmbeddingCacheIdentity, EmbeddingRecipeId};
use crate::TldrResult;

/// Cache key combining content hash, function identity, and model
///
/// P0 Mitigation (premortem 1.2): Include function identity in cache key,
/// not just content hash. This prevents hash collisions when two functions
/// have identical content but different names (copy-paste code).
/// Version tag for the embedding-INPUT recipe (the text fed to the embedder),
/// distinct from the model. Folded into the cache key so vectors produced under
/// one recipe are never served under another. Reflects the actual recipe used:
/// raw source vs enriched text (gated by TLDR_ENRICH in index.rs). TLDR-lwg.
///
/// TODO(TLDR-blm Phase 2): when enrichment is promoted from an env gate to a
/// BuildOptions field, derive this from that field instead of re-reading env.
fn embed_schema_version() -> &'static str {
    let enrich = std::env::var("TLDR_ENRICH")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if enrich {
        "enriched-v3-structural"
    } else {
        "raw-v3-structural"
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
struct CacheKey {
    /// Fingerprint of pipeline/model/tokenizer/mode/pooling/normalization.
    recipe: [u8; 32],
    /// BLAKE3 hash of the exact composed embedding document.
    revision: [u8; 32],
}

impl CacheKey {
    #[cfg(test)]
    fn from_chunk(chunk: &CodeChunk, model: EmbeddingModel, _key_root: &Path) -> Self {
        let recipe = EmbeddingRecipeId::for_document(model, embed_schema_version());
        Self::from_document(&chunk.content, &recipe)
    }

    /// Create a cache key from the complete recipe and exact model input.
    fn from_document(document: &str, recipe: &EmbeddingRecipeId) -> Self {
        let identity = EmbeddingCacheIdentity::new(recipe, document);
        Self {
            recipe: identity.recipe,
            revision: identity.revision.0,
        }
    }

    /// Convert to a string key for HashMap storage
    fn to_key_string(&self) -> String {
        format!("{}:{}", hex_bytes(&self.recipe), hex_bytes(&self.revision))
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

/// Cached embedding entry
#[derive(
    Archive, Debug, Clone, PartialEq, RkyvDeserialize, RkyvSerialize, Serialize, Deserialize,
)]
struct CacheEntry {
    /// The embedding vector
    embedding: Vec<f32>,
    /// Unix timestamp when cached
    cached_at: u64,
    /// File modification time when cached (P1 mitigation)
    file_mtime: Option<u64>,
}

type DiskMap = HashMap<String, CacheEntry>;
type ArchivedDiskMap = rkyv::Archived<DiskMap>;

const CACHE_FILE_PREFIX: &str = "cache.v1.";
const CACHE_FILE_SUFFIX: &str = ".rkyv";
const CACHE_LOCK_FILE: &str = "cache.lock";
const RETAINED_GENERATIONS: usize = 2;

struct ValidatedMmap {
    mmap: Mmap,
}

impl ValidatedMmap {
    fn open(path: &Path) -> TldrResult<Self> {
        let file = File::open(path)?;
        if file.metadata()?.len() == 0 {
            return Err(Self::parse_error(path, "empty cache archive"));
        }

        // SAFETY: published cache generations are immutable and are never
        // truncated or rewritten. The mapping starts at offset zero, whose
        // page alignment satisfies the archived root's alignment.
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        rkyv::access::<ArchivedDiskMap, rkyv::rancor::Error>(&mmap[..]).map_err(|e| {
            Self::parse_error(path, format!("cache archive validation failed: {e}"))
        })?;

        Ok(Self { mmap })
    }

    fn root(&self) -> &ArchivedDiskMap {
        // SAFETY: `open` validated these exact bytes, the immutable mmap owns
        // them for this borrow, and published generations are never modified.
        unsafe { rkyv::access_unchecked::<ArchivedDiskMap>(&self.mmap[..]) }
    }

    fn parse_error(path: &Path, message: impl Into<String>) -> crate::TldrError {
        crate::TldrError::ParseError {
            file: path.to_path_buf(),
            line: None,
            message: message.into(),
        }
    }
}

/// Embedding cache with file locking for concurrent access
///
/// Provides persistent storage of embeddings with automatic invalidation
/// based on content hash changes, TTL expiration, and file modification.
///
/// # P0 Mitigations
///
/// - File locking with `fs2` for concurrent writes (premortem pass 2, 5.1)
/// - TTL check on every read, not just eviction (premortem pass 3, 2.1)
/// - Function identity in cache key (premortem pass 3, 1.2)
/// - Atomic writes with temp file + rename (premortem pass 3, 3.2)
pub struct EmbeddingCache {
    /// Cache configuration
    config: CacheConfig,
    /// Immutable, validated zero-copy snapshot of the last committed generation.
    base: Option<ValidatedMmap>,
    /// Entries added or replaced since the base generation was opened.
    overlay: DiskMap,
    /// Keys removed from the logical cache and the base entry observed when
    /// removed. Flush applies a tombstone only if the latest generation still
    /// contains that exact entry, so a stale process cannot delete a refresh.
    tombstones: HashMap<String, Option<CacheEntry>>,
    /// Cache statistics
    stats: CacheStats,
    /// Dirty flag for lazy writes
    dirty: bool,
}

impl EmbeddingCache {
    /// Open or create a cache at the configured location
    ///
    /// Creates the cache directory if it doesn't exist and loads
    /// any existing cache entries from disk.
    ///
    /// # P0 Mitigations
    ///
    /// - File locking with fs2 for concurrent writes
    /// - TTL check on every read
    /// - Atomic writes with temp file + rename
    ///
    /// # Errors
    ///
    /// Returns an error if the cache directory cannot be created
    /// or the cache file is corrupted.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use tldr_core::semantic::{EmbeddingCache, CacheConfig};
    ///
    /// let config = CacheConfig::default();
    /// let cache = EmbeddingCache::open(config)?;
    /// ```
    pub fn open(config: CacheConfig) -> TldrResult<Self> {
        fs::create_dir_all(&config.cache_dir)?;

        let lock = Self::open_lock_file(&config.cache_dir)?;
        lock.lock_exclusive()?;
        Self::cleanup_temp_files(&config.cache_dir);
        let base = Self::open_latest_valid_generation(&config.cache_dir);
        FileExt::unlock(&lock)?;

        let (entries, size_bytes) = base
            .as_ref()
            .map(|snapshot| {
                let root = snapshot.root();
                let size = root
                    .iter()
                    .map(|(_, entry)| entry.embedding.len() * std::mem::size_of::<f32>())
                    .sum();
                (root.len(), size)
            })
            .unwrap_or((0, 0));

        Ok(Self {
            config,
            stats: CacheStats {
                entries,
                size_bytes,
                hit_rate: 0.0,
            },
            base,
            overlay: HashMap::new(),
            tombstones: HashMap::new(),
            dirty: false,
        })
    }

    /// Retained compatibility hook; content-addressed v2 keys are path-free.
    ///
    /// `SemanticIndex::build` calls this with the index root so keys become
    /// root-relative. The current cache identity uses only the exact document
    /// revision plus the full embedding recipe, so source roots cannot diverge.
    pub fn set_key_root(&mut self, _root: &Path) {}

    /// Clean up orphaned temp files from previous crashes
    fn cleanup_temp_files(cache_dir: &Path) {
        if let Ok(entries) = fs::read_dir(cache_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(CACHE_FILE_PREFIX) && name.ends_with(".tmp") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }

    fn open_lock_file(cache_dir: &Path) -> TldrResult<File> {
        Ok(OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(cache_dir.join(CACHE_LOCK_FILE))?)
    }

    fn generation_from_name(name: &str) -> Option<u64> {
        name.strip_prefix(CACHE_FILE_PREFIX)?
            .strip_suffix(CACHE_FILE_SUFFIX)?
            .parse()
            .ok()
    }

    fn generation_files(cache_dir: &Path) -> Vec<(u64, PathBuf)> {
        let mut files: Vec<_> = fs::read_dir(cache_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let generation = Self::generation_from_name(&name.to_string_lossy())?;
                Some((generation, entry.path()))
            })
            .collect();
        files.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        files
    }

    fn open_latest_valid_generation(cache_dir: &Path) -> Option<ValidatedMmap> {
        Self::generation_files(cache_dir)
            .into_iter()
            .find_map(|(_, path)| ValidatedMmap::open(&path).ok())
    }

    fn archived_entry(&self, key: &str) -> Option<&rkyv::Archived<CacheEntry>> {
        self.base.as_ref()?.root().get(key)
    }

    fn deserialize_archived_entry(entry: &rkyv::Archived<CacheEntry>) -> TldrResult<CacheEntry> {
        rkyv::deserialize::<CacheEntry, rkyv::rancor::Error>(entry).map_err(|e| {
            ValidatedMmap::parse_error(
                Path::new("<embedding-cache>"),
                format!("cache entry deserialization failed: {e}"),
            )
        })
    }

    fn entry_is_valid(entry: &CacheEntry, ttl_days: u32) -> bool {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let ttl_secs = ttl_days as u64 * 24 * 60 * 60;
        if now.saturating_sub(entry.cached_at) >= ttl_secs {
            return false;
        }

        true
    }

    /// Get embedding from cache
    ///
    /// Returns `None` if:
    /// - Entry not found
    /// - TTL expired (P0: check on every read)
    /// - Content changed (hash mismatch)
    /// - File modified since caching (P1: mtime validation)
    ///
    /// # Arguments
    ///
    /// * `chunk` - The code chunk to look up
    /// * `model` - The embedding model that was used
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// if let Some(embedding) = cache.get(&chunk, EmbeddingModel::ArcticM) {
    ///     // Use cached embedding
    /// }
    /// ```
    pub fn get(&mut self, chunk: &CodeChunk, model: EmbeddingModel) -> Option<Vec<f32>> {
        let recipe = EmbeddingRecipeId::for_document(model, embed_schema_version());
        self.get_document(chunk, &chunk.content, &recipe)
    }

    /// Get an embedding by its exact composed input and complete recipe.
    pub fn get_document(
        &mut self,
        _chunk: &CodeChunk,
        document: &str,
        recipe: &EmbeddingRecipeId,
    ) -> Option<Vec<f32>> {
        let key = CacheKey::from_document(document, recipe);
        let key_str = key.to_key_string();

        if self.tombstones.contains_key(&key_str) {
            self.stats.hit_rate = self.calculate_hit_rate(false);
            return None;
        }

        let result = if let Some(entry) = self.overlay.get(&key_str) {
            Self::entry_is_valid(entry, self.config.ttl_days).then(|| entry.embedding.clone())
        } else {
            self.archived_entry(&key_str)
                .and_then(|entry| Self::deserialize_archived_entry(entry).ok())
                .filter(|entry| Self::entry_is_valid(entry, self.config.ttl_days))
                .map(|entry| entry.embedding)
        };

        self.stats.hit_rate = self.calculate_hit_rate(result.is_some());
        result
    }

    /// Calculate hit rate (simple moving average approximation)
    fn calculate_hit_rate(&self, hit: bool) -> f64 {
        // Simple exponential moving average
        let alpha = 0.1;
        if hit {
            self.stats.hit_rate * (1.0 - alpha) + alpha
        } else {
            self.stats.hit_rate * (1.0 - alpha)
        }
    }

    /// Store embedding in cache
    ///
    /// Stores the embedding with the current timestamp and file mtime.
    /// The cache is marked dirty and will be flushed on next `flush()` call
    /// or when the cache is dropped.
    ///
    /// # Arguments
    ///
    /// * `chunk` - The code chunk
    /// * `embedding` - The embedding vector
    /// * `model` - The embedding model used
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// cache.put(&chunk, embedding, EmbeddingModel::ArcticM);
    /// ```
    pub fn put(&mut self, chunk: &CodeChunk, embedding: Vec<f32>, model: EmbeddingModel) {
        let recipe = EmbeddingRecipeId::for_document(model, embed_schema_version());
        self.put_document(chunk, &chunk.content, embedding, &recipe);
    }

    /// Store an embedding by its exact composed input and complete recipe.
    pub fn put_document(
        &mut self,
        chunk: &CodeChunk,
        document: &str,
        embedding: Vec<f32>,
        recipe: &EmbeddingRecipeId,
    ) {
        let key = CacheKey::from_document(document, recipe);
        let key_str = key.to_key_string();

        // Get file mtime for change detection
        let file_mtime = fs::metadata(&chunk.file_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let old_size = self
            .overlay
            .get(&key_str)
            .map(|entry| entry.embedding.len() * std::mem::size_of::<f32>())
            .or_else(|| {
                (!self.tombstones.contains_key(&key_str))
                    .then(|| {
                        self.archived_entry(&key_str)
                            .map(|entry| entry.embedding.len() * std::mem::size_of::<f32>())
                    })
                    .flatten()
            });
        let entry_size = embedding.len() * std::mem::size_of::<f32>();
        if let Some(old_size) = old_size {
            self.stats.size_bytes = self.stats.size_bytes.saturating_sub(old_size);
        } else {
            self.stats.entries += 1;
        }
        self.stats.size_bytes += entry_size;

        self.tombstones.remove(&key_str);
        self.overlay.insert(
            key_str,
            CacheEntry {
                embedding,
                cached_at: now,
                file_mtime,
            },
        );

        self.dirty = true;
    }

    /// Flush cache to disk
    ///
    /// Uses atomic write pattern: write to temp file, then rename.
    /// This prevents corruption if the process crashes mid-write.
    ///
    /// # P1 Mitigation
    ///
    /// Atomic writes with temp file + rename (premortem pass 3, 3.2)
    ///
    /// # Errors
    ///
    /// Returns an error if the cache file cannot be written.
    pub fn flush(&mut self) -> TldrResult<()> {
        if !self.dirty {
            return Ok(());
        }

        let lock = Self::open_lock_file(&self.config.cache_dir)?;
        lock.lock_exclusive()?;
        let latest = Self::open_latest_valid_generation(&self.config.cache_dir);
        let mut merged: DiskMap = latest
            .as_ref()
            .map(|snapshot| {
                rkyv::deserialize::<DiskMap, rkyv::rancor::Error>(snapshot.root()).map_err(|e| {
                    ValidatedMmap::parse_error(
                        Path::new("<embedding-cache>"),
                        format!("cache snapshot deserialization failed: {e}"),
                    )
                })
            })
            .transpose()?
            .unwrap_or_default();

        for (key, observed_entry) in &self.tombstones {
            if merged.get(key) == observed_entry.as_ref() {
                merged.remove(key);
            }
        }
        for (key, entry) in &self.overlay {
            merged.insert(key.clone(), entry.clone());
        }

        let next_generation = Self::generation_files(&self.config.cache_dir)
            .first()
            .map(|(generation, _)| {
                generation.checked_add(1).ok_or_else(|| {
                    ValidatedMmap::parse_error(
                        &self.config.cache_dir,
                        "cache generation counter overflow",
                    )
                })
            })
            .transpose()?
            .unwrap_or(1);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let temp_file = self.config.cache_dir.join(format!(
            "{CACHE_FILE_PREFIX}{}.{}.{}.tmp",
            next_generation,
            std::process::id(),
            nanos
        ));
        let cache_file = self.config.cache_dir.join(format!(
            "{CACHE_FILE_PREFIX}{next_generation:020}{CACHE_FILE_SUFFIX}"
        ));

        let write_result = (|| -> TldrResult<()> {
            let mut bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&merged).map_err(|e| {
                ValidatedMmap::parse_error(&temp_file, format!("failed to serialize cache: {e}"))
            })?;
            let max_bytes = self.config.max_size_mb.saturating_mul(1024 * 1024);
            while bytes.len() > max_bytes {
                if merged.len() <= 1 {
                    return Err(crate::TldrError::Embedding(format!(
                        "one embedding cache entry requires {} bytes, exceeding configured maximum of {}MB",
                        bytes.len(),
                        self.config.max_size_mb
                    )));
                }

                let excess = bytes.len() - max_bytes;
                let remove_count = (((merged.len() as u128 * excess as u128)
                    .div_ceil(bytes.len() as u128)) as usize)
                    .max(1)
                    .min(merged.len() - 1);
                let mut oldest: Vec<_> = merged
                    .iter()
                    .map(|(key, entry)| (key.clone(), entry.cached_at))
                    .collect();
                oldest.sort_unstable_by_key(|(_, cached_at)| *cached_at);
                for (key, _) in oldest.into_iter().take(remove_count) {
                    merged.remove(&key);
                }
                bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&merged).map_err(|e| {
                    ValidatedMmap::parse_error(
                        &temp_file,
                        format!("failed to serialize compacted cache: {e}"),
                    )
                })?;
            }
            rkyv::access::<ArchivedDiskMap, rkyv::rancor::Error>(&bytes[..]).map_err(|e| {
                ValidatedMmap::parse_error(
                    &temp_file,
                    format!("serialized cache validation failed: {e}"),
                )
            })?;

            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp_file)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temp_file, &cache_file)?;
            Self::sync_dir(&self.config.cache_dir)?;
            Ok(())
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temp_file);
            return Err(error);
        }

        self.base = Some(ValidatedMmap::open(&cache_file)?);
        self.stats.entries = merged.len();
        self.stats.size_bytes = merged
            .values()
            .map(|entry| entry.embedding.len() * std::mem::size_of::<f32>())
            .sum();
        self.overlay.clear();
        self.tombstones.clear();
        self.dirty = false;
        let _ = fs::remove_file(self.config.cache_dir.join("cache.json"));
        let _ = fs::remove_file(self.config.cache_dir.join("cache.bin"));
        Self::cleanup_old_generations(&self.config.cache_dir);
        Ok(())
    }

    /// Evict entries older than TTL
    ///
    /// Removes all cache entries that have exceeded the configured TTL.
    /// Returns the number of entries evicted.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let evicted = cache.evict_stale();
    /// println!("Evicted {} stale entries", evicted);
    /// ```
    pub fn evict_stale(&mut self) -> usize {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let ttl_secs = self.config.ttl_days as u64 * 24 * 60 * 60;

        let mut evicted_sizes = HashMap::new();
        self.overlay.retain(|key, entry| {
            if now.saturating_sub(entry.cached_at) >= ttl_secs {
                evicted_sizes.insert(
                    key.clone(),
                    entry.embedding.len() * std::mem::size_of::<f32>(),
                );
                false
            } else {
                true
            }
        });

        if let Some(base) = &self.base {
            for (key, archived) in base.root().iter() {
                let key = key.as_str();
                if self.overlay.contains_key(key)
                    || self.tombstones.contains_key(key)
                    || evicted_sizes.contains_key(key)
                {
                    continue;
                }
                if let Ok(entry) = Self::deserialize_archived_entry(archived) {
                    if now.saturating_sub(entry.cached_at) >= ttl_secs {
                        evicted_sizes.insert(
                            key.to_string(),
                            entry.embedding.len() * std::mem::size_of::<f32>(),
                        );
                    }
                }
            }
        }

        for (key, size) in &evicted_sizes {
            let observed_base = self
                .archived_entry(key)
                .and_then(|entry| Self::deserialize_archived_entry(entry).ok());
            self.tombstones.insert(key.clone(), observed_base);
            self.stats.entries = self.stats.entries.saturating_sub(1);
            self.stats.size_bytes = self.stats.size_bytes.saturating_sub(*size);
        }
        let evicted = evicted_sizes.len();
        if evicted != 0 {
            self.dirty = true;
        }

        evicted
    }

    /// Get cache statistics
    ///
    /// Returns current cache statistics including entry count,
    /// size in bytes, and hit rate.
    pub fn stats(&self) -> &CacheStats {
        &self.stats
    }

    /// Get number of entries
    pub fn len(&self) -> usize {
        self.stats.entries
    }

    /// Check if cache is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn sync_dir(dir: &Path) -> TldrResult<()> {
        #[cfg(unix)]
        {
            File::open(dir)?.sync_all()?;
        }
        #[cfg(not(unix))]
        {
            let _ = dir;
        }
        Ok(())
    }

    fn cleanup_old_generations(cache_dir: &Path) {
        let mut valid_kept = 0;
        for (_, path) in Self::generation_files(cache_dir) {
            if ValidatedMmap::open(&path).is_ok() {
                valid_kept += 1;
                if valid_kept <= RETAINED_GENERATIONS {
                    continue;
                }
            }
            let _ = fs::remove_file(path);
        }
    }
}

impl Drop for EmbeddingCache {
    fn drop(&mut self) {
        // Best-effort flush on drop
        let _ = self.flush();
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use crate::Language;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn create_test_chunk(name: &str, content: &str) -> CodeChunk {
        CodeChunk {
            file_path: PathBuf::from(format!("test/{}.rs", name)),
            function_name: Some(name.to_string()),
            class_name: None,
            line_start: 1,
            line_end: 10,
            content: content.to_string(),
            content_hash: format!("{:x}", md5::compute(content)),
            language: Language::Rust,
            structure: Default::default(),
        }
    }

    #[test]
    fn cache_config_default_values() {
        // GIVEN: Default cache config
        let config = CacheConfig::default();

        // THEN: Should have sensible defaults
        assert!(config.cache_dir.ends_with("tldr/embeddings"));
        assert_eq!(config.max_size_mb, 500);
        assert_eq!(config.ttl_days, 30);
    }

    #[test]
    fn cache_open_creates_directory() {
        // GIVEN: A temp directory
        let temp = tempdir().unwrap();
        let cache_dir = temp.path().join("cache");

        // WHEN: We open a cache with a non-existent directory
        let config = CacheConfig {
            cache_dir: cache_dir.clone(),
            max_size_mb: 100,
            ttl_days: 7,
        };
        let _cache = EmbeddingCache::open(config).unwrap();

        // THEN: The directory should be created
        assert!(cache_dir.exists());
    }

    #[test]
    fn cache_put_get_roundtrip() {
        // GIVEN: A cache and a chunk
        let temp = tempdir().unwrap();
        let config = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 100,
            ttl_days: 7,
        };
        let mut cache = EmbeddingCache::open(config).unwrap();
        let chunk = create_test_chunk("foo", "fn foo() {}");
        let embedding = vec![0.1, 0.2, 0.3];

        // WHEN: We put and get
        cache.put(&chunk, embedding.clone(), EmbeddingModel::ArcticM);
        let result = cache.get(&chunk, EmbeddingModel::ArcticM);

        // THEN: We should get the same embedding back
        assert!(result.is_some());
        assert_eq!(result.unwrap(), embedding);
    }

    /// TLDR-atc regression: the SAME logical file indexed via a RELATIVE root
    /// (cold CLI, e.g. `crates/x/src`) and via an ABSOLUTE root (daemon
    /// canonicalizes `self.project`) must produce the SAME cache key. Before the
    /// root-relative key fix, the daemon's absolute keys never matched the cold
    /// cache's relative keys -> 100% miss -> a full re-embed on every daemon
    /// query. This locks the convergence so that regression cannot silently
    /// return (it is invisible to the suffix-matching eval; only key identity
    /// catches it).
    #[test]
    fn cache_key_is_source_position_independent() {
        let content = "fn foo() {}";
        let mk = |fp: &str| CodeChunk {
            file_path: PathBuf::from(fp),
            function_name: Some("foo".to_string()),
            class_name: None,
            line_start: 1,
            line_end: 10,
            content: content.to_string(),
            content_hash: format!("{:x}", md5::compute(content)),
            language: Language::Rust,
            structure: Default::default(),
        };

        // Cold CLI: relative root + relative chunk path.
        let rel_chunk = mk("crates/x/src/a.rs");
        let rel_key = CacheKey::from_chunk(
            &rel_chunk,
            EmbeddingModel::ArcticL,
            Path::new("crates/x/src"),
        )
        .to_key_string();

        // Daemon: absolute root + absolute chunk path (same logical file).
        let abs_chunk = mk("/Users/me/proj/crates/x/src/a.rs");
        let abs_key = CacheKey::from_chunk(
            &abs_chunk,
            EmbeddingModel::ArcticL,
            Path::new("/Users/me/proj/crates/x/src"),
        )
        .to_key_string();

        assert_eq!(
            rel_key, abs_key,
            "relative-root and absolute-root invocations must yield identical \
             cache keys; got {rel_key} vs {abs_key}"
        );
        // Source position and root are intentionally absent from v2 cache keys.
        let raw_key = CacheKey::from_chunk(&rel_chunk, EmbeddingModel::ArcticL, Path::new(""))
            .to_key_string();
        assert_eq!(raw_key, rel_key);
    }

    #[test]
    fn cache_miss_on_content_hash_change() {
        // GIVEN: A cache with an entry
        let temp = tempdir().unwrap();
        let config = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 100,
            ttl_days: 7,
        };
        let mut cache = EmbeddingCache::open(config).unwrap();
        let chunk1 = create_test_chunk("foo", "fn foo() {}");
        let embedding = vec![0.1, 0.2, 0.3];
        cache.put(&chunk1, embedding, EmbeddingModel::ArcticM);

        // WHEN: We query with a different content hash
        let chunk2 = create_test_chunk("foo", "fn foo() { return 1; }");

        // THEN: We should get a cache miss
        let result = cache.get(&chunk2, EmbeddingModel::ArcticM);
        assert!(result.is_none());
    }

    #[test]
    fn cache_miss_on_model_change() {
        // GIVEN: A cache with an entry for ArcticM
        let temp = tempdir().unwrap();
        let config = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 100,
            ttl_days: 7,
        };
        let mut cache = EmbeddingCache::open(config).unwrap();
        let chunk = create_test_chunk("foo", "fn foo() {}");
        let embedding = vec![0.1, 0.2, 0.3];
        cache.put(&chunk, embedding, EmbeddingModel::ArcticM);

        // WHEN: We query with a different model
        let result = cache.get(&chunk, EmbeddingModel::ArcticL);

        // THEN: We should get a cache miss
        assert!(result.is_none());
    }

    #[test]
    fn cache_flush_persists_to_disk() {
        // GIVEN: A cache with an entry
        let temp = tempdir().unwrap();
        let config = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 100,
            ttl_days: 7,
        };
        let chunk = create_test_chunk("foo", "fn foo() {}");
        let embedding = vec![0.1, 0.2, 0.3];

        // Put and flush
        {
            let mut cache = EmbeddingCache::open(config.clone()).unwrap();
            cache.put(&chunk, embedding.clone(), EmbeddingModel::ArcticM);
            cache.flush().unwrap();
        }

        // WHEN: We open a new cache from the same directory
        let mut cache2 = EmbeddingCache::open(config).unwrap();

        // THEN: The entry should be persisted
        let result = cache2.get(&chunk, EmbeddingModel::ArcticM);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), embedding);
    }

    #[test]
    fn cache_evict_stale_removes_old_entries() {
        // GIVEN: A cache with entries that we'll manually age
        let temp = tempdir().unwrap();
        let config = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 100,
            ttl_days: 7, // 7 days TTL
        };
        let mut cache = EmbeddingCache::open(config).unwrap();
        let chunk = create_test_chunk("foo", "fn foo() {}");
        let embedding = vec![0.1, 0.2, 0.3];

        // Put an entry
        cache.put(&chunk, embedding, EmbeddingModel::ArcticM);
        assert_eq!(cache.len(), 1);

        // Manually age the entry to be older than TTL (8 days ago)
        let key =
            CacheKey::from_chunk(&chunk, EmbeddingModel::ArcticM, Path::new("")).to_key_string();
        if let Some(entry) = cache.overlay.get_mut(&key) {
            // Set cached_at to 8 days ago
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            entry.cached_at = now - (8 * 24 * 60 * 60); // 8 days ago
        }

        // WHEN: We evict stale entries
        let evicted = cache.evict_stale();

        // THEN: The entry should be evicted (older than 7 day TTL)
        assert_eq!(evicted, 1);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn cache_stats_tracking() {
        // GIVEN: A cache
        let temp = tempdir().unwrap();
        let config = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 100,
            ttl_days: 7,
        };
        let mut cache = EmbeddingCache::open(config).unwrap();

        // Initial stats
        assert_eq!(cache.stats().entries, 0);
        assert_eq!(cache.stats().size_bytes, 0);

        // WHEN: We add entries
        let chunk1 = create_test_chunk("foo", "fn foo() {}");
        let chunk2 = create_test_chunk("bar", "fn bar() {}");
        let embedding = vec![0.1_f32, 0.2, 0.3]; // 3 floats = 12 bytes

        cache.put(&chunk1, embedding.clone(), EmbeddingModel::ArcticM);
        cache.put(&chunk2, embedding.clone(), EmbeddingModel::ArcticM);

        // THEN: Stats should be updated
        assert_eq!(cache.stats().entries, 2);
        assert_eq!(cache.stats().size_bytes, 24); // 2 * 3 * 4 bytes
    }

    #[test]
    fn cache_deduplicates_identical_documents_across_source_identity() {
        // GIVEN: Two source chunks whose exact model input is identical.
        let temp = tempdir().unwrap();
        let config = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 100,
            ttl_days: 7,
        };
        let mut cache = EmbeddingCache::open(config).unwrap();

        // Same content, different function names
        let content = "fn template() { return 1; }";
        let chunk1 = CodeChunk {
            file_path: PathBuf::from("test/foo.rs"),
            function_name: Some("foo".to_string()),
            class_name: None,
            line_start: 1,
            line_end: 10,
            content: content.to_string(),
            content_hash: format!("{:x}", md5::compute(content)),
            language: Language::Rust,
            structure: Default::default(),
        };
        let chunk2 = CodeChunk {
            file_path: PathBuf::from("test/bar.rs"),
            function_name: Some("bar".to_string()),
            class_name: None,
            line_start: 1,
            line_end: 10,
            content: content.to_string(),
            content_hash: format!("{:x}", md5::compute(content)), // Same hash!
            language: Language::Rust,
            structure: Default::default(),
        };

        let embedding1 = vec![0.1, 0.2, 0.3];
        let embedding2 = vec![0.4, 0.5, 0.6];

        // WHEN: We store both
        cache.put(&chunk1, embedding1.clone(), EmbeddingModel::ArcticM);
        cache.put(&chunk2, embedding2.clone(), EmbeddingModel::ArcticM);

        // THEN: the content-addressed cache stores one reusable vector.
        assert_eq!(cache.len(), 1);
        let result1 = cache.get(&chunk1, EmbeddingModel::ArcticM);
        let result2 = cache.get(&chunk2, EmbeddingModel::ArcticM);
        assert_eq!(result1.unwrap(), embedding2);
        assert_eq!(result2.unwrap(), embedding2);
    }

    #[test]
    fn cache_ttl_checked_on_read() {
        let temp = tempdir().unwrap();
        let config = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 100,
            ttl_days: 0,
        };
        let mut cache = EmbeddingCache::open(config).unwrap();
        let chunk = create_test_chunk("foo", "fn foo() {}");
        cache.put(&chunk, vec![0.1, 0.2, 0.3], EmbeddingModel::ArcticM);

        assert_eq!(cache.get(&chunk, EmbeddingModel::ArcticM), None);
    }

    #[test]
    fn cache_len_and_is_empty() {
        let temp = tempdir().unwrap();
        let config = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 100,
            ttl_days: 7,
        };
        let mut cache = EmbeddingCache::open(config).unwrap();

        // Initially empty
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);

        // Add entry
        let chunk = create_test_chunk("foo", "fn foo() {}");
        cache.put(&chunk, vec![0.1, 0.2], EmbeddingModel::ArcticM);

        // Not empty anymore
        assert!(!cache.is_empty());
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn cache_handles_corrupted_file() {
        // GIVEN: A corrupted cache file
        let temp = tempdir().unwrap();
        let cache_file = temp.path().join("cache.v1.00000000000000000001.rkyv");
        fs::write(&cache_file, b"not a valid rkyv archive").unwrap();

        // WHEN: We try to open the cache
        let config = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 100,
            ttl_days: 7,
        };
        let cache = EmbeddingCache::open(config);

        // THEN: It should succeed with an empty cache (graceful degradation)
        assert!(cache.is_ok());
        assert!(cache.unwrap().is_empty());
    }

    #[test]
    fn cache_removes_legacy_json_only_after_successful_flush() {
        let temp = tempdir().unwrap();
        let legacy = temp.path().join("cache.json");
        fs::write(&legacy, b"legacy cache").unwrap();

        let config = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 100,
            ttl_days: 7,
        };
        let mut cache = EmbeddingCache::open(config).unwrap();

        assert!(cache.is_empty());
        assert!(legacy.exists());

        let chunk = create_test_chunk("foo", "fn foo() {}");
        cache.put(&chunk, vec![0.1, 0.2], EmbeddingModel::ArcticM);
        cache.flush().unwrap();

        assert!(!legacy.exists());
        assert_eq!(EmbeddingCache::generation_files(temp.path()).len(), 1);
    }

    #[test]
    fn failed_flush_preserves_legacy_json_and_dirty_overlay() {
        let temp = tempdir().unwrap();
        let legacy = temp.path().join("cache.json");
        fs::write(&legacy, b"legacy cache").unwrap();
        let config = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 0,
            ttl_days: 7,
        };
        let chunk = create_test_chunk("foo", "fn foo() {}");
        let mut cache = EmbeddingCache::open(config).unwrap();
        cache.put(&chunk, vec![0.1, 0.2], EmbeddingModel::ArcticM);

        assert!(cache.flush().is_err());
        assert!(legacy.exists());
        assert!(cache.dirty);
        assert_eq!(cache.overlay.len(), 1);
    }

    #[test]
    fn cache_cleans_up_temp_files() {
        // GIVEN: A cache directory with orphaned temp files
        let temp = tempdir().unwrap();
        let temp_file = temp.path().join("cache.v1.1.123.456.tmp");
        fs::write(&temp_file, "orphaned temp file").unwrap();
        assert!(temp_file.exists());

        // WHEN: We open a cache
        let config = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 100,
            ttl_days: 7,
        };
        let _cache = EmbeddingCache::open(config).unwrap();

        // THEN: The temp file should be cleaned up
        assert!(!temp_file.exists());
    }

    #[test]
    fn reopened_cache_uses_mmap_base_without_owned_entries() {
        let temp = tempdir().unwrap();
        let config = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 100,
            ttl_days: 7,
        };
        let chunk = create_test_chunk("foo", "fn foo() {}");
        let embedding = vec![0.1, 0.2, 0.3];

        let mut writer = EmbeddingCache::open(config.clone()).unwrap();
        writer.put(&chunk, embedding.clone(), EmbeddingModel::ArcticM);
        writer.flush().unwrap();

        let mut reopened = EmbeddingCache::open(config).unwrap();
        assert!(reopened.base.is_some());
        assert!(reopened.overlay.is_empty());
        assert_eq!(
            reopened.get(&chunk, EmbeddingModel::ArcticM),
            Some(embedding)
        );
    }

    #[test]
    fn stale_instances_merge_latest_generation_on_flush() {
        let temp = tempdir().unwrap();
        let config = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 100,
            ttl_days: 7,
        };
        let chunk_a = create_test_chunk("a", "fn a() {}");
        let chunk_b = create_test_chunk("b", "fn b() {}");

        let mut first = EmbeddingCache::open(config.clone()).unwrap();
        let mut stale = EmbeddingCache::open(config.clone()).unwrap();
        first.put(&chunk_a, vec![1.0], EmbeddingModel::ArcticM);
        first.flush().unwrap();
        stale.put(&chunk_b, vec![2.0], EmbeddingModel::ArcticM);
        stale.flush().unwrap();

        let mut reopened = EmbeddingCache::open(config).unwrap();
        assert_eq!(
            reopened.get(&chunk_a, EmbeddingModel::ArcticM),
            Some(vec![1.0])
        );
        assert_eq!(
            reopened.get(&chunk_b, EmbeddingModel::ArcticM),
            Some(vec![2.0])
        );
    }

    #[test]
    fn stale_tombstone_does_not_delete_concurrent_refresh() {
        let temp = tempdir().unwrap();
        let normal = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 100,
            ttl_days: 7,
        };
        let immediate_expiry = CacheConfig {
            ttl_days: 0,
            ..normal.clone()
        };
        let chunk = create_test_chunk("foo", "fn foo() {}");

        let mut seed = EmbeddingCache::open(normal.clone()).unwrap();
        seed.put(&chunk, vec![1.0], EmbeddingModel::ArcticM);
        seed.flush().unwrap();

        let mut stale = EmbeddingCache::open(immediate_expiry).unwrap();
        assert_eq!(stale.evict_stale(), 1);

        let mut refresher = EmbeddingCache::open(normal.clone()).unwrap();
        refresher.put(&chunk, vec![2.0], EmbeddingModel::ArcticM);
        refresher.flush().unwrap();
        stale.flush().unwrap();

        let mut reopened = EmbeddingCache::open(normal).unwrap();
        assert_eq!(
            reopened.get(&chunk, EmbeddingModel::ArcticM),
            Some(vec![2.0])
        );
    }

    #[test]
    fn mmap_snapshot_satisfies_archived_root_alignment() {
        let temp = tempdir().unwrap();
        let config = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 100,
            ttl_days: 7,
        };
        let chunk = create_test_chunk("foo", "fn foo() {}");
        let mut cache = EmbeddingCache::open(config.clone()).unwrap();
        cache.put(&chunk, vec![0.1, 0.2], EmbeddingModel::ArcticM);
        cache.flush().unwrap();

        let reopened = EmbeddingCache::open(config).unwrap();
        let mmap = &reopened.base.as_ref().unwrap().mmap;
        assert_eq!(
            mmap.as_ptr() as usize % std::mem::align_of::<ArchivedDiskMap>(),
            0
        );
    }

    #[test]
    fn corrupt_newest_generation_falls_back_to_previous_snapshot() {
        let temp = tempdir().unwrap();
        let config = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 100,
            ttl_days: 7,
        };
        let chunk = create_test_chunk("foo", "fn foo() {}");
        let embedding = vec![0.1, 0.2];
        let mut cache = EmbeddingCache::open(config.clone()).unwrap();
        cache.put(&chunk, embedding.clone(), EmbeddingModel::ArcticM);
        cache.flush().unwrap();
        fs::write(
            temp.path().join("cache.v1.00000000000000000002.rkyv"),
            b"corrupt",
        )
        .unwrap();

        let mut reopened = EmbeddingCache::open(config).unwrap();
        assert_eq!(
            reopened.get(&chunk, EmbeddingModel::ArcticM),
            Some(embedding)
        );
    }

    #[test]
    fn rkyv_archive_is_smaller_than_json_for_embedding_payloads() {
        let entries: DiskMap = (0..100)
            .map(|i| {
                (
                    format!("src/file_{i}.rs:function_{i}:hash:model"),
                    CacheEntry {
                        embedding: (0..384).map(|j| ((i * 384 + j) as f32).sin()).collect(),
                        cached_at: 1_700_000_000 + i,
                        file_mtime: Some(1_700_000_000 + i),
                    },
                )
            })
            .collect();
        let json = serde_json::to_vec(&entries).unwrap();
        let archived = rkyv::to_bytes::<rkyv::rancor::Error>(&entries).unwrap();

        assert!(
            archived.len() * 2 < json.len(),
            "expected rkyv to use less than half the JSON storage: rkyv={} JSON={}",
            archived.len(),
            json.len()
        );
    }

    #[test]
    fn flush_compacts_oldest_entries_to_configured_size() {
        let temp = tempdir().unwrap();
        let config = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 1,
            ttl_days: 7,
        };
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut cache = EmbeddingCache::open(config.clone()).unwrap();
        let mut newest = None;
        for i in 0..1_000 {
            let chunk = create_test_chunk(&format!("f{i}"), &format!("fn f{i}() {{}}"));
            cache.put(&chunk, vec![i as f32; 384], EmbeddingModel::ArcticS);
            let key = CacheKey::from_chunk(&chunk, EmbeddingModel::ArcticS, Path::new(""))
                .to_key_string();
            cache.overlay.get_mut(&key).unwrap().cached_at = now + i;
            newest = Some(chunk);
        }

        cache.flush().unwrap();
        let generation = EmbeddingCache::generation_files(temp.path())
            .into_iter()
            .next()
            .unwrap()
            .1;
        assert!(fs::metadata(generation).unwrap().len() <= 1024 * 1024);

        let mut reopened = EmbeddingCache::open(config).unwrap();
        assert!(reopened.len() < 1_000);
        assert!(reopened
            .get(&newest.unwrap(), EmbeddingModel::ArcticS)
            .is_some());
    }
}

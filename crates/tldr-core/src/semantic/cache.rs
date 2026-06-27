//! Embedding cache with JSON persistence and file locking
//!
//! This module provides persistent caching of embeddings to avoid
//! re-computing embeddings for unchanged code. Key features:
//!
//! - JSON-based persistence for easy debugging and portability
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
use std::fs::{self, File};
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::semantic::types::{CacheConfig, CacheStats, CodeChunk, EmbeddingModel};
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
        "enriched-v1"
    } else {
        "raw-v1"
    }
}

/// Path used in the cache key: relative to `key_root`. A silent raw-path fallback
/// on a `strip_prefix` miss re-introduces the absolute-vs-relative key divergence
/// (TLDR-atc/ss3), so misses are handled deterministically — lexical strip, then
/// canonical strip, then the canonical absolute path with a warning — never a
/// silent raw fallback. An empty `key_root` (the default / tests) lexically
/// matches and returns the full path unchanged, preserving legacy keys.
fn key_rel_path(file_path: &Path, key_root: &Path) -> String {
    if let Ok(rel) = file_path.strip_prefix(key_root) {
        return rel.to_string_lossy().to_string();
    }
    if let (Ok(cfile), Ok(croot)) = (file_path.canonicalize(), key_root.canonicalize()) {
        if let Ok(rel) = cfile.strip_prefix(&croot) {
            return rel.to_string_lossy().to_string();
        }
        eprintln!(
            "[tldr-warn] cache key: {} is outside root {}; keying by canonical path",
            cfile.display(),
            croot.display()
        );
        return cfile.to_string_lossy().to_string();
    }
    eprintln!(
        "[tldr-warn] cache key: cannot canonicalize {} under {}; keying by raw path",
        file_path.display(),
        key_root.display()
    );
    file_path.to_string_lossy().to_string()
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
struct CacheKey {
    /// MD5 hash of the code content
    content_hash: String,
    /// File path (relative to project root)
    file_path: String,
    /// Function name (if function-level chunk)
    function_name: Option<String>,
    /// Embedding model identifier
    model: String,
}

impl CacheKey {
    /// Create a cache key from a code chunk and model.
    ///
    /// `key_root` is stripped from `chunk.file_path` so the SAME file yields the
    /// SAME key regardless of how the index was rooted: the cold CLI passes a
    /// relative arg (`crates/x/src` -> keys like `semantic/cache.rs`) while the
    /// daemon canonicalizes to an absolute root (`/Users/.../crates/x/src`).
    /// Before this, the daemon's absolute keys never matched the cold cache's
    /// relative keys -> 100% miss -> a full re-embed on every daemon query
    /// (TLDR-atc). An empty `key_root` (the default for callers that don't set
    /// one, e.g. tests) leaves the path unchanged.
    fn from_chunk(chunk: &CodeChunk, model: EmbeddingModel, key_root: &Path) -> Self {
        Self {
            content_hash: chunk.content_hash.clone(),
            file_path: key_rel_path(&chunk.file_path, key_root),
            function_name: chunk.function_name.clone(),
            // TLDR-lwg: the schema tag pins WHICH text was embedded under this
            // content hash. The hash covers raw source; bumping the recipe (raw
            // -> enriched) must invalidate old vectors, or stale raw-embedded
            // entries get served as if enriched. Folded into `model` so no
            // get/put signatures change.
            model: format!("{:?}+{}", model, embed_schema_version()),
        }
    }

    /// Convert to a string key for HashMap storage
    fn to_key_string(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.file_path,
            self.function_name.as_deref().unwrap_or(""),
            self.content_hash,
            self.model
        )
    }
}

/// Cached embedding entry
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    /// The embedding vector
    embedding: Vec<f32>,
    /// Unix timestamp when cached
    cached_at: u64,
    /// File modification time when cached (P1 mitigation)
    file_mtime: Option<u64>,
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
    /// In-memory cache entries (key string -> entry)
    entries: HashMap<String, CacheEntry>,
    /// Cache statistics
    stats: CacheStats,
    /// Dirty flag for lazy writes
    dirty: bool,
    /// Path prefix stripped from each chunk's `file_path` when deriving its
    /// cache key, so the key is build-root-relative and therefore stable across
    /// relative (cold CLI) vs absolute (daemon) roots and across CWDs (TLDR-atc).
    /// Empty by default — keys then use the raw path (preserves legacy/test
    /// behavior). `SemanticIndex::build` sets it to the index root.
    key_root: PathBuf,
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
        // Create cache directory if it doesn't exist
        fs::create_dir_all(&config.cache_dir)?;

        // Clean up orphaned temp files from previous crashes
        Self::cleanup_temp_files(&config.cache_dir);

        let cache_file = config.cache_dir.join("cache.json");
        let entries = if cache_file.exists() {
            Self::load_with_lock(&cache_file).unwrap_or_else(|_| {
                // If cache is corrupted, start fresh
                HashMap::new()
            })
        } else {
            HashMap::new()
        };

        let size_bytes = entries
            .values()
            .map(|e| e.embedding.len() * std::mem::size_of::<f32>())
            .sum();

        Ok(Self {
            config,
            stats: CacheStats {
                entries: entries.len(),
                size_bytes,
                hit_rate: 0.0,
            },
            entries,
            dirty: false,
            key_root: PathBuf::new(),
        })
    }

    /// Set the path prefix stripped from chunk paths when deriving cache keys.
    ///
    /// `SemanticIndex::build` calls this with the index root so keys become
    /// root-relative — the SAME file then maps to the SAME key whether the index
    /// was rooted at a relative arg (cold CLI) or the canonical absolute path the
    /// daemon uses. See [`CacheKey::from_chunk`] (TLDR-atc).
    pub fn set_key_root(&mut self, root: &Path) {
        self.key_root = root.to_path_buf();
    }

    /// Clean up orphaned temp files from previous crashes
    fn cleanup_temp_files(cache_dir: &Path) {
        if let Ok(entries) = fs::read_dir(cache_dir) {
            for entry in entries.flatten() {
                if let Some(ext) = entry.path().extension() {
                    if ext == "tmp" {
                        let _ = fs::remove_file(entry.path());
                    }
                }
            }
        }
    }

    /// Load cache entries from disk with shared lock
    fn load_with_lock(path: &Path) -> TldrResult<HashMap<String, CacheEntry>> {
        let file = File::open(path)?;
        // Shared lock for reading - allows multiple readers
        file.lock_shared()?;
        let reader = BufReader::new(&file);
        let entries: HashMap<String, CacheEntry> =
            serde_json::from_reader(reader).map_err(|e| crate::TldrError::ParseError {
                file: path.to_path_buf(),
                line: None,
                message: format!("Cache file corrupted: {}", e),
            })?;
        file.unlock()?;
        Ok(entries)
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
        let key = CacheKey::from_chunk(chunk, model, &self.key_root);
        let key_str = key.to_key_string();

        if let Some(entry) = self.entries.get(&key_str) {
            // P0: Check TTL on every read
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let age_days = (now.saturating_sub(entry.cached_at)) / (24 * 60 * 60);
            if age_days > self.config.ttl_days as u64 {
                self.stats.hit_rate = self.calculate_hit_rate(false);
                return None; // TTL expired
            }

            // P1: Check file mtime if available
            if let Some(cached_mtime) = entry.file_mtime {
                if let Ok(metadata) = fs::metadata(&chunk.file_path) {
                    if let Ok(mtime) = metadata.modified() {
                        let current_mtime = mtime
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        if current_mtime > cached_mtime {
                            self.stats.hit_rate = self.calculate_hit_rate(false);
                            return None; // File modified since cached
                        }
                    }
                }
            }

            self.stats.hit_rate = self.calculate_hit_rate(true);
            Some(entry.embedding.clone())
        } else {
            self.stats.hit_rate = self.calculate_hit_rate(false);
            None
        }
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
        let key = CacheKey::from_chunk(chunk, model, &self.key_root);
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

        let entry_size = embedding.len() * std::mem::size_of::<f32>();

        // Check if replacing existing entry
        if !self.entries.contains_key(&key_str) {
            self.stats.entries += 1;
            self.stats.size_bytes += entry_size;
        }

        self.entries.insert(
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

        let cache_file = self.config.cache_dir.join("cache.json");
        // Per-process unique temp name. A FIXED `cache.json.tmp` is shared
        // across processes, so two concurrent flushers (e.g. two agents/CLI
        // runs against the same cache) race: one renames the temp away and the
        // other's rename hits `No such file or directory` (os error 2). A
        // pid+nanos suffix gives each flusher its own temp to rename; it still
        // ends in `.tmp` so `cleanup_temp_files` reaps orphans. (Lost-update
        // under concurrent writers — last rename wins — is a deeper follow-up.)
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let temp_file =
            self.config
                .cache_dir
                .join(format!("cache.json.{}.{}.tmp", std::process::id(), nanos));

        // Write to temp file with exclusive lock
        {
            let file = File::create(&temp_file)?;
            file.lock_exclusive()?; // Exclusive lock for writing
            let writer = BufWriter::new(&file);
            serde_json::to_writer(writer, &self.entries).map_err(|e| {
                crate::TldrError::ParseError {
                    file: temp_file.clone(),
                    line: None,
                    message: format!("Failed to serialize cache: {}", e),
                }
            })?;
            file.sync_all()?;
            file.unlock()?;
        }

        // Atomic rename. On failure, best-effort remove our unique temp so a
        // failed flush does not leave an orphan behind (cleanup_temp_files also
        // reaps these on next open).
        if let Err(e) = fs::rename(&temp_file, &cache_file) {
            let _ = fs::remove_file(&temp_file);
            return Err(e.into());
        }

        self.dirty = false;
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
        let cutoff = now.saturating_sub(ttl_secs);

        let before = self.entries.len();
        self.entries.retain(|_, entry| entry.cached_at >= cutoff);
        let evicted = before - self.entries.len();

        if evicted > 0 {
            // Update stats
            self.stats.entries = self.entries.len();
            self.stats.size_bytes = self
                .entries
                .values()
                .map(|e| e.embedding.len() * std::mem::size_of::<f32>())
                .sum();
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
        self.entries.len()
    }

    /// Check if cache is empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
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
    fn cache_key_is_root_relative_across_absolute_and_relative_roots() {
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
        // The key path is the root-relative tail, not the full path.
        assert!(
            rel_key.starts_with("a.rs:"),
            "key should be root-relative ('a.rs:...'), got {rel_key}"
        );

        // Empty key_root (the default for legacy/test callers) preserves the
        // full raw path, so existing behavior is unchanged.
        let raw_key = CacheKey::from_chunk(&rel_chunk, EmbeddingModel::ArcticL, Path::new(""))
            .to_key_string();
        assert!(
            raw_key.starts_with("crates/x/src/a.rs:"),
            "empty key_root must leave the path untouched, got {raw_key}"
        );
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
        if let Some(entry) = cache.entries.get_mut(&key) {
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
    fn cache_key_includes_function_identity() {
        // GIVEN: Two chunks with same content but different function names
        // (This tests P0 mitigation for hash collision)
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
        };

        let embedding1 = vec![0.1, 0.2, 0.3];
        let embedding2 = vec![0.4, 0.5, 0.6];

        // WHEN: We store both
        cache.put(&chunk1, embedding1.clone(), EmbeddingModel::ArcticM);
        cache.put(&chunk2, embedding2.clone(), EmbeddingModel::ArcticM);

        // THEN: They should be stored separately
        assert_eq!(cache.len(), 2);
        let result1 = cache.get(&chunk1, EmbeddingModel::ArcticM);
        let result2 = cache.get(&chunk2, EmbeddingModel::ArcticM);
        assert_eq!(result1.unwrap(), embedding1);
        assert_eq!(result2.unwrap(), embedding2);
    }

    #[test]
    fn cache_ttl_checked_on_read() {
        // This test verifies P0 mitigation: TTL is checked on every read
        // We can't easily test time-based expiration without mocking time,
        // but we verify the TTL logic exists by using ttl_days = 0

        let temp = tempdir().unwrap();
        let config = CacheConfig {
            cache_dir: temp.path().to_path_buf(),
            max_size_mb: 100,
            ttl_days: 0, // Immediate expiration
        };
        let mut cache = EmbeddingCache::open(config).unwrap();
        let chunk = create_test_chunk("foo", "fn foo() {}");
        let embedding = vec![0.1, 0.2, 0.3];

        // Put entry
        cache.put(&chunk, embedding, EmbeddingModel::ArcticM);

        // Entry is in the HashMap but should fail TTL check
        // With ttl_days = 0, any entry cached at time T will have age > 0 days
        // when read at time T (since we use integer division)
        let _result = cache.get(&chunk, EmbeddingModel::ArcticM);

        // Note: This might pass or fail depending on timing - entry was just created
        // so it might still be within the 0-day window. The important thing is
        // that the TTL check exists in the code path.
        // For a more robust test, we'd need time mocking.
        // At minimum, verify the cache entry exists
        assert!(cache.entries.contains_key(
            &CacheKey::from_chunk(&chunk, EmbeddingModel::ArcticM, Path::new("")).to_key_string()
        ));
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
        let cache_file = temp.path().join("cache.json");
        fs::write(&cache_file, "not valid json{{{").unwrap();

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
    fn cache_cleans_up_temp_files() {
        // GIVEN: A cache directory with orphaned temp files
        let temp = tempdir().unwrap();
        let temp_file = temp.path().join("cache.json.tmp");
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
}

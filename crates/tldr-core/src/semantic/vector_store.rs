//! TLDR-l5d: usearch-backed vector store (`key u64 -> f32 vector`).
//!
//! **Step 1 (this commit):** an index-creation helper plus a dependency smoke
//! test. The test proves the usearch C++/cxx dependency builds and links in this
//! workspace and that the `exact_search` + `save`/`load` + `remove` round-trip
//! behaves as the design assumes — pinning the exact API the full store builds on.
//!
//! Still to land on top of this (see `docs/INCREMENTAL_REINDEX_DESIGN.md`):
//! the metadata sidecar (§4.2) + per-file records (§4.3), the store manifest
//! (§4.0), the content-addressed dedup layer (§4.1), and the crash-safe
//! generation + `CURRENT`-pointer save (§7.1). This module is the foundation
//! those build on, kept deliberately minimal until the dependency is proven.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use crate::error::TldrError;
use crate::semantic::index::BuildOptions;
use crate::semantic::lineage::{
    reconcile_chunks, ChunkCandidate, ChunkId, ChunkIdAllocator, ChunkRevision, PriorChunk,
    StructuralAnchor,
};
use crate::semantic::types::{CacheConfig, CodeChunk, EmbeddingModel};
use crate::TldrResult;

/// Map a usearch error (`cxx::Exception`, or anything `Display`) into `TldrError`.
/// Generic over `Display` so we don't take a direct `cxx` dependency just to name
/// the exception type.
fn vs_err<E: std::fmt::Display>(context: &str, e: E) -> TldrError {
    TldrError::Embedding(format!("usearch {context}: {e}"))
}

/// Create an empty exact-search **f32** index over `dimensions`-dimensional,
/// unit-normalized vectors, pre-reserving room for `capacity` entries.
///
/// - Metric is **cosine** (vectors are unit-normalized; see
///   [`crate::semantic::similarity::normalize`]).
/// - Quantization is **f32** — the TLDR-l5d first pass; i8 compact mode is
///   TLDR-ccg.
/// - Query time uses [`Index::exact_search`] (exact KNN, 100% recall), so the
///   HNSW graph usearch builds on `add` is unused but harmless at our scale.
fn new_f32_index(dimensions: usize, capacity: usize) -> TldrResult<Index> {
    let options = IndexOptions {
        dimensions,
        metric: MetricKind::Cos,
        quantization: ScalarKind::F32,
        ..Default::default()
    };
    let index = Index::new(&options).map_err(|e| vs_err("new", e))?;
    index.reserve(capacity).map_err(|e| vs_err("reserve", e))?;
    Ok(index)
}

/// Per-chunk metadata held in the sidecar — everything needed to serve a search
/// result, since the usearch index stores **only** the vector. Design doc §4.2.
#[derive(
    Archive, Debug, Clone, PartialEq, RkyvDeserialize, RkyvSerialize, Serialize, Deserialize,
)]
pub struct ChunkMeta {
    /// Hex-encoded [`ChunkId`] used to guard the derived u64 usearch key.
    pub identity: String,
    /// Stable logical identity preserved across unambiguous localized edits.
    pub chunk_id: ChunkId,
    /// Hash of the exact composed document embedded for this chunk.
    pub revision: ChunkRevision,
    /// Structural evidence used for future lineage reconciliation.
    pub anchor: StructuralAnchor,
    /// Root-relative path (CWD/absolute-independent).
    pub file_rel_path: String,
    /// Function/method name (`None` for file-level chunks).
    pub function_name: Option<String>,
    /// Enclosing class/struct, if any.
    pub class_name: Option<String>,
    /// 1-indexed start line.
    pub line_start: u32,
    /// 1-indexed end line (inclusive).
    pub line_end: u32,
    /// Detects body changes; also anchors the lazy snippet read.
    pub content_hash: String,
    /// Persisted structural provenance for snippet/source reconstruction.
    #[serde(default)]
    pub structure: crate::semantic::types::ChunkStructure,
}

/// A search result: the matched key, its cosine **distance** (lower = closer;
/// cosine similarity ≈ `1 - distance`), and the chunk's sidecar metadata.
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// The matched chunk's stable u64 key.
    pub key: u64,
    /// Cosine distance to the query (lower = closer; similarity ≈ 1 - distance).
    pub distance: f32,
    /// The matched chunk's sidecar metadata.
    pub meta: ChunkMeta,
}

/// usearch-backed vector store: `key(u64) -> vector` (the usearch index) paired
/// with a `key -> ChunkMeta` sidecar. One store per embedding model (the vector
/// dimensionality is fixed per model). Persistence is implemented here — a
/// manifest plus crash-safe generation/`CURRENT` save/load (see [`Self::save`] /
/// [`Self::load`]).
///
/// `Send + Sync` (usearch `Index` is `unsafe impl Send + Sync`; `search` takes
/// `&self` + a pre-computed query vector, while `add`/`remove` take `&mut self`),
/// so `Arc<RwLock<VectorStore>>` supports concurrent reads with exclusive writes
/// (TLDR-ac0.1).
pub struct VectorStore {
    dimensions: usize,
    /// Reserved usearch capacity; grown (doubled) on demand since usearch does
    /// not auto-grow on `add`.
    capacity: usize,
    index: Index,
    /// Sidecar: key -> metadata. Kept in lockstep with the index on add/remove.
    meta: HashMap<u64, ChunkMeta>,
    /// Per-file record: file_rel_path -> {keys, mtime, size, file_type}. The
    /// startup-reconcile signal and per-file key lookup (design doc §4.3).
    /// Populated by the build/delta path; persisted in the sidecar.
    files: HashMap<String, FileRecord>,
    /// Stat-only digest of the candidate corpus at build time (TLDR-kkt). Set by
    /// [`Self::build`], persisted in the manifest, and restored by [`Self::load`].
    /// 0 for stores built without a root (e.g. unit tests via `new`/`from_embedded`),
    /// which simply never trip the freshness gate.
    corpus_digest: u64,
    build_stats: crate::semantic::chunker::ChunkStats,
    /// Build-time instrumentation captured when `BuildOptions::collect_metrics`
    /// was set (TLDR-9bxa.1). `None` for stores built/loaded without metrics.
    build_metrics: Option<crate::semantic::build_metrics::MetricsReport>,
}

impl VectorStore {
    /// Minimum reserved capacity, so tiny stores still have headroom.
    const MIN_CAPACITY: usize = 16;

    /// Create an empty store for `dimensions`-dimensional vectors, pre-reserving
    /// room for `capacity` entries.
    pub fn new(dimensions: usize, capacity: usize) -> TldrResult<Self> {
        let capacity = capacity.max(Self::MIN_CAPACITY);
        let index = new_f32_index(dimensions, capacity)?;
        Ok(Self {
            dimensions,
            capacity,
            index,
            meta: HashMap::new(),
            files: HashMap::new(),
            corpus_digest: 0,
            build_stats: Default::default(),
            build_metrics: None,
        })
    }

    /// Record (or replace) a file's per-file entry (design doc §4.3). Used by the
    /// build/delta path; persisted in the sidecar for reconcile on restart.
    pub fn set_file_record(&mut self, file_rel_path: String, record: FileRecord) {
        self.files.insert(file_rel_path, record);
    }

    /// Look up a file's record (keys + reconcile signal).
    pub fn file_record(&self, file_rel_path: &str) -> Option<&FileRecord> {
        self.files.get(file_rel_path)
    }

    /// Persisted lineage evidence for every chunk currently owned by a file.
    pub fn file_chunk_meta(&self, file_rel_path: &str) -> Vec<ChunkMeta> {
        self.files
            .get(file_rel_path)
            .into_iter()
            .flat_map(|record| record.keys.iter())
            .filter_map(|key| self.meta.get(key).cloned())
            .collect()
    }

    /// Number of vectors currently in the store.
    pub fn len(&self) -> usize {
        self.index.size()
    }

    /// Whether the store holds no vectors.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Counts captured during the most recent build.
    pub fn build_stats(&self) -> crate::semantic::chunker::ChunkStats {
        self.build_stats
    }

    /// Build-time instrumentation captured when `BuildOptions::collect_metrics`
    /// was set (TLDR-9bxa.1). `None` for stores built/loaded without metrics
    /// (the default path), so callers requesting `--metrics` can tell whether a
    /// fresh build actually ran.
    pub fn build_metrics(&self) -> Option<&crate::semantic::build_metrics::MetricsReport> {
        self.build_metrics.as_ref()
    }

    pub(crate) fn build_metrics_mut(
        &mut self,
    ) -> Option<&mut crate::semantic::build_metrics::MetricsReport> {
        self.build_metrics.as_mut()
    }

    /// The build-time corpus digest persisted with this store (TLDR-kkt). Compare
    /// against [`compute_corpus_digest`] over the current root to detect source
    /// drift (added/removed file, or any file's mtime/size change). 0 for stores
    /// built without a root (unit tests).
    pub fn corpus_digest(&self) -> u64 {
        self.corpus_digest
    }

    /// The vector dimensionality (fixed per embedding model).
    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Visit every vector in stable key order without materializing the corpus.
    pub(crate) fn visit_records(
        &self,
        mut visitor: impl FnMut(u64, &[f32], &ChunkMeta) -> TldrResult<()>,
    ) -> TldrResult<()> {
        let mut keys: Vec<_> = self.meta.keys().copied().collect();
        keys.sort_unstable();
        let mut vector = vec![0.0_f32; self.dimensions];
        for key in keys {
            let found = self
                .index
                .get(key, &mut vector)
                .map_err(|error| vs_err("get", error))?;
            if found != 1 {
                return Err(vs_err("get", format!("key {key} returned {found} vectors")));
            }
            visitor(key, &vector, &self.meta[&key])?;
        }
        Ok(())
    }

    pub(crate) fn files_snapshot(&self) -> HashMap<String, FileRecord> {
        self.files.clone()
    }

    pub(crate) fn from_generation_records(
        dimensions: usize,
        records: impl IntoIterator<Item = (u64, Vec<f32>, ChunkMeta)>,
        files: HashMap<String, FileRecord>,
        corpus_digest: u64,
    ) -> TldrResult<Self> {
        let records: Vec<_> = records.into_iter().collect();
        let mut store = Self::new(dimensions, records.len())?;
        for (key, vector, meta) in records {
            store.add(key, &vector, meta)?;
        }
        store.files = files;
        store.corpus_digest = corpus_digest;
        Ok(store)
    }

    /// Whether `key` is present in the index.
    pub fn contains(&self, key: u64) -> bool {
        self.index.contains(key)
    }

    /// Insert or replace `key`'s vector + metadata. Re-adding an existing key
    /// updates it in place (used by deltas: a changed body keeps its key).
    pub fn add(&mut self, key: u64, vector: &[f32], meta: ChunkMeta) -> TldrResult<()> {
        if vector.len() != self.dimensions {
            return Err(TldrError::Embedding(format!(
                "vector dimension {} != store dimension {}",
                vector.len(),
                self.dimensions
            )));
        }
        // Collision guard (Codex review): a re-add with the SAME identity is a
        // legitimate update (delta: changed body, same key); a same-key/DIFFERENT-
        // identity is a u64 hash collision that would silently lose a chunk.
        if let Some(existing) = self.meta.get(&key) {
            if existing.identity != meta.identity {
                return Err(vs_err(
                    "add",
                    format!(
                        "u64 key collision: '{}' vs '{}' both hash to {key}",
                        existing.identity, meta.identity
                    ),
                ));
            }
        }
        // Replace semantics: drop any existing vector first. A replace reuses the
        // freed slot, so only a NEW key can grow the index — reserve just for that
        // (Codex review: don't reserve when merely updating a full store).
        let replacing = self.index.contains(key);
        if replacing {
            self.index.remove(key).map_err(|e| vs_err("remove", e))?;
        } else if self.index.size() >= self.capacity {
            // usearch does not auto-grow; reserve more before we run out.
            self.capacity = self.capacity.saturating_mul(2).max(self.index.size() + 1);
            self.index
                .reserve(self.capacity)
                .map_err(|e| vs_err("reserve", e))?;
        }
        self.index.add(key, vector).map_err(|e| vs_err("add", e))?;
        self.meta.insert(key, meta);
        Ok(())
    }

    /// Remove `key` from the index and sidecar. Returns whether it was present.
    pub fn remove(&mut self, key: u64) -> TldrResult<bool> {
        let present = self.index.contains(key);
        if present {
            self.index.remove(key).map_err(|e| vs_err("remove", e))?;
        }
        self.meta.remove(&key);
        Ok(present)
    }

    /// The stored source content hash for `key`, if present.
    pub fn content_hash(&self, key: u64) -> Option<&str> {
        self.meta.get(&key).map(|m| m.content_hash.as_str())
    }

    /// Exact composed-document revision stored for `key`.
    pub fn revision(&self, key: u64) -> Option<ChunkRevision> {
        self.meta.get(&key).map(|meta| meta.revision)
    }

    /// Drop a file's per-file record. Returns the removed record (its keys), if
    /// any. The keys' vectors are NOT removed here — callers that delete a file
    /// use [`Self::apply_file_delete`], which removes both.
    pub fn remove_file_record(&mut self, file_rel_path: &str) -> Option<FileRecord> {
        self.files.remove(file_rel_path)
    }

    /// Remove every chunk of a **deleted** file: drop each key's vector + sidecar
    /// entry, then the per-file record. Returns the number of vectors removed.
    /// Design doc §5 "File deletion" (TLDR-t8f).
    pub fn apply_file_delete(&mut self, file_rel_path: &str) -> TldrResult<usize> {
        let keys: Vec<u64> = match self.files.get(file_rel_path) {
            Some(rec) => rec.keys.iter().copied().collect(),
            None => return Ok(0),
        };
        let mut removed = 0;
        for k in keys {
            if self.remove(k)? {
                removed += 1;
            }
        }
        self.files.remove(file_rel_path);
        Ok(removed)
    }

    /// Apply an incremental delta for a **single file** atomically (design doc
    /// §5). `keyed` is the file's freshly re-chunked `(key, ChunkMeta)` set (from
    /// the shared [`key_chunks`]); `embedded` supplies vectors for exactly the
    /// keys whose body changed (the EMBED set, computed lock-free by the caller).
    ///
    /// Steps, all under the caller's write lock:
    /// 1. **Remove** keys in the old file record but not in `keyed` (deleted /
    ///    renamed-away functions).
    /// 2. For each `(key, meta)`: re-classify against the *current* store
    ///    (re-validation — the caller classified under a since-dropped read lock,
    ///    so a concurrent delta could have shifted state). A key that needs a
    ///    vector but is absent from `embedded` is a **stale snapshot**: return an
    ///    error so the caller falls back to a full rebuild rather than serve a
    ///    half-applied delta. An unchanged body gets a **metadata-only** refresh
    ///    (new line numbers, no ONNX).
    /// 3. Replace the per-file record with the new key set + `signal`.
    ///
    /// `signal` is the `(mtime, size, kind)` from [`stat_signal`] on the file.
    pub fn apply_file_delta(
        &mut self,
        file_rel_path: &str,
        keyed: &[(u64, ChunkMeta)],
        embedded: &HashMap<u64, Vec<f32>>,
        signal: (u64, u64, FileKind),
    ) -> TldrResult<()> {
        use std::collections::BTreeSet;

        let new_keys: BTreeSet<u64> = keyed.iter().map(|(k, _)| *k).collect();

        // 1. Removed = old keys no longer present in the re-chunked file.
        if let Some(old) = self.files.get(file_rel_path) {
            let removed: Vec<u64> = old.keys.difference(&new_keys).copied().collect();
            for k in removed {
                self.remove(k)?;
            }
        }

        // 2. Add / update each current chunk using exact document revisions.
        for (key, meta) in keyed {
            let needs_embed = match self.revision(*key) {
                None => true,
                Some(revision) => revision != meta.revision,
            };
            if needs_embed {
                match embedded.get(key) {
                    // add() replaces in place when the key already exists.
                    Some(vector) => self.add(*key, vector, meta.clone())?,
                    None => {
                        return Err(vs_err(
                            "delta",
                            format!(
                                "stale snapshot: no vector for changed key {key} ({})",
                                meta.identity
                            ),
                        ))
                    }
                }
            } else {
                // META-ONLY: refresh line numbers etc. without re-embedding.
                self.meta.insert(*key, meta.clone());
            }
        }

        // 3. Refresh the per-file record (key set + reconcile signal).
        self.set_file_record(
            file_rel_path.to_string(),
            FileRecord {
                keys: new_keys,
                mtime: signal.0,
                size: signal.1,
                file_type: signal.2,
            },
        );
        Ok(())
    }

    /// Apply a reconciled delta only if its prior lineage snapshot is current.
    ///
    /// Planning and embedding happen outside the store write lock. A concurrent
    /// edit may therefore replace the file record before this call; rejecting
    /// that stale snapshot prevents an older reconciliation from deleting the
    /// newer lineage and restoring obsolete keys.
    pub fn apply_file_delta_reconciled(
        &mut self,
        file_rel_path: &str,
        expected_old_keys: &std::collections::BTreeSet<u64>,
        keyed: &[(u64, ChunkMeta)],
        embedded: &HashMap<u64, Vec<f32>>,
        signal: (u64, u64, FileKind),
    ) -> TldrResult<()> {
        let current_keys = self
            .files
            .get(file_rel_path)
            .map(|record| record.keys.clone())
            .unwrap_or_default();
        if current_keys != *expected_old_keys {
            return Err(vs_err(
                "delta",
                format!(
                    "stale lineage snapshot for {file_rel_path}: expected {:?}, current {:?}",
                    expected_old_keys, current_keys
                ),
            ));
        }
        self.apply_file_delta(file_rel_path, keyed, embedded, signal)
    }

    /// Exact (100% recall) top-`k` search. Returns hits joined to their sidecar
    /// metadata, nearest first. A key present in the index but missing from the
    /// sidecar is skipped (defensive; the two are kept in lockstep).
    pub fn search(&self, query: &[f32], k: usize) -> TldrResult<Vec<SearchHit>> {
        if query.len() != self.dimensions {
            return Err(TldrError::Embedding(format!(
                "query dimension {} != store dimension {}",
                query.len(),
                self.dimensions
            )));
        }
        let k = k.min(self.index.size());
        if k == 0 {
            return Ok(Vec::new());
        }
        let matches = self
            .index
            .exact_search(query, k)
            .map_err(|e| vs_err("exact_search", e))?;
        let hits = matches
            .keys
            .iter()
            .zip(matches.distances.iter())
            .filter_map(|(&key, &distance)| {
                self.meta.get(&key).map(|meta| SearchHit {
                    key,
                    distance,
                    meta: meta.clone(),
                })
            })
            .collect();
        Ok(hits)
    }
}

// =============================================================================
// Persistence (design doc §4.0 manifest, §4.3 records, §7.1/§7.2 crash-safe save)
// =============================================================================

/// On-disk layout version. Bump on any breaking change to the file formats.
/// v2: switched persisted checksums + identity key from DefaultHasher to a
/// stable FNV-1a hash (Codex review) — old stores are rejected on load.
/// v3: added `corpus_digest` to the manifest (TLDR-kkt freshness gate) — old
/// stores lack it and are rebuilt once.
/// v4: replaced positional identities with persisted chunk lineage, exact
/// document revisions, and structural reconciliation anchors (TLDR-9bxa.4).
const STORE_FORMAT_VERSION: u32 = 5;
/// `CURRENT` magic ("TLDR") so a torn/foreign pointer is detectable.
const CURRENT_MAGIC: u32 = 0x544C_4452;
/// Generations retained by GC (the active one + rollback headroom). Keeps a
/// concurrent reader's snapshot alive across a few saves (design doc §7.1).
const KEEP_GENS: u64 = 3;
/// Maximum number of vectors whose cache lookup/inference is grouped into one
/// durable semantic-build progress window.
pub const EMBEDDING_WINDOW_VECTORS: usize = 128;

/// What kind of filesystem object a tracked path was at index time — lets
/// reconcile (§7.3) detect file↔dir/type swaps, not just content changes.
#[derive(
    Archive,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    RkyvDeserialize,
    RkyvSerialize,
    Serialize,
    Deserialize,
)]
pub enum FileKind {
    /// A regular indexable source file.
    Regular,
    /// A symbolic link.
    Symlink,
    /// Anything else (directory, socket, …) — treated as a deletion on reconcile.
    Other,
}

/// Per-file record (design doc §4.3): which keys belong to the file plus the
/// `(mtime, size, file_type)` reconcile signal.
#[derive(
    Archive, Debug, Clone, PartialEq, RkyvDeserialize, RkyvSerialize, Serialize, Deserialize,
)]
pub struct FileRecord {
    /// Chunk keys belonging to this file (for O(1) per-file deltas).
    pub keys: std::collections::BTreeSet<u64>,
    /// File mtime (seconds) at index time — reconcile signal.
    pub mtime: u64,
    /// File size at index time — catches same-mtime edits.
    pub size: u64,
    /// File kind at index time — detects file↔dir/type swaps.
    pub file_type: FileKind,
}

/// The subset of the manifest that must match the running config on `load`, or
/// the persisted store is incompatible and the caller must full-rebuild
/// (design doc §4.0). Every field here changes the vectors OR the chunk
/// boundaries, so a mismatch means the stored vectors can't be trusted.
#[derive(
    Archive, Debug, Clone, PartialEq, Eq, RkyvDeserialize, RkyvSerialize, Serialize, Deserialize,
)]
pub struct ManifestId {
    /// Embedding model identifier (e.g. `"ArcticL"`).
    pub embedding_model: String,
    /// Weights + tokenizer revision — a tokenizer bump invalidates vectors even
    /// under the same model name.
    pub model_revision: String,
    /// Vector dimensionality.
    pub dimensions: u32,
    /// Distance metric (`"cos"`).
    pub metric: String,
    /// Scalar quantization (`"f32"` / `"i8"`).
    pub scalar_kind: String,
    /// Search mode (`"exact"` vs `"hnsw"`).
    pub search_mode: String,
    /// Embed-input recipe tag (`raw-v2` / `enriched-v2`).
    pub embed_schema: String,
    /// Digest of ChunkOptions (granularity/max_tokens/overlap/lang filter).
    pub chunk_params: String,
    /// Digest of the source-selection / ignore rules.
    pub walker_version: String,
    /// Canonical project root the keys are relative to.
    pub root: String,
}

#[derive(Archive, RkyvDeserialize, RkyvSerialize, Serialize, Deserialize)]
struct Manifest {
    format_version: u32,
    generation: u64,
    #[serde(flatten)]
    id: ManifestId,
    chunk_count: u64,
    /// Digest of the sorted key set — key membership.
    keys_checksum: u64,
    /// Digest of the index FILE bytes — vector correctness.
    index_checksum: u64,
    /// Digest of the sidecar payload.
    sidecar_checksum: u64,
    /// Stat-only digest of the candidate source corpus at build time (TLDR-kkt).
    /// `store_search` rebuilds when the current corpus digest differs — i.e. a
    /// file was added/removed or any file's mtime/size changed. `serde(default)`
    /// so a v2 manifest (which lacks it) still deserializes; it then fails the
    /// `format_version` gate and is rebuilt.
    #[serde(default)]
    corpus_digest: u64,
}

/// Owned view for deserialization on load.
#[derive(Archive, RkyvDeserialize, RkyvSerialize, Deserialize)]
struct SidecarOwned {
    meta: HashMap<u64, ChunkMeta>,
    files: HashMap<String, FileRecord>,
}

/// The structured `CURRENT` pointer — the single atomic commit point. `magic` +
/// `checksum` make a torn/partial write detectable (design doc §7.1).
#[derive(Archive, RkyvDeserialize, RkyvSerialize, Serialize, Deserialize)]
struct CurrentPointer {
    magic: u32,
    generation: u64,
    checksum: u32,
}

impl VectorStore {
    /// Persist the store into `dir` as a NEW immutable generation, committing
    /// atomically by swapping the `CURRENT` pointer last (design doc §7.1).
    ///
    /// `id` carries the running config (model/dims/params/root) recorded in the
    /// manifest; `load` rejects a store whose `id` differs. Files written:
    /// `index.<gen>.usearch`, `meta.<gen>`, `manifest.<gen>`, then `CURRENT`.
    pub fn save(&self, dir: &Path, id: &ManifestId) -> TldrResult<()> {
        if id.dimensions as usize != self.dimensions {
            return Err(vs_err(
                "save",
                format!(
                    "id.dimensions {} != store {}",
                    id.dimensions, self.dimensions
                ),
            ));
        }
        std::fs::create_dir_all(dir)?;

        // Serialize writers (Codex review): two concurrent saves could derive the
        // same generation from CURRENT and interleave index/sidecar/manifest. An
        // exclusive advisory lock on a store lockfile makes save single-writer.
        // Held until this function returns (the guard drops -> unlocks, even on
        // error). m01 should ALSO keep writes daemon-only; this is defense-in-depth.
        use fs2::FileExt;
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(dir.join("lock"))?;
        lock_file.lock_exclusive()?;

        // Next generation = max(valid CURRENT, highest on-disk manifest) + 1. A
        // torn CURRENT must NOT reset numbering to 1 and overwrite existing
        // manifest.<gen> history (Codex review). `checked_add` guards against a
        // stray/adversarial manifest.<u64::MAX> filename overflowing the counter
        // (Codex review): the base is drawn from arbitrary on-disk filenames.
        let gen = next_generation(dir)?;
        self.write_generation(dir, id, gen)?;
        activate_current(dir, gen)?;

        // 5. GC — retain the last KEEP_GENS generations.
        gc_old_generations(dir, gen);
        Ok(())
    }

    /// Write and verify one immutable generation without publishing it.
    pub(crate) fn write_generation(&self, dir: &Path, id: &ManifestId, gen: u64) -> TldrResult<()> {
        if id.dimensions as usize != self.dimensions {
            return Err(vs_err("save", "manifest dimensions do not match store"));
        }
        std::fs::create_dir_all(dir)?;
        let staged_index = dir.join(format!("index.{gen}.staged"));
        let index_path = dir.join(format!("index.{gen}.usearch"));
        let staged_str = staged_index
            .to_str()
            .ok_or_else(|| vs_err("save", "non-utf8 index path"))?;
        self.index
            .save(staged_str)
            .map_err(|error| vs_err("save", error))?;
        sync_path(&staged_index)?;
        std::fs::rename(&staged_index, &index_path)?;
        sync_dir(dir)?;
        let index_checksum = digest_bytes(&std::fs::read(&index_path)?);

        let sidecar_bytes = encode_binary(&SidecarOwned {
            meta: self.meta.clone(),
            files: self.files.clone(),
        })?;
        let sidecar_checksum = digest_bytes(&sidecar_bytes);
        write_sync(&dir.join(format!("meta.{gen}")), &sidecar_bytes)?;
        let mut keys: Vec<u64> = self.meta.keys().copied().collect();
        keys.sort_unstable();
        let manifest = Manifest {
            format_version: STORE_FORMAT_VERSION,
            generation: gen,
            id: id.clone(),
            chunk_count: self.meta.len() as u64,
            keys_checksum: keys_digest(&keys),
            index_checksum,
            sidecar_checksum,
            corpus_digest: self.corpus_digest,
        };
        let manifest_bytes = encode_binary(&manifest)?;
        write_sync(&dir.join(format!("manifest.{gen}")), &manifest_bytes)?;
        sync_dir(dir)
    }

    pub(crate) fn load_specific_generation(
        dir: &Path,
        generation: u64,
        expect: &ManifestId,
    ) -> TldrResult<Self> {
        Self::load_generation(dir, generation, expect).map_err(LoadFail::into_err)
    }

    /// Load the active generation from `dir`, verifying against the running config
    /// `expect`. Scans candidate generations newest-to-oldest for the newest that
    /// both MATCHES `expect` and verifies intact, with one exception that guards
    /// against serving stale data (Codex review):
    ///
    /// - If the NEWEST committed generation is `Incompatible` (config/format
    ///   mismatch), the store was built under a different model/schema → REJECT so
    ///   the caller full-rebuilds. We never resurrect a stale older generation
    ///   behind a config change.
    /// - Otherwise (the newest is `Corrupt`, or any OLDER generation fails), fall
    ///   back: an older generation that is `Corrupt` is skipped as unusable, and one
    ///   that is `Incompatible` is skipped as not-a-candidate for the current config.
    ///   Either way the scan continues to the next-older generation.
    ///
    /// Errors (→ caller full-rebuilds) only if no retained generation matches and
    /// verifies.
    pub fn load(dir: &Path, expect: &ManifestId) -> TldrResult<Self> {
        // Shared lock: a concurrent save() holds the EXCLUSIVE lock while it writes
        // its generation files, so this blocks until no save is mid-write — the
        // fallback scan can't pick up an in-flight, not-yet-committed generation
        // (Codex review).
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(dir.join("lock"))?;
        // Fully-qualified via fs2 (not the std inherent `File::lock_shared`, which is
        // only stable from 1.89) so the lock path is MSRV-agnostic — the project pins
        // no rust-version.
        fs2::FileExt::lock_shared(&lock)?;

        let current_gen = read_current(dir).map(|c| c.generation);
        let mut gens = manifest_gens(dir);
        gens.sort_unstable_by(|a, b| b.cmp(a)); // newest first
        if let Some(cg) = current_gen {
            // Trust CURRENT as the newest COMMITTED generation: ignore any
            // higher-numbered manifest (an in-flight save that didn't commit).
            gens.retain(|g| *g <= cg);
        }
        if gens.is_empty() {
            return Err(vs_err("load", "no store generation found"));
        }

        let mut newest = true;
        let mut last_err = None;
        for gen in gens {
            match Self::load_generation(dir, gen, expect) {
                Ok(store) => {
                    if !newest {
                        eprintln!(
                            "[tldr-warn] vector_store: recovered from older generation {gen} \
                             (the newest committed one was unusable); the next save repairs CURRENT"
                        );
                    }
                    return Ok(store);
                }
                // The NEWEST committed generation being for a different
                // model/schema means the config changed -> rebuild; do NOT
                // resurrect a stale older generation (Codex review).
                Err(LoadFail::Incompatible(e)) if newest => return Err(e),
                Err(f) => last_err = Some(f.into_err()),
            }
            newest = false;
        }
        Err(last_err.unwrap_or_else(|| vs_err("load", "no verifying generation")))
    }

    /// Verify and load one specific generation. The failure is typed `Incompatible`
    /// (config/format mismatch) vs `Corrupt` (IO/parse/checksum/drift) so `load()`
    /// can REJECT when the NEWEST committed generation is `Incompatible` (config
    /// changed → rebuild) while still scanning older generations past any other
    /// failure. See `load()` for the full fallback policy.
    fn load_generation(dir: &Path, gen: u64, expect: &ManifestId) -> Result<Self, LoadFail> {
        let manifest_bytes = std::fs::read(dir.join(format!("manifest.{gen}")))
            .map_err(|e| LoadFail::Corrupt(e.into()))?;
        let manifest: Manifest = decode_binary(&manifest_bytes).map_err(LoadFail::Corrupt)?;
        if manifest.format_version != STORE_FORMAT_VERSION {
            return Err(LoadFail::Incompatible(vs_err(
                "load",
                "format_version mismatch",
            )));
        }
        if &manifest.id != expect {
            return Err(LoadFail::Incompatible(vs_err(
                "load",
                "config mismatch (model/dims/params/root)",
            )));
        }
        if manifest.generation != gen {
            return Err(LoadFail::Corrupt(vs_err(
                "load",
                "manifest generation != filename",
            )));
        }

        let meta_bytes = std::fs::read(dir.join(format!("meta.{gen}")))
            .map_err(|e| LoadFail::Corrupt(e.into()))?;
        if digest_bytes(&meta_bytes) != manifest.sidecar_checksum {
            return Err(LoadFail::Corrupt(vs_err(
                "load",
                "sidecar checksum mismatch",
            )));
        }
        let index_path = dir.join(format!("index.{gen}.usearch"));
        let index_bytes = std::fs::read(&index_path).map_err(|e| LoadFail::Corrupt(e.into()))?;
        if digest_bytes(&index_bytes) != manifest.index_checksum {
            return Err(LoadFail::Corrupt(vs_err("load", "index checksum mismatch")));
        }

        let sidecar: SidecarOwned = decode_binary(&meta_bytes).map_err(LoadFail::Corrupt)?;
        let mut keys: Vec<u64> = sidecar.meta.keys().copied().collect();
        keys.sort_unstable();
        if keys_digest(&keys) != manifest.keys_checksum {
            return Err(LoadFail::Corrupt(vs_err("load", "keys checksum mismatch")));
        }

        let dimensions = expect.dimensions as usize;
        let capacity = sidecar.meta.len().max(Self::MIN_CAPACITY);
        let index = new_f32_index(dimensions, capacity).map_err(LoadFail::Corrupt)?;
        let index_str = index_path
            .to_str()
            .ok_or_else(|| LoadFail::Corrupt(vs_err("load", "non-utf8 index path")))?;
        index
            .load(index_str)
            .map_err(|e| LoadFail::Corrupt(vs_err("load", e)))?;
        if index.size() != sidecar.meta.len() {
            return Err(LoadFail::Corrupt(vs_err(
                "load",
                "index size != sidecar count (drift)",
            )));
        }
        // `keys_checksum` only proves the sidecar matches the manifest; verify the
        // usearch index actually CONTAINS every sidecar key (Codex — not circular).
        for &key in sidecar.meta.keys() {
            if !index.contains(key) {
                return Err(LoadFail::Corrupt(vs_err(
                    "load",
                    "index is missing a sidecar key (drift)",
                )));
            }
        }

        Ok(Self {
            dimensions,
            capacity,
            index,
            meta: sidecar.meta,
            files: sidecar.files,
            // Restore the build-time corpus digest so the freshness gate can
            // compare it against the current on-disk corpus (TLDR-kkt).
            corpus_digest: manifest.corpus_digest,
            build_stats: Default::default(),
            build_metrics: None,
        })
    }
}

/// Why a single generation failed to load — drives whether `load()` may fall
/// back to an older generation (`Corrupt`) or must reject and rebuild
/// (`Incompatible`, when the newest committed generation is the offender).
enum LoadFail {
    Incompatible(TldrError),
    Corrupt(TldrError),
}

impl LoadFail {
    fn into_err(self) -> TldrError {
        match self {
            LoadFail::Incompatible(e) | LoadFail::Corrupt(e) => e,
        }
    }
}

pub(crate) fn next_generation(dir: &Path) -> TldrResult<u64> {
    let previous = read_current(dir)
        .map(|current| current.generation)
        .unwrap_or(0)
        .max(manifest_gens(dir).into_iter().max().unwrap_or(0));
    previous
        .checked_add(1)
        .ok_or_else(|| vs_err("save", "generation counter overflow"))
}

pub(crate) fn activate_current(dir: &Path, generation: u64) -> TldrResult<()> {
    let current = CurrentPointer {
        magic: CURRENT_MAGIC,
        generation,
        checksum: current_checksum(CURRENT_MAGIC, generation),
    };
    let bytes = encode_binary(&current)?;
    let staged = dir.join("CURRENT.tmp");
    write_sync(&staged, &bytes)?;
    std::fs::rename(&staged, dir.join("CURRENT"))?;
    sync_dir(dir)
}

/// All generation numbers with an on-disk `manifest.<gen>` (unsorted).
fn manifest_gens(dir: &Path) -> Vec<u64> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_string_lossy()
                .strip_prefix("manifest.")
                .and_then(|r| r.parse::<u64>().ok())
        })
        .collect()
}

/// Stat-only digest of the candidate source corpus under `root` — the TLDR-kkt
/// freshness gate. Hashes the sorted `(root-relative path, mtime_secs, size)` of
/// every file [`chunker::enumerate_corpus_files`](crate::semantic::chunker::enumerate_corpus_files)
/// would feed the chunker. NO content read, NO parse — just a walk + `stat`, so
/// it stays bounded on large repos (design §7.3: do NOT content-hash every file).
///
/// The digest flips when the file SET changes (add/remove) or any file's
/// mtime/size changes; `store_search` rebuilds when the stored digest differs.
/// Sorted + root-relative so the value is identical regardless of cwd or the
/// walk's enumeration order. Because membership is decided at the WALK layer
/// (before parsing), a supported file that yields zero chunks counts identically
/// at build and check — it can never read as a spurious addition.
///
/// Residual (documented, design §7.3): an edit with the SAME mtime AND SAME size
/// AND no set change is not detected; self-heals on the next real edit, escape
/// hatch = manual rebuild.
pub fn compute_corpus_digest(root: &Path) -> u64 {
    let mut rows: Vec<(String, u64, u64)> = crate::semantic::chunker::enumerate_corpus_files(root)
        .into_iter()
        .map(|path| {
            let (mtime, size) = match std::fs::metadata(&path) {
                Ok(md) => {
                    let mtime = md
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    (mtime, md.len())
                }
                Err(_) => (0, 0),
            };
            (root_relative(root, &path), mtime, size)
        })
        .collect();
    rows.sort_unstable();
    let mut buf = Vec::with_capacity(rows.len() * 24);
    for (path, mtime, size) in &rows {
        buf.extend_from_slice(path.as_bytes());
        buf.push(0); // separator so ("ab","c") and ("a","bc") can't collide
        buf.extend_from_slice(&mtime.to_le_bytes());
        buf.extend_from_slice(&size.to_le_bytes());
    }
    stable_hash(&buf)
}

/// Stable FNV-1a 64-bit hash. Deterministic across processes, platforms, and
/// Rust versions — unlike `DefaultHasher` (SipHash), whose output is NOT a
/// guaranteed-stable on-disk primitive. Used for persisted checksums and the
/// u128 [`ChunkId`] to u64 usearch-key projection.
fn stable_hash(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}

fn digest_bytes(bytes: &[u8]) -> u64 {
    stable_hash(bytes)
}

fn keys_digest(sorted_keys: &[u64]) -> u64 {
    let mut buf = Vec::with_capacity(sorted_keys.len() * 8);
    for k in sorted_keys {
        buf.extend_from_slice(&k.to_le_bytes());
    }
    stable_hash(&buf)
}

fn current_checksum(magic: u32, generation: u64) -> u32 {
    let mut buf = [0u8; 12];
    buf[..4].copy_from_slice(&magic.to_le_bytes());
    buf[4..].copy_from_slice(&generation.to_le_bytes());
    (stable_hash(&buf) & 0xFFFF_FFFF) as u32
}

pub(crate) fn encode_binary<T>(value: &T) -> TldrResult<Vec<u8>>
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
        .map_err(|error| vs_err("binary encode", error))
}

pub(crate) fn decode_binary<T>(bytes: &[u8]) -> TldrResult<T>
where
    T: Archive,
    T::Archived: for<'a> rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>
        + RkyvDeserialize<T, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>,
{
    let mut aligned: rkyv::util::AlignedVec<16> =
        rkyv::util::AlignedVec::with_capacity(bytes.len());
    aligned.extend_from_slice(bytes);
    rkyv::from_bytes::<T, rkyv::rancor::Error>(&aligned)
        .map_err(|error| vs_err("binary decode", error))
}

/// Write `bytes` to `path` and fsync the file.
fn write_sync(path: &Path, bytes: &[u8]) -> TldrResult<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

/// fsync an already-written file (usearch's `save` may not fsync).
fn sync_path(path: &Path) -> TldrResult<()> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

/// fsync a directory so the renames/creates inside it are durable. Crash-safety
/// depends on this, so errors are PROPAGATED, not swallowed (Codex review). On
/// non-unix platforms where a directory can't be opened as a file, renames are
/// still ordered, so it's a documented no-op there.
fn sync_dir(dir: &Path) -> TldrResult<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
    Ok(())
}

/// Read + validate the `CURRENT` pointer. `None` if missing, unparseable, wrong
/// magic, or failing its checksum (a torn write) — `load()` then falls back to
/// scanning `manifest.<gen>` for the newest verifying generation.
fn read_current(dir: &Path) -> Option<CurrentPointer> {
    let bytes = std::fs::read(dir.join("CURRENT")).ok()?;
    let cur: CurrentPointer = decode_binary(&bytes).ok()?;
    if cur.magic != CURRENT_MAGIC {
        return None;
    }
    if cur.checksum != current_checksum(cur.magic, cur.generation) {
        return None;
    }
    Some(cur)
}

/// Extract `<gen>` from `index.<gen>.usearch` / `meta.<gen>` / `manifest.<gen>`.
fn parse_gen(name: &str) -> Option<u64> {
    let rest = if let Some(r) = name.strip_prefix("index.") {
        r.strip_suffix(".usearch")?
    } else if let Some(r) = name.strip_prefix("meta.") {
        r
    } else if let Some(r) = name.strip_prefix("manifest.") {
        r
    } else {
        return None;
    };
    rest.parse::<u64>().ok()
}

/// Delete generation files older than `current_gen - (KEEP_GENS - 1)`.
fn gc_old_generations(dir: &Path, current_gen: u64) {
    let keep_from = current_gen.saturating_sub(KEEP_GENS - 1);
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if let Some(gen) = parse_gen(&e.file_name().to_string_lossy()) {
                if gen < keep_from {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
}

// =============================================================================
// Build path — chunk identity -> stable u64 key, and populate from embeddings
// (design doc §4.1). The actual chunk_code + embed wiring lives in the index
// build; this layer is the deterministic key scheme + store population.
// =============================================================================

/// Derive the u64 usearch key from the complete logical chunk identity.
pub fn chunk_id_key(id: ChunkId) -> u64 {
    stable_hash(&id.0.to_le_bytes())
}

/// Compute fresh `(key, ChunkMeta)` pairs when no prior lineage is available.
/// The chunk source is treated as the exact document for this compatibility
/// entry point; production structural builds use [`key_chunks_reconciled`].
///
/// Shared by [`VectorStore::from_embedded`] (whole corpus) and the per-file
/// delta path (TLDR-t8f), so both compute **identical keys**. A divergence here
/// would make a delta's `remove`/replace miss the old vectors it must update —
/// hence the single source of truth.
pub fn key_chunks(root: &Path, chunks: &[CodeChunk]) -> Vec<(u64, ChunkMeta)> {
    let documents: Vec<String> = chunks.iter().map(|chunk| chunk.content.clone()).collect();
    key_chunks_reconciled(root, chunks, &documents, &[])
        .expect("one source document was constructed for every chunk")
}

/// Assign stable lineage and compute usearch keys for newly planned chunks.
///
/// `documents[i]` must be the exact text embedded for `chunks[i]`. Existing
/// metadata may contain chunks from the whole store; repository paths in the
/// anchors scope reconciliation to the changed file.
pub fn key_chunks_reconciled(
    root: &Path,
    chunks: &[CodeChunk],
    documents: &[String],
    prior: &[ChunkMeta],
) -> TldrResult<Vec<(u64, ChunkMeta)>> {
    let mut allocator = chunk_id_allocator(root, prior);
    key_chunks_reconciled_with_allocator(root, chunks, documents, prior, &mut allocator)
}

fn chunk_id_allocator(root: &Path, prior: &[ChunkMeta]) -> ChunkIdAllocator {
    let mut nonce_hasher = blake3::Hasher::new();
    nonce_hasher.update(normalize_sep(root).as_bytes());
    let mut prior_ids: Vec<u128> = prior.iter().map(|meta| meta.chunk_id.0).collect();
    prior_ids.sort_unstable();
    for id in prior_ids {
        nonce_hasher.update(&id.to_le_bytes());
    }
    let nonce_hash = nonce_hasher.finalize();
    let mut nonce_bytes = [0_u8; 16];
    nonce_bytes.copy_from_slice(&nonce_hash.as_bytes()[..16]);
    ChunkIdAllocator::new(u128::from_le_bytes(nonce_bytes))
}

fn key_chunks_reconciled_with_allocator(
    root: &Path,
    chunks: &[CodeChunk],
    documents: &[String],
    prior: &[ChunkMeta],
    allocator: &mut ChunkIdAllocator,
) -> TldrResult<Vec<(u64, ChunkMeta)>> {
    if chunks.len() != documents.len() {
        return Err(TldrError::Embedding(format!(
            "lineage input mismatch: {} chunks != {} composed documents",
            chunks.len(),
            documents.len()
        )));
    }
    let candidates: Vec<ChunkCandidate> = chunks
        .iter()
        .zip(documents)
        .map(|(chunk, document)| ChunkCandidate {
            anchor: structural_anchor(root, chunk),
            revision: ChunkRevision::from_document(document),
        })
        .collect();
    let prior_chunks: Vec<PriorChunk> = prior
        .iter()
        .map(|meta| PriorChunk {
            id: meta.chunk_id,
            anchor: meta.anchor.clone(),
            revision: meta.revision,
        })
        .collect();
    let reconciled = reconcile_chunks(&prior_chunks, &candidates, allocator);

    Ok(chunks
        .iter()
        .zip(candidates)
        .zip(reconciled)
        .map(|((chunk, candidate), lineage)| {
            let file_rel = root_relative(root, &chunk.file_path);
            let identity = format!("{:032x}", lineage.id.0);
            let key = chunk_id_key(lineage.id);
            (
                key,
                ChunkMeta {
                    identity,
                    chunk_id: lineage.id,
                    revision: candidate.revision,
                    anchor: candidate.anchor,
                    file_rel_path: file_rel,
                    function_name: chunk.function_name.clone(),
                    class_name: chunk.class_name.clone(),
                    line_start: chunk.line_start,
                    line_end: chunk.line_end,
                    content_hash: chunk.content_hash.clone(),
                    structure: chunk.structure.clone(),
                },
            )
        })
        .collect())
}

fn structural_anchor(root: &Path, chunk: &CodeChunk) -> StructuralAnchor {
    let repository_path = if chunk.structure.repository_path.is_empty() {
        root_relative(root, &chunk.file_path)
    } else {
        chunk.structure.repository_path.clone()
    };
    let enclosing_symbol = chunk.class_name.clone();
    let qualified_symbol = chunk.structure.qualified_symbol.clone().or_else(|| {
        chunk.function_name.as_ref().map(|function| {
            chunk
                .class_name
                .as_ref()
                .map(|class| format!("{class}::{function}"))
                .unwrap_or_else(|| function.clone())
        })
    });
    StructuralAnchor {
        repository_path,
        qualified_symbol,
        enclosing_symbol,
        signature: chunk.structure.signature.clone(),
        role: chunk.structure.role,
        ast_path: chunk.structure.ast_path.clone(),
    }
}

/// Path relative to the build `root`, used as part of the stable chunk key.
///
/// A silent raw-path fallback on a `strip_prefix` miss would re-introduce the
/// absolute-vs-relative key divergence that caused the daemon re-embed bug
/// (TLDR-atc/ss3), so the misses are handled deterministically and never
/// silently:
/// 1. lexical strip (the normal case — chunk paths are root-prefixed);
/// 2. canonical strip (symlinked root, mixed abs/rel, normalization);
/// 3. outside the root → the **canonical absolute** path (deterministic), warned;
/// 4. un-canonicalizable (file gone) → the raw path, but **warned** so the
///    divergence is diagnosable rather than silent.
pub fn root_relative(root: &Path, file_path: &Path) -> String {
    if let Ok(rel) = file_path.strip_prefix(root) {
        return normalize_sep(rel);
    }
    if let (Ok(cfile), Ok(croot)) = (file_path.canonicalize(), root.canonicalize()) {
        if let Ok(rel) = cfile.strip_prefix(&croot) {
            return normalize_sep(rel);
        }
        eprintln!(
            "[tldr-warn] vector_store: {} is outside root {}; keying by canonical path",
            cfile.display(),
            croot.display()
        );
        return normalize_sep(&cfile);
    }
    eprintln!(
        "[tldr-warn] vector_store: cannot canonicalize {} under root {}; keying by raw path",
        file_path.display(),
        root.display()
    );
    normalize_sep(file_path)
}

/// Normalize path separators to `/` for stable, cross-platform keys.
fn normalize_sep(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Plan and compose one changed file exactly like the raw whole-corpus build.
///
/// Corpus inclusion must be checked by the caller with
/// [`crate::semantic::is_corpus_file`] before invoking this helper.
pub fn plan_structural_delta(
    root: &Path,
    file: &Path,
    budget: &crate::semantic::TokenBudget,
    granularity: crate::semantic::ChunkGranularity,
) -> TldrResult<(Vec<CodeChunk>, Vec<String>)> {
    let result = crate::semantic::chunk_file(
        file,
        &crate::semantic::ChunkOptions {
            granularity: crate::semantic::ChunkGranularity::File,
            ..Default::default()
        },
    )?;
    let mut files = result.chunks;
    for chunk in &mut files {
        chunk.structure.repository_path = root_relative(root, &chunk.file_path);
    }
    let chunks = crate::semantic::structural_planner::plan_chunks(&files, budget, granularity)?;
    let documents = chunks
        .iter()
        .map(|chunk| crate::semantic::structural_planner::compose_minimal(chunk, budget))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| TldrError::Embedding(format!("delta composition failed: {error}")))?;
    Ok((chunks, documents))
}

/// Plan a changed file from the complete source chunk in the shared artifact
/// generation. This is the delta counterpart to `build_from_artifacts`.
pub fn plan_structural_delta_from_artifact(
    root: &Path,
    mut file: CodeChunk,
    budget: &crate::semantic::TokenBudget,
    granularity: crate::semantic::ChunkGranularity,
) -> TldrResult<(Vec<CodeChunk>, Vec<String>)> {
    file.structure.repository_path = root_relative(root, &file.file_path);
    let chunks = crate::semantic::structural_planner::plan_chunks(&[file], budget, granularity)?;
    let documents = chunks
        .iter()
        .map(|chunk| crate::semantic::structural_planner::compose_minimal(chunk, budget))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| TldrError::Embedding(format!("delta composition failed: {error}")))?;
    Ok((chunks, documents))
}

/// `(mtime_secs, size, kind)` for a path — the per-file reconcile signal.
/// Best-effort: an un-stattable path yields `(0, 0, Other)`. Also the signal a
/// delta stamps into the refreshed [`FileRecord`] (TLDR-t8f).
pub fn stat_signal(path: &Path) -> (u64, u64, FileKind) {
    match std::fs::symlink_metadata(path) {
        Ok(md) => {
            let ft = md.file_type();
            let kind = if ft.is_symlink() {
                FileKind::Symlink
            } else if ft.is_file() {
                FileKind::Regular
            } else {
                FileKind::Other
            };
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (mtime, md.len(), kind)
        }
        Err(_) => (0, 0, FileKind::Other),
    }
}

impl VectorStore {
    /// Build a store from `chunks` and their aligned embedding `vectors` (so
    /// `vectors[i]` embeds `chunks[i]`), rooted at `root`. Computes each chunk's
    /// stable u64 key with per-file ordinal disambiguation, fills the sidecar and
    /// the per-file records. This is the in-process populate; the caller supplies
    /// chunking + embedding (and the content-addressed dedup via EmbeddingCache).
    pub fn from_embedded(
        chunks: &[CodeChunk],
        vectors: &[Vec<f32>],
        root: &Path,
    ) -> TldrResult<Self> {
        let documents: Vec<String> = chunks.iter().map(|chunk| chunk.content.clone()).collect();
        Self::from_embedded_documents(chunks, vectors, &documents, root)
    }

    fn from_embedded_documents(
        chunks: &[CodeChunk],
        vectors: &[Vec<f32>],
        documents: &[String],
        root: &Path,
    ) -> TldrResult<Self> {
        if chunks.len() != vectors.len() {
            return Err(vs_err(
                "build",
                format!("chunks {} != vectors {}", chunks.len(), vectors.len()),
            ));
        }
        let dimensions = match vectors.first() {
            Some(v) if !v.is_empty() => v.len(),
            _ => return Err(vs_err("build", "empty or zero-dimension vectors")),
        };

        let mut store = Self::new(dimensions, chunks.len())?;
        // Identical key/meta computation to the delta path (shared `key_chunks`),
        // so a delta's remove/replace lands on the same keys this build wrote.
        let keyed = key_chunks_reconciled(root, chunks, documents, &[])?;
        let mut file_keys: HashMap<String, std::collections::BTreeSet<u64>> = HashMap::new();
        let mut file_abs: HashMap<String, PathBuf> = HashMap::new();

        for ((key, meta), (chunk, vector)) in keyed.iter().zip(chunks.iter().zip(vectors.iter())) {
            // add() detects a u64 key collision between distinct identities.
            store.add(*key, vector, meta.clone())?;
            file_keys
                .entry(meta.file_rel_path.clone())
                .or_default()
                .insert(*key);
            file_abs
                .entry(meta.file_rel_path.clone())
                .or_insert_with(|| chunk.file_path.clone());
        }

        for (file_rel, keys) in file_keys {
            let (mtime, size, file_type) = file_abs
                .get(&file_rel)
                .map(|p| stat_signal(p))
                .unwrap_or((0, 0, FileKind::Other));
            store.set_file_record(
                file_rel,
                FileRecord {
                    keys,
                    mtime,
                    size,
                    file_type,
                },
            );
        }
        Ok(store)
    }

    /// Incrementally add one complete planned file to a build-local store.
    ///
    /// Keeping the complete file together preserves stable lineage allocation;
    /// the surrounding streaming build drops its source/doc/vector window
    /// immediately afterward. The store itself is not published until every
    /// file succeeds.
    fn insert_embedded_file(
        &mut self,
        chunks: &[CodeChunk],
        vectors: &[Vec<f32>],
        documents: &[String],
        root: &Path,
        lineage_allocator: &mut ChunkIdAllocator,
    ) -> TldrResult<()> {
        if chunks.len() != vectors.len() || chunks.len() != documents.len() {
            return Err(vs_err(
                "stream_sink",
                format!(
                    "unaligned file payloads: chunks={} vectors={} documents={}",
                    chunks.len(),
                    vectors.len(),
                    documents.len()
                ),
            ));
        }
        if chunks.is_empty() {
            return Ok(());
        }
        if vectors.iter().any(|vector| vector.len() != self.dimensions) {
            return Err(vs_err(
                "stream_sink",
                "embedding dimensions differ from the target store",
            ));
        }
        let keyed =
            key_chunks_reconciled_with_allocator(root, chunks, documents, &[], lineage_allocator)?;
        let file_rel = keyed[0].1.file_rel_path.clone();
        if keyed.iter().any(|(_, meta)| meta.file_rel_path != file_rel) {
            return Err(vs_err(
                "stream_sink",
                "insert_embedded_file received more than one file",
            ));
        }
        let mut keys = std::collections::BTreeSet::new();
        for ((key, meta), vector) in keyed.iter().zip(vectors) {
            self.add(*key, vector, meta.clone())?;
            keys.insert(*key);
        }
        let (mtime, size, file_type) = stat_signal(&chunks[0].file_path);
        if let Some(existing) = self.files.get_mut(&file_rel) {
            existing.keys.extend(keys);
            existing.mtime = mtime;
            existing.size = size;
            existing.file_type = file_type;
        } else {
            self.set_file_record(
                file_rel,
                FileRecord {
                    keys,
                    mtime,
                    size,
                    file_type,
                },
            );
        }
        Ok(())
    }

    /// Production build: chunk `root`, embed each chunk (reusing the
    /// content-addressed [`EmbeddingCache`] for dedup), and populate the store.
    ///
    /// This mirrors [`crate::semantic::SemanticIndex::build`]'s embed loop and
    /// shares `chunk_code` + `Embedder` + `EmbeddingCache`, so it produces the
    /// **same vectors** — the basis for results-equivalence (TLDR-l5d acceptance,
    /// validated on the n=52 eval). Embeds raw `content` (enrichment is off by
    /// default, matching the index's default path).
    pub fn build(
        root: &Path,
        options: &BuildOptions,
        cache_config: Option<CacheConfig>,
    ) -> TldrResult<Self> {
        Self::build_with_backend(
            root,
            options,
            cache_config,
            crate::semantic::DocumentEmbeddingBackend::from_env()?,
        )
    }

    /// Build with an explicit document backend.
    ///
    /// Production callers normally use [`Self::build`], whose staged-default
    /// selector comes from `TLDR_EMBEDDING_BACKEND`. Tests, benchmarks, and
    /// rollback validation use this entry point to avoid process-global
    /// environment mutation.
    pub fn build_with_backend(
        root: &Path,
        options: &BuildOptions,
        cache_config: Option<CacheConfig>,
        document_backend: crate::semantic::DocumentEmbeddingBackend,
    ) -> TldrResult<Self> {
        Self::build_with_backend_and_control(
            root,
            options,
            cache_config,
            document_backend,
            crate::semantic::StreamingBuildConfig::default(),
            crate::semantic::BuildCancellation::default(),
        )
    }

    /// Bounded streaming build with explicit resource and cancellation control.
    pub fn build_with_backend_and_control(
        root: &Path,
        options: &BuildOptions,
        cache_config: Option<CacheConfig>,
        document_backend: crate::semantic::DocumentEmbeddingBackend,
        pipeline_config: crate::semantic::StreamingBuildConfig,
        cancellation: crate::semantic::BuildCancellation,
    ) -> TldrResult<Self> {
        build_streaming_store(
            root,
            options,
            cache_config,
            document_backend,
            pipeline_config,
            cancellation,
            None,
            None,
        )
    }

    /// Build from complete source chunks emitted by the shared artifact
    /// generation. The semantic worker still owns inference and usearch
    /// publication, but no longer walks, reads, or initially parses source.
    pub fn build_from_artifacts(
        root: &Path,
        options: &BuildOptions,
        cache_config: Option<CacheConfig>,
        source_chunks: Vec<CodeChunk>,
    ) -> TldrResult<Self> {
        build_streaming_store(
            root,
            options,
            cache_config,
            crate::semantic::DocumentEmbeddingBackend::from_env()?,
            crate::semantic::StreamingBuildConfig::default(),
            crate::semantic::BuildCancellation::default(),
            Some(source_chunks),
            None,
        )
    }

    /// Build from shared artifacts while reporting durable window progress.
    pub fn build_from_artifacts_with_progress(
        root: &Path,
        options: &BuildOptions,
        cache_config: Option<CacheConfig>,
        source_chunks: Vec<CodeChunk>,
        observer: &mut dyn FnMut(crate::semantic::BuildProgress) -> TldrResult<()>,
    ) -> TldrResult<Self> {
        build_streaming_store(
            root,
            options,
            cache_config,
            crate::semantic::DocumentEmbeddingBackend::from_env()?,
            crate::semantic::StreamingBuildConfig::default(),
            crate::semantic::BuildCancellation::default(),
            Some(source_chunks),
            Some(observer),
        )
    }
}

struct PendingStreamFile {
    path: PathBuf,
    chunks: Vec<CodeChunk>,
    documents: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WindowOutcome {
    cache_hits: usize,
    new_vectors: usize,
    duration_ms: u64,
}

enum StreamingEmbedder {
    Fast(crate::semantic::Embedder),
    Fixed(crate::semantic::FixedShapeEmbedder),
}

impl StreamingEmbedder {
    fn token_budget(&self) -> &crate::semantic::TokenBudget {
        match self {
            Self::Fast(embedder) => embedder
                .token_budget()
                .expect("streaming embedder checked tokenizer configuration"),
            Self::Fixed(embedder) => embedder.token_budget(),
        }
    }

    fn check_corpus(&self, texts: &[&str]) -> crate::semantic::token_budget::TokenStats {
        match self {
            Self::Fast(embedder) => embedder.check_corpus(texts),
            Self::Fixed(embedder) => {
                let mut stats = crate::semantic::token_budget::TokenStats::default();
                for text in texts {
                    match embedder.token_budget().check(text) {
                        Ok(check) => stats.record(check),
                        Err(_) => stats.mark_unavailable(),
                    }
                }
                stats
            }
        }
    }

    fn embed(&mut self, indexed: Vec<(usize, &str)>) -> TldrResult<Vec<(usize, Vec<f32>)>> {
        match self {
            Self::Fast(embedder) => embedder.embed_batch_indexed(indexed, false),
            Self::Fixed(embedder) => embedder.embed_indexed(indexed),
        }
    }

    fn take_fixed_executions(&mut self) -> Vec<crate::semantic::FixedShapeExecution> {
        match self {
            Self::Fast(_) => Vec::new(),
            Self::Fixed(embedder) => embedder.take_executions(),
        }
    }
}

fn build_streaming_store(
    root: &Path,
    options: &BuildOptions,
    cache_config: Option<CacheConfig>,
    document_backend: crate::semantic::DocumentEmbeddingBackend,
    pipeline_config: crate::semantic::StreamingBuildConfig,
    cancellation: crate::semantic::BuildCancellation,
    source_artifacts: Option<Vec<CodeChunk>>,
    mut observer: Option<&mut dyn FnMut(crate::semantic::BuildProgress) -> TldrResult<()>>,
) -> TldrResult<VectorStore> {
    use crate::semantic::build_pipeline::{BuildPipelineError, PipelineStage};
    use crate::semantic::cache::EmbeddingCache;
    use crate::semantic::enrichment::enrich_chunks;
    use crate::semantic::index::{BYTES_PER_CHUNK, MAX_INDEX_SIZE, MAX_MEMORY_BYTES};
    let build_started = std::time::Instant::now();
    let capacities = pipeline_config.capacities().map_err(pipeline_error)?;
    let corpus_digest = compute_corpus_digest(root);
    let enrich = std::env::var("TLDR_ENRICH")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let mut metrics = options.collect_metrics.then(|| {
        crate::semantic::build_metrics::BuildMetrics::new(
            options.model.model_name(),
            options.model.dimensions(),
            root.to_string_lossy().into_owned(),
            corpus_digest,
            options,
            enrich,
            document_backend,
        )
    });
    let mut telemetry = crate::semantic::PipelineTelemetry {
        capacities: Some(capacities),
        ..Default::default()
    };

    if let Some(metrics) = metrics.as_mut() {
        // Preserve the established report phase name while the implementation
        // beneath it now enumerates and chunks incrementally.
        metrics.begin_phase("chunk");
    }
    let languages = options.languages.as_ref().map(|languages| {
        languages
            .iter()
            .filter_map(|language| crate::Language::from_extension(language))
            .collect::<Vec<_>>()
    });
    let mut artifact_files = source_artifacts.map(|chunks| {
        chunks
            .into_iter()
            .map(|chunk| (chunk.file_path.clone(), chunk))
            .collect::<std::collections::HashMap<_, _>>()
    });
    let mut files = artifact_files
        .as_ref()
        .map(|chunks| chunks.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_else(|| crate::semantic::chunker::enumerate_corpus_files(root));
    files.sort();
    if let Some(languages) = languages.as_ref() {
        files.retain(|file| {
            crate::Language::from_path(file).is_some_and(|language| languages.contains(&language))
        });
    }
    let files_total = files.len() as u64;
    emit_progress(
        &mut observer,
        crate::semantic::BuildProgress {
            phase: crate::semantic::BuildPhase::Planning,
            files_seen: 0,
            files_total: Some(files_total),
            windows_completed: 0,
            cache_hits: 0,
            new_vectors: 0,
            last_window_duration_ms: None,
            elapsed_ms: build_started.elapsed().as_millis() as u64,
            retries: 0,
        },
    )?;
    if let Some(metrics) = metrics.as_mut() {
        metrics.end_phase();
    }

    let mut store = VectorStore::new(options.model.dimensions(), VectorStore::MIN_CAPACITY)?;
    if files.is_empty() {
        store.corpus_digest = corpus_digest;
        if let Some(mut metrics) = metrics {
            metrics.set_pipeline_telemetry(telemetry);
            store.build_metrics = Some(metrics.finalize(0));
        }
        return Ok(store);
    }

    cancellation.check().map_err(pipeline_error)?;
    emit_progress(
        &mut observer,
        crate::semantic::BuildProgress {
            phase: crate::semantic::BuildPhase::ModelLoad,
            files_seen: 0,
            files_total: Some(files_total),
            windows_completed: 0,
            cache_hits: 0,
            new_vectors: 0,
            last_window_duration_ms: None,
            elapsed_ms: build_started.elapsed().as_millis() as u64,
            retries: 0,
        },
    )?;
    if let Some(metrics) = metrics.as_mut() {
        metrics.begin_phase("model_load");
    }
    let oracle = crate::semantic::Embedder::new(options.model)?;
    if oracle.token_budget().is_none() {
        return Err(TldrError::Embedding(
            "structural planning requires FastEmbed tokenizer configuration".into(),
        ));
    }
    let mut embedder = match document_backend {
        crate::semantic::DocumentEmbeddingBackend::FastEmbed => StreamingEmbedder::Fast(oracle),
        crate::semantic::DocumentEmbeddingBackend::FixedShapeOrt => StreamingEmbedder::Fixed(
            oracle.into_fixed_shape(crate::semantic::OrtBackendConfig::default())?,
        ),
    };
    if let Some(metrics) = metrics.as_mut() {
        metrics.end_phase();
    }

    let mut cache = if options.use_cache {
        cache_config.map(EmbeddingCache::open).transpose()?
    } else {
        None
    };
    if let Some(cache) = cache.as_mut() {
        cache.set_key_root(root);
    }
    if let Some(metrics) = metrics.as_mut() {
        metrics.record_cache_opened(cache.is_some());
        metrics.begin_phase("streaming_pipeline");
    }
    let cache_recipe =
        crate::semantic::EmbeddingRecipeId::for_document(options.model, embed_schema_tag());
    let producer = crate::semantic::build_pipeline::FileProducer::spawn(
        files,
        capacities.files,
        cancellation.clone(),
    )
    .map_err(pipeline_error)?;
    let mut build_stats = crate::semantic::chunker::ChunkStats::default();
    // Allocation order remains corpus-global even though payloads are flushed
    // per window, preserving the whole-corpus stable IDs exactly.
    let mut lineage_allocator = chunk_id_allocator(root, &[]);
    let mut total_chunks = 0usize;
    let mut token_stats = crate::semantic::token_budget::TokenStats::default();
    let mut window = Vec::<PendingStreamFile>::new();
    let mut window_items = 0usize;
    let mut window_bytes = 0usize;
    let mut cache_hits = 0usize;
    let mut new_vectors = 0usize;
    let mut last_window_duration_ms = None;
    let max_window_items = capacities.chunks.clamp(1, EMBEDDING_WINDOW_VECTORS);
    let max_file_bytes = (pipeline_config.memory_budget_bytes / 4).max(1);
    let max_window_bytes = pipeline_config
        .memory_budget_bytes
        .saturating_sub(max_file_bytes)
        .max(1);

    while let Some(file) = producer.recv() {
        let file_started = std::time::Instant::now();
        cancellation.check().map_err(pipeline_error)?;
        telemetry.files_seen += 1;
        let mut source_files = if let Some(artifacts) = artifact_files.as_mut() {
            artifacts.remove(&file).into_iter().collect()
        } else {
            let result = crate::semantic::chunk_file(
                &file,
                &crate::semantic::ChunkOptions {
                    granularity: crate::semantic::ChunkGranularity::File,
                    languages: languages.clone(),
                    ..Default::default()
                },
            )
            .map_err(|error| {
                pipeline_error(
                    BuildPipelineError::new(PipelineStage::Parse, error.to_string()).at_file(&file),
                )
            })?;
            build_stats.files_skipped += result.stats.files_skipped;
            build_stats.files_unsupported += result.stats.files_unsupported;
            build_stats.files_oversized += result.stats.files_oversized;
            result.chunks
        };
        build_stats.files_indexed += usize::from(!source_files.is_empty());
        for chunk in &mut source_files {
            chunk.structure.repository_path = root_relative(root, &chunk.file_path);
        }
        let chunks = crate::semantic::structural_planner::plan_chunks(
            &source_files,
            embedder.token_budget(),
            options.granularity,
        )
        .map_err(|error| {
            pipeline_error(
                BuildPipelineError::new(PipelineStage::Parse, error.to_string()).at_file(&file),
            )
        })?;
        if chunks.is_empty() {
            continue;
        }
        let documents = if enrich {
            enrich_chunks(&chunks, root)
                .iter()
                .map(|unit| {
                    crate::semantic::structural_planner::compose_enriched(
                        unit,
                        embedder.token_budget(),
                    )
                })
                .collect::<Result<Vec<_>, _>>()
        } else {
            chunks
                .iter()
                .map(|chunk| {
                    crate::semantic::structural_planner::compose_minimal(
                        chunk,
                        embedder.token_budget(),
                    )
                })
                .collect::<Result<Vec<_>, _>>()
        }
        .map_err(|error| {
            pipeline_error(
                BuildPipelineError::new(PipelineStage::Compose, error.to_string()).at_file(&file),
            )
        })?;
        if let Some(metrics) = metrics.as_mut() {
            metrics.record_unit(
                "semantic_file_plan",
                root_relative(root, &file),
                file_started.elapsed().as_millis() as u64,
            );
        }
        for (chunk, document) in chunks.into_iter().zip(documents) {
            let payload_bytes = chunk.content.len() * 2
                + document.len()
                + options.model.dimensions() * std::mem::size_of::<f32>();
            if payload_bytes > max_file_bytes {
                return Err(pipeline_error(
                    BuildPipelineError::new(
                        PipelineStage::Compose,
                        format!(
                            "one chunk needs {payload_bytes} bytes; per-unit bound is {max_file_bytes}"
                        ),
                    )
                    .at_file(&file),
                ));
            }
            if !window.is_empty()
                && (window_items + 1 > max_window_items
                    || window_bytes + payload_bytes > max_window_bytes)
            {
                let outcome = flush_streaming_window(
                    &mut window,
                    &mut store,
                    root,
                    &mut embedder,
                    &mut cache,
                    &cache_recipe,
                    metrics.as_mut(),
                    &mut token_stats,
                    &mut telemetry,
                    &cancellation,
                    &mut lineage_allocator,
                )?
                .expect("non-empty streaming window");
                cache_hits += outcome.cache_hits;
                new_vectors += outcome.new_vectors;
                last_window_duration_ms = Some(outcome.duration_ms);
                emit_progress(
                    &mut observer,
                    crate::semantic::BuildProgress {
                        phase: crate::semantic::BuildPhase::Embedding,
                        files_seen: telemetry.files_seen as u64,
                        files_total: Some(files_total),
                        windows_completed: telemetry.windows_completed as u64,
                        cache_hits: cache_hits as u64,
                        new_vectors: new_vectors as u64,
                        last_window_duration_ms,
                        elapsed_ms: build_started.elapsed().as_millis() as u64,
                        retries: 0,
                    },
                )?;
                window_items = 0;
                window_bytes = 0;
            }
            window_items += 1;
            window_bytes += payload_bytes;
            telemetry.observe_window(window_items, window_bytes);
            window.push(PendingStreamFile {
                path: file.clone(),
                chunks: vec![chunk],
                documents: vec![document],
            });
            total_chunks += 1;
            if total_chunks > MAX_INDEX_SIZE {
                return Err(TldrError::IndexTooLarge {
                    count: total_chunks,
                    max: MAX_INDEX_SIZE,
                });
            }
            let estimated_memory = total_chunks * BYTES_PER_CHUNK;
            if estimated_memory > MAX_MEMORY_BYTES {
                return Err(TldrError::MemoryLimitExceeded {
                    estimated_mb: estimated_memory / (1024 * 1024),
                    max_mb: MAX_MEMORY_BYTES / (1024 * 1024),
                });
            }
        }
    }
    let final_outcome = flush_streaming_window(
        &mut window,
        &mut store,
        root,
        &mut embedder,
        &mut cache,
        &cache_recipe,
        metrics.as_mut(),
        &mut token_stats,
        &mut telemetry,
        &cancellation,
        &mut lineage_allocator,
    )?;
    if let Some(outcome) = final_outcome {
        cache_hits += outcome.cache_hits;
        new_vectors += outcome.new_vectors;
        last_window_duration_ms = Some(outcome.duration_ms);
        emit_progress(
            &mut observer,
            crate::semantic::BuildProgress {
                phase: crate::semantic::BuildPhase::Embedding,
                files_seen: telemetry.files_seen as u64,
                files_total: Some(files_total),
                windows_completed: telemetry.windows_completed as u64,
                cache_hits: cache_hits as u64,
                new_vectors: new_vectors as u64,
                last_window_duration_ms,
                elapsed_ms: build_started.elapsed().as_millis() as u64,
                retries: 0,
            },
        )?;
    }
    telemetry.producer_backpressure_events = producer.finish().map_err(pipeline_error)?;
    build_stats.chunks_created = total_chunks;
    store.corpus_digest = corpus_digest;
    store.build_stats = build_stats;
    if let Some(mut metrics) = metrics {
        metrics.set_token_stats(token_stats);
        metrics.set_pipeline_telemetry(telemetry.clone());
        store.build_metrics = Some(metrics.finalize(total_chunks));
    }
    emit_progress(
        &mut observer,
        crate::semantic::BuildProgress {
            phase: crate::semantic::BuildPhase::Verifying,
            files_seen: telemetry.files_seen as u64,
            files_total: Some(files_total),
            windows_completed: telemetry.windows_completed as u64,
            cache_hits: cache_hits as u64,
            new_vectors: new_vectors as u64,
            last_window_duration_ms,
            elapsed_ms: build_started.elapsed().as_millis() as u64,
            retries: 0,
        },
    )?;
    Ok(store)
}

#[allow(clippy::too_many_arguments)]
fn flush_streaming_window(
    window: &mut Vec<PendingStreamFile>,
    store: &mut VectorStore,
    root: &Path,
    embedder: &mut StreamingEmbedder,
    cache: &mut Option<crate::semantic::cache::EmbeddingCache>,
    cache_recipe: &crate::semantic::EmbeddingRecipeId,
    mut metrics: Option<&mut crate::semantic::build_metrics::BuildMetrics>,
    token_stats: &mut crate::semantic::token_budget::TokenStats,
    telemetry: &mut crate::semantic::PipelineTelemetry,
    cancellation: &crate::semantic::BuildCancellation,
    lineage_allocator: &mut ChunkIdAllocator,
) -> TldrResult<Option<WindowOutcome>> {
    use crate::semantic::build_pipeline::{BuildPipelineError, PipelineStage};
    if window.is_empty() {
        return Ok(None);
    }
    let window_started = std::time::Instant::now();
    cancellation.check().map_err(pipeline_error)?;
    let total = window.iter().map(|file| file.chunks.len()).sum::<usize>();
    let mut cache_chunks = Vec::with_capacity(total);
    let mut document_refs = Vec::with_capacity(total);
    for file in window.iter() {
        for (chunk, document) in file.chunks.iter().zip(&file.documents) {
            let mut cache_chunk = chunk.clone();
            cache_chunk.content_hash = format!("{:x}", md5::compute(document.as_bytes()));
            cache_chunks.push(cache_chunk);
            document_refs.push(document.as_str());
        }
    }
    let mut vectors = vec![None; total];
    let mut misses = Vec::new();
    let cache_lookup_started = std::time::Instant::now();
    for (index, (chunk, document)) in cache_chunks.iter().zip(&document_refs).enumerate() {
        match cache
            .as_mut()
            .and_then(|cache| cache.get_document(chunk, document, cache_recipe))
        {
            Some(vector) => vectors[index] = Some(vector),
            None => misses.push(index),
        }
    }
    if let Some(metrics) = metrics.as_mut() {
        metrics.record_unit(
            "cache_lookup_window",
            telemetry.windows_completed.to_string(),
            cache_lookup_started.elapsed().as_millis() as u64,
        );
    }
    if let Some(metrics) = metrics.as_mut() {
        metrics.record_cache(total - misses.len(), misses.len());
        let mut lengths = misses
            .iter()
            .map(|index| document_refs[*index].len())
            .collect::<Vec<_>>();
        lengths.sort_unstable();
        metrics.record_embed_inputs(lengths);
    }
    token_stats.merge(&embedder.check_corpus(&document_refs));
    if !misses.is_empty() {
        cancellation.check().map_err(pipeline_error)?;
        let indexed = misses
            .iter()
            .map(|index| (*index, document_refs[*index]))
            .collect::<Vec<_>>();
        let started = std::time::Instant::now();
        let embedded = embedder.embed(indexed).map_err(|error| {
            pipeline_error(BuildPipelineError::new(
                PipelineStage::Inference,
                error.to_string(),
            ))
        })?;
        if let Some(metrics) = metrics.as_mut() {
            let inference_ms = started.elapsed().as_millis() as u64;
            metrics.record_embed_latency_ms(inference_ms);
            metrics.record_unit(
                "inference_window",
                telemetry.windows_completed.to_string(),
                inference_ms,
            );
            metrics.record_fixed_executions(embedder.take_fixed_executions());
        }
        for (index, vector) in embedded {
            if !misses.contains(&index) || vectors[index].replace(vector).is_some() {
                return Err(pipeline_error(
                    BuildPipelineError::new(
                        PipelineStage::Inference,
                        "backend returned an unexpected or duplicate index",
                    )
                    .at_chunk(index.to_string()),
                ));
            }
        }
    }
    let cache_write_started = std::time::Instant::now();
    let vectors = vectors
        .into_iter()
        .enumerate()
        .map(|(index, vector)| {
            vector.ok_or_else(|| {
                pipeline_error(
                    BuildPipelineError::new(
                        PipelineStage::Inference,
                        "backend omitted an embedding",
                    )
                    .at_chunk(index.to_string()),
                )
            })
        })
        .collect::<TldrResult<Vec<_>>>()?;
    for (index, vector) in vectors.iter().enumerate() {
        if let Some(cache) = cache.as_mut() {
            cache.put_document(
                &cache_chunks[index],
                document_refs[index],
                vector.clone(),
                cache_recipe,
            );
        }
    }
    if let Some(cache) = cache.as_mut() {
        cache.flush().map_err(|error| {
            pipeline_error(BuildPipelineError::new(
                PipelineStage::Cache,
                error.to_string(),
            ))
        })?;
    }
    if let Some(metrics) = metrics.as_mut() {
        metrics.record_unit(
            "cache_write_window",
            telemetry.windows_completed.to_string(),
            cache_write_started.elapsed().as_millis() as u64,
        );
    }
    let assembly_started = std::time::Instant::now();
    let mut offset = 0;
    for file in window.iter() {
        let end = offset + file.chunks.len();
        store
            .insert_embedded_file(
                &file.chunks,
                &vectors[offset..end],
                &file.documents,
                root,
                lineage_allocator,
            )
            .map_err(|error| {
                pipeline_error(
                    BuildPipelineError::new(PipelineStage::Sink, error.to_string())
                        .at_file(&file.path),
                )
            })?;
        offset = end;
    }
    if let Some(metrics) = metrics.as_mut() {
        metrics.record_unit(
            "vector_assembly_window",
            telemetry.windows_completed.to_string(),
            assembly_started.elapsed().as_millis() as u64,
        );
    }
    telemetry.windows_completed += 1;
    let duration_ms = window_started.elapsed().as_millis() as u64;
    if let Some(metrics) = metrics.as_mut() {
        metrics.record_unit(
            "embedding_window",
            telemetry.windows_completed.saturating_sub(1).to_string(),
            duration_ms,
        );
    }
    window.clear();
    Ok(Some(WindowOutcome {
        cache_hits: total - misses.len(),
        new_vectors: misses.len(),
        duration_ms,
    }))
}

fn emit_progress(
    observer: &mut Option<&mut dyn FnMut(crate::semantic::BuildProgress) -> TldrResult<()>>,
    progress: crate::semantic::BuildProgress,
) -> TldrResult<()> {
    match observer.as_deref_mut() {
        Some(observer) => observer(progress),
        None => Ok(()),
    }
}

fn pipeline_error(error: crate::semantic::BuildPipelineError) -> TldrError {
    TldrError::Embedding(error.to_string())
}

impl ManifestId {
    /// Derive the manifest identity from the build config. A change to ANY field
    /// here invalidates the persisted store on load (design doc §4.0). The `root`
    /// is **canonicalized** so abs/rel/symlinked invocations produce the same
    /// identity. `chunk_params` and `walker_version` are stable digests of the
    /// chunk options / ignore-rule set supplied by the caller, and `model_revision`
    /// is currently `model_name()` — encoding the tokenizer+weights revision and
    /// the chunk/walker inputs more fully is a §14 open item (TLDR-l5d follow-up).
    pub fn for_build(
        model: EmbeddingModel,
        root: &Path,
        chunk_params: &str,
        walker_version: &str,
    ) -> Self {
        let root = root
            .canonicalize()
            .unwrap_or_else(|_| root.to_path_buf())
            .to_string_lossy()
            .replace('\\', "/");
        Self {
            embedding_model: format!("{model:?}"),
            model_revision: model.model_name().to_string(),
            dimensions: model.dimensions() as u32,
            metric: "cos".to_string(),
            scalar_kind: "f32".to_string(),
            search_mode: "exact".to_string(),
            embed_schema: embed_schema_tag(),
            chunk_params: chunk_params.to_string(),
            walker_version: walker_version.to_string(),
            root,
        }
    }
}

/// The embed-input recipe tag (raw vs enriched), mirroring the embedding-cache
/// key's schema tag so a recipe change invalidates the persisted store.
fn embed_schema_tag() -> String {
    let enrich = std::env::var("TLDR_ENRICH")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if enrich {
        "enriched-v3-structural".to_string()
    } else {
        "raw-v3-structural".to_string()
    }
}

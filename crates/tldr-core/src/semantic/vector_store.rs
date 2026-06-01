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

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use crate::error::TldrError;
use crate::semantic::types::CodeChunk;
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkMeta {
    /// `file_rel_path::class::function::ordinal` — the source of the u64 key.
    pub identity: String,
    /// Root-relative path (CWD/absolute-independent).
    pub file_rel_path: String,
    pub function_name: Option<String>,
    pub class_name: Option<String>,
    pub line_start: u32,
    pub line_end: u32,
    /// Detects body changes; also anchors the lazy snippet read.
    pub content_hash: String,
}

/// A search result: the matched key, its cosine **distance** (lower = closer;
/// cosine similarity ≈ `1 - distance`), and the chunk's sidecar metadata.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub key: u64,
    pub distance: f32,
    pub meta: ChunkMeta,
}

/// usearch-backed vector store: `key(u64) -> vector` (the usearch index) paired
/// with a `key -> ChunkMeta` sidecar. One store per embedding model (the vector
/// dimensionality is fixed per model). Persistence (manifest + crash-safe
/// generation/`CURRENT` save) lands in the next step; this is the in-memory core.
///
/// `Send` but not `Sync` (the usearch `Index` is not `Sync`); it lives behind the
/// daemon's `Mutex` like `SemanticIndex`.
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

    /// Number of vectors currently in the store.
    pub fn len(&self) -> usize {
        self.index.size()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

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
        // usearch does not auto-grow; reserve more before we run out.
        if self.index.size() >= self.capacity {
            self.capacity = self.capacity.saturating_mul(2).max(self.index.size() + 1);
            self.index
                .reserve(self.capacity)
                .map_err(|e| vs_err("reserve", e))?;
        }
        // Replace semantics: drop any existing vector for this key first.
        if self.index.contains(key) {
            self.index.remove(key).map_err(|e| vs_err("remove", e))?;
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
const STORE_FORMAT_VERSION: u32 = 1;
/// `CURRENT` magic ("TLDR") so a torn/foreign pointer is detectable.
const CURRENT_MAGIC: u32 = 0x544C_4452;
/// Generations retained by GC (the active one + rollback headroom). Keeps a
/// concurrent reader's snapshot alive across a few saves (design doc §7.1).
const KEEP_GENS: u64 = 3;

/// What kind of filesystem object a tracked path was at index time — lets
/// reconcile (§7.3) detect file↔dir/type swaps, not just content changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileKind {
    Regular,
    Symlink,
    Other,
}

/// Per-file record (design doc §4.3): which keys belong to the file plus the
/// `(mtime, size, file_type)` reconcile signal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileRecord {
    pub keys: std::collections::BTreeSet<u64>,
    pub mtime: u64,
    pub size: u64,
    pub file_type: FileKind,
}

/// The subset of the manifest that must match the running config on `load`, or
/// the persisted store is incompatible and the caller must full-rebuild
/// (design doc §4.0). Every field here changes the vectors OR the chunk
/// boundaries, so a mismatch means the stored vectors can't be trusted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestId {
    pub embedding_model: String,
    /// Weights + tokenizer revision — a tokenizer bump invalidates vectors even
    /// under the same model name.
    pub model_revision: String,
    pub dimensions: u32,
    pub metric: String,
    pub scalar_kind: String,
    pub search_mode: String,
    pub embed_schema: String,
    /// Digest of ChunkOptions (granularity/max_tokens/overlap/lang filter).
    pub chunk_params: String,
    /// Digest of the source-selection / ignore rules.
    pub walker_version: String,
    pub root: String,
}

#[derive(Serialize, Deserialize)]
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
}

/// Borrowed view for serialization (avoids cloning the sidecar on save).
#[derive(Serialize)]
struct SidecarRef<'a> {
    meta: &'a HashMap<u64, ChunkMeta>,
    files: &'a HashMap<String, FileRecord>,
}

/// Owned view for deserialization on load.
#[derive(Deserialize)]
struct SidecarOwned {
    meta: HashMap<u64, ChunkMeta>,
    files: HashMap<String, FileRecord>,
}

/// The structured `CURRENT` pointer — the single atomic commit point. `magic` +
/// `checksum` make a torn/partial write detectable (design doc §7.1).
#[derive(Serialize, Deserialize)]
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
                format!("id.dimensions {} != store {}", id.dimensions, self.dimensions),
            ));
        }
        std::fs::create_dir_all(dir)?;
        let gen = read_current(dir).map(|c| c.generation + 1).unwrap_or(1);

        // 1. index.<gen>.usearch (immutable; not referenced until CURRENT commits)
        let index_path = dir.join(format!("index.{gen}.usearch"));
        let index_str = index_path
            .to_str()
            .ok_or_else(|| vs_err("save", "non-utf8 index path"))?;
        self.index.save(index_str).map_err(|e| vs_err("save", e))?;
        sync_path(&index_path)?;
        let index_checksum = digest_bytes(&std::fs::read(&index_path)?);

        // 2. meta.<gen> (sidecar: key->ChunkMeta + per-file records)
        let sidecar = SidecarRef {
            meta: &self.meta,
            files: &self.files,
        };
        let sidecar_bytes = serde_json::to_vec(&sidecar).map_err(|e| vs_err("save", e))?;
        let sidecar_checksum = digest_bytes(&sidecar_bytes);
        write_sync(&dir.join(format!("meta.{gen}")), &sidecar_bytes)?;

        // 3. manifest.<gen>
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
        };
        let manifest_bytes = serde_json::to_vec(&manifest).map_err(|e| vs_err("save", e))?;
        write_sync(&dir.join(format!("manifest.{gen}")), &manifest_bytes)?;

        sync_dir(dir);

        // 4. CURRENT — the single atomic commit point (temp + rename).
        let cur = CurrentPointer {
            magic: CURRENT_MAGIC,
            generation: gen,
            checksum: current_checksum(CURRENT_MAGIC, gen),
        };
        let cur_bytes = serde_json::to_vec(&cur).map_err(|e| vs_err("save", e))?;
        let tmp = dir.join("CURRENT.tmp");
        write_sync(&tmp, &cur_bytes)?;
        std::fs::rename(&tmp, dir.join("CURRENT"))?;
        sync_dir(dir);

        // 5. GC — retain the last KEEP_GENS generations.
        gc_old_generations(dir, gen);
        Ok(())
    }

    /// Load the active generation from `dir`, verifying the manifest against the
    /// running config `expect`. Returns an error (→ caller full-rebuilds) on a
    /// missing/torn `CURRENT`, a config mismatch, or any checksum/drift failure.
    pub fn load(dir: &Path, expect: &ManifestId) -> TldrResult<Self> {
        let cur =
            read_current(dir).ok_or_else(|| vs_err("load", "missing or torn CURRENT pointer"))?;
        let gen = cur.generation;

        let manifest: Manifest =
            serde_json::from_slice(&std::fs::read(dir.join(format!("manifest.{gen}")))?)
                .map_err(|e| vs_err("load", e))?;
        if manifest.format_version != STORE_FORMAT_VERSION {
            return Err(vs_err("load", "format_version mismatch"));
        }
        if &manifest.id != expect {
            return Err(vs_err("load", "config mismatch (model/dims/params/root)"));
        }
        if manifest.generation != gen {
            return Err(vs_err("load", "manifest generation != CURRENT"));
        }

        let meta_bytes = std::fs::read(dir.join(format!("meta.{gen}")))?;
        if digest_bytes(&meta_bytes) != manifest.sidecar_checksum {
            return Err(vs_err("load", "sidecar checksum mismatch"));
        }
        let index_path = dir.join(format!("index.{gen}.usearch"));
        if digest_bytes(&std::fs::read(&index_path)?) != manifest.index_checksum {
            return Err(vs_err("load", "index checksum mismatch"));
        }

        let sidecar: SidecarOwned =
            serde_json::from_slice(&meta_bytes).map_err(|e| vs_err("load", e))?;
        let mut keys: Vec<u64> = sidecar.meta.keys().copied().collect();
        keys.sort_unstable();
        if keys_digest(&keys) != manifest.keys_checksum {
            return Err(vs_err("load", "keys checksum mismatch"));
        }

        let dimensions = expect.dimensions as usize;
        let capacity = sidecar.meta.len().max(Self::MIN_CAPACITY);
        let index = new_f32_index(dimensions, capacity)?;
        let index_str = index_path
            .to_str()
            .ok_or_else(|| vs_err("load", "non-utf8 index path"))?;
        index.load(index_str).map_err(|e| vs_err("load", e))?;
        if index.size() != sidecar.meta.len() {
            return Err(vs_err("load", "index size != sidecar count (drift)"));
        }

        Ok(Self {
            dimensions,
            capacity,
            index,
            meta: sidecar.meta,
            files: sidecar.files,
        })
    }
}

fn digest_bytes(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

fn keys_digest(sorted_keys: &[u64]) -> u64 {
    let mut h = DefaultHasher::new();
    sorted_keys.hash(&mut h);
    h.finish()
}

fn current_checksum(magic: u32, generation: u64) -> u32 {
    let mut h = DefaultHasher::new();
    (magic, generation).hash(&mut h);
    (h.finish() & 0xFFFF_FFFF) as u32
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

/// Best-effort directory fsync so renames/creates are durable. No-op where the
/// platform doesn't support opening a directory as a file.
fn sync_dir(dir: &Path) {
    if let Ok(f) = std::fs::File::open(dir) {
        let _ = f.sync_all();
    }
}

/// Read + validate the `CURRENT` pointer. `None` if missing, unparseable, wrong
/// magic, or failing its checksum (a torn write) — the caller then treats the
/// store as absent and rebuilds (the newest-verifying-manifest fallback scan is
/// a later step).
fn read_current(dir: &Path) -> Option<CurrentPointer> {
    let bytes = std::fs::read(dir.join("CURRENT")).ok()?;
    let cur: CurrentPointer = serde_json::from_slice(&bytes).ok()?;
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

/// Build the stable identity string for a chunk (design doc §4.1):
/// `file_rel_path::class::function::ordinal`. `ordinal` disambiguates duplicate
/// `(class, function)` names within one file. File-level chunks (no function)
/// use `file_rel_path#file`.
pub fn chunk_identity(
    file_rel_path: &str,
    class_name: Option<&str>,
    function_name: Option<&str>,
    ordinal: u32,
) -> String {
    match function_name {
        Some(f) => format!(
            "{}::{}::{}::{}",
            file_rel_path,
            class_name.unwrap_or(""),
            f,
            ordinal
        ),
        None => format!("{file_rel_path}#file"),
    }
}

/// Hash an identity string into the stable u64 usearch key.
pub fn identity_key(identity: &str) -> u64 {
    let mut h = DefaultHasher::new();
    identity.hash(&mut h);
    h.finish()
}

/// Path relative to the build `root`. On a `strip_prefix` miss, fall back to the
/// path as-is and normalize separators. (Hardening this miss to a canonical
/// fallback + warning is tracked as TLDR-ss3.)
fn root_relative(root: &Path, file_path: &Path) -> String {
    file_path
        .strip_prefix(root)
        .unwrap_or(file_path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// `(mtime_secs, size, kind)` for a path — the per-file reconcile signal.
/// Best-effort: an un-stattable path yields `(0, 0, Other)`.
fn stat_signal(path: &Path) -> (u64, u64, FileKind) {
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
        // ordinal counter keyed by identity-without-ordinal.
        let mut ordinals: HashMap<String, u32> = HashMap::new();
        let mut file_keys: HashMap<String, std::collections::BTreeSet<u64>> = HashMap::new();
        let mut file_abs: HashMap<String, PathBuf> = HashMap::new();

        for (chunk, vector) in chunks.iter().zip(vectors.iter()) {
            let file_rel = root_relative(root, &chunk.file_path);
            let base = format!(
                "{}::{}::{}",
                file_rel,
                chunk.class_name.as_deref().unwrap_or(""),
                chunk.function_name.as_deref().unwrap_or("")
            );
            let ordinal = ordinals.entry(base).or_insert(0);
            let identity = chunk_identity(
                &file_rel,
                chunk.class_name.as_deref(),
                chunk.function_name.as_deref(),
                *ordinal,
            );
            *ordinal += 1;
            let key = identity_key(&identity);

            store.add(
                key,
                vector,
                ChunkMeta {
                    identity,
                    file_rel_path: file_rel.clone(),
                    function_name: chunk.function_name.clone(),
                    class_name: chunk.class_name.clone(),
                    line_start: chunk.line_start,
                    line_end: chunk.line_end,
                    content_hash: chunk.content_hash.clone(),
                },
            )?;
            file_keys.entry(file_rel.clone()).or_default().insert(key);
            file_abs.entry(file_rel).or_insert_with(|| chunk.file_path.clone());
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// De-risks the whole l5d plan: usearch links, and add → exact_search →
    /// save → load → exact_search → remove behaves exactly as the design relies
    /// on (stable u64 keys, identical results after a save/load round-trip,
    /// and working removal — the incremental-delta prerequisite for t8f).
    #[test]
    fn usearch_f32_exact_search_save_load_remove_roundtrip() {
        const DIMS: usize = 8;
        let index = new_f32_index(DIMS, 32).expect("create index");

        // Deterministic, non-contiguous u64 keys (mirrors hashed chunk keys).
        let vecs: Vec<(u64, Vec<f32>)> = (0..10u64)
            .map(|i| {
                let mut v = vec![0.0f32; DIMS];
                v[i as usize % DIMS] = 1.0;
                v[(i as usize + 1) % DIMS] = 0.5;
                (i.wrapping_mul(1000).wrapping_add(7), v)
            })
            .collect();
        for (k, v) in &vecs {
            index.add(*k, v.as_slice()).expect("add");
        }
        assert_eq!(index.size(), vecs.len());

        // Nearest neighbour of an indexed vector is itself (cosine distance ~0).
        let (want_key, query) = &vecs[3];
        let m = index.exact_search(query.as_slice(), 5).expect("exact_search");
        assert_eq!(m.keys.len(), 5);
        assert_eq!(m.keys[0], *want_key, "nearest to a vector must be itself");

        // save -> load into a fresh index -> identical top hit.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.usearch");
        let path_str = path.to_str().unwrap();
        index.save(path_str).expect("save");
        assert!(path.exists(), "save must write the index file");

        let loaded = new_f32_index(DIMS, 32).expect("create for load");
        loaded.load(path_str).expect("load");
        assert_eq!(loaded.size(), vecs.len(), "size preserved across save/load");
        let m2 = loaded
            .exact_search(query.as_slice(), 5)
            .expect("exact_search after load");
        assert_eq!(m2.keys[0], *want_key, "results identical after save/load");

        // remove(key) works — the incremental-delta prerequisite (TLDR-t8f).
        assert!(loaded.contains(*want_key));
        loaded.remove(*want_key).expect("remove");
        assert!(!loaded.contains(*want_key));
        assert_eq!(loaded.size(), vecs.len() - 1, "removal shrinks the index");
    }

    // ---- VectorStore (step 2) -------------------------------------------------

    fn meta(id: &str) -> ChunkMeta {
        ChunkMeta {
            identity: id.to_string(),
            file_rel_path: format!("src/{id}.rs"),
            function_name: Some(id.to_string()),
            class_name: None,
            line_start: 1,
            line_end: 10,
            content_hash: format!("hash-{id}"),
        }
    }

    fn unit(dims: usize, i: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; dims];
        v[i % dims] = 1.0;
        v[(i + 1) % dims] = 0.5;
        v
    }

    #[test]
    fn vector_store_add_search_joins_metadata() {
        const D: usize = 8;
        let mut store = VectorStore::new(D, 4).unwrap();
        for i in 0..5u64 {
            store
                .add(i * 10 + 1, &unit(D, i as usize), meta(&format!("f{i}")))
                .unwrap();
        }
        assert_eq!(store.len(), 5);

        let hits = store.search(&unit(D, 2), 3).unwrap();
        assert_eq!(hits.len(), 3);
        // Nearest is the vector itself, with its sidecar metadata joined in.
        assert_eq!(hits[0].key, 21);
        assert_eq!(hits[0].meta.identity, "f2");
        assert_eq!(hits[0].meta.file_rel_path, "src/f2.rs");
        assert!(hits[0].distance <= hits[1].distance, "nearest first");
    }

    #[test]
    fn vector_store_remove_drops_index_and_sidecar() {
        const D: usize = 8;
        let mut store = VectorStore::new(D, 4).unwrap();
        store.add(7, &unit(D, 1), meta("a")).unwrap();
        store.add(9, &unit(D, 2), meta("b")).unwrap();

        assert!(store.contains(7));
        assert!(store.remove(7).unwrap());
        assert!(!store.contains(7));
        assert_eq!(store.len(), 1);
        let hits = store.search(&unit(D, 1), 5).unwrap();
        assert!(hits.iter().all(|h| h.key != 7), "removed key not returned");
        assert!(!store.remove(123).unwrap(), "removing an absent key is false");
    }

    #[test]
    fn vector_store_readd_updates_metadata_in_place() {
        const D: usize = 8;
        let mut store = VectorStore::new(D, 4).unwrap();
        store.add(42, &unit(D, 1), meta("old")).unwrap();
        store.add(42, &unit(D, 1), meta("new")).unwrap(); // same key, new meta
        assert_eq!(store.len(), 1, "re-add of the same key does not grow the store");
        let hits = store.search(&unit(D, 1), 1).unwrap();
        assert_eq!(hits[0].meta.identity, "new");
    }

    #[test]
    fn vector_store_grows_past_initial_capacity() {
        const D: usize = 4;
        let mut store = VectorStore::new(D, 2).unwrap(); // tiny request (floored to MIN)
        for i in 0..64u64 {
            store
                .add(i, &unit(D, i as usize), meta(&format!("f{i}")))
                .unwrap();
        }
        assert_eq!(store.len(), 64);
    }

    #[test]
    fn vector_store_rejects_dimension_mismatch() {
        let mut store = VectorStore::new(8, 4).unwrap();
        assert!(store.add(1, &[0.1, 0.2], meta("x")).is_err());
        assert!(store.search(&[0.1, 0.2], 3).is_err());
    }

    // ---- Persistence (step 3) -------------------------------------------------

    fn manifest_id(dims: usize) -> ManifestId {
        ManifestId {
            embedding_model: "ArcticL".into(),
            model_revision: "rev-1".into(),
            dimensions: dims as u32,
            metric: "cos".into(),
            scalar_kind: "f32".into(),
            search_mode: "exact".into(),
            embed_schema: "raw-v1".into(),
            chunk_params: "fn".into(),
            walker_version: "w1".into(),
            root: "/proj".into(),
        }
    }

    fn file_record(keys: &[u64]) -> FileRecord {
        FileRecord {
            keys: keys.iter().copied().collect(),
            mtime: 1234,
            size: 4096,
            file_type: FileKind::Regular,
        }
    }

    #[test]
    fn store_save_load_roundtrip_preserves_vectors_meta_and_files() {
        const D: usize = 8;
        let dir = tempfile::tempdir().unwrap();
        let id = manifest_id(D);

        let mut store = VectorStore::new(D, 4).unwrap();
        for i in 0..6u64 {
            store
                .add(i * 10 + 1, &unit(D, i as usize), meta(&format!("f{i}")))
                .unwrap();
        }
        store.set_file_record("src/f2.rs".into(), file_record(&[21]));
        store.save(dir.path(), &id).unwrap();

        let loaded = VectorStore::load(dir.path(), &id).unwrap();
        assert_eq!(loaded.len(), 6);
        let hits = loaded.search(&unit(D, 2), 1).unwrap();
        assert_eq!(hits[0].key, 21);
        assert_eq!(hits[0].meta.identity, "f2");
        let rec = loaded.file_record("src/f2.rs").expect("file record persisted");
        assert!(rec.keys.contains(&21));
        assert_eq!(rec.file_type, FileKind::Regular);
    }

    #[test]
    fn store_generations_increment_and_gc_retains_last_k() {
        const D: usize = 4;
        let dir = tempfile::tempdir().unwrap();
        let id = manifest_id(D);
        let mut store = VectorStore::new(D, 4).unwrap();
        store.add(1, &unit(D, 0), meta("a")).unwrap();
        for _ in 0..5 {
            store.save(dir.path(), &id).unwrap();
        }
        assert_eq!(read_current(dir.path()).unwrap().generation, 5);

        let manifest_exists = |g: u64| dir.path().join(format!("manifest.{g}")).exists();
        // KEEP_GENS = 3 → gens 1,2 collected; 3,4,5 retained.
        assert!(!manifest_exists(1) && !manifest_exists(2), "old gens gc'd");
        assert!(manifest_exists(3) && manifest_exists(4) && manifest_exists(5));
        assert_eq!(VectorStore::load(dir.path(), &id).unwrap().len(), 1);
    }

    #[test]
    fn store_load_rejects_config_mismatch() {
        const D: usize = 8;
        let dir = tempfile::tempdir().unwrap();
        let mut store = VectorStore::new(D, 4).unwrap();
        store.add(1, &unit(D, 0), meta("a")).unwrap();
        store.save(dir.path(), &manifest_id(D)).unwrap();

        let mut other = manifest_id(D);
        other.model_revision = "rev-2".into(); // tokenizer/weights changed
        assert!(
            VectorStore::load(dir.path(), &other).is_err(),
            "a config mismatch must reject -> caller rebuilds"
        );
    }

    #[test]
    fn store_load_rejects_torn_current() {
        const D: usize = 8;
        let dir = tempfile::tempdir().unwrap();
        let id = manifest_id(D);
        let mut store = VectorStore::new(D, 4).unwrap();
        store.add(1, &unit(D, 0), meta("a")).unwrap();
        store.save(dir.path(), &id).unwrap();
        // Bad magic / checksum → read_current rejects → load errors.
        std::fs::write(
            dir.path().join("CURRENT"),
            br#"{"magic":1,"generation":1,"checksum":0}"#,
        )
        .unwrap();
        assert!(VectorStore::load(dir.path(), &id).is_err());
    }

    #[test]
    fn store_load_rejects_tampered_sidecar() {
        const D: usize = 8;
        let dir = tempfile::tempdir().unwrap();
        let id = manifest_id(D);
        let mut store = VectorStore::new(D, 4).unwrap();
        store.add(1, &unit(D, 0), meta("a")).unwrap();
        store.save(dir.path(), &id).unwrap();

        let gen = read_current(dir.path()).unwrap().generation;
        let meta_path = dir.path().join(format!("meta.{gen}"));
        let mut bytes = std::fs::read(&meta_path).unwrap();
        bytes.push(b' '); // alter payload → sidecar checksum mismatch
        std::fs::write(&meta_path, &bytes).unwrap();
        assert!(VectorStore::load(dir.path(), &id).is_err());
    }

    // ---- Build path (step 4) --------------------------------------------------

    fn code_chunk(path: &str, class: Option<&str>, func: Option<&str>, content: &str) -> CodeChunk {
        CodeChunk {
            file_path: std::path::PathBuf::from(path),
            function_name: func.map(str::to_string),
            class_name: class.map(str::to_string),
            line_start: 1,
            line_end: 10,
            content: content.to_string(),
            content_hash: format!("{:x}", md5::compute(content)),
            language: crate::Language::Rust,
        }
    }

    #[test]
    fn identity_and_key_are_stable_and_ordinal_disambiguates() {
        let a = chunk_identity("src/a.rs", None, Some("foo"), 0);
        assert_eq!(a, "src/a.rs::::foo::0");
        assert_eq!(identity_key(&a), identity_key("src/a.rs::::foo::0"), "stable");
        // Duplicate (file, class, fn) name → different ordinal → distinct keys.
        let k0 = identity_key(&chunk_identity("src/a.rs", Some("S"), Some("new"), 0));
        let k1 = identity_key(&chunk_identity("src/a.rs", Some("S"), Some("new"), 1));
        assert_ne!(k0, k1);
        // File-level chunk identity.
        assert_eq!(chunk_identity("src/a.rs", None, None, 7), "src/a.rs#file");
    }

    #[test]
    fn from_embedded_populates_store_and_file_records() {
        const D: usize = 6;
        let root = std::path::Path::new("/proj");
        let chunks = vec![
            code_chunk("/proj/src/a.rs", None, Some("foo"), "fn foo(){}"),
            code_chunk("/proj/src/a.rs", Some("S"), Some("new"), "fn new()->S{}"),
            code_chunk("/proj/src/a.rs", Some("S"), Some("new"), "fn new(x)->S{}"), // dup name
            code_chunk("/proj/src/b.rs", None, Some("bar"), "fn bar(){}"),
        ];
        let vectors: Vec<Vec<f32>> = (0..chunks.len()).map(|i| unit(D, i)).collect();

        let store = VectorStore::from_embedded(&chunks, &vectors, root).unwrap();
        assert_eq!(store.len(), 4, "same-named fns get distinct keys via ordinal");

        // Root-relative path + correct metadata joined on search.
        let hits = store.search(&unit(D, 0), 1).unwrap();
        assert_eq!(hits[0].meta.file_rel_path, "src/a.rs");
        assert_eq!(hits[0].meta.function_name.as_deref(), Some("foo"));

        // Per-file records grouped by root-relative path.
        assert_eq!(store.file_record("src/a.rs").unwrap().keys.len(), 3);
        assert_eq!(store.file_record("src/b.rs").unwrap().keys.len(), 1);

        // Deterministic: a rebuild yields identical keys.
        let store2 = VectorStore::from_embedded(&chunks, &vectors, root).unwrap();
        let keys = |s: &VectorStore| -> Vec<u64> {
            let mut k: Vec<u64> = s.file_record("src/a.rs").unwrap().keys.iter().copied().collect();
            k.sort_unstable();
            k
        };
        assert_eq!(keys(&store), keys(&store2));
    }

    #[test]
    fn from_embedded_rejects_mismatched_lengths() {
        let root = std::path::Path::new("/proj");
        let chunks = vec![code_chunk("/proj/a.rs", None, Some("f"), "x")];
        assert!(VectorStore::from_embedded(&chunks, &[], root).is_err());
    }
}

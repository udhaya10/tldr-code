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

use serde::{Deserialize, Serialize};
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use crate::error::TldrError;
use crate::semantic::index::BuildOptions;
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkMeta {
    /// `file_rel_path::class::function::ordinal` — the source of the u64 key.
    pub identity: String,
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

    /// Whether the store holds no vectors.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The vector dimensionality (fixed per embedding model).
    pub fn dimensions(&self) -> usize {
        self.dimensions
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
const STORE_FORMAT_VERSION: u32 = 2;
/// `CURRENT` magic ("TLDR") so a torn/foreign pointer is detectable.
const CURRENT_MAGIC: u32 = 0x544C_4452;
/// Generations retained by GC (the active one + rollback headroom). Keeps a
/// concurrent reader's snapshot alive across a few saves (design doc §7.1).
const KEEP_GENS: u64 = 3;

/// What kind of filesystem object a tracked path was at index time — lets
/// reconcile (§7.3) detect file↔dir/type swaps, not just content changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Embed-input recipe tag (`raw-v1` / `enriched-v1`).
    pub embed_schema: String,
    /// Digest of ChunkOptions (granularity/max_tokens/overlap/lang filter).
    pub chunk_params: String,
    /// Digest of the source-selection / ignore rules.
    pub walker_version: String,
    /// Canonical project root the keys are relative to.
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

        sync_dir(dir)?;

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
        sync_dir(dir)?;

        // 5. GC — retain the last KEEP_GENS generations.
        gc_old_generations(dir, gen);
        Ok(())
    }

    /// Load the active generation from `dir`, verifying against the running config
    /// `expect`. Tries the `CURRENT` pointer first; if it is missing/torn, or its
    /// generation fails verification, FALLS BACK to scanning `manifest.<gen>`
    /// newest-to-oldest for the newest generation that verifies (Codex review).
    /// Errors (→ caller full-rebuilds) only if no retained generation verifies.
    pub fn load(dir: &Path, expect: &ManifestId) -> TldrResult<Self> {
        // Candidate generations, newest first: CURRENT's gen (if valid), then
        // every on-disk manifest.<gen> descending.
        let mut gens: Vec<u64> = Vec::new();
        if let Some(cur) = read_current(dir) {
            gens.push(cur.generation);
        }
        let mut scanned: Vec<u64> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                e.file_name()
                    .to_string_lossy()
                    .strip_prefix("manifest.")
                    .and_then(|r| r.parse::<u64>().ok())
            })
            .collect();
        scanned.sort_unstable_by(|a, b| b.cmp(a));
        for g in scanned {
            if !gens.contains(&g) {
                gens.push(g);
            }
        }
        if gens.is_empty() {
            return Err(vs_err("load", "no store generation found"));
        }

        let mut last_err = None;
        for gen in gens {
            match Self::load_generation(dir, gen, expect) {
                Ok(store) => return Ok(store),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| vs_err("load", "no verifying generation")))
    }

    /// Verify and load one specific generation: manifest config gates +
    /// sidecar/index/keys checksums + index size + every sidecar key present in
    /// the index. Errors on any mismatch (the caller tries an older generation).
    fn load_generation(dir: &Path, gen: u64, expect: &ManifestId) -> TldrResult<Self> {
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
            return Err(vs_err("load", "manifest generation != filename"));
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
        // `keys_checksum` only proves the sidecar matches the manifest; verify the
        // usearch index actually CONTAINS every sidecar key, so a vector key-set
        // that drifted from the sidecar is caught (Codex review — not circular).
        for &key in sidecar.meta.keys() {
            if !index.contains(key) {
                return Err(vs_err("load", "index is missing a sidecar key (drift)"));
            }
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

/// Stable FNV-1a 64-bit hash. Deterministic across processes, platforms, and
/// Rust versions — unlike `DefaultHasher` (SipHash), whose output is NOT a
/// guaranteed-stable on-disk primitive. Used for every persisted checksum AND
/// for the chunk identity key (`identity_key`), so the on-disk format and the
/// key scheme don't silently shift under a std change (Codex review).
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

/// Hash an identity string into the stable u64 usearch key (FNV-1a — stable
/// across processes/Rust versions, unlike `DefaultHasher`).
pub fn identity_key(identity: &str) -> u64 {
    stable_hash(identity.as_bytes())
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
fn root_relative(root: &Path, file_path: &Path) -> String {
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
        // key -> identity, to DETECT a 64-bit hash collision between two distinct
        // identities (astronomically rare, but a silent replace would lose a chunk
        // — Codex review: the stored identity must actually be checked).
        let mut seen: HashMap<u64, String> = HashMap::new();

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
            if let Some(prev) = seen.get(&key) {
                if prev != &identity {
                    return Err(vs_err(
                        "build",
                        format!("u64 key collision: '{prev}' and '{identity}' both hash to {key}"),
                    ));
                }
            }
            seen.insert(key, identity.clone());

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
        use crate::semantic::cache::EmbeddingCache;
        use crate::semantic::chunker::chunk_code;
        use crate::semantic::embedder::Embedder;
        use crate::semantic::enrichment::{build_embedding_text, enrich_chunks};
        use crate::semantic::index::{BYTES_PER_CHUNK, MAX_INDEX_SIZE, MAX_MEMORY_BYTES};
        use crate::semantic::types::ChunkOptions;

        let languages = options.languages.as_ref().map(|langs| {
            langs
                .iter()
                .filter_map(|s| crate::Language::from_extension(s))
                .collect()
        });
        let chunk_opts = ChunkOptions {
            granularity: options.granularity,
            languages,
            ..Default::default()
        };
        let chunks = chunk_code(root, &chunk_opts)?.chunks;

        // P0 guards — shared with SemanticIndex::build (same limits, not copies).
        if chunks.len() > MAX_INDEX_SIZE {
            return Err(TldrError::IndexTooLarge {
                count: chunks.len(),
                max: MAX_INDEX_SIZE,
            });
        }
        let estimated_memory = chunks.len() * BYTES_PER_CHUNK;
        if estimated_memory > MAX_MEMORY_BYTES {
            return Err(TldrError::MemoryLimitExceeded {
                estimated_mb: estimated_memory / (1024 * 1024),
                max_mb: MAX_MEMORY_BYTES / (1024 * 1024),
            });
        }

        let mut cache = if options.use_cache {
            cache_config.map(EmbeddingCache::open).transpose()?
        } else {
            None
        };
        // Match the index/CLI cache-key normalization (root-relative keys).
        if let Some(c) = cache.as_mut() {
            c.set_key_root(root);
        }

        // Phase 1: content-addressed cache hits vs. misses.
        let mut vectors: Vec<Vec<f32>> = vec![Vec::new(); chunks.len()];
        let mut uncached: Vec<usize> = Vec::new();
        for (i, chunk) in chunks.iter().enumerate() {
            match cache.as_mut().and_then(|c| c.get(chunk, options.model)) {
                Some(v) => vectors[i] = v,
                None => uncached.push(i),
            }
        }

        // Phase 2: embed the misses. Honor TLDR_ENRICH exactly like
        // SemanticIndex::build, so the store embeds the SAME text the index does
        // (else the vectors — and the cache keys' embed_schema tag — diverge).
        if !uncached.is_empty() {
            let mut embedder = Embedder::new(options.model)?;
            let enrich = std::env::var("TLDR_ENRICH")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            let enriched_texts: Vec<String> = if enrich {
                let units = enrich_chunks(&chunks, root);
                uncached.iter().map(|&i| build_embedding_text(&units[i])).collect()
            } else {
                Vec::new()
            };
            let texts: Vec<&str> = if enrich {
                enriched_texts.iter().map(|s| s.as_str()).collect()
            } else {
                uncached.iter().map(|&i| chunks[i].content.as_str()).collect()
            };
            let embeddings = embedder.embed_batch(texts, options.show_progress)?;
            for (&i, embedding) in uncached.iter().zip(embeddings) {
                if let Some(c) = cache.as_mut() {
                    c.put(&chunks[i], embedding.clone(), options.model);
                }
                vectors[i] = embedding;
            }
        }
        if let Some(c) = cache.as_mut() {
            c.flush()?;
        }

        Self::from_embedded(&chunks, &vectors, root)
    }
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
        "enriched-v1".to_string()
    } else {
        "raw-v1".to_string()
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
    fn store_load_recovers_from_torn_current() {
        const D: usize = 8;
        let dir = tempfile::tempdir().unwrap();
        let id = manifest_id(D);
        let mut store = VectorStore::new(D, 4).unwrap();
        store.add(1, &unit(D, 0), meta("a")).unwrap();
        store.save(dir.path(), &id).unwrap();
        // Bad magic/checksum → read_current rejects CURRENT, but load() FALLS BACK
        // to scanning manifest.<gen> and recovers the valid generation.
        std::fs::write(
            dir.path().join("CURRENT"),
            br#"{"magic":1,"generation":1,"checksum":0}"#,
        )
        .unwrap();
        let loaded = VectorStore::load(dir.path(), &id).expect("recover via manifest scan");
        assert_eq!(loaded.len(), 1);
    }

    #[test]
    fn store_load_errors_when_no_generation_verifies() {
        const D: usize = 8;
        let dir = tempfile::tempdir().unwrap();
        let id = manifest_id(D);
        let mut store = VectorStore::new(D, 4).unwrap();
        store.add(1, &unit(D, 0), meta("a")).unwrap();
        store.save(dir.path(), &id).unwrap();
        // Corrupt every manifest.<gen> AND CURRENT → nothing verifies → error.
        for e in std::fs::read_dir(dir.path()).unwrap().flatten() {
            let name = e.file_name();
            if name.to_string_lossy().starts_with("manifest.")
                || name.to_string_lossy() == "CURRENT"
            {
                std::fs::write(e.path(), b"garbage").unwrap();
            }
        }
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

    #[test]
    fn root_relative_strips_lexically_and_is_deterministic_on_miss() {
        // Common case: lexical strip.
        assert_eq!(
            root_relative(std::path::Path::new("/proj"), std::path::Path::new("/proj/src/a.rs")),
            "src/a.rs"
        );
        // Not under root and not canonicalizable → raw path, but DETERMINISTIC and
        // warned (TLDR-ss3) — never a silent abs-vs-rel divergence.
        assert_eq!(
            root_relative(std::path::Path::new("/proj"), std::path::Path::new("/elsewhere/x.rs")),
            "/elsewhere/x.rs"
        );
    }

    // ---- Integration / equivalence (step 5) -----------------------------------

    /// The acceptance core: usearch `exact_search` must rank identically to the
    /// existing brute-force cosine `top_k_similar` over the same vectors. Proven
    /// embedder-free with vectors of strictly-decreasing cosine to the query, so
    /// the order is unambiguous (no tie flakiness).
    #[test]
    fn search_ranking_matches_brute_force_cosine() {
        use crate::semantic::similarity::top_k_similar;
        const D: usize = 16;
        let mut store = VectorStore::new(D, 16).unwrap();
        let vecs: Vec<Vec<f32>> = (0..8u64)
            .map(|i| {
                let mut v = vec![0.0f32; D];
                v[0] = 1.0; // shared direction
                v[1 + i as usize] = i as f32 * 0.3; // distinct orthogonal -> distinct cos
                v
            })
            .collect();
        for (i, v) in vecs.iter().enumerate() {
            store.add(i as u64, v, meta(&format!("c{i}"))).unwrap();
        }
        let mut query = vec![0.0f32; D];
        query[0] = 1.0;

        let candidates: Vec<(usize, &[f32])> =
            vecs.iter().enumerate().map(|(i, v)| (i, v.as_slice())).collect();
        let brute: Vec<u64> = top_k_similar(&query, &candidates, 5, 0.0)
            .iter()
            .map(|(i, _)| *i as u64)
            .collect();
        let usearch: Vec<u64> = store
            .search(&query, 5)
            .unwrap()
            .iter()
            .map(|h| h.key)
            .collect();

        assert_eq!(usearch, brute, "exact_search ranking == brute-force cosine");
        assert_eq!(usearch, vec![0, 1, 2, 3, 4], "deterministic decreasing-cos order");
    }

    #[test]
    fn manifest_id_for_build_is_complete_and_deterministic() {
        let p = std::path::Path::new("/proj");
        let id = ManifestId::for_build(EmbeddingModel::ArcticL, p, "fn", "v1");
        assert_eq!(id.dimensions, 1024);
        assert_eq!(id.metric, "cos");
        assert_eq!(id.scalar_kind, "f32");
        assert_eq!(id.search_mode, "exact");
        assert_eq!(id.root, "/proj");
        assert!(id.model_revision.contains("arctic"));
        assert_eq!(id, ManifestId::for_build(EmbeddingModel::ArcticL, p, "fn", "v1"));
    }

    /// End-to-end through the real ONNX embedder: build → search → manifest →
    /// save → load. Ignored by default (loads/downloads the model, slow); run
    /// with `cargo test -- --ignored build_end_to_end_small_corpus`.
    #[test]
    #[ignore = "loads the ONNX embedder; run on demand"]
    fn build_end_to_end_small_corpus() {
        use crate::semantic::embedder::Embedder;
        use crate::semantic::types::ChunkGranularity;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "/// cosine similarity\nfn cosine_similarity(a: &[f32], b: &[f32]) -> f32 { 0.0 }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("b.rs"),
            "/// parse configuration\nfn parse_config(p: &str) {}\n",
        )
        .unwrap();

        let model = EmbeddingModel::ArcticXS;
        let opts = BuildOptions {
            model,
            granularity: ChunkGranularity::Function,
            languages: None,
            show_progress: false,
            use_cache: true,
        };
        let cache = CacheConfig {
            cache_dir: dir.path().join("cache"),
            max_size_mb: 50,
            ttl_days: 1,
        };

        let store = VectorStore::build(dir.path(), &opts, Some(cache)).unwrap();
        assert!(store.len() >= 2);

        let mut emb = Embedder::new(model).unwrap();
        let q = emb
            .embed_query("compute cosine similarity between vectors")
            .unwrap();
        let hits = store.search(&q, 1).unwrap();
        assert_eq!(hits[0].meta.file_rel_path, "a.rs", "right function ranks top");

        let id = ManifestId::for_build(model, dir.path(), "fn", "v1");
        let store_dir = dir.path().join("store");
        store.save(&store_dir, &id).unwrap();
        let loaded = VectorStore::load(&store_dir, &id).unwrap();
        assert_eq!(loaded.len(), store.len());
    }
}

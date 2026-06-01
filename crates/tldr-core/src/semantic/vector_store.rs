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

use serde::{Deserialize, Serialize};
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use crate::error::TldrError;
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
        })
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
}

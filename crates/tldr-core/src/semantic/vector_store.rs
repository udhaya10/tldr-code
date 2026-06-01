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
//
// Scaffolding: exercised by the smoke test now and by the store type next.
#[allow(dead_code)]
pub(crate) fn new_f32_index(dimensions: usize, capacity: usize) -> TldrResult<Index> {
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
}

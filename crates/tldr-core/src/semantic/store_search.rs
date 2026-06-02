//! Store-backed semantic search (TLDR-m01): the production bridge from a query
//! to the usearch [`VectorStore`], with a transparent fall back to the in-memory
//! [`SemanticIndex`].
//!
//! This is the first production caller of [`VectorStore::load`]. The store proved
//! results-equivalent to the dense `SemanticIndex` path (TLDR-l5d, 52/52), so this
//! helper returns the SAME [`SemanticSearchReport`] shape the index does and can be
//! dropped into the CLI / daemon / MCP search paths (wired in a follow-up PR).
//!
//! Control flow (Codex + advisor reviewed):
//! - `load()` fails (no/torn/incompatible generation) → REBUILD via
//!   [`VectorStore::build`] then best-effort `save()`. A rebuild is the designed
//!   response to any [`VectorStore::load`] failure (it already falls back across
//!   retained generations internally and only errors when none verify).
//! - `build()` or `search()` errors → fall back to [`SemanticIndex`] (the store is
//!   unusable for some environmental reason; never surface the error to the user).
//! - `save()` errors → warn and keep going with the in-RAM store; persistence is an
//!   optimization for the NEXT query, not required for THIS one.
//!
//! `store_dir` is an explicit input: the global-vs-`.tldr/` location decision (and
//! making the daemon writer + cold CLI reader resolve a byte-identical path) belongs
//! at the call sites, not here — which also keeps this unit tempdir-testable.

use std::path::Path;
use std::time::Instant;

use crate::semantic::index::{make_snippet, BuildOptions, SearchOptions, SemanticIndex};
use crate::semantic::types::{
    CacheConfig, EmbeddingModel, SemanticSearchReport, SemanticSearchResult,
};
use crate::semantic::vector_store::{ChunkMeta, ManifestId, SearchHit, VectorStore};
use crate::TldrResult;

/// Version of the chunker/walker pipeline that produces embedded chunks. BUMP
/// this whenever chunk boundaries change (so an on-disk store built by an older
/// pipeline is rejected as `Incompatible` and rebuilt). Paired with
/// [`chunk_params_tag`] in the [`ManifestId`] (TLDR-7al).
pub(crate) const CHUNK_WALKER_VERSION: &str = "w1";

/// Encode the chunk-boundary-affecting build inputs into a stable tag for the
/// manifest. ONLY inputs that change which chunks/vectors exist belong here
/// (granularity, languages) — NOT `show_progress` / `use_cache`, which are
/// runtime concerns. Languages are sorted so the tag is order-independent.
pub(crate) fn chunk_params_tag(options: &BuildOptions) -> String {
    let langs = match &options.languages {
        Some(l) => {
            let mut v = l.clone();
            v.sort();
            v.join(",")
        }
        None => "auto".to_string(),
    };
    format!("gran={:?};langs={}", options.granularity, langs)
}

/// The manifest identity for a store built from `root` with `options` — the real
/// config inputs (resolves the TLDR-7al placeholders): model + chunk params +
/// walker version. `load()` rejects a store whose identity differs, forcing a
/// rebuild on any model/recipe/chunking change.
pub(crate) fn manifest_id_for(root: &Path, options: &BuildOptions) -> ManifestId {
    ManifestId::for_build(
        options.model,
        root,
        &chunk_params_tag(options),
        CHUNK_WALKER_VERSION,
    )
}

/// Run a semantic query through the usearch store, building+persisting it on a
/// miss and falling back to the in-memory [`SemanticIndex`] on any store error.
///
/// Returns the same [`SemanticSearchReport`] as [`SemanticIndex::search`], so it
/// is a drop-in for the production search paths.
pub fn search_with_store(
    root: &Path,
    store_dir: &Path,
    query: &str,
    search_options: &SearchOptions,
    build_options: &BuildOptions,
    cache_config: Option<CacheConfig>,
) -> TldrResult<SemanticSearchReport> {
    match try_store_search(
        root,
        store_dir,
        query,
        search_options,
        build_options,
        cache_config.clone(),
    ) {
        Ok(report) => Ok(report),
        Err(e) => {
            eprintln!(
                "[tldr-warn] store search path failed ({e}); falling back to in-memory index"
            );
            let mut index = SemanticIndex::build(root, build_options.clone(), cache_config)?;
            index.search(query, search_options)
        }
    }
}

/// The store-only path. Errors here (build/search) trigger the [`SemanticIndex`]
/// fallback in [`search_with_store`]; a `load` failure rebuilds in place and a
/// `save` failure is swallowed with a warning.
fn try_store_search(
    root: &Path,
    store_dir: &Path,
    query: &str,
    search_options: &SearchOptions,
    build_options: &BuildOptions,
    cache_config: Option<CacheConfig>,
) -> TldrResult<SemanticSearchReport> {
    use crate::semantic::embedder::Embedder;

    let start = Instant::now();
    let id = manifest_id_for(root, build_options);

    // load() → on ANY failure, REBUILD. load() already scans retained generations
    // and only errors when none verify (missing / torn / config-incompatible), all
    // of which mean "the on-disk store is unusable as-is → rebuild".
    let store = match VectorStore::load(store_dir, &id) {
        Ok(s) => s,
        Err(_) => {
            // A build()/save() failure is environmental; let build() errors
            // propagate to the SemanticIndex fallback, but treat a save() failure
            // as non-fatal (we still have a usable in-RAM store for THIS query).
            let s = VectorStore::build(root, build_options, cache_config)?;
            if let Err(e) = s.save(store_dir, &id) {
                eprintln!(
                    "[tldr-warn] store save failed ({e}); serving from the in-RAM store \
                     (the next query rebuilds)"
                );
            }
            s
        }
    };

    // Same query embedding as SemanticIndex: embed_query applies the Arctic
    // asymmetric query prefix (documents were indexed WITHOUT a prefix).
    let mut embedder = Embedder::new(build_options.model)?;
    let qv = embedder.embed_query(query)?;

    let total_chunks = store.len();
    let hits = store.search(&qv, search_options.top_k)?;

    Ok(hits_to_report(
        query,
        build_options.model,
        hits,
        root,
        search_options,
        total_chunks,
        start.elapsed().as_millis() as u64,
    ))
}

/// Convert raw store [`SearchHit`]s into a [`SemanticSearchReport`] with the SAME
/// shape `SemanticIndex::search` produces. Pure apart from the lazy snippet read,
/// so the parity-critical steps are unit-testable without an embedder:
///
/// - cosine DISTANCE → similarity SCORE via `1 - distance`;
/// - apply the `threshold` the store does NOT enforce (filter AFTER conversion —
///   correct because the hits are already globally score-ordered, so the top-k
///   intersect {score ≥ T} equals `SemanticIndex`'s filter-then-take-k).
fn hits_to_report(
    query: &str,
    model: EmbeddingModel,
    hits: Vec<SearchHit>,
    root: &Path,
    search_options: &SearchOptions,
    total_chunks: usize,
    latency_ms: u64,
) -> SemanticSearchReport {
    let results: Vec<SemanticSearchResult> = hits
        .into_iter()
        .map(|h| {
            let score = 1.0 - h.distance as f64;
            let snippet = if search_options.include_snippet {
                read_snippet(root, &h.meta, search_options.snippet_lines)
            } else {
                String::new()
            };
            SemanticSearchResult {
                file_path: root.join(&h.meta.file_rel_path),
                function_name: h.meta.function_name,
                class_name: h.meta.class_name,
                score,
                line_start: h.meta.line_start,
                line_end: h.meta.line_end,
                snippet,
            }
        })
        .filter(|r| r.score >= search_options.threshold)
        .collect();

    let n = results.len();
    SemanticSearchReport {
        results,
        total_results: n,
        query: query.to_string(),
        model,
        total_chunks,
        matches_above_threshold: n,
        latency_ms,
        cache_hit: false, // query embeddings are not cached (matches SemanticIndex)
    }
}

/// Lazily read the chunk's source lines for a display snippet. The store keeps
/// only `(file_rel_path, line_start, line_end)`, not the body. Degrades to an
/// empty snippet on any failure (file gone, moved, or line range out of bounds);
/// content-hash validation of the read is deferred to a follow-up.
fn read_snippet(root: &Path, meta: &ChunkMeta, max_lines: usize) -> String {
    let path = root.join(&meta.file_rel_path);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return String::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    // line_start/line_end are 1-indexed inclusive.
    let start = (meta.line_start as usize).saturating_sub(1);
    let end = (meta.line_end as usize).min(lines.len());
    if start >= end {
        return String::new();
    }
    make_snippet(&lines[start..end].join("\n"), max_lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::types::ChunkGranularity;

    fn opts(threshold: f64, include_snippet: bool) -> SearchOptions {
        SearchOptions {
            top_k: 10,
            threshold,
            include_snippet,
            snippet_lines: 5,
        }
    }

    fn hit(distance: f32, file: &str, line_start: u32, line_end: u32) -> SearchHit {
        SearchHit {
            key: line_start as u64,
            distance,
            meta: ChunkMeta {
                identity: format!("{file}::{line_start}"),
                file_rel_path: file.to_string(),
                function_name: Some("f".to_string()),
                class_name: None,
                line_start,
                line_end,
                content_hash: "h".to_string(),
            },
        }
    }

    fn build_opts(model: EmbeddingModel, gran: ChunkGranularity, langs: Option<&[&str]>) -> BuildOptions {
        BuildOptions {
            model,
            granularity: gran,
            languages: langs.map(|l| l.iter().map(|s| s.to_string()).collect()),
            show_progress: false,
            use_cache: true,
        }
    }

    #[test]
    fn score_is_one_minus_distance_and_threshold_filters() {
        // distances 0.1/0.4/0.7 -> scores 0.9/0.6/0.3. threshold 0.5 keeps the first
        // two; the store applies NO threshold itself so this filter is the only gate.
        let hits = vec![
            hit(0.1, "a.rs", 1, 2),
            hit(0.4, "b.rs", 3, 4),
            hit(0.7, "c.rs", 5, 6),
        ];
        let root = Path::new("/proj");
        let report = hits_to_report("q", EmbeddingModel::ArcticM, hits, root, &opts(0.5, false), 3, 0);

        assert_eq!(report.results.len(), 2, "0.3-score hit is below threshold");
        assert_eq!(report.total_results, 2);
        assert_eq!(report.matches_above_threshold, 2);
        assert_eq!(report.total_chunks, 3);
        assert!((report.results[0].score - 0.9).abs() < 1e-6);
        assert!((report.results[1].score - 0.6).abs() < 1e-6);
        // file_path is reconstructed root-relative -> absolute.
        assert_eq!(report.results[0].file_path, root.join("a.rs"));
    }

    #[test]
    fn threshold_zero_keeps_all_hits() {
        let hits = vec![hit(0.1, "a.rs", 1, 2), hit(0.9, "b.rs", 3, 4)];
        let report =
            hits_to_report("q", EmbeddingModel::ArcticM, hits, Path::new("/p"), &opts(0.0, false), 2, 0);
        assert_eq!(report.results.len(), 2);
    }

    #[test]
    fn include_snippet_false_yields_empty_snippet() {
        let hits = vec![hit(0.1, "a.rs", 1, 2)];
        let report =
            hits_to_report("q", EmbeddingModel::ArcticM, hits, Path::new("/p"), &opts(0.0, false), 1, 0);
        assert!(report.results[0].snippet.is_empty());
    }

    #[test]
    fn chunk_params_tag_is_order_independent_and_ignores_runtime_flags() {
        // Languages in different order -> identical tag (sorted).
        let a = build_opts(EmbeddingModel::ArcticM, ChunkGranularity::Function, Some(&["rust", "go"]));
        let b = build_opts(EmbeddingModel::ArcticM, ChunkGranularity::Function, Some(&["go", "rust"]));
        assert_eq!(chunk_params_tag(&a), chunk_params_tag(&b));

        // show_progress / use_cache do NOT affect the tag (runtime concerns).
        let mut c = a.clone();
        c.show_progress = true;
        c.use_cache = false;
        assert_eq!(chunk_params_tag(&a), chunk_params_tag(&c));

        // Granularity DOES affect the tag.
        let d = build_opts(EmbeddingModel::ArcticM, ChunkGranularity::File, Some(&["rust", "go"]));
        assert_ne!(chunk_params_tag(&a), chunk_params_tag(&d));
    }

    #[test]
    fn manifest_id_reflects_model_and_chunk_inputs() {
        let root = Path::new("/proj");
        let base = build_opts(EmbeddingModel::ArcticM, ChunkGranularity::Function, Some(&["rust"]));
        let id = manifest_id_for(root, &base);

        // A model change MUST change identity (else load() serves stale vectors).
        let other_model = build_opts(EmbeddingModel::ArcticL, ChunkGranularity::Function, Some(&["rust"]));
        assert_ne!(id, manifest_id_for(root, &other_model));

        // A granularity change MUST change identity.
        let other_gran = build_opts(EmbeddingModel::ArcticM, ChunkGranularity::File, Some(&["rust"]));
        assert_ne!(id, manifest_id_for(root, &other_gran));

        // A language-set change MUST change identity.
        let other_langs = build_opts(EmbeddingModel::ArcticM, ChunkGranularity::Function, Some(&["rust", "go"]));
        assert_ne!(id, manifest_id_for(root, &other_langs));

        // walker_version is wired through.
        assert_eq!(id.walker_version, CHUNK_WALKER_VERSION);
    }

    #[test]
    fn read_snippet_reads_line_range_and_degrades_gracefully() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.rs"), "L1\nL2\nL3\nL4\nL5\n").unwrap();
        let meta = hit(0.0, "f.rs", 2, 4).meta; // lines 2..=4 -> L2,L3,L4

        let snip = read_snippet(dir.path(), &meta, 5);
        assert_eq!(snip, "L2\nL3\nL4");

        // snippet_lines cap applies.
        let capped = read_snippet(dir.path(), &meta, 2);
        assert_eq!(capped, "L2\nL3");

        // Missing file degrades to empty, never errors.
        let missing = hit(0.0, "nope.rs", 1, 3).meta;
        assert!(read_snippet(dir.path(), &missing, 5).is_empty());

        // Out-of-range lines degrade to empty.
        let oob = hit(0.0, "f.rs", 99, 100).meta;
        assert!(read_snippet(dir.path(), &oob, 5).is_empty());
    }

    // End-to-end with the real embedder: rebuild-on-miss -> persist -> load-hit, and
    // parity against SemanticIndex. Ignored by default (loads the ONNX model).
    #[test]
    #[ignore = "loads the ONNX embedder; run on demand"]
    fn search_with_store_rebuilds_persists_and_matches_index() {
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
        let bopts = build_opts(model, ChunkGranularity::Function, None);
        let sopts = opts(0.0, true); // threshold 0 so parity isn't masked by filtering
        let store_dir = dir.path().join("store");
        let cache = || {
            Some(CacheConfig {
                cache_dir: dir.path().join("cache"),
                max_size_mb: 50,
                ttl_days: 1,
            })
        };
        let query = "compute cosine similarity between vectors";

        // First call: store_dir is empty -> load miss -> rebuild -> save -> search.
        let r1 = search_with_store(dir.path(), &store_dir, query, &sopts, &bopts, cache()).unwrap();
        assert!(!r1.results.is_empty());
        assert!(store_dir.join("CURRENT").exists(), "store was persisted");
        assert_eq!(
            r1.results[0].file_path,
            dir.path().join("a.rs"),
            "the cosine fn ranks top"
        );

        // Second call: store_dir now exists -> load hit (no rebuild) -> same ranking.
        let r2 = search_with_store(dir.path(), &store_dir, query, &sopts, &bopts, cache()).unwrap();
        let order = |r: &SemanticSearchReport| {
            r.results.iter().map(|x| x.file_path.clone()).collect::<Vec<_>>()
        };
        assert_eq!(order(&r1), order(&r2), "load-hit path matches rebuild path");

        // Parity: the store path's ranking equals the in-memory SemanticIndex path.
        let mut index = SemanticIndex::build(dir.path(), bopts.clone(), cache()).unwrap();
        let ir = index.search(query, &sopts).unwrap();
        assert_eq!(order(&r1), order(&ir), "store path == SemanticIndex ranking");
    }
}

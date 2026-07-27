//! Store-backed semantic search (TLDR-m01 / TLDR-zxb): the production search
//! path using the usearch [`VectorStore`].
//!
//! ## No fallback (TLDR-lx7)
//!
//! This is the ONLY search path. There is no silent degradation to the legacy
//! in-memory `SemanticIndex` or JSON cache. If the store cannot load, build, or
//! search, the error propagates with a detailed message so the user can fix it.
//! A tool should run at 100% performance or tell you why it can't.
//!
//! ## Two entry points
//!
//! - [`search_with_store`] — cold CLI one-shot: loads or builds the store,
//!   checks freshness, embeds the query, searches. One call does everything.
//! - [`query_store`] — daemon reuse: takes an already-loaded [`VectorStore`]
//!   reference, embeds the query, searches. No load/build/freshness overhead
//!   per query — the daemon manages the resident store and freshness separately.
//!
//! ## Freshness gate (TLDR-kkt)
//!
//! [`VectorStore::load`] only verifies persisted manifest/index integrity, not
//! whether the SOURCE changed since the store was built — so the cold path adds
//! a coarse "detect drift → full rebuild" gate. After a clean load it compares
//! the store's build-time corpus digest against [`compute_corpus_digest`] over
//! the current `root`; on any difference (a file added/removed, or any file's
//! mtime/size changed) it REBUILDS instead of serving stale rankings. The digest
//! is a stat-only walk (no parse), computed over the pre-parse candidate set, so
//! a supported file that yields zero chunks counts identically at build and check.
//!
//! Residual (design §7.3): an edit with the SAME mtime-second AND SAME size is
//! not detected; self-heals on the next real edit, escape hatch = manual rebuild.
//!
//! ## Control flow
//!
//! - `load()` fails (no/torn/incompatible generation) → REBUILD via
//!   [`VectorStore::build`] then `save()`.
//! - `build()` or `save()` errors → propagate with detailed message.
//! - query-`embed` or `search()` errors → propagate with detailed message.
//! - An empty/whitespace query → empty report (the store would otherwise score
//!   every chunk 1.0 off the zero query vector).
//!
//! `store_dir` is an explicit input: the global-vs-`.tldr/` location decision
//! (and making the daemon writer + cold CLI reader resolve a byte-identical path)
//! belongs at the call sites, not here — keeps this unit tempdir-testable.
//!
//! ## Store-location precondition
//!
//! `store_dir` and the embedding `cache_dir` MUST live OUTSIDE the indexed corpus
//! (`root`). The freshness gate walks `root`; if the store's own writes land
//! inside `root` they register as "source drift" and force a rebuild on EVERY
//! query. The global cache dir (`~/.cache/tldr/…`) is outside any project, and
//! an in-tree `.tldr/` store is skipped by `ProjectWalker`.

use std::path::Path;
use std::time::Instant;

use crate::semantic::embedder::Embedder;
use crate::semantic::index::{make_snippet, BuildOptions, SearchOptions};
use crate::semantic::types::{
    CacheConfig, EmbeddingModel, SemanticSearchReport, SemanticSearchResult,
};
use crate::semantic::vector_store::{
    compute_corpus_digest, ChunkMeta, ManifestId, SearchHit, VectorStore,
};
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
pub fn manifest_id_for(root: &Path, options: &BuildOptions) -> ManifestId {
    ManifestId::for_build(
        options.model,
        root,
        &chunk_params_tag(options),
        CHUNK_WALKER_VERSION,
    )
}

/// Run a semantic query through the usearch store, building+persisting it on a
/// miss. Returns a [`SemanticSearchReport`].
///
/// This is the cold CLI entry point — loads or builds the store, checks
/// freshness, embeds the query, searches. For the daemon (resident store),
/// use [`query_store`] instead.
///
/// There is NO fallback (TLDR-lx7). If the store cannot load, build, save,
/// or search, the error propagates so the user can diagnose and fix it.
pub fn search_with_store(
    root: &Path,
    store_dir: &Path,
    query: &str,
    search_options: &SearchOptions,
    build_options: &BuildOptions,
    cache_config: Option<CacheConfig>,
) -> TldrResult<SemanticSearchReport> {
    if query.trim().is_empty() {
        return Ok(empty_search_report(query, build_options.model));
    }

    let start = Instant::now();
    let store = load_or_build_store(root, store_dir, build_options, cache_config)?;
    query_store(
        &store,
        root,
        query,
        search_options,
        build_options.model,
        start,
    )
}

/// Load an existing store (if fresh) or build+save a new one.
///
/// Exported for callers that need the store itself (e.g. the daemon warm
/// command pre-builds the store without issuing a query).
pub fn load_or_build_store(
    root: &Path,
    store_dir: &Path,
    build_options: &BuildOptions,
    cache_config: Option<CacheConfig>,
) -> TldrResult<VectorStore> {
    let id = manifest_id_for(root, build_options);
    let current_digest = compute_corpus_digest(root);
    let generations = super::GenerationManager::open(store_dir)?;

    match generations.load(&id) {
        // A fresh persisted store can be reused — UNLESS the caller asked for
        // build instrumentation (TLDR-9bxa.1): a loaded store carries no
        // metrics, so `collect_metrics` must force a rebuild. `collect_metrics`
        // is off for every default caller (daemon, search), so this only
        // affects `tldr embed --metrics`.
        Ok(s) if s.corpus_digest() == current_digest && !build_options.collect_metrics => Ok(s),
        Ok(s) => {
            // A store loaded but not reused: either stale, or fresh-but-metrics.
            // Report the ACTUAL reason(s) — staleness and metrics are
            // independent and can both apply (TLDR-9bxa.1 review).
            let fresh = s.corpus_digest() == current_digest;
            if !fresh {
                eprintln!("[tldr-info] semantic store is stale (source changed); rebuilding");
            }
            if build_options.collect_metrics {
                eprintln!(
                    "[tldr-info] rebuilding{} to collect build metrics (--metrics)",
                    if fresh { "" } else { " (also)" }
                );
            }
            let built = VectorStore::build(root, build_options, cache_config)?;
            generations.publish(&built, &id)?;
            Ok(built)
        }
        Err(_) => {
            // No usable persisted store: initial build (not "stale").
            let built = VectorStore::build(root, build_options, cache_config)?;
            generations.publish(&built, &id)?;
            Ok(built)
        }
    }
}

/// Load a fresh vector generation or rebuild it exclusively from the complete
/// source chunks exported by the matching shared artifact generation.
pub fn load_or_build_store_from_artifacts(
    root: &Path,
    store_dir: &Path,
    build_options: &BuildOptions,
    cache_config: Option<CacheConfig>,
    source_chunks: Vec<crate::semantic::CodeChunk>,
) -> TldrResult<VectorStore> {
    let id = manifest_id_for(root, build_options);
    let current_digest = compute_corpus_digest(root);
    let generations = super::GenerationManager::open(store_dir)?;
    if let Ok(store) = generations.load(&id) {
        if store.corpus_digest() == current_digest && !build_options.collect_metrics {
            return Ok(store);
        }
    }
    let built =
        VectorStore::build_from_artifacts(root, build_options, cache_config, source_chunks)?;
    generations.publish(&built, &id)?;
    Ok(built)
}

/// Search an already-loaded store — the daemon reuse entry point.
///
/// Takes a [`VectorStore`] reference (the daemon holds this resident in its
/// state), embeds the query, and searches. No load/build/freshness overhead —
/// the caller is responsible for store lifecycle and freshness checks.
///
/// `start` is the caller's timing anchor (pass `Instant::now()` if you don't
/// care about including load time in the latency).
pub fn query_store(
    store: &VectorStore,
    root: &Path,
    query: &str,
    search_options: &SearchOptions,
    model: EmbeddingModel,
    start: Instant,
) -> TldrResult<SemanticSearchReport> {
    // Cold-CLI path: no resident embedder, so construct a one-shot one and embed
    // here. The daemon (resident embedder — TLDR-ac0.5) embeds the query against
    // its warm `IndexManager` embedder and calls `query_store_with_vector` instead,
    // skipping this per-query `Embedder::new` (ONNX reload).
    if query.trim().is_empty() {
        return Ok(empty_search_report(query, model));
    }
    let mut embedder = Embedder::new(model)?;
    let qv = embedder.embed_query(query)?;
    query_store_with_vector(store, root, query, &qv, search_options, model, start)
}

/// Search an already-loaded store with a PRE-COMPUTED query vector — the daemon's
/// resident-embedder path (TLDR-ac0.5).
///
/// Identical to [`query_store`] except the caller supplies `query_vector` (already
/// run through [`Embedder::embed_query`], i.e. WITH the model's asymmetric query
/// prefix), so the daemon reuses one warm embedder across queries instead of
/// reloading ONNX per search. `query` is still passed for the empty-query guard
/// and to echo back in the report.
pub fn query_store_with_vector(
    store: &VectorStore,
    root: &Path,
    query: &str,
    query_vector: &[f32],
    search_options: &SearchOptions,
    model: EmbeddingModel,
    start: Instant,
) -> TldrResult<SemanticSearchReport> {
    // Guard lives HERE (not only in the wrapper) because the daemon calls this
    // directly: an empty query would otherwise score every chunk 1.0 off the zero
    // vector (the failure pinned by `empty_query_returns_empty_report_*`).
    if query.trim().is_empty() {
        return Ok(empty_search_report(query, model));
    }

    let total_chunks = store.len();
    let hits = store.search(query_vector, search_options.top_k)?;

    Ok(hits_to_report(
        query,
        model,
        hits,
        root,
        search_options,
        total_chunks,
        start.elapsed().as_millis() as u64,
    ))
}

/// The empty-query short-circuit report (no store/embedder work). Public so the
/// daemon (`IndexManager::query`) can guard a blank query BEFORE touching the
/// resident embedder — its construction loads ONNX, so the guard must precede it,
/// not just `store.search` (TLDR-ac0.5 Codex review).
pub fn empty_search_report(query: &str, model: EmbeddingModel) -> SemanticSearchReport {
    SemanticSearchReport {
        results: Vec::new(),
        total_results: 0,
        query: query.to_string(),
        model,
        total_chunks: 0,
        matches_above_threshold: 0,
        latency_ms: 0,
        cache_hit: false,
    }
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

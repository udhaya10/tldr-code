//! Hybrid search using Reciprocal Rank Fusion (RRF)
//!
//! Combines BM25 keyword search with semantic embedding search using RRF.
//!
//! # RRF Formula
//! ```text
//! RRF_score(d) = sum(1 / (k + rank_i(d)) for each ranking i)
//! ```
//!
//! Where:
//! - k: constant to prevent division by small numbers (default 60)
//! - rank_i(d): rank of document d in ranking i (1-indexed)
//!
//! # Degradation
//! Gracefully degrades to BM25-only when no dense results are supplied
//! (`semantic_results` empty) — e.g. when the `semantic` feature is off.
//
// TLDR-4er/cs5 (done): WIRED. The dead `EmbeddingClient` HTTP stub is gone;
//   `hybrid_search` no longer constructs any embedding backend. Instead the
//   caller passes `semantic_results: &[SemanticResult]` computed from the
//   in-process `SemanticIndex` (caller-side, behind the `semantic` feature),
//   so this module stays feature-agnostic while fusing REAL dense results.
//   Empty slice => BM25-only (the honest degraded mode).

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::bm25::{Bm25Index, Bm25Result};
use crate::types::Language;
use crate::TldrResult;

/// One dense (embedding) search hit, as fed into RRF fusion.
///
/// TLDR-4er/cs5: this used to live in the dead HTTP `embedding_client` stub.
/// `hybrid_search` is now decoupled from any embedding backend — the caller
/// (which holds the in-process `SemanticIndex`, behind the `semantic` feature)
/// produces these and passes them in. That keeps `search/hybrid.rs` free of the
/// `semantic` feature gate while still fusing real dense results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticResult {
    /// Document ID / file path (the fusion key; must match BM25's file path).
    pub doc_id: String,
    /// Cosine similarity score (0-1 for normalized vectors).
    pub score: f64,
    /// Start line of the matching region.
    pub line_start: u32,
    /// End line of the matching region.
    pub line_end: u32,
    /// Snippet of matching content.
    pub snippet: String,
}

/// Default RRF k constant
pub const DEFAULT_K_CONSTANT: f64 = 60.0;

/// A single hybrid search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridResult {
    /// File path
    pub file_path: std::path::PathBuf,
    /// Combined RRF score
    pub rrf_score: f64,
    /// Rank in BM25 results (None if not in BM25 top-k)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bm25_rank: Option<usize>,
    /// Rank in semantic results (None if not in semantic top-k)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dense_rank: Option<usize>,
    /// BM25 score (None if not in BM25 results)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bm25_score: Option<f64>,
    /// Semantic similarity score (None if not in semantic results)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dense_score: Option<f64>,
    /// Snippet of matching content
    pub snippet: String,
    /// Terms that matched in BM25
    pub matched_terms: Vec<String>,
}

/// Report from hybrid search
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridSearchReport {
    /// Search results sorted by RRF score
    pub results: Vec<HybridResult>,
    /// Original query
    pub query: String,
    /// Total candidates considered
    pub total_candidates: usize,
    /// Results only in BM25
    pub bm25_only: usize,
    /// Results only in semantic search
    pub dense_only: usize,
    /// Results in both rankings
    pub overlap: usize,
    /// Fallback mode (if semantic search unavailable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_mode: Option<String>,
}

/// Perform hybrid search combining BM25 and semantic search
///
/// # Arguments
/// * `query` - Search query string
/// * `root` - Project root directory
/// * `language` - Programming language to search
/// * `top_k` - Number of results to return
/// * `k_constant` - RRF k constant (default 60)
/// * `semantic_results` - Dense hits from the caller's `SemanticIndex`, already
///   reduced to file granularity (best chunk per file). Empty => BM25-only.
///
/// # Returns
/// HybridSearchReport containing fused results
///
/// # Example
/// ```ignore
/// use tldr_core::search::hybrid::{hybrid_search, SemanticResult};
///
/// // Caller (with the `semantic` feature) builds a SemanticIndex, searches it,
/// // and converts the dense hits into `SemanticResult`s keyed by file path.
/// let dense: Vec<SemanticResult> = /* from SemanticIndex::search */ vec![];
/// let report = hybrid_search(
///     "process data",
///     Path::new("src/"),
///     Language::Python,
///     10,
///     60.0,
///     &dense,
/// )?;
/// ```
pub fn hybrid_search(
    query: &str,
    root: &Path,
    language: Language,
    top_k: usize,
    k_constant: f64,
    semantic_results: &[SemanticResult],
) -> TldrResult<HybridSearchReport> {
    // Build BM25 index and search
    let bm25_index = Bm25Index::from_project(root, language)?;
    let bm25_results = bm25_index.search(query, top_k * 2); // Get more for RRF

    // Dense side is supplied by the caller (from the in-process SemanticIndex).
    // Empty => honest BM25-only degradation (e.g. `semantic` feature off).
    let fallback_mode = if semantic_results.is_empty() {
        Some("bm25_only".to_string())
    } else {
        None
    };

    // Fuse results using RRF
    let fused = fuse_rrf(&bm25_results, semantic_results, k_constant, top_k);

    // Calculate statistics
    let bm25_files: std::collections::HashSet<_> = bm25_results
        .iter()
        .map(|r| r.file_path.to_string_lossy().to_string())
        .collect();
    let dense_files: std::collections::HashSet<_> =
        semantic_results.iter().map(|r| r.doc_id.clone()).collect();

    let overlap = bm25_files.intersection(&dense_files).count();
    let bm25_only = bm25_files.len() - overlap;
    let dense_only = dense_files.len() - overlap;

    Ok(HybridSearchReport {
        results: fused,
        query: query.to_string(),
        total_candidates: bm25_files.len() + dense_files.len() - overlap,
        bm25_only,
        dense_only,
        overlap,
        fallback_mode,
    })
}

// NOTE (TLDR-7xz.7): `hybrid_search_with_index` — the per-call cold path that
// pulled dense hits from an in-process `SemanticIndex` — was removed along
// with that type. The fusion machinery below (`hybrid_search`, `fuse_rrf`,
// the report types) is backend-agnostic and stays: Phase 2 (TLDR-utj.3) feeds
// it dense hits from the daemon's warm resident store instead. One lesson it
// carried forward: dense hits must be keyed by the SAME root-relative path
// form BM25 uses, or RRF overlap is always 0 and fusion degenerates to
// concatenation.

/// Fuse BM25 and semantic results using Reciprocal Rank Fusion
fn fuse_rrf(
    bm25_results: &[Bm25Result],
    semantic_results: &[SemanticResult],
    k: f64,
    top_k: usize,
) -> Vec<HybridResult> {
    let mut scores: HashMap<String, HybridResult> = HashMap::new();

    // Add BM25 results
    for (rank, result) in bm25_results.iter().enumerate() {
        let file_key = result.file_path.to_string_lossy().to_string();
        let rrf_contrib = 1.0 / (k + (rank + 1) as f64);

        let entry = scores
            .entry(file_key.clone())
            .or_insert_with(|| HybridResult {
                file_path: result.file_path.clone(),
                rrf_score: 0.0,
                bm25_rank: None,
                dense_rank: None,
                bm25_score: None,
                dense_score: None,
                snippet: String::new(),
                matched_terms: Vec::new(),
            });

        entry.rrf_score += rrf_contrib;
        entry.bm25_rank = Some(rank + 1);
        entry.bm25_score = Some(result.score);
        entry.snippet = result.snippet.clone();
        entry.matched_terms = result.matched_terms.clone();
    }

    // Add semantic results
    for (rank, result) in semantic_results.iter().enumerate() {
        let file_key = result.doc_id.clone();
        let rrf_contrib = 1.0 / (k + (rank + 1) as f64);

        let entry = scores
            .entry(file_key.clone())
            .or_insert_with(|| HybridResult {
                file_path: std::path::PathBuf::from(&result.doc_id),
                rrf_score: 0.0,
                bm25_rank: None,
                dense_rank: None,
                bm25_score: None,
                dense_score: None,
                snippet: String::new(),
                matched_terms: Vec::new(),
            });

        entry.rrf_score += rrf_contrib;
        entry.dense_rank = Some(rank + 1);
        entry.dense_score = Some(result.score);

        // Use semantic snippet if BM25 didn't provide one
        if entry.snippet.is_empty() {
            entry.snippet = result.snippet.clone();
        }
    }

    // Sort by RRF score and take top_k
    let mut results: Vec<HybridResult> = scores.into_values().collect();
    results.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(top_k);

    results
}

/// Calculate RRF score for a single document
///
/// # Arguments
/// * `ranks` - Vector of (ranking_id, rank) pairs for this document
/// * `k` - RRF constant
pub fn calculate_rrf_score(ranks: &[(usize, usize)], k: f64) -> f64 {
    ranks.iter().map(|(_, rank)| 1.0 / (k + *rank as f64)).sum()
}

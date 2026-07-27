//! Shared semantic build/search options and index limits
//!
//! Historically this module also housed `SemanticIndex`, the in-memory
//! per-call index (chunk → embed → cosine search). That type was removed in
//! TLDR-7xz.7: every consumer cold-built it per invocation (a full corpus
//! embed + ONNX load on EVERY call), which is exactly the silent slow path
//! the warm-daemon architecture eliminates. Serving now happens exclusively
//! through the daemon's resident `VectorStore` (see `vector_store.rs` /
//! `store_search.rs`); seeded similarity returns via a daemon API in Phase 2
//! (TLDR-utj).
//!
//! What remains here is the SHARED vocabulary both the store builder and the
//! daemon speak:
//!
//! - [`BuildOptions`] — model / granularity / language / cache selection
//! - [`SearchOptions`] — top-k / threshold / snippet shaping
//! - The P0 corpus limits ([`MAX_INDEX_SIZE`], memory bounds)
//! - [`make_snippet`] — result snippet shaping

use serde::{Deserialize, Serialize};

use crate::semantic::types::{ChunkGranularity, EmbeddingModel};

// =============================================================================
// Constants (P0 Mitigations)
// =============================================================================

/// Maximum number of chunks allowed in an index/store (P0 mitigation)
///
/// Prevents memory exhaustion on large codebases. For larger projects,
/// users should filter by language or directory.
pub const MAX_INDEX_SIZE: usize = 100_000;

/// Estimated memory per chunk in bytes
///
/// Calculation: 768 dims * 4 bytes per f32 + ~500 bytes metadata
pub(crate) const BYTES_PER_CHUNK: usize = 768 * 4 + 500;

/// Maximum memory usage in bytes (500MB)
pub(crate) const MAX_MEMORY_BYTES: usize = 500 * 1024 * 1024;

// =============================================================================
// Build Options
// =============================================================================

/// Options for building a semantic vector store
///
/// Controls how the store is constructed, including model selection,
/// chunking granularity, and caching behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildOptions {
    /// Embedding model to use
    pub model: EmbeddingModel,

    /// Chunking granularity (file or function level)
    pub granularity: ChunkGranularity,

    /// Languages to process (None = auto-detect all)
    pub languages: Option<Vec<String>>,

    /// Show progress during building
    pub show_progress: bool,

    /// Use embedding cache
    pub use_cache: bool,

    /// Collect build-time instrumentation (per-batch shape, cache accounting,
    /// RSS timeline + peak, phase boundaries, throughput) and expose it via
    /// [`crate::semantic::vector_store::VectorStore::build_metrics`].
    /// Off by default so the production path is byte-identical to the
    /// un-instrumented build (TLDR-9bxa.1: observe without changing behavior).
    pub collect_metrics: bool,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            model: EmbeddingModel::default(),
            granularity: ChunkGranularity::Function,
            languages: None,
            show_progress: true,
            use_cache: true,
            collect_metrics: false,
        }
    }
}

// =============================================================================
// Search Options
// =============================================================================

/// Options for semantic search operations
///
/// Controls how search results are filtered and ranked.
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// Maximum number of results to return
    pub top_k: usize,

    /// Minimum similarity threshold (0.0 to 1.0)
    pub threshold: f64,

    /// Include code snippet in results
    pub include_snippet: bool,

    /// Maximum lines in snippet
    pub snippet_lines: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            top_k: 10,
            threshold: 0.5,
            include_snippet: true,
            snippet_lines: 5,
        }
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Create a snippet from code content
///
/// Takes the first N lines of the content for display purposes.
pub(crate) fn make_snippet(content: &str, max_lines: usize) -> String {
    content
        .lines()
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n")
}

// =============================================================================
// Tests
// =============================================================================

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
#[derive(Debug, Clone)]
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
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            model: EmbeddingModel::default(),
            granularity: ChunkGranularity::Function,
            languages: None,
            show_progress: true,
            use_cache: true,
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

#[cfg(test)]
mod index_tests {
    use super::*;

    // =========================================================================
    // SearchOptions tests
    // =========================================================================

    #[test]
    fn search_options_default_values() {
        // GIVEN: Default search options
        let options = SearchOptions::default();

        // THEN: Should have sensible defaults
        assert_eq!(options.top_k, 10);
        assert!((options.threshold - 0.5).abs() < 1e-6);
        assert!(options.include_snippet);
        assert_eq!(options.snippet_lines, 5);
    }

    // =========================================================================
    // BuildOptions tests
    // =========================================================================

    #[test]
    fn build_options_default_values() {
        // GIVEN: Default build options
        let options = BuildOptions::default();

        // THEN: Should have sensible defaults
        assert_eq!(options.model, EmbeddingModel::ArcticM);
        assert_eq!(options.granularity, ChunkGranularity::Function);
        assert!(options.languages.is_none());
        assert!(options.show_progress);
        assert!(options.use_cache);
    }

    // =========================================================================
    // make_snippet tests
    // =========================================================================

    #[test]
    fn make_snippet_limits_lines() {
        // GIVEN: Multi-line content
        let content = "line1\nline2\nline3\nline4\nline5\nline6";

        // WHEN: We create a snippet with max 3 lines
        let snippet = make_snippet(content, 3);

        // THEN: Should have only 3 lines
        assert_eq!(snippet, "line1\nline2\nline3");
    }

    #[test]
    fn make_snippet_handles_short_content() {
        // GIVEN: Content with fewer lines than limit
        let content = "line1\nline2";

        // WHEN: We create a snippet with max 5 lines
        let snippet = make_snippet(content, 5);

        // THEN: Should have all lines
        assert_eq!(snippet, "line1\nline2");
    }

    #[test]
    fn make_snippet_handles_empty_content() {
        // GIVEN: Empty content
        let content = "";

        // WHEN: We create a snippet
        let snippet = make_snippet(content, 5);

        // THEN: Should be empty
        assert_eq!(snippet, "");
    }
}

//! Semantic search module for embedding-based code search
//!
//! This module provides AI-powered semantic code search using dense embeddings
//! from the Snowflake Arctic model family. It enables:
//!
//! - Natural language queries to find semantically related code
//! - Similarity detection between code fragments
//! - Embedding generation for downstream tools
//!
// TLDR-AUDIT(TLDR-6fm): ROOT CAUSE of the whole semantic-search epic. docs/
//   ARCHITECTURE.md (v2.0) defines tldr as a 5-layer STATIC stack
//   (AST -> CallGraph -> CFG -> DFG -> PDG). This module — embeddings, index,
//   hybrid fusion — is NOT a documented layer, NOT in the data-flow diagram,
//   NOT in the caching section. It exists only as scattered CLI/MCP glue. That
//   non-adoption is *why* it rotted (FAISS-downgrade=7kf, dead stub=cs5, unwired
//   hybrid=4er) while the 5 documented layers stayed clean and tested. The fix
//   is not just patching the bugs: PROMOTE this to a first-class "Layer 6:
//   Semantic/Retrieval" with the same rigor — own embedding+index+fusion here,
//   document it in ARCHITECTURE.md, back it with in-process `usearch`. See epic
//   TLDR-blm. (NOTE: `SemanticIndex` below is the REAL, working path — the old
//   HTTP stub search/embedding_client.rs was removed in TLDR-cs5.)
//!
//! # Architecture
//!
//! ```text
//! +-------------------+     +------------------+
//! |   SemanticIndex   |<--->|  EmbeddingCache  |
//! +-------------------+     +------------------+
//!          |                        |
//!          v                        v
//! +-------------------+     +------------------+
//! |     Embedder      |     |     Chunker      |
//! | (fastembed-rs)    |     | (tree-sitter)    |
//! +-------------------+     +------------------+
//!          |
//!          v
//! +-------------------+
//! |    Similarity     |
//! | (cosine, top-K)   |
//! +-------------------+
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use tldr_core::semantic::{SemanticIndex, SearchOptions, ChunkOptions, EmbedOptions};
//!
//! // Build an index from a project directory
//! let index = SemanticIndex::build(
//!     Path::new("src/"),
//!     ChunkOptions::default(),
//!     EmbedOptions::default(),
//!     None, // No cache
//! )?;
//!
//! // Search for semantically related code
//! let report = index.search("parse configuration file", SearchOptions::default())?;
//!
//! for result in report.results {
//!     println!("{}: {} (score: {:.2})",
//!         result.file_path.display(),
//!         result.function_name.unwrap_or_default(),
//!         result.score
//!     );
//! }
//! ```
//!
//! # Modules
//!
//! - `types`: Core data structures (CodeChunk, EmbeddingModel, etc.)
//! - `embedder`: Embedding generation using fastembed-rs (Phase 3)
//! - `chunker`: Code chunking via tree-sitter (Phase 4)
//! - `similarity`: Cosine similarity and top-K search (Phase 2)
//! - `cache`: JSON-based embedding cache (Phase 5)
//! - `index`: In-memory semantic index (Phase 6)

pub mod types;

// Re-export all public types for convenience
pub use types::{
    CacheConfig,
    CacheStats,
    ChunkGranularity,
    ChunkOptions,
    // Core types
    CodeChunk,
    // Option types
    EmbedOptions,
    EmbedReport,
    EmbeddedChunk,
    EmbeddingModel,
    SearchOptions,
    SemanticSearchReport,
    // Result types
    SemanticSearchResult,
    SimilarityReport,
    store_dir_for,
};

// Phase 2: Similarity
pub mod similarity;
pub use similarity::{cosine_similarity, is_normalized, normalize, top_k_similar};

// Placeholder re-exports for future phases
// These will be uncommented as each phase is implemented

// Phase 3: Embedder
pub mod embedder;
pub use embedder::Embedder;

// Phase 4: Chunker
pub mod chunker;
pub use chunker::{chunk_code, chunk_file, is_corpus_file, ChunkResult, SkippedFile};

// Phase 5: Cache
pub mod cache;
pub use cache::EmbeddingCache;

// Phase 6: Index
pub mod index;
pub use index::{BuildOptions, SearchOptions as IndexSearchOptions, SemanticIndex, MAX_INDEX_SIZE};

// Enrichment: builds the structural "embedding text" (signature + callers/callees
// + CFG/DFG summaries + deps) that the index embeds instead of raw source.
// TLDR-lwg: this module was never declared, so it shipped uncompiled and unwired.
pub mod enrichment;
pub use enrichment::{build_embedding_text, enrich_chunks, EmbeddingUnit};

// TLDR-l5d: usearch-backed vector store (key u64 -> f32 vector). Step 1 is the
// dependency smoke test + index helper; sidecar/manifest/crash-safe-save follow
// (docs/INCREMENTAL_REINDEX_DESIGN.md §4/§7).
pub mod vector_store;

// TLDR-m01/zxb: store-backed semantic search — the ONLY search path (TLDR-lx7).
// No SemanticIndex fallback. VectorStore works or the user gets an error.
pub mod store_search;
pub use store_search::{load_or_build_store, query_store, search_with_store};

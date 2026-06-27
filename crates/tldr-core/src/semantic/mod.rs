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
//! |    VectorStore    |<--->|  EmbeddingCache  |
//! | (usearch, warm    |     +------------------+
//! |  in the daemon)   |              |
//! +-------------------+              v
//!          |                +------------------+
//!          v                |     Chunker      |
//! +-------------------+     | (tree-sitter)    |
//! |     Embedder      |     +------------------+
//! | (fastembed-rs)    |
//! +-------------------+
//! ```
//!
//! Serving happens exclusively through the daemon's resident [`vector_store`]
//! (warm, full quality) — there is no per-call in-process index. The old
//! `SemanticIndex` (cold chunk→embed→search on every invocation) was removed
//! in TLDR-7xz.7.
//!
//! # Example
//!
//! ```rust,ignore
//! use tldr_core::semantic::{load_or_build_store, query_store, BuildOptions, IndexSearchOptions};
//!
//! // Build (or load) the persistent vector store for a project
//! let store = load_or_build_store(root, &store_dir, &BuildOptions::default(), None)?;
//!
//! // Search it (the daemon does this on its warm resident copy)
//! let report = query_store(&store, root, "parse configuration file",
//!     &IndexSearchOptions::default(), BuildOptions::default().model)?;
//! ```
//!
//! # Modules
//!
//! - `types`: Core data structures (CodeChunk, EmbeddingModel, etc.)
//! - `embedder`: Embedding generation using fastembed-rs (Phase 3)
//! - `chunker`: Code chunking via tree-sitter (Phase 4)
//! - `similarity`: Cosine similarity and top-K search (Phase 2)
//! - `cache`: JSON-based embedding cache (Phase 5)
//! - `index`: shared build/search options + corpus limits (Phase 6)
//! - `vector_store` / `store_search`: persistent usearch store — the serve path

pub mod types;

// Re-export all public types for convenience
pub use types::{
    store_dir_for,
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

// Phase 6: shared build/search options + index limits. The `SemanticIndex`
// type itself was nuked in TLDR-7xz.7 — serving goes through the daemon's
// resident VectorStore only (store_search/vector_store below).
pub mod index;
pub use index::{BuildOptions, SearchOptions as IndexSearchOptions, MAX_INDEX_SIZE};

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
pub use store_search::{
    empty_search_report, load_or_build_store, query_store, query_store_with_vector,
    search_with_store,
};

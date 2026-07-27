//! Core types for the semantic search module
//!
//! This module defines all data structures used by the semantic search system:
//! - `CodeChunk`: A piece of code that can be embedded
//! - `EmbeddedChunk`: A CodeChunk with its embedding vector
//! - `EmbeddingModel`: Available embedding models (Snowflake Arctic family)
//! - `ChunkGranularity`: File-level vs function-level chunking
//! - `SemanticSearchResult`: A single search result with score
//! - `SemanticSearchReport`: Full search report with results and metadata
//! - `EmbedReport`: Report from embedding generation
//! - `SimilarityReport`: Report from similarity search

use std::path::PathBuf;

use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};

use crate::Language;

/// A chunk of code that can be embedded
///
/// Represents a discrete unit of code extracted from a source file,
/// either at file-level or function-level granularity.
///
/// # Example
///
/// ```rust
/// use std::path::PathBuf;
/// use tldr_core::semantic::CodeChunk;
/// use tldr_core::Language;
///
/// let chunk = CodeChunk {
///     file_path: PathBuf::from("src/main.rs"),
///     function_name: Some("process_data".to_string()),
///     class_name: None,
///     line_start: 10,
///     line_end: 25,
///     content: "fn process_data() { ... }".to_string(),
///     content_hash: "abc123".to_string(),
///     language: Language::Rust,
///     structure: Default::default(),
/// };
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CodeChunk {
    /// Source file path (relative to project root)
    pub file_path: PathBuf,

    /// Function/method name (None for file-level chunks)
    pub function_name: Option<String>,

    /// Class/struct name containing this function (if any)
    pub class_name: Option<String>,

    /// Start line number (1-indexed)
    pub line_start: u32,

    /// End line number (1-indexed, inclusive)
    pub line_end: u32,

    /// The source code text
    pub content: String,

    /// Content hash for cache invalidation (MD5)
    pub content_hash: String,

    /// Language of the code
    pub language: Language,

    /// Structural planning metadata. Defaults preserve compatibility with
    /// chunks serialized before structural planning was introduced.
    #[serde(default)]
    pub structure: ChunkStructure,
}

/// How a chunk relates to its extracted semantic root.
#[derive(
    Archive,
    Debug,
    Clone,
    Copy,
    Default,
    RkyvDeserialize,
    RkyvSerialize,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum StructuralRole {
    /// Extracted semantic root retained intact.
    #[default]
    WholeRoot,
    /// Signature/header duplicated to preserve parent context.
    ParentSummary,
    /// One or more adjacent AST/source segments.
    AstChild,
    /// Oversized indivisible region split by tokenizer offsets.
    TokenizerFallback,
    /// Malformed or unparseable region split by tokenizer offsets.
    ParseFallback,
}

/// Deterministic structural provenance for a planned chunk.
#[derive(
    Archive,
    Debug,
    Clone,
    Default,
    RkyvDeserialize,
    RkyvSerialize,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
)]
#[serde(default)]
pub struct ChunkStructure {
    /// Structural role of this chunk.
    pub role: StructuralRole,
    /// Named-child ordinals from the semantic root (never source line numbers).
    pub ast_path: Vec<u32>,
    /// Half-open byte range relative to the complete source file.
    pub source_range: (usize, usize),
    /// Explicit duplicated source range, if tokenizer windows overlap.
    pub overlap_range: Option<(usize, usize)>,
    /// Number of bytes intentionally duplicated from the previous chunk.
    pub overlap_bytes: usize,
    /// Deterministic repository-relative path used in composed context.
    pub repository_path: String,
    /// Qualified semantic symbol, if the AST exposes one.
    pub qualified_symbol: Option<String>,
    /// Signature/header retained as ancestor context.
    pub signature: Option<String>,
}

/// A CodeChunk with its embedding vector
///
/// Wraps a `CodeChunk` together with its dense embedding vector
/// for use in similarity search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddedChunk {
    /// The original code chunk
    pub chunk: CodeChunk,

    /// Dense embedding vector (dimensions depend on model)
    // TLDR-AUDIT(TLDR-8pt): Full f32, no quantization — ~3KB/vector, 357MB at the
    // 100K index cap. Subsumed by TLDR-7kf: if `usearch` is adopted it owns vector
    // storage and quantizes natively (ScalarKind::I8/BF16/B1x8), so this field's
    // role shrinks to "transient input handed to index.add". Don't build a
    // bespoke quantizer here — let the index do it. See epic TLDR-blm.
    pub embedding: Vec<f32>,
}

/// Supported embedding models (Snowflake Arctic family)
///
/// All models are from the Snowflake Arctic embedding family,
/// which is optimized for code and technical content.
///
/// # Model Comparison
///
/// | Model | Dimensions | Size | Context |
/// |-------|------------|------|---------|
/// | ArcticXS | 384 | 30MB | 512 |
/// | ArcticS | 384 | 90MB | 512 |
/// | ArcticM | 768 | 110MB | 512 |
/// | ArcticMLong | 768 | 110MB | 8192 |
/// | ArcticL | 1024 | 335MB | 512 |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmbeddingModel {
    /// 384 dimensions, 30MB, 512 context - fastest, smallest
    ArcticXS,
    /// 384 dimensions, 90MB, 512 context - small
    ArcticS,
    /// 768 dimensions, 110MB, 512 context - balanced (DEFAULT)
    #[default]
    ArcticM,
    /// 768 dimensions, 110MB, 8192 context - long context
    ArcticMLong,
    /// 1024 dimensions, 335MB, 512 context - highest quality
    ArcticL,
}

impl EmbeddingModel {
    /// Get embedding dimension for this model
    ///
    /// Returns the size of the embedding vector produced by this model.
    ///
    /// # Example
    ///
    /// ```rust
    /// use tldr_core::semantic::EmbeddingModel;
    ///
    /// assert_eq!(EmbeddingModel::ArcticM.dimensions(), 768);
    /// assert_eq!(EmbeddingModel::ArcticXS.dimensions(), 384);
    /// ```
    pub fn dimensions(&self) -> usize {
        match self {
            Self::ArcticXS | Self::ArcticS => 384,
            Self::ArcticM | Self::ArcticMLong => 768,
            Self::ArcticL => 1024,
        }
    }

    /// Get max context length (tokens)
    ///
    /// Returns the maximum number of tokens the model can process.
    /// Text longer than this will be truncated.
    ///
    /// # Example
    ///
    /// ```rust
    /// use tldr_core::semantic::EmbeddingModel;
    ///
    /// assert_eq!(EmbeddingModel::ArcticM.max_context(), 512);
    /// assert_eq!(EmbeddingModel::ArcticMLong.max_context(), 8192);
    /// ```
    pub fn max_context(&self) -> usize {
        match self {
            Self::ArcticMLong => 8192,
            _ => 512,
        }
    }

    /// Query-side instruction prefix for retrieval.
    ///
    /// Snowflake Arctic Embed models are trained ASYMMETRICALLY: the search
    /// query is prefixed with this instruction, while indexed documents/passages
    /// are embedded with NO prefix. Prepending it to the query (only) is the
    /// model's intended usage and measurably improves recall. fastembed does not
    /// apply it automatically — it's the caller's responsibility. TLDR-dlk.
    pub fn query_prefix(&self) -> &'static str {
        // All current variants are Snowflake Arctic Embed v1, which share this
        // query prefix (per the model card).
        match self {
            Self::ArcticXS | Self::ArcticS | Self::ArcticM | Self::ArcticMLong | Self::ArcticL => {
                "Represent this sentence for searching relevant passages: "
            }
        }
    }

    /// Get the model name as used by fastembed
    ///
    /// Returns a string identifier for the model.
    pub fn model_name(&self) -> &'static str {
        match self {
            Self::ArcticXS => "Snowflake/snowflake-arctic-embed-xs",
            Self::ArcticS => "Snowflake/snowflake-arctic-embed-s",
            Self::ArcticM => "Snowflake/snowflake-arctic-embed-m",
            Self::ArcticMLong => "Snowflake/snowflake-arctic-embed-m-long",
            Self::ArcticL => "Snowflake/snowflake-arctic-embed-l",
        }
    }

    /// Parse a model string (e.g. "arctic-m", "m") into an EmbeddingModel.
    pub fn parse(model_str: &str) -> Result<Self, String> {
        match model_str {
            "arctic-xs" | "xs" => Ok(Self::ArcticXS),
            "arctic-s" | "s" => Ok(Self::ArcticS),
            "arctic-m" | "m" => Ok(Self::ArcticM),
            "arctic-m-long" | "m-long" => Ok(Self::ArcticMLong),
            "arctic-l" | "l" => Ok(Self::ArcticL),
            _ => Err(format!(
                "Invalid model '{}'. Options: arctic-xs, arctic-s, arctic-m, arctic-m-long, arctic-l",
                model_str
            )),
        }
    }

    /// Resolve the effective model from CLI flag and config.
    /// Precedence: cli_flag (if provided) > config > built-in default.
    pub fn resolve(
        cli_model: Option<&str>,
        config: &crate::config::TldrConfig,
    ) -> Result<Self, String> {
        if config.embedding.provider != "local" {
            return Err(format!(
                "Cloud embedding provider '{}' is not supported in this build. \
                 Set embedding.provider to \"local\" in your config, or remove it.",
                config.embedding.provider
            ));
        }

        if let Some(flag) = cli_model {
            return Self::parse(flag);
        }

        if let Some(ref model_str) = config.embedding.model {
            return Self::parse(model_str);
        }

        Ok(Self::default())
    }
}

/// Granularity for code chunking
///
/// Determines how code is split into chunks for embedding.
///
/// # Variants
///
/// - `File`: One chunk per file (entire file content)
/// - `Function`: One chunk per function/method (DEFAULT)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChunkGranularity {
    /// One chunk per file
    File,
    /// One chunk per function/method (DEFAULT)
    #[default]
    Function,
}

/// Semantic search result
///
/// Represents a single result from a semantic search query,
/// including the matched code location and similarity score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticSearchResult {
    /// File path
    pub file_path: PathBuf,

    /// Function name (if function-level)
    pub function_name: Option<String>,

    /// Class name (if method)
    pub class_name: Option<String>,

    /// Cosine similarity score (0.0 to 1.0 for normalized vectors)
    pub score: f64,

    /// Start line
    pub line_start: u32,

    /// End line
    pub line_end: u32,

    /// Code snippet (truncated for display)
    pub snippet: String,
}

/// Report from semantic search
///
/// Contains all results from a semantic search query along with
/// metadata about the search (model used, timing, etc).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticSearchReport {
    /// Search results sorted by score (descending)
    pub results: Vec<SemanticSearchResult>,

    /// Total number of results returned (equals `results.len()`).
    ///
    /// schema-cleanup-v1 BUG-15: explicit count populated by the
    /// search executor so consumers don't need to re-derive it from
    /// `results | length`. Mirrors the new `total_results` field on
    /// `EnrichedSearchReport` so semantic search and BM25 search share
    /// the same schema shape.
    #[serde(default)]
    pub total_results: usize,

    /// Original query
    pub query: String,

    /// Model used for query embedding
    pub model: EmbeddingModel,

    /// Total chunks searched
    pub total_chunks: usize,

    /// Results above threshold
    pub matches_above_threshold: usize,

    /// Search latency in milliseconds
    pub latency_ms: u64,

    /// Whether cache was used
    pub cache_hit: bool,
}

/// Report from embedding generation
///
/// Contains metadata about an embedding operation,
/// including timing and cache statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedReport {
    /// Path that was embedded
    pub path: PathBuf,

    /// Model used
    pub model: EmbeddingModel,

    /// Granularity used
    pub granularity: ChunkGranularity,

    /// Number of chunks embedded
    pub chunks_embedded: usize,

    /// Number of chunks loaded from cache
    pub chunks_cached: usize,

    /// Number of source files represented by created chunks.
    pub files_indexed: usize,

    /// Number of files skipped during chunking.
    pub files_skipped: usize,

    /// Skipped files with unsupported or filtered languages.
    pub files_unsupported: usize,

    /// Skipped files rejected by the centralized size policy.
    pub files_oversized: usize,

    /// Embedded chunks (if output requested)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunks: Option<Vec<EmbeddedChunk>>,

    /// Total embedding time in milliseconds
    pub latency_ms: u64,
}

/// Report from similarity search
///
/// Contains results from finding code similar to a given chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimilarityReport {
    /// Source file/function being compared
    pub source: CodeChunk,

    /// Similar code fragments
    pub similar: Vec<SemanticSearchResult>,

    /// Model used
    pub model: EmbeddingModel,

    /// Total chunks compared
    pub total_compared: usize,

    /// Whether self was excluded
    pub exclude_self: bool,
}

/// Options for embedding generation
#[derive(Debug, Clone)]
pub struct EmbedOptions {
    /// Model to use (default: ArcticM)
    pub model: EmbeddingModel,

    /// Show progress during embedding
    pub show_progress: bool,

    /// Batch size for embedding (default: 32)
    pub batch_size: usize,
}

impl Default for EmbedOptions {
    fn default() -> Self {
        Self {
            model: EmbeddingModel::default(),
            show_progress: false,
            batch_size: 32,
        }
    }
}

/// Code chunking options
#[derive(Debug, Clone, Default)]
pub struct ChunkOptions {
    /// Granularity (file or function)
    pub granularity: ChunkGranularity,

    /// Maximum chunk size in characters (0 = no limit)
    pub max_chunk_size: usize,

    /// Include docstrings/comments in chunks
    pub include_docs: bool,

    /// Languages to process (None = auto-detect)
    pub languages: Option<Vec<Language>>,
}

/// Options for similarity search
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// Number of results to return
    pub top_k: usize,

    /// Minimum similarity threshold (0.0 to 1.0)
    pub threshold: f64,

    /// Model to use for query embedding
    pub model: EmbeddingModel,

    /// Exclude exact matches (for similarity search)
    pub exclude_self: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            top_k: 10,
            threshold: 0.5,
            model: EmbeddingModel::default(),
            exclude_self: false,
        }
    }
}

/// Cache configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    /// Platform cache directory containing the redb cache and optional legacy
    /// rkyv generation awaiting one-time migration.
    pub cache_dir: PathBuf,

    /// Maximum cache size in MB (default: 500)
    pub max_size_mb: usize,

    /// Cache entry TTL in days (default: 30)
    pub ttl_days: u32,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            cache_dir: dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("tldr")
                .join("embeddings"),
            max_size_mb: 500,
            ttl_days: 30,
        }
    }
}

/// Resolve the per-project store directory for usearch vector stores.
///
/// Layout: `~/.cache/tldr/stores/<hash>/` where `<hash>` is the first 16 hex
/// chars of the MD5 of the canonicalized project root. Both the daemon and
/// cold CLI call this so they resolve a BYTE-IDENTICAL path (TLDR-zxb
/// requirement). The directory is OUTSIDE the indexed corpus (satisfying
/// the store-location precondition for the freshness gate).
pub fn store_dir_for(project_root: &std::path::Path) -> std::path::PathBuf {
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let digest = md5::compute(canonical.to_string_lossy().as_bytes());
    let hash = &format!("{:x}", digest)[..16];
    dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("tldr")
        .join("stores")
        .join(hash)
}

/// Cache statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    /// Number of entries
    pub entries: usize,
    /// Total size in bytes
    pub size_bytes: usize,
    /// Hit rate (0.0 to 1.0)
    pub hit_rate: f64,
}

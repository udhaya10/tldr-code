//! Error types for TLDR operations
//!
//! This module defines the error taxonomy for all TLDR operations.
//! Error messages are designed to match Python output format (M17: Error Message Parity).

use std::path::PathBuf;
use thiserror::Error;

/// All possible errors from TLDR operations.
///
/// Error messages follow a consistent format to maintain parity with
/// the Python implementation and enable reliable error handling in
/// downstream tooling.
#[derive(Debug, Error)]
pub enum TldrError {
    // =========================================================================
    // File System Errors
    // =========================================================================
    /// Path does not exist
    #[error("Path not found: {0}")]
    PathNotFound(PathBuf),

    /// Directory traversal attack detected (path contains ..)
    #[error("Path traversal detected: {0}")]
    PathTraversal(PathBuf),

    /// Symlink creates a cycle
    #[error("Symlink cycle detected: {0}")]
    SymlinkCycle(PathBuf),

    /// Insufficient permissions to access path
    #[error("Permission denied: {0}")]
    PermissionDenied(PathBuf),

    // =========================================================================
    // Metrics Errors (Session 15)
    // =========================================================================
    /// File exceeds maximum allowed size
    #[error("File too large: {path} is {size_mb}MB (max {max_mb}MB)")]
    FileTooLarge {
        /// Path of the oversized file.
        path: PathBuf,
        /// Observed size in megabytes.
        size_mb: usize,
        /// Configured maximum size in megabytes.
        max_mb: usize,
    },

    /// Encoding error when reading file
    #[error("Encoding error in {path}: {detail}")]
    EncodingError {
        /// File path that could not be decoded.
        path: PathBuf,
        /// Decoder failure details.
        detail: String,
    },

    /// Coverage report parsing error
    #[error("Coverage parse error ({format}): {detail}")]
    CoverageParseError {
        /// Coverage format being parsed (for example, `lcov`).
        format: String,
        /// Parser failure details.
        detail: String,
    },

    /// Not a git repository (for hotspots command)
    #[error("Not a git repository: {0}")]
    NotGitRepository(PathBuf),

    /// Git operation in progress (rebase, merge, etc.)
    #[error("Git operation in progress: {0}. Complete or abort the operation first.")]
    GitOperationInProgress(String),

    /// Generic git command failure
    #[error("Git error: {0}")]
    GitError(String),

    // =========================================================================
    // Parse Errors
    // =========================================================================
    /// Syntax or parsing error in source file
    ///
    /// Includes optional line number for better error messages (M17, M20).
    #[error("Parse error in {file}{}: {message}",
        line.map(|l| format!(" at line {}", l)).unwrap_or_default())]
    ParseError {
        /// Source file where parsing failed.
        file: PathBuf,
        /// Optional one-based source line where parsing failed.
        line: Option<u32>,
        /// Human-readable parse error message.
        message: String,
    },

    /// File extension doesn't match any supported language
    #[error("Unsupported language: {0}")]
    UnsupportedLanguage(String),

    // =========================================================================
    // Analysis Errors
    // =========================================================================
    /// Function could not be found in the codebase
    ///
    /// Includes suggestions for similar names when available (M20: Error Messages Unusable).
    #[error("Function not found: {name}{}{}",
        file.as_ref().map(|f| format!(" in {}", f.display())).unwrap_or_default(),
        if suggestions.is_empty() { String::new() }
        else { format!("\n\nDid you mean:\n{}", suggestions.iter().map(|s| format!("  - {}", s)).collect::<Vec<_>>().join("\n")) }
    )]
    FunctionNotFound {
        /// Requested function name.
        name: String,
        /// Optional file scope used during lookup.
        file: Option<PathBuf>,
        /// Suggested nearby function names.
        suggestions: Vec<String>,
    },

    /// Invalid slice direction (must be "backward" or "forward")
    #[error("Invalid direction: {0}. Expected 'backward' or 'forward'")]
    InvalidDirection(String),

    /// Line number is not within the specified function
    #[error("Line {0} is not within the specified function")]
    LineNotInFunction(u32),

    /// No supported files found in directory (T32 mitigation)
    #[error("No supported files found in {0}")]
    NoSupportedFiles(PathBuf),

    /// Generic entity not found error (class, module, etc.)
    #[error("{entity} not found: {name}{}",
        suggestion.as_ref().map(|s| format!("\n\n{}", s)).unwrap_or_default())]
    NotFound {
        /// Kind of entity that was requested (class, module, etc.).
        entity: String,
        /// Name of the missing entity.
        name: String,
        /// Optional hint to resolve the issue.
        suggestion: Option<String>,
    },

    /// Invalid argument value
    #[error("Invalid argument {arg}: {message}{}",
        suggestion.as_ref().map(|s| format!("\n\nHint: {}", s)).unwrap_or_default())]
    InvalidArgs {
        /// Argument name that failed validation.
        arg: String,
        /// Validation failure details.
        message: String,
        /// Optional corrective hint.
        suggestion: Option<String>,
    },

    // =========================================================================
    // Daemon Errors
    // =========================================================================
    /// Error communicating with the daemon
    #[error("Daemon error: {0}")]
    DaemonError(String),

    /// Connection to daemon failed
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    /// Operation timed out
    #[error("Timeout: {0}")]
    Timeout(String),

    // =========================================================================
    // MCP Errors
    // =========================================================================
    /// Error in MCP protocol handling
    #[error("MCP error: {0}")]
    McpError(String),

    // =========================================================================
    // Serialization Errors
    // =========================================================================
    /// JSON or other serialization error
    #[error("Serialization error: {0}")]
    SerializationError(String),

    // =========================================================================
    // Semantic Search Errors (Session 16)
    // =========================================================================
    /// Embedding model or generation error
    #[error("Embedding error: {0}")]
    Embedding(String),

    /// Model loading/initialization failed
    #[error("Failed to load embedding model '{model}': {detail}")]
    ModelLoadError {
        /// Model identifier that failed to initialize.
        model: String,
        /// Loader error details.
        detail: String,
    },

    /// Index exceeds maximum allowed size (P0 mitigation)
    #[error("Index too large: {count} chunks exceeds maximum of {max}. Filter by language or directory.")]
    IndexTooLarge {
        /// Number of chunks requested for indexing.
        count: usize,
        /// Configured maximum chunk count.
        max: usize,
    },

    /// Estimated memory usage exceeds limit (P0 mitigation)
    #[error("Memory limit exceeded: estimated {estimated_mb}MB exceeds maximum of {max_mb}MB")]
    MemoryLimitExceeded {
        /// Estimated memory requirement in megabytes.
        estimated_mb: usize,
        /// Configured maximum memory limit in megabytes.
        max_mb: usize,
    },

    /// Code chunk not found in index
    #[error("Chunk not found: {file}{}", function.as_ref().map(|f| format!("::{}", f)).unwrap_or_default())]
    ChunkNotFound {
        /// File path key used during chunk lookup.
        file: String,
        /// Optional function key within the file.
        function: Option<String>,
    },

    // =========================================================================
    // IO Errors
    // =========================================================================
    /// General IO error
    #[error(transparent)]
    IoError(#[from] std::io::Error),
}

impl TldrError {
    /// Create a FunctionNotFound error with just the name
    pub fn function_not_found(name: impl Into<String>) -> Self {
        TldrError::FunctionNotFound {
            name: name.into(),
            file: None,
            suggestions: Vec::new(),
        }
    }

    /// Create a FunctionNotFound error with file context
    pub fn function_not_found_in_file(name: impl Into<String>, file: PathBuf) -> Self {
        TldrError::FunctionNotFound {
            name: name.into(),
            file: Some(file),
            suggestions: Vec::new(),
        }
    }

    /// Create a FunctionNotFound error with suggestions (M20)
    pub fn function_not_found_with_suggestions(
        name: impl Into<String>,
        file: Option<PathBuf>,
        suggestions: Vec<String>,
    ) -> Self {
        TldrError::FunctionNotFound {
            name: name.into(),
            file,
            suggestions,
        }
    }

    /// Create a ParseError with line information
    pub fn parse_error(file: PathBuf, line: Option<u32>, message: impl Into<String>) -> Self {
        TldrError::ParseError {
            file,
            line,
            message: message.into(),
        }
    }

    /// Check if this is a recoverable error that allows partial results
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            TldrError::ParseError { .. }
                | TldrError::PermissionDenied(_)
                | TldrError::FunctionNotFound { .. }
                // typescript-large-file-perf-v1: a single oversize
                // file (e.g. an auto-generated 2.3 MB `.d.ts`)
                // should be skipped with a warning, not abort the
                // whole scan. Callers that walk multiple files (the
                // structure / calls / smells / dead / secure
                // entrypoints) treat this variant as a per-file
                // skip and continue.
                | TldrError::FileTooLarge { .. }
                // kotlin-extract-and-cpp-extensions-v1 (P6.BUG-N2): a
                // single file with an unrecognised extension picked
                // up by a directory walk (e.g. a `.zzz` next to
                // valid `.cpp` sources, or any extension the
                // single-bucket classifier hasn't been taught yet)
                // must not abort the whole walk. Per-file commands
                // (`tldr extract somefile.zzz`) still surface this
                // as a hard error because they consult the error
                // directly, not via `is_recoverable`.
                | TldrError::UnsupportedLanguage(_)
        )
    }

    /// Get error code for CLI exit status
    ///
    /// Exit codes are consistent with Unix conventions:
    /// - 1: General IO error
    /// - 2-9: File system errors
    /// - 10-19: Parse errors
    /// - 20-29: Analysis errors
    /// - 30-39: Network/daemon errors
    /// - 40-49: Serialization errors
    pub fn exit_code(&self) -> i32 {
        match self {
            // File system errors (2-9)
            TldrError::PathNotFound(_) => 2,
            TldrError::PathTraversal(_) => 3,
            TldrError::SymlinkCycle(_) => 4,
            TldrError::PermissionDenied(_) => 5,
            TldrError::FileTooLarge { .. } => 6,
            TldrError::EncodingError { .. } => 7,
            TldrError::NotGitRepository(_) => 8,
            TldrError::GitOperationInProgress(_) => 9,
            TldrError::GitError(_) => 9,

            // Parse errors (10-19)
            TldrError::ParseError { .. } => 10,
            TldrError::UnsupportedLanguage(_) => 11,
            TldrError::CoverageParseError { .. } => 12,

            // Analysis errors (20-29)
            TldrError::FunctionNotFound { .. } => 20,
            TldrError::InvalidDirection(_) => 21,
            TldrError::LineNotInFunction(_) => 22,
            TldrError::NoSupportedFiles(_) => 23,
            TldrError::NotFound { .. } => 24,
            TldrError::InvalidArgs { .. } => 25,

            // Network/daemon errors (30-39)
            TldrError::DaemonError(_) => 30,
            TldrError::ConnectionFailed(_) => 31,
            TldrError::Timeout(_) => 32,
            TldrError::McpError(_) => 33,

            // Serialization errors (40-49)
            TldrError::SerializationError(_) => 40,

            // Semantic search errors (50-59)
            TldrError::Embedding(_) => 50,
            TldrError::ModelLoadError { .. } => 51,
            TldrError::IndexTooLarge { .. } => 52,
            TldrError::MemoryLimitExceeded { .. } => 53,
            TldrError::ChunkNotFound { .. } => 54,

            // General IO error
            TldrError::IoError(_) => 1,
        }
    }

    /// Get a short error category for logging/metrics
    pub fn category(&self) -> &'static str {
        match self {
            TldrError::PathNotFound(_)
            | TldrError::PathTraversal(_)
            | TldrError::SymlinkCycle(_)
            | TldrError::PermissionDenied(_)
            | TldrError::FileTooLarge { .. }
            | TldrError::EncodingError { .. } => "filesystem",

            TldrError::NotGitRepository(_)
            | TldrError::GitOperationInProgress(_)
            | TldrError::GitError(_) => "git",

            TldrError::ParseError { .. }
            | TldrError::UnsupportedLanguage(_)
            | TldrError::CoverageParseError { .. } => "parse",

            TldrError::FunctionNotFound { .. }
            | TldrError::InvalidDirection(_)
            | TldrError::LineNotInFunction(_)
            | TldrError::NoSupportedFiles(_)
            | TldrError::NotFound { .. }
            | TldrError::InvalidArgs { .. } => "analysis",

            TldrError::DaemonError(_) | TldrError::ConnectionFailed(_) | TldrError::Timeout(_) => {
                "daemon"
            }

            TldrError::McpError(_) => "mcp",

            TldrError::SerializationError(_) => "serialization",

            TldrError::Embedding(_)
            | TldrError::ModelLoadError { .. }
            | TldrError::IndexTooLarge { .. }
            | TldrError::MemoryLimitExceeded { .. }
            | TldrError::ChunkNotFound { .. } => "semantic",

            TldrError::IoError(_) => "io",
        }
    }
}

// Allow conversion from serde_json errors
impl From<serde_json::Error> for TldrError {
    fn from(err: serde_json::Error) -> Self {
        TldrError::SerializationError(err.to_string())
    }
}

// Allow conversion from regex errors (used in references module for text search)
impl From<regex::Error> for TldrError {
    fn from(err: regex::Error) -> Self {
        TldrError::ParseError {
            file: std::path::PathBuf::new(),
            line: None,
            message: format!("Invalid regex pattern: {}", err),
        }
    }
}

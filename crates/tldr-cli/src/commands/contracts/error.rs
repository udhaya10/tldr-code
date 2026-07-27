//! Error types for Contracts & Flow commands
//!
//! Provides specific error types for all failure modes in the contracts
//! and flow analysis commands. Errors include actionable information like
//! file paths and line numbers.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Errors specific to contracts and flow analysis commands.
///
/// Each variant includes contextual information to help users understand
/// and fix the issue.
#[derive(Debug, Error)]
pub enum ContractsError {
    /// Source file not found.
    #[error("file not found: {}", path.display())]
    FileNotFound {
        /// Path that was not found
        path: PathBuf,
    },

    /// Function not found in source file.
    #[error("function '{function}' not found in {}", file.display())]
    FunctionNotFound {
        /// Function name that was searched for
        function: String,
        /// File that was searched
        file: PathBuf,
    },

    /// Test path not found.
    #[error("test path not found: {}", path.display())]
    TestPathNotFound {
        /// Path that was not found
        path: PathBuf,
    },

    /// Line number is outside function range.
    #[error("line {line} is outside function '{function}' (lines {start}-{end})")]
    LineOutsideFunction {
        /// Line number that was requested
        line: u32,
        /// Function name
        function: String,
        /// Start line of function
        start: u32,
        /// End line of function
        end: u32,
    },

    /// Parse error in source file.
    #[error("parse error in {}: {message}", file.display())]
    ParseError {
        /// File that failed to parse
        file: PathBuf,
        /// Parser error message
        message: String,
    },

    /// SSA construction failed.
    #[error("SSA construction failed: {0}")]
    SsaError(String),

    /// Analysis did not converge within iteration limit.
    #[error("analysis did not converge after {iterations} iterations")]
    DidNotConverge {
        /// Number of iterations attempted
        iterations: u32,
    },

    /// Sub-analysis failed in verify command.
    #[error("sub-analysis '{name}' failed: {message}")]
    SubAnalysisFailed {
        /// Name of the sub-analysis that failed
        name: String,
        /// Error message from the sub-analysis
        message: String,
    },

    /// No test directory found in project.
    #[error("no test directory found in {}", project.display())]
    NoTestDirectory {
        /// Project directory that was searched
        project: PathBuf,
    },

    /// Operation timed out.
    #[error("operation timed out after {timeout_secs}s")]
    Timeout {
        /// Timeout duration in seconds
        timeout_secs: u64,
    },

    /// File too large to analyze.
    #[error("file too large: {} ({bytes} bytes, max {max_bytes} bytes)", path.display())]
    FileTooLarge {
        /// Path to the file
        path: PathBuf,
        /// Actual file size
        bytes: u64,
        /// Maximum allowed size
        max_bytes: u64,
    },

    /// AST too deeply nested.
    #[error("AST too deeply nested in {}: depth {depth} exceeds limit {max_depth}", file.display())]
    AstTooDeep {
        /// File with deeply nested AST
        file: PathBuf,
        /// Actual depth
        depth: u32,
        /// Maximum allowed depth
        max_depth: u32,
    },

    /// SSA graph has too many nodes.
    #[error("SSA graph too large: {nodes} nodes exceeds limit {max_nodes}")]
    SsaTooLarge {
        /// Actual number of nodes
        nodes: u32,
        /// Maximum allowed nodes
        max_nodes: u32,
    },

    /// Slice computation exceeded depth limit.
    #[error("slice computation exceeded depth limit of {max_depth}")]
    SliceDepthExceeded {
        /// Maximum allowed depth
        max_depth: u32,
    },

    /// Invalid function name.
    #[error("invalid function name: {reason}")]
    InvalidFunctionName {
        /// Why the name is invalid
        reason: String,
    },

    /// Path traversal attempt detected.
    #[error("path traversal blocked: {} attempts to escape project root", path.display())]
    PathTraversal {
        /// Suspicious path
        path: PathBuf,
    },

    /// Generic IO error.
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    /// JSON serialization/deserialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Result type for contracts commands.
pub type ContractsResult<T> = Result<T, ContractsError>;

// =============================================================================
// Error Construction Helpers
// =============================================================================

impl ContractsError {
    /// Create a FileNotFound error.
    pub fn file_not_found(path: impl Into<PathBuf>) -> Self {
        Self::FileNotFound { path: path.into() }
    }

    /// Create a FunctionNotFound error.
    pub fn function_not_found(function: impl Into<String>, file: impl Into<PathBuf>) -> Self {
        Self::FunctionNotFound {
            function: function.into(),
            file: file.into(),
        }
    }

    /// Create a ParseError.
    pub fn parse_error(file: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self::ParseError {
            file: file.into(),
            message: message.into(),
        }
    }

    /// Create an SsaError.
    pub fn ssa_error(message: impl Into<String>) -> Self {
        Self::SsaError(message.into())
    }

    /// Create a LineOutsideFunction error.
    pub fn line_outside_function(
        line: u32,
        function: impl Into<String>,
        start: u32,
        end: u32,
    ) -> Self {
        Self::LineOutsideFunction {
            line,
            function: function.into(),
            start,
            end,
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

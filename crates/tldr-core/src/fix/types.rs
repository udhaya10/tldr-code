//! Types for the `tldr fix` diagnostic and auto-fix system.
//!
//! These types model the full lifecycle of error diagnosis:
//! - `ParsedError`: Structured representation of an error from compiler/runtime output
//! - `Diagnosis`: Result of analyzing an error, with optional fix
//! - `Fix`: A set of text edits that resolve the error
//! - `TextEdit`: A single edit operation on source text

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A parsed error extracted from compiler/runtime output.
///
/// This is the normalized representation of an error regardless of the
/// source format (Python traceback, rustc JSON, tsc line format, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedError {
    /// The error type/class (e.g., "UnboundLocalError", "E0599", "TS2304")
    pub error_type: String,
    /// The error message text
    pub message: String,
    /// Source file where the error occurred
    pub file: Option<PathBuf>,
    /// Line number (1-indexed)
    pub line: Option<usize>,
    /// Column number (0-indexed)
    pub column: Option<usize>,
    /// Detected or specified language
    pub language: String,
    /// The raw error text before parsing
    pub raw_text: String,
    /// The function name where the error occurred (extracted from traceback)
    pub function_name: Option<String>,
    /// The offending source line from the traceback
    pub offending_line: Option<String>,
}

/// Result of analyzing an error.
///
/// Contains the diagnosis explanation, confidence level, and an optional
/// fix that can be applied to resolve the error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnosis {
    /// Language the error was in
    pub language: String,
    /// Error code (e.g., "UnboundLocalError", "E0599", "TS2304")
    pub error_code: String,
    /// Human-readable explanation of what went wrong
    pub message: String,
    /// Source file and line where the error occurred
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<FixLocation>,
    /// Confidence that the fix will resolve the error
    pub confidence: FixConfidence,
    /// The fix to apply (None means cannot fix, escalate to a model)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<Fix>,
}

/// A source location for fix diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixLocation {
    /// Source file path
    pub file: PathBuf,
    /// Line number (1-indexed)
    pub line: usize,
    /// Column number (0-indexed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
}

/// A fix consisting of one or more text edits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fix {
    /// What the fix does (human-readable)
    pub description: String,
    /// The edits to apply
    pub edits: Vec<TextEdit>,
}

/// A single text edit operation on source code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextEdit {
    /// Line number (1-indexed)
    pub line: usize,
    /// Column (0-indexed, for range operations)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    /// Kind of edit
    pub kind: EditKind,
    /// New text to insert or replace with
    pub new_text: String,
}

/// The kind of text edit operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EditKind {
    /// Insert text as a new line before the specified line
    InsertBefore,
    /// Insert text as a new line after the specified line
    InsertAfter,
    /// Replace the entire line
    ReplaceLine,
    /// Delete the line entirely (removes it from output, unlike ReplaceLine
    /// with empty string which leaves a blank line)
    DeleteLine,
    /// Replace a specific column range on the line
    ReplaceRange {
        /// Start column (0-indexed, inclusive)
        start_col: usize,
        /// End column (0-indexed, exclusive)
        end_col: usize,
    },
}

/// Confidence level for a fix.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FixConfidence {
    /// Fix is deterministic and proven correct for this error pattern
    High,
    /// Fix is likely correct but has edge cases
    Medium,
    /// Fix is a guess -- escalate to model if possible
    Low,
}

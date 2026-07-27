//! Types for bugbot analysis reports
//!
//! All types derive Serialize and Deserialize for JSON output compatibility
//! with the OutputWriter system.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use super::tools::ToolResult;

/// Exit status from bugbot check, used to propagate exit codes without
/// calling `process::exit` (which skips Drop destructors and is untestable).
#[derive(Debug)]
pub enum BugbotExitError {
    /// Findings were detected and `--no-fail` was not set.
    FindingsDetected {
        /// Number of findings in the report.
        count: usize,
    },
    /// Critical findings detected — highest priority, exit code 3.
    CriticalFindings {
        /// Number of critical findings in the report.
        count: usize,
    },
    /// Analysis pipeline encountered errors but produced no findings.
    /// A broken pipeline should not report "clean."
    AnalysisErrors {
        /// Number of non-fatal errors encountered.
        count: usize,
    },
}

impl BugbotExitError {
    /// Return the process exit code for this error.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::FindingsDetected { .. } => 1,
            Self::AnalysisErrors { .. } => 2,
            Self::CriticalFindings { .. } => 3,
        }
    }
}

impl fmt::Display for BugbotExitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FindingsDetected { count } => {
                write!(f, "bugbot: {} finding(s) detected", count)
            }
            Self::CriticalFindings { count } => {
                write!(f, "bugbot: {} CRITICAL finding(s) detected", count)
            }
            Self::AnalysisErrors { count } => {
                write!(
                    f,
                    "bugbot: analysis had {} error(s) with no findings",
                    count
                )
            }
        }
    }
}

impl std::error::Error for BugbotExitError {}

/// Top-level report output from bugbot check
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BugbotCheckReport {
    /// Always "bugbot"
    pub tool: String,
    /// Always "check"
    pub mode: String,
    /// Language detected or specified
    pub language: String,
    /// Git base reference (e.g. "HEAD", "main")
    pub base_ref: String,
    /// How changes were detected (e.g. "git:uncommitted", "git:staged")
    pub detection_method: String,
    /// ISO 8601 timestamp
    pub timestamp: String,
    /// Files that had changes
    pub changed_files: Vec<PathBuf>,
    /// The actual findings
    pub findings: Vec<BugbotFinding>,
    /// Summary statistics
    pub summary: BugbotSummary,
    /// Pipeline timing in milliseconds
    pub elapsed_ms: u64,
    /// Non-fatal errors encountered
    pub errors: Vec<String>,
    /// Informational notes (e.g. "stub_implementation", "no_changes_detected", "truncated_to_50")
    pub notes: Vec<String>,
    /// Tool execution results (L1)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_results: Vec<ToolResult>,
    /// Tools that were available to run
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools_available: Vec<String>,
    /// Tools that were not found
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools_missing: Vec<String>,
    /// L2 engine execution results
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub l2_engine_results: Vec<L2AnalyzerResult>,
}

/// A single finding from bugbot analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BugbotFinding {
    /// e.g. "signature-regression", "born-dead"
    pub finding_type: String,
    /// "high", "medium", "low"
    pub severity: String,
    /// File path (relative to project root)
    pub file: PathBuf,
    /// Function/method name
    pub function: String,
    /// Line number in current file
    pub line: usize,
    /// Human-readable description
    pub message: String,
    /// Type-specific evidence
    pub evidence: serde_json::Value,
    /// Confidence level (L2/L3 only). L1 findings leave this as None.
    /// Values: "CONFIRMED", "LIKELY", "POSSIBLE", "FALSE_POSITIVE"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    /// Deterministic finding ID for cross-run tracking.
    /// Hash of (finding_type, file, function, line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finding_id: Option<String>,
}

/// Per-engine execution result for the report.
/// Mirrors ToolResult for L1 tools, providing identical observability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L2AnalyzerResult {
    /// Engine name (e.g. "DeltaEngine", "FlowEngine")
    pub name: String,
    /// Whether the engine completed fully
    pub success: bool,
    /// Execution time in milliseconds
    pub duration_ms: u64,
    /// Number of findings produced
    pub finding_count: usize,
    /// Number of functions analyzed
    pub functions_analyzed: usize,
    /// Number of functions skipped
    pub functions_skipped: usize,
    /// Engine completion status description
    pub status: String,
    /// Errors encountered (empty if success)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// Summary statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BugbotSummary {
    /// Total number of findings
    pub total_findings: usize,
    /// Findings grouped by severity
    pub by_severity: HashMap<String, usize>,
    /// Findings grouped by finding type
    pub by_type: HashMap<String, usize>,
    /// Number of files analyzed
    pub files_analyzed: usize,
    /// Number of functions analyzed
    pub functions_analyzed: usize,
    /// L1 tool-based findings count
    #[serde(default)]
    pub l1_findings: usize,
    /// L2 AST-based findings count
    #[serde(default)]
    pub l2_findings: usize,
    /// Number of tools that ran
    #[serde(default)]
    pub tools_run: usize,
    /// Number of tools that failed
    #[serde(default)]
    pub tools_failed: usize,
}

//! L2 data types for bugbot analysis pipeline.
//!
//! Contains all types used by L2 analyzers: output containers, function
//! identifiers, structured errors, pipeline modes, and the finding store trait.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

use super::super::types::BugbotFinding;

/// Rich return type from L2 analysis engines.
///
/// Captures findings, status, timing, and function-level statistics
/// for a single analyzer run.
#[derive(Debug, Clone)]
pub struct L2AnalyzerOutput {
    /// Findings produced by this analyzer.
    pub findings: Vec<BugbotFinding>,
    /// Whether the analyzer completed fully, partially, or was skipped.
    pub status: AnalyzerStatus,
    /// Wall-clock time spent in this analyzer, in milliseconds.
    pub duration_ms: u64,
    /// Number of functions that were successfully analyzed.
    pub functions_analyzed: usize,
    /// Number of functions that were skipped (e.g. too complex, unsupported).
    pub functions_skipped: usize,
}

/// Status of an analyzer run.
///
/// Tracks whether an analyzer completed all work or encountered issues
/// that limited its coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnalyzerStatus {
    /// All target functions were analyzed without errors.
    Complete,
    /// Some functions were analyzed but others were skipped.
    Partial {
        /// Human-readable explanation of why analysis was partial.
        reason: String,
    },
    /// The analyzer was entirely skipped (e.g. wrong language, no functions).
    Skipped {
        /// Human-readable explanation of why the analyzer was skipped.
        reason: String,
    },
    /// The analyzer exceeded its time budget.
    TimedOut {
        /// Number of findings produced before the timeout.
        partial_findings: usize,
    },
}

/// Unique identifier for a function within the project.
///
/// Combines file path, qualified name, and definition line to
/// unambiguously identify a function even when multiple functions
/// share the same name across modules.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct FunctionId {
    /// Source file containing the function definition.
    pub file: PathBuf,
    /// Fully qualified name (e.g. `MyStruct::method`).
    pub qualified_name: String,
    /// Line number where the function definition starts (1-based).
    pub def_line: usize,
}

impl FunctionId {
    /// Create a new `FunctionId`.
    pub fn new(
        file: impl Into<PathBuf>,
        qualified_name: impl Into<String>,
        def_line: usize,
    ) -> Self {
        Self {
            file: file.into(),
            qualified_name: qualified_name.into(),
            def_line,
        }
    }
}

impl fmt::Display for FunctionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}",
            self.file.display(),
            self.def_line,
            self.qualified_name,
        )
    }
}

/// Structured error from an L2 analyzer.
///
/// Provides enough context to log actionable diagnostics without
/// aborting the entire pipeline. Includes the analyzer name, optional
/// file/function context, and a list of finding types that were
/// skipped as a result of the error.
#[derive(Debug)]
pub struct AnalyzerError {
    /// Name of the analyzer that produced this error (e.g. "dead-code", "taint").
    pub analyzer: &'static str,
    /// File being analyzed when the error occurred, if applicable.
    pub file: Option<PathBuf>,
    /// Function being analyzed when the error occurred, if applicable.
    pub function: Option<String>,
    /// The underlying error.
    pub cause: anyhow::Error,
    /// Finding types that could not be produced due to this error.
    pub skipped_finding_types: Vec<&'static str>,
    /// Whether the error is transient (e.g. timeout) vs permanent (e.g. parse failure).
    pub is_transient: bool,
}

impl fmt::Display for AnalyzerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "analyzer '{}' failed", self.analyzer)?;
        if let Some(ref file) = self.file {
            write!(f, " on {}", file.display())?;
        }
        if let Some(ref function) = self.function {
            write!(f, " in {}", function)?;
        }
        write!(f, ": {}", self.cause)
    }
}

impl std::error::Error for AnalyzerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause.source()
    }
}

/// Determines how the L2 pipeline is invoked.
///
/// `Check` mode analyzes only changed functions (the default `bugbot check` path).
/// `Scan` mode analyzes an entire project (stub for future `bugbot scan`).
#[derive(Debug)]
pub enum PipelineMode {
    /// Analyze only functions affected by recent changes.
    Check(CheckContext),
    /// Analyze all functions in a project (future).
    Scan(ScanContext),
}

/// Context for `Check` mode.
///
/// Marker struct -- the real per-run fields live on L2Context, which
/// is constructed by the pipeline orchestrator.
#[derive(Debug, Clone)]
pub struct CheckContext;

/// Context for `Scan` mode.
///
/// Carries the project root, detected language, and full file list
/// needed to scan an entire codebase.
#[derive(Debug, Clone)]
pub struct ScanContext {
    /// Root directory of the project being scanned.
    pub project: PathBuf,
    /// Primary language of the project.
    pub language: tldr_core::Language,
    /// All source files to analyze.
    pub all_files: Vec<PathBuf>,
}

/// Persistence layer for findings.
///
/// Allows the pipeline to record findings, check suppression status,
/// and retrieve false-positive rates for adaptive thresholding.
/// The trait is object-safe to allow `Box<dyn FindingStore>`.
pub trait FindingStore: Send + Sync {
    /// Record a batch of findings (e.g. persist to disk or database).
    fn record_findings(&self, findings: &[BugbotFinding]) -> anyhow::Result<()>;
    /// Check whether a specific finding has been suppressed by the user.
    fn was_suppressed(&self, finding_id: &str) -> bool;
    /// Return the historical false-positive rate for a finding type (0.0..1.0).
    fn false_positive_rate(&self, finding_type: &str) -> f64;
}

/// No-op implementation of [`FindingStore`] for use in tests and
/// single-run modes where persistence is not needed.
pub struct NoOpFindingStore;

impl FindingStore for NoOpFindingStore {
    fn record_findings(&self, _findings: &[BugbotFinding]) -> anyhow::Result<()> {
        Ok(())
    }

    fn was_suppressed(&self, _finding_id: &str) -> bool {
        false
    }

    fn false_positive_rate(&self, _finding_type: &str) -> f64 {
        0.0
    }
}

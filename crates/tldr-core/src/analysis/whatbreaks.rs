//! Whatbreaks analysis - unified impact analysis wrapper
//!
//! This module provides the `whatbreaks` command which auto-detects
//! the target type and runs appropriate sub-analyses.
//!
//! ## Target Types
//!
//! - **Function**: Runs `impact` analysis to find callers
//! - **File**: Runs `importers` + `change-impact` analysis
//! - **Module**: Runs `importers` analysis
//!
//! ## Target Detection Algorithm (T15 mitigation)
//!
//! 1. If target is an existing file path -> File
//! 2. If target contains "/" or ends with language extension -> File
//! 3. If target contains "." and first part is a directory -> Module
//! 4. Default to Function
//!
//! ## Partial Failure Handling (T29 mitigation)
//!
//! Each sub-analysis is wrapped in `SubResult` which captures:
//! - Success/failure status
//! - Data on success
//! - Error message on failure
//! - Elapsed time
//!
//! If one sub-analysis fails, others continue to run.
//!
//! ## Error Mapping (T11 mitigation)
//!
//! Sub-command errors are mapped as follows:
//! - `impact` FunctionNotFound -> SubResult { success: false, error: "..." }
//! - `importers` empty list -> SubResult { success: true, data: [] }
//! - `change-impact` no tests -> SubResult { success: true, data: [] }
//!
//! # Example
//!
//! ```rust,ignore
//! use tldr_core::analysis::whatbreaks::{whatbreaks_analysis, WhatbreaksOptions};
//! use tldr_core::Language;
//! use std::path::Path;
//!
//! let options = WhatbreaksOptions::default();
//! let report = whatbreaks_analysis(
//!     "process_data",
//!     Path::new("src"),
//!     &options,
//! )?;
//!
//! println!("Target type: {:?}", report.target_type);
//! println!("Direct callers: {}", report.summary.direct_caller_count);
//! ```

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::analysis::change_impact::change_impact;
use crate::analysis::clones::is_test_file;
use crate::analysis::impact::impact_analysis_with_ast_fallback;
use crate::analysis::importers::find_importers;
use crate::callgraph::build_project_call_graph;
use crate::types::{Language, ProjectCallGraph};
use crate::TldrResult;

// =============================================================================
// Types
// =============================================================================

/// Detected type of whatbreaks target (T15 mitigation)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetType {
    /// Target is a function name - run impact analysis
    Function,
    /// Target is a file path - run importers + change-impact
    File,
    /// Target is a module name - run importers
    Module,
}

impl std::fmt::Display for TargetType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TargetType::Function => write!(f, "function"),
            TargetType::File => write!(f, "file"),
            TargetType::Module => write!(f, "module"),
        }
    }
}

/// Status of a sub-analysis (T29 mitigation)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubStatus {
    /// Analysis completed successfully
    Success,
    /// Analysis failed with an error
    Error,
    /// Analysis was skipped (e.g., not applicable for target type)
    Skipped,
}

/// Result of a sub-analysis with error handling (T11, T29 mitigation)
///
/// Wraps individual sub-analysis results to allow partial failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubResult {
    /// Whether the analysis succeeded
    pub success: bool,
    /// Analysis data on success (serialized to JSON Value for flexibility)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Error message on failure
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Warnings that don't prevent success (T29 mitigation)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Whether the data is incomplete/partial
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
    /// Time taken for this sub-analysis in milliseconds
    pub elapsed_ms: f64,
}

impl SubResult {
    /// Create a successful result with data
    pub fn success<T: Serialize>(data: T, elapsed_ms: f64) -> Self {
        Self {
            success: true,
            data: Some(serde_json::to_value(data).unwrap_or(serde_json::Value::Null)),
            error: None,
            warnings: Vec::new(),
            partial: false,
            elapsed_ms,
        }
    }

    /// Create a failed result with error
    pub fn error(error: String, elapsed_ms: f64) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(error),
            warnings: Vec::new(),
            partial: false,
            elapsed_ms,
        }
    }

    /// Create a skipped result
    pub fn skipped(reason: &str) -> Self {
        Self {
            success: true,
            data: None,
            error: None,
            warnings: vec![reason.to_string()],
            partial: false,
            elapsed_ms: 0.0,
        }
    }

    /// Create a partial success result
    pub fn partial<T: Serialize>(data: T, warnings: Vec<String>, elapsed_ms: f64) -> Self {
        Self {
            success: true,
            data: Some(serde_json::to_value(data).unwrap_or(serde_json::Value::Null)),
            error: None,
            warnings,
            partial: true,
            elapsed_ms,
        }
    }

    /// Add a warning to the result
    pub fn with_warning(mut self, warning: String) -> Self {
        self.warnings.push(warning);
        self
    }
}

/// Summary statistics aggregated across sub-analyses
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WhatbreaksSummary {
    /// Number of direct callers (from impact analysis)
    pub direct_caller_count: usize,
    /// Number of transitive callers (from impact analysis)
    pub transitive_caller_count: usize,
    /// Number of files importing the target (from importers analysis)
    pub importer_count: usize,
    /// Number of test files affected (from change-impact analysis)
    pub affected_test_count: usize,
}

/// Full whatbreaks analysis report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhatbreaksReport {
    /// Always "whatbreaks" - identifies this as a whatbreaks wrapper report
    pub wrapper: String,
    /// Project path analyzed
    pub path: PathBuf,
    /// Target that was analyzed
    pub target: String,
    /// Detected or forced target type
    pub target_type: TargetType,
    /// Explanation of why this target type was chosen (T15 mitigation)
    pub detection_reason: String,
    /// Results from each sub-analysis
    pub sub_results: HashMap<String, SubResult>,
    /// Aggregated summary statistics
    pub summary: WhatbreaksSummary,
    /// Total time for all analyses in milliseconds
    pub total_elapsed_ms: f64,
}

/// Options for whatbreaks analysis
#[derive(Debug, Clone)]
pub struct WhatbreaksOptions {
    /// Maximum depth for impact traversal
    pub depth: usize,
    /// Skip slow analyses (e.g., diff-impact)
    pub quick: bool,
    /// Programming language (None = auto-detect)
    pub language: Option<Language>,
    /// Force target type (None = auto-detect)
    pub force_type: Option<TargetType>,
}

impl Default for WhatbreaksOptions {
    fn default() -> Self {
        Self {
            depth: 3,
            quick: false,
            language: None,
            force_type: None,
        }
    }
}

// =============================================================================
// Target Detection (T15 mitigation)
// =============================================================================

/// Detect the type of target based on its pattern and project structure
///
/// Returns (TargetType, detection_reason) where detection_reason explains
/// why this type was chosen (for debugging and user clarity).
///
/// # Algorithm
///
/// 1. If target is an existing file path -> File
/// 2. If target contains "/" or ends with language extension -> File
/// 3. If target contains "::" or "." suggesting qualified name:
///    a. If first part before "." is a directory -> Module
///    b. Otherwise -> Function (qualified name like Class.method)
/// 4. Default to Function
///
/// # Arguments
///
/// * `target` - The target string from user input
/// * `project_path` - Project root directory
///
/// # Returns
///
/// (TargetType, String) - The detected type and explanation
pub fn detect_target_type(target: &str, project_path: &Path) -> (TargetType, String) {
    // Check 1: Is it an existing file path?
    let target_path = if Path::new(target).is_absolute() {
        PathBuf::from(target)
    } else {
        project_path.join(target)
    };

    if target_path.is_file() {
        return (
            TargetType::File,
            format!("Path '{}' exists as a file", target),
        );
    }

    // Check 2: Does it look like a file path?
    if target.contains('/') || target.contains('\\') {
        return (
            TargetType::File,
            format!("Path '{}' contains path separator", target),
        );
    }

    // Check for file extensions
    let file_extensions = [".py", ".ts", ".js", ".tsx", ".jsx", ".go", ".rs"];
    for ext in &file_extensions {
        if target.ends_with(ext) {
            return (
                TargetType::File,
                format!("Target '{}' ends with file extension '{}'", target, ext),
            );
        }
    }

    // Check 3: Is it a directory (module)?
    if target_path.is_dir() {
        return (
            TargetType::Module,
            format!("Path '{}' exists as a directory", target),
        );
    }

    // Check 4: Qualified name with dots
    if target.contains('.') {
        let parts: Vec<&str> = target.split('.').collect();
        let first_part = parts[0];

        // Check if the first part is a directory (module pattern)
        let first_part_path = project_path.join(first_part);
        if first_part_path.is_dir() {
            return (
                TargetType::Module,
                format!(
                    "First part '{}' of '{}' is a directory (module pattern)",
                    first_part, target
                ),
            );
        }

        // Otherwise it's likely a qualified function name like Class.method
        return (
            TargetType::Function,
            format!(
                "Target '{}' contains '.' but first part '{}' is not a directory (qualified function name)",
                target, first_part
            ),
        );
    }

    // Check 5: Rust-style qualified name
    if target.contains("::") {
        return (
            TargetType::Function,
            format!(
                "Target '{}' contains '::' (qualified function name)",
                target
            ),
        );
    }

    // Default: assume function
    (
        TargetType::Function,
        format!(
            "Target '{}' does not match file or module patterns (defaulting to function)",
            target
        ),
    )
}

// =============================================================================
// Sub-Analysis Execution
// =============================================================================

/// Run impact analysis for a function target
fn run_impact_analysis(
    target: &str,
    project_path: &Path,
    call_graph: &ProjectCallGraph,
    depth: usize,
    language: Language,
) -> SubResult {
    let start = Instant::now();

    match impact_analysis_with_ast_fallback(call_graph, target, depth, None, project_path, language)
    {
        Ok(mut report) => {
            // sibling-resolver-gaps-v1 (P14.AGG14-4): mirror the
            // user-facing `tldr impact` references-enrichment so
            // `whatbreaks` finds the same callers. Without this,
            // `whatbreaks WriteToken` reports `caller_count = 0`
            // ("Entry point") for csharp/kotlin/java cases that the
            // call-graph alone misses but `find_references` resolves.
            crate::analysis::impact::enrich_impact_with_references(
                &mut report,
                project_path,
                target,
                language,
                depth,
            );

            // Count direct callers from all targets
            let direct_count: usize = report.targets.values().map(|t| t.caller_count).sum();

            // Count transitive callers (all callers in the tree)
            let transitive_count: usize =
                report.targets.values().map(count_transitive_callers).sum();

            // VAL-002 (#1.E): collect unique test-file paths across all caller
            // trees so the Function-target branch in `whatbreaks_analysis` can
            // populate `summary.affected_test_count`. Walking the typed
            // ImpactReport here matches `transitive_count`'s depth semantics
            // exactly (same tree, same nodes). De-duped via HashSet<PathBuf>.
            let mut test_files: HashSet<PathBuf> = HashSet::new();
            for tree in report.targets.values() {
                collect_test_files_from_tree(tree, &mut test_files);
            }
            let test_file_count = test_files.len();

            SubResult::success(
                serde_json::json!({
                    "targets": report.targets.len(),
                    "direct_callers": direct_count,
                    "transitive_callers": transitive_count,
                    "affected_test_count": test_file_count,
                    "report": report,
                }),
                start.elapsed().as_secs_f64() * 1000.0,
            )
        }
        Err(e) => SubResult::error(e.to_string(), start.elapsed().as_secs_f64() * 1000.0),
    }
}

/// Count transitive callers in a caller tree (recursive)
fn count_transitive_callers(tree: &crate::types::CallerTree) -> usize {
    let mut count = tree.caller_count;
    for caller in &tree.callers {
        count += count_transitive_callers(caller);
    }
    count
}

/// Recursively walk a caller tree and accumulate unique test-file paths into
/// `acc`. A node is considered a test file iff
/// [`crate::analysis::clones::is_test_file`] returns true for `tree.file`.
///
/// Used by [`run_impact_analysis`] (VAL-002) to compute the
/// `affected_test_count` JSON field that
/// [`whatbreaks_analysis`]'s Function-target branch reads back into
/// [`WhatbreaksSummary::affected_test_count`].
fn collect_test_files_from_tree(tree: &crate::types::CallerTree, acc: &mut HashSet<PathBuf>) {
    if is_test_file(&tree.file) {
        acc.insert(tree.file.clone());
    }
    for child in &tree.callers {
        collect_test_files_from_tree(child, acc);
    }
}

/// Run importers analysis for a file or module target
fn run_importers_analysis(target: &str, project_path: &Path, language: Language) -> SubResult {
    let start = Instant::now();

    // Derive module name from target
    let module_name = derive_module_name(target);

    match find_importers(project_path, &module_name, language) {
        Ok(report) => SubResult::success(
            serde_json::json!({
                "module": report.module,
                "importers": report.importers,
                "count": report.total,
            }),
            start.elapsed().as_secs_f64() * 1000.0,
        ),
        Err(e) => SubResult::error(e.to_string(), start.elapsed().as_secs_f64() * 1000.0),
    }
}

/// Run change-impact analysis for a file target
fn run_change_impact_analysis(target: &str, project_path: &Path, language: Language) -> SubResult {
    let start = Instant::now();

    // For file targets, provide the file as an explicit changed file
    let target_path = if Path::new(target).is_absolute() {
        PathBuf::from(target)
    } else {
        project_path.join(target)
    };

    let changed_files = if target_path.exists() {
        Some(vec![target_path])
    } else {
        None
    };

    match change_impact(project_path, changed_files.as_deref(), language) {
        Ok(report) => SubResult::success(
            serde_json::json!({
                "changed_files": report.changed_files,
                "affected_tests": report.affected_tests,
                "affected_functions": report.affected_functions.len(),
            }),
            start.elapsed().as_secs_f64() * 1000.0,
        ),
        Err(e) => SubResult::error(e.to_string(), start.elapsed().as_secs_f64() * 1000.0),
    }
}

/// Derive module name from a target string
fn derive_module_name(target: &str) -> String {
    // Remove file extension if present
    let without_ext = if let Some(idx) = target.rfind('.') {
        let ext = &target[idx..];
        if [".py", ".ts", ".js", ".go", ".rs"].contains(&ext) {
            &target[..idx]
        } else {
            target
        }
    } else {
        target
    };

    // Replace path separators with dots for module notation
    without_ext.replace(['/', '\\'], ".")
}

// =============================================================================
// Main Analysis Function
// =============================================================================

/// Run whatbreaks analysis on a target
///
/// This is the main entry point for the whatbreaks command.
/// It auto-detects (or uses forced) target type and runs appropriate sub-analyses.
///
/// # Arguments
///
/// * `target` - Target to analyze (function name, file path, or module name)
/// * `project_path` - Project root directory
/// * `options` - Analysis options
///
/// # Returns
///
/// * `Ok(WhatbreaksReport)` - Complete analysis report with sub-results
/// * `Err(TldrError)` - If fundamental analysis fails (e.g., can't build call graph)
///
/// # Example
///
/// ```rust,ignore
/// let report = whatbreaks_analysis("process_data", Path::new("src"), &options)?;
/// ```
pub fn whatbreaks_analysis(
    target: &str,
    project_path: &Path,
    options: &WhatbreaksOptions,
) -> TldrResult<WhatbreaksReport> {
    let total_start = Instant::now();

    // Determine target type
    let (target_type, detection_reason) = if let Some(forced_type) = options.force_type {
        (
            forced_type,
            format!("Forced via --type flag to {:?}", forced_type),
        )
    } else {
        detect_target_type(target, project_path)
    };

    // Detect language (auto-detect from directory, default to Python)
    let language = options
        .language
        .unwrap_or_else(|| Language::from_directory(project_path).unwrap_or(Language::Python));

    // Build call graph (needed for function targets, optional for others)
    let call_graph = match target_type {
        TargetType::Function => {
            // Must build call graph for impact analysis
            build_project_call_graph(project_path, language, None, true)?
        }
        _ => {
            // Try to build but don't fail if it doesn't work
            build_project_call_graph(project_path, language, None, true)
                .unwrap_or_else(|_| ProjectCallGraph::new())
        }
    };

    // Run sub-analyses based on target type
    let mut sub_results: HashMap<String, SubResult> = HashMap::new();
    let mut summary = WhatbreaksSummary::default();

    match target_type {
        TargetType::Function => {
            // Run impact analysis
            let impact_result =
                run_impact_analysis(target, project_path, &call_graph, options.depth, language);

            // Extract counts from successful result
            if impact_result.success {
                if let Some(data) = &impact_result.data {
                    if let Some(direct) = data.get("direct_callers").and_then(|v| v.as_u64()) {
                        summary.direct_caller_count = direct as usize;
                    }
                    if let Some(transitive) =
                        data.get("transitive_callers").and_then(|v| v.as_u64())
                    {
                        summary.transitive_caller_count = transitive as usize;
                    }
                    // VAL-002 (#1.E): read the test-file count emitted by
                    // run_impact_analysis. Mirrors the direct/transitive
                    // pattern above. The File-target branch populates this
                    // same field via change_impact, but the Function path
                    // had been silently leaving it at the
                    // WhatbreaksSummary::default() value of 0.
                    if let Some(test_count) =
                        data.get("affected_test_count").and_then(|v| v.as_u64())
                    {
                        summary.affected_test_count = test_count as usize;
                    }
                }
            }

            sub_results.insert("impact".to_string(), impact_result);
        }

        TargetType::File => {
            // Run importers analysis
            let importers_result = run_importers_analysis(target, project_path, language);

            if importers_result.success {
                if let Some(data) = &importers_result.data {
                    if let Some(count) = data.get("count").and_then(|v| v.as_u64()) {
                        summary.importer_count = count as usize;
                    }
                }
            }

            sub_results.insert("importers".to_string(), importers_result);

            // Run change-impact analysis (unless --quick)
            if !options.quick {
                let change_impact_result =
                    run_change_impact_analysis(target, project_path, language);

                if change_impact_result.success {
                    if let Some(data) = &change_impact_result.data {
                        if let Some(tests) = data.get("affected_tests").and_then(|v| v.as_array()) {
                            summary.affected_test_count = tests.len();
                        }
                    }
                }

                sub_results.insert("change-impact".to_string(), change_impact_result);
            } else {
                sub_results.insert(
                    "change-impact".to_string(),
                    SubResult::skipped("Skipped due to --quick flag"),
                );
            }
        }

        TargetType::Module => {
            // Run importers analysis
            let importers_result = run_importers_analysis(target, project_path, language);

            if importers_result.success {
                if let Some(data) = &importers_result.data {
                    if let Some(count) = data.get("count").and_then(|v| v.as_u64()) {
                        summary.importer_count = count as usize;
                    }
                }
            }

            sub_results.insert("importers".to_string(), importers_result);
        }
    }

    let total_elapsed_ms = total_start.elapsed().as_secs_f64() * 1000.0;

    Ok(WhatbreaksReport {
        wrapper: "whatbreaks".to_string(),
        path: project_path.to_path_buf(),
        target: target.to_string(),
        target_type,
        detection_reason,
        sub_results,
        summary,
        total_elapsed_ms,
    })
}

// =============================================================================
// Tests
// =============================================================================

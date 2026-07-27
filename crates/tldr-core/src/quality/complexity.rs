//! Complexity Analyzer for Health Command
//!
//! This module provides project-wide complexity analysis with hotspot detection.
//! It wraps the existing cyclomatic complexity calculation from metrics/complexity.rs
//! and adds hotspot detection (functions with CC > threshold).
//!
//! # Features
//! - Project-wide complexity scanning
//! - Hotspot detection (CC > configurable threshold, default 10)
//! - Per-function complexity data with ranking
//! - Multi-language support
//!
//! # Example
//!
//! ```ignore
//! use tldr_core::quality::complexity::{analyze_complexity, ComplexityOptions};
//! use std::path::Path;
//!
//! let report = analyze_complexity(Path::new("src/"), None, None)?;
//! println!("Hotspots: {}", report.hotspot_count);
//! println!("Avg CC: {:.2}", report.avg_cyclomatic);
//! ```

use std::path::{Path, PathBuf};

use crate::walker::walk_project;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ast::count::count_functions_canonical;
use crate::ast::extract::extract_file;
use crate::error::TldrError;
use crate::metrics::calculate_all_complexities_file;
use crate::types::Language;
use crate::TldrResult;

// =============================================================================
// Types
// =============================================================================

/// A function identified as a complexity hotspot (CC > threshold)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityHotspot {
    /// Function name
    pub name: String,
    /// File path containing the function
    pub file: PathBuf,
    /// Line number where the function starts
    pub line: usize,
    /// Cyclomatic complexity
    pub cyclomatic: usize,
    /// Cognitive complexity (optional, for future use)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cognitive: Option<usize>,
    /// Lines of code in the function
    pub loc: usize,
    /// Rank by complexity (1 = highest complexity)
    pub rank: usize,
}

/// Per-function complexity data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionComplexity {
    /// Function name
    pub name: String,
    /// File path containing the function
    pub file: PathBuf,
    /// Line number where the function starts
    pub line: usize,
    /// Cyclomatic complexity
    pub cyclomatic: usize,
    /// Cognitive complexity
    pub cognitive: usize,
    /// Lines of code in the function
    pub loc: usize,
    /// Rank by cyclomatic complexity (1 = highest)
    pub rank: usize,
    /// Whether this function is a hotspot
    pub is_hotspot: bool,
}

/// Summary statistics for complexity analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexitySummary {
    /// Total number of functions analyzed
    pub total_functions: usize,
    /// Average cyclomatic complexity
    pub avg_cyclomatic: f64,
    /// Maximum cyclomatic complexity found
    pub max_cyclomatic: usize,
    /// Number of functions exceeding the hotspot threshold
    pub hotspot_count: usize,
    /// Total lines of code across all functions
    pub total_loc: usize,
}

impl Default for ComplexitySummary {
    fn default() -> Self {
        Self {
            total_functions: 0,
            avg_cyclomatic: 0.0,
            max_cyclomatic: 0,
            hotspot_count: 0,
            total_loc: 0,
        }
    }
}

/// Complete complexity analysis report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityReport {
    /// Number of functions analyzed
    pub functions_analyzed: usize,
    /// Average cyclomatic complexity across all functions
    pub avg_cyclomatic: f64,
    /// Maximum cyclomatic complexity found
    pub max_cyclomatic: usize,
    /// Number of functions with CC > threshold
    pub hotspot_count: usize,
    /// List of hotspots sorted by CC descending
    pub hotspots: Vec<ComplexityHotspot>,
    /// All functions with complexity data (sorted by CC descending)
    pub functions: Vec<FunctionComplexity>,
    /// Summary statistics
    pub summary: ComplexitySummary,
}

impl Default for ComplexityReport {
    fn default() -> Self {
        Self {
            functions_analyzed: 0,
            avg_cyclomatic: 0.0,
            max_cyclomatic: 0,
            hotspot_count: 0,
            hotspots: Vec::new(),
            functions: Vec::new(),
            summary: ComplexitySummary::default(),
        }
    }
}

/// Options for complexity analysis
#[derive(Debug, Clone)]
pub struct ComplexityOptions {
    /// Threshold for hotspot detection (default: 10)
    pub hotspot_threshold: usize,
    /// Maximum number of hotspots to return (default: 20)
    pub max_hotspots: usize,
    /// Include cognitive complexity (default: true)
    pub include_cognitive: bool,
}

impl Default for ComplexityOptions {
    fn default() -> Self {
        Self {
            hotspot_threshold: 10,
            max_hotspots: 20,
            include_cognitive: true,
        }
    }
}

// =============================================================================
// Main API
// =============================================================================

/// Analyze cyclomatic complexity across a codebase
///
/// Scans all supported files in the given path, calculates complexity for each
/// function, and identifies hotspots (functions with CC > threshold).
///
/// # Arguments
/// * `path` - Directory or file to analyze
/// * `language` - Optional language filter (auto-detect if None)
/// * `options` - Optional configuration (uses defaults if None)
///
/// # Returns
/// * `Ok(ComplexityReport)` - Report with complexity metrics and hotspots
/// * `Err(TldrError)` - On file system errors
///
/// # Behavior
/// - Empty files return success with zero metrics
/// - Parse errors in individual files are skipped (logged)
/// - Functions sorted by cyclomatic complexity descending
/// - Hotspots filtered by CC > threshold
///
/// # Example
/// ```ignore
/// use tldr_core::quality::complexity::analyze_complexity;
/// use std::path::Path;
///
/// let report = analyze_complexity(Path::new("src/"), None, None)?;
/// for hotspot in &report.hotspots {
///     println!("{}: CC={}", hotspot.name, hotspot.cyclomatic);
/// }
/// ```
pub fn analyze_complexity(
    path: &Path,
    language: Option<Language>,
    options: Option<ComplexityOptions>,
) -> TldrResult<ComplexityReport> {
    let opts = options.unwrap_or_default();

    // Collect files to analyze
    let file_paths: Vec<PathBuf> = if path.is_file() {
        vec![path.to_path_buf()]
    } else {
        walk_project(path)
            .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
            .filter(|e| {
                let detected = Language::from_path(e.path());
                match (detected, language) {
                    (Some(d), Some(l)) => d == l,
                    (Some(_), None) => true,
                    _ => false,
                }
            })
            .map(|e| e.path().to_path_buf())
            .collect()
    };

    // Collect all function complexity data
    let all_functions_nested: Vec<Vec<FunctionComplexity>> = file_paths
        .par_iter()
        .filter_map(|file_path| analyze_file_complexity(file_path, opts.include_cognitive).ok())
        .collect();

    let mut all_functions: Vec<FunctionComplexity> =
        all_functions_nested.into_iter().flatten().collect();

    // Sort by cyclomatic complexity descending
    all_functions.sort_by(|a, b| b.cyclomatic.cmp(&a.cyclomatic));

    // Assign ranks
    for (rank, func) in all_functions.iter_mut().enumerate() {
        func.rank = rank + 1;
    }

    // Mark hotspots
    for func in &mut all_functions {
        func.is_hotspot = func.cyclomatic > opts.hotspot_threshold;
    }

    // Calculate summary statistics
    let total_functions = all_functions.len();
    let total_cc: usize = all_functions.iter().map(|f| f.cyclomatic).sum();
    let total_loc: usize = all_functions.iter().map(|f| f.loc).sum();
    let max_cyclomatic = all_functions.first().map(|f| f.cyclomatic).unwrap_or(0);
    let avg_cyclomatic = if total_functions > 0 {
        total_cc as f64 / total_functions as f64
    } else {
        0.0
    };

    // Extract hotspots
    let hotspots: Vec<ComplexityHotspot> = all_functions
        .iter()
        .filter(|f| f.is_hotspot)
        .take(opts.max_hotspots)
        .map(|f| ComplexityHotspot {
            name: f.name.clone(),
            file: f.file.clone(),
            line: f.line,
            cyclomatic: f.cyclomatic,
            cognitive: if opts.include_cognitive {
                Some(f.cognitive)
            } else {
                None
            },
            loc: f.loc,
            rank: f.rank,
        })
        .collect();

    let hotspot_count = all_functions.iter().filter(|f| f.is_hotspot).count();

    // canonical-function-enumerator-v1: report the canonical function count
    // as `functions_analyzed` so health/structure/dead all agree. The
    // per-function complexity rows (`functions`/`hotspots`) intentionally
    // remain the metrics-derived subset (functions for which cyclomatic
    // metrics could be computed); only the headline count is canonicalized.
    let canonical_lang =
        language.unwrap_or_else(|| Language::from_directory(path).unwrap_or(Language::Python));
    let canonical_count = count_functions_canonical(path, canonical_lang) as usize;
    let report_functions_analyzed = if canonical_count > 0 {
        canonical_count
    } else {
        total_functions
    };

    let summary = ComplexitySummary {
        total_functions: report_functions_analyzed,
        avg_cyclomatic,
        max_cyclomatic,
        hotspot_count,
        total_loc,
    };

    Ok(ComplexityReport {
        functions_analyzed: report_functions_analyzed,
        avg_cyclomatic,
        max_cyclomatic,
        hotspot_count,
        hotspots,
        functions: all_functions,
        summary,
    })
}

/// Analyze complexity of all functions in a single file
///
/// Uses single-pass complexity calculation to avoid re-parsing the file
/// for each function. A file with N functions is parsed twice (once for
/// module structure, once for complexity) instead of N+1 times.
fn analyze_file_complexity(
    file_path: &Path,
    include_cognitive: bool,
) -> TldrResult<Vec<FunctionComplexity>> {
    // Verify this is a supported language before attempting analysis.
    // calculate_all_complexities_file also checks, but this provides a
    // consistent early-exit with the same error type.
    Language::from_path(file_path).ok_or_else(|| {
        TldrError::UnsupportedLanguage(
            file_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("unknown")
                .to_string(),
        )
    })?;

    // Single-pass: parse file once, get all complexities in one AST walk
    let metrics_map = calculate_all_complexities_file(file_path)?;

    // Extract module info for line numbers and class/method structure
    let module = extract_file(file_path, None)?;

    let mut results = Vec::new();

    // Process top-level functions
    for func in &module.functions {
        if let Some(metrics) = metrics_map.get(&func.name) {
            results.push(FunctionComplexity {
                name: func.name.clone(),
                file: file_path.to_path_buf(),
                line: func.line_number as usize,
                cyclomatic: metrics.cyclomatic as usize,
                cognitive: if include_cognitive {
                    metrics.cognitive as usize
                } else {
                    0
                },
                loc: metrics.lines_of_code as usize,
                rank: 0,           // Will be set after sorting
                is_hotspot: false, // Will be set after threshold check
            });
        }
    }

    // Process methods in classes
    for class in &module.classes {
        for method in &class.methods {
            // Skip dunder methods for complexity hotspot analysis
            if method.name.starts_with("__") && method.name.ends_with("__") {
                continue;
            }

            // calculate_all_complexities_file() keys by bare function name
            // (from get_function_name()), not qualified ClassName.method.
            // Look up by bare method name.
            if let Some(metrics) = metrics_map.get(&method.name) {
                results.push(FunctionComplexity {
                    name: format!("{}.{}", class.name, method.name),
                    file: file_path.to_path_buf(),
                    line: method.line_number as usize,
                    cyclomatic: metrics.cyclomatic as usize,
                    cognitive: if include_cognitive {
                        metrics.cognitive as usize
                    } else {
                        0
                    },
                    loc: metrics.lines_of_code as usize,
                    rank: 0,
                    is_hotspot: false,
                });
            }
        }
    }

    Ok(results)
}

// =============================================================================
// Tests
// =============================================================================

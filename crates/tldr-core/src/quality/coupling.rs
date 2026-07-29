//! Coupling Analyzer for Health Command
//!
//! This module provides pairwise module coupling analysis. It analyzes cross-file
//! dependencies through imports and function calls to detect tightly coupled modules
//! that may benefit from refactoring.
//!
//! # Algorithm
//!
//! 1. Build project call graph (cross-file calls)
//! 2. Find all module pairs with cross-file interactions
//! 3. For each pair, compute coupling score based on:
//!    - Cross-module function calls (both directions)
//!    - Shared imports (modules imported by both)
//! 4. Return top N pairs by coupling score
//!
//! # Score Calculation
//!
//! ```text
//! score = normalize(import_count + call_count)
//! ```
//!
//! Where normalization ensures score is in [0.0, 1.0] range.
//!
//! # Verdicts
//!
//! - Loose: score < 0.3
//! - Moderate: 0.3 <= score < 0.6
//! - Tight: score >= 0.6
//!
//! # References
//!
//! - Health spec section 4.5
//! - Premortem T12: Uses rayon for parallelization in similarity (not coupling)

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::walker::walk_project;
use serde::{Deserialize, Serialize};

use crate::analysis::deps::{DepCycle, DepsReport};
use crate::ast::extract::extract_file;
use crate::callgraph::build_project_call_graph;
use crate::types::{CallEdge, Language, ModuleInfo, ProjectCallGraph};
use crate::TldrResult;

// =============================================================================
// Types
// =============================================================================

/// Verdict for coupling between modules
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CouplingVerdict {
    /// Loosely coupled (score < 0.3)
    Loose,
    /// Moderately coupled (0.3 <= score < 0.6)
    Moderate,
    /// Tightly coupled (score >= 0.6)
    Tight,
}

impl CouplingVerdict {
    /// Determine verdict from coupling score
    pub fn from_score(score: f64) -> Self {
        if score < 0.3 {
            CouplingVerdict::Loose
        } else if score < 0.6 {
            CouplingVerdict::Moderate
        } else {
            CouplingVerdict::Tight
        }
    }
}

/// A call site between two modules
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallSite {
    /// Caller function name
    pub caller: String,
    /// Callee function name
    pub callee: String,
    /// Line number of the call (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
}

/// Coupling analysis between two modules
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleCoupling {
    /// First module (source)
    pub source: PathBuf,
    /// Second module (target)
    pub target: PathBuf,
    /// Number of imports from source to target
    pub import_count: usize,
    /// Number of cross-module calls
    pub call_count: usize,
    /// Calls from source to target
    pub calls_source_to_target: Vec<CallSite>,
    /// Calls from target to source
    pub calls_target_to_source: Vec<CallSite>,
    /// Shared imports (modules both import)
    pub shared_imports: Vec<String>,
    /// Coupling score (0.0 - 1.0)
    pub score: f64,
    /// Coupling verdict based on score
    pub verdict: CouplingVerdict,
}

/// Complete coupling analysis report
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CouplingReport {
    /// Number of modules analyzed
    pub modules_analyzed: usize,
    /// Number of module pairs analyzed
    pub pairs_analyzed: usize,
    /// Total cross-file call pairs found
    pub total_cross_file_pairs: usize,
    /// Average coupling score across all pairs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_coupling_score: Option<f64>,
    /// Number of tightly coupled pairs
    pub tight_coupling_count: usize,
    /// Top module pairs by coupling score (descending)
    pub top_pairs: Vec<ModuleCoupling>,
    /// Whether the results were truncated due to max_pairs limit
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    /// Total number of pairs before truncation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_pairs: Option<usize>,
    /// Number of pairs shown (after truncation)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shown_pairs: Option<usize>,
}

/// Options for coupling analysis
#[derive(Debug, Clone)]
pub struct CouplingOptions {
    /// Maximum number of pairs to return (default: 10)
    pub max_pairs: usize,
    /// Tight coupling threshold (default: 0.6)
    pub tight_threshold: f64,
}

impl Default for CouplingOptions {
    fn default() -> Self {
        Self {
            max_pairs: 10,
            tight_threshold: 0.6,
        }
    }
}

// =============================================================================
// Martin Metrics Types (Project-Wide Coupling)
// =============================================================================

/// Martin metrics for a single module (file-level coupling analysis).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MartinModuleMetrics {
    /// Path to the module file.
    pub module: PathBuf,
    /// Afferent coupling: number of modules that depend on this module.
    pub ca: usize,
    /// Efferent coupling: number of modules this module depends on.
    pub ce: usize,
    /// Instability metric: Ce / (Ca + Ce). Range [0.0, 1.0].
    pub instability: f64,
    /// Whether this module participates in a dependency cycle.
    pub in_cycle: bool,
}

/// Summary statistics for Martin metrics analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MartinSummary {
    /// Average instability across all modules.
    pub avg_instability: f64,
    /// Total number of dependency cycles detected.
    pub total_cycles: usize,
    /// Module with lowest instability (most stable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub most_stable: Option<PathBuf>,
    /// Module with highest instability (most unstable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub most_unstable: Option<PathBuf>,
}

impl Default for MartinSummary {
    fn default() -> Self {
        Self {
            avg_instability: 0.0,
            total_cycles: 0,
            most_stable: None,
            most_unstable: None,
        }
    }
}

/// Complete Martin metrics report for a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MartinMetricsReport {
    /// Schema version for forward-compatible JSON output.
    pub schema_version: String,
    /// Number of modules analyzed.
    pub modules_analyzed: usize,
    /// Per-module Martin metrics.
    pub metrics: Vec<MartinModuleMetrics>,
    /// Detected dependency cycles.
    pub cycles: Vec<DepCycle>,
    /// Summary statistics.
    pub summary: MartinSummary,
}

impl Default for MartinMetricsReport {
    fn default() -> Self {
        Self {
            schema_version: "1.0".to_string(),
            modules_analyzed: 0,
            metrics: Vec::new(),
            cycles: Vec::new(),
            summary: MartinSummary::default(),
        }
    }
}

/// Options for Martin metrics computation.
#[derive(Debug, Clone)]
pub struct MartinOptions {
    /// Number of top modules to include in report.
    pub top: usize,
    /// If true, only report modules involved in cycles.
    pub cycles_only: bool,
}

// =============================================================================
// Martin Metrics Pure Functions
// =============================================================================

/// Compute instability metric: Ce / (Ca + Ce).
/// Returns 0.0 for isolated modules (Ca + Ce == 0).
pub fn compute_instability(ca: usize, ce: usize) -> f64 {
    let total = ca + ce;
    if total == 0 {
        0.0
    } else {
        ce as f64 / total as f64
    }
}

/// Compute afferent (Ca) and efferent (Ce) coupling for each module.
/// Self-imports are filtered. Every module appearing as key or target
/// is present in both returned maps.
pub fn compute_ca_ce(
    internal_deps: &BTreeMap<PathBuf, Vec<PathBuf>>,
) -> (HashMap<PathBuf, usize>, HashMap<PathBuf, usize>) {
    let mut ca_map: HashMap<PathBuf, usize> = HashMap::new();
    let mut ce_map: HashMap<PathBuf, usize> = HashMap::new();

    // Initialize all modules (both sources and targets) with 0
    for (source, targets) in internal_deps {
        ca_map.entry(source.clone()).or_insert(0);
        ce_map.entry(source.clone()).or_insert(0);
        for target in targets {
            ca_map.entry(target.clone()).or_insert(0);
            ce_map.entry(target.clone()).or_insert(0);
        }
    }

    // Compute Ce and Ca
    for (source, targets) in internal_deps {
        // Filter self-imports and deduplicate targets
        let unique_targets: HashSet<&PathBuf> = targets.iter().filter(|t| *t != source).collect();

        // Ce for source = number of unique non-self targets
        *ce_map.get_mut(source).unwrap() = unique_targets.len();

        // Increment Ca for each target
        for target in &unique_targets {
            *ca_map.get_mut(*target).unwrap() += 1;
        }
    }

    (ca_map, ce_map)
}

/// Build set of all modules participating in at least one cycle.
pub fn build_cycle_membership(cycles: &[DepCycle]) -> HashSet<PathBuf> {
    let mut members = HashSet::new();
    for cycle in cycles {
        for module in &cycle.path {
            members.insert(module.clone());
        }
    }
    members
}

// =============================================================================
// Martin Metrics Orchestrator
// =============================================================================

/// Compute Martin metrics from a pre-computed DepsReport.
///
/// This function takes a DepsReport (from analyze_dependencies) and produces
/// a MartinMetricsReport with per-module Ca, Ce, Instability, and cycle membership.
///
/// # Algorithm
///
/// 1. Compute Ca (afferent) and Ce (efferent) coupling for each module
/// 2. Build the set of modules participating in dependency cycles
/// 3. For each module, compute instability and cycle membership
/// 4. Sort by instability DESC, then Ce DESC, then module path ASC
/// 5. Compute summary statistics from ALL modules (before filtering)
/// 6. Apply filtering (cycles_only) and truncation (top N)
pub fn compute_martin_metrics_from_deps(
    deps_report: &DepsReport,
    options: &MartinOptions,
) -> MartinMetricsReport {
    let (ca_map, ce_map) = compute_ca_ce(&deps_report.internal_dependencies);
    let cycle_members = build_cycle_membership(&deps_report.circular_dependencies);

    if ca_map.is_empty() {
        return MartinMetricsReport {
            schema_version: "1.0".to_string(),
            modules_analyzed: 0,
            metrics: Vec::new(),
            cycles: deps_report.circular_dependencies.clone(),
            summary: MartinSummary::default(),
        };
    }

    // Build per-module metrics for ALL modules
    let mut all_metrics: Vec<MartinModuleMetrics> = ca_map
        .keys()
        .map(|module| {
            let ca = ca_map[module];
            let ce = ce_map[module];
            let instability = compute_instability(ca, ce);
            let in_cycle = cycle_members.contains(module);
            MartinModuleMetrics {
                module: module.clone(),
                ca,
                ce,
                instability,
                in_cycle,
            }
        })
        .collect();

    // Sort: instability DESC, ce DESC, module path ASC
    all_metrics.sort_by(|a, b| {
        b.instability
            .partial_cmp(&a.instability)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.ce.cmp(&a.ce))
            .then_with(|| a.module.cmp(&b.module))
    });

    let modules_analyzed = all_metrics.len();

    // Compute summary from ALL modules before filtering
    let avg_instability = if modules_analyzed > 0 {
        all_metrics.iter().map(|m| m.instability).sum::<f64>() / modules_analyzed as f64
    } else {
        0.0
    };

    // Most stable = lowest instability; most unstable = highest instability.
    // Since sorted DESC, last is most stable, first is most unstable.
    let most_unstable = all_metrics.first().map(|m| m.module.clone());
    let most_stable = all_metrics
        .iter()
        .min_by(|a, b| {
            a.instability
                .partial_cmp(&b.instability)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.module.cmp(&b.module))
        })
        .map(|m| m.module.clone());

    let summary = MartinSummary {
        avg_instability,
        total_cycles: deps_report.circular_dependencies.len(),
        most_stable,
        most_unstable,
    };

    // Apply filters
    let mut filtered_metrics = if options.cycles_only {
        all_metrics.into_iter().filter(|m| m.in_cycle).collect()
    } else {
        all_metrics
    };

    if options.top > 0 {
        filtered_metrics.truncate(options.top);
    }

    MartinMetricsReport {
        schema_version: "1.0".to_string(),
        modules_analyzed,
        metrics: filtered_metrics,
        cycles: deps_report.circular_dependencies.clone(),
        summary,
    }
}

// =============================================================================
// Main API
// =============================================================================

/// Analyze module coupling in a codebase
///
/// Detects tightly coupled modules using call graph and import analysis.
///
/// # Arguments
/// * `path` - Directory to analyze
/// * `language` - Optional language filter (auto-detect if None)
/// * `max_pairs` - Maximum number of pairs to return (default: 10)
///
/// # Returns
/// * `Ok(CouplingReport)` - Report with coupling findings
/// * `Err(TldrError)` - On file system errors
///
/// # Example
/// ```ignore
/// use tldr_core::quality::coupling::analyze_coupling;
/// use std::path::Path;
///
/// let report = analyze_coupling(Path::new("src/"), None, Some(10))?;
/// for pair in &report.top_pairs {
///     println!("{} <-> {}: {:.2} ({:?})",
///         pair.source.display(),
///         pair.target.display(),
///         pair.score,
///         pair.verdict
///     );
/// }
/// ```
pub fn analyze_coupling(
    path: &Path,
    language: Option<Language>,
    max_pairs: Option<usize>,
) -> TldrResult<CouplingReport> {
    let options = CouplingOptions {
        max_pairs: max_pairs.unwrap_or(10),
        ..Default::default()
    };

    // Detect language if not specified
    let lang = language.unwrap_or_else(|| detect_dominant_language(path));

    // Build call graph
    let call_graph = build_project_call_graph(path, lang, None, true)?;

    // Analyze with the call graph
    analyze_coupling_with_graph(path, lang, &call_graph, &options)
}

/// Analyze coupling using a pre-built call graph
///
/// This is useful when the call graph is shared across multiple analyzers
/// (dead code, coupling, similarity) to avoid rebuilding it.
pub fn analyze_coupling_with_graph(
    path: &Path,
    language: Language,
    call_graph: &ProjectCallGraph,
    options: &CouplingOptions,
) -> TldrResult<CouplingReport> {
    // Collect module info for import analysis
    let module_infos = collect_module_infos(path, language)?;
    Ok(analyze_coupling_with_module_infos(
        call_graph,
        module_infos,
        options,
    ))
}

/// Analyze coupling from a stored call graph and stored module projections.
///
/// This is the resident ArtifactStore seam; it performs no source walk.
pub fn analyze_coupling_with_module_infos(
    call_graph: &ProjectCallGraph,
    module_infos: HashMap<PathBuf, ModuleInfo>,
    options: &CouplingOptions,
) -> CouplingReport {
    if module_infos.is_empty() {
        return CouplingReport::default();
    }

    // Build import maps for each module
    let import_maps = build_import_maps(&module_infos);

    // Find all cross-file call pairs
    let call_pairs = extract_call_pairs(call_graph);

    // Group calls by module pair
    let mut pair_calls: HashMap<(PathBuf, PathBuf), Vec<CallEdge>> = HashMap::new();
    for edge in call_graph.edges() {
        if edge.src_file != edge.dst_file {
            // Normalize pair order for consistent grouping
            let (a, b) = normalize_pair(&edge.src_file, &edge.dst_file);
            pair_calls.entry((a, b)).or_default().push(edge.clone());
        }
    }

    // Calculate coupling for each pair
    let mut couplings: Vec<ModuleCoupling> = Vec::new();

    for ((source, target), edges) in &pair_calls {
        let coupling =
            calculate_module_coupling(source, target, edges, &import_maps, &module_infos);
        couplings.push(coupling);
    }

    // Sort by score descending
    couplings.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Calculate statistics
    let total_pairs = couplings.len();
    let avg_score = if total_pairs > 0 {
        Some(couplings.iter().map(|c| c.score).sum::<f64>() / total_pairs as f64)
    } else {
        None
    };
    let tight_count = couplings
        .iter()
        .filter(|c| c.verdict == CouplingVerdict::Tight)
        .count();

    // Take top N pairs
    let shown_pairs = couplings.len().min(options.max_pairs);
    let was_truncated = couplings.len() > options.max_pairs;
    couplings.truncate(options.max_pairs);

    CouplingReport {
        modules_analyzed: module_infos.len(),
        pairs_analyzed: total_pairs,
        total_cross_file_pairs: call_pairs.len(),
        avg_coupling_score: avg_score,
        tight_coupling_count: tight_count,
        top_pairs: couplings,
        truncated: if was_truncated { Some(true) } else { None },
        total_pairs: if was_truncated {
            Some(total_pairs)
        } else {
            None
        },
        shown_pairs: if was_truncated {
            Some(shown_pairs)
        } else {
            None
        },
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Detect the dominant language in a directory
fn detect_dominant_language(path: &Path) -> Language {
    let mut counts: HashMap<Language, usize> = HashMap::new();

    for entry in walk_project(path) {
        if let Some(lang) = Language::from_path(entry.path()) {
            *counts.entry(lang).or_insert(0) += 1;
        }
    }

    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(lang, _)| lang)
        .unwrap_or(Language::Python)
}

/// Collect module information from all source files
fn collect_module_infos(
    path: &Path,
    language: Language,
) -> TldrResult<HashMap<PathBuf, ModuleInfo>> {
    let mut infos = HashMap::new();

    let extensions: HashSet<String> = language
        .extensions()
        .iter()
        .map(|s| s.to_string())
        .collect();

    for entry in walk_project(path) {
        let entry_path = entry.path();
        if !entry_path.is_file() {
            continue;
        }

        if let Some(ext) = entry_path.extension().and_then(|e| e.to_str()) {
            let ext_with_dot = format!(".{}", ext);
            if !extensions.contains(&ext_with_dot) {
                continue;
            }
        } else {
            continue;
        }

        match extract_file(entry_path, Some(path)) {
            Ok(info) => {
                // Normalize path relative to root to match call graph edge paths
                let normalized = if let Ok(relative) = entry_path.strip_prefix(path) {
                    // Convert to forward slashes for consistency with call graph
                    PathBuf::from(relative.to_string_lossy().replace('\\', "/"))
                } else {
                    entry_path.to_path_buf()
                };
                infos.insert(normalized, info);
            }
            Err(_) => {
                // Skip files that fail to parse
                continue;
            }
        }
    }

    Ok(infos)
}

/// Build a map of imports for each module
fn build_import_maps(
    module_infos: &HashMap<PathBuf, ModuleInfo>,
) -> HashMap<PathBuf, HashSet<String>> {
    let mut maps = HashMap::new();

    for (path, info) in module_infos {
        let imports: HashSet<String> = info.imports.iter().map(|i| i.module.clone()).collect();
        maps.insert(path.clone(), imports);
    }

    maps
}

/// Extract unique call pairs from the call graph
fn extract_call_pairs(call_graph: &ProjectCallGraph) -> HashSet<(PathBuf, PathBuf)> {
    let mut pairs = HashSet::new();

    for edge in call_graph.edges() {
        if edge.src_file != edge.dst_file {
            let (a, b) = normalize_pair(&edge.src_file, &edge.dst_file);
            pairs.insert((a, b));
        }
    }

    pairs
}

/// Normalize a pair of paths for consistent ordering
fn normalize_pair(a: &Path, b: &Path) -> (PathBuf, PathBuf) {
    if a < b {
        (a.to_path_buf(), b.to_path_buf())
    } else {
        (b.to_path_buf(), a.to_path_buf())
    }
}

/// Calculate coupling between two modules
fn calculate_module_coupling(
    source: &Path,
    target: &Path,
    edges: &[CallEdge],
    import_maps: &HashMap<PathBuf, HashSet<String>>,
    module_infos: &HashMap<PathBuf, ModuleInfo>,
) -> ModuleCoupling {
    // Separate calls by direction
    let mut calls_s_to_t: Vec<CallSite> = Vec::new();
    let mut calls_t_to_s: Vec<CallSite> = Vec::new();

    for edge in edges {
        let call_site = CallSite {
            caller: edge.src_func.clone(),
            callee: edge.dst_func.clone(),
            line: None, // Line info not available in CallEdge
        };

        if edge.src_file == source {
            calls_s_to_t.push(call_site);
        } else {
            calls_t_to_s.push(call_site);
        }
    }

    // Count imports between modules
    let import_count = count_imports_between(source, target, module_infos);

    // Find shared imports
    let shared_imports = find_shared_imports(source, target, import_maps);

    // Calculate call count
    let call_count = edges.len();

    // Calculate coupling score
    // Score is normalized: (imports + calls) / max_possible
    // We use a simple heuristic: score = tanh((imports + calls) / 10)
    // This maps to [0, 1) with reasonable scaling
    let raw_coupling = (import_count + call_count) as f64;
    let score = (raw_coupling / 10.0).tanh();

    let verdict = CouplingVerdict::from_score(score);

    ModuleCoupling {
        source: source.to_path_buf(),
        target: target.to_path_buf(),
        import_count,
        call_count,
        calls_source_to_target: calls_s_to_t,
        calls_target_to_source: calls_t_to_s,
        shared_imports,
        score,
        verdict,
    }
}

/// Count direct imports between two modules
fn count_imports_between(
    source: &Path,
    target: &Path,
    module_infos: &HashMap<PathBuf, ModuleInfo>,
) -> usize {
    let mut count = 0;

    // Get module name for target
    let target_module = path_to_module_name(target);

    // Check if source imports target
    if let Some(source_info) = module_infos.get(source) {
        for import in &source_info.imports {
            if import.module.contains(&target_module) || target_module.contains(&import.module) {
                count += 1;
            }
        }
    }

    // Check if target imports source
    let source_module = path_to_module_name(source);
    if let Some(target_info) = module_infos.get(target) {
        for import in &target_info.imports {
            if import.module.contains(&source_module) || source_module.contains(&import.module) {
                count += 1;
            }
        }
    }

    count
}

/// Find imports that both modules share
fn find_shared_imports(
    source: &Path,
    target: &Path,
    import_maps: &HashMap<PathBuf, HashSet<String>>,
) -> Vec<String> {
    let empty = HashSet::new();
    let source_imports = import_maps.get(source).unwrap_or(&empty);
    let target_imports = import_maps.get(target).unwrap_or(&empty);

    source_imports
        .intersection(target_imports)
        .cloned()
        .collect()
}

/// Convert a path to a module name
fn path_to_module_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

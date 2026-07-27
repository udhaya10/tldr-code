//! Hub detection algorithms for call graph centrality analysis
//!
//! This module provides centrality measures to identify "hub" functions
//! that are critical to the codebase - changes to them affect many others.
//!
//! ## Implemented Measures
//!
//! - `compute_in_degree`: Normalized count of callers (who depends on this?)
//! - `compute_out_degree`: Normalized count of callees (what does this depend on?)
//! - `compute_pagerank`: Recursive importance via PageRank algorithm
//! - `compute_betweenness`: Bridge detection via betweenness centrality
//!
//! ## Normalization
//!
//! All centrality scores are normalized to [0, 1] using:
//! - `in_degree(v) = |callers| / (n - 1)` where n = total nodes
//! - `out_degree(v) = |callees| / (n - 1)`
//! - `pagerank`: Normalized by dividing by max value after convergence
//! - `betweenness`: Normalized by (n-1)(n-2) for directed graphs, then by max
//!
//! ## Risk Levels
//!
//! Based on composite score:
//! - Critical: >= 0.8
//! - High: >= 0.6
//! - Medium: >= 0.4
//! - Low: < 0.4
//!
//! ## Composite Score Weights
//!
//! Default weights (from spec):
//! - in_degree: 0.25
//! - out_degree: 0.25
//! - betweenness: 0.30
//! - pagerank: 0.20
//!
//! # Example
//!
//! ```rust,ignore
//! use tldr_core::analysis::hubs::{compute_in_degree, compute_out_degree, compute_pagerank, compute_betweenness, HubScore, RiskLevel, PageRankConfig, BetweennessConfig};
//! use tldr_core::callgraph::graph_utils::{build_forward_graph, build_reverse_graph, collect_nodes};
//!
//! let forward = build_forward_graph(&call_graph);
//! let reverse = build_reverse_graph(&call_graph);
//! let nodes = collect_nodes(&call_graph);
//!
//! let in_degrees = compute_in_degree(&nodes, &reverse);
//! let out_degrees = compute_out_degree(&nodes, &forward);
//!
//! // PageRank with default config
//! let pr_config = PageRankConfig::default();
//! let pagerank_result = compute_pagerank(&nodes, &reverse, &forward, &pr_config);
//!
//! // Betweenness with sampling for large graphs
//! let bc_config = BetweennessConfig::default();
//! let betweenness = compute_betweenness(&nodes, &forward, &bc_config);
//! ```

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::types::{FunctionRef, Language};

// =============================================================================
// Configuration Types
// =============================================================================

/// Configuration for PageRank computation (T2 mitigation)
///
/// Default values tuned for code call graphs:
/// - damping: 0.85 (standard)
/// - max_iterations: 150 (increased for deep chains)
/// - epsilon: 1e-5 (faster convergence with negligible accuracy loss)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageRankConfig {
    /// Damping factor (probability of following edges vs random jump)
    /// Default: 0.85
    pub damping: f64,
    /// Maximum iterations before stopping
    /// Default: 150 (T2 mitigation: increased from 100)
    pub max_iterations: usize,
    /// Convergence threshold (stop when max delta < epsilon)
    /// Default: 1e-5 (T2 mitigation: larger for faster convergence)
    pub epsilon: f64,
}

impl Default for PageRankConfig {
    fn default() -> Self {
        Self {
            damping: 0.85,
            max_iterations: 150,
            epsilon: 1e-5,
        }
    }
}

/// Configuration for betweenness centrality (T4 mitigation)
///
/// For large graphs, betweenness is O(V*E) which can be prohibitive.
/// Sampling uses k random sources to approximate betweenness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BetweennessConfig {
    /// Sample size for approximation. None = compute from all sources
    /// For graphs > 1000 nodes, recommend Some(100) per Brandes 2008
    pub sample_size: Option<usize>,
    /// Maximum nodes before auto-skipping betweenness
    /// Default: 5000 (warn but still compute with sampling)
    pub max_nodes: usize,
}

impl Default for BetweennessConfig {
    fn default() -> Self {
        Self {
            sample_size: None,
            max_nodes: 5000,
        }
    }
}

impl BetweennessConfig {
    /// Create config with sampling enabled
    pub fn with_sampling(sample_size: usize) -> Self {
        Self {
            sample_size: Some(sample_size),
            max_nodes: 5000,
        }
    }
}

/// Result of PageRank computation with convergence info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageRankResult {
    /// PageRank scores for each node, normalized to [0, 1]
    pub scores: HashMap<FunctionRef, f64>,
    /// Number of iterations used
    pub iterations_used: usize,
    /// Whether the algorithm converged (delta < epsilon)
    pub converged: bool,
}

// =============================================================================
// Types
// =============================================================================

/// Risk level classification for hub functions
///
/// Thresholds based on composite centrality score:
/// - Critical (>=0.8): Top ~5% - changes require extensive testing
/// - High (>=0.6): Top ~15% - changes need careful review
/// - Medium (>=0.4): Top ~30% - normal caution
/// - Low (<0.4): Safe to modify with standard practices
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    /// Composite score >= 0.8: top ~5% of functions, changes require extensive testing.
    Critical,
    /// Composite score >= 0.6: top ~15% of functions, changes need careful review.
    High,
    /// Composite score >= 0.4: top ~30% of functions, normal caution advised.
    Medium,
    /// Composite score < 0.4: safe to modify with standard development practices.
    Low,
}

impl RiskLevel {
    /// Classify risk level from a composite score
    ///
    /// # Arguments
    /// * `score` - Composite centrality score in range [0, 1]
    ///
    /// # Returns
    /// Appropriate risk level based on thresholds
    pub fn from_score(score: f64) -> Self {
        if score >= 0.8 {
            RiskLevel::Critical
        } else if score >= 0.6 {
            RiskLevel::High
        } else if score >= 0.4 {
            RiskLevel::Medium
        } else {
            RiskLevel::Low
        }
    }
}

impl std::fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RiskLevel::Critical => write!(f, "critical"),
            RiskLevel::High => write!(f, "high"),
            RiskLevel::Medium => write!(f, "medium"),
            RiskLevel::Low => write!(f, "low"),
        }
    }
}

/// Hub score for a single function
///
/// Contains all centrality metrics and derived risk classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubScore {
    /// Reference to the function
    pub function_ref: FunctionRef,
    /// File path (convenience accessor)
    pub file: PathBuf,
    /// Function name (convenience accessor)
    pub name: String,
    /// Normalized in-degree [0, 1] - how many functions call this one
    pub in_degree: f64,
    /// Normalized out-degree [0, 1] - how many functions this one calls
    pub out_degree: f64,
    /// PageRank score [0, 1] - recursive importance based on callers
    /// None if PageRank was not computed
    pub pagerank: Option<f64>,
    /// Betweenness centrality [0, 1] - how often on shortest paths
    /// None if betweenness was not computed
    pub betweenness: Option<f64>,
    /// Raw count of callers
    pub callers_count: usize,
    /// Raw count of callees
    pub callees_count: usize,
    /// Composite score combining all measures [0, 1]
    pub composite_score: f64,
    /// Risk level based on composite score
    pub risk_level: RiskLevel,
}

/// Default weights for composite score calculation (from spec)
pub const WEIGHT_IN_DEGREE: f64 = 0.25;
/// Weight applied to normalized out-degree in the composite hub score formula.
pub const WEIGHT_OUT_DEGREE: f64 = 0.25;
/// Weight applied to betweenness centrality in the composite hub score formula.
pub const WEIGHT_BETWEENNESS: f64 = 0.30;
/// Weight applied to PageRank score in the composite hub score formula.
pub const WEIGHT_PAGERANK: f64 = 0.20;

impl HubScore {
    /// Create a new HubScore from centrality values (in/out degree only)
    ///
    /// # Arguments
    /// * `function_ref` - Reference to the function
    /// * `in_degree` - Normalized in-degree [0, 1]
    /// * `out_degree` - Normalized out-degree [0, 1]
    /// * `callers_count` - Raw count of callers
    /// * `callees_count` - Raw count of callees
    pub fn new(
        function_ref: FunctionRef,
        in_degree: f64,
        out_degree: f64,
        callers_count: usize,
        callees_count: usize,
    ) -> Self {
        // Simple composite: average of in_degree and out_degree (when no pagerank/betweenness)
        let composite_score = (in_degree + out_degree) / 2.0;
        let risk_level = RiskLevel::from_score(composite_score);

        Self {
            file: function_ref.file.clone(),
            name: function_ref.name.clone(),
            function_ref,
            in_degree,
            out_degree,
            pagerank: None,
            betweenness: None,
            callers_count,
            callees_count,
            composite_score,
            risk_level,
        }
    }

    /// Create HubScore with all four centrality measures
    ///
    /// Uses weighted composite:
    /// - in_degree: 0.25
    /// - out_degree: 0.25
    /// - betweenness: 0.30
    /// - pagerank: 0.20
    pub fn with_all_measures(
        function_ref: FunctionRef,
        in_degree: f64,
        out_degree: f64,
        pagerank: f64,
        betweenness: f64,
        callers_count: usize,
        callees_count: usize,
    ) -> Self {
        let composite_score =
            compute_composite_score(in_degree, out_degree, Some(pagerank), Some(betweenness));
        let risk_level = RiskLevel::from_score(composite_score);

        Self {
            file: function_ref.file.clone(),
            name: function_ref.name.clone(),
            function_ref,
            in_degree,
            out_degree,
            pagerank: Some(pagerank),
            betweenness: Some(betweenness),
            callers_count,
            callees_count,
            composite_score,
            risk_level,
        }
    }

    /// Create HubScore with explicit composite score
    ///
    /// Used when composite is computed with additional measures (PageRank, betweenness)
    pub fn with_composite(
        function_ref: FunctionRef,
        in_degree: f64,
        out_degree: f64,
        callers_count: usize,
        callees_count: usize,
        composite_score: f64,
    ) -> Self {
        let risk_level = RiskLevel::from_score(composite_score);

        Self {
            file: function_ref.file.clone(),
            name: function_ref.name.clone(),
            function_ref,
            in_degree,
            out_degree,
            pagerank: None,
            betweenness: None,
            callers_count,
            callees_count,
            composite_score,
            risk_level,
        }
    }

    /// Create HubScore with optional pagerank and betweenness
    pub fn with_optional_measures(
        function_ref: FunctionRef,
        in_degree: f64,
        out_degree: f64,
        pagerank: Option<f64>,
        betweenness: Option<f64>,
        callers_count: usize,
        callees_count: usize,
    ) -> Self {
        let composite_score = compute_composite_score(in_degree, out_degree, pagerank, betweenness);
        let risk_level = RiskLevel::from_score(composite_score);

        Self {
            file: function_ref.file.clone(),
            name: function_ref.name.clone(),
            function_ref,
            in_degree,
            out_degree,
            pagerank,
            betweenness,
            callers_count,
            callees_count,
            composite_score,
            risk_level,
        }
    }
}

/// Compute composite score from available measures
///
/// Uses weighted average with weights normalized to sum to 1.0 for available measures.
/// Default weights (from spec):
/// - in_degree: 0.25
/// - out_degree: 0.25
/// - betweenness: 0.30
/// - pagerank: 0.20
pub fn compute_composite_score(
    in_degree: f64,
    out_degree: f64,
    pagerank: Option<f64>,
    betweenness: Option<f64>,
) -> f64 {
    let mut total_weight = WEIGHT_IN_DEGREE + WEIGHT_OUT_DEGREE;
    let mut weighted_sum = WEIGHT_IN_DEGREE * in_degree + WEIGHT_OUT_DEGREE * out_degree;

    if let Some(pr) = pagerank {
        weighted_sum += WEIGHT_PAGERANK * pr;
        total_weight += WEIGHT_PAGERANK;
    }

    if let Some(bc) = betweenness {
        weighted_sum += WEIGHT_BETWEENNESS * bc;
        total_weight += WEIGHT_BETWEENNESS;
    }

    if total_weight > 0.0 {
        weighted_sum / total_weight
    } else {
        0.0
    }
}

// =============================================================================
// Degree Centrality Functions
// =============================================================================

/// Compute normalized in-degree for all nodes
///
/// In-degree measures how many functions call each function.
/// Higher in-degree means more functions depend on this one.
///
/// Formula: `in_degree(v) = |callers| / (n - 1)`
///
/// Where:
/// - `|callers|` = number of functions that call v
/// - `n` = total number of nodes in the graph
/// - `n - 1` = maximum possible in-degree (all other nodes)
///
/// # Arguments
/// * `nodes` - Set of all function references in the graph
/// * `reverse_graph` - Map from callee -> [callers]
///
/// # Returns
/// HashMap mapping each FunctionRef to its normalized in-degree [0, 1]
///
/// # Edge Cases
/// - Empty graph: returns empty map
/// - Single node: returns { node: 0.0 } (no possible callers)
/// - Node with no callers: returns 0.0 for that node
pub fn compute_in_degree(
    nodes: &HashSet<FunctionRef>,
    reverse_graph: &HashMap<FunctionRef, Vec<FunctionRef>>,
) -> HashMap<FunctionRef, f64> {
    let n = nodes.len();

    // Handle edge cases
    if n == 0 {
        return HashMap::new();
    }

    // Single node has no possible callers (n-1 = 0)
    if n == 1 {
        return nodes.iter().map(|node| (node.clone(), 0.0)).collect();
    }

    let max_degree = (n - 1) as f64;

    nodes
        .iter()
        .map(|node| {
            let callers_count = reverse_graph
                .get(node)
                .map(|callers| callers.len())
                .unwrap_or(0);

            let normalized = callers_count as f64 / max_degree;
            (node.clone(), normalized)
        })
        .collect()
}

/// Compute normalized out-degree for all nodes
///
/// Out-degree measures how many functions each function calls.
/// Higher out-degree means this function orchestrates/coordinates many others.
///
/// Formula: `out_degree(v) = |callees| / (n - 1)`
///
/// Where:
/// - `|callees|` = number of functions that v calls
/// - `n` = total number of nodes in the graph
/// - `n - 1` = maximum possible out-degree (all other nodes)
///
/// # Arguments
/// * `nodes` - Set of all function references in the graph
/// * `forward_graph` - Map from caller -> [callees]
///
/// # Returns
/// HashMap mapping each FunctionRef to its normalized out-degree [0, 1]
///
/// # Edge Cases
/// - Empty graph: returns empty map
/// - Single node: returns { node: 0.0 } (no possible callees)
/// - Node with no callees: returns 0.0 for that node
pub fn compute_out_degree(
    nodes: &HashSet<FunctionRef>,
    forward_graph: &HashMap<FunctionRef, Vec<FunctionRef>>,
) -> HashMap<FunctionRef, f64> {
    let n = nodes.len();

    // Handle edge cases
    if n == 0 {
        return HashMap::new();
    }

    // Single node has no possible callees (n-1 = 0)
    if n == 1 {
        return nodes.iter().map(|node| (node.clone(), 0.0)).collect();
    }

    let max_degree = (n - 1) as f64;

    nodes
        .iter()
        .map(|node| {
            let callees_count = forward_graph
                .get(node)
                .map(|callees| callees.len())
                .unwrap_or(0);

            let normalized = callees_count as f64 / max_degree;
            (node.clone(), normalized)
        })
        .collect()
}

/// Get raw caller counts for all nodes
///
/// # Arguments
/// * `nodes` - Set of all function references in the graph
/// * `reverse_graph` - Map from callee -> [callers]
///
/// # Returns
/// HashMap mapping each FunctionRef to its raw caller count
pub fn get_caller_counts(
    nodes: &HashSet<FunctionRef>,
    reverse_graph: &HashMap<FunctionRef, Vec<FunctionRef>>,
) -> HashMap<FunctionRef, usize> {
    nodes
        .iter()
        .map(|node| {
            let count = reverse_graph
                .get(node)
                .map(|callers| callers.len())
                .unwrap_or(0);
            (node.clone(), count)
        })
        .collect()
}

// =============================================================================
// PageRank Algorithm (T1 mitigation - corrected formula)
// =============================================================================

/// Compute PageRank for all nodes (reverse PageRank for call graphs)
///
/// For call graph analysis, we use **reverse PageRank** to measure
/// "how many important functions depend on this one."
///
/// ## Algorithm (power iteration)
///
/// 1. Initialize all nodes with score 1/n
/// 2. Iterate until convergence:
///    - Compute dangling node contribution (nodes with no callers)
///    - For each node v, new_score = (1-d)/n + d*(incoming_contrib + dangling_contrib)
/// 3. Normalize to [0, 1] by dividing by max value
///
/// ## Formula (T1 mitigation - CORRECTED)
///
/// ```text
/// PR(v) = (1-d)/n + d * (sum(PR(u)/out_deg(u) for u in callers(v)) + dangling_sum/n)
/// ```
///
/// The key correction is that `dangling_sum/n` is INSIDE the damping term,
/// not added separately (which would double-apply damping).
///
/// ## Dangling Nodes (T1 mitigation)
///
/// Dangling nodes are nodes with no outgoing edges (in our reversed view,
/// these are entry points with no callers). Their PageRank is distributed
/// evenly to all nodes.
///
/// # Arguments
/// * `nodes` - Set of all function references
/// * `reverse_graph` - Map from callee -> [callers]
/// * `forward_graph` - Map from caller -> [callees]
/// * `config` - PageRank configuration (damping, max_iter, epsilon)
///
/// # Returns
/// PageRankResult containing normalized scores and convergence info
pub fn compute_pagerank(
    nodes: &HashSet<FunctionRef>,
    reverse_graph: &HashMap<FunctionRef, Vec<FunctionRef>>,
    forward_graph: &HashMap<FunctionRef, Vec<FunctionRef>>,
    config: &PageRankConfig,
) -> PageRankResult {
    let n = nodes.len();

    // Handle edge cases
    if n == 0 {
        return PageRankResult {
            scores: HashMap::new(),
            iterations_used: 0,
            converged: true,
        };
    }

    if n == 1 {
        return PageRankResult {
            scores: nodes.iter().map(|node| (node.clone(), 1.0)).collect(),
            iterations_used: 0,
            converged: true,
        };
    }

    let n_f64 = n as f64;
    let d = config.damping;
    let base_score = (1.0 - d) / n_f64;

    // determinism-and-stderr-hygiene-v1 (BUG-3): the iteration loop below
    // walks `nodes` (a `HashSet<FunctionRef>`) per iteration. HashSet
    // iteration order is non-deterministic (DefaultHasher seeds per
    // process), and floating-point summation is non-associative, so
    // identical inputs produced last-digit drift across runs of
    // `tldr hubs <repo>` — enough to shuffle the top-N when scores
    // were near-tied. Materialize a deterministic, sorted node list
    // ONCE and reuse it for every iteration so accumulation order is
    // stable across processes. Sort key is `(file, name)`, which is
    // the FunctionRef identity tuple per its PartialEq/Hash impls
    // (`crates/tldr-core/src/types.rs:1429-1443`).
    let mut sorted_nodes: Vec<FunctionRef> = nodes.iter().cloned().collect();
    sorted_nodes.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.name.cmp(&b.name)));

    // Initialize scores uniformly
    let mut scores: HashMap<FunctionRef, f64> = sorted_nodes
        .iter()
        .map(|node| (node.clone(), 1.0 / n_f64))
        .collect();

    // Pre-compute out-degrees on reversed graph (= number of callers for each node)
    // For reverse PageRank, "out-degree" is the number of callees (who we point to in the original)
    // But we're computing importance based on who calls us, so we use the reverse graph
    let out_degrees: HashMap<FunctionRef, usize> = sorted_nodes
        .iter()
        .map(|node| {
            // Out-degree in the reverse graph = number of nodes this node points to in reverse
            // = number of functions this function calls (forward graph edges from this node)
            let deg = forward_graph.get(node).map_or(0, |v| v.len());
            (node.clone(), deg)
        })
        .collect();

    // Identify dangling nodes (nodes with no outgoing edges in the original graph)
    // These are leaf functions that don't call anything. Iterating
    // `sorted_nodes` (deterministic order) ensures the resulting Vec
    // matches across runs — the `dangling_sum` reduction below depends
    // on this for byte-stable PageRank values.
    let dangling_nodes: Vec<FunctionRef> = sorted_nodes
        .iter()
        .filter(|node| out_degrees.get(*node).copied().unwrap_or(0) == 0)
        .cloned()
        .collect();

    let mut iterations_used = 0;
    let mut converged = false;

    for _ in 0..config.max_iterations {
        iterations_used += 1;

        // Compute dangling node contribution
        let dangling_sum: f64 = dangling_nodes.iter().map(|node| scores[node]).sum();
        let dangling_contrib = dangling_sum / n_f64;

        let mut new_scores: HashMap<FunctionRef, f64> = HashMap::new();
        let mut max_delta: f64 = 0.0;

        // Iterate the sorted node list (BUG-3 fix) so float
        // accumulation order is identical across runs; iterating
        // `nodes` directly walked the HashSet in DefaultHasher order.
        // We also sort each `callers` slice from `reverse_graph` by
        // (file, name) before reducing into `incoming_contrib` —
        // upstream callgraph builders return Vec<FunctionRef> whose
        // order tracked HashMap insertion order, which is also
        // process-non-deterministic.
        for node in &sorted_nodes {
            // Contribution from nodes that call this node (reverse graph)
            // In the original graph, these are the callers of `node`
            let incoming_contrib: f64 = reverse_graph.get(node).map_or(0.0, |callers| {
                let mut sorted_callers: Vec<&FunctionRef> = callers.iter().collect();
                sorted_callers
                    .sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.name.cmp(&b.name)));
                sorted_callers
                    .iter()
                    .map(|caller| {
                        let caller_out_deg = out_degrees.get(*caller).copied().unwrap_or(0);
                        if caller_out_deg > 0 {
                            scores[*caller] / caller_out_deg as f64
                        } else {
                            0.0
                        }
                    })
                    .sum()
            });

            // CORRECTED formula (T1): dangling_contrib is inside the damping term
            let new_score = base_score + d * (incoming_contrib + dangling_contrib);

            let delta = (new_score - scores[node]).abs();
            if delta > max_delta {
                max_delta = delta;
            }

            new_scores.insert(node.clone(), new_score);
        }

        scores = new_scores;

        // Check convergence
        if max_delta < config.epsilon {
            converged = true;
            break;
        }
    }

    // Normalize to [0, 1] by dividing by max value
    let max_score = scores.values().copied().fold(0.0_f64, f64::max);
    if max_score > 0.0 {
        for score in scores.values_mut() {
            *score /= max_score;
        }
    }

    PageRankResult {
        scores,
        iterations_used,
        converged,
    }
}

// =============================================================================
// Betweenness Centrality (T4 mitigation - with sampling)
// =============================================================================

/// Compute betweenness centrality for all nodes
///
/// Betweenness measures how often a node lies on shortest paths between
/// other nodes. High betweenness indicates a "bridge" or "bottleneck".
///
/// ## Algorithm (Brandes)
///
/// For each source node s:
/// 1. BFS to find shortest path distances and predecessors
/// 2. Backward pass to accumulate dependency values
/// 3. Update betweenness for each node (except source)
///
/// ## Complexity
///
/// O(V * E) for unweighted graphs. For large graphs, use sampling (T4 mitigation).
///
/// ## Sampling (T4 mitigation)
///
/// When `sample_size` is Some(k), only k random sources are used.
/// The results are then scaled by n/k to approximate full betweenness.
/// Per Brandes 2008, k=100 gives good approximation.
///
/// ## Normalization (T3 mitigation)
///
/// For directed graphs: `b(v) / ((n-1)(n-2))`
/// Then normalized to [0, 1] by dividing by max value.
///
/// # Arguments
/// * `nodes` - Set of all function references
/// * `forward_graph` - Map from caller -> [callees]
/// * `config` - Betweenness configuration (sample_size, max_nodes)
///
/// # Returns
/// HashMap mapping each FunctionRef to its normalized betweenness [0, 1]
pub fn compute_betweenness(
    nodes: &HashSet<FunctionRef>,
    forward_graph: &HashMap<FunctionRef, Vec<FunctionRef>>,
    config: &BetweennessConfig,
) -> HashMap<FunctionRef, f64> {
    let n = nodes.len();

    // Handle edge cases
    if n <= 2 {
        return nodes.iter().map(|node| (node.clone(), 0.0)).collect();
    }

    // Check if graph is too large
    if n > config.max_nodes {
        // For very large graphs, return zeros with a warning
        // In practice, the caller should use sampling
        return nodes.iter().map(|node| (node.clone(), 0.0)).collect();
    }

    // Convert nodes to Vec for indexing
    let node_list: Vec<FunctionRef> = nodes.iter().cloned().collect();

    // Determine which sources to use
    let sources: Vec<&FunctionRef> = match config.sample_size {
        Some(k) if k < n => {
            // Sample k sources deterministically (using modular arithmetic for reproducibility)
            // For true randomness, you'd use rand, but determinism is better for testing
            let step = n / k.max(1);
            (0..k).map(|i| &node_list[(i * step) % n]).collect()
        }
        _ => {
            // Use all sources
            node_list.iter().collect()
        }
    };

    let num_sources = sources.len();
    let scaling_factor = if num_sources < n {
        n as f64 / num_sources as f64
    } else {
        1.0
    };

    let mut betweenness: HashMap<FunctionRef, f64> =
        nodes.iter().map(|node| (node.clone(), 0.0)).collect();

    // Brandes algorithm
    for source in &sources {
        // BFS for single-source shortest paths
        let mut dist: HashMap<&FunctionRef, usize> = HashMap::new();
        let mut sigma: HashMap<&FunctionRef, f64> = HashMap::new();
        let mut pred: HashMap<&FunctionRef, Vec<&FunctionRef>> = HashMap::new();

        dist.insert(source, 0);
        sigma.insert(source, 1.0);

        let mut queue: VecDeque<&FunctionRef> = VecDeque::new();
        queue.push_back(source);

        let mut order: Vec<&FunctionRef> = Vec::new();

        while let Some(current) = queue.pop_front() {
            order.push(current);

            // Get neighbors (callees in forward graph)
            if let Some(neighbors) = forward_graph.get(current) {
                for neighbor in neighbors {
                    if !nodes.contains(neighbor) {
                        continue;
                    }
                    // First time seeing neighbor?
                    if !dist.contains_key(&neighbor) {
                        dist.insert(neighbor, dist[&current] + 1);
                        queue.push_back(neighbor);
                    }

                    // Is this neighbor on a shortest path from source?
                    if dist.get(&neighbor) == Some(&(dist[&current] + 1)) {
                        *sigma.entry(neighbor).or_insert(0.0) += sigma[&current];
                        pred.entry(neighbor).or_default().push(current);
                    }
                }
            }
        }

        // Back-propagation of dependencies
        let mut delta: HashMap<&FunctionRef, f64> =
            node_list.iter().map(|node| (node, 0.0)).collect();

        // Process in reverse order (farthest to nearest)
        while let Some(w) = order.pop() {
            if let Some(predecessors) = pred.get(&w) {
                for v in predecessors {
                    let sigma_v = sigma.get(v).copied().unwrap_or(0.0);
                    let sigma_w = sigma.get(&w).copied().unwrap_or(0.0);
                    if sigma_w > 0.0 {
                        let contribution = (sigma_v / sigma_w) * (1.0 + delta[&w]);
                        *delta.get_mut(v).unwrap() += contribution;
                    }
                }
            }

            // Accumulate (skip source)
            if w != *source {
                *betweenness.get_mut(w).unwrap() += delta[&w];
            }
        }
    }

    // Apply scaling factor for sampling
    if scaling_factor > 1.0 {
        for value in betweenness.values_mut() {
            *value *= scaling_factor;
        }
    }

    // Normalize for directed graph: (n-1)(n-2)
    let normalizer = ((n - 1) * (n - 2)) as f64;
    if normalizer > 0.0 {
        for value in betweenness.values_mut() {
            *value /= normalizer;
        }
    }

    // Normalize to [0, 1] by dividing by max value
    let max_val = betweenness.values().copied().fold(0.0_f64, f64::max);
    if max_val > 1.0 {
        for value in betweenness.values_mut() {
            *value /= max_val;
        }
    }

    betweenness
}

// =============================================================================
// Utility Functions
// =============================================================================

/// Get raw callee counts for all nodes
///
/// # Arguments
/// * `nodes` - Set of all function references in the graph
/// * `forward_graph` - Map from caller -> [callees]
///
/// # Returns
/// HashMap mapping each FunctionRef to its raw callee count
pub fn get_callee_counts(
    nodes: &HashSet<FunctionRef>,
    forward_graph: &HashMap<FunctionRef, Vec<FunctionRef>>,
) -> HashMap<FunctionRef, usize> {
    nodes
        .iter()
        .map(|node| {
            let count = forward_graph
                .get(node)
                .map(|callees| callees.len())
                .unwrap_or(0);
            (node.clone(), count)
        })
        .collect()
}

/// Compute HubScores for all nodes using in-degree and out-degree only
///
/// This is a convenience function that combines in-degree and out-degree
/// computation into full HubScore objects. For full centrality analysis
/// including PageRank and betweenness, use `compute_hub_scores_full`.
///
/// # Arguments
/// * `nodes` - Set of all function references in the graph
/// * `forward_graph` - Map from caller -> [callees]
/// * `reverse_graph` - Map from callee -> [callers]
///
/// # Returns
/// Vec of HubScores sorted by composite_score descending
pub fn compute_hub_scores(
    nodes: &HashSet<FunctionRef>,
    forward_graph: &HashMap<FunctionRef, Vec<FunctionRef>>,
    reverse_graph: &HashMap<FunctionRef, Vec<FunctionRef>>,
) -> Vec<HubScore> {
    let in_degrees = compute_in_degree(nodes, reverse_graph);
    let out_degrees = compute_out_degree(nodes, forward_graph);
    let caller_counts = get_caller_counts(nodes, reverse_graph);
    let callee_counts = get_callee_counts(nodes, forward_graph);

    let mut scores: Vec<HubScore> = nodes
        .iter()
        .map(|node| {
            let in_deg = in_degrees.get(node).copied().unwrap_or(0.0);
            let out_deg = out_degrees.get(node).copied().unwrap_or(0.0);
            let callers = caller_counts.get(node).copied().unwrap_or(0);
            let callees = callee_counts.get(node).copied().unwrap_or(0);

            HubScore::new(node.clone(), in_deg, out_deg, callers, callees)
        })
        .collect();

    // Sort by composite score descending
    scores.sort_by(|a, b| {
        b.composite_score
            .partial_cmp(&a.composite_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    scores
}

/// Algorithm selection for hub detection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HubAlgorithm {
    /// All algorithms: in_degree, out_degree, pagerank, betweenness
    #[default]
    All,
    /// In-degree only (fast)
    InDegree,
    /// Out-degree only (fast)
    OutDegree,
    /// PageRank only
    PageRank,
    /// Betweenness only (slow for large graphs)
    Betweenness,
}

impl std::str::FromStr for HubAlgorithm {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "all" => Ok(HubAlgorithm::All),
            "indegree" | "in_degree" | "in-degree" => Ok(HubAlgorithm::InDegree),
            "outdegree" | "out_degree" | "out-degree" => Ok(HubAlgorithm::OutDegree),
            "pagerank" | "page_rank" => Ok(HubAlgorithm::PageRank),
            "betweenness" => Ok(HubAlgorithm::Betweenness),
            _ => Err(format!(
                "Unknown algorithm '{}'. Valid: all, indegree, outdegree, pagerank, betweenness",
                s
            )),
        }
    }
}

impl std::fmt::Display for HubAlgorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HubAlgorithm::All => write!(f, "all"),
            HubAlgorithm::InDegree => write!(f, "indegree"),
            HubAlgorithm::OutDegree => write!(f, "outdegree"),
            HubAlgorithm::PageRank => write!(f, "pagerank"),
            HubAlgorithm::Betweenness => write!(f, "betweenness"),
        }
    }
}

/// Full hub detection report (spec Section 3 - hubs CLI)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubReport {
    /// Top hubs sorted by composite score descending
    pub hubs: Vec<HubScore>,
    /// Total number of nodes in the call graph
    pub total_nodes: usize,
    /// Number of hubs returned (may be less than total if threshold applied)
    pub hub_count: usize,
    /// Measures used in this analysis
    pub measures_used: Vec<String>,
    /// Top K by in-degree (for by_measure breakdown)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub by_in_degree: Vec<HubScore>,
    /// Top K by out-degree (for by_measure breakdown)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub by_out_degree: Vec<HubScore>,
    /// Top K by pagerank (for by_measure breakdown)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub by_pagerank: Vec<HubScore>,
    /// Top K by betweenness (for by_measure breakdown)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub by_betweenness: Vec<HubScore>,
    /// PageRank convergence info (if computed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pagerank_info: Option<PageRankConvergenceInfo>,
    /// Explanation message (T16 mitigation: small graph messaging)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
}

/// PageRank convergence info for the report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageRankConvergenceInfo {
    /// Number of iterations used
    pub iterations_used: usize,
    /// Whether the algorithm converged
    pub converged: bool,
}

impl From<&PageRankResult> for PageRankConvergenceInfo {
    fn from(result: &PageRankResult) -> Self {
        Self {
            iterations_used: result.iterations_used,
            converged: result.converged,
        }
    }
}

/// Compute HubScores with all four centrality measures
///
/// Computes in-degree, out-degree, PageRank, and betweenness centrality.
/// Uses weighted composite score with default weights:
/// - in_degree: 0.25
/// - out_degree: 0.25
/// - betweenness: 0.30
/// - pagerank: 0.20
///
/// # Arguments
/// * `nodes` - Set of all function references in the graph
/// * `forward_graph` - Map from caller -> [callees]
/// * `reverse_graph` - Map from callee -> [callers]
/// * `pagerank_config` - Optional PageRank configuration (uses default if None)
/// * `betweenness_config` - Optional betweenness configuration (uses default if None)
///
/// # Returns
/// (Vec<HubScore>, PageRankResult) - Scores sorted by composite descending, and PageRank info
pub fn compute_hub_scores_full(
    nodes: &HashSet<FunctionRef>,
    forward_graph: &HashMap<FunctionRef, Vec<FunctionRef>>,
    reverse_graph: &HashMap<FunctionRef, Vec<FunctionRef>>,
    pagerank_config: Option<&PageRankConfig>,
    betweenness_config: Option<&BetweennessConfig>,
) -> (Vec<HubScore>, PageRankResult) {
    let in_degrees = compute_in_degree(nodes, reverse_graph);
    let out_degrees = compute_out_degree(nodes, forward_graph);
    let caller_counts = get_caller_counts(nodes, reverse_graph);
    let callee_counts = get_callee_counts(nodes, forward_graph);

    // Compute PageRank
    let pr_config = pagerank_config.cloned().unwrap_or_default();
    let pagerank_result = compute_pagerank(nodes, reverse_graph, forward_graph, &pr_config);

    // Compute betweenness
    let bc_config = betweenness_config.cloned().unwrap_or_default();
    let betweenness = compute_betweenness(nodes, forward_graph, &bc_config);

    let mut scores: Vec<HubScore> = nodes
        .iter()
        .map(|node| {
            let in_deg = in_degrees.get(node).copied().unwrap_or(0.0);
            let out_deg = out_degrees.get(node).copied().unwrap_or(0.0);
            let pr = pagerank_result.scores.get(node).copied().unwrap_or(0.0);
            let bc = betweenness.get(node).copied().unwrap_or(0.0);
            let callers = caller_counts.get(node).copied().unwrap_or(0);
            let callees = callee_counts.get(node).copied().unwrap_or(0);

            HubScore::with_all_measures(node.clone(), in_deg, out_deg, pr, bc, callers, callees)
        })
        .collect();

    // Sort by composite score descending
    scores.sort_by(|a, b| {
        b.composite_score
            .partial_cmp(&a.composite_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    (scores, pagerank_result)
}

/// Compute HubScores with selected algorithm(s)
///
/// This function allows selecting which centrality measures to compute,
/// which is useful for faster analysis when only specific measures are needed.
///
/// # Arguments
/// * `nodes` - Set of all function references in the graph
/// * `forward_graph` - Map from caller -> [callees]
/// * `reverse_graph` - Map from callee -> [callers]
/// * `algorithm` - Which algorithm(s) to use
/// * `top_k` - Number of top hubs to return
/// * `threshold` - Optional minimum composite score to include
///
/// # Returns
/// HubReport containing the analysis results
pub fn compute_hub_report(
    nodes: &HashSet<FunctionRef>,
    forward_graph: &HashMap<FunctionRef, Vec<FunctionRef>>,
    reverse_graph: &HashMap<FunctionRef, Vec<FunctionRef>>,
    algorithm: HubAlgorithm,
    top_k: usize,
    threshold: Option<f64>,
) -> HubReport {
    compute_hub_report_with_lines(
        nodes,
        forward_graph,
        reverse_graph,
        algorithm,
        top_k,
        threshold,
        None,
    )
}

/// Lookup map from `(file, function_name)` -> 1-based definition line.
///
/// hubs-line-population-v1: built by [`enumerate_function_lines`] and consumed by
/// [`compute_hub_report_with_lines`] so each hub's `function_ref.line` reflects
/// the actual AST definition position instead of the legacy `0` placeholder.
///
/// The `name` key is whatever the call-graph builder records as the function
/// identifier, including qualified `Class.method` forms produced by
/// `CallGraphIR::build_indices` (cross_file_types.rs:1349-1351).
pub type FunctionLineLookup = HashMap<(PathBuf, String), u32>;

/// Build a `(file, name) -> line` lookup for every function defined under `root`.
///
/// hubs-line-population-v1: this is the canonical line source for `tldr hubs`.
/// We walk the project with the shared [`crate::walker::ProjectWalker`] (so
/// `.gitignore`, `node_modules`, `target`, etc. are honored), parse each file
/// with [`crate::ast::extract_file`], and index every top-level function plus
/// every method by both `name` and qualified `Class.method`.
///
/// File keys use **paths relative to `root`** with forward slashes — this
/// matches `FunctionRef.file` produced by the call-graph builder
/// (`cross_file_types::normalize_path_buf`).
///
/// # Arguments
/// * `root` - Project root (same path passed to `build_project_call_graph`).
/// * `language` - Project language used for extension filtering.
///
/// # Returns
/// A lookup keyed by `(relative_file_path, function_name_or_Class_dot_method)`.
/// Functions that fail to parse are skipped silently (mirrors the call-graph
/// builder's behavior — bad files don't poison hub metrics).
pub fn enumerate_function_lines(root: &Path, language: Language) -> FunctionLineLookup {
    use crate::ast::extract_file;
    use crate::walker::ProjectWalker;

    let mut lookup: FunctionLineLookup = HashMap::new();

    if !root.exists() || !root.is_dir() {
        return lookup;
    }

    // Strip leading dots from extensions: walker wants "py", `Language::extensions`
    // returns ".py".
    let exts: Vec<&'static str> = language
        .extensions()
        .iter()
        .map(|e| e.trim_start_matches('.'))
        .collect();

    let walker = ProjectWalker::new(root).extensions(&exts).iter();
    for entry in walker {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        // Compute relative path matching the call-graph's normalized scheme:
        // strip the project root and forward-slash any backslashes.
        let rel = match path.strip_prefix(root) {
            Ok(p) => p.to_path_buf(),
            Err(_) => path.to_path_buf(),
        };
        let rel_norm = PathBuf::from(rel.to_string_lossy().replace('\\', "/"));

        let module = match extract_file(path, Some(root)) {
            Ok(m) => m,
            Err(_) => continue,
        };

        // Top-level functions: index by bare name.
        for f in &module.functions {
            lookup
                .entry((rel_norm.clone(), f.name.clone()))
                .or_insert(f.line_number);
        }

        // Class methods: index by both bare `name` and qualified `Class.name`.
        for class in &module.classes {
            for m in &class.methods {
                let qualified = format!("{}.{}", class.name, m.name);
                lookup
                    .entry((rel_norm.clone(), qualified))
                    .or_insert(m.line_number);
                // Also index the bare method name as a fallback for
                // builders that do not qualify (first-writer-wins so the
                // qualified form takes priority when both exist).
                lookup
                    .entry((rel_norm.clone(), m.name.clone()))
                    .or_insert(m.line_number);
            }
        }
    }

    lookup
}

/// Same as [`compute_hub_report`] but populates `HubScore.function_ref.line`
/// from a `(file, name) -> line` lookup.
///
/// hubs-line-population-v1: callers (typically `tldr hubs`) build the lookup
/// with [`enumerate_function_lines`] and pass it here so hub output identifies
/// each function by its real AST line instead of `0`. When `lookup` is `None`
/// or a node is absent from the lookup, `line` stays `0` — matching the
/// existing FunctionRef convention (`types.rs:1401`: `0 = unknown`).
pub fn compute_hub_report_with_lines(
    nodes: &HashSet<FunctionRef>,
    forward_graph: &HashMap<FunctionRef, Vec<FunctionRef>>,
    reverse_graph: &HashMap<FunctionRef, Vec<FunctionRef>>,
    algorithm: HubAlgorithm,
    top_k: usize,
    threshold: Option<f64>,
    function_line_lookup: Option<&FunctionLineLookup>,
) -> HubReport {
    let total_nodes = nodes.len();

    // Handle empty graph (T16 mitigation)
    if total_nodes == 0 {
        return HubReport {
            hubs: Vec::new(),
            total_nodes: 0,
            hub_count: 0,
            measures_used: Vec::new(),
            by_in_degree: Vec::new(),
            by_out_degree: Vec::new(),
            by_pagerank: Vec::new(),
            by_betweenness: Vec::new(),
            pagerank_info: None,
            explanation: Some("Empty call graph - no functions found.".to_string()),
        };
    }

    // Compute base degrees (always needed)
    let in_degrees = compute_in_degree(nodes, reverse_graph);
    let out_degrees = compute_out_degree(nodes, forward_graph);
    let caller_counts = get_caller_counts(nodes, reverse_graph);
    let callee_counts = get_callee_counts(nodes, forward_graph);

    // Compute optional measures based on algorithm
    let (pagerank_scores, pagerank_info) =
        if matches!(algorithm, HubAlgorithm::All | HubAlgorithm::PageRank) {
            let config = PageRankConfig::default();
            let result = compute_pagerank(nodes, reverse_graph, forward_graph, &config);
            let info = PageRankConvergenceInfo::from(&result);
            (Some(result.scores), Some(info))
        } else {
            (None, None)
        };

    let betweenness_scores = if matches!(algorithm, HubAlgorithm::All | HubAlgorithm::Betweenness) {
        let config = BetweennessConfig::default();
        Some(compute_betweenness(nodes, forward_graph, &config))
    } else {
        None
    };

    // Build HubScores for all nodes.
    //
    // hubs-line-population-v1: when a `function_line_lookup` is provided, look
    // up the function by `(file, name)` and overwrite `function_ref.line`
    // (the call-graph builder constructs FunctionRefs with `line: 0`, see
    // `graph_utils::collect_nodes`). Misses leave the field at `0`, matching
    // the documented FunctionRef convention (`types.rs:1401`).
    let mut all_scores: Vec<HubScore> = nodes
        .iter()
        .map(|node| {
            let in_deg = in_degrees.get(node).copied().unwrap_or(0.0);
            let out_deg = out_degrees.get(node).copied().unwrap_or(0.0);
            let pr = pagerank_scores.as_ref().and_then(|s| s.get(node).copied());
            let bc = betweenness_scores
                .as_ref()
                .and_then(|s| s.get(node).copied());
            let callers = caller_counts.get(node).copied().unwrap_or(0);
            let callees = callee_counts.get(node).copied().unwrap_or(0);

            // Populate the line from the canonical AST extractor. If the
            // node is not in the lookup, fall back to whatever line the
            // FunctionRef already carries (typically 0 = unknown).
            let mut node_with_line = node.clone();
            if let Some(lookup) = function_line_lookup {
                let key = (node.file.clone(), node.name.clone());
                if let Some(&line) = lookup.get(&key) {
                    node_with_line.line = line;
                }
            }

            HubScore::with_optional_measures(
                node_with_line,
                in_deg,
                out_deg,
                pr,
                bc,
                callers,
                callees,
            )
        })
        .collect();

    // Apply threshold filter if specified
    if let Some(thresh) = threshold {
        all_scores.retain(|s| s.composite_score >= thresh);
    }

    // determinism-and-stderr-hygiene-v1 (BUG-3): every sort_by below
    // previously broke ties by leaving original-Vec order, but the
    // input Vec was `nodes.iter()` over a HashSet — process-non-
    // deterministic. When several functions had identical (or
    // FP-near-identical) scores, the top-N list shuffled across runs.
    // Add `(file, name)` as a final tiebreaker on every sort so the
    // total order is stable. (PageRank values themselves are now
    // byte-stable per the `compute_pagerank` fix above, so the
    // tiebreaker rarely fires for the primary `composite_score` sort
    // — but the by_* breakdowns can still tie on integer in_degree /
    // out_degree, where this matters.)
    fn hub_id_tiebreak(a: &HubScore, b: &HubScore) -> std::cmp::Ordering {
        a.function_ref
            .file
            .cmp(&b.function_ref.file)
            .then_with(|| a.function_ref.name.cmp(&b.function_ref.name))
    }

    // Sort by composite score descending, with file/name tiebreaker
    all_scores.sort_by(|a, b| {
        b.composite_score
            .partial_cmp(&a.composite_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| hub_id_tiebreak(a, b))
    });

    // Take top K
    let hubs: Vec<HubScore> = all_scores.into_iter().take(top_k).collect();
    let hub_count = hubs.len();

    // Build by_* breakdowns (only for 'all' algorithm)
    let (by_in_degree, by_out_degree, by_pagerank, by_betweenness) =
        if matches!(algorithm, HubAlgorithm::All) {
            // Sort copies by each measure (each with file/name tiebreaker)
            let mut by_in: Vec<HubScore> = hubs.clone();
            by_in.sort_by(|a, b| {
                b.in_degree
                    .partial_cmp(&a.in_degree)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| hub_id_tiebreak(a, b))
            });

            let mut by_out: Vec<HubScore> = hubs.clone();
            by_out.sort_by(|a, b| {
                b.out_degree
                    .partial_cmp(&a.out_degree)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| hub_id_tiebreak(a, b))
            });

            let mut by_pr: Vec<HubScore> = hubs.clone();
            by_pr.sort_by(|a, b| {
                let a_pr = a.pagerank.unwrap_or(0.0);
                let b_pr = b.pagerank.unwrap_or(0.0);
                b_pr.partial_cmp(&a_pr)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| hub_id_tiebreak(a, b))
            });

            let mut by_bc: Vec<HubScore> = hubs.clone();
            by_bc.sort_by(|a, b| {
                let a_bc = a.betweenness.unwrap_or(0.0);
                let b_bc = b.betweenness.unwrap_or(0.0);
                b_bc.partial_cmp(&a_bc)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| hub_id_tiebreak(a, b))
            });

            (by_in, by_out, by_pr, by_bc)
        } else {
            (Vec::new(), Vec::new(), Vec::new(), Vec::new())
        };

    // Build measures_used list
    let measures_used = match algorithm {
        HubAlgorithm::All => vec![
            "in_degree".to_string(),
            "out_degree".to_string(),
            "pagerank".to_string(),
            "betweenness".to_string(),
        ],
        HubAlgorithm::InDegree => vec!["in_degree".to_string()],
        HubAlgorithm::OutDegree => vec!["out_degree".to_string()],
        HubAlgorithm::PageRank => vec!["pagerank".to_string()],
        HubAlgorithm::Betweenness => vec!["betweenness".to_string()],
    };

    // T16 mitigation: small graph messaging
    let explanation = if total_nodes < 10 {
        Some(format!(
            "Small call graph ({} nodes). Hub metrics may not be statistically meaningful for graphs with fewer than 10 nodes.",
            total_nodes
        ))
    } else {
        // Count critical and high risk hubs
        let critical_count = hubs
            .iter()
            .filter(|h| h.risk_level == RiskLevel::Critical)
            .count();
        let high_count = hubs
            .iter()
            .filter(|h| h.risk_level == RiskLevel::High)
            .count();
        if critical_count > 0 || high_count > 0 {
            Some(format!(
                "Found {} critical and {} high-risk hubs. Changes to these functions may have widespread impact.",
                critical_count, high_count
            ))
        } else {
            None
        }
    };

    HubReport {
        hubs,
        total_nodes,
        hub_count,
        measures_used,
        by_in_degree,
        by_out_degree,
        by_pagerank,
        by_betweenness,
        pagerank_info,
        explanation,
    }
}

// =============================================================================
// Tests
// =============================================================================

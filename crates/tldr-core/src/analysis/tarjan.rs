//! Iterative Tarjan's Strongly Connected Components Algorithm
//!
//! This module implements Tarjan's algorithm for finding strongly connected
//! components (SCCs) in a directed graph. The implementation is **iterative**
//! rather than recursive to avoid stack overflow on deeply nested graphs
//! (addresses A1 premortem risk).
//!
//! # Algorithm Overview
//!
//! Tarjan's algorithm finds all SCCs in O(V + E) time where:
//! - V = number of vertices (nodes)
//! - E = number of edges
//!
//! An SCC is a maximal set of vertices such that there is a path from every
//! vertex to every other vertex in the set. SCCs with size > 1 represent cycles.
//!
//! # Key Design Decisions
//!
//! 1. **Iterative Implementation**: Uses explicit work stack instead of recursion
//!    to handle graphs with 10,000+ nodes without stack overflow.
//!
//! 2. **Generic API**: Works with any node type that implements `Hash + Eq + Clone`.
//!
//! 3. **Phase-based State Machine**: Each node goes through phases:
//!    - Entering: Initialize index/lowlink, push to stack
//!    - ProcessingSuccessors: Visit each successor
//!    - Finishing: Check if SCC root, pop stack to form SCC
//!
//! # References
//!
//! - Original paper: Tarjan, R. E. (1972). "Depth-first search and linear graph algorithms"
//! - Spec: architecture-spec.md lines 244-280
//! - Premortem: architecture-premortem-1.yaml (A1, A4)

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::path::PathBuf;

use crate::types::{CycleGranularity, CycleReport, FunctionRef, SCC};

/// Trait for types that can be converted to a string representation for SCC output.
///
/// This is needed because PathBuf doesn't implement Display directly.
pub trait ToSccString {
    /// Convert to a string representation for SCC output.
    fn to_scc_string(&self) -> String;
}

impl ToSccString for String {
    fn to_scc_string(&self) -> String {
        self.clone()
    }
}

impl ToSccString for &str {
    fn to_scc_string(&self) -> String {
        (*self).to_string()
    }
}

impl ToSccString for PathBuf {
    fn to_scc_string(&self) -> String {
        self.to_string_lossy().to_string()
    }
}

impl ToSccString for FunctionRef {
    fn to_scc_string(&self) -> String {
        format!("{}:{}", self.file.display(), self.name)
    }
}

// =============================================================================
// Core Algorithm Types
// =============================================================================

/// Phase of Tarjan's algorithm for a node
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TarjanPhase {
    /// Just discovered this node, initialize state
    Entering,
    /// Processing successors (edges out from this node)
    ProcessingSuccessors,
    /// All successors processed, check if SCC root
    Finishing,
}

/// State tracked for each node during Tarjan's algorithm
#[derive(Debug, Clone)]
struct NodeState {
    /// Discovery index (order in which node was first visited)
    index: usize,
    /// Lowest index reachable from this node's subtree
    lowlink: usize,
    /// Whether this node is currently on the DFS stack
    on_stack: bool,
    /// Current successor index being processed (for iterative DFS)
    successor_idx: usize,
}

impl NodeState {
    fn new(index: usize) -> Self {
        Self {
            index,
            lowlink: index,
            on_stack: true,
            successor_idx: 0,
        }
    }
}

// =============================================================================
// Main Algorithm
// =============================================================================

/// Find all strongly connected components in a directed graph.
///
/// This is an iterative implementation of Tarjan's algorithm that uses an
/// explicit work stack instead of recursion, allowing it to handle graphs
/// with 10,000+ nodes without stack overflow.
///
/// # Arguments
///
/// * `nodes` - All nodes in the graph
/// * `edges` - Adjacency list: for each node, the list of nodes it has edges to
///
/// # Returns
///
/// A vector of SCCs. Each SCC contains the nodes that form a strongly connected
/// component. Only SCCs with size > 1 (actual cycles) are typically relevant
/// for cycle detection.
///
/// # Example
///
/// ```ignore
/// use std::collections::HashMap;
/// use tldr_core::analysis::tarjan::find_sccs;
///
/// let nodes = vec!["A", "B", "C"];
/// let mut edges = HashMap::new();
/// edges.insert("A", vec!["B"]);
/// edges.insert("B", vec!["C"]);
/// edges.insert("C", vec!["A"]); // Creates A -> B -> C -> A cycle
///
/// let sccs = find_sccs(&nodes, &edges);
/// assert_eq!(sccs.len(), 1);
/// assert_eq!(sccs[0].nodes.len(), 3);
/// ```
pub fn find_sccs<N>(nodes: &[N], edges: &HashMap<N, Vec<N>>) -> Vec<SCC>
where
    N: Hash + Eq + Clone + ToSccString,
{
    let mut index_counter: usize = 0;
    let mut states: HashMap<N, NodeState> = HashMap::new();
    let mut node_stack: Vec<N> = Vec::new();
    let mut sccs: Vec<SCC> = Vec::new();

    // Work stack for iterative DFS: (node, phase)
    let mut work_stack: Vec<(N, TarjanPhase)> = Vec::new();

    // Process each node as a potential starting point
    for start_node in nodes {
        if states.contains_key(start_node) {
            continue; // Already processed
        }

        work_stack.push((start_node.clone(), TarjanPhase::Entering));

        while let Some((node, phase)) = work_stack.pop() {
            match phase {
                TarjanPhase::Entering => {
                    // Initialize this node
                    states.insert(node.clone(), NodeState::new(index_counter));
                    index_counter += 1;
                    node_stack.push(node.clone());

                    // Move to processing successors
                    work_stack.push((node, TarjanPhase::ProcessingSuccessors));
                }

                TarjanPhase::ProcessingSuccessors => {
                    let successors = edges.get(&node).cloned().unwrap_or_default();
                    let state = states.get_mut(&node).unwrap();
                    let successor_idx = state.successor_idx;

                    if successor_idx < successors.len() {
                        let successor = &successors[successor_idx];
                        state.successor_idx += 1;

                        if !states.contains_key(successor) {
                            // Successor not yet visited - recurse (push to work stack)
                            work_stack.push((node.clone(), TarjanPhase::ProcessingSuccessors));
                            work_stack.push((successor.clone(), TarjanPhase::Entering));
                        } else if states.get(successor).map(|s| s.on_stack).unwrap_or(false) {
                            // Successor is on stack - update lowlink
                            let succ_index = states.get(successor).unwrap().index;
                            let state = states.get_mut(&node).unwrap();
                            state.lowlink = state.lowlink.min(succ_index);
                            // Continue processing remaining successors
                            work_stack.push((node.clone(), TarjanPhase::ProcessingSuccessors));
                        } else {
                            // Successor already processed and not on stack - continue
                            work_stack.push((node.clone(), TarjanPhase::ProcessingSuccessors));
                        }
                    } else {
                        // All successors processed - move to finishing
                        work_stack.push((node, TarjanPhase::Finishing));
                    }
                }

                TarjanPhase::Finishing => {
                    let state = states.get(&node).unwrap();
                    let is_root = state.lowlink == state.index;

                    if is_root {
                        // This node is the root of an SCC
                        let mut scc_nodes: Vec<String> = Vec::new();

                        loop {
                            let w = node_stack.pop().expect("Stack should not be empty");
                            states.get_mut(&w).unwrap().on_stack = false;
                            scc_nodes.push(w.to_scc_string());
                            if w == node {
                                break;
                            }
                        }

                        // Create SCC (we'll add edges later if needed)
                        let scc = SCC::new(scc_nodes);
                        sccs.push(scc);
                    }

                    // Update parent's lowlink (if we're returning from a recursive call)
                    // This is handled by checking the finished node's lowlink in the parent's
                    // ProcessingSuccessors phase on the next iteration
                    if let Some((parent_node, TarjanPhase::ProcessingSuccessors)) =
                        work_stack.last()
                    {
                        let node_lowlink = states.get(&node).unwrap().lowlink;
                        let parent_state = states.get_mut(parent_node).unwrap();
                        parent_state.lowlink = parent_state.lowlink.min(node_lowlink);
                    }
                }
            }
        }
    }

    sccs
}

/// Detect cycles in a call graph and return a CycleReport.
///
/// This is a convenience function that wraps `find_sccs` and filters to
/// only include SCCs with size > 1 (actual cycles).
///
/// # Arguments
///
/// * `graph` - Call graph as adjacency list (caller -> callees)
/// * `granularity` - Whether to report at function or file level
///
/// # Returns
///
/// A `CycleReport` containing all detected cycles.
pub fn detect_cycles(
    graph: &HashMap<FunctionRef, Vec<FunctionRef>>,
    granularity: CycleGranularity,
) -> CycleReport {
    // Collect all nodes
    let mut all_nodes: HashSet<FunctionRef> = HashSet::new();
    for (src, dsts) in graph {
        all_nodes.insert(src.clone());
        for dst in dsts {
            all_nodes.insert(dst.clone());
        }
    }
    let nodes: Vec<FunctionRef> = all_nodes.into_iter().collect();

    // Find SCCs
    let sccs = find_sccs(&nodes, graph);

    // Build report with only cycles (size > 1)
    let mut report = CycleReport::new(granularity);

    for scc in sccs {
        if scc.size > 1 {
            // Add edges within the SCC
            let scc_nodes: HashSet<&String> = scc.nodes.iter().collect();
            let mut edges: Vec<(String, String)> = Vec::new();

            for node_str in &scc.nodes {
                // Find the FunctionRef for this node
                for (src, dsts) in graph {
                    if src.to_scc_string() == *node_str {
                        for dst in dsts {
                            let dst_str = dst.to_scc_string();
                            if scc_nodes.contains(&dst_str) {
                                edges.push((node_str.clone(), dst_str));
                            }
                        }
                    }
                }
            }

            let scc_with_edges = scc.with_edges(edges);
            report.add_cycle(scc_with_edges);
        }
    }

    report.with_explanation()
}

// =============================================================================
// Tests
// =============================================================================

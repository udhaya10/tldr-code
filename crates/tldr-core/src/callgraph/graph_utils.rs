//! Graph utilities for call graph analysis
//!
//! This module provides shared graph infrastructure used by analysis commands:
//!
//! - `build_reverse_graph`: Build callee -> [callers] mapping
//! - `build_forward_graph`: Build caller -> [callees] mapping
//! - `collect_nodes`: Extract all unique function references
//!
//! # Example
//!
//! ```rust,ignore
//! use tldr_core::callgraph::graph_utils::{build_forward_graph, build_reverse_graph, collect_nodes};
//! use tldr_core::types::ProjectCallGraph;
//!
//! let graph = ProjectCallGraph::new();
//! // ... add edges ...
//!
//! let forward = build_forward_graph(&graph);
//! let reverse = build_reverse_graph(&graph);
//! let nodes = collect_nodes(&graph);
//! ```

use std::collections::{HashMap, HashSet};

use crate::types::{FunctionRef, ProjectCallGraph};

/// Build reverse graph: callee -> [callers]
///
/// For each function, returns the list of functions that call it.
/// This is useful for impact analysis ("who calls this function?").
///
/// # Arguments
/// * `call_graph` - The project call graph
///
/// # Returns
/// A HashMap where keys are callees (FunctionRef) and values are vectors of callers
pub fn build_reverse_graph(
    call_graph: &ProjectCallGraph,
) -> HashMap<FunctionRef, Vec<FunctionRef>> {
    let mut reverse: HashMap<FunctionRef, Vec<FunctionRef>> = HashMap::new();

    for edge in call_graph.edges() {
        let callee = FunctionRef::new(edge.dst_file.clone(), edge.dst_func.clone());
        let caller = FunctionRef::new(edge.src_file.clone(), edge.src_func.clone());

        reverse.entry(callee).or_default().push(caller);
    }

    reverse
}

/// Build forward graph: caller -> [callees]
///
/// For each function, returns the list of functions it calls.
/// This is useful for hubs analysis (out-degree) and PageRank computation.
///
/// # Arguments
/// * `call_graph` - The project call graph
///
/// # Returns
/// A HashMap where keys are callers (FunctionRef) and values are vectors of callees
pub fn build_forward_graph(
    call_graph: &ProjectCallGraph,
) -> HashMap<FunctionRef, Vec<FunctionRef>> {
    let mut forward: HashMap<FunctionRef, Vec<FunctionRef>> = HashMap::new();

    for edge in call_graph.edges() {
        let caller = FunctionRef::new(edge.src_file.clone(), edge.src_func.clone());
        let callee = FunctionRef::new(edge.dst_file.clone(), edge.dst_func.clone());

        forward.entry(caller).or_default().push(callee);
    }

    forward
}

/// Collect all unique nodes (function references) from the call graph
///
/// Returns a HashSet of all functions that appear as either caller or callee
/// in any edge of the call graph.
///
/// # Arguments
/// * `call_graph` - The project call graph
///
/// # Returns
/// A HashSet of unique FunctionRef values
pub fn collect_nodes(call_graph: &ProjectCallGraph) -> HashSet<FunctionRef> {
    let mut nodes = HashSet::new();

    for edge in call_graph.edges() {
        nodes.insert(FunctionRef::new(
            edge.src_file.clone(),
            edge.src_func.clone(),
        ));
        nodes.insert(FunctionRef::new(
            edge.dst_file.clone(),
            edge.dst_func.clone(),
        ));
    }

    nodes
}

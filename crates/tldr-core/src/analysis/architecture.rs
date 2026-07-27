//! Architecture analysis (spec Section 2.2.5)
//!
//! Detect architectural layers from call patterns.
//!
//! # Layer Detection Rules
//! - Entry layer: High calls_out, low calls_in (controllers/handlers)
//! - Middle layer (Service): Balanced calls_out and calls_in
//! - Leaf layer (Utility): Low calls_out, high calls_in
//! - DynamicDispatch: Directories named languages/, handlers/, plugins/, adapters/
//!
//! # Features
//! - Detects circular dependencies between directories
//! - Groups functions by directory
//! - Infers layer types based on call patterns
//! - Tarjan SCC algorithm for precise cycle detection (A1, A4 mitigations)

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::types::{
    ArchitectureReport, CircularDep, CycleGranularity, CycleReport, DirStats, FunctionRef,
    LayerType, ProjectCallGraph, SCC,
};
use crate::TldrResult;

use super::tarjan::find_sccs;

/// Analyze codebase architecture.
///
/// # Arguments
/// * `call_graph` - Project call graph
///
/// # Returns
/// * `Ok(ArchitectureReport)` - Architecture analysis results
pub fn architecture_analysis(call_graph: &ProjectCallGraph) -> TldrResult<ArchitectureReport> {
    // Collect all functions and their directories
    let mut dir_functions: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    let mut func_calls_out: HashMap<FunctionRef, usize> = HashMap::new();
    let mut func_calls_in: HashMap<FunctionRef, usize> = HashMap::new();

    // Build call counts
    for edge in call_graph.edges() {
        let src_ref = FunctionRef::new(edge.src_file.clone(), &edge.src_func);
        let dst_ref = FunctionRef::new(edge.dst_file.clone(), &edge.dst_func);

        *func_calls_out.entry(src_ref.clone()).or_insert(0) += 1;
        *func_calls_in.entry(dst_ref.clone()).or_insert(0) += 1;

        // Track functions by directory
        if let Some(dir) = edge.src_file.parent() {
            dir_functions
                .entry(dir.to_path_buf())
                .or_default()
                .insert(edge.src_func.clone());
        }
        if let Some(dir) = edge.dst_file.parent() {
            dir_functions
                .entry(dir.to_path_buf())
                .or_default()
                .insert(edge.dst_func.clone());
        }
    }

    // Classify functions into layers
    let mut entry_layer = Vec::new();
    let mut middle_layer = Vec::new();
    let mut leaf_layer = Vec::new();

    // Get all unique functions
    let mut all_functions: HashSet<FunctionRef> = HashSet::new();
    for edge in call_graph.edges() {
        all_functions.insert(FunctionRef::new(edge.src_file.clone(), &edge.src_func));
        all_functions.insert(FunctionRef::new(edge.dst_file.clone(), &edge.dst_func));
    }

    for func_ref in all_functions {
        let calls_out = func_calls_out.get(&func_ref).copied().unwrap_or(0);
        let calls_in = func_calls_in.get(&func_ref).copied().unwrap_or(0);

        if calls_in == 0 && calls_out > 0 {
            // Not called by anyone, but calls others -> Entry layer
            entry_layer.push(func_ref);
        } else if calls_out == 0 && calls_in > 0 {
            // Called but doesn't call others -> Leaf layer
            leaf_layer.push(func_ref);
        } else if calls_in > 0 && calls_out > 0 {
            // Both called and calls others -> Middle layer
            middle_layer.push(func_ref);
        }
    }

    // Build directory statistics
    let mut directories: HashMap<PathBuf, DirStats> = HashMap::new();

    for (dir, functions) in &dir_functions {
        let mut total_calls_out = 0;
        let mut total_calls_in = 0;

        for func_name in functions {
            // Find all files in this directory with this function
            for edge in call_graph.edges() {
                if edge.src_file.parent() == Some(dir.as_path()) && &edge.src_func == func_name {
                    total_calls_out += 1;
                }
                if edge.dst_file.parent() == Some(dir.as_path()) && &edge.dst_func == func_name {
                    total_calls_in += 1;
                }
            }
        }

        directories.insert(
            dir.clone(),
            DirStats {
                functions: functions.iter().cloned().collect(),
                calls_out: total_calls_out,
                calls_in: total_calls_in,
            },
        );
    }

    // Detect circular dependencies between directories
    let circular_dependencies = detect_circular_dependencies(call_graph);

    // Infer layer types for directories
    let mut inferred_layers: HashMap<PathBuf, LayerType> = HashMap::new();

    for (dir, stats) in &directories {
        let layer = infer_layer_type(dir, stats);
        inferred_layers.insert(dir.clone(), layer);
    }

    Ok(ArchitectureReport {
        entry_layer,
        middle_layer,
        leaf_layer,
        directories,
        circular_dependencies,
        inferred_layers,
    })
}

/// Infer layer type for a directory based on call patterns and naming
fn infer_layer_type(dir: &Path, stats: &DirStats) -> LayerType {
    let dir_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();

    // Check for dynamic dispatch patterns
    let dynamic_dispatch_names = [
        "languages",
        "handlers",
        "plugins",
        "adapters",
        "drivers",
        "providers",
        "backends",
        "strategies",
    ];
    if dynamic_dispatch_names.contains(&dir_name.as_str()) {
        return LayerType::DynamicDispatch;
    }

    // Entry layer patterns
    let entry_names = [
        "api",
        "routes",
        "controllers",
        "endpoints",
        "views",
        "cli",
        "commands",
        "main",
    ];
    if entry_names.contains(&dir_name.as_str()) {
        return LayerType::Entry;
    }

    // Service layer patterns
    let service_names = ["services", "business", "domain", "core", "logic"];
    if service_names.contains(&dir_name.as_str()) {
        return LayerType::Service;
    }

    // Utility layer patterns
    let utility_names = ["utils", "helpers", "common", "shared", "lib", "tools"];
    if utility_names.contains(&dir_name.as_str()) {
        return LayerType::Utility;
    }

    // Infer from call patterns if no name match
    let ratio = if stats.calls_in > 0 {
        stats.calls_out as f64 / stats.calls_in as f64
    } else if stats.calls_out > 0 {
        f64::INFINITY
    } else {
        1.0
    };

    if ratio > 2.0 || stats.calls_in == 0 {
        // High calls_out relative to calls_in -> Entry
        LayerType::Entry
    } else if ratio < 0.5 || stats.calls_out == 0 {
        // Low calls_out relative to calls_in -> Utility
        LayerType::Utility
    } else {
        // Balanced -> Service
        LayerType::Service
    }
}

/// Detect circular dependencies between directories
fn detect_circular_dependencies(call_graph: &ProjectCallGraph) -> Vec<CircularDep> {
    let mut circular = Vec::new();
    let mut dir_edges: HashMap<PathBuf, HashSet<PathBuf>> = HashMap::new();

    // Build directory-level call graph
    for edge in call_graph.edges() {
        if let (Some(src_dir), Some(dst_dir)) = (edge.src_file.parent(), edge.dst_file.parent()) {
            if src_dir != dst_dir {
                dir_edges
                    .entry(src_dir.to_path_buf())
                    .or_default()
                    .insert(dst_dir.to_path_buf());
            }
        }
    }

    // Check for bidirectional edges (A -> B and B -> A)
    let mut checked: HashSet<(PathBuf, PathBuf)> = HashSet::new();

    for (dir_a, targets) in &dir_edges {
        for dir_b in targets {
            if checked.contains(&(dir_a.clone(), dir_b.clone()))
                || checked.contains(&(dir_b.clone(), dir_a.clone()))
            {
                continue;
            }

            checked.insert((dir_a.clone(), dir_b.clone()));

            // Check if there's a reverse edge
            if let Some(reverse_targets) = dir_edges.get(dir_b) {
                if reverse_targets.contains(dir_a) {
                    circular.push(CircularDep {
                        a: dir_a.clone(),
                        b: dir_b.clone(),
                    });
                }
            }
        }
    }

    circular
}

/// Detect circular dependencies using Tarjan's SCC algorithm.
///
/// This is a more precise cycle detection method that finds all strongly
/// connected components, including cycles with 3+ nodes (e.g., A -> B -> C -> A).
///
/// The basic `detect_circular_dependencies` only finds bidirectional edges (A <-> B).
/// This function uses Tarjan's iterative algorithm to find all cycles.
///
/// # Arguments
///
/// * `call_graph` - Project call graph
/// * `granularity` - Whether to detect cycles at function or file level
///
/// # Returns
///
/// A `CycleReport` with all detected cycles (SCCs with size > 1).
///
/// # Mitigations
///
/// - A1: Uses iterative Tarjan (no stack overflow on deep graphs)
/// - A4: Finds 3+ node cycles, not just bidirectional edges
pub fn find_circular_dependencies_tarjan(
    call_graph: &ProjectCallGraph,
    granularity: CycleGranularity,
) -> CycleReport {
    match granularity {
        CycleGranularity::Function => find_function_level_cycles(call_graph),
        CycleGranularity::File => find_file_level_cycles(call_graph),
    }
}

/// Find cycles at the function level.
fn find_function_level_cycles(call_graph: &ProjectCallGraph) -> CycleReport {
    // Build function-level graph
    let mut graph: HashMap<FunctionRef, Vec<FunctionRef>> = HashMap::new();
    let mut all_nodes: HashSet<FunctionRef> = HashSet::new();

    for edge in call_graph.edges() {
        let src = FunctionRef::new(edge.src_file.clone(), &edge.src_func);
        let dst = FunctionRef::new(edge.dst_file.clone(), &edge.dst_func);

        all_nodes.insert(src.clone());
        all_nodes.insert(dst.clone());

        graph.entry(src).or_default().push(dst);
    }

    let nodes: Vec<FunctionRef> = all_nodes.into_iter().collect();

    // Run Tarjan
    let sccs = find_sccs(&nodes, &graph);

    // Build report
    let mut report = CycleReport::new(CycleGranularity::Function);

    for scc in sccs {
        if scc.size > 1 {
            // Add edges within the SCC
            let scc_nodes: HashSet<&String> = scc.nodes.iter().collect();
            let mut edges: Vec<(String, String)> = Vec::new();

            for node_str in &scc.nodes {
                for (src, dsts) in &graph {
                    if src.to_string() == *node_str {
                        for dst in dsts {
                            let dst_str = dst.to_string();
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

/// Find cycles at the file/directory level.
fn find_file_level_cycles(call_graph: &ProjectCallGraph) -> CycleReport {
    // Build directory-level graph
    let mut graph: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    let mut all_nodes: HashSet<PathBuf> = HashSet::new();

    for edge in call_graph.edges() {
        if let (Some(src_dir), Some(dst_dir)) = (edge.src_file.parent(), edge.dst_file.parent()) {
            if src_dir != dst_dir {
                let src = src_dir.to_path_buf();
                let dst = dst_dir.to_path_buf();

                all_nodes.insert(src.clone());
                all_nodes.insert(dst.clone());

                // Avoid duplicate edges
                let entry = graph.entry(src).or_default();
                if !entry.contains(&dst) {
                    entry.push(dst);
                }
            }
        }
    }

    let nodes: Vec<PathBuf> = all_nodes.into_iter().collect();

    // Run Tarjan
    let sccs = find_sccs(&nodes, &graph);

    // Build report
    let mut report = CycleReport::new(CycleGranularity::File);

    for scc in sccs {
        if scc.size > 1 {
            // Add edges within the SCC
            let scc_nodes: HashSet<&String> = scc.nodes.iter().collect();
            let mut edges: Vec<(String, String)> = Vec::new();

            for node_str in &scc.nodes {
                // Find matching PathBuf and look up edges
                for (src, dsts) in &graph {
                    if src.to_string_lossy() == *node_str {
                        for dst in dsts {
                            let dst_str = dst.to_string_lossy().to_string();
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

/// Convert legacy CircularDep format to the new CycleReport format.
///
/// This is a helper for backwards compatibility during the transition.
pub fn circular_deps_to_cycle_report(deps: &[CircularDep]) -> CycleReport {
    let mut report = CycleReport::new(CycleGranularity::File);

    for dep in deps {
        // Each bidirectional edge is a 2-node cycle
        let nodes = vec![
            dep.a.to_string_lossy().to_string(),
            dep.b.to_string_lossy().to_string(),
        ];
        let edges = vec![
            (
                dep.a.to_string_lossy().to_string(),
                dep.b.to_string_lossy().to_string(),
            ),
            (
                dep.b.to_string_lossy().to_string(),
                dep.a.to_string_lossy().to_string(),
            ),
        ];

        let scc = SCC::new(nodes).with_edges(edges);
        report.add_cycle(scc);
    }

    report.with_explanation()
}

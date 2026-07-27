//! Inheritance pattern detection
//!
//! Detects patterns in inheritance hierarchies:
//! - ABC/Protocol/Interface detection
//! - Mixin class detection
//! - Diamond inheritance detection (A2 - optimized using BFS + set intersection)
//!
//! # Diamond Detection Algorithm (A2 mitigation)
//!
//! Instead of O(n^3) path enumeration, we use BFS + set intersection:
//! 1. For each class with 2+ parents, compute ancestor_set(P_i) for each parent
//! 2. Diamond common ancestors = intersection of all ancestor_sets
//! 3. Complexity: O(|nodes| * |edges|) for BFS per class

use std::collections::{HashMap, HashSet};

use crate::types::{DiamondPattern, InheritanceGraph};

/// Detect ABC/Protocol/Interface patterns and mark nodes
pub fn detect_abc_protocol(graph: &mut InheritanceGraph) {
    // For Python: ABC inheritors and @abstractmethod
    // For TypeScript: abstract class and interface
    // For Rust: trait definitions

    for (_name, node) in graph.nodes.iter_mut() {
        // Check if bases contain ABC
        if node.bases.iter().any(|b| b == "ABC" || b == "ABCMeta") {
            node.is_abstract = Some(true);
        }

        // Check if bases contain Protocol
        if node
            .bases
            .iter()
            .any(|b| b == "Protocol" || b.ends_with(".Protocol"))
        {
            node.protocol = Some(true);
        }
    }
}

/// Detect mixin classes using naming heuristics and usage patterns
///
/// Heuristics:
/// 1. Name ends with "Mixin" (case-insensitive) -> definite mixin
/// 2. Appears as secondary base (not first) in 2+ classes with no bases itself -> likely mixin (A6)
pub fn detect_mixins(graph: &mut InheritanceGraph) {
    // Pre-compute secondary_base_count in single pass (A6 optimization)
    let mut secondary_base_count: HashMap<String, usize> = HashMap::new();

    for node in graph.nodes.values() {
        if node.bases.len() > 1 {
            // Secondary bases are all bases except the first
            for base in &node.bases[1..] {
                *secondary_base_count.entry(base.clone()).or_insert(0) += 1;
            }
        }
    }

    // Mark mixins
    for (name, node) in graph.nodes.iter_mut() {
        // Heuristic 1: Name ends with "Mixin"
        if name.to_lowercase().ends_with("mixin") {
            node.mixin = Some(true);
            continue;
        }

        // Heuristic 2: Used as secondary base 2+ times and has no bases
        if node.bases.is_empty() {
            if let Some(&count) = secondary_base_count.get(name) {
                if count >= 2 {
                    node.mixin = Some(true);
                }
            }
        }
    }
}

/// Detect diamond inheritance patterns using BFS + set intersection (A2 optimization)
///
/// A diamond occurs when a class has multiple paths to the same ancestor
/// through different immediate parents.
///
/// ```text
///        A          <- common_ancestor
///       / \
///      B   C
///       \ /
///        D          <- class_name (diamond)
/// ```
pub fn detect_diamonds(graph: &InheritanceGraph) -> Vec<DiamondPattern> {
    let mut diamonds = Vec::new();

    // For each class with 2+ parents
    for (class_name, parents_raw) in graph.multi_parent_classes() {
        // inheritance-and-dead-cleanup-v1 (M4): require DISTINCT parents.
        // Duplicate edges (M5 bug) caused linear chains like
        // `CSSTransition -> Animation -> EventTarget` to be misreported as
        // diamonds. A real diamond requires two distinct immediate parents
        // converging on a common ancestor.
        let mut seen = HashSet::new();
        let parents: Vec<String> = parents_raw
            .iter()
            .filter(|p| seen.insert((*p).clone()))
            .cloned()
            .collect();

        if parents.len() < 2 {
            continue;
        }

        // Compute ancestor sets for each (distinct) parent using BFS
        let ancestor_sets: Vec<HashSet<String>> = parents
            .iter()
            .map(|parent| graph.ancestors_bfs(parent))
            .collect();

        // Find common ancestors (intersection of all ancestor sets)
        if ancestor_sets.is_empty() {
            continue;
        }

        let common: HashSet<String> = if ancestor_sets.len() == 1 {
            ancestor_sets[0].clone()
        } else {
            ancestor_sets[1..]
                .iter()
                .fold(ancestor_sets[0].clone(), |acc, s| {
                    acc.intersection(s).cloned().collect()
                })
        };

        // For each common ancestor, create a diamond pattern.
        // A real diamond requires two DISTINCT paths (through different
        // immediate parents) converging on the same ancestor.
        for ancestor in common {
            let paths = compute_paths_to_ancestor(graph, class_name, &ancestor, &parents);

            // Deduplicate paths (defensive — different parents may share
            // intermediate path segments but should yield distinct vectors).
            let mut unique_paths: Vec<Vec<String>> = Vec::new();
            for p in paths {
                if !unique_paths.iter().any(|q| q == &p) {
                    unique_paths.push(p);
                }
            }

            if unique_paths.len() >= 2 {
                diamonds.push(DiamondPattern {
                    class_name: class_name.clone(),
                    common_ancestor: ancestor,
                    paths: unique_paths,
                });
            }
        }
    }

    diamonds
}

/// Compute paths from class to ancestor through each parent
fn compute_paths_to_ancestor(
    graph: &InheritanceGraph,
    class_name: &str,
    ancestor: &str,
    parents: &[String],
) -> Vec<Vec<String>> {
    let mut paths = Vec::new();

    for parent in parents {
        // Check if this parent has a path to the ancestor
        if let Some(path) = find_path_to_ancestor(graph, parent, ancestor) {
            let mut full_path = vec![class_name.to_string()];
            full_path.extend(path);
            paths.push(full_path);
        }
    }

    paths
}

/// Find a path from start to ancestor using BFS
fn find_path_to_ancestor(
    graph: &InheritanceGraph,
    start: &str,
    ancestor: &str,
) -> Option<Vec<String>> {
    use std::collections::VecDeque;

    if start == ancestor {
        return Some(vec![start.to_string()]);
    }

    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();
    let mut parent_map: HashMap<String, String> = HashMap::new();

    queue.push_back(start.to_string());
    visited.insert(start.to_string());

    while let Some(current) = queue.pop_front() {
        if let Some(parents) = graph.parents.get(&current) {
            for parent in parents {
                if !visited.contains(parent) {
                    visited.insert(parent.clone());
                    parent_map.insert(parent.clone(), current.clone());
                    queue.push_back(parent.clone());

                    if parent == ancestor {
                        // Reconstruct path
                        let mut path = vec![ancestor.to_string()];
                        let mut curr = ancestor.to_string();
                        while let Some(child) = parent_map.get(&curr) {
                            path.push(child.clone());
                            curr = child.clone();
                        }
                        path.reverse();
                        return Some(path);
                    }
                }
            }
        }
    }

    None
}

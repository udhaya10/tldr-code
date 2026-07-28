//! Resident, generation-bound call graph projection.
//!
//! Persistence and generation publication belong to [`super::ArtifactStore`].
//! This module is deliberately a derived in-memory index: rebuilding it from a
//! published generation is linear in nodes plus edges and never creates a
//! second durable source of truth.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::types::{CallerTree, ImpactReport};
use crate::{Language, ProjectCallGraph, TldrError, TldrResult};

use super::{FileFacts, ProjectCallEdgeFact};

/// Dense function identifier. Snapshot order is the canonical paging order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FuncId(u32);

impl FuncId {
    /// Zero-based dense index within one immutable graph snapshot.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Structure-of-arrays node payload addressed by [`FuncId`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionNode {
    /// Stable lowercase language.
    pub language: String,
    /// Root-relative source file.
    pub file: String,
    /// Definition name.
    pub name: String,
    /// Definition kind, or `external` when known only from an edge.
    pub kind: String,
    /// One-indexed start line, zero when unavailable.
    pub line_start: u32,
    /// One-indexed inclusive end line, zero when unavailable.
    pub line_end: u32,
    /// Stable display signature.
    pub signature: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct NodeKey {
    language: String,
    file: String,
    name: String,
    line: u32,
}

/// Immutable forward/reverse CSR plus symbol-addressing indexes.
#[derive(Clone, Debug, Default)]
pub struct GraphSnapshot {
    nodes: Vec<FunctionNode>,
    forward_offsets: Vec<usize>,
    forward_targets: Vec<FuncId>,
    reverse_offsets: Vec<usize>,
    reverse_targets: Vec<FuncId>,
    by_name: HashMap<String, Vec<FuncId>>,
    by_file: HashMap<String, Vec<FuncId>>,
}

impl GraphSnapshot {
    /// Build the same projection from an explicit one-shot graph.
    ///
    /// This performs no persistence but preserves identical addressing and
    /// traversal semantics for the `--oneshot` parity path.
    pub fn from_project_call_graph(graph: &ProjectCallGraph, language: Language) -> Self {
        let edges = graph
            .edges()
            .map(|edge| ProjectCallEdgeFact {
                language: language.as_str().into(),
                source_file: edge.src_file.to_string_lossy().replace('\\', "/"),
                caller: edge.src_func.clone(),
                destination_file: edge.dst_file.to_string_lossy().replace('\\', "/"),
                callee: edge.dst_func.clone(),
                call_type: "direct".into(),
            })
            .collect::<Vec<_>>();
        Self::build(&HashMap::new(), &edges)
    }

    pub(crate) fn build(
        files: &HashMap<String, Arc<FileFacts>>,
        edges: &[ProjectCallEdgeFact],
    ) -> Self {
        let mut keyed = Vec::<(NodeKey, FunctionNode)>::new();
        for facts in files.values() {
            for definition in &facts.definitions {
                let key = NodeKey {
                    language: facts.language.clone(),
                    file: facts.path.clone(),
                    name: definition.name.clone(),
                    line: definition.line_start,
                };
                keyed.push((
                    key,
                    FunctionNode {
                        language: facts.language.clone(),
                        file: facts.path.clone(),
                        name: definition.name.clone(),
                        kind: definition.kind.clone(),
                        line_start: definition.line_start,
                        line_end: definition.line_end,
                        signature: definition.signature.clone(),
                    },
                ));
            }
        }

        // Resolvers identify an edge endpoint by qualified file + name. Add a
        // line-zero node only when ingestion has no matching definition fact.
        let mut defined = keyed
            .iter()
            .map(|(key, _)| (key.language.clone(), key.file.clone(), key.name.clone()))
            .collect::<HashSet<_>>();
        for edge in edges {
            for (file, name) in [
                (&edge.source_file, &edge.caller),
                (&edge.destination_file, &edge.callee),
            ] {
                let base = (edge.language.clone(), file.clone(), name.clone());
                if defined.insert(base.clone()) {
                    keyed.push((
                        NodeKey {
                            language: base.0.clone(),
                            file: base.1.clone(),
                            name: base.2.clone(),
                            line: 0,
                        },
                        FunctionNode {
                            language: base.0,
                            file: base.1,
                            name: base.2,
                            kind: "external".into(),
                            line_start: 0,
                            line_end: 0,
                            signature: String::new(),
                        },
                    ));
                }
            }
        }

        keyed.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        keyed.dedup_by(|left, right| left.0 == right.0);
        let nodes = keyed.into_iter().map(|(_, node)| node).collect::<Vec<_>>();
        let mut by_identity = HashMap::<(String, String, String), Vec<FuncId>>::new();
        let mut by_name = HashMap::<String, Vec<FuncId>>::new();
        let mut by_file = HashMap::<String, Vec<FuncId>>::new();
        for (index, node) in nodes.iter().enumerate() {
            let id = FuncId(u32::try_from(index).expect("function count exceeds u32"));
            by_identity
                .entry((node.language.clone(), node.file.clone(), node.name.clone()))
                .or_default()
                .push(id);
            by_name.entry(node.name.clone()).or_default().push(id);
            by_file.entry(node.file.clone()).or_default().push(id);
        }

        let mut forward = vec![Vec::new(); nodes.len()];
        let mut reverse = vec![Vec::new(); nodes.len()];
        for edge in edges {
            let source = by_identity
                .get(&(
                    edge.language.clone(),
                    edge.source_file.clone(),
                    edge.caller.clone(),
                ))
                .and_then(|ids| ids.first())
                .copied();
            let destination = by_identity
                .get(&(
                    edge.language.clone(),
                    edge.destination_file.clone(),
                    edge.callee.clone(),
                ))
                .and_then(|ids| ids.first())
                .copied();
            if let (Some(source), Some(destination)) = (source, destination) {
                forward[source.index()].push(destination);
                reverse[destination.index()].push(source);
            }
        }

        let (forward_offsets, forward_targets) = build_csr(forward);
        let (reverse_offsets, reverse_targets) = build_csr(reverse);
        Self {
            nodes,
            forward_offsets,
            forward_targets,
            reverse_offsets,
            reverse_targets,
            by_name,
            by_file,
        }
    }

    /// Number of resident function nodes.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Number of deduplicated directed call edges.
    pub fn edge_count(&self) -> usize {
        self.forward_targets.len()
    }

    /// Read a node payload.
    pub fn node(&self, id: FuncId) -> Option<&FunctionNode> {
        self.nodes.get(id.index())
    }

    /// Iterate in canonical FuncId-major snapshot order.
    pub fn nodes(&self) -> impl Iterator<Item = (FuncId, &FunctionNode)> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (FuncId(index as u32), node))
    }

    /// O(out-degree) forward-neighbor view.
    pub fn out_neighbors(&self, id: FuncId) -> &[FuncId] {
        csr_row(&self.forward_offsets, &self.forward_targets, id)
    }

    /// O(in-degree) reverse-neighbor view.
    pub fn in_neighbors(&self, id: FuncId) -> &[FuncId] {
        csr_row(&self.reverse_offsets, &self.reverse_targets, id)
    }

    /// Exact-name lookup. Ambiguity is preserved as multiple FuncIds.
    pub fn by_name(&self, name: &str) -> &[FuncId] {
        self.by_name.get(name).map_or(&[], Vec::as_slice)
    }

    /// File lookup in canonical snapshot order.
    pub fn by_file(&self, file: &str) -> &[FuncId] {
        self.by_file.get(file).map_or(&[], Vec::as_slice)
    }

    /// Definitions whose source range contains `line`.
    pub fn by_file_line(&self, file: &str, line: u32) -> Vec<FuncId> {
        self.by_file(file)
            .iter()
            .copied()
            .filter(|id| {
                self.node(*id).is_some_and(|node| {
                    node.line_start > 0 && node.line_start <= line && line <= node.line_end
                })
            })
            .collect()
    }

    /// Predicate scan over the SoA node table.
    pub fn scan(&self, mut predicate: impl FnMut(&FunctionNode) -> bool) -> Vec<FuncId> {
        self.nodes()
            .filter_map(|(id, node)| predicate(node).then_some(id))
            .collect()
    }

    /// Reverse breadth-first closure as `(FuncId, depth)` pairs.
    pub fn reverse_bfs(&self, starts: &[FuncId], max_depth: usize) -> Vec<(FuncId, usize)> {
        let mut seen = vec![false; self.nodes.len()];
        let mut queue = VecDeque::new();
        let mut answer = Vec::new();
        for start in starts {
            if start.index() < seen.len() && !seen[start.index()] {
                seen[start.index()] = true;
                queue.push_back((*start, 0));
            }
        }
        while let Some((id, depth)) = queue.pop_front() {
            answer.push((id, depth));
            if depth == max_depth {
                continue;
            }
            for caller in self.in_neighbors(id) {
                if !seen[caller.index()] {
                    seen[caller.index()] = true;
                    queue.push_back((*caller, depth + 1));
                }
            }
        }
        answer
    }

    /// Tarjan strongly connected components, excluding singleton non-cycles.
    pub fn strongly_connected_components(&self) -> Vec<Vec<FuncId>> {
        struct Tarjan<'a> {
            graph: &'a GraphSnapshot,
            next: usize,
            indexes: Vec<Option<usize>>,
            low: Vec<usize>,
            stack: Vec<FuncId>,
            on_stack: Vec<bool>,
            components: Vec<Vec<FuncId>>,
        }
        fn visit(state: &mut Tarjan<'_>, node: FuncId) {
            let index = state.next;
            state.next += 1;
            state.indexes[node.index()] = Some(index);
            state.low[node.index()] = index;
            state.stack.push(node);
            state.on_stack[node.index()] = true;

            for next in state.graph.out_neighbors(node) {
                if state.indexes[next.index()].is_none() {
                    visit(state, *next);
                    state.low[node.index()] = state.low[node.index()].min(state.low[next.index()]);
                } else if state.on_stack[next.index()] {
                    state.low[node.index()] = state.low[node.index()]
                        .min(state.indexes[next.index()].expect("visited node"));
                }
            }
            if state.low[node.index()] == state.indexes[node.index()].expect("visited node") {
                let mut component = Vec::new();
                loop {
                    let member = state.stack.pop().expect("SCC stack");
                    state.on_stack[member.index()] = false;
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                component.sort_unstable();
                let self_cycle = component.len() == 1
                    && state
                        .graph
                        .out_neighbors(component[0])
                        .contains(&component[0]);
                if component.len() > 1 || self_cycle {
                    state.components.push(component);
                }
            }
        }

        let mut state = Tarjan {
            graph: self,
            next: 0,
            indexes: vec![None; self.nodes.len()],
            low: vec![0; self.nodes.len()],
            stack: Vec::new(),
            on_stack: vec![false; self.nodes.len()],
            components: Vec::new(),
        };
        for index in 0..self.nodes.len() {
            if state.indexes[index].is_none() {
                visit(&mut state, FuncId(index as u32));
            }
        }
        state.components.sort_by_key(|component| component[0]);
        state.components
    }

    /// Build a small impact answer directly from reverse CSR.
    pub fn impact_report(
        &self,
        target: &str,
        depth: usize,
        file_filter: Option<&str>,
    ) -> TldrResult<ImpactReport> {
        let candidates = self
            .by_name(target)
            .iter()
            .copied()
            .filter(|id| {
                file_filter.is_none_or(|filter| {
                    self.node(*id)
                        .is_some_and(|node| node.file == filter || node.file.ends_with(filter))
                })
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Err(TldrError::function_not_found(target));
        }
        let ambiguous = candidates.len() > 1;
        let mut targets = HashMap::new();
        for id in candidates {
            let node = self.node(id).expect("indexed node");
            let key = if ambiguous {
                format!("{}:{}", node.file, node.name)
            } else {
                target.to_string()
            };
            let mut path = vec![id];
            targets.insert(key, self.caller_tree(id, depth, &mut path));
        }
        Ok(ImpactReport {
            total_targets: targets.len(),
            targets,
            type_resolution: None,
        })
    }

    fn caller_tree(&self, id: FuncId, remaining: usize, path: &mut Vec<FuncId>) -> CallerTree {
        let node = self.node(id).expect("indexed node");
        let direct = self.in_neighbors(id);
        let mut callers = Vec::new();
        let mut truncated = false;
        let mut note = None;
        if remaining == 0 && !direct.is_empty() {
            truncated = true;
            note = Some("Depth limit reached".into());
        } else {
            for caller in direct {
                if path.contains(caller) {
                    let cycle_node = self.node(*caller).expect("indexed node");
                    callers.push(CallerTree {
                        function: cycle_node.name.clone(),
                        file: cycle_node.file.clone().into(),
                        caller_count: self.in_neighbors(*caller).len(),
                        callers: Vec::new(),
                        truncated: true,
                        note: Some("Cycle detected".into()),
                        confidence: None,
                        receiver_type: None,
                    });
                    continue;
                }
                path.push(*caller);
                callers.push(self.caller_tree(*caller, remaining - 1, path));
                path.pop();
            }
        }
        CallerTree {
            function: node.name.clone(),
            file: node.file.clone().into(),
            caller_count: direct.len(),
            callers,
            truncated,
            note,
            confidence: None,
            receiver_type: None,
        }
    }

    /// Approximate resident bytes owned by the graph projection.
    pub fn resident_bytes(&self) -> usize {
        let strings = self
            .nodes
            .iter()
            .map(|node| {
                node.language.len()
                    + node.file.len()
                    + node.name.len()
                    + node.kind.len()
                    + node.signature.len()
            })
            .sum::<usize>();
        strings
            + self.nodes.len() * std::mem::size_of::<FunctionNode>()
            + (self.forward_offsets.len() + self.reverse_offsets.len())
                * std::mem::size_of::<usize>()
            + (self.forward_targets.len() + self.reverse_targets.len())
                * std::mem::size_of::<FuncId>()
    }
}

fn build_csr(mut rows: Vec<Vec<FuncId>>) -> (Vec<usize>, Vec<FuncId>) {
    let mut offsets = Vec::with_capacity(rows.len() + 1);
    let mut targets = Vec::new();
    offsets.push(0);
    for row in &mut rows {
        row.sort_unstable();
        row.dedup();
        targets.extend_from_slice(row);
        offsets.push(targets.len());
    }
    (offsets, targets)
}

fn csr_row<'a>(offsets: &[usize], targets: &'a [FuncId], id: FuncId) -> &'a [FuncId] {
    let index = id.index();
    if index + 1 >= offsets.len() {
        return &[];
    }
    &targets[offsets[index]..offsets[index + 1]]
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Instant;

    use super::GraphSnapshot;
    use crate::artifact_store::ProjectCallEdgeFact;

    fn edge(caller: &str, callee: &str) -> ProjectCallEdgeFact {
        ProjectCallEdgeFact {
            language: "rust".into(),
            source_file: format!("{caller}.rs"),
            caller: caller.into(),
            destination_file: format!("{callee}.rs"),
            callee: callee.into(),
            call_type: "direct".into(),
        }
    }

    #[test]
    fn csr_primitives_preserve_ambiguity_cycles_and_depth() {
        let graph = GraphSnapshot::build(
            &HashMap::new(),
            &[edge("a", "b"), edge("b", "c"), edge("c", "b")],
        );
        let a = graph.by_name("a")[0];
        let b = graph.by_name("b")[0];
        let c = graph.by_name("c")[0];

        assert_eq!(graph.node_count(), 3);
        assert_eq!(graph.edge_count(), 3);
        assert_eq!(graph.out_neighbors(a), &[b]);
        assert_eq!(graph.in_neighbors(b), &[a, c]);
        assert_eq!(graph.reverse_bfs(&[c], 2), vec![(c, 0), (b, 1), (a, 2)]);
        assert_eq!(graph.strongly_connected_components(), vec![vec![b, c]]);
        assert_eq!(graph.by_file("b.rs"), &[b]);
        assert!(graph.by_file_line("b.rs", 1).is_empty());

        let impact = graph.impact_report("c", 2, None).expect("impact");
        assert_eq!(impact.total_targets, 1);
        assert_eq!(impact.targets["c"].caller_count, 1);
    }

    #[test]
    fn duplicate_edges_are_deduplicated_in_csr() {
        let graph = GraphSnapshot::build(
            &HashMap::new(),
            &[edge("a", "b"), edge("a", "b"), edge("a", "b")],
        );
        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    #[ignore = "measurement gate; run explicitly in release mode"]
    fn records_frozen_corpus_scale_rebuild_time() {
        const NODES: usize = 20_320;
        const EDGES: usize = 26_312;
        let edges = (0..EDGES)
            .map(|index| {
                let caller = index % NODES;
                let callee = (index.wrapping_mul(17) + index / NODES + 1) % NODES;
                ProjectCallEdgeFact {
                    language: "rust".into(),
                    source_file: format!("src/f{caller}.rs"),
                    caller: format!("f{caller}"),
                    destination_file: format!("src/f{callee}.rs"),
                    callee: format!("f{callee}"),
                    call_type: "direct".into(),
                }
            })
            .collect::<Vec<_>>();
        let started = Instant::now();
        let graph = GraphSnapshot::build(&HashMap::new(), &edges);
        let elapsed = started.elapsed();
        let target = graph.by_name("f10000")[0];
        let neighbor_started = Instant::now();
        for _ in 0..10_000 {
            std::hint::black_box(graph.in_neighbors(target));
            std::hint::black_box(graph.out_neighbors(target));
        }
        let neighbor_elapsed = neighbor_started.elapsed();
        let bfs_started = Instant::now();
        let closure = graph.reverse_bfs(&[target], 3);
        let bfs_elapsed = bfs_started.elapsed();
        eprintln!(
            "csr_rebuild nodes={} edges={} elapsed_us={} resident_bytes={} neighbor_10k_us={} reverse_bfs_depth3_us={} reverse_bfs_nodes={}",
            graph.node_count(),
            graph.edge_count(),
            elapsed.as_micros(),
            graph.resident_bytes(),
            neighbor_elapsed.as_micros(),
            bfs_elapsed.as_micros(),
            closure.len(),
        );
        assert_eq!(graph.node_count(), NODES);
        assert_eq!(graph.edge_count(), EDGES);
    }
}

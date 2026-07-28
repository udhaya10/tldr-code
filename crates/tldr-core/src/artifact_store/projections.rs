//! Typed, generation-pinned projections over normalized artifacts.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::analysis::dead::dead_code_analysis_refcount;
use crate::types::{CallEdge, DeadCodeReport, ModuleInfo, ProjectCallGraph};
use crate::{
    collect_all_functions, dead_code_analysis, CodeStructure, Language, TldrError, TldrResult,
};

use super::redb::decode;
use super::GraphSnapshot;
use super::{
    ArtifactKind, ArtifactStore, ArtifactSubject, DefinitionFact, FileFacts, ImportFact,
    ProjectCallEdgeFact, SemanticChunkFact,
};

/// Immutable structural view assembled from one published generation.
#[derive(Clone, Debug)]
pub struct GenerationSnapshot {
    generation: u64,
    files: HashMap<String, Arc<FileFacts>>,
    project_calls: Vec<ProjectCallEdgeFact>,
    call_nodes: HashMap<String, Vec<String>>,
    graph: GraphSnapshot,
}

impl GenerationSnapshot {
    /// Pin and decode the active generation.
    pub fn active(store: &dyn ArtifactStore) -> TldrResult<Option<Self>> {
        store
            .active_generation()?
            .map(|generation| Self::load(store, generation))
            .transpose()
    }

    /// Pin and decode a specific published generation.
    pub fn load(store: &dyn ArtifactStore, generation: u64) -> TldrResult<Self> {
        let manifest = store.generation(generation)?.ok_or_else(|| {
            TldrError::DaemonError(format!("published generation {generation} is missing"))
        })?;
        let files = manifest
            .artifacts
            .iter()
            .filter(|key| key.kind == ArtifactKind::FileFacts)
            .map(|key| {
                let artifact = store.artifact(key)?.ok_or_else(|| {
                    TldrError::DaemonError(format!(
                        "published generation {generation} references a missing file artifact"
                    ))
                })?;
                let facts: FileFacts = decode(&artifact.payload)?;
                Ok((facts.path.clone(), Arc::new(facts)))
            })
            .collect::<TldrResult<HashMap<_, _>>>()?;
        let project_calls: Vec<ProjectCallEdgeFact> = manifest
            .artifacts
            .iter()
            .find(|key| key.kind == ArtifactKind::CallGraph)
            .map(|key| {
                let artifact = store.artifact(key)?.ok_or_else(|| {
                    TldrError::DaemonError(format!(
                        "published generation {generation} references a missing call graph"
                    ))
                })?;
                decode(&artifact.payload)
            })
            .transpose()?
            .unwrap_or_default();
        Ok(Self::from_resident(generation, files, project_calls))
    }

    /// Refresh a published file-scoped generation from a resident predecessor.
    ///
    /// Unchanged file facts retain their existing `Arc`; only explicitly
    /// changed paths are removed or decoded from the new manifest.
    pub fn refresh_files(
        store: &dyn ArtifactStore,
        generation: u64,
        previous: &Self,
        changed: &[String],
    ) -> TldrResult<Self> {
        let manifest = store.generation(generation)?.ok_or_else(|| {
            TldrError::DaemonError(format!("published generation {generation} is missing"))
        })?;
        let file_keys = manifest
            .artifacts
            .iter()
            .filter_map(|key| match (&key.kind, &key.subject) {
                (ArtifactKind::FileFacts, ArtifactSubject::File(path)) => {
                    Some((path.as_str(), key))
                }
                _ => None,
            })
            .collect::<HashMap<_, _>>();

        let mut files = previous.files.clone();
        for path in changed {
            files.remove(path);
            if let Some(key) = file_keys.get(path.as_str()) {
                let artifact = store.artifact(key)?.ok_or_else(|| {
                    TldrError::DaemonError(format!(
                        "published generation {generation} references a missing file artifact"
                    ))
                })?;
                let facts: FileFacts = decode(&artifact.payload)?;
                files.insert(facts.path.clone(), Arc::new(facts));
            }
        }

        let resident_paths = files.keys().map(String::as_str).collect::<HashSet<_>>();
        let manifest_paths = file_keys.keys().copied().collect::<HashSet<_>>();
        if resident_paths != manifest_paths {
            return Err(TldrError::DaemonError(format!(
                "file-scoped generation {generation} does not match its resident predecessor"
            )));
        }

        let project_calls: Vec<ProjectCallEdgeFact> = manifest
            .artifacts
            .iter()
            .find(|key| key.kind == ArtifactKind::CallGraph)
            .map(|key| {
                let artifact = store.artifact(key)?.ok_or_else(|| {
                    TldrError::DaemonError(format!(
                        "published generation {generation} references a missing call graph"
                    ))
                })?;
                decode(&artifact.payload)
            })
            .transpose()?
            .unwrap_or_default();
        Ok(Self::from_resident(generation, files, project_calls))
    }

    fn from_resident(
        generation: u64,
        files: HashMap<String, Arc<FileFacts>>,
        project_calls: Vec<ProjectCallEdgeFact>,
    ) -> Self {
        let mut call_nodes = HashMap::<String, Vec<String>>::new();
        for facts in files.values() {
            if let Ok(ir) = ciborium::de::from_reader::<crate::callgraph::FileIR, _>(
                facts.callgraph_ir.as_slice(),
            ) {
                let nodes = call_nodes.entry(facts.language.clone()).or_default();
                for function in ir.funcs {
                    let qualified = function.class_name.map_or(function.name.clone(), |class| {
                        format!("{class}.{}", function.name)
                    });
                    nodes.push(format!("{}:{qualified}", facts.path));
                }
            }
        }
        for nodes in call_nodes.values_mut() {
            nodes.sort();
            nodes.dedup();
        }
        let graph = GraphSnapshot::build(&files, &project_calls);
        Self {
            generation,
            files,
            project_calls,
            call_nodes,
            graph,
        }
    }

    /// Generation pinned by this snapshot.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Number of normalized source-file revisions in the snapshot.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Resident CSR and symbol indexes derived from this exact generation.
    pub fn graph(&self) -> &GraphSnapshot {
        &self.graph
    }

    /// Read normalized facts for a root-relative path.
    pub fn file(&self, path: impl AsRef<Path>) -> Option<&FileFacts> {
        let normalized = path.as_ref().to_string_lossy().replace('\\', "/");
        self.files.get(&normalized).map(Arc::as_ref)
    }

    /// Iterate over all normalized file facts.
    pub fn files(&self) -> impl Iterator<Item = &FileFacts> {
        self.files.values().map(Arc::as_ref)
    }

    /// Project all definitions without re-reading source.
    pub fn definitions(&self) -> impl Iterator<Item = (&str, &DefinitionFact)> {
        self.files.values().flat_map(|facts| {
            facts
                .definitions
                .iter()
                .map(move |definition| (facts.path.as_str(), definition))
        })
    }

    /// Project all imports without re-reading source.
    pub fn imports(&self) -> impl Iterator<Item = (&str, &ImportFact)> {
        self.files.values().flat_map(|facts| {
            facts
                .imports
                .iter()
                .map(move |import| (facts.path.as_str(), import))
        })
    }

    /// Project semantic chunks from the same parse as structural facts.
    pub fn semantic_chunks(&self) -> impl Iterator<Item = (&str, &SemanticChunkFact)> {
        self.files.values().flat_map(|facts| {
            facts
                .semantic_chunks
                .iter()
                .map(move |chunk| (facts.path.as_str(), chunk))
        })
    }

    /// Restore the semantic planner's complete source inputs without walking or
    /// reading the project again.
    #[cfg(feature = "semantic")]
    pub fn semantic_source_chunks(&self, project: &Path) -> Vec<crate::semantic::CodeChunk> {
        let mut chunks = self
            .files
            .values()
            .flat_map(|facts| semantic_source_chunks_for_facts(project, facts))
            .collect::<Vec<_>>();
        chunks.sort_by(|left, right| left.file_path.cmp(&right.file_path));
        chunks
    }

    /// Restore semantic planner inputs only for explicitly requested files.
    ///
    /// Unlike [`Self::semantic_source_chunks`], this performs one resident
    /// `HashMap` lookup per requested path and never walks, clones, or sorts
    /// unrelated corpus entries. Paths may be root-relative or absolute under
    /// `project`; input order and per-file chunk order are preserved.
    #[cfg(feature = "semantic")]
    pub fn semantic_source_chunks_for<'a>(
        &self,
        project: &Path,
        paths: impl IntoIterator<Item = &'a Path>,
    ) -> Vec<crate::semantic::CodeChunk> {
        paths
            .into_iter()
            .flat_map(|path| {
                let relative = path.strip_prefix(project).unwrap_or(path);
                self.file(relative)
                    .into_iter()
                    .flat_map(|facts| semantic_source_chunks_for_facts(project, facts))
            })
            .collect()
    }

    /// Build the structure command's exact schema from stored file projections.
    pub fn code_structure(
        &self,
        project: &Path,
        root: &Path,
        language: Language,
        max_results: usize,
    ) -> CodeStructure {
        let relative_root = root
            .strip_prefix(project)
            .unwrap_or(root)
            .to_string_lossy()
            .replace('\\', "/");
        let single_file = root.is_file();
        let extensions = language.scan_extensions();
        let mut files = self
            .files
            .values()
            .filter(|facts| {
                let under_root = if relative_root.is_empty() {
                    true
                } else if single_file {
                    facts.path == relative_root
                } else {
                    facts.path == relative_root
                        || facts.path.starts_with(&format!("{relative_root}/"))
                };
                under_root
                    && Path::new(&facts.path)
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| {
                            extensions
                                .iter()
                                .any(|candidate| candidate.trim_start_matches('.') == extension)
                        })
            })
            .map(|facts| {
                let mut structure = facts.structure.to_file_structure();
                if !single_file && !relative_root.is_empty() {
                    structure.path = structure
                        .path
                        .strip_prefix(&relative_root)
                        .unwrap_or(&structure.path)
                        .to_path_buf();
                }
                structure
            })
            .collect::<Vec<_>>();
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let files_skipped = if max_results > 0 {
            u32::try_from(files.len().saturating_sub(max_results)).unwrap_or(u32::MAX)
        } else {
            0
        };
        if max_results > 0 {
            files.truncate(max_results);
        }
        let empty = files.is_empty();
        CodeStructure {
            root: root.to_path_buf(),
            language: (!empty).then_some(language),
            files,
            files_skipped,
            warnings: if empty {
                vec!["No source files found in directory".into()]
            } else {
                Vec::new()
            },
        }
    }

    /// Answer dead-code analysis from generation-resident module and identifier
    /// facts. No source walk, parse, or materialized whole-project answer cache
    /// is involved.
    pub fn dead_report(
        &self,
        project: &Path,
        root: &Path,
        language: Language,
        entry_points: Option<&[String]>,
        use_call_graph: bool,
    ) -> TldrResult<DeadCodeReport> {
        let relative_root = root
            .strip_prefix(project)
            .unwrap_or(root)
            .to_string_lossy()
            .replace('\\', "/");
        let single_file = root.is_file();
        let mut selected = self
            .files
            .values()
            .filter_map(|facts| {
                if facts.language != language.as_str()
                    || facts.path.to_ascii_lowercase().ends_with(".d.ts")
                {
                    return None;
                }
                scope_path(&facts.path, &relative_root, single_file)
                    .map(|path| (path, facts.module.to_module_info(), facts))
            })
            .collect::<Vec<_>>();
        selected.sort_by(|left, right| left.0.cmp(&right.0));
        let module_infos = selected
            .iter()
            .map(|(path, module, _)| (path.clone(), module.clone()))
            .collect::<Vec<(PathBuf, ModuleInfo)>>();
        let all_functions = collect_all_functions(&module_infos);

        if use_call_graph {
            let mut graph = ProjectCallGraph::new();
            for edge in self.project_calls.iter().filter(|edge| {
                edge.language == language.as_str()
                    && scope_path(&edge.source_file, &relative_root, single_file).is_some()
                    && scope_path(&edge.destination_file, &relative_root, single_file).is_some()
            }) {
                graph.add_edge(CallEdge {
                    src_file: scope_path(&edge.source_file, &relative_root, single_file)
                        .expect("edge scope checked"),
                    src_func: edge.caller.clone(),
                    dst_file: scope_path(&edge.destination_file, &relative_root, single_file)
                        .expect("edge scope checked"),
                    dst_func: edge.callee.clone(),
                });
            }
            return dead_code_analysis(&graph, &all_functions, entry_points);
        }

        let mut counts = HashMap::<String, usize>::new();
        let mut dotted = HashMap::<String, usize>::new();
        for (_, _, facts) in &selected {
            for (name, count) in &facts.identifier_counts {
                *counts.entry(name.clone()).or_default() += *count as usize;
            }
            for path in &facts.python_dotted_strings {
                *dotted.entry(path.clone()).or_default() += 1;
            }
        }
        let known_dotted = indexed_dotted_symbols(&module_infos);
        for (path, count) in dotted {
            if known_dotted.contains(&path) {
                if let Some(symbol) = path.rsplit('.').next() {
                    *counts.entry(symbol.to_string()).or_default() += count;
                }
            }
        }
        dead_code_analysis_refcount(&all_functions, &counts, entry_points)
    }

    /// Compose the generation's project-level call graph artifact.
    pub fn call_graph(&self, language: Option<Language>) -> ProjectCallGraph {
        let mut graph = ProjectCallGraph::new();
        for edge in self
            .project_calls
            .iter()
            .filter(|edge| language.is_none_or(|language| edge.language == language.as_str()))
        {
            graph.add_edge(CallEdge {
                src_file: PathBuf::from(&edge.source_file),
                src_func: edge.caller.clone(),
                dst_file: PathBuf::from(&edge.destination_file),
                dst_func: edge.callee.clone(),
            });
        }
        graph
    }

    /// Compose all languages for language-agnostic relationship queries.
    pub fn intra_file_call_graph(&self) -> ProjectCallGraph {
        let mut graph = ProjectCallGraph::new();
        for edge in self
            .project_calls
            .iter()
            .filter(|edge| edge.source_file == edge.destination_file)
        {
            graph.add_edge(CallEdge {
                src_file: PathBuf::from(&edge.source_file),
                src_func: edge.caller.clone(),
                dst_file: PathBuf::from(&edge.destination_file),
                dst_func: edge.callee.clone(),
            });
        }
        graph
    }

    /// Stored V2 edge facts, pinned to this generation.
    pub fn call_edges(
        &self,
        language: Option<Language>,
    ) -> impl Iterator<Item = &ProjectCallEdgeFact> {
        self.project_calls
            .iter()
            .filter(move |edge| language.is_none_or(|language| edge.language == language.as_str()))
    }

    /// Canonical FileIR function inventory, grouped by resolver language.
    pub fn call_nodes(&self, language: Option<Language>) -> impl Iterator<Item = &str> {
        self.call_nodes
            .iter()
            .filter(move |(label, _)| {
                language.is_none_or(|language| label.as_str() == language.as_str())
            })
            .flat_map(|(_, nodes)| nodes.iter().map(String::as_str))
    }
}

#[cfg(feature = "semantic")]
fn semantic_source_chunks_for_facts<'a>(
    project: &'a Path,
    facts: &'a FileFacts,
) -> impl Iterator<Item = crate::semantic::CodeChunk> + 'a {
    let language = Language::from_extension(&facts.language)
        .or_else(|| Language::from_path(Path::new(&facts.path)));
    facts.semantic_chunks.iter().filter_map(move |chunk| {
        language.map(|language| crate::semantic::CodeChunk {
            file_path: project.join(&facts.path),
            function_name: chunk.function_name.clone(),
            class_name: chunk.class_name.clone(),
            line_start: chunk.line_start,
            line_end: chunk.line_end,
            content: chunk.content.clone(),
            content_hash: chunk.content_hash.clone(),
            language,
            structure: Default::default(),
        })
    })
}

fn scope_path(path: &str, relative_root: &str, single_file: bool) -> Option<PathBuf> {
    if relative_root.is_empty() {
        return Some(PathBuf::from(path));
    }
    if single_file {
        return (path == relative_root).then(|| {
            Path::new(path)
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(path))
        });
    }
    Path::new(path)
        .strip_prefix(relative_root)
        .ok()
        .map(Path::to_path_buf)
}

fn indexed_dotted_symbols(module_infos: &[(PathBuf, ModuleInfo)]) -> HashSet<String> {
    let mut symbols = HashSet::new();
    for (path, info) in module_infos {
        let mut components = path
            .components()
            .filter_map(|component| component.as_os_str().to_str())
            .map(str::to_string)
            .collect::<Vec<_>>();
        let Some(file) = components.pop() else {
            continue;
        };
        let Some(stem) = Path::new(&file).file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if stem != "__init__" {
            components.push(stem.to_string());
        }
        if components.is_empty() {
            continue;
        }
        let module = components.join(".");
        for function in &info.functions {
            symbols.insert(format!("{module}.{}", function.name));
        }
        for class in &info.classes {
            symbols.insert(format!("{module}.{}", class.name));
            for method in &class.methods {
                symbols.insert(format!("{module}.{}.{}", class.name, method.name));
            }
        }
    }
    symbols
}

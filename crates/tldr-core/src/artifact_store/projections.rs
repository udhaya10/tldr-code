//! Typed, generation-pinned projections over normalized artifacts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::types::{CallEdge, ProjectCallGraph};
use crate::{CodeStructure, Language, TldrError, TldrResult};

use super::redb::decode;
use super::{
    ArtifactKind, ArtifactStore, DefinitionFact, FileFacts, ImportFact, ProjectCallEdgeFact,
    SemanticChunkFact,
};

/// Immutable structural view assembled from one published generation.
#[derive(Clone, Debug)]
pub struct GenerationSnapshot {
    generation: u64,
    files: HashMap<String, FileFacts>,
    project_calls: Vec<ProjectCallEdgeFact>,
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
                Ok((facts.path.clone(), facts))
            })
            .collect::<TldrResult<HashMap<_, _>>>()?;
        let project_calls = manifest
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
        Ok(Self {
            generation,
            files,
            project_calls,
        })
    }

    /// Generation pinned by this snapshot.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Number of normalized source-file revisions in the snapshot.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Read normalized facts for a root-relative path.
    pub fn file(&self, path: impl AsRef<Path>) -> Option<&FileFacts> {
        let normalized = path.as_ref().to_string_lossy().replace('\\', "/");
        self.files.get(&normalized)
    }

    /// Iterate over all normalized file facts.
    pub fn files(&self) -> impl Iterator<Item = &FileFacts> {
        self.files.values()
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
            .flat_map(|facts| {
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
            })
            .collect::<Vec<_>>();
        chunks.sort_by(|left, right| left.file_path.cmp(&right.file_path));
        chunks
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
}

//! Typed, generation-pinned projections over normalized artifacts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::types::{CallEdge, ProjectCallGraph};
use crate::TldrResult;

use super::redb::decode;
use super::{
    ArtifactKind, ArtifactStore, CallFact, DefinitionFact, FileFacts, ImportFact, SemanticChunkFact,
};

/// Immutable structural view assembled from one published generation.
#[derive(Clone, Debug)]
pub struct GenerationSnapshot {
    generation: u64,
    files: HashMap<String, FileFacts>,
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
        let files = store
            .artifacts(generation, ArtifactKind::FileFacts)?
            .into_iter()
            .map(|artifact| {
                let facts: FileFacts = decode(&artifact.payload)?;
                Ok((facts.path.clone(), facts))
            })
            .collect::<TldrResult<HashMap<_, _>>>()?;
        Ok(Self { generation, files })
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

    /// Compose the exact intra-file call edges captured by shared ingestion.
    ///
    /// Cross-file resolution remains a separate project-level producer; this
    /// projection deliberately does not invent destination files.
    pub fn intra_file_call_graph(&self) -> ProjectCallGraph {
        let mut graph = ProjectCallGraph::new();
        for facts in self.files.values() {
            let file = PathBuf::from(&facts.path);
            for CallFact { caller, callee } in &facts.calls {
                graph.add_edge(CallEdge {
                    src_file: file.clone(),
                    src_func: caller.clone(),
                    dst_file: file.clone(),
                    dst_func: callee.clone(),
                });
            }
        }
        graph
    }
}

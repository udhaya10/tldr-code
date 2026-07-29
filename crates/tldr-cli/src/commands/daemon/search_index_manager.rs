//! Generation-pinned resident BM25 indexes.
//!
//! Construction happens only when ArtifactStore publishes a generation.
//! Query methods are read-only and fail closed on generation mismatch.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;
use tldr_core::artifact_store::GenerationSnapshot;
use tldr_core::{Bm25Index, Language};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchIndexStats {
    pub generation: u64,
    pub languages: usize,
    pub documents: usize,
}

struct SearchIndexState {
    generation: u64,
    by_language: HashMap<Language, Arc<Bm25Index>>,
    file_languages: HashMap<PathBuf, Language>,
    documents: usize,
}

#[derive(Default)]
pub struct SearchIndexManager {
    state: RwLock<Option<SearchIndexState>>,
}

impl SearchIndexManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace all lexical indexes from one immutable artifact generation.
    pub fn refresh(&self, snapshot: &GenerationSnapshot) -> SearchIndexStats {
        let file_languages = snapshot
            .files()
            .map(|facts| (PathBuf::from(&facts.path), facts.module.language))
            .collect::<HashMap<_, _>>();
        let languages = file_languages.values().copied().collect::<HashSet<_>>();
        let mut by_language = HashMap::with_capacity(languages.len());
        let mut documents = 0;
        for language in languages {
            let index = Bm25Index::from_documents(snapshot.lexical_documents(language));
            documents += index.document_count();
            by_language.insert(language, Arc::new(index));
        }
        let stats = SearchIndexStats {
            generation: snapshot.generation(),
            languages: by_language.len(),
            documents,
        };
        *self.state.write() = Some(SearchIndexState {
            generation: stats.generation,
            by_language,
            file_languages,
            documents,
        });
        stats
    }

    /// Refresh only the language indexes touched by a published delta.
    ///
    /// Deleted and renamed paths use the previous generation's path map, while
    /// newly-created paths use the new snapshot. Unaffected language indexes
    /// retain their `Arc`, so a watcher burst never re-tokenizes the complete
    /// multi-language corpus.
    pub fn refresh_paths(
        &self,
        snapshot: &GenerationSnapshot,
        changed_paths: &[PathBuf],
    ) -> SearchIndexStats {
        let new_file_languages = snapshot
            .files()
            .map(|facts| (PathBuf::from(&facts.path), facts.module.language))
            .collect::<HashMap<_, _>>();

        let (mut by_language, old_file_languages) = self
            .state
            .read()
            .as_ref()
            .map(|state| (state.by_language.clone(), state.file_languages.clone()))
            .unwrap_or_default();
        let mut affected = HashSet::new();
        for path in changed_paths {
            if let Some(language) = old_file_languages.get(path) {
                affected.insert(*language);
            }
            if let Some(language) = new_file_languages.get(path) {
                affected.insert(*language);
            }
        }

        for language in affected {
            let index = Bm25Index::from_documents(snapshot.lexical_documents(language));
            if index.is_empty() {
                by_language.remove(&language);
            } else {
                by_language.insert(language, Arc::new(index));
            }
        }
        let documents = by_language
            .values()
            .map(|index| index.document_count())
            .sum();
        let stats = SearchIndexStats {
            generation: snapshot.generation(),
            languages: by_language.len(),
            documents,
        };
        *self.state.write() = Some(SearchIndexState {
            generation: stats.generation,
            by_language,
            file_languages: new_file_languages,
            documents,
        });
        stats
    }

    /// Pin an index for a request. This method never constructs an index.
    pub fn index(&self, generation: u64, language: Language) -> Result<Arc<Bm25Index>, String> {
        let state = self.state.read();
        let state = state
            .as_ref()
            .ok_or_else(|| "resident BM25 index is not ready — run tldr warm".to_string())?;
        if state.generation != generation {
            return Err(format!(
                "resident BM25 generation {} does not match artifact generation {generation}",
                state.generation
            ));
        }
        state.by_language.get(&language).cloned().ok_or_else(|| {
            format!(
                "resident BM25 has no documents for language {} in generation {generation}",
                language
            )
        })
    }

    pub fn stats(&self) -> Option<SearchIndexStats> {
        self.state.read().as_ref().map(|state| SearchIndexStats {
            generation: state.generation,
            languages: state.by_language.len(),
            documents: state.documents,
        })
    }

    pub fn invalidate(&self) {
        *self.state.write() = None;
    }

    #[cfg(test)]
    fn index_ptr(&self, language: Language) -> Option<*const Bm25Index> {
        self.state
            .read()
            .as_ref()?
            .by_language
            .get(&language)
            .map(Arc::as_ptr)
    }
}

#[cfg(test)]
mod tests {
    use super::SearchIndexManager;
    use crate::commands::daemon::artifact_manager::ArtifactManager;
    use std::path::PathBuf;
    use tldr_core::Language;

    #[test]
    fn cold_manager_fails_closed() {
        let manager = SearchIndexManager::new();
        let error = manager
            .index(1, tldr_core::Language::Rust)
            .expect_err("cold manager must fail");
        assert!(error.contains("not ready"));
    }

    #[test]
    fn delta_refresh_reuses_unaffected_language_index() {
        let project = tempfile::tempdir().expect("temp project");
        std::fs::write(project.path().join("risk.rs"), "fn risk_limit() {}\n").expect("write rust");
        std::fs::write(project.path().join("report.py"), "def report(): pass\n")
            .expect("write python");
        let artifacts = ArtifactManager::open(project.path()).expect("open artifacts");
        artifacts.warm().expect("warm artifacts");
        let baseline = artifacts.snapshot().expect("baseline snapshot");
        let manager = SearchIndexManager::new();
        manager.refresh(&baseline);
        let python_before = manager.index_ptr(Language::Python).expect("python index");

        std::fs::write(
            project.path().join("risk.rs"),
            "fn position_exposure_limit() {}\n",
        )
        .expect("edit rust");
        artifacts
            .apply_delta(&project.path().join("risk.rs"))
            .expect("publish delta");
        let updated = artifacts.snapshot().expect("updated snapshot");
        manager.refresh_paths(&updated, &[PathBuf::from("risk.rs")]);

        let python_after = manager.index_ptr(Language::Python).expect("python index");
        assert_eq!(python_before, python_after);
        assert!(manager
            .index(updated.generation(), Language::Rust)
            .expect("rust index")
            .search("position exposure limit", 10)
            .iter()
            .any(|result| result.file_path == std::path::Path::new("risk.rs")));
    }
}

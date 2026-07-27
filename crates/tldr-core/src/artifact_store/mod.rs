//! Shared, generation-aware persistence for structural and semantic artifacts.

mod file_facts;
mod function_artifacts;
mod ingestion;
mod projections;
mod redb;
pub mod schema;
mod types;

pub use file_facts::{
    CallFact, DefinitionFact, FileFacts, FileFactsParser, ImportFact, ProjectCallEdgeFact,
    SemanticChunkFact, StoredFileStructure, StoredModuleInfo,
};
pub use function_artifacts::FunctionArtifactCoordinator;
pub use ingestion::{IngestionEngine, IngestionReport};
pub use projections::GenerationSnapshot;
pub use redb::{ArtifactStore, RedbArtifactStore};
pub use types::{
    ArtifactBatch, ArtifactEnvelope, ArtifactKey, ArtifactKind, ArtifactSubject,
    GenerationManifest, IngestionJob, IngestionScope, IngestionStage, ProducerId, ProjectId,
    RevisionId,
};

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn key(
        project: ProjectId,
        revision: RevisionId,
        path: &str,
        kind: ArtifactKind,
    ) -> ArtifactKey {
        ArtifactKey {
            project,
            revision,
            subject: ArtifactSubject::File(path.into()),
            kind,
            producer: ProducerId::new("test", 1),
        }
    }

    #[test]
    fn artifact_batch_checkpoint_and_publication_are_durable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(schema::STORE_FILE);
        let project = ProjectId([1; 32]);
        let revision = RevisionId([2; 32]);
        let artifact_key = key(project, revision, "src/lib.rs", ArtifactKind::FileFacts);
        let artifact = ArtifactEnvelope::new(artifact_key.clone(), 1, Vec::new(), vec![4, 5, 6]);
        let job = IngestionJob {
            id: "ingestion-1".into(),
            target_generation: 1,
            scope: IngestionScope::Project,
            stage: IngestionStage::Commit,
            next_batch: 1,
            total_batches: 1,
            source_revision: revision,
        };
        {
            let store = RedbArtifactStore::open(&path).unwrap();
            store
                .commit_batch(
                    &ArtifactBatch {
                        generation: 1,
                        artifacts: vec![artifact.clone()],
                    },
                    &job,
                )
                .unwrap();
            store
                .publish(&GenerationManifest {
                    generation: 1,
                    project,
                    source_revision: revision,
                    artifacts: vec![artifact_key.clone()],
                    vector_generation: None,
                })
                .unwrap();
        }
        let reopened = RedbArtifactStore::open(&path).unwrap();
        assert_eq!(reopened.active_generation().unwrap(), Some(1));
        assert_eq!(reopened.job("ingestion-1").unwrap(), Some(job));
        assert_eq!(reopened.artifact(&artifact_key).unwrap(), Some(artifact));
    }

    #[test]
    fn one_parse_supplies_structural_calls_and_semantic_chunks() {
        let project = tempfile::tempdir().unwrap();
        let path = project.path().join("sample.rs");
        std::fs::write(&path, "fn helper() {}\nfn main() { helper(); }\n").unwrap();
        let parser = FileFactsParser::default();
        let facts = parser.parse(project.path(), &path).unwrap();
        assert_eq!(parser.invocations(), 1);
        assert!(facts.definitions.iter().any(|fact| fact.name == "main"));
        assert!(facts.calls.iter().any(|fact| fact.callee == "helper"));
        assert!(!facts.semantic_chunks.is_empty());
    }

    #[test]
    fn full_and_delta_ingestion_share_the_same_engine() {
        let project = tempfile::tempdir().unwrap();
        let database = tempfile::tempdir().unwrap();
        let first = project.path().join("first.rs");
        let second = project.path().join("second.rs");
        std::fs::write(&first, "fn first() {}\n").unwrap();
        std::fs::write(&second, "fn second() {}\n").unwrap();
        let store: Arc<dyn ArtifactStore> =
            Arc::new(RedbArtifactStore::open(&database.path().join(schema::STORE_FILE)).unwrap());
        let engine = IngestionEngine::new(project.path(), store.clone()).unwrap();
        let full = engine.ingest(IngestionScope::Project).unwrap();
        assert_eq!(full.generation, 1);
        assert_eq!(full.parsed_files, 2);
        let unchanged = engine.ingest(IngestionScope::Project).unwrap();
        assert_eq!(unchanged.generation, 2);
        assert_eq!(unchanged.parsed_files, 0);

        std::fs::write(&first, "fn first_changed() {}\n").unwrap();
        let delta = engine
            .ingest(IngestionScope::Files(vec!["first.rs".into()]))
            .unwrap();
        assert_eq!(delta.generation, 3);
        assert_eq!(delta.parsed_files, 1);
        let manifest = store.generation(3).unwrap().unwrap();
        assert!(manifest
            .artifacts
            .iter()
            .any(|key| matches!(&key.subject, ArtifactSubject::File(path) if path == "second.rs")));
        let snapshot = GenerationSnapshot::active(store.as_ref()).unwrap().unwrap();
        assert_eq!(snapshot.generation(), 3);
        assert_eq!(snapshot.file_count(), 2);
        assert!(snapshot
            .definitions()
            .any(|(_, definition)| definition.name == "first_changed"));
        store.set_vector_generation(3, 17).unwrap();
        assert_eq!(
            store.generation(3).unwrap().unwrap().vector_generation,
            Some(17)
        );
    }

    #[test]
    fn project_graph_composes_cross_file_edges_from_stored_file_ir() {
        let project = tempfile::tempdir().unwrap();
        let database = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("main.py"),
            "from helper import work\n\ndef run():\n    work()\n",
        )
        .unwrap();
        std::fs::write(
            project.path().join("helper.py"),
            "def work():\n    return 1\n",
        )
        .unwrap();
        let store: Arc<dyn ArtifactStore> =
            Arc::new(RedbArtifactStore::open(&database.path().join(schema::STORE_FILE)).unwrap());
        IngestionEngine::new(project.path(), store.clone())
            .unwrap()
            .ingest(IngestionScope::Project)
            .unwrap();
        let snapshot = GenerationSnapshot::active(store.as_ref()).unwrap().unwrap();
        assert!(snapshot
            .call_edges(Some(crate::Language::Python))
            .any(|edge| {
                edge.source_file == "main.py"
                    && edge.caller == "run"
                    && edge.destination_file == "helper.py"
                    && edge.callee == "work"
            }));
    }
}

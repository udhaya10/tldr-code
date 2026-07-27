//! Shared, generation-aware persistence for structural and semantic artifacts.

mod file_facts;
mod function_artifacts;
mod graph_snapshot;
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
pub use graph_snapshot::{FuncId, FunctionNode, GraphSnapshot};
pub use ingestion::{IngestionEngine, IngestionReport};
pub use projections::GenerationSnapshot;
pub use redb::{ArtifactStore, RedbArtifactStore};
pub use types::{
    ArtifactBatch, ArtifactEnvelope, ArtifactKey, ArtifactKind, ArtifactSubject,
    GenerationManifest, IngestionJob, IngestionScope, IngestionStage, ProducerId, ProjectId,
    RevisionId,
};

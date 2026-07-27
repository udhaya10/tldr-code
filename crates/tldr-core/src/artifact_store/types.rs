//! Stable identities and durable envelopes for project analysis artifacts.

use std::path::Path;

use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};

/// Content-derived identity of a canonical project root.
#[derive(
    Archive,
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Hash,
    PartialEq,
    RkyvDeserialize,
    RkyvSerialize,
    Serialize,
)]
pub struct ProjectId(pub [u8; 32]);

impl ProjectId {
    /// Derive a stable project identity from its canonical root path.
    pub fn for_root(root: &Path) -> std::io::Result<Self> {
        let canonical = dunce::canonicalize(root)?;
        let canonical = canonical.to_str().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "canonical project root is not valid UTF-8",
            )
        })?;
        Ok(Self(*blake3::hash(canonical.as_bytes()).as_bytes()))
    }
}

/// Content revision of a source file or project manifest.
#[derive(
    Archive,
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Hash,
    PartialEq,
    RkyvDeserialize,
    RkyvSerialize,
    Serialize,
)]
pub struct RevisionId(pub [u8; 32]);

impl RevisionId {
    /// Hash exact source bytes into a revision.
    pub fn for_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }
}

/// Scope to which an artifact belongs.
#[derive(
    Archive,
    Clone,
    Debug,
    Deserialize,
    Eq,
    Hash,
    PartialEq,
    RkyvDeserialize,
    RkyvSerialize,
    Serialize,
)]
pub enum ArtifactSubject {
    /// Artifact represents a whole project.
    Project,
    /// Artifact represents one root-relative file.
    File(String),
    /// Artifact represents one stable symbol anchor.
    Symbol(String),
}

/// Durable artifact category.
#[derive(
    Archive,
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Hash,
    PartialEq,
    RkyvDeserialize,
    RkyvSerialize,
    Serialize,
)]
pub enum ArtifactKind {
    /// Normalized result of parsing one file revision.
    FileFacts,
    /// Definitions and symbol anchors.
    Symbols,
    /// Scoped references.
    References,
    /// Per-file call edges.
    CallEdges,
    /// Composed project call graph.
    CallGraph,
    /// Control-flow graph.
    Cfg,
    /// Data-flow graph.
    Dfg,
    /// Program-dependence graph.
    Pdg,
    /// Text units submitted for semantic embedding.
    SemanticChunks,
    /// Semantic vector recipe and lineage.
    Embeddings,
}

/// Versioned producer of an artifact.
#[derive(
    Archive,
    Clone,
    Debug,
    Deserialize,
    Eq,
    Hash,
    PartialEq,
    RkyvDeserialize,
    RkyvSerialize,
    Serialize,
)]
pub struct ProducerId {
    /// Stable producer name.
    pub name: String,
    /// Incremented whenever producer semantics change.
    pub version: u32,
}

impl ProducerId {
    /// Construct a versioned producer identity.
    pub fn new(name: impl Into<String>, version: u32) -> Self {
        Self {
            name: name.into(),
            version,
        }
    }
}

/// Complete immutable identity of an artifact.
#[derive(
    Archive,
    Clone,
    Debug,
    Deserialize,
    Eq,
    Hash,
    PartialEq,
    RkyvDeserialize,
    RkyvSerialize,
    Serialize,
)]
pub struct ArtifactKey {
    /// Owning project.
    pub project: ProjectId,
    /// Exact source revision.
    pub revision: RevisionId,
    /// Project, file, or symbol scope.
    pub subject: ArtifactSubject,
    /// Artifact category.
    pub kind: ArtifactKind,
    /// Versioned producer.
    pub producer: ProducerId,
}

/// Immutable artifact record stored in redb.
#[derive(
    Archive, Clone, Debug, Deserialize, PartialEq, RkyvDeserialize, RkyvSerialize, Serialize,
)]
pub struct ArtifactEnvelope {
    /// Stable artifact identity.
    pub key: ArtifactKey,
    /// Project generation that first materialized this record.
    pub generation: u64,
    /// Exact input artifacts from which this record was derived.
    pub dependencies: Vec<ArtifactKey>,
    /// Integrity hash of `payload`.
    pub payload_checksum: [u8; 32],
    /// Versioned, producer-owned binary payload.
    pub payload: Vec<u8>,
}

impl ArtifactEnvelope {
    /// Wrap a binary payload and calculate its integrity checksum.
    pub fn new(
        key: ArtifactKey,
        generation: u64,
        dependencies: Vec<ArtifactKey>,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            key,
            generation,
            dependencies,
            payload_checksum: *blake3::hash(&payload).as_bytes(),
            payload,
        }
    }

    /// Validate payload integrity.
    pub fn is_valid(&self) -> bool {
        self.generation > 0 && self.payload_checksum == *blake3::hash(&self.payload).as_bytes()
    }
}

/// Bounded set of artifacts committed by the single writer.
#[derive(Clone, Debug, Default)]
pub struct ArtifactBatch {
    /// Target generation.
    pub generation: u64,
    /// Immutable records in this batch.
    pub artifacts: Vec<ArtifactEnvelope>,
}

/// Scope shared by full builds and file deltas.
#[derive(
    Archive, Clone, Debug, Deserialize, Eq, PartialEq, RkyvDeserialize, RkyvSerialize, Serialize,
)]
pub enum IngestionScope {
    /// Discover and process the canonical project corpus.
    Project,
    /// Process only these root-relative files.
    Files(Vec<String>),
}

/// Durable ingestion lifecycle.
#[derive(
    Archive,
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    PartialEq,
    RkyvDeserialize,
    RkyvSerialize,
    Serialize,
)]
pub enum IngestionStage {
    /// Source discovery is pending.
    Discovery,
    /// Files are being parsed and derived.
    Derivation,
    /// Batches are being committed.
    Commit,
    /// Generation closure is being validated.
    Validation,
    /// Generation is visible to readers.
    Published,
    /// Work failed but its checkpoint remains resumable.
    Failed,
}

/// Durable checkpoint used by both bulk and delta ingestion.
#[derive(
    Archive, Clone, Debug, Deserialize, Eq, PartialEq, RkyvDeserialize, RkyvSerialize, Serialize,
)]
pub struct IngestionJob {
    /// Caller-stable job identity.
    pub id: String,
    /// Target project generation.
    pub target_generation: u64,
    /// Full-project or file subset.
    pub scope: IngestionScope,
    /// Current lifecycle stage.
    pub stage: IngestionStage,
    /// Next batch that has not committed.
    pub next_batch: u64,
    /// Total planned batches.
    pub total_batches: u64,
    /// Digest of the discovered source manifest.
    pub source_revision: RevisionId,
}

/// Atomically published project generation.
#[derive(
    Archive, Clone, Debug, Deserialize, Eq, PartialEq, RkyvDeserialize, RkyvSerialize, Serialize,
)]
pub struct GenerationManifest {
    /// Monotonic generation number.
    pub generation: u64,
    /// Owning project.
    pub project: ProjectId,
    /// Digest of the complete source manifest.
    pub source_revision: RevisionId,
    /// Required artifact keys.
    pub artifacts: Vec<ArtifactKey>,
    /// Derived usearch generation, when semantic indexing is enabled.
    pub vector_generation: Option<u64>,
}

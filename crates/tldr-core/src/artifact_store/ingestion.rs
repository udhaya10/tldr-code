//! One resumable engine for full-project and file-delta ingestion.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;

use crate::walker::ProjectWalker;
use crate::{Language, TldrError, TldrResult};

use super::redb::encode;
use super::{
    ArtifactBatch, ArtifactEnvelope, ArtifactKey, ArtifactKind, ArtifactStore, ArtifactSubject,
    FileFacts, FileFactsParser, GenerationManifest, IngestionJob, IngestionScope, IngestionStage,
    ProducerId, ProjectId, RevisionId,
};

const BATCH_FILES: usize = 32;
const FILE_FACTS_PRODUCER: &str = "file-facts";
const FILE_FACTS_VERSION: u32 = 1;

/// Summary of a completed or resumed ingestion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IngestionReport {
    /// Published generation.
    pub generation: u64,
    /// Number of source files parsed during this invocation.
    pub parsed_files: usize,
    /// Number of committed artifact records.
    pub artifacts: usize,
    /// Whether an existing durable checkpoint was resumed.
    pub resumed: bool,
}

/// Coordinates discovery, parallel derivation, bounded commits, and publication.
pub struct IngestionEngine {
    root: PathBuf,
    project: ProjectId,
    store: Arc<dyn ArtifactStore>,
}

impl IngestionEngine {
    /// Create an engine for one canonical project.
    pub fn new(root: &Path, store: Arc<dyn ArtifactStore>) -> TldrResult<Self> {
        Ok(Self {
            root: dunce::canonicalize(root)?,
            project: ProjectId::for_root(root)?,
            store,
        })
    }

    /// Build or resume either the complete project or a file subset.
    pub fn ingest(&self, scope: IngestionScope) -> TldrResult<IngestionReport> {
        let all_files = discover(&self.root);
        let source_revision = manifest_revision(&self.root, &all_files)?;
        let active = self.store.active_generation()?.unwrap_or(0);
        let generation = active.saturating_add(1);
        let job_id = format!("ingestion-{generation}");
        let previous = self.store.generation(active)?;

        let selected = match &scope {
            IngestionScope::Project => all_files.clone(),
            IngestionScope::Files(files) => {
                let wanted = files.iter().cloned().collect::<HashSet<_>>();
                all_files
                    .iter()
                    .filter(|path| wanted.contains(&relative(&self.root, path)))
                    .cloned()
                    .collect()
            }
        };
        let parser = FileFactsParser::default();
        let facts = selected
            .par_iter()
            .map(|path| parser.parse(&self.root, path))
            .collect::<TldrResult<Vec<_>>>()?;
        let parsed_files = parser.invocations() as usize;
        let mut derived = facts
            .iter()
            .map(|facts| self.derive(generation, facts))
            .collect::<TldrResult<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        derived.sort_by(|left, right| artifact_order(&left.key).cmp(&artifact_order(&right.key)));

        let changed = match &scope {
            IngestionScope::Project => HashSet::new(),
            IngestionScope::Files(files) => files.iter().cloned().collect(),
        };
        let mut manifest_keys = previous
            .map(|manifest| {
                manifest
                    .artifacts
                    .into_iter()
                    .filter(|key| !subject_changed(&key.subject, &changed))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if matches!(scope, IngestionScope::Project) {
            manifest_keys.clear();
        }
        manifest_keys.extend(derived.iter().map(|artifact| artifact.key.clone()));

        let total_batches = derived.len().div_ceil(BATCH_FILES) as u64;
        let existing = self.store.job(&job_id)?;
        let resumed = existing.is_some();
        let next_batch = existing.as_ref().map_or(0, |job| job.next_batch);
        for (index, chunk) in derived.chunks(BATCH_FILES).enumerate() {
            if (index as u64) < next_batch {
                continue;
            }
            let job = IngestionJob {
                id: job_id.clone(),
                target_generation: generation,
                scope: scope.clone(),
                stage: IngestionStage::Commit,
                next_batch: index as u64 + 1,
                total_batches,
                source_revision,
            };
            self.store.commit_batch(
                &ArtifactBatch {
                    generation,
                    artifacts: chunk.to_vec(),
                },
                &job,
            )?;
        }

        let validation_job = IngestionJob {
            id: job_id.clone(),
            target_generation: generation,
            scope: scope.clone(),
            stage: IngestionStage::Validation,
            next_batch: total_batches,
            total_batches,
            source_revision,
        };
        self.store.commit_batch(
            &ArtifactBatch {
                generation,
                artifacts: Vec::new(),
            },
            &validation_job,
        )?;
        self.store.publish(&GenerationManifest {
            generation,
            project: self.project,
            source_revision,
            artifacts: manifest_keys,
            vector_generation: None,
        })?;
        let published_job = IngestionJob {
            stage: IngestionStage::Published,
            ..validation_job
        };
        self.store.commit_batch(
            &ArtifactBatch {
                generation,
                artifacts: Vec::new(),
            },
            &published_job,
        )?;
        Ok(IngestionReport {
            generation,
            parsed_files,
            artifacts: derived.len(),
            resumed,
        })
    }

    fn derive(&self, generation: u64, facts: &FileFacts) -> TldrResult<Vec<ArtifactEnvelope>> {
        let subject = ArtifactSubject::File(facts.path.clone());
        let producer = ProducerId::new(FILE_FACTS_PRODUCER, FILE_FACTS_VERSION);
        let key = |kind| ArtifactKey {
            project: self.project,
            revision: facts.revision,
            subject: subject.clone(),
            kind,
            producer: producer.clone(),
        };
        let facts_key = key(ArtifactKind::FileFacts);
        let facts_artifact =
            ArtifactEnvelope::new(facts_key.clone(), generation, Vec::new(), encode(facts)?);
        let symbols = ArtifactEnvelope::new(
            key(ArtifactKind::Symbols),
            generation,
            vec![facts_key.clone()],
            encode(&facts.definitions)?,
        );
        let calls = ArtifactEnvelope::new(
            key(ArtifactKind::CallEdges),
            generation,
            vec![facts_key.clone()],
            encode(&facts.calls)?,
        );
        let chunks = ArtifactEnvelope::new(
            key(ArtifactKind::SemanticChunks),
            generation,
            vec![facts_key],
            encode(&facts.semantic_chunks)?,
        );
        Ok(vec![facts_artifact, symbols, calls, chunks])
    }
}

fn discover(root: &Path) -> Vec<PathBuf> {
    let mut paths = ProjectWalker::new(root)
        .iter()
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(|entry| entry.into_path())
        .filter(|path| Language::from_path(path).is_some())
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn manifest_revision(root: &Path, paths: &[PathBuf]) -> TldrResult<RevisionId> {
    let mut hasher = blake3::Hasher::new();
    for path in paths {
        hasher.update(relative(root, path).as_bytes());
        hasher.update(&std::fs::read(path)?);
    }
    Ok(RevisionId(*hasher.finalize().as_bytes()))
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn subject_changed(subject: &ArtifactSubject, changed: &HashSet<String>) -> bool {
    matches!(subject, ArtifactSubject::File(path) if changed.contains(path))
}

fn artifact_order(key: &ArtifactKey) -> String {
    format!("{:?}:{:?}", key.subject, key.kind)
}

#[allow(dead_code)]
fn ingestion_error(message: impl Into<String>) -> TldrError {
    TldrError::ParseError {
        file: PathBuf::from("<ingestion>"),
        line: None,
        message: message.into(),
    }
}

//! One resumable engine for full-project and file-delta ingestion.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;

use crate::walker::ProjectWalker;
use crate::{Language, TldrError, TldrResult};

use super::redb::encode;
use super::{
    ArtifactBatch, ArtifactEnvelope, ArtifactKey, ArtifactKind, ArtifactStore, ArtifactSubject,
    FileFacts, FileFactsParser, GenerationManifest, IngestionJob, IngestionScope, IngestionStage,
    ProducerId, ProjectCallEdgeFact, ProjectId, RevisionId,
};

const BATCH_FILES: usize = 32;
const FILE_FACTS_PRODUCER: &str = "file-facts";
const FILE_FACTS_VERSION: u32 = 3;

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
        let (source_revision, revisions) = source_manifest(&self.root, &all_files)?;
        let active = self.store.active_generation()?.unwrap_or(0);
        let generation = active.saturating_add(1);
        let revision_tag = source_revision.0[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let job_id = format!("ingestion-{generation}-{revision_tag}");
        let previous = self.store.generation(active)?;
        let previous_keys = previous
            .as_ref()
            .map(|manifest| manifest.artifacts.as_slice())
            .unwrap_or_default();

        let selected: Vec<PathBuf> = match &scope {
            IngestionScope::Project => all_files
                .iter()
                .filter(|path| {
                    let relative = relative(&self.root, path);
                    let revision = revisions.get(&relative);
                    !previous_keys.iter().any(|key| {
                        key.kind == ArtifactKind::FileFacts
                            && key.producer
                                == ProducerId::new(FILE_FACTS_PRODUCER, FILE_FACTS_VERSION)
                            && key.revision == *revision.expect("discovered file has revision")
                            && key.subject == ArtifactSubject::File(relative.clone())
                    })
                })
                .cloned()
                .collect(),
            IngestionScope::Files(files) => {
                let wanted = files.iter().cloned().collect::<HashSet<_>>();
                all_files
                    .iter()
                    .filter(|path| {
                        let relative = relative(&self.root, path);
                        let revision = revisions.get(&relative);
                        wanted.contains(&relative)
                            && !previous_keys.iter().any(|key| {
                                key.kind == ArtifactKind::FileFacts
                                    && key.producer
                                        == ProducerId::new(FILE_FACTS_PRODUCER, FILE_FACTS_VERSION)
                                    && key.revision
                                        == *revision.expect("discovered file has revision")
                                    && key.subject == ArtifactSubject::File(relative.clone())
                            })
                    })
                    .cloned()
                    .collect()
            }
        };

        let existing = self.store.job(&job_id)?;
        let resumed = existing.is_some();
        let next_batch = existing.as_ref().map_or(0, |job| job.next_batch);
        let total_batches = selected.len().div_ceil(BATCH_FILES) as u64;
        let remaining = selected
            .iter()
            .skip(next_batch as usize * BATCH_FILES)
            .cloned()
            .collect::<Vec<_>>();
        let parser = FileFactsParser::default();
        let facts = remaining
            .par_iter()
            .map(|path| parser.parse(&self.root, path))
            .collect::<TldrResult<Vec<_>>>()?;
        let parsed_files = parser.invocations() as usize;

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
            let selected_paths = selected
                .iter()
                .map(|path| relative(&self.root, path))
                .collect::<HashSet<_>>();
            manifest_keys.retain(|key| !subject_changed(&key.subject, &selected_paths));
        }

        // A resumed generation already owns committed records that are not yet
        // reachable from an active manifest. Recover those keys before adding
        // the remaining batches.
        for kind in ALL_ARTIFACT_KINDS {
            manifest_keys.extend(
                self.store
                    .artifacts(generation, kind)?
                    .into_iter()
                    .map(|artifact| artifact.key),
            );
        }

        let mut committed_artifacts = 0usize;
        for (offset, chunk) in facts.chunks(BATCH_FILES).enumerate() {
            let index = next_batch + offset as u64;
            let mut artifacts = chunk
                .iter()
                .map(|facts| self.derive(generation, facts))
                .collect::<TldrResult<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            artifacts
                .sort_by(|left, right| artifact_order(&left.key).cmp(&artifact_order(&right.key)));
            let job = IngestionJob {
                id: job_id.clone(),
                target_generation: generation,
                scope: scope.clone(),
                stage: IngestionStage::Commit,
                next_batch: index + 1,
                total_batches,
                source_revision,
            };
            manifest_keys.extend(artifacts.iter().map(|artifact| artifact.key.clone()));
            committed_artifacts += artifacts.len();
            self.store.commit_batch(
                &ArtifactBatch {
                    generation,
                    artifacts,
                },
                &job,
            )?;
        }
        manifest_keys.sort_by_key(artifact_order);
        manifest_keys.dedup();

        let call_dependencies = manifest_keys
            .iter()
            .filter(|key| key.kind == ArtifactKind::CallEdges)
            .cloned()
            .collect::<Vec<_>>();
        let mut project_calls = Vec::new();
        for dependency in &call_dependencies {
            let Some(artifact) = self.store.artifact(dependency)? else {
                return Err(ingestion_error("call-edge dependency disappeared"));
            };
            let path = match &dependency.subject {
                ArtifactSubject::File(path) => path.clone(),
                _ => continue,
            };
            let calls: Vec<super::CallFact> = super::redb::decode(&artifact.payload)?;
            project_calls.extend(calls.into_iter().map(|call| ProjectCallEdgeFact {
                source_file: path.clone(),
                caller: call.caller,
                destination_file: path.clone(),
                callee: call.callee,
            }));
        }
        let graph_key = ArtifactKey {
            project: self.project,
            revision: source_revision,
            subject: ArtifactSubject::Project,
            kind: ArtifactKind::CallGraph,
            producer: ProducerId::new("project-call-graph", 1),
        };
        let graph = ArtifactEnvelope::new(
            graph_key.clone(),
            generation,
            call_dependencies,
            encode(&project_calls)?,
        );
        manifest_keys.retain(|key| key.kind != ArtifactKind::CallGraph);
        manifest_keys.push(graph_key);

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
                artifacts: vec![graph],
            },
            &validation_job,
        )?;
        committed_artifacts += 1;
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
            artifacts: committed_artifacts,
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
        let references = ArtifactEnvelope::new(
            key(ArtifactKind::References),
            generation,
            vec![facts_key.clone()],
            encode(&facts.calls)?,
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
        Ok(vec![facts_artifact, symbols, references, calls, chunks])
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

const ALL_ARTIFACT_KINDS: [ArtifactKind; 10] = [
    ArtifactKind::FileFacts,
    ArtifactKind::Symbols,
    ArtifactKind::References,
    ArtifactKind::CallEdges,
    ArtifactKind::CallGraph,
    ArtifactKind::Cfg,
    ArtifactKind::Dfg,
    ArtifactKind::Pdg,
    ArtifactKind::SemanticChunks,
    ArtifactKind::Embeddings,
];

fn source_manifest(
    root: &Path,
    paths: &[PathBuf],
) -> TldrResult<(RevisionId, HashMap<String, RevisionId>)> {
    let mut hasher = blake3::Hasher::new();
    let mut revisions = HashMap::with_capacity(paths.len());
    for path in paths {
        let relative = relative(root, path);
        let bytes = std::fs::read(path)?;
        let revision = RevisionId::for_bytes(&bytes);
        hasher.update(relative.as_bytes());
        hasher.update(&revision.0);
        revisions.insert(relative, revision);
    }
    Ok((RevisionId(*hasher.finalize().as_bytes()), revisions))
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

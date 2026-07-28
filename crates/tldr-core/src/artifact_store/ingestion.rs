//! One resumable engine for full-project and file-delta ingestion.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::build_timing::{PhaseTiming, UnitSummary, UnitTimingCollector};
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
const FILE_FACTS_VERSION: u32 = 7;

/// Summary of a completed or resumed ingestion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestionReport {
    /// Correlation identifier shared with semantic workers.
    pub run_id: String,
    /// Component role that produced this report.
    pub process_role: String,
    /// Invocation start time, unix epoch milliseconds.
    pub started_at_unix_ms: u64,
    /// End-to-end ingestion wall time.
    pub duration_ms: u64,
    /// Major structural wall-time phases.
    pub phases: Vec<PhaseTiming>,
    /// Bounded atomic-unit timing distributions.
    pub units: Vec<UnitSummary>,
    /// Published generation.
    pub generation: u64,
    /// Number of source files parsed during this invocation.
    pub parsed_files: usize,
    /// Number of committed artifact records.
    pub artifacts: usize,
    /// Whether an existing durable checkpoint was resumed.
    pub resumed: bool,
}

/// Timing controls for one ingestion invocation.
#[derive(Clone, Debug)]
pub struct IngestionTimingOptions {
    /// Correlation identifier shared by the owning build.
    pub run_id: String,
    /// Process/component role.
    pub process_role: String,
    /// Optional exact per-unit JSONL output.
    pub detail_path: Option<PathBuf>,
}

impl Default for IngestionTimingOptions {
    fn default() -> Self {
        let started_at_unix_ms = unix_millis();
        Self {
            run_id: format!("{started_at_unix_ms}-{}", std::process::id()),
            process_role: "artifact_producer".into(),
            detail_path: None,
        }
    }
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
        let root = dunce::canonicalize(root)?;
        let project = ProjectId::for_root(&root)?;
        Ok(Self {
            root,
            project,
            store,
        })
    }

    /// Build or resume either the complete project or a file subset.
    pub fn ingest(&self, scope: IngestionScope) -> TldrResult<IngestionReport> {
        self.ingest_with_timing(scope, IngestionTimingOptions::default())
    }

    /// Build with an explicit run identity and optional raw unit output.
    pub fn ingest_with_timing(
        &self,
        scope: IngestionScope,
        timing: IngestionTimingOptions,
    ) -> TldrResult<IngestionReport> {
        self.ingest_with_batch_limit(scope, None, timing)
    }

    /// Certification hook that interrupts after a fixed number of durable
    /// batches. A subsequent normal [`Self::ingest`] must resume that job.
    #[doc(hidden)]
    pub fn ingest_interrupted_after(
        &self,
        scope: IngestionScope,
        committed_batches: usize,
    ) -> TldrResult<IngestionReport> {
        self.ingest_with_batch_limit(
            scope,
            Some(committed_batches),
            IngestionTimingOptions::default(),
        )
    }

    fn ingest_with_batch_limit(
        &self,
        scope: IngestionScope,
        batch_limit: Option<usize>,
        timing: IngestionTimingOptions,
    ) -> TldrResult<IngestionReport> {
        let invocation_started = Instant::now();
        let started_at_unix_ms = unix_millis();
        let mut phases = Vec::new();
        let mut units = UnitTimingCollector::new(
            timing.run_id.clone(),
            timing.process_role.clone(),
            timing.detail_path.as_deref(),
        )?;
        let scope = normalize_scope(scope);
        let active = self.store.active_generation()?.unwrap_or(0);
        let generation = active.saturating_add(1);
        let previous = self.store.generation(active)?;
        let discovery_started = Instant::now();
        let (all_files, source_revision, revisions, removed_files) = match &scope {
            IngestionScope::Project => {
                let all_files = discover(&self.root);
                let (source_revision, revisions) = source_manifest(&self.root, &all_files)?;
                let removed_files =
                    removed_files(previous.as_ref(), |path| !revisions.contains_key(path));
                (all_files, source_revision, revisions, removed_files)
            }
            IngestionScope::Files(files) => file_scope_manifest(
                &self.root,
                files,
                previous.as_ref().map(|manifest| manifest.source_revision),
            )?,
        };
        phases.push(PhaseTiming {
            name: "source_discovery".into(),
            duration_ms: elapsed_ms(discovery_started),
        });
        let revision_tag = source_revision.0[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let job_id = format!(
            "ingestion-{generation}-{revision_tag}-{}",
            scope_tag(&scope)
        );
        let previous_keys = previous
            .as_ref()
            .map(|manifest| manifest.artifacts.as_slice())
            .unwrap_or_default();
        let fresh_file_facts = previous_keys
            .iter()
            .filter(|key| {
                key.kind == ArtifactKind::FileFacts
                    && key.producer == ProducerId::new(FILE_FACTS_PRODUCER, FILE_FACTS_VERSION)
            })
            .map(|key| (key.subject.clone(), key.revision))
            .collect::<HashSet<_>>();
        let selected: Vec<PathBuf> = match &scope {
            IngestionScope::Project => all_files
                .iter()
                .filter(|path| {
                    let relative = relative(&self.root, path);
                    !fresh_file_facts.contains(&(
                        ArtifactSubject::File(relative.clone()),
                        *revisions
                            .get(&relative)
                            .expect("discovered file has revision"),
                    ))
                })
                .cloned()
                .collect(),
            IngestionScope::Files(files) => {
                let wanted = files.iter().cloned().collect::<HashSet<_>>();
                all_files
                    .iter()
                    .filter(|path| {
                        let relative = relative(&self.root, path);
                        wanted.contains(&relative)
                            && !fresh_file_facts.contains(&(
                                ArtifactSubject::File(relative.clone()),
                                *revisions
                                    .get(&relative)
                                    .expect("discovered file has revision"),
                            ))
                    })
                    .cloned()
                    .collect()
            }
        };

        let existing = self.store.job(&job_id)?.filter(|job| {
            job.target_generation == generation
                && job.scope == scope
                && job.source_revision == source_revision
                && job.stage != IngestionStage::Published
        });
        let resumed = existing.is_some();
        let next_batch = existing.as_ref().map_or(0, |job| job.next_batch);
        let total_batches = selected.len().div_ceil(BATCH_FILES) as u64;
        let remaining = selected
            .iter()
            .skip(next_batch as usize * BATCH_FILES)
            .cloned()
            .collect::<Vec<_>>();
        let parser = FileFactsParser::default();
        let parse_started = Instant::now();
        let timed_facts = remaining
            .par_iter()
            .map(|path| {
                let started = Instant::now();
                let facts = parser.parse(&self.root, path)?;
                Ok((facts, relative(&self.root, path), elapsed_ms(started)))
            })
            .collect::<TldrResult<Vec<_>>>()?;
        phases.push(PhaseTiming {
            name: "ast_parse".into(),
            duration_ms: elapsed_ms(parse_started),
        });
        let facts = timed_facts
            .into_iter()
            .map(|(facts, path, duration_ms)| {
                units.record_grouped("ast_parse", facts.language.clone(), path, duration_ms);
                facts
            })
            .collect::<Vec<_>>();
        let parsed_files = parser.invocations() as usize;

        let regenerated = match &scope {
            IngestionScope::Project => selected
                .iter()
                .map(|path| relative(&self.root, path))
                .chain(removed_files.iter().cloned())
                .collect::<HashSet<_>>(),
            IngestionScope::Files(files) => {
                let selected = selected
                    .iter()
                    .map(|path| relative(&self.root, path))
                    .collect::<HashSet<_>>();
                files
                    .iter()
                    .filter(|path| selected.contains(*path) || !revisions.contains_key(*path))
                    .cloned()
                    .collect::<HashSet<_>>()
            }
        };
        let mut manifest_keys = previous
            .map(|manifest| {
                manifest
                    .artifacts
                    .into_iter()
                    .filter(|key| !subject_changed(&key.subject, &regenerated))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        // A resumed generation already owns committed records that are not yet
        // reachable from an active manifest. Recover those keys before adding
        // the remaining batches. Only recover current-revision subjects in
        // this exact scope: an abandoned generation may otherwise contain
        // records from a different delta request.
        for kind in ALL_ARTIFACT_KINDS {
            manifest_keys.extend(
                self.store
                    .artifacts(generation, kind)?
                    .into_iter()
                    .filter(|artifact| {
                        subject_file(&artifact.key.subject).is_some_and(|path| {
                            regenerated.contains(path)
                                && revisions
                                    .get(path)
                                    .is_some_and(|revision| *revision == artifact.key.revision)
                        })
                    })
                    .map(|artifact| artifact.key),
            );
        }

        let artifact_started = Instant::now();
        let mut committed_artifacts = 0usize;
        for (offset, chunk) in facts.chunks(BATCH_FILES).enumerate() {
            let batch_started = Instant::now();
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
            units.record(
                "artifact_batch",
                None,
                index.to_string(),
                elapsed_ms(batch_started),
            );
            if batch_limit.is_some_and(|limit| offset + 1 >= limit) {
                return Err(ingestion_error("certification interruption"));
            }
        }
        phases.push(PhaseTiming {
            name: "artifact_write".into(),
            duration_ms: elapsed_ms(artifact_started),
        });
        manifest_keys.sort_by_key(artifact_order);
        manifest_keys.dedup();

        let callgraph_started = Instant::now();
        let call_dependencies = manifest_keys
            .iter()
            .filter(|key| key.kind == ArtifactKind::FileFacts)
            .cloned()
            .collect::<Vec<_>>();
        let decoded_irs = call_dependencies
            .par_iter()
            .map(|dependency| {
                let Some(artifact) = self.store.artifact(dependency)? else {
                    return Err(ingestion_error("file-facts dependency disappeared"));
                };
                let facts: FileFacts = super::redb::decode(&artifact.payload)?;
                let ir: crate::callgraph::FileIR =
                    ciborium::de::from_reader(facts.callgraph_ir.as_slice()).map_err(|error| {
                        ingestion_error(format!("call-graph artifact decoding failed: {error}"))
                    })?;
                Ok((facts.language, ir))
            })
            .collect::<TldrResult<Vec<_>>>()?;
        let mut file_irs = HashMap::<String, Vec<crate::callgraph::FileIR>>::new();
        for (language, ir) in decoded_irs {
            file_irs.entry(language).or_default().push(ir);
        }
        let mut project_calls = Vec::new();
        for (language, irs) in file_irs {
            let language_started = Instant::now();
            let config = crate::callgraph::BuildConfig {
                language: language.clone(),
                use_type_resolution: true,
                ..Default::default()
            };
            let graph = crate::callgraph::compose_call_graph_v2(&self.root, &config, irs)
                .map_err(|error| ingestion_error(error.to_string()))?;
            project_calls.extend(graph.edges.into_iter().map(|edge| ProjectCallEdgeFact {
                language: language.clone(),
                source_file: relative(&self.root, &edge.src_file),
                caller: edge.src_func,
                destination_file: relative(&self.root, &edge.dst_file),
                callee: edge.dst_func,
                call_type: call_type_name(edge.call_type).into(),
            }));
            units.record_grouped(
                "callgraph_compose",
                language.clone(),
                language,
                elapsed_ms(language_started),
            );
        }
        phases.push(PhaseTiming {
            name: "callgraph_compose".into(),
            duration_ms: elapsed_ms(callgraph_started),
        });
        let graph_key = ArtifactKey {
            project: self.project,
            revision: source_revision,
            subject: ArtifactSubject::Project,
            kind: ArtifactKind::CallGraph,
            producer: ProducerId::new("project-call-graph", 2),
        };
        let graph = ArtifactEnvelope::new(
            graph_key.clone(),
            generation,
            call_dependencies,
            encode(&project_calls)?,
        );
        manifest_keys.retain(|key| key.kind != ArtifactKind::CallGraph);
        manifest_keys.push(graph_key);

        let publication_started = Instant::now();
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
        phases.push(PhaseTiming {
            name: "publication".into(),
            duration_ms: elapsed_ms(publication_started),
        });
        units.finish()?;
        Ok(IngestionReport {
            run_id: timing.run_id,
            process_role: timing.process_role,
            started_at_unix_ms,
            duration_ms: invocation_started.elapsed().as_millis() as u64,
            phases,
            units: units.summaries(),
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

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
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

type FileScopeManifest = (
    Vec<PathBuf>,
    RevisionId,
    HashMap<String, RevisionId>,
    HashSet<String>,
);

fn file_scope_manifest(
    root: &Path,
    files: &[String],
    previous_revision: Option<RevisionId>,
) -> TldrResult<FileScopeManifest> {
    let mut selected_paths = Vec::with_capacity(files.len());
    let mut revisions = HashMap::with_capacity(files.len());
    let mut removed = HashSet::new();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"file-scope-generation-v1");
    if let Some(previous_revision) = previous_revision {
        hasher.update(&previous_revision.0);
    }

    for relative in files {
        let relative_path = Path::new(relative);
        if relative_path.as_os_str().is_empty()
            || relative_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(ingestion_error(format!(
                "file-scoped ingestion requires a normalized root-relative path: {relative}"
            )));
        }

        hasher.update(relative.as_bytes());
        let absolute = root.join(relative_path);
        if is_file_scope_source(root, &absolute) {
            let bytes = std::fs::read(&absolute)?;
            let revision = RevisionId::for_bytes(&bytes);
            hasher.update(b"present");
            hasher.update(&revision.0);
            revisions.insert(relative.clone(), revision);
            selected_paths.push(absolute);
        } else {
            hasher.update(b"absent");
            removed.insert(relative.clone());
        }
    }

    Ok((
        selected_paths,
        RevisionId(*hasher.finalize().as_bytes()),
        revisions,
        removed,
    ))
}

fn is_file_scope_source(root: &Path, path: &Path) -> bool {
    if !path.is_file() || Language::from_path(path).is_none() {
        return false;
    }
    #[cfg(feature = "semantic")]
    {
        crate::semantic::is_corpus_file(root, path)
    }
    #[cfg(not(feature = "semantic"))]
    {
        true
    }
}

fn removed_files(
    previous: Option<&GenerationManifest>,
    should_remove: impl Fn(&str) -> bool,
) -> HashSet<String> {
    previous
        .into_iter()
        .flat_map(|manifest| manifest.artifacts.iter())
        .filter(|key| key.kind == ArtifactKind::FileFacts)
        .filter_map(|key| subject_file(&key.subject))
        .filter(|path| should_remove(path))
        .map(ToOwned::to_owned)
        .collect()
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn subject_changed(subject: &ArtifactSubject, changed: &HashSet<String>) -> bool {
    match subject {
        ArtifactSubject::File(path) => changed.contains(path),
        ArtifactSubject::Symbol(anchor) => changed
            .iter()
            .any(|path| anchor == path || anchor.starts_with(&format!("{path}::"))),
        ArtifactSubject::Project => false,
    }
}

fn subject_file(subject: &ArtifactSubject) -> Option<&str> {
    match subject {
        ArtifactSubject::File(path) => Some(path),
        ArtifactSubject::Symbol(anchor) => anchor.split_once("::").map(|(path, _)| path),
        ArtifactSubject::Project => None,
    }
}

fn normalize_scope(scope: IngestionScope) -> IngestionScope {
    match scope {
        IngestionScope::Project => IngestionScope::Project,
        IngestionScope::Files(mut files) => {
            files.sort();
            files.dedup();
            IngestionScope::Files(files)
        }
    }
}

fn scope_tag(scope: &IngestionScope) -> String {
    match scope {
        IngestionScope::Project => "project".into(),
        IngestionScope::Files(files) => {
            let mut hasher = blake3::Hasher::new();
            for file in files {
                hasher.update(file.as_bytes());
                hasher.update(&[0]);
            }
            hasher.finalize().to_hex()[..16].to_owned()
        }
    }
}

fn artifact_order(key: &ArtifactKey) -> String {
    format!("{:?}:{:?}", key.subject, key.kind)
}

fn call_type_name(call_type: crate::callgraph::CallType) -> &'static str {
    use crate::callgraph::CallType;
    match call_type {
        CallType::Intra => "intra",
        CallType::Direct => "direct",
        CallType::LocalImport => "local-import",
        CallType::Method => "method",
        CallType::Attr => "attr",
        CallType::Ref => "ref",
        CallType::Static => "static",
    }
}

#[allow(dead_code)]
fn ingestion_error(message: impl Into<String>) -> TldrError {
    TldrError::ParseError {
        file: PathBuf::from("<ingestion>"),
        line: None,
        message: message.into(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    use super::{IngestionEngine, IngestionScope};
    use crate::artifact_store::{ArtifactStore, RedbArtifactStore};

    #[test]
    fn file_scope_does_not_read_unchanged_sources() {
        let project = tempfile::tempdir().expect("project");
        let store_dir = tempfile::tempdir().expect("store");
        let changed = project.path().join("changed.rs");
        let untouched = project.path().join("untouched.rs");
        std::fs::write(&changed, "pub fn changed() -> u8 { 1 }\n").expect("write changed");
        std::fs::write(&untouched, "pub fn untouched() -> u8 { 2 }\n").expect("write untouched");

        let store: Arc<dyn ArtifactStore> = Arc::new(
            RedbArtifactStore::open(&store_dir.path().join("artifacts.redb")).expect("open store"),
        );
        let engine = IngestionEngine::new(project.path(), store).expect("engine");
        engine
            .ingest(IngestionScope::Project)
            .expect("baseline generation");

        std::fs::write(&changed, "pub fn changed() -> u8 { 3 }\n").expect("edit changed");
        let mut permissions = std::fs::metadata(&untouched)
            .expect("untouched metadata")
            .permissions();
        permissions.set_mode(0o000);
        std::fs::set_permissions(&untouched, permissions).expect("make unreadable");

        let result = engine.ingest(IngestionScope::Files(vec!["changed.rs".into()]));

        let mut permissions = std::fs::metadata(&untouched)
            .expect("unreadable metadata")
            .permissions();
        permissions.set_mode(0o644);
        std::fs::set_permissions(&untouched, permissions).expect("restore permissions");

        let report = result.expect("file-scoped generation must not read untouched source");
        assert_eq!(report.parsed_files, 1);
    }
}

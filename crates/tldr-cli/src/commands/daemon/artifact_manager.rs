//! Daemon ownership of the authoritative project artifact generation.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use tldr_core::artifact_store::{
    schema::STORE_FILE, ArtifactKey, ArtifactKind, ArtifactStore, ArtifactSubject, FileFacts,
    FunctionArtifactCoordinator, GenerationSnapshot, IngestionEngine, IngestionReport,
    IngestionScope, ProducerId, ProjectId, RedbArtifactStore,
};
use tldr_core::{CfgInfo, DfgInfo, Language, TldrError};

/// Lifecycle of the shared structural/semantic artifact generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtifactState {
    /// No complete generation exists yet.
    Cold,
    /// One bulk or delta generation is being constructed.
    Building {
        /// Generation being constructed.
        target_generation: u64,
    },
    /// Queries may pin this complete generation.
    Ready {
        /// Atomically published generation.
        generation: u64,
    },
    /// The last build failed; an older generation remains queryable when set.
    Failed {
        /// Last complete generation, if one exists.
        active_generation: Option<u64>,
        /// Durable-build failure surfaced to operators.
        error: String,
    },
}

/// Storage and hot-snapshot statistics exposed without JSON persistence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArtifactStats {
    /// Published generation.
    pub active_generation: Option<u64>,
    /// Number of normalized files in the resident snapshot.
    pub hot_files: usize,
    /// redb file size.
    pub redb_bytes: u64,
}

/// Single daemon coordinator for full and incremental artifact ingestion.
pub struct ArtifactManager {
    project: PathBuf,
    store: Arc<RedbArtifactStore>,
    state: RwLock<ArtifactState>,
    hot: RwLock<Option<Arc<GenerationSnapshot>>>,
    writer: Mutex<()>,
    functions: FunctionArtifactCoordinator,
}

impl ArtifactManager {
    /// Open the new, incompatible store without consulting legacy caches.
    pub fn open(project: &Path) -> tldr_core::TldrResult<Self> {
        let project = dunce::canonicalize(project)?;
        let store = Arc::new(RedbArtifactStore::open(
            &project.join(".tldr").join("store").join(STORE_FILE),
        )?);
        // A producer/schema cutover may leave a previously complete generation
        // whose payload no longer decodes. Treat it as cold and rebuild from
        // source; never fall back to a legacy cache.
        let snapshot = GenerationSnapshot::active(store.as_ref())
            .ok()
            .flatten()
            .map(Arc::new);
        let state =
            snapshot
                .as_ref()
                .map_or(ArtifactState::Cold, |snapshot| ArtifactState::Ready {
                    generation: snapshot.generation(),
                });
        let functions = FunctionArtifactCoordinator::new(store.clone());
        Ok(Self {
            project,
            store,
            state: RwLock::new(state),
            hot: RwLock::new(snapshot),
            writer: Mutex::new(()),
            functions,
        })
    }

    /// Current non-blocking lifecycle snapshot.
    pub fn state(&self) -> ArtifactState {
        self.state.read().expect("artifact state poisoned").clone()
    }

    /// Pin the current immutable generation for an entire request.
    pub fn snapshot(&self) -> Result<Arc<GenerationSnapshot>, ArtifactState> {
        self.hot
            .read()
            .expect("artifact snapshot poisoned")
            .clone()
            .ok_or_else(|| self.state())
    }

    /// Read one file's facts from a pinned active generation.
    pub fn file_facts(&self, file: &Path) -> Result<FileFacts, ArtifactState> {
        let relative = file
            .strip_prefix(&self.project)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        self.snapshot()?
            .file(relative)
            .cloned()
            .ok_or_else(|| self.state())
    }

    /// Load or persist one demand-driven CFG.
    pub fn cfg(
        &self,
        file: &Path,
        function: &str,
        language: Language,
    ) -> tldr_core::TldrResult<CfgInfo> {
        let facts = self.file_facts(file).map_err(not_ready)?;
        let (facts_key, cfg_key) =
            self.function_keys_for_facts(&facts, function, ArtifactKind::Cfg, "cfg", 1)?;
        let source = file.to_string_lossy().into_owned();
        self.functions.materialize(cfg_key, vec![facts_key], || {
            tldr_core::get_cfg_context(&source, function, language)
        })
    }

    /// Load or persist one demand-driven DFG with an explicit CFG dependency.
    pub fn dfg(
        &self,
        file: &Path,
        function: &str,
        language: Language,
    ) -> tldr_core::TldrResult<DfgInfo> {
        let facts = self.file_facts(file).map_err(not_ready)?;
        let (facts_key, cfg_key) =
            self.function_keys_for_facts(&facts, function, ArtifactKind::Cfg, "cfg", 1)?;
        let (_, dfg_key) =
            self.function_keys_for_facts(&facts, function, ArtifactKind::Dfg, "dfg", 1)?;
        let source = file.to_string_lossy().into_owned();
        // Materialize both artifacts from one pinned FileFacts revision.
        let cfg_source = source.clone();
        self.functions
            .materialize(cfg_key.clone(), vec![facts_key], || {
                tldr_core::get_cfg_context(&cfg_source, function, language)
            })?;
        self.functions.materialize(dfg_key, vec![cfg_key], || {
            tldr_core::get_dfg_context(&source, function, language)
        })
    }

    /// Start or resume a full generation, reusing unchanged file artifacts.
    pub fn warm(&self) -> tldr_core::TldrResult<IngestionReport> {
        self.ingest(IngestionScope::Project)
    }

    /// Submit a canonical source change through the same resumable engine.
    pub fn apply_delta(&self, file: &Path) -> tldr_core::TldrResult<IngestionReport> {
        let relative = delta_relative(&self.project, file)?;
        self.ingest(IngestionScope::Files(vec![relative]))
    }

    /// Current storage and resident-view footprint.
    pub fn stats(&self) -> ArtifactStats {
        let snapshot = self.hot.read().expect("artifact snapshot poisoned").clone();
        ArtifactStats {
            active_generation: snapshot.as_ref().map(|snapshot| snapshot.generation()),
            hot_files: snapshot
                .as_ref()
                .map_or(0, |snapshot| snapshot.file_count()),
            redb_bytes: std::fs::metadata(self.store.path())
                .map(|metadata| metadata.len())
                .unwrap_or(0),
        }
    }

    /// Atomically join the current usearch generation to the pinned project
    /// manifest after semantic publication succeeds.
    pub fn attach_vector_generation(
        &self,
        expected_generation: u64,
        vector_generation: u64,
    ) -> tldr_core::TldrResult<()> {
        let generation = self
            .store
            .active_generation()?
            .ok_or_else(|| TldrError::DaemonError("artifact store is not ready".into()))?;
        if generation != expected_generation {
            return Err(TldrError::DaemonError(format!(
                "artifact generation changed during semantic build: expected {expected_generation}, active {generation}"
            )));
        }
        self.store
            .set_vector_generation(generation, vector_generation)
    }

    fn ingest(&self, scope: IngestionScope) -> tldr_core::TldrResult<IngestionReport> {
        let _writer = self.writer.lock().expect("artifact writer poisoned");
        let target_generation = self.store.active_generation()?.unwrap_or(0) + 1;
        *self.state.write().expect("artifact state poisoned") =
            ArtifactState::Building { target_generation };

        let result = IngestionEngine::new(&self.project, self.store.clone())
            .and_then(|engine| engine.ingest(scope));
        match result {
            Ok(report) => {
                let snapshot = Arc::new(GenerationSnapshot::load(
                    self.store.as_ref(),
                    report.generation,
                )?);
                *self.hot.write().expect("artifact snapshot poisoned") = Some(snapshot);
                *self.state.write().expect("artifact state poisoned") = ArtifactState::Ready {
                    generation: report.generation,
                };
                Ok(report)
            }
            Err(error) => {
                let active_generation = self.store.active_generation().ok().flatten();
                *self.state.write().expect("artifact state poisoned") = ArtifactState::Failed {
                    active_generation,
                    error: error.to_string(),
                };
                Err(error)
            }
        }
    }

    fn function_keys_for_facts(
        &self,
        facts: &FileFacts,
        function: &str,
        kind: ArtifactKind,
        producer: &str,
        version: u32,
    ) -> tldr_core::TldrResult<(ArtifactKey, ArtifactKey)> {
        let project = ProjectId::for_root(&self.project)?;
        let facts_key = ArtifactKey {
            project,
            revision: facts.revision,
            subject: ArtifactSubject::File(facts.path.clone()),
            kind: ArtifactKind::FileFacts,
            producer: ProducerId::new("file-facts", 4),
        };
        let key = ArtifactKey {
            project,
            revision: facts.revision,
            subject: ArtifactSubject::Symbol(format!("{}::{function}", facts.path)),
            kind,
            producer: ProducerId::new(producer, version),
        };
        Ok((facts_key, key))
    }
}

fn delta_relative(project: &Path, file: &Path) -> tldr_core::TldrResult<String> {
    let resolved = if file.exists() {
        dunce::canonicalize(file)?
    } else {
        let mut ancestor = file;
        while !ancestor.exists() {
            ancestor = ancestor.parent().ok_or_else(|| {
                TldrError::DaemonError(format!(
                    "delta path {} has no existing ancestor",
                    file.display()
                ))
            })?;
        }
        let canonical_ancestor = dunce::canonicalize(ancestor)?;
        let suffix = file.strip_prefix(ancestor).map_err(|_| {
            TldrError::DaemonError(format!(
                "delta path {} cannot be normalized",
                file.display()
            ))
        })?;
        if suffix
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(TldrError::DaemonError(format!(
                "delta path {} contains non-normal components",
                file.display()
            )));
        }
        canonical_ancestor.join(suffix)
    };
    let relative = resolved.strip_prefix(project).map_err(|_| {
        TldrError::DaemonError(format!(
            "delta path {} is outside project root {}",
            file.display(),
            project.display()
        ))
    })?;
    relative
        .to_str()
        .map(|value| value.replace('\\', "/"))
        .ok_or_else(|| TldrError::DaemonError("delta path is not valid UTF-8".into()))
}

fn not_ready(state: ArtifactState) -> TldrError {
    TldrError::DaemonError(format!("artifact generation is not ready: {state:?}"))
}

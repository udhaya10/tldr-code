//! Daemon ownership of the authoritative project artifact generation.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use tldr_core::artifact_store::{
    schema::STORE_FILE, ArtifactStore, GenerationSnapshot, IngestionEngine, IngestionReport,
    IngestionScope, RedbArtifactStore,
};

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
}

impl ArtifactManager {
    /// Open the new, incompatible store without consulting legacy caches.
    pub fn open(project: &Path) -> tldr_core::TldrResult<Self> {
        let project = dunce::canonicalize(project)?;
        let store = Arc::new(RedbArtifactStore::open(
            &project.join(".tldr").join("store").join(STORE_FILE),
        )?);
        let snapshot = GenerationSnapshot::active(store.as_ref())?.map(Arc::new);
        let state =
            snapshot
                .as_ref()
                .map_or(ArtifactState::Cold, |snapshot| ArtifactState::Ready {
                    generation: snapshot.generation(),
                });
        Ok(Self {
            project,
            store,
            state: RwLock::new(state),
            hot: RwLock::new(snapshot),
            writer: Mutex::new(()),
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

    /// Start or resume a full generation, reusing unchanged file artifacts.
    pub fn warm(&self) -> tldr_core::TldrResult<IngestionReport> {
        self.ingest(IngestionScope::Project)
    }

    /// Submit a canonical source change through the same resumable engine.
    pub fn apply_delta(&self, file: &Path) -> tldr_core::TldrResult<IngestionReport> {
        let relative = file
            .strip_prefix(&self.project)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
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
}

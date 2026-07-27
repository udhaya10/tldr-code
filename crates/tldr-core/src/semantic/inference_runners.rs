//! Workload-specific embedding sessions for daemon query, delta, and bulk paths.
//!
//! Query and delta runners deliberately own distinct ONNX sessions. Bulk work
//! is represented by a separately serialized boundary today; Epic 10 moves
//! that boundary into a resumable child process without changing status shape.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use serde::{Deserialize, Serialize};

use super::{
    Embedder, EmbeddingModel, FixedShapeEmbedder, OrtBackendConfig, ShapeObservation, TokenBudget,
};

/// Stable workload names surfaced through daemon status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceWorkload {
    /// Latency-oriented, exact batch-one query inference.
    Query,
    /// Small fixed-shape file-delta inference.
    Delta,
    /// Full-build boundary (in-process until the Epic 10 worker).
    Bulk,
}

impl InferenceWorkload {
    /// Stable status label.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Delta => "delta",
            Self::Bulk => "bulk",
        }
    }
}

/// Point-in-time state for one workload runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InferenceRunnerSnapshot {
    /// Stable workload label.
    pub workload: InferenceWorkload,
    /// `cold`, `ready`, or `busy`.
    pub state: String,
    /// Active model, when one has been selected.
    pub model: Option<String>,
    /// Number of session constructions (bulk: isolated run boundaries).
    pub sessions_built: u64,
    /// Number of successful requests/runs.
    pub requests: u64,
    /// Failed requests/runs.
    pub failures: u64,
    /// Finite tensor shapes observed by this session.
    pub exact_shapes: Vec<(usize, usize)>,
}

struct FixedSession {
    model: EmbeddingModel,
    embedder: FixedShapeEmbedder,
}

/// One independently owned fixed-shape ONNX session.
pub struct FixedShapeInferenceRunner {
    workload: InferenceWorkload,
    batch_one: bool,
    session: Mutex<Option<FixedSession>>,
    active_model: RwLock<Option<EmbeddingModel>>,
    busy: AtomicUsize,
    sessions_built: AtomicU64,
    requests: AtomicU64,
    failures: AtomicU64,
}

impl FixedShapeInferenceRunner {
    /// A finite-shape query runner emitting only batch-one tensors.
    pub fn query() -> Self {
        Self::new(InferenceWorkload::Query, true)
    }

    /// A small document runner isolated from query inference state.
    pub fn delta() -> Self {
        Self::new(InferenceWorkload::Delta, false)
    }

    fn new(workload: InferenceWorkload, batch_one: bool) -> Self {
        Self {
            workload,
            batch_one,
            session: Mutex::new(None),
            active_model: RwLock::new(None),
            busy: AtomicUsize::new(0),
            sessions_built: AtomicU64::new(0),
            requests: AtomicU64::new(0),
            failures: AtomicU64::new(0),
        }
    }

    fn ensure_session<'a>(
        &self,
        guard: &'a mut Option<FixedSession>,
        model: EmbeddingModel,
    ) -> Result<&'a mut FixedSession, String> {
        if !guard.as_ref().is_some_and(|session| session.model == model) {
            let embedder = Embedder::new(model)
                .map_err(|error| error.to_string())?
                .into_fixed_shape(OrtBackendConfig::default())
                .map_err(|error| error.to_string())?;
            *guard = Some(FixedSession { model, embedder });
            *write_lock(&self.active_model) = Some(model);
            self.sessions_built.fetch_add(1, Ordering::Relaxed);
        }
        Ok(guard.as_mut().expect("session initialized above"))
    }

    /// Embed one query with the exact Arctic prefix and batch-one shape.
    pub fn embed_query(&self, model: EmbeddingModel, query: &str) -> Result<Vec<f32>, String> {
        if self.workload != InferenceWorkload::Query || !self.batch_one {
            return Err("embed_query requires the query runner".to_string());
        }
        let _busy = BusyGuard::new(&self.busy);
        let result = (|| {
            let document = Embedder::query_document(model, query);
            let mut guard = mutex_lock(&self.session);
            let session = self.ensure_session(&mut guard, model)?;
            let mut vectors = session
                .embedder
                .embed_indexed_batch_one(vec![(0, document.as_str())])
                .map_err(|error| error.to_string())?;
            vectors
                .pop()
                .map(|(_, vector)| vector)
                .ok_or_else(|| "query runner returned no vector".to_string())
        })();
        self.record_result(&result);
        result
    }

    /// Run structural planning against the delta session's exact tokenizer.
    pub fn with_token_budget<T>(
        &self,
        model: EmbeddingModel,
        operation: impl FnOnce(&TokenBudget) -> Result<T, String>,
    ) -> Result<T, String> {
        if self.workload != InferenceWorkload::Delta {
            return Err("token-budget planning requires the delta runner".to_string());
        }
        let _busy = BusyGuard::new(&self.busy);
        let result = (|| {
            let mut guard = mutex_lock(&self.session);
            let session = self.ensure_session(&mut guard, model)?;
            operation(session.embedder.token_budget())
        })();
        if result.is_err() {
            self.failures.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    /// Embed delta documents on the delta-only fixed-shape session.
    pub fn embed_documents(
        &self,
        model: EmbeddingModel,
        indexed: Vec<(usize, &str)>,
    ) -> Result<Vec<(usize, Vec<f32>)>, String> {
        if self.workload != InferenceWorkload::Delta || self.batch_one {
            return Err("document embedding requires the delta runner".to_string());
        }
        let _busy = BusyGuard::new(&self.busy);
        let result = (|| {
            let mut guard = mutex_lock(&self.session);
            let session = self.ensure_session(&mut guard, model)?;
            session
                .embedder
                .embed_indexed(indexed)
                .map_err(|error| error.to_string())
        })();
        self.record_result(&result);
        result
    }

    fn record_result<T>(&self, result: &Result<T, String>) {
        if result.is_ok() {
            self.requests.fetch_add(1, Ordering::Relaxed);
        } else {
            self.failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Non-blocking state snapshot; status never waits behind inference.
    pub fn snapshot(&self) -> InferenceRunnerSnapshot {
        let busy = self.busy.load(Ordering::Relaxed) > 0;
        let exact_shapes = self
            .session
            .try_lock()
            .ok()
            .and_then(|guard| {
                guard.as_ref().map(|session| {
                    session
                        .embedder
                        .shape_observations()
                        .into_iter()
                        .map(
                            |ShapeObservation {
                                 batch, sequence, ..
                             }| (batch, sequence),
                        )
                        .collect()
                })
            })
            .unwrap_or_default();
        let sessions_built = self.sessions_built.load(Ordering::Relaxed);
        InferenceRunnerSnapshot {
            workload: self.workload,
            state: if busy {
                "busy"
            } else if sessions_built == 0 {
                "cold"
            } else {
                "ready"
            }
            .to_string(),
            model: read_lock(&self.active_model)
                .as_ref()
                .map(|model| model.model_name().to_string()),
            sessions_built,
            requests: self.requests.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
            exact_shapes,
        }
    }
}

/// Serialized bulk-build boundary with independently observable state.
pub struct BulkInferenceRunner {
    serial: Mutex<()>,
    active_model: RwLock<Option<EmbeddingModel>>,
    busy: AtomicUsize,
    runs: AtomicU64,
    failures: AtomicU64,
}

impl Default for BulkInferenceRunner {
    fn default() -> Self {
        Self {
            serial: Mutex::new(()),
            active_model: RwLock::new(None),
            busy: AtomicUsize::new(0),
            runs: AtomicU64::new(0),
            failures: AtomicU64::new(0),
        }
    }
}

impl BulkInferenceRunner {
    /// Serialize one bulk build without sharing query or delta session state.
    pub fn run<T>(
        &self,
        model: EmbeddingModel,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _serial = mutex_lock(&self.serial);
        *write_lock(&self.active_model) = Some(model);
        let _busy = BusyGuard::new(&self.busy);
        let result = operation();
        if result.is_ok() {
            self.runs.fetch_add(1, Ordering::Relaxed);
        } else {
            self.failures.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    /// Non-blocking bulk-boundary state.
    pub fn snapshot(&self) -> InferenceRunnerSnapshot {
        let runs = self.runs.load(Ordering::Relaxed);
        InferenceRunnerSnapshot {
            workload: InferenceWorkload::Bulk,
            state: if self.busy.load(Ordering::Relaxed) > 0 {
                "busy"
            } else if runs == 0 {
                "cold"
            } else {
                "ready"
            }
            .to_string(),
            model: read_lock(&self.active_model)
                .as_ref()
                .map(|model| model.model_name().to_string()),
            sessions_built: runs,
            requests: runs,
            failures: self.failures.load(Ordering::Relaxed),
            exact_shapes: Vec::new(),
        }
    }
}

struct BusyGuard<'a> {
    busy: &'a AtomicUsize,
}

impl<'a> BusyGuard<'a> {
    fn new(busy: &'a AtomicUsize) -> Self {
        busy.fetch_add(1, Ordering::Relaxed);
        Self { busy }
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.busy.fetch_sub(1, Ordering::Relaxed);
    }
}

fn mutex_lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn read_lock<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write_lock<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

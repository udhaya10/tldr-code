//! Bounded infrastructure for the streaming semantic build.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;

use serde::{Deserialize, Serialize};

const DEFAULT_PIPELINE_MEMORY_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_ESTIMATED_RECORD_BYTES: usize = 32 * 1024;

/// Explicit queue/window limits derived from one non-ONNX memory budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageCapacities {
    /// Maximum paths buffered between enumeration and parsing.
    pub files: usize,
    /// Maximum planned chunks retained in one build window.
    pub chunks: usize,
    /// Maximum composed documents retained in one build window.
    pub documents: usize,
    /// Maximum tokenized inputs retained in one inference window.
    pub tokenized: usize,
    /// Maximum embedding results retained before the sink consumes them.
    pub results: usize,
}

/// Streaming build configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamingBuildConfig {
    /// Total non-ONNX payload budget shared by the bounded stages.
    pub memory_budget_bytes: usize,
    /// Conservative bytes-per-record estimate used to derive queue capacities.
    pub estimated_record_bytes: usize,
}

impl Default for StreamingBuildConfig {
    fn default() -> Self {
        let memory_budget_bytes = std::env::var("TLDR_PIPELINE_MEMORY_MB")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .and_then(|mb| mb.checked_mul(1024 * 1024))
            .filter(|bytes| *bytes > 0)
            .unwrap_or(DEFAULT_PIPELINE_MEMORY_BYTES);
        Self {
            memory_budget_bytes,
            estimated_record_bytes: DEFAULT_ESTIMATED_RECORD_BYTES,
        }
    }
}

impl StreamingBuildConfig {
    /// Derive every bounded stage capacity from the configured memory budget.
    pub fn capacities(self) -> Result<StageCapacities, BuildPipelineError> {
        if self.memory_budget_bytes == 0 || self.estimated_record_bytes == 0 {
            return Err(BuildPipelineError::new(
                PipelineStage::Configure,
                "memory and record estimates must be positive",
            ));
        }
        // Four payload-heavy stages may coexist transiently: chunks,
        // documents, token tensors, and result vectors.
        let records = self.memory_budget_bytes / self.estimated_record_bytes / 4;
        if records == 0 {
            return Err(BuildPipelineError::new(
                PipelineStage::Configure,
                "memory budget cannot hold one record in every payload stage",
            ));
        }
        let records = records.clamp(1, 2_048);
        Ok(StageCapacities {
            files: (records / 4).clamp(1, 256),
            chunks: records,
            documents: records,
            tokenized: records,
            results: records,
        })
    }
}

/// Pipeline stage carried by every typed failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStage {
    /// Pipeline configuration and capacity derivation.
    Configure,
    /// Source-file enumeration.
    Enumerate,
    /// File parsing and structural chunk planning.
    Parse,
    /// Final embedding-document composition.
    Compose,
    /// Content-addressed cache lookup or persistence.
    Cache,
    /// Token-budget accounting and fixed-shape tokenization.
    Tokenize,
    /// Model inference.
    Inference,
    /// Vector-store population.
    Sink,
    /// Cooperative cancellation observed between stages.
    Cancelled,
}

/// User-visible phase of one fresh semantic build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildPhase {
    /// Source artifacts are being enumerated and planned.
    Planning,
    /// The embedding model and tokenizer are loading.
    ModelLoad,
    /// Cache lookup and missing-vector inference are running.
    Embedding,
    /// The complete in-memory vector set is being checked.
    Verifying,
    /// The verified generation is being published atomically.
    Publishing,
    /// The generation is durable and active.
    Completed,
}

/// Reconciled progress for one semantic build.
///
/// Cache hits and new vectors describe the current scan. They are advisory
/// counters; compatible cache records remain the durable source of completed
/// inference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildProgress {
    /// Current build phase.
    pub phase: BuildPhase,
    /// Candidate files consumed so far.
    pub files_seen: u64,
    /// Exact candidate-file total when cheaply known.
    pub files_total: Option<u64>,
    /// Durable embedding windows completed by this scan.
    pub windows_completed: u64,
    /// Compatible vectors reused from the embedding cache.
    pub cache_hits: u64,
    /// Missing vectors inferred and durably cached by this scan.
    pub new_vectors: u64,
    /// Duration of the most recently completed embedding window.
    pub last_window_duration_ms: Option<u64>,
    /// Wall time since this worker build started.
    pub elapsed_ms: u64,
    /// Failed attempts consumed before the current attempt.
    pub retries: u32,
}

/// Error with the exact stage and optional file/chunk identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPipelineError {
    /// Stage that failed.
    pub stage: PipelineStage,
    /// Source file active when the failure occurred, when applicable.
    pub file: Option<PathBuf>,
    /// Stable or window-local chunk identity, when applicable.
    pub chunk: Option<String>,
    /// Human-readable underlying failure.
    pub detail: String,
}

impl BuildPipelineError {
    /// Create a stage-scoped failure.
    pub fn new(stage: PipelineStage, detail: impl Into<String>) -> Self {
        Self {
            stage,
            file: None,
            chunk: None,
            detail: detail.into(),
        }
    }

    /// Attach the source file that was active at failure time.
    pub fn at_file(mut self, file: impl Into<PathBuf>) -> Self {
        self.file = Some(file.into());
        self
    }

    /// Attach the chunk identity that was active at failure time.
    pub fn at_chunk(mut self, chunk: impl Into<String>) -> Self {
        self.chunk = Some(chunk.into());
        self
    }
}

impl std::fmt::Display for BuildPipelineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "semantic build {:?} stage", self.stage)?;
        if let Some(file) = &self.file {
            write!(formatter, " file={}", file.display())?;
        }
        if let Some(chunk) = &self.chunk {
            write!(formatter, " chunk={chunk}")?;
        }
        write!(formatter, ": {}", self.detail)
    }
}

impl std::error::Error for BuildPipelineError {}

/// Cooperative cancellation shared by producers and the build consumer.
#[derive(Debug, Clone, Default)]
pub struct BuildCancellation {
    cancelled: Arc<AtomicBool>,
}

impl BuildCancellation {
    /// Request cancellation for the producer and consumer.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Return whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Convert the current cancellation state into a typed stage result.
    pub fn check(&self) -> Result<(), BuildPipelineError> {
        if self.is_cancelled() {
            Err(BuildPipelineError::new(
                PipelineStage::Cancelled,
                "build was cancelled",
            ))
        } else {
            Ok(())
        }
    }
}

/// Build-scoped bounded-stage measurements.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineTelemetry {
    /// Derived capacity configuration used by this build.
    pub capacities: Option<StageCapacities>,
    /// Candidate source files consumed by the parser.
    pub files_seen: usize,
    /// Complete windows persisted into the build-local store.
    pub windows_completed: usize,
    /// Largest number of planned chunks held by any window.
    pub peak_window_items: usize,
    /// Largest estimated non-ONNX payload held by any window.
    pub peak_payload_bytes: usize,
    /// Number of producer sends that encountered a full file queue.
    pub producer_backpressure_events: u64,
}

impl PipelineTelemetry {
    /// Record the high-water marks for one active window.
    pub fn observe_window(&mut self, items: usize, payload_bytes: usize) {
        self.peak_window_items = self.peak_window_items.max(items);
        self.peak_payload_bytes = self.peak_payload_bytes.max(payload_bytes);
    }
}

/// Bounded deterministic file stream. A slow consumer blocks the producer.
pub(crate) struct FileProducer {
    receiver: Option<Receiver<PathBuf>>,
    handle: Option<JoinHandle<()>>,
    backpressure: Arc<AtomicU64>,
}

impl FileProducer {
    pub(crate) fn spawn(
        files: Vec<PathBuf>,
        capacity: usize,
        cancellation: BuildCancellation,
    ) -> Result<Self, BuildPipelineError> {
        if capacity == 0 {
            return Err(BuildPipelineError::new(
                PipelineStage::Configure,
                "file queue capacity must be positive",
            ));
        }
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let backpressure = Arc::new(AtomicU64::new(0));
        let thread_backpressure = Arc::clone(&backpressure);
        let handle = std::thread::Builder::new()
            .name("tldr-semantic-files".to_string())
            .spawn(move || produce_files(files, sender, cancellation, thread_backpressure))
            .map_err(|error| {
                BuildPipelineError::new(
                    PipelineStage::Enumerate,
                    format!("cannot start file producer: {error}"),
                )
            })?;
        Ok(Self {
            receiver: Some(receiver),
            handle: Some(handle),
            backpressure,
        })
    }

    pub(crate) fn recv(&self) -> Option<PathBuf> {
        self.receiver.as_ref()?.recv().ok()
    }

    pub(crate) fn backpressure_events(&self) -> u64 {
        self.backpressure.load(Ordering::Relaxed)
    }

    pub(crate) fn finish(mut self) -> Result<u64, BuildPipelineError> {
        self.receiver.take();
        if self
            .handle
            .take()
            .expect("producer handle present")
            .join()
            .is_err()
        {
            return Err(BuildPipelineError::new(
                PipelineStage::Enumerate,
                "file producer panicked",
            ));
        }
        Ok(self.backpressure_events())
    }
}

impl Drop for FileProducer {
    fn drop(&mut self) {
        self.receiver.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn produce_files(
    files: Vec<PathBuf>,
    sender: SyncSender<PathBuf>,
    cancellation: BuildCancellation,
    backpressure: Arc<AtomicU64>,
) {
    for file in files {
        if cancellation.is_cancelled() {
            break;
        }
        match sender.try_send(file) {
            Ok(()) => {}
            Err(TrySendError::Full(file)) => {
                backpressure.fetch_add(1, Ordering::Relaxed);
                if sender.send(file).is_err() {
                    break;
                }
            }
            Err(TrySendError::Disconnected(_)) => break,
        }
    }
}

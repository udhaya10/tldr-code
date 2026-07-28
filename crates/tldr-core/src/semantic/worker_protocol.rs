//! Bounded, versioned local protocol for the disposable bulk embedding worker.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{BuildOptions, BuildProgress, CacheConfig};
use crate::{TldrError, TldrResult};

/// Wire compatibility version. A mismatch fails before model loading.
pub const WORKER_PROTOCOL_VERSION: u32 = 2;
/// Embedding/chunking pipeline identity negotiated with the worker.
pub const WORKER_PIPELINE_VERSION: &str = "structural-embedding-v1";
/// Maximum request or response line accepted over local stdio.
pub const MAX_WORKER_MESSAGE_BYTES: usize = 64 * 1024;
/// Finite default number of worker attempts.
pub const DEFAULT_WORKER_ATTEMPTS: u32 = 3;

/// One path-referenced build request. Source and vector payloads never cross IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerBuildRequest {
    /// Protocol compatibility version.
    pub protocol_version: u32,
    /// Full pipeline compatibility tag.
    pub pipeline_version: String,
    /// Stable durable checkpoint identifier.
    pub job_id: String,
    /// Canonical project root.
    pub project: PathBuf,
    /// Generation output directory.
    pub store_dir: PathBuf,
    /// Complete build configuration.
    pub options: BuildOptions,
    /// Optional embedding-cache configuration.
    pub cache_config: Option<CacheConfig>,
    /// Short-lived CBOR export of source chunks from the pinned artifact
    /// generation. The worker never walks the project source tree.
    pub source_artifacts: Option<PathBuf>,
    /// Finite retry limit shared with the durable job record.
    pub max_retries: u32,
    /// Optional worker-local semantic metrics report. This is diagnostic
    /// transport metadata and never participates in semantic compatibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_output: Option<PathBuf>,
    /// Optional exact per-unit JSONL output for this worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_detail_output: Option<PathBuf>,
    /// Correlated owning run for diagnostic reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_parent_run_id: Option<String>,
}

impl WorkerBuildRequest {
    /// Construct a compatible request with finite retry defaults.
    pub fn new(
        job_id: String,
        project: PathBuf,
        store_dir: PathBuf,
        options: BuildOptions,
        cache_config: Option<CacheConfig>,
    ) -> Self {
        Self {
            protocol_version: WORKER_PROTOCOL_VERSION,
            pipeline_version: WORKER_PIPELINE_VERSION.to_string(),
            job_id,
            project,
            store_dir,
            options,
            cache_config,
            source_artifacts: None,
            max_retries: DEFAULT_WORKER_ATTEMPTS,
            metrics_output: None,
            metrics_detail_output: None,
            metrics_parent_run_id: None,
        }
    }

    /// Reject incompatible or unbounded input before any model can load.
    pub fn validate(&self) -> TldrResult<()> {
        if self.protocol_version != WORKER_PROTOCOL_VERSION {
            return Err(protocol_error(format!(
                "protocol mismatch: daemon={}, worker={WORKER_PROTOCOL_VERSION}",
                self.protocol_version
            )));
        }
        if self.pipeline_version != WORKER_PIPELINE_VERSION {
            return Err(protocol_error(format!(
                "pipeline mismatch: daemon={:?}, worker={WORKER_PIPELINE_VERSION:?}",
                self.pipeline_version
            )));
        }
        if self.job_id.is_empty() || self.max_retries == 0 {
            return Err(protocol_error("job id and retry limit must be non-zero"));
        }
        if self.options.model.dimensions() == 0 {
            return Err(protocol_error("embedding model has zero dimensions"));
        }
        if self
            .source_artifacts
            .as_ref()
            .is_none_or(|path| !path.is_file())
        {
            return Err(protocol_error(
                "shared semantic source artifact export is missing",
            ));
        }
        Ok(())
    }

    /// Stable compatibility fingerprint for durable worker metadata.
    ///
    /// The existing manifest identity owns all output-affecting semantic
    /// inputs. Process-local request fields such as the temporary artifact
    /// export, store path, retry policy, and transport details are deliberately
    /// excluded.
    pub fn compatibility_fingerprint(&self) -> TldrResult<[u8; 32]> {
        let manifest = super::store_search::manifest_id_for(&self.project, &self.options);
        let manifest = serde_json::to_vec(&manifest).map_err(protocol_error)?;
        let mut hasher = blake3::Hasher::new();
        let protocol_version = self.protocol_version.to_le_bytes();
        for field in [
            protocol_version.as_slice(),
            self.pipeline_version.as_bytes(),
            self.job_id.as_bytes(),
            manifest.as_slice(),
        ] {
            hasher.update(&(field.len() as u64).to_le_bytes());
            hasher.update(field);
        }
        Ok(*hasher.finalize().as_bytes())
    }
}

/// Acknowledgements emitted only after their represented state is durable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WorkerEvent {
    /// Compatibility passed and the durable job is running.
    Started {
        /// Attempt number, starting at one.
        attempt: u32,
    },
    /// Live reconciled progress emitted while the worker is running.
    Progress {
        /// Current phase and scan/cache counters.
        progress: BuildProgress,
    },
    /// Incompatible advisory job metadata was discarded before model load.
    Invalidated {
        /// Stable reason suitable for logs and postmortems.
        reason: String,
    },
    /// One bounded work unit and its checkpoint are durable.
    BatchCommitted {
        /// Zero-based batch number.
        batch: u64,
    },
    /// A complete generation is durable and active.
    Completed {
        /// Published vector count.
        vectors: usize,
    },
    /// A bounded failure report; the process exits non-zero afterward.
    Failed {
        /// Stable human-readable failure.
        message: String,
        /// Attempts durably consumed.
        retries: u32,
    },
}

/// Serialize one bounded newline-delimited protocol message.
pub fn encode_message<T: Serialize>(message: &T) -> TldrResult<Vec<u8>> {
    let mut bytes = serde_json::to_vec(message).map_err(protocol_error)?;
    if bytes.len() >= MAX_WORKER_MESSAGE_BYTES {
        return Err(protocol_error(format!(
            "IPC message exceeds {} bytes",
            MAX_WORKER_MESSAGE_BYTES
        )));
    }
    bytes.push(b'\n');
    Ok(bytes)
}

/// Decode one bounded newline-delimited protocol message.
pub fn decode_message<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> TldrResult<T> {
    if bytes.len() > MAX_WORKER_MESSAGE_BYTES {
        return Err(protocol_error(format!(
            "IPC message exceeds {} bytes",
            MAX_WORKER_MESSAGE_BYTES
        )));
    }
    serde_json::from_slice(bytes).map_err(protocol_error)
}

fn protocol_error(error: impl std::fmt::Display) -> TldrError {
    TldrError::Embedding(format!("bulk worker protocol: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::{ChunkGranularity, EmbeddingModel};

    fn request(root: &std::path::Path) -> WorkerBuildRequest {
        WorkerBuildRequest::new(
            "stable-source-job".into(),
            root.to_path_buf(),
            root.join("store-a"),
            BuildOptions {
                model: EmbeddingModel::ArcticXS,
                granularity: ChunkGranularity::Function,
                ..Default::default()
            },
            None,
        )
    }

    #[test]
    fn transport_and_diagnostic_fields_do_not_change_compatibility() {
        let root = tempfile::tempdir().unwrap();
        let mut left = request(root.path());
        left.source_artifacts = Some(root.path().join("export-a.cbor"));
        left.max_retries = 2;
        let mut right = left.clone();
        right.store_dir = root.path().join("store-b");
        right.source_artifacts = Some(root.path().join("export-b.cbor"));
        right.max_retries = 9;
        right.metrics_output = Some(root.path().join("metrics.json"));
        right.metrics_detail_output = Some(root.path().join("units.jsonl"));
        right.metrics_parent_run_id = Some("parent".into());

        assert_eq!(
            left.compatibility_fingerprint().unwrap(),
            right.compatibility_fingerprint().unwrap()
        );
    }

    #[test]
    fn output_affecting_identity_changes_compatibility() {
        let root = tempfile::tempdir().unwrap();
        let base = request(root.path());
        let mut different_model = base.clone();
        different_model.options.model = EmbeddingModel::ArcticL;
        let mut different_granularity = base.clone();
        different_granularity.options.granularity = ChunkGranularity::File;
        let mut different_source = base.clone();
        different_source.job_id = "different-source-job".into();

        let identity = base.compatibility_fingerprint().unwrap();
        assert_ne!(
            identity,
            different_model.compatibility_fingerprint().unwrap()
        );
        assert_ne!(
            identity,
            different_granularity.compatibility_fingerprint().unwrap()
        );
        assert_ne!(
            identity,
            different_source.compatibility_fingerprint().unwrap()
        );
    }

    #[test]
    fn protocol_frames_reject_oversized_input() {
        let oversized = vec![b'x'; MAX_WORKER_MESSAGE_BYTES + 1];
        assert!(decode_message::<WorkerEvent>(&oversized).is_err());
        let event = WorkerEvent::Invalidated {
            reason: "x".repeat(MAX_WORKER_MESSAGE_BYTES),
        };
        assert!(encode_message(&event).is_err());
    }
}

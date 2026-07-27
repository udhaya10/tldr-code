//! Bounded, versioned local protocol for the disposable bulk embedding worker.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{BuildOptions, CacheConfig};
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

    #[test]
    fn protocol_roundtrip_is_bounded_and_versioned() {
        let request = WorkerBuildRequest::new(
            "job".into(),
            "/project".into(),
            "/store".into(),
            BuildOptions::default(),
            Some(CacheConfig::default()),
        );
        let encoded = encode_message(&request).unwrap();
        let decoded: WorkerBuildRequest = decode_message(&encoded).unwrap();
        decoded.validate().unwrap();
        assert!(encoded.len() < MAX_WORKER_MESSAGE_BYTES);
    }

    #[test]
    fn mismatch_and_oversize_fail_before_execution() {
        let mut request = WorkerBuildRequest::new(
            "job".into(),
            "/project".into(),
            "/store".into(),
            BuildOptions::default(),
            None,
        );
        request.protocol_version += 1;
        assert!(request.validate().is_err());
        assert!(decode_message::<WorkerEvent>(&vec![b'x'; MAX_WORKER_MESSAGE_BYTES + 1]).is_err());
    }
}

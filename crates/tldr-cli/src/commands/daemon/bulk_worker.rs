//! Daemon-side lifecycle manager for the disposable bulk embedding process.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tldr_core::semantic::{
    decode_worker_message, encode_worker_message, BuildCancellation, BuildOptions, CacheConfig,
    CodeChunk, WorkerBuildRequest, WorkerEvent, DEFAULT_WORKER_ATTEMPTS, MAX_WORKER_MESSAGE_BYTES,
};

const DEFAULT_RSS_WATERMARK_BYTES: u64 = 768 * 1024 * 1024;

/// Observable result of one isolated build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkWorkerReport {
    /// Number of vectors in the complete published generation.
    pub vectors: usize,
    /// Child processes started, including crash/recycle retries.
    pub attempts: u32,
    /// Workers recycled after exceeding the RSS watermark.
    pub rss_recycles: u32,
    /// End-to-end orchestration latency.
    pub elapsed_ms: u64,
}

/// Optional correlated worker metrics destinations.
#[derive(Debug, Clone)]
pub struct WorkerMetricsConfig {
    /// Owning warm/build run.
    pub parent_run_id: String,
    /// Semantic metrics JSON output.
    pub report_path: PathBuf,
    /// Optional exact atomic-unit JSONL output.
    pub detail_path: Option<PathBuf>,
}

/// Finite child-process orchestration policy.
pub struct BulkWorker {
    executable: PathBuf,
    max_attempts: u32,
    rss_watermark_bytes: u64,
    poll_interval: Duration,
}

impl BulkWorker {
    /// Resolve the installed sibling worker binary.
    pub fn installed() -> Result<Self, String> {
        let executable = if let Some(path) = std::env::var_os("TLDR_EMBED_WORKER") {
            PathBuf::from(path)
        } else {
            let current = std::env::current_exe().map_err(|error| error.to_string())?;
            let suffix = std::env::consts::EXE_SUFFIX;
            let sibling = current.with_file_name(format!("tldr-embed-worker{suffix}"));
            if sibling.exists() {
                sibling
            } else if current
                .parent()
                .is_some_and(|parent| parent.file_name().is_some_and(|name| name == "deps"))
            {
                current
                    .parent()
                    .and_then(Path::parent)
                    .unwrap_or_else(|| Path::new("."))
                    .join(format!("tldr-embed-worker{suffix}"))
            } else {
                sibling
            }
        };
        Ok(Self {
            executable,
            max_attempts: DEFAULT_WORKER_ATTEMPTS,
            rss_watermark_bytes: DEFAULT_RSS_WATERMARK_BYTES,
            poll_interval: Duration::from_millis(20),
        })
    }

    /// Build or load a generation in a disposable process. The prior resident
    /// generation remains untouched until the child durably publishes.
    pub fn build(
        &self,
        project: &Path,
        store_dir: &Path,
        options: &BuildOptions,
        cache_config: Option<CacheConfig>,
        cancellation: &BuildCancellation,
        source_chunks: &[CodeChunk],
    ) -> Result<BulkWorkerReport, String> {
        self.build_with_progress(
            project,
            store_dir,
            options,
            cache_config,
            cancellation,
            source_chunks,
            |_| {},
        )
    }

    /// Build while consuming worker events before the child exits.
    pub fn build_with_progress(
        &self,
        project: &Path,
        store_dir: &Path,
        options: &BuildOptions,
        cache_config: Option<CacheConfig>,
        cancellation: &BuildCancellation,
        source_chunks: &[CodeChunk],
        on_event: impl FnMut(&WorkerEvent),
    ) -> Result<BulkWorkerReport, String> {
        self.build_with_progress_and_metrics(
            project,
            store_dir,
            options,
            cache_config,
            cancellation,
            source_chunks,
            None,
            on_event,
        )
    }

    /// Build with live progress and an optional correlated metrics report.
    #[allow(clippy::too_many_arguments)]
    pub fn build_with_progress_and_metrics(
        &self,
        project: &Path,
        store_dir: &Path,
        options: &BuildOptions,
        cache_config: Option<CacheConfig>,
        cancellation: &BuildCancellation,
        source_chunks: &[CodeChunk],
        metrics: Option<WorkerMetricsConfig>,
        mut on_event: impl FnMut(&WorkerEvent),
    ) -> Result<BulkWorkerReport, String> {
        let started = Instant::now();
        let mut source_export =
            tempfile::NamedTempFile::new().map_err(|error| error.to_string())?;
        ciborium::ser::into_writer(source_chunks, source_export.as_file_mut())
            .map_err(|error| error.to_string())?;
        source_export
            .as_file_mut()
            .flush()
            .map_err(|error| error.to_string())?;
        let mut request = WorkerBuildRequest::new(
            stable_job_id(project, options, source_chunks)?,
            project.to_path_buf(),
            store_dir.to_path_buf(),
            options.clone(),
            cache_config,
        );
        request.source_artifacts = Some(source_export.path().to_path_buf());
        request.max_retries = self.max_attempts;
        if let Some(metrics) = metrics.as_ref() {
            request.metrics_output = Some(metrics.report_path.clone());
            request.metrics_detail_output = metrics.detail_path.clone();
            request.metrics_parent_run_id = Some(metrics.parent_run_id.clone());
        }
        let payload = encode_worker_message(&request).map_err(|error| error.to_string())?;
        let mut rss_recycles = 0;
        let mut last_error = "worker did not start".to_string();

        for attempt in 1..=self.max_attempts {
            cancellation.check().map_err(|error| error.to_string())?;
            let mut child = self.spawn(&payload, &request, attempt)?;
            let (events_rx, reader) = spawn_event_reader(&mut child)?;
            let mut events = Vec::new();
            loop {
                if let Err(error) = drain_events(&events_rx, &mut events, &mut on_event) {
                    terminate(&mut child);
                    let _ = finish_events(events_rx, reader, &mut events, &mut on_event);
                    return Err(error);
                }
                if cancellation.is_cancelled() {
                    terminate(&mut child);
                    let _ = finish_events(events_rx, reader, &mut events, &mut on_event);
                    return Err("bulk embedding worker cancelled".into());
                }
                if process_rss_bytes(child.id()).is_some_and(|rss| rss > self.rss_watermark_bytes) {
                    rss_recycles += 1;
                    last_error = format!(
                        "worker exceeded RSS watermark {} bytes",
                        self.rss_watermark_bytes
                    );
                    terminate(&mut child);
                    finish_events(events_rx, reader, &mut events, &mut on_event)?;
                    break;
                }
                match child.try_wait().map_err(|error| error.to_string())? {
                    Some(status) => {
                        finish_events(events_rx, reader, &mut events, &mut on_event)?;
                        if status.success() {
                            if let Some(WorkerEvent::Completed { vectors }) = events.last() {
                                return Ok(BulkWorkerReport {
                                    vectors: *vectors,
                                    attempts: attempt,
                                    rss_recycles,
                                    elapsed_ms: started.elapsed().as_millis() as u64,
                                });
                            }
                            last_error =
                                "worker exited successfully without durable completion".into();
                        } else {
                            last_error = events
                                .iter()
                                .rev()
                                .find_map(|event| match event {
                                    WorkerEvent::Failed { message, .. } => Some(message.clone()),
                                    _ => None,
                                })
                                .unwrap_or_else(|| format!("worker exited with {status}"));
                        }
                        break;
                    }
                    None => std::thread::sleep(self.poll_interval),
                }
            }
        }
        Err(format!(
            "bulk embedding worker exhausted {} attempts: {last_error}",
            self.max_attempts
        ))
    }

    fn spawn(
        &self,
        payload: &[u8],
        request: &WorkerBuildRequest,
        attempt: u32,
    ) -> Result<Child, String> {
        let mut command = Command::new(&self.executable);
        if let Some(parent_run_id) = request.metrics_parent_run_id.as_ref() {
            command
                .env(
                    "TLDR_BUILD_RUN_ID",
                    format!("{parent_run_id}:semantic-worker:{attempt}"),
                )
                .env("TLDR_BUILD_PARENT_RUN_ID", parent_run_id)
                .env("TLDR_BUILD_PROCESS_ROLE", "semantic_worker");
        }
        if let Some(detail_path) = request.metrics_detail_output.as_ref() {
            command.env("TLDR_BUILD_METRICS_DETAIL", detail_path);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| {
                format!(
                    "cannot start bulk worker {}: {error}",
                    self.executable.display()
                )
            })?;
        child
            .stdin
            .take()
            .ok_or_else(|| "worker stdin unavailable".to_string())?
            .write_all(payload)
            .map_err(|error| error.to_string())?;
        Ok(child)
    }
}

fn stable_job_id(
    project: &Path,
    options: &BuildOptions,
    source_chunks: &[CodeChunk],
) -> Result<String, String> {
    let manifest = tldr_core::semantic::store_search::manifest_id_for(project, options);
    let manifest = serde_json::to_vec(&manifest).map_err(|error| error.to_string())?;
    let mut ordered = source_chunks.to_vec();
    ordered.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.line_start.cmp(&right.line_start))
            .then_with(|| left.content_hash.cmp(&right.content_hash))
    });
    let mut artifacts = Vec::new();
    ciborium::ser::into_writer(&ordered, &mut artifacts).map_err(|error| error.to_string())?;
    let mut hasher = blake3::Hasher::new();
    for field in [manifest.as_slice(), artifacts.as_slice()] {
        hasher.update(&(field.len() as u64).to_le_bytes());
        hasher.update(field);
    }
    Ok(format!("bulk-{}", hasher.finalize().to_hex()))
}

fn spawn_event_reader(
    child: &mut Child,
) -> Result<(Receiver<Result<WorkerEvent, String>>, JoinHandle<()>), String> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "worker stdout unavailable".to_string())?;
    let (sender, receiver) = mpsc::sync_channel(64);
    let handle = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = Vec::new();
            match (&mut reader)
                .take(MAX_WORKER_MESSAGE_BYTES as u64 + 1)
                .read_until(b'\n', &mut line)
            {
                Ok(0) => break,
                Ok(_) => {
                    if line.last() == Some(&b'\n') {
                        line.pop();
                    }
                    let decoded = decode_event_line(&line);
                    let stop = decoded.is_err();
                    if sender.send(decoded).is_err() {
                        break;
                    }
                    if stop {
                        break;
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(error.to_string()));
                    break;
                }
            }
        }
    });
    Ok((receiver, handle))
}

fn decode_event_line(line: &[u8]) -> Result<WorkerEvent, String> {
    if line.len() > MAX_WORKER_MESSAGE_BYTES {
        return Err(format!(
            "worker event exceeds {} bytes",
            MAX_WORKER_MESSAGE_BYTES
        ));
    }
    decode_worker_message(line).map_err(|error| error.to_string())
}

fn drain_events(
    receiver: &Receiver<Result<WorkerEvent, String>>,
    events: &mut Vec<WorkerEvent>,
    on_event: &mut impl FnMut(&WorkerEvent),
) -> Result<(), String> {
    for event in receiver.try_iter() {
        let event = event?;
        on_event(&event);
        events.push(event);
    }
    Ok(())
}

fn finish_events(
    receiver: Receiver<Result<WorkerEvent, String>>,
    reader: JoinHandle<()>,
    events: &mut Vec<WorkerEvent>,
    on_event: &mut impl FnMut(&WorkerEvent),
) -> Result<(), String> {
    for event in receiver {
        let event = event?;
        on_event(&event);
        events.push(event);
    }
    reader
        .join()
        .map_err(|_| "worker event reader panicked".to_string())
}

fn terminate(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(target_os = "linux")]
fn process_rss_bytes(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status.lines().find_map(|line| {
        line.strip_prefix("VmRSS:")
            .and_then(|value| value.split_whitespace().next())
            .and_then(|kb| kb.parse::<u64>().ok())
            .and_then(|kb| kb.checked_mul(1024))
    })
}

#[cfg(not(target_os = "linux"))]
fn process_rss_bytes(_pid: u32) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tldr_core::semantic::{ChunkStructure, EmbeddingModel};
    use tldr_core::Language;

    fn chunk(root: &Path, content: &str) -> CodeChunk {
        CodeChunk {
            file_path: root.join("src/lib.rs"),
            function_name: Some("work".into()),
            class_name: None,
            line_start: 1,
            line_end: 1,
            content: content.into(),
            content_hash: format!("{:x}", md5::compute(content)),
            language: Language::Rust,
            structure: ChunkStructure::default(),
        }
    }

    #[test]
    fn job_identity_tracks_pinned_source_and_manifest() {
        let root = tempfile::tempdir().unwrap();
        let options = BuildOptions {
            model: EmbeddingModel::ArcticXS,
            ..Default::default()
        };
        let first =
            stable_job_id(root.path(), &options, &[chunk(root.path(), "fn work() {}")]).unwrap();
        let repeat =
            stable_job_id(root.path(), &options, &[chunk(root.path(), "fn work() {}")]).unwrap();
        let mut second = chunk(root.path(), "fn second() {}");
        second.file_path = root.path().join("src/second.rs");
        let ordered = stable_job_id(
            root.path(),
            &options,
            &[chunk(root.path(), "fn work() {}"), second.clone()],
        )
        .unwrap();
        let reversed = stable_job_id(
            root.path(),
            &options,
            &[second, chunk(root.path(), "fn work() {}")],
        )
        .unwrap();
        let changed = stable_job_id(
            root.path(),
            &options,
            &[chunk(root.path(), "fn work() { todo!() }")],
        )
        .unwrap();
        let mut other_model = options;
        other_model.model = EmbeddingModel::ArcticL;
        let changed_model = stable_job_id(
            root.path(),
            &other_model,
            &[chunk(root.path(), "fn work() {}")],
        )
        .unwrap();

        assert_eq!(first, repeat);
        assert_eq!(ordered, reversed);
        assert_ne!(first, changed);
        assert_ne!(first, changed_model);
    }

    #[test]
    fn malformed_and_oversized_events_are_rejected() {
        assert!(decode_event_line(b"not-json").is_err());
        assert!(decode_event_line(&vec![b'x'; MAX_WORKER_MESSAGE_BYTES + 1]).is_err());
    }
}

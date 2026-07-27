//! Daemon-side lifecycle manager for the disposable bulk embedding process.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tldr_core::semantic::vector_store::compute_corpus_digest;
use tldr_core::semantic::{
    decode_worker_message, encode_worker_message, BuildCancellation, BuildOptions, CacheConfig,
    WorkerBuildRequest, WorkerEvent, DEFAULT_WORKER_ATTEMPTS, MAX_WORKER_MESSAGE_BYTES,
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

    #[cfg(test)]
    fn for_test(executable: PathBuf, max_attempts: u32, rss_watermark_bytes: u64) -> Self {
        Self {
            executable,
            max_attempts,
            rss_watermark_bytes,
            poll_interval: Duration::from_millis(5),
        }
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
    ) -> Result<BulkWorkerReport, String> {
        let started = Instant::now();
        let mut request = WorkerBuildRequest::new(
            stable_job_id(project, options),
            project.to_path_buf(),
            store_dir.to_path_buf(),
            options.clone(),
            cache_config,
        );
        request.max_retries = self.max_attempts;
        let payload = encode_worker_message(&request).map_err(|error| error.to_string())?;
        let mut rss_recycles = 0;
        let mut last_error = "worker did not start".to_string();

        for attempt in 1..=self.max_attempts {
            cancellation.check().map_err(|error| error.to_string())?;
            let mut child = self.spawn(&payload)?;
            loop {
                if cancellation.is_cancelled() {
                    terminate(&mut child);
                    return Err("bulk embedding worker cancelled".into());
                }
                if process_rss_bytes(child.id()).is_some_and(|rss| rss > self.rss_watermark_bytes) {
                    rss_recycles += 1;
                    last_error = format!(
                        "worker exceeded RSS watermark {} bytes",
                        self.rss_watermark_bytes
                    );
                    terminate(&mut child);
                    break;
                }
                match child.try_wait().map_err(|error| error.to_string())? {
                    Some(status) => {
                        let events = read_events(&mut child)?;
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

    fn spawn(&self, payload: &[u8]) -> Result<Child, String> {
        let mut child = Command::new(&self.executable)
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

fn stable_job_id(project: &Path, options: &BuildOptions) -> String {
    let digest = compute_corpus_digest(project);
    let identity = format!(
        "{}:{digest}:{}:{:?}:{:?}",
        project.display(),
        options.model.model_name(),
        options.granularity,
        options.languages
    );
    format!("bulk-{:x}", md5::compute(identity))
}

fn read_events(child: &mut Child) -> Result<Vec<WorkerEvent>, String> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "worker stdout unavailable".to_string())?;
    let mut bytes = Vec::new();
    stdout
        .take((MAX_WORKER_MESSAGE_BYTES * 4) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| decode_worker_message(line).map_err(|error| error.to_string()))
        .collect()
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(body: &str) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("worker");
        std::fs::write(&path, format!("#!/bin/sh\nread request\n{body}\n")).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        (directory, path)
    }

    #[test]
    fn bounded_protocol_completion_is_observable() {
        let (_directory, executable) =
            script("printf '%s\\n' '{\"event\":\"started\",\"attempt\":1}' '{\"event\":\"completed\",\"vectors\":7}'");
        let worker = BulkWorker::for_test(executable, 1, u64::MAX);
        let project = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let report = worker
            .build(
                project.path(),
                store.path(),
                &BuildOptions::default(),
                None,
                &BuildCancellation::default(),
            )
            .unwrap();
        assert_eq!(report.vectors, 7);
        assert_eq!(report.attempts, 1);
        // Local protocol/process overhead stays negligible relative to a cold
        // model build; this fixture performs no model work.
        assert!(report.elapsed_ms < 2_000);
    }

    #[test]
    fn crashes_retry_finitely_and_cancellation_stays_responsive() {
        let (_directory, executable) = script("exit 9");
        let worker = BulkWorker::for_test(executable, 2, u64::MAX);
        let project = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let cancellation = BuildCancellation::default();
        let error = worker
            .build(
                project.path(),
                store.path(),
                &BuildOptions::default(),
                None,
                &cancellation,
            )
            .unwrap_err();
        assert!(error.contains("exhausted 2 attempts"));

        cancellation.cancel();
        let started = Instant::now();
        assert!(worker
            .build(
                project.path(),
                store.path(),
                &BuildOptions::default(),
                None,
                &cancellation,
            )
            .is_err());
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rss_recycling_kills_child_and_reclaims_process_memory() {
        let (_directory, executable) = script("sleep 2");
        let worker = BulkWorker::for_test(executable, 1, 1);
        let project = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let result = worker.build(
            project.path(),
            store.path(),
            &BuildOptions::default(),
            None,
            &BuildCancellation::default(),
        );
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}

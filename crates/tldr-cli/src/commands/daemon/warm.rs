//! Warm command implementation
//!
//! CLI command: `tldr warm PATH [--background] [--lang LANG]`
//!
//! Pre-builds call graph cache for faster subsequent queries.
//!
//! # Behavior
//!
//! 1. If `--background`: spawn detached process, return immediately
//! 2. Foreground mode: build call graph synchronously
//! 3. If daemon is running: send Warm command via IPC
//! 4. If daemon not running and background: start daemon then warm
//!
//! # Output
//!
//! JSON format:
//! ```json
//! {
//!   "status": "ok",
//!   "files": 150,
//!   "edges": 2500,
//!   "languages": ["python", "typescript"],
//!   "cache_path": ".tldr/store/project.redb"
//! }
//! ```

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::output::OutputFormat;
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use tldr_core::artifact_store::{IngestionReport, IngestionTimingOptions};

use super::error::{DaemonError, DaemonResult};
use super::ipc::{check_socket_alive, send_command};
use super::types::{DaemonCommand, DaemonResponse};

// =============================================================================
// CLI Arguments
// =============================================================================

/// Arguments for the `warm` command.
#[derive(Debug, Clone, Args)]
pub struct WarmArgs {
    /// Project root directory to warm
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Run warming in background process
    #[arg(long, short = 'b')]
    pub background: bool,

    /// Write one correlated artifact + semantic build report as JSON.
    #[arg(long)]
    pub metrics: Option<PathBuf>,

    /// Stream exact atomic-unit timings beside the report.
    #[arg(long, value_enum, requires = "metrics")]
    pub metrics_detail: Option<MetricsDetail>,
    // Note: Use global --lang to specify language, or auto-detect if not specified
}

/// Optional detail level for build timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MetricsDetail {
    /// Stream every measured atomic unit to JSONL.
    Units,
}

// =============================================================================
// Output Types
// =============================================================================

/// Output structure for successful warm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarmOutput {
    /// Status message
    pub status: String,
    /// Number of files indexed
    pub files: usize,
    /// Number of call graph edges
    pub edges: usize,
    /// Languages detected/analyzed
    pub languages: Vec<String>,
    /// Path to the cache file
    pub cache_path: PathBuf,
}

/// One correlated report for the full warm workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarmMetricsReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Shared build correlation identifier.
    pub run_id: String,
    /// Process role coordinating the component reports.
    pub orchestrator_role: String,
    /// Project being built.
    pub project: PathBuf,
    /// Run start, unix epoch milliseconds.
    pub started_at_unix_ms: u64,
    /// End-to-end warm wall time.
    pub duration_ms: u64,
    /// Structural ingestion and AST timing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<IngestionReport>,
    /// Semantic worker report, when the semantic feature/build was available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic: Option<serde_json::Value>,
    /// Exact unit stream, when requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_detail_path: Option<PathBuf>,
    /// Component failures retained in an incomplete report.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    /// Clarifies why summed concurrent unit work may exceed wall time.
    pub concurrency_note: String,
}

// =============================================================================
// Implementation
// =============================================================================

impl WarmArgs {
    /// Run the warm command.
    pub fn run(&self, format: OutputFormat, quiet: bool) -> anyhow::Result<()> {
        // Create a new tokio runtime for the async operations
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(self.run_async(format, quiet))
    }

    /// Async implementation of the warm command.
    async fn run_async(&self, format: OutputFormat, quiet: bool) -> anyhow::Result<()> {
        // Resolve project path to absolute
        let project = self.path.canonicalize().unwrap_or_else(|_| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(&self.path)
        });

        if self.background {
            // Run in background
            self.run_background(&project, format, quiet).await
        } else {
            // Check if daemon is running - if so, send command via IPC
            if check_socket_alive(&project).await {
                self.run_via_daemon(&project, format, quiet).await
            } else {
                // Run synchronously in foreground
                self.run_foreground(&project, format, quiet).await
            }
        }
    }

    /// Run warming in background (spawn detached process).
    async fn run_background(
        &self,
        project: &Path,
        format: OutputFormat,
        quiet: bool,
    ) -> anyhow::Result<()> {
        // Spawn detached process
        let exe = std::env::current_exe()?;
        let mut cmd = StdCommand::new(exe);
        cmd.arg("warm").arg(project.to_str().unwrap_or("."));
        if let Some(metrics) = self.metrics.as_ref() {
            cmd.arg("--metrics").arg(absolute_output(metrics)?);
        }
        if let Some(detail) = self.metrics_detail {
            cmd.arg("--metrics-detail").arg(match detail {
                MetricsDetail::Units => "units",
            });
        }

        // Language auto-detection happens in the background process

        // On Unix, we use setsid to detach
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        // On Windows, use CREATE_NO_WINDOW and DETACHED_PROCESS
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            const DETACHED_PROCESS: u32 = 0x00000008;
            cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
        }

        cmd.spawn()?;

        // Output background message
        if !quiet {
            match format {
                OutputFormat::Json | OutputFormat::Compact => {
                    let output = serde_json::json!({
                        "status": "ok",
                        "message": "Warming cache in background..."
                    });
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                OutputFormat::Text | OutputFormat::Sarif | OutputFormat::Dot => {
                    println!("Warming cache in background...");
                }
            }
        }

        Ok(())
    }

    /// Run warming via IPC to running daemon.
    async fn run_via_daemon(
        &self,
        project: &Path,
        format: OutputFormat,
        quiet: bool,
    ) -> anyhow::Result<()> {
        let metrics_path = self
            .metrics
            .as_ref()
            .map(|path| absolute_output(path))
            .transpose()?;
        let metrics_detail_path = metrics_path
            .as_ref()
            .filter(|_| self.metrics_detail == Some(MetricsDetail::Units))
            .map(|path| unit_detail_path(path));
        let cmd = DaemonCommand::Warm {
            language: None, // Auto-detect
            metrics_path,
            metrics_detail_path,
            run_id: self.metrics.as_ref().map(|_| new_run_id()),
        };

        let response = send_command(project, &cmd)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send warm command to daemon: {}", e))?;

        if !quiet {
            match format {
                OutputFormat::Json | OutputFormat::Compact => {
                    println!("{}", serde_json::to_string_pretty(&response)?);
                }
                OutputFormat::Text | OutputFormat::Sarif | OutputFormat::Dot => {
                    // TLDR-utj.7: the daemon acks immediately ("started" /
                    // "already_building") and builds in the background —
                    // relay its message (which points at `tldr daemon
                    // status`) instead of implying the warm completed.
                    match &response {
                        DaemonResponse::Status {
                            message: Some(msg), ..
                        } => println!("{}", msg),
                        _ => println!("Warm command sent to daemon"),
                    }
                }
            }
        }

        Ok(())
    }

    /// Run warming synchronously in foreground.
    async fn run_foreground(
        &self,
        project: &Path,
        format: OutputFormat,
        quiet: bool,
    ) -> anyhow::Result<()> {
        if !quiet {
            match format {
                OutputFormat::Text | OutputFormat::Sarif | OutputFormat::Dot => {
                    println!("Building shared artifact generation...");
                }
                _ => {}
            }
        }

        let manager = super::artifact_manager::ArtifactManager::open(project)?;
        let metrics_started = Instant::now();
        let metrics_started_at_unix_ms = unix_millis();
        let run_id = self.metrics.as_ref().map(|_| new_run_id());
        let metrics_path = self
            .metrics
            .as_ref()
            .map(|path| absolute_output(path))
            .transpose()?;
        let detail_path = metrics_path
            .as_ref()
            .filter(|_| self.metrics_detail == Some(MetricsDetail::Units))
            .map(|path| unit_detail_path(path));
        if let Some(path) = detail_path.as_ref() {
            std::fs::File::create(path)?;
        }
        let report = if let Some(run_id) = run_id.as_ref() {
            manager.warm_with_timing(IngestionTimingOptions {
                run_id: run_id.clone(),
                process_role: "artifact_producer".into(),
                detail_path: detail_path.clone(),
            })?
        } else {
            manager.warm()?
        };
        let snapshot = manager
            .snapshot()
            .map_err(|state| anyhow::anyhow!("artifact generation is not ready: {state:?}"))?;
        let mut languages = snapshot
            .files()
            .map(|facts| facts.language.clone())
            .collect::<Vec<_>>();
        languages.sort();
        languages.dedup();
        let files = snapshot.file_count();
        let edges = snapshot.intra_file_call_graph().edge_count();

        #[cfg(feature = "semantic")]
        let (semantic_metrics, metrics_errors) =
            if let (Some(run_id), Some(metrics_path)) = (run_id.as_ref(), metrics_path.as_ref()) {
                use tldr_core::config::TldrConfig;
                use tldr_core::semantic::EmbeddingModel;

                let config = TldrConfig::resolve(Some(project));
                let model = EmbeddingModel::resolve(None, &config).map_err(anyhow::Error::msg)?;
                let source_chunks = snapshot.semantic_source_chunks(project);
                let parent = metrics_path.parent().unwrap_or_else(|| Path::new("."));
                let semantic_file = tempfile::NamedTempFile::new_in(parent)?;
                let semantic_path = semantic_file.path().to_path_buf();
                let index = super::index_manager::IndexManager::new();
                let result = index
                    .warm_with_metrics(
                        project,
                        model,
                        source_chunks,
                        Some(super::bulk_worker::WorkerMetricsConfig {
                            parent_run_id: run_id.clone(),
                            report_path: semantic_path,
                            detail_path: detail_path.clone(),
                        }),
                    )
                    .map_err(anyhow::Error::msg)?;
                (
                    result.metrics.map(serde_json::to_value).transpose()?,
                    result
                        .metrics_error
                        .map(|error| vec![format!("semantic_metrics: {error}")])
                        .unwrap_or_default(),
                )
            } else {
                (None, Vec::new())
            };
        #[cfg(not(feature = "semantic"))]
        let (semantic_metrics, metrics_errors): (Option<serde_json::Value>, Vec<String>) =
            (None, Vec::new());

        if let (Some(metrics_path), Some(run_id)) = (metrics_path.as_ref(), run_id) {
            write_metrics_report(
                metrics_path,
                &WarmMetricsReport {
                    schema_version: 1,
                    run_id,
                    orchestrator_role: "foreground".into(),
                    project: project.to_path_buf(),
                    started_at_unix_ms: metrics_started_at_unix_ms,
                    duration_ms: metrics_started.elapsed().as_millis() as u64,
                    artifact: Some(report.clone()),
                    semantic: semantic_metrics,
                    unit_detail_path: detail_path,
                    errors: metrics_errors,
                    concurrency_note: "phase durations are wall time; atomic-unit totals are summed work and may exceed wall time when units run concurrently".into(),
                },
            )?;
        }

        // Output result
        let output = WarmOutput {
            status: "ok".to_string(),
            files,
            edges,
            languages,
            cache_path: PathBuf::from(".tldr/store/project.redb"),
        };

        // Always output result (quiet only suppresses progress messages)
        match format {
            OutputFormat::Json | OutputFormat::Compact => {
                println!("{}", serde_json::to_string_pretty(&output)?);
            }
            OutputFormat::Text | OutputFormat::Sarif | OutputFormat::Dot => {
                println!(
                    "Indexed {} files, found {} edges",
                    output.files, output.edges
                );
                println!("Languages: {}", output.languages.join(", "));
                println!(
                    "Generation {} published to: {}{}",
                    report.generation,
                    output.cache_path.display(),
                    if report.resumed { " (resumed)" } else { "" }
                );
            }
        }

        Ok(())
    }
}

pub(super) fn write_metrics_report(path: &Path, report: &WarmMetricsReport) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), report)?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn absolute_output(path: &Path) -> anyhow::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

pub(super) fn unit_detail_path(report: &Path) -> PathBuf {
    let file_name = report
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("tldr-build");
    report.with_file_name(format!("{file_name}.units.jsonl"))
}

pub(super) fn new_run_id() -> String {
    format!("{}-{}", unix_millis(), std::process::id())
}

pub(super) fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Public function to run warm command (for daemon integration).
pub async fn cmd_warm(args: WarmArgs) -> DaemonResult<WarmOutput> {
    // Resolve project path
    let project = args.path.canonicalize().unwrap_or_else(|_| {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(&args.path)
    });

    let manager = super::artifact_manager::ArtifactManager::open(&project)
        .map_err(|error| DaemonError::Io(std::io::Error::other(error.to_string())))?;
    manager
        .warm()
        .map_err(|error| DaemonError::Io(std::io::Error::other(error.to_string())))?;
    let snapshot = manager.snapshot().map_err(|state| {
        DaemonError::Io(std::io::Error::other(format!(
            "artifact generation is not ready: {state:?}"
        )))
    })?;
    let mut languages = snapshot
        .files()
        .map(|facts| facts.language.clone())
        .collect::<Vec<_>>();
    languages.sort();
    languages.dedup();
    let files = snapshot.file_count();
    let edges = snapshot.intra_file_call_graph().edge_count();

    Ok(WarmOutput {
        status: "ok".to_string(),
        files,
        edges,
        languages,
        cache_path: PathBuf::from(".tldr/store/project.redb"),
    })
}

// =============================================================================
// Tests
// =============================================================================

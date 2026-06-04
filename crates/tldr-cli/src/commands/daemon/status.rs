//! Daemon status command implementation
//!
//! CLI command: `tldr daemon status [--project PATH] [--session SESSION_ID]`
//!
//! This module provides status information about a running daemon:
//! - Current status (initializing, indexing, ready, shutting_down)
//! - Uptime
//! - Number of indexed files
//! - Cache statistics (hits, misses, hit rate, invalidations)
//! - Session statistics (if requested)
//! - Hook activity statistics

use std::path::{Path, PathBuf};

use clap::Args;
use serde::Serialize;

use crate::output::OutputFormat;

use super::daemon_active::read_active;
use super::daemon_registry::live_entries;
use super::error::DaemonError;
use super::ipc::send_command;
use super::types::{
    DaemonCommand, DaemonResponse, DaemonStatus, LivenessStats, MemoryStats, SalsaCacheStats,
    SemanticIndexStats,
};

// =============================================================================
// CLI Arguments
// =============================================================================

/// Arguments for the `daemon status` command.
#[derive(Debug, Clone, Args)]
pub struct DaemonStatusArgs {
    /// Project root directory (default: current directory).
    ///
    /// When omitted, falls back to the active daemon's project path
    /// recorded by `daemon start`, allowing `tldr daemon status` to
    /// report the running daemon's status from any working directory.
    #[arg(long, short = 'p', default_value = ".")]
    pub project: PathBuf,

    /// Session ID to get session-specific stats
    #[arg(long, short = 's')]
    pub session: Option<String>,
}

// =============================================================================
// Output Types
// =============================================================================

/// Output structure for daemon status when running.
#[derive(Debug, Clone, Serialize)]
pub struct DaemonStatusOutput {
    /// Current status
    pub status: String,
    /// Uptime in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime: Option<f64>,
    /// Human-readable uptime
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_human: Option<String>,
    /// Number of indexed files
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<usize>,
    /// Project path
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<PathBuf>,
    /// Cache statistics
    #[serde(skip_serializing_if = "Option::is_none")]
    pub salsa_stats: Option<SalsaCacheStats>,
    /// Liveness observability: per-source presence ages, busy work with age,
    /// idle deadline (TLDR-qzc).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub liveness: Option<LivenessStats>,
    /// Resident semantic index state: warm/building/cold (TLDR-qzc).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_index: Option<SemanticIndexStats>,
    /// Daemon process memory: current + peak RSS (TLDR-yll).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryStats>,
    /// Optional message
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// =============================================================================
// Command Implementation
// =============================================================================

impl DaemonStatusArgs {
    /// Run the daemon status command.
    pub fn run(&self, format: OutputFormat, quiet: bool) -> anyhow::Result<()> {
        // Create a new tokio runtime for the async operations
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(self.run_async(format, quiet))
    }

    /// Async implementation of the daemon status command.
    async fn run_async(&self, format: OutputFormat, quiet: bool) -> anyhow::Result<()> {
        // VAL-003 (v0.3.0): when `--project` is the default (the literal
        // ".", meaning the user did not pass an explicit path), dispatch on
        // the live multi-daemon registry:
        //   - 0 entries: fall back to the legacy daemon-active.json record
        //     (covers the migration window) and ultimately the cwd path.
        //   - 1 entry:   use that entry's project path (preserves VAL-013).
        //   - 2+ entries: ERROR with a hint to pass `--project` or list.
        //
        // An explicit `--project` (anything other than ".") is ALWAYS
        // honoured — the workaround path is preserved.
        let project = if self.project == Path::new(".") {
            let entries = live_entries();
            match entries.len() {
                0 => match read_active() {
                    Some(active) => active.project,
                    None => self.project.canonicalize().unwrap_or_else(|_| {
                        std::env::current_dir()
                            .unwrap_or_else(|_| PathBuf::from("."))
                            .join(&self.project)
                    }),
                },
                1 => entries.into_iter().next().unwrap().project,
                n => {
                    return Err(anyhow::anyhow!(
                        "multiple daemons running ({}); use --project <abs-path> or run 'tldr daemon list'",
                        n
                    ));
                }
            }
        } else {
            self.project.canonicalize().unwrap_or_else(|_| {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(&self.project)
            })
        };

        // Send status command
        let cmd = DaemonCommand::Status {
            session: self.session.clone(),
        };

        match send_command(&project, &cmd).await {
            Ok(response) => self.handle_response(response, format, quiet),
            Err(DaemonError::NotRunning) | Err(DaemonError::ConnectionRefused) => {
                // Daemon not running
                let output = DaemonStatusOutput {
                    status: "not_running".to_string(),
                    uptime: None,
                    uptime_human: None,
                    files: None,
                    project: None,
                    salsa_stats: None,
                    liveness: None,
                    semantic_index: None,
                    memory: None,
                    message: Some("Daemon not running".to_string()),
                };

                if !quiet {
                    match format {
                        OutputFormat::Json | OutputFormat::Compact => {
                            println!("{}", serde_json::to_string_pretty(&output)?);
                        }
                        OutputFormat::Text | OutputFormat::Sarif | OutputFormat::Dot => {
                            println!("Daemon not running");
                        }
                    }
                }

                Ok(())
            }
            Err(e) => Err(anyhow::anyhow!("Failed to get daemon status: {}", e)),
        }
    }

    /// Handle the daemon response.
    fn handle_response(
        &self,
        response: DaemonResponse,
        format: OutputFormat,
        quiet: bool,
    ) -> anyhow::Result<()> {
        match response {
            DaemonResponse::FullStatus {
                status,
                uptime,
                files,
                project,
                salsa_stats,
                liveness,
                semantic_index,
                memory,
                ..
            } => {
                let status_str = format_status(status);
                let uptime_human = format_uptime(uptime);

                let output = DaemonStatusOutput {
                    status: status_str.clone(),
                    uptime: Some(uptime),
                    uptime_human: Some(uptime_human.clone()),
                    files: Some(files),
                    project: Some(project.clone()),
                    salsa_stats: Some(salsa_stats.clone()),
                    liveness: liveness.clone(),
                    semantic_index: semantic_index.clone(),
                    memory: memory.clone(),
                    message: None,
                };

                if !quiet {
                    match format {
                        OutputFormat::Json | OutputFormat::Compact => {
                            println!("{}", serde_json::to_string_pretty(&output)?);
                        }
                        OutputFormat::Text | OutputFormat::Sarif | OutputFormat::Dot => {
                            println!("TLDR Daemon Status");
                            println!("==================");
                            println!("Status:  {}", status_str);
                            println!("Uptime:  {}", uptime_human);
                            println!("Project: {}", project.display());
                            println!("Files:   {}", files);
                            if let Some(idx) = &semantic_index {
                                println!("Index:   {}", format_semantic_index(idx));
                            }
                            if let Some(mem) = &memory {
                                if let Some(line) = format_memory(mem) {
                                    println!("Memory:  {}", line);
                                }
                            }
                            if let Some(live) = &liveness {
                                println!();
                                print_liveness(live);
                            }
                            println!();
                            println!("Cache Statistics");
                            println!("----------------");
                            println!("Hits:          {}", format_number(salsa_stats.hits));
                            println!("Misses:        {}", format_number(salsa_stats.misses));
                            println!("Hit Rate:      {:.2}%", salsa_stats.hit_rate());
                            println!(
                                "Invalidations: {}",
                                format_number(salsa_stats.invalidations)
                            );
                        }
                    }
                }

                Ok(())
            }
            DaemonResponse::Status { status, message } => {
                let output = DaemonStatusOutput {
                    status: status.clone(),
                    uptime: None,
                    uptime_human: None,
                    files: None,
                    project: None,
                    salsa_stats: None,
                    liveness: None,
                    semantic_index: None,
                    memory: None,
                    message,
                };

                if !quiet {
                    match format {
                        OutputFormat::Json | OutputFormat::Compact => {
                            println!("{}", serde_json::to_string_pretty(&output)?);
                        }
                        OutputFormat::Text | OutputFormat::Sarif | OutputFormat::Dot => {
                            println!("Status: {}", status);
                            if let Some(msg) = &output.message {
                                println!("{}", msg);
                            }
                        }
                    }
                }

                Ok(())
            }
            DaemonResponse::Error { error, .. } => Err(anyhow::anyhow!("Daemon error: {}", error)),
            _ => Err(anyhow::anyhow!("Unexpected response from daemon")),
        }
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Format DaemonStatus as a string.
fn format_status(status: DaemonStatus) -> String {
    match status {
        DaemonStatus::Initializing => "initializing".to_string(),
        DaemonStatus::Indexing => "indexing".to_string(),
        DaemonStatus::Ready => "running".to_string(),
        DaemonStatus::ShuttingDown => "shutting_down".to_string(),
        DaemonStatus::Stopped => "stopped".to_string(),
    }
}

/// Format uptime seconds as human-readable string.
fn format_uptime(secs: f64) -> String {
    let total_secs = secs as u64;
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    format!("{}h {}m {}s", hours, minutes, seconds)
}

/// Human bytes: `1.2 GB`, `850.0 MB`. Round numbers beat precision here —
/// the point is spotting a 22.7 GB daemon at a glance (TLDR-yll).
fn format_bytes(bytes: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else {
        format!("{:.1} MB", b / MB)
    }
}

/// One-line memory readout: `1.2 GB (peak 22.7 GB)`. `None` when neither
/// figure is readable on this platform.
fn format_memory(mem: &MemoryStats) -> Option<String> {
    match (mem.rss_bytes, mem.peak_rss_bytes) {
        (Some(rss), Some(peak)) => {
            Some(format!("{} (peak {})", format_bytes(rss), format_bytes(peak)))
        }
        (Some(rss), None) => Some(format_bytes(rss)),
        (None, Some(peak)) => Some(format!("peak {}", format_bytes(peak))),
        (None, None) => None,
    }
}

/// One-line semantic index state: `warm (12,345 vectors)` / `building` /
/// `cold (run 'tldr warm')`.
fn format_semantic_index(idx: &SemanticIndexStats) -> String {
    match (idx.state.as_str(), idx.vectors) {
        ("warm", Some(n)) => format!("warm ({} vectors)", format_number(n as u64)),
        ("cold", _) => "cold (run 'tldr warm')".to_string(),
        (state, _) => state.to_string(),
    }
}

/// Render the liveness block (TLDR-qzc): what kept the daemon alive, what
/// internal work is in flight (with age — a hung build shows up here as an
/// ever-growing `busy`), and when idle shutdown would fire.
fn print_liveness(live: &LivenessStats) {
    println!("Liveness");
    println!("--------");
    for (source, age) in &live.presence_age_secs {
        println!("{:<13}{} ago", format!("{}:", source), format_uptime(*age));
    }
    for token in &live.busy {
        println!(
            "{:<13}{} (running {})",
            "busy:", token.label,
            format_uptime(token.age_secs)
        );
    }
    match live.idle_shutdown_in_secs {
        Some(secs) => println!(
            "{:<13}in {} (timeout {})",
            "idle stop:",
            format_uptime(secs),
            format_uptime(live.idle_timeout_secs as f64)
        ),
        None => println!("{:<13}deferred (internal work in flight)", "idle stop:"),
    }
}

/// Format a number with thousands separators.
fn format_number(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut result = String::new();
    let len = bytes.len();

    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(b as char);
    }

    result
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::super::types::BusyTokenStats;
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_daemon_status_args_default() {
        let args = DaemonStatusArgs {
            project: PathBuf::from("."),
            session: None,
        };

        assert_eq!(args.project, PathBuf::from("."));
        assert!(args.session.is_none());
    }

    #[test]
    fn test_daemon_status_args_with_session() {
        let args = DaemonStatusArgs {
            project: PathBuf::from("/test/project"),
            session: Some("test-session".to_string()),
        };

        assert_eq!(args.session, Some("test-session".to_string()));
    }

    #[test]
    fn test_format_status() {
        assert_eq!(format_status(DaemonStatus::Ready), "running");
        assert_eq!(format_status(DaemonStatus::Initializing), "initializing");
        assert_eq!(format_status(DaemonStatus::Indexing), "indexing");
        assert_eq!(format_status(DaemonStatus::ShuttingDown), "shutting_down");
        assert_eq!(format_status(DaemonStatus::Stopped), "stopped");
    }

    #[test]
    fn test_format_uptime() {
        assert_eq!(format_uptime(0.0), "0h 0m 0s");
        assert_eq!(format_uptime(61.0), "0h 1m 1s");
        assert_eq!(format_uptime(3661.0), "1h 1m 1s");
        assert_eq!(format_uptime(7200.0), "2h 0m 0s");
    }

    #[test]
    fn test_format_number() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(999), "999");
        assert_eq!(format_number(1000), "1,000");
        assert_eq!(format_number(1234567), "1,234,567");
    }

    #[test]
    fn test_daemon_status_output_serialization() {
        let output = DaemonStatusOutput {
            status: "running".to_string(),
            uptime: Some(3600.0),
            uptime_human: Some("1h 0m 0s".to_string()),
            files: Some(100),
            project: Some(PathBuf::from("/test/project")),
            salsa_stats: Some(SalsaCacheStats {
                hits: 90,
                misses: 10,
                invalidations: 5,
                recomputations: 3,
            }),
            liveness: Some(LivenessStats {
                presence_age_secs: [("socket".to_string(), 12.5)].into_iter().collect(),
                busy: vec![BusyTokenStats {
                    label: "warm-build".to_string(),
                    age_secs: 900.0,
                }],
                idle_timeout_secs: 1800,
                idle_shutdown_in_secs: None,
            }),
            semantic_index: Some(SemanticIndexStats {
                state: "building".to_string(),
                vectors: None,
            }),
            memory: Some(MemoryStats {
                rss_bytes: Some(1024 * 1024 * 1024),
                peak_rss_bytes: Some(22 * 1024 * 1024 * 1024),
            }),
            message: None,
        };

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("running"));
        assert!(json.contains("3600"));
        assert!(json.contains("hits"));
        assert!(json.contains("warm-build"));
        assert!(json.contains("building"));
        assert!(json.contains("idle_timeout_secs"));
        // None deadline (busy) must be omitted, not "null"
        assert!(!json.contains("idle_shutdown_in_secs"));
    }

    #[test]
    fn test_daemon_status_output_not_running() {
        let output = DaemonStatusOutput {
            status: "not_running".to_string(),
            uptime: None,
            uptime_human: None,
            files: None,
            project: None,
            salsa_stats: None,
            liveness: None,
            semantic_index: None,
            memory: None,
            message: Some("Daemon not running".to_string()),
        };

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("not_running"));
        assert!(json.contains("not running"));
    }

    #[tokio::test]
    async fn test_daemon_status_not_running() {
        let temp = TempDir::new().unwrap();
        let args = DaemonStatusArgs {
            project: temp.path().to_path_buf(),
            session: None,
        };

        // Should succeed when daemon is not running (reports not_running)
        let result = args.run_async(OutputFormat::Json, true).await;
        assert!(result.is_ok());
    }
}

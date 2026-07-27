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

use crate::output::OutputFormat;
use clap::Args;
use serde::{Deserialize, Serialize};

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
    // Note: Use global --lang to specify language, or auto-detect if not specified
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
        let cmd = DaemonCommand::Warm {
            language: None, // Auto-detect
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
        let report = manager.warm()?;
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

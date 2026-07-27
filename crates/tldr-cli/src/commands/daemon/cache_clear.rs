//! Cache clear command implementation
//!
//! CLI command: `tldr cache clear [--project PATH]`
//!
//! Stops the project daemon, removes the authoritative redb/usearch derived
//! store, and then removes preserved legacy cache files.

use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;
use serde::Serialize;

use crate::output::OutputFormat;

use super::error::DaemonResult;
use super::ipc::send_command;
use super::types::DaemonCommand;

// =============================================================================
// CLI Arguments
// =============================================================================

/// Arguments for the `cache clear` command.
#[derive(Debug, Clone, Args)]
pub struct CacheClearArgs {
    /// Project root directory (default: current directory)
    #[arg(long, short = 'p', default_value = ".")]
    pub project: PathBuf,
}

// =============================================================================
// Output Types
// =============================================================================

/// Output structure for cache clear command.
#[derive(Debug, Clone, Serialize)]
pub struct CacheClearOutput {
    /// Status of the operation
    pub status: String,
    /// Number of files removed
    pub files_removed: usize,
    /// Bytes freed
    pub bytes_freed: u64,
    /// Human-readable size freed
    pub size_freed_human: String,
    /// Optional message
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// =============================================================================
// Command Implementation
// =============================================================================

impl CacheClearArgs {
    /// Run the cache clear command.
    pub fn run(&self, format: OutputFormat, quiet: bool) -> anyhow::Result<()> {
        // Create a new tokio runtime for the async operations
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(self.run_async(format, quiet))
    }

    /// Async implementation of the cache clear command.
    async fn run_async(&self, format: OutputFormat, quiet: bool) -> anyhow::Result<()> {
        // Resolve project path to absolute
        let project = self.project.canonicalize().unwrap_or_else(|_| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(&self.project)
        });

        // Try to stop daemon first if it's running
        // This ensures the daemon doesn't continue writing to cache files
        self.try_stop_daemon(&project).await;

        // Clear cache files
        let (files_removed, bytes_freed) = self.clear_cache_files(&project)?;

        let output = if files_removed == 0 {
            CacheClearOutput {
                status: "ok".to_string(),
                files_removed: 0,
                bytes_freed: 0,
                size_freed_human: "0 B".to_string(),
                message: Some("No cache directory found".to_string()),
            }
        } else {
            CacheClearOutput {
                status: "ok".to_string(),
                files_removed,
                bytes_freed,
                size_freed_human: format_bytes(bytes_freed),
                message: Some(format!("Cache cleared: {} file(s) removed", files_removed)),
            }
        };

        self.print_output(&output, format, quiet)
    }

    /// Try to stop the daemon if it's running.
    async fn try_stop_daemon(&self, project: &Path) {
        let cmd = DaemonCommand::Shutdown;
        // Ignore errors - daemon might not be running
        let _ = send_command(project, &cmd).await;
    }

    /// Clear the new artifact store and any preserved legacy cache files.
    fn clear_cache_files(&self, project: &Path) -> DaemonResult<(usize, u64)> {
        let mut files_removed = 0;
        let mut bytes_freed = 0u64;
        let mut roots = vec![
            project.join(".tldr").join("store"),
            project.join(".tldr").join("cache"),
        ];
        #[cfg(feature = "semantic")]
        roots.push(tldr_core::semantic::store_dir_for(project));

        for root in roots {
            if root.exists() {
                clear_files_recursive(&root, &mut files_removed, &mut bytes_freed)?;
                let _ = fs::remove_dir_all(&root);
            }
        }

        Ok((files_removed, bytes_freed))
    }

    /// Print output in the requested format.
    fn print_output(
        &self,
        output: &CacheClearOutput,
        format: OutputFormat,
        quiet: bool,
    ) -> anyhow::Result<()> {
        if quiet {
            return Ok(());
        }

        match format {
            OutputFormat::Json | OutputFormat::Compact => {
                println!("{}", serde_json::to_string_pretty(output)?);
            }
            OutputFormat::Text | OutputFormat::Sarif | OutputFormat::Dot => {
                if output.files_removed == 0 {
                    println!("No cache directory found");
                } else {
                    println!(
                        "Cache cleared: {} file(s) removed ({})",
                        output.files_removed, output.size_freed_human
                    );
                }
            }
        }

        Ok(())
    }
}

fn clear_files_recursive(
    directory: &Path,
    files_removed: &mut usize,
    bytes_freed: &mut u64,
) -> DaemonResult<()> {
    for entry in fs::read_dir(directory)?.flatten() {
        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            clear_files_recursive(&path, files_removed, bytes_freed)?;
        } else if metadata.is_file() {
            *bytes_freed += metadata.len();
            fs::remove_file(path)?;
            *files_removed += 1;
        }
    }
    Ok(())
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Format bytes as human-readable string.
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

// =============================================================================
// Tests
// =============================================================================

//! Cache statistics command implementation
//!
//! CLI command: `tldr cache stats [--project PATH]`
//!
//! Displays shared artifact-store statistics for a TLDR project.
//!
//! Statistics include:
//! - Salsa-style query cache: hits, misses, hit rate, invalidations
//! - Cache files: file count, total size on disk

use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;
use serde::Serialize;

use crate::output::OutputFormat;

use super::error::{DaemonError, DaemonResult};
use super::ipc::send_command;
use super::types::{
    ArtifactStoreStats, CacheFileInfo, DaemonCommand, DaemonResponse, SalsaCacheStats,
};
use tldr_core::artifact_store::{
    schema::STORE_FILE, ArtifactStore, GenerationSnapshot, RedbArtifactStore,
};

// =============================================================================
// CLI Arguments
// =============================================================================

/// Arguments for the `cache stats` command.
#[derive(Debug, Clone, Args)]
pub struct CacheStatsArgs {
    /// Project root directory (default: current directory)
    #[arg(long, short = 'p', default_value = ".")]
    pub project: PathBuf,
}

// =============================================================================
// Output Types
// =============================================================================

/// Output structure for cache stats command.
#[derive(Debug, Clone, Serialize)]
pub struct CacheStatsOutput {
    /// Authoritative shared artifact generation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_store: Option<ArtifactStoreStats>,
    /// Salsa-style query cache statistics
    #[serde(skip_serializing_if = "Option::is_none")]
    pub salsa_stats: Option<SalsaCacheStats>,
    /// Cache file information
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_files: Option<CacheFileInfo>,
    /// Optional message (e.g., "No cache statistics found")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// =============================================================================
// Command Implementation
// =============================================================================

impl CacheStatsArgs {
    /// Run the cache stats command.
    pub fn run(&self, format: OutputFormat, quiet: bool) -> anyhow::Result<()> {
        // Create a new tokio runtime for the async operations
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(self.run_async(format, quiet))
    }

    /// Async implementation of the cache stats command.
    async fn run_async(&self, format: OutputFormat, quiet: bool) -> anyhow::Result<()> {
        // Resolve project path to absolute
        let project = self.project.canonicalize().unwrap_or_else(|_| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(&self.project)
        });

        // Try to get stats from running daemon first
        let cmd = DaemonCommand::Status { session: None };

        match send_command(&project, &cmd).await {
            Ok(DaemonResponse::FullStatus {
                salsa_stats,
                artifact_store,
                ..
            }) => {
                // Daemon is running, use its stats
                let cache_files = scan_cache_files(&project)?;
                let output = CacheStatsOutput {
                    artifact_store,
                    salsa_stats: Some(salsa_stats),
                    cache_files: Some(cache_files),
                    message: None,
                };
                self.print_output(&output, format, quiet)
            }
            Ok(_) | Err(DaemonError::NotRunning) | Err(DaemonError::ConnectionRefused) => {
                // Daemon not running, read from cache files directly
                self.read_cache_from_files(&project, format, quiet)
            }
            Err(e) => Err(anyhow::anyhow!("Failed to get cache stats: {}", e)),
        }
    }

    /// Read cache statistics from files when daemon is not running.
    fn read_cache_from_files(
        &self,
        project: &Path,
        format: OutputFormat,
        quiet: bool,
    ) -> anyhow::Result<()> {
        let cache_dir = project.join(".tldr").join("store");

        // Check if cache directory exists
        if !cache_dir.exists() {
            let output = CacheStatsOutput {
                artifact_store: None,
                salsa_stats: None,
                cache_files: None,
                message: Some("No artifact store found".to_string()),
            };
            return self.print_output(&output, format, quiet);
        }

        let artifact_store = load_artifact_stats(project);

        // Scan cache files
        let cache_files = scan_cache_files(project)?;

        // Check if we have any cache data
        if artifact_store.is_none() && cache_files.file_count == 0 {
            let output = CacheStatsOutput {
                artifact_store: None,
                salsa_stats: None,
                cache_files: Some(cache_files),
                message: Some("No cache statistics found".to_string()),
            };
            return self.print_output(&output, format, quiet);
        }

        let output = CacheStatsOutput {
            artifact_store,
            salsa_stats: None,
            cache_files: Some(cache_files),
            message: None,
        };

        self.print_output(&output, format, quiet)
    }

    /// Print output in the requested format.
    fn print_output(
        &self,
        output: &CacheStatsOutput,
        format: OutputFormat,
        quiet: bool,
    ) -> anyhow::Result<()> {
        match format {
            OutputFormat::Json | OutputFormat::Compact => {
                // Always emit the structured payload — `quiet` suppresses
                // progress chatter, never the result (TLDR-3bk).
                println!("{}", serde_json::to_string_pretty(output)?);
            }
            OutputFormat::Text | OutputFormat::Sarif | OutputFormat::Dot => {
                if quiet {
                    return Ok(());
                }
                if let Some(ref msg) = output.message {
                    println!("{}", msg);
                    return Ok(());
                }

                println!("Cache Statistics");
                println!("================");

                if let Some(ref store) = output.artifact_store {
                    println!();
                    println!("Artifact Store:");
                    println!("  State:      {}", store.state);
                    println!("  Generation: {:?}", store.active_generation);
                    println!("  Files:      {}", format_number(store.files as u64));
                    println!("  redb bytes: {}", format_number(store.redb_bytes));
                }

                if let Some(ref stats) = output.salsa_stats {
                    println!();
                    println!("Hot Response Cache:");
                    println!("  Hits:          {}", format_number(stats.hits));
                    println!("  Misses:        {}", format_number(stats.misses));
                    println!("  Hit Rate:      {:.2}%", stats.hit_rate());
                    println!("  Invalidations: {}", format_number(stats.invalidations));
                    println!("  Recomputations: {}", format_number(stats.recomputations));
                }

                if let Some(ref files) = output.cache_files {
                    println!();
                    println!("Cache Files:");
                    println!("  Count: {} files", files.file_count);
                    println!("  Size:  {}", files.total_size_human);
                }
            }
        }

        Ok(())
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Scan durable files in the project's `.tldr/store/` directory.
fn scan_cache_files(project: &Path) -> DaemonResult<CacheFileInfo> {
    let cache_dir = project.join(".tldr").join("store");

    if !cache_dir.exists() {
        return Ok(CacheFileInfo {
            file_count: 0,
            total_bytes: 0,
            total_size_human: "0 B".to_string(),
        });
    }

    let mut file_count = 0;
    let mut total_bytes = 0u64;

    // Count all files in cache directory
    if let Ok(entries) = fs::read_dir(&cache_dir) {
        for entry in entries.flatten() {
            if let Ok(metadata) = entry.metadata() {
                if metadata.is_file() {
                    file_count += 1;
                    total_bytes += metadata.len();
                }
            }
        }
    }

    Ok(CacheFileInfo {
        file_count,
        total_bytes,
        total_size_human: format_bytes(total_bytes),
    })
}

fn load_artifact_stats(project: &Path) -> Option<ArtifactStoreStats> {
    let path = project.join(".tldr").join("store").join(STORE_FILE);
    if !path.exists() {
        return None;
    }
    let store = RedbArtifactStore::open(&path).ok()?;
    let active_generation = store.active_generation().ok().flatten();
    let snapshot = GenerationSnapshot::active(&store).ok().flatten();
    let files = snapshot.as_ref().map_or(0, GenerationSnapshot::file_count);
    let parse_errors = snapshot.as_ref().map_or(0, |snapshot| {
        snapshot
            .files()
            .map(|facts| facts.structure.parse_errors)
            .sum()
    });
    Some(ArtifactStoreStats {
        state: if active_generation.is_some() {
            "ready".into()
        } else {
            "cold".into()
        },
        active_generation,
        target_generation: None,
        files,
        parse_errors,
        redb_bytes: std::fs::metadata(path)
            .map(|value| value.len())
            .unwrap_or(0),
        last_error: None,
    })
}

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

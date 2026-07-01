//! `tldr daemon list` — print all live daemon entries from the v0.3.0
//! multi-daemon registry.
//!
//! Supersedes `daemon status`'s implicit single-daemon assumption: when
//! multiple daemons are running, users invoke `daemon list` to enumerate
//! them and `daemon status --project <path>` (or `daemon stop --project
//! <path> | --all`) to operate on a chosen one.

use clap::Args;
use serde::Serialize;

use crate::output::OutputFormat;

use super::daemon_registry::{live_entries, DaemonRegistryEntry};

/// Arguments for the `daemon list` command.
///
/// Output format is controlled by the global `--format` flag (default
/// `json`). No subcommand-local flags are required; the registry contents
/// fully describe the output.
#[derive(Debug, Clone, Args, Default)]
pub struct DaemonListArgs {}

/// JSON output shape: `{"daemons": [...entries...]}`.
#[derive(Debug, Serialize)]
struct DaemonListOutput<'a> {
    daemons: &'a [DaemonRegistryEntry],
}

impl DaemonListArgs {
    /// Run the daemon list command.
    pub fn run(&self, format: OutputFormat, quiet: bool) -> anyhow::Result<()> {
        let entries = live_entries();

        match format {
            OutputFormat::Json | OutputFormat::Compact => {
                // Always emit the structured payload (TLDR-3bk).
                let out = DaemonListOutput { daemons: &entries };
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
            OutputFormat::Text | OutputFormat::Sarif | OutputFormat::Dot if !quiet => {
                if entries.is_empty() {
                    println!("No daemons running");
                } else {
                    println!("PROJECT\tPID\tSOCKET\tSTARTED_AT");
                    for e in &entries {
                        println!(
                            "{}\t{}\t{}\t{}",
                            e.project.display(),
                            e.pid,
                            e.socket.display(),
                            e.started_at
                        );
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_args_default_constructs() {
        let _args = DaemonListArgs::default();
    }
}

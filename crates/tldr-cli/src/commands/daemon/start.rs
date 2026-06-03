//! Daemon start command implementation
//!
//! CLI command: `tldr daemon start [--project PATH] [--foreground]`
//!
//! This module handles starting the TLDR daemon process with:
//! - PID file locking to ensure single instance per project
//! - Daemonization (background mode) or foreground mode
//! - Socket binding for IPC communication
//!
//! # Security Mitigations
//!
//! - TIGER-P1-01: Exclusive file lock on PID file prevents race conditions
//! - TIGER-P2-02: Stale socket cleanup on startup

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Args;
use serde::Serialize;

use crate::output::OutputFormat;

use super::daemon::{start_daemon_background, wait_for_daemon, TLDRDaemon};
use super::daemon_registry::{add_entry, find_entry_unpruned, is_pid_alive, remove_entry};
use super::error::DaemonError;
use super::ipc::{check_socket_alive, cleanup_socket, compute_socket_path, IpcListener};
use super::pid::{compute_pid_path, try_acquire_lock};
use super::types::DaemonConfig;

// =============================================================================
// CLI Arguments
// =============================================================================

/// Arguments for the `daemon start` command.
#[derive(Debug, Clone, Args)]
pub struct DaemonStartArgs {
    /// Project root directory (default: current directory)
    #[arg(long, short = 'p', default_value = ".")]
    pub project: PathBuf,

    /// Run daemon in foreground (don't daemonize)
    #[arg(long)]
    pub foreground: bool,
}

// =============================================================================
// Output Types
// =============================================================================

/// Output structure for successful daemon start.
#[derive(Debug, Clone, Serialize)]
pub struct DaemonStartOutput {
    /// Status message
    pub status: String,
    /// PID of the daemon process
    pub pid: u32,
    /// Path to the socket file
    pub socket: PathBuf,
    /// Optional message
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// =============================================================================
// Project root resolution
// =============================================================================

/// Resolve the `--project` argument to a canonical absolute root.
///
/// FAIL-CLOSED single-instance hardening: if the path can't be canonicalized we
/// REFUSE to start rather than falling back to a non-canonical path. The socket
/// path and PID-lock key are both derived from this root (`compute_socket_path`
/// / `compute_pid_path` hash it), so an ambiguous fallback could hash to a
/// *different* key for the *same* folder — letting a second daemon index it
/// concurrently. That same-folder duplication is exactly what drove the
/// cold-build storm (multiple ~90-min/7GB rebuilds racing on one project). Same
/// folder must always resolve to the same key, so a path we can't resolve is a
/// hard error, not a guess.
fn resolve_project_root(project: &Path) -> anyhow::Result<PathBuf> {
    project.canonicalize().map_err(|e| {
        anyhow::anyhow!(
            "cannot resolve project root {}: {} — refusing to start so a second \
             daemon can't index the same folder under a different path key",
            project.display(),
            e
        )
    })
}

// =============================================================================
// Command Implementation
// =============================================================================

impl DaemonStartArgs {
    /// Run the daemon start command.
    pub fn run(&self, format: OutputFormat, quiet: bool) -> anyhow::Result<()> {
        // Create a new tokio runtime for the async operations
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(self.run_async(format, quiet))
    }

    /// Async implementation of the daemon start command.
    async fn run_async(&self, format: OutputFormat, quiet: bool) -> anyhow::Result<()> {
        // Resolve project path to a canonical absolute root. FAIL-CLOSED: an
        // unresolvable path is a hard error, never a non-canonical fallback,
        // so the socket/PID-lock key is stable per folder (see
        // resolve_project_root — single-instance hardening).
        let project = resolve_project_root(&self.project)?;

        // Single-instance guard (TLDR-82b, Hole B): a daemon that already owns
        // this folder may be mid-cold-build and unable to answer a socket
        // connect, so we judge liveness by the REGISTERED PID, never by
        // `check_socket_alive` — otherwise a busy owner is misjudged dead and
        // we spawn a duplicate that re-indexes the same folder (the cold-build
        // storm). Use the UNPRUNED lookup: the pruning `find_entry` would
        // delete a dead entry that `cleanup_socket`'s cross-tmpdir path still
        // relies on.
        if let Some(entry) = find_entry_unpruned(&project) {
            if is_pid_alive(entry.pid) {
                return Err(anyhow::anyhow!(
                    "Daemon already running (PID: {})",
                    entry.pid
                ));
            }
        }

        // Note: stale PID cleanup is intentionally NOT performed here, and
        // neither is stale-SOCKET cleanup (TLDR-82b, Hole B). Both happen
        // INSIDE the PID flock in `run_foreground` — a pre-lock reap could
        // race a concurrent start and yank the socket out from under a daemon
        // that is about to own the folder. `try_acquire_lock` is the
        // authoritative single-instance backstop; `IpcListener::bind_unix`
        // never unlinks a live socket for itself (issue #14).

        if self.foreground {
            // Run in foreground
            self.run_foreground(&project, format, quiet).await
        } else {
            // Run in background
            self.run_background(&project, format, quiet).await
        }
    }

    /// Run the daemon in foreground mode.
    async fn run_foreground(
        &self,
        project: &Path,
        format: OutputFormat,
        quiet: bool,
    ) -> anyhow::Result<()> {
        // Try to acquire PID lock
        let pid_path = compute_pid_path(project);
        let _pid_guard = try_acquire_lock(&pid_path).map_err(|e| match e {
            DaemonError::AlreadyRunning { pid } => {
                anyhow::anyhow!("Daemon already running (PID: {})", pid)
            }
            DaemonError::StalePidFile { pid } => {
                anyhow::anyhow!("Stale PID file (process {} not running)", pid)
            }
            other => anyhow::anyhow!("Failed to acquire lock: {}", other),
        })?;

        // Stale-socket cleanup UNDER the lock (TLDR-82b, Hole B): now that we
        // hold the single-instance flock, reap a socket left behind by a
        // previously-DEAD daemon so we can bind. Doing this only as the lock
        // owner means no concurrent start can yank the socket from a daemon
        // that is about to own the folder. `bind_unix` refuses to unlink a
        // live socket for itself, so the reap must happen here, not pre-lock.
        let socket_path = compute_socket_path(project);
        if socket_path.exists() && !check_socket_alive(project).await {
            cleanup_socket(project)?;
        }

        // Bind IPC listener
        let listener = IpcListener::bind(project).await.map_err(|e| match e {
            DaemonError::AddressInUse { addr } => {
                anyhow::anyhow!("Address already in use: {}", addr)
            }
            DaemonError::SocketBindFailed(io_err) => {
                anyhow::anyhow!("Failed to bind socket: {}", io_err)
            }
            other => anyhow::anyhow!("Socket error: {}", other),
        })?;

        let our_pid = std::process::id();

        // VAL-003 (v0.3.0): register the daemon in the multi-daemon
        // registry. Replaces v0.2.x's single-slot daemon-active.json.
        // Failures are logged but non-fatal — the file is auxiliary
        // state. A missing file degrades to the legacy behaviour where
        // `daemon status` from a different cwd reports `not_running`.
        if let Err(e) = add_entry(project, our_pid, &socket_path) {
            eprintln!(
                "warning: could not register daemon in registry: {}",
                e
            );
        }

        // Print startup message
        let output = DaemonStartOutput {
            status: "ok".to_string(),
            pid: our_pid,
            socket: socket_path.clone(),
            message: Some("Daemon started in foreground".to_string()),
        };

        if !quiet {
            match format {
                OutputFormat::Json | OutputFormat::Compact => {
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                OutputFormat::Text | OutputFormat::Sarif | OutputFormat::Dot => {
                    println!("Daemon started with PID {}", our_pid);
                    println!("Socket: {}", socket_path.display());
                }
            }
        }

        // Create and run daemon
        let config = DaemonConfig::default();
        let daemon = Arc::new(TLDRDaemon::new(project.to_path_buf(), config));
        daemon.run(listener).await?;

        // Cleanup socket and registry entry on exit.
        let _ = cleanup_socket(project);
        let _ = remove_entry(project);

        Ok(())
    }

    /// Run the daemon in background mode.
    async fn run_background(
        &self,
        project: &Path,
        format: OutputFormat,
        quiet: bool,
    ) -> anyhow::Result<()> {
        // First check if daemon is already running
        if check_socket_alive(project).await {
            // Try to get PID from PID file
            let pid_path = compute_pid_path(project);
            let pid = std::fs::read_to_string(&pid_path)
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);

            return Err(anyhow::anyhow!("Daemon already running (PID: {})", pid));
        }

        // Start the daemon in background
        let pid = start_daemon_background(project).await?;

        // Wait for daemon to become ready
        wait_for_daemon(project, 10)
            .await
            .map_err(|_| anyhow::anyhow!("Daemon failed to start within timeout"))?;

        let socket_path = compute_socket_path(project);

        // Print output
        let output = DaemonStartOutput {
            status: "ok".to_string(),
            pid,
            socket: socket_path.clone(),
            message: Some("Daemon started".to_string()),
        };

        if !quiet {
            match format {
                OutputFormat::Json | OutputFormat::Compact => {
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                OutputFormat::Text | OutputFormat::Sarif | OutputFormat::Dot => {
                    println!("Daemon started with PID {}", pid);
                    println!("Socket: {}", socket_path.display());
                }
            }
        }

        Ok(())
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// An existing directory resolves to its canonical absolute form.
    #[test]
    fn resolve_project_root_canonicalizes_existing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let resolved = resolve_project_root(tmp.path()).unwrap();
        assert_eq!(resolved, tmp.path().canonicalize().unwrap());
        assert!(resolved.is_absolute());
    }

    /// FAIL-CLOSED: an unresolvable path is a hard error, never a fallback —
    /// so the socket/PID key can't drift to a second value for one folder.
    #[test]
    fn resolve_project_root_fails_closed_on_missing_path() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist-1a2b3c");
        let err = resolve_project_root(&missing).unwrap_err();
        assert!(
            err.to_string().contains("cannot resolve project root"),
            "unexpected error: {err}"
        );
    }

    /// SAME FOLDER ⇒ SAME KEY: reaching a folder through a symlink resolves to
    /// the identical canonical root as reaching it directly. This is what
    /// guarantees two `daemon start`s on the same folder (one via a symlinked
    /// path) compute the same lock/socket key and so can't both index it.
    #[cfg(unix)]
    #[test]
    fn resolve_project_root_symlink_resolves_to_same_root() {
        let real = tempfile::tempdir().unwrap();
        let link_base = tempfile::tempdir().unwrap();
        let link = link_base.path().join("link");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();

        let via_direct = resolve_project_root(real.path()).unwrap();
        let via_link = resolve_project_root(&link).unwrap();
        assert_eq!(
            via_direct, via_link,
            "symlinked and direct paths must resolve to the same canonical root"
        );
    }

    /// HOLE B (TLDR-82b): a live owner that is NOT answering its socket — e.g.
    /// a daemon pegged mid-~90-min cold build — must still be treated as alive
    /// so `start` bails instead of spawning a duplicate that re-indexes the
    /// same folder. We simulate "alive but no live socket" by registering a
    /// LIVE non-daemon PID (our own test process) with a bogus socket path:
    /// nothing is listening, so the OLD socket-connect liveness check would
    /// report "dead" and proceed to reap+spawn — the PID-based check must bail.
    #[test]
    fn run_async_bails_on_live_owner_even_without_a_live_socket() {
        use crate::commands::daemon::daemon_registry::{
            add_entry, remove_entry, test_support::with_registry_dir,
        };
        use crate::output::OutputFormat;

        with_registry_dir(|dir| {
            let proj_dir = dir.join("busy-project");
            std::fs::create_dir_all(&proj_dir).unwrap();
            let project = proj_dir.canonicalize().unwrap();

            // Live PID (ourselves) + a socket path nothing is bound to.
            let bogus_socket = dir.join("busy-project.sock");
            add_entry(&project, std::process::id(), &bogus_socket)
                .expect("inject live owner");

            let args = DaemonStartArgs {
                project: project.clone(),
                foreground: true,
            };
            let rt = tokio::runtime::Runtime::new().unwrap();
            let err = rt
                .block_on(args.run_async(OutputFormat::Json, true))
                .expect_err("start must bail when a live PID already owns the folder");
            assert!(
                err.to_string().contains("already running"),
                "expected an 'already running' bail, got: {err}"
            );

            let _ = remove_entry(&project);
        });
    }

    #[test]
    fn test_daemon_start_args_default() {
        let args = DaemonStartArgs {
            project: PathBuf::from("."),
            foreground: false,
        };

        assert_eq!(args.project, PathBuf::from("."));
        assert!(!args.foreground);
    }

    #[test]
    fn test_daemon_start_args_foreground() {
        let args = DaemonStartArgs {
            project: PathBuf::from("/test/project"),
            foreground: true,
        };

        assert!(args.foreground);
    }

    #[test]
    fn test_daemon_start_output_serialization() {
        let output = DaemonStartOutput {
            status: "ok".to_string(),
            pid: 12345,
            socket: PathBuf::from("/tmp/tldr-abc123.sock"),
            message: Some("Daemon started".to_string()),
        };

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("ok"));
        assert!(json.contains("12345"));
        assert!(json.contains("tldr-abc123.sock"));
    }
}

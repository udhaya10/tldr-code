//! Active-daemon discovery file (VAL-013, issue #20).
//!
//! `tldr daemon status` historically defaulted `--project` to `"."`, computed
//! a socket-path hash from the canonicalized cwd, and connected via that
//! hash. Invoked from a cwd different from the original
//! `daemon start --project` cwd, the hash differs → connect fails → status
//! incorrectly reports `not_running`, even when a daemon IS alive.
//!
//! This module implements the **single-daemon quick-fix path** from the
//! VAL-013 spec: on successful bind, daemon start atomically writes
//! `<cache_dir>/tldr/daemon-active.json` containing `{project, pid, socket}`.
//! When `daemon status` is invoked WITHOUT an explicit `--project`, it reads
//! this file, verifies the PID is alive (via `kill(pid, 0)` on Unix), and
//! falls back to the recorded project path for socket discovery.
//!
//! The multi-daemon case is intentionally NOT handled here — users running
//! multiple daemons can still pass `--project` explicitly. A global daemon
//! registry is deferred to v0.3.0.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Active-daemon discovery record persisted to disk.
///
/// Written atomically by `daemon start` after a successful socket bind, read
/// by `daemon status` when `--project` is the default, and removed by
/// `daemon stop` after a successful shutdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonActive {
    /// Canonicalized project path the daemon was started with.
    pub project: PathBuf,
    /// PID of the daemon process. Validated via `kill(pid, 0)` on Unix
    /// before the record is trusted.
    pub pid: u32,
    /// Path to the daemon's IPC socket (informational; status recomputes
    /// from `project` for safety).
    pub socket: PathBuf,
}

/// Path to the active-daemon discovery file.
///
/// Resolves to `<cache_dir>/tldr/daemon-active.json`. Falls back to
/// `./.cache/tldr/daemon-active.json` if `dirs::cache_dir()` is unavailable
/// (e.g., in restricted sandboxes); the file is auxiliary state, so this
/// fallback is benign.
pub fn active_file_path() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("tldr")
        .join("daemon-active.json")
}

/// Atomically write the active-daemon record.
///
/// Writes to `<path>.tmp` first, then renames into place. The rename is
/// atomic on POSIX (and on NTFS via MoveFileEx), so a concurrent reader
/// either sees the previous file or the new one — never a half-written
/// file.
///
/// Failures are surfaced to the caller, but the caller (`daemon start`)
/// treats them as warnings rather than fatal errors: the discovery file
/// is auxiliary state and a missing file simply degrades to the
/// pre-fix behaviour (i.e., `daemon status` from a different cwd reports
/// `not_running`, exactly as today).
pub fn write_active(project: &Path, pid: u32, socket: &Path) -> std::io::Result<()> {
    let path = active_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let record = DaemonActive {
        project: project.to_path_buf(),
        pid,
        socket: socket.to_path_buf(),
    };
    let json = serde_json::to_string_pretty(&record).map_err(std::io::Error::other)?;

    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Read the active-daemon record, or `None` if absent / stale / corrupt.
///
/// "Stale" here means the recorded PID is no longer alive — `kill(pid, 0)`
/// returns `ESRCH`. This guards against the case where a daemon crashed
/// without removing the file: we don't want `daemon status` to report a
/// dead daemon as `running`.
pub fn read_active() -> Option<DaemonActive> {
    let path = active_file_path();
    let content = std::fs::read_to_string(&path).ok()?;
    let parsed: DaemonActive = serde_json::from_str(&content).ok()?;
    if !is_pid_alive(parsed.pid) {
        return None;
    }
    Some(parsed)
}

/// Remove the active-daemon record, ignoring `NotFound`.
///
/// Called from `daemon stop` after a successful shutdown. NotFound is
/// expected when the file was never written (e.g., daemon crashed during
/// bind) or was already cleaned up.
pub fn remove_active() -> std::io::Result<()> {
    match std::fs::remove_file(active_file_path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Best-effort liveness probe. Delegates to the shared cross-platform
/// implementation in `daemon_registry`.
fn is_pid_alive(pid: u32) -> bool {
    super::daemon_registry::is_pid_alive(pid)
}

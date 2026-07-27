//! CLI-wide liveness poke (TLDR-nke, epic TLDR-cxa).
//!
//! Only ~18 of ~64 CLI commands touch the daemon's stream socket; the rest
//! generated ZERO liveness, so a daemon serving a project where someone runs
//! `tldr loc` all afternoon still idled out. Every `tldr` invocation now
//! fires a one-shot datagram poke at any registered daemon whose project
//! contains the cwd, deferring idle shutdown ([`Source::CliPoke`]).
//!
//! Transport: a UNIX DATAGRAM side channel at `<stream-socket>.poke` —
//! deliberately NOT:
//! - stream connect-and-close: connect can block up to
//!   `CONNECTION_TIMEOUT_SECS` (5s), and an accepted odd connect logs a
//!   spurious "Connection error" daemon-side;
//! - a touch-file: would turn the daemon's 100ms accept loop into a
//!   perpetual disk poller.
//!
//! Hard constraints (all verified empirically on macOS, 2026-06-04: an
//! unbound `SOCK_DGRAM` sender delivers, and a dead target errors with
//! ENOENT instantly):
//! - ZERO perceptible latency on unrelated commands: one env check, one
//!   registry file read (unpruned — never writes), one non-blocking
//!   `send_to`. No retries.
//! - Silent failure everywhere: a missing/dead daemon, a full socket
//!   buffer (EAGAIN), or an unsupported platform must never surface.
//! - Opt-out for CI/bulk callers via `TLDR_NO_POKE=1`.

use std::path::PathBuf;

#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::sync::Arc;

#[cfg(unix)]
use super::activity::{ActivityTracker, Source};

/// Sender-side pieces (path derivation, registry-gated fire-and-forget send)
/// live in `tldr_core::liveness` — shared with the `tldr_mcp` binary
/// (TLDR-axz), which cannot depend on this crate (cycle: `tldr-cli` already
/// depends on `tldr-mcp` for the bin re-export). This module keeps only the
/// DAEMON-side receiver.
pub(crate) use tldr_core::liveness::poke_path_for;

/// Removes the poke socket file on drop (daemon shutdown). Mirrors the
/// stream socket's cleanup discipline — Unix socket files do not vanish on
/// close.
#[cfg(unix)]
pub(crate) struct PokeGuard {
    path: PathBuf,
}

#[cfg(unix)]
impl Drop for PokeGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Bind the datagram poke receiver next to the daemon's stream socket and
/// spawn its listener task. Returns the cleanup guard to hold for the
/// daemon's lifetime, or `None` (logged) if the bind fails — the daemon
/// keeps running; pokes are an enhancement, not a dependency.
///
/// Must be called from within a Tokio runtime.
#[cfg(unix)]
pub(crate) fn spawn_poke_receiver(
    stream_socket_path: &Path,
    activity: Arc<ActivityTracker>,
) -> Option<PokeGuard> {
    use std::os::unix::fs::PermissionsExt;

    let path = poke_path_for(stream_socket_path);

    // Stale file from a crashed predecessor: safe to remove unconditionally —
    // our STREAM socket bind already succeeded (see start.rs), so this
    // process owns the project's daemon identity and any leftover poke file
    // is necessarily orphaned.
    let _ = std::fs::remove_file(&path);

    let sock = match tokio::net::UnixDatagram::bind(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[nke] failed to bind poke socket {}: {e}", path.display());
            return None;
        }
    };
    // Owner-only, matching the stream socket (TIGER-P3-01).
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));

    tokio::spawn(async move {
        let mut buf = [0u8; 8];
        loop {
            match sock.recv_from(&mut buf).await {
                Ok(_) => activity.touch(Source::CliPoke),
                Err(e) => {
                    // Do NOT continue on error: a vanished socket would make
                    // recv fail in a hot loop and burn a core. One log, done.
                    eprintln!("[nke] poke receiver error: {e}; receiver stopped");
                    break;
                }
            }
        }
    });

    Some(PokeGuard { path })
}

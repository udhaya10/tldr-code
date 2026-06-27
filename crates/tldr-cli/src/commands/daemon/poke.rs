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

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end over a real datagram socket: receiver bound, unbound
    /// sender pokes, CliPoke presence refreshed. This is the macOS
    /// unbound-sender semantics the whole design rests on.
    #[cfg(unix)]
    #[tokio::test]
    async fn poke_round_trip_touches_cli_poke_presence() {
        let dir = tempfile::tempdir().unwrap();
        let stream_path = dir.path().join("tldr-test.sock");
        let activity = Arc::new(ActivityTracker::new());

        let _guard = spawn_poke_receiver(&stream_path, Arc::clone(&activity))
            .expect("receiver must bind in a fresh tempdir");

        // Make CliPoke presence measurably stale before the poke.
        std::thread::sleep(std::time::Duration::from_millis(30));
        let before = activity.presence_ages()[Source::CliPoke as usize];

        let sock = std::os::unix::net::UnixDatagram::unbound().unwrap();
        sock.set_nonblocking(true).unwrap();
        sock.send_to(b"1", poke_path_for(&stream_path)).unwrap();

        // Poll for the receiver task to process the datagram.
        let mut after = before;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            after = activity.presence_ages()[Source::CliPoke as usize];
            if after < before {
                break;
            }
        }
        assert!(
            after < before,
            "poke must refresh CliPoke presence (before {before:?}, after {after:?})"
        );
    }

    /// A dead daemon (no receiver) must fail instantly and silently — the
    /// sender's contract on the CLI hot path.
    #[cfg(unix)]
    #[test]
    fn poke_to_dead_target_is_instant_and_silent() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("tldr-gone.poke");
        let started = std::time::Instant::now();

        let sock = std::os::unix::net::UnixDatagram::unbound().unwrap();
        sock.set_nonblocking(true).unwrap();
        let res = sock.send_to(b"1", &gone);

        assert!(res.is_err(), "dead target must error (ENOENT), not deliver");
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "dead-target send must not block"
        );
    }

    /// Guard drop must remove the socket file (shutdown cleanup parity with
    /// the stream socket).
    #[cfg(unix)]
    #[tokio::test]
    async fn guard_drop_removes_poke_socket_file() {
        let dir = tempfile::tempdir().unwrap();
        let stream_path = dir.path().join("tldr-test.sock");
        let activity = Arc::new(ActivityTracker::new());

        let guard = spawn_poke_receiver(&stream_path, activity).unwrap();
        let poke = poke_path_for(&stream_path);
        assert!(poke.exists(), "receiver must bind the poke file");
        drop(guard);
        assert!(!poke.exists(), "guard drop must remove the poke file");
    }
}

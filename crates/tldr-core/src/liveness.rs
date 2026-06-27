//! Daemon liveness poke — the SENDER side of presence-based daemon liveness
//! (epic TLDR-cxa), shared by every tldr binary.
//!
//! The daemon idles out only when the project is dormant (TLDR-3w5). Any
//! tldr-family process doing work in a project is presence, so each binary
//! pokes the project's registered daemon once per unit of work:
//! - `tldr` CLI: once per invocation (TLDR-nke, `run_command` dispatch);
//! - `tldr_mcp`: once per `tools/call` (TLDR-axz — it is a SEPARATE binary;
//!   without this, an MCP-only agent reproduces the original idle-kill bug).
//!
//! This lives in `tldr-core` because it is the only crate below both
//! binaries: `tldr-cli` depends on `tldr-mcp` (the `tldr_mcp` bin
//! re-export), so the MCP crate cannot import the CLI's daemon module
//! without a cycle. The daemon-side receiver and the full registry
//! read/write machinery stay in `tldr-cli`; this module owns exactly the
//! pieces a sender needs — the registry path, the poke-socket derivation,
//! and the fire-and-forget datagram send — so the two sides cannot drift
//! apart in separate copies.
//!
//! Transport contract (verified empirically on macOS, 2026-06-04: an
//! unbound `SOCK_DGRAM` sender delivers; a dead target errors ENOENT
//! instantly): one env check, one small registry file read (never writes),
//! one non-blocking `send_to` per registered daemon. No retries; every
//! failure is silent; `TLDR_NO_POKE=1` opts out (CI/bulk callers).

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Location of the daemon registry file.
///
/// Resolution order (MUST match the writer in
/// `tldr-cli/src/commands/daemon/daemon_registry.rs`, which delegates here):
/// 1. `TLDR_DAEMON_REGISTRY_DIR` env override (test isolation).
/// 2. `<dirs::cache_dir()>/tldr/daemon-registry.json`.
/// 3. `./.cache/tldr/daemon-registry.json` fallback.
pub fn registry_file_path() -> PathBuf {
    if let Ok(dir) = std::env::var("TLDR_DAEMON_REGISTRY_DIR") {
        warn_registry_override_once();
        return PathBuf::from(dir).join("daemon-registry.json");
    }
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("tldr")
        .join("daemon-registry.json")
}

/// One-time stderr warning when the `TLDR_DAEMON_REGISTRY_DIR` override is
/// honored (W5): it is a test-isolation hook, and in normal operation it
/// silently redirects every daemon lookup — an environment-controlling
/// caller could point clients at a registry they own. Surface it once.
fn warn_registry_override_once() {
    use std::sync::Once;
    static WARN: Once = Once::new();
    WARN.call_once(|| {
        eprintln!(
            "[tldr] WARNING: TLDR_DAEMON_REGISTRY_DIR override active — daemon registry redirected"
        );
    });
}

/// Datagram poke path derived from a daemon's STREAM socket path:
/// `tldr-{hash}.sock` → `tldr-{hash}.poke`. Always derive from the
/// registry-RECORDED socket, not a locally recomputed one — the daemon binds
/// in ITS temp dir, which can differ from this process's `TMPDIR` (the W6
/// cross-TMPDIR class).
pub fn poke_path_for(socket_path: &Path) -> PathBuf {
    socket_path.with_extension("poke")
}

/// Minimal read-only view of a registry entry — just what a sender needs.
#[derive(Debug, Deserialize)]
struct SenderEntry {
    project: PathBuf,
    socket: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
struct SenderRegistry {
    #[serde(default)]
    daemons: Vec<SenderEntry>,
}

/// Fire-and-forget liveness poke: defer idle shutdown of every registered
/// daemon whose project CONTAINS the current directory (ancestor match — an
/// invocation from a subdirectory is presence for the project's daemon).
///
/// Cost contract (this runs on EVERY CLI invocation and MCP tool call): one
/// env check, one small file read — strictly read-only, never prunes or
/// rewrites the registry; a dead entry's `send_to` just ENOENTs — and one
/// non-blocking datagram send per match. All failures silent by design.
pub fn poke_registered_daemons() {
    if std::env::var_os("TLDR_NO_POKE").is_some() {
        return;
    }
    #[cfg(unix)]
    {
        let Ok(cwd) = std::env::current_dir() else {
            return;
        };
        let cwd = cwd.canonicalize().unwrap_or(cwd);
        let registry: SenderRegistry = match std::fs::read_to_string(registry_file_path()) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => return,
        };
        for entry in registry
            .daemons
            .iter()
            .filter(|d| cwd.starts_with(&d.project))
        {
            let poke = poke_path_for(&entry.socket);
            if let Ok(sock) = std::os::unix::net::UnixDatagram::unbound() {
                let _ = sock.set_nonblocking(true);
                let _ = sock.send_to(b"1", &poke);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both env-mutating tests serialize on this — cargo runs tests in
    /// parallel threads and `TLDR_DAEMON_REGISTRY_DIR` is process-global.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn poke_path_replaces_sock_extension() {
        assert_eq!(
            poke_path_for(Path::new("/tmp/tldr-abc123.sock")),
            PathBuf::from("/tmp/tldr-abc123.poke")
        );
    }

    #[test]
    fn registry_path_honors_env_override() {
        let _env = ENV_LOCK.lock().unwrap();
        std::env::set_var("TLDR_DAEMON_REGISTRY_DIR", "/custom/dir");
        let p = registry_file_path();
        std::env::remove_var("TLDR_DAEMON_REGISTRY_DIR");
        assert_eq!(p, PathBuf::from("/custom/dir/daemon-registry.json"));
    }

    #[test]
    fn env_opt_out_short_circuits() {
        // Must not panic or touch the registry; observable behavior is just
        // "returns immediately" — a smoke for the TLDR_NO_POKE gate.
        let _env = ENV_LOCK.lock().unwrap();
        std::env::set_var("TLDR_NO_POKE", "1");
        poke_registered_daemons();
        std::env::remove_var("TLDR_NO_POKE");
    }

    /// End-to-end sender: registry entry whose project contains cwd → the
    /// bound poke socket receives a datagram. Also proves a NON-matching
    /// entry (project not an ancestor of cwd) is not poked.
    #[cfg(unix)]
    #[test]
    fn pokes_ancestor_registered_daemon_only() {
        use std::os::unix::net::UnixDatagram;

        let _env = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();

        // Receiver for the MATCHING daemon (project = cwd's root ancestor).
        let match_sock = dir.path().join("tldr-match.sock");
        let match_rx = UnixDatagram::bind(poke_path_for(&match_sock)).unwrap();
        match_rx.set_nonblocking(true).unwrap();

        // Receiver for the NON-matching daemon (unrelated project path).
        let other_sock = dir.path().join("tldr-other.sock");
        let other_rx = UnixDatagram::bind(poke_path_for(&other_sock)).unwrap();
        other_rx.set_nonblocking(true).unwrap();

        let registry = serde_json::json!({
            "daemons": [
                { "project": cwd, "pid": 1, "socket": match_sock, "started_at": "t" },
                { "project": dir.path().join("unrelated-project"), "pid": 2,
                  "socket": other_sock, "started_at": "t" },
            ]
        });
        std::fs::write(
            dir.path().join("daemon-registry.json"),
            serde_json::to_string(&registry).unwrap(),
        )
        .unwrap();

        std::env::set_var("TLDR_DAEMON_REGISTRY_DIR", dir.path());
        poke_registered_daemons();
        std::env::remove_var("TLDR_DAEMON_REGISTRY_DIR");

        let mut buf = [0u8; 8];
        assert!(
            match_rx.recv_from(&mut buf).is_ok(),
            "daemon whose project contains cwd must be poked"
        );
        assert!(
            other_rx.recv_from(&mut buf).is_err(),
            "daemon for an unrelated project must NOT be poked"
        );
    }
}

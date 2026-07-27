//! Multi-daemon registry (v0.3.0 VAL-003).
//!
//! Replaces v0.2.2's single-slot `daemon-active.json` with a multi-entry
//! `daemon-registry.json` file. Each entry records one running daemon; the
//! file always contains the union of all live daemons known to the user.
//!
//! # Concurrency (OS-level advisory file lock)
//!
//! The per-project flock at [`super::pid::try_acquire_lock`] (pid.rs:261,
//! `libc::flock(LOCK_EX | LOCK_NB)`) protects the SOCKET file, NOT the
//! registry. Two `daemon start` calls from DIFFERENT projects bypass that
//! flock and race read-modify-write the shared registry.
//!
//! Registry writes are serialized via [`std::fs::File::lock`] (stable since
//! Rust 1.89) on an adjacent `.lock` file. This is cross-platform — it uses
//! `flock` on Unix and `LockFileEx` on Windows — and replaces the previous
//! hand-rolled `libc::flock` + retry loop and the no-op Windows stub.
//!
//! ## Why this matters: launchd / multi-project startup
//!
//! On macOS, launchd plists with `RunAtLoad: true` fire in parallel at login.
//! If each project has its own plist, multiple `daemon start` calls hit this
//! shared registry simultaneously. Without serialization, entries silently
//! overwrite each other. The blocking `File::lock` ensures each writer waits
//! its turn — correct behavior for a startup sequence where sub-second
//! latency is irrelevant.
//!
//! # Migration from v0.2.x
//!
//! On first registry access, [`migrate_from_active_if_needed`] looks for the
//! legacy `daemon-active.json`; if present and its PID is alive, it is
//! converted into a registry entry and the legacy file is deleted. If the
//! PID is dead, the legacy file is also removed (stale record cleanup).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One live-daemon record in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonRegistryEntry {
    /// Canonicalized project path the daemon was started with.
    pub project: PathBuf,
    /// PID of the daemon process. Validated via `kill(pid, 0)` on Unix.
    pub pid: u32,
    /// Path to the daemon's IPC socket (informational).
    pub socket: PathBuf,
    /// RFC3339 timestamp recorded at registration time.
    pub started_at: String,
}

/// On-disk registry shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonRegistry {
    /// All live daemons known at the time of read.
    pub daemons: Vec<DaemonRegistryEntry>,
}

/// Path to the daemon registry file.
///
/// Delegates to `tldr_core::liveness::registry_file_path` — the resolution
/// (env override → cache dir → fallback) is shared with the liveness-poke
/// SENDERS in other binaries (`tldr_mcp`, TLDR-axz) and must not drift from
/// this writer side. Includes the one-time `TLDR_DAEMON_REGISTRY_DIR`
/// override warning (W5).
pub fn registry_file_path() -> PathBuf {
    tldr_core::liveness::registry_file_path()
}

/// Create `dir` (if missing) and constrain it to owner-only access (`0700` on
/// unix). The registry records project paths and PIDs, so the directory must
/// not be world-traversable even under a permissive umask (e.g. `umask 000`
/// in CI/Docker), which would otherwise defeat the socket filename binding in
/// `ipc::connect_unix` (W3/W4).
fn ensure_secure_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Atomically write `registry` to [`registry_file_path`] via tmp + rename.
fn write_registry_atomic(registry: &DaemonRegistry) -> std::io::Result<()> {
    let path = registry_file_path();
    if let Some(parent) = path.parent() {
        ensure_secure_dir(parent)?;
    }
    let json = serde_json::to_string_pretty(registry).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    // Constrain the registry file to owner read/write before it is published
    // via rename — it leaks project paths and PIDs otherwise (W4). rename
    // preserves the mode, so the live file inherits 0600.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn registry_lock_path() -> PathBuf {
    registry_file_path().with_extension("json.lock")
}

/// Acquire an exclusive OS-level advisory lock on the registry lock file,
/// execute `f`, and release the lock when the file handle drops.
fn with_registry_lock<T>(f: impl FnOnce() -> std::io::Result<T>) -> std::io::Result<T> {
    let lock_path = registry_lock_path();
    if let Some(parent) = lock_path.parent() {
        ensure_secure_dir(parent)?;
    }

    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;

    // Blocks until the lock is acquired. Cross-platform: uses flock on Unix,
    // LockFileEx on Windows. The lock is released when `lock_file` drops at
    // the end of this function.
    lock_file.lock()?;

    f()
}

/// Read the registry from disk, run one-shot v0.2.x migration if needed,
/// and prune dead-PID entries. The pruned-and-migrated registry is also
/// written back so subsequent reads observe a clean state.
///
/// Auxiliary state — a missing/corrupt file simply yields an empty registry.
pub fn read_registry() -> DaemonRegistry {
    migrate_from_active_if_needed();
    let path = registry_file_path();
    let mut registry = match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => DaemonRegistry::default(),
    };
    let original_len = registry.daemons.len();
    registry.daemons.retain(|d| is_pid_alive(d.pid));
    if registry.daemons.len() != original_len {
        // Pruned at least one stale entry — flush back. Best-effort.
        let _ = write_registry_atomic(&registry);
    }
    registry
}

/// Convenience: list of live entries (after pruning + migration).
pub fn live_entries() -> Vec<DaemonRegistryEntry> {
    read_registry().daemons
}

/// Look up a registry entry by canonicalized project path.
pub fn find_entry(project: &Path) -> Option<DaemonRegistryEntry> {
    let canon = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());
    read_registry()
        .daemons
        .into_iter()
        .find(|d| d.project == canon)
}

/// Read the registry from disk WITHOUT pruning dead-PID entries, migrating, or
/// writing back.
///
/// [`read_registry`] prunes dead entries, which is wrong for socket cleanup:
/// the stale-cleanup scenario is *precisely* when the daemon is dead, so a
/// pruning read would drop the very entry whose recorded socket path must be
/// removed — leaving a cross-TMPDIR orphan behind (W6). Callers that act on a
/// dead daemon's record (only `ipc::cleanup_socket`) must use this.
fn read_registry_unpruned() -> DaemonRegistry {
    let path = registry_file_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => DaemonRegistry::default(),
    }
}

/// Look up a registry entry by canonicalized project path WITHOUT pruning dead
/// PIDs. The returned entry's [`is_pid_alive`] status is the caller's to check.
/// See [`read_registry_unpruned`] for why cleanup must not prune.
pub fn find_entry_unpruned(project: &Path) -> Option<DaemonRegistryEntry> {
    let canon = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());
    read_registry_unpruned()
        .daemons
        .into_iter()
        .find(|d| d.project == canon)
}

/// Add (or replace) the registry entry for `project` while holding the
/// registry write lock.
pub fn add_entry(project: &Path, pid: u32, socket: &Path) -> std::io::Result<()> {
    let canon = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());

    with_registry_lock(|| {
        let mut registry = read_registry();
        registry.daemons.retain(|d| d.project != canon);
        registry.daemons.push(DaemonRegistryEntry {
            project: canon.clone(),
            pid,
            socket: socket.to_path_buf(),
            started_at: chrono::Utc::now().to_rfc3339(),
        });
        write_registry_atomic(&registry)
    })
}

/// Remove the registry entry for `project` while holding the registry write
/// lock.
pub fn remove_entry(project: &Path) -> std::io::Result<()> {
    let canon = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());

    with_registry_lock(|| {
        let mut registry = read_registry();
        let before = registry.daemons.len();
        registry.daemons.retain(|d| d.project != canon);
        if registry.daemons.len() == before {
            // Nothing to remove — caller's invariant satisfied.
            return Ok(());
        }
        write_registry_atomic(&registry)
    })
}

/// One-shot migration from v0.2.x `daemon-active.json`.
///
/// Triggered on first registry access. If a legacy daemon-active.json exists
/// and its PID is alive, append it as a registry entry. Either way, delete
/// the legacy file so subsequent registry reads do not re-trigger migration.
fn migrate_from_active_if_needed() {
    let registry_path = registry_file_path();
    if registry_path.exists() {
        return;
    }

    // Use the same parent dir as the registry for the legacy file lookup.
    // This matches the production layout (both files share `<cache>/tldr/`)
    // AND the test layout (both share the env-overridden directory).
    let active_path = match registry_path.parent() {
        Some(p) => p.join("daemon-active.json"),
        None => return,
    };
    if !active_path.exists() {
        return;
    }

    // Best-effort: read + validate + migrate. Failures collapse to "delete
    // the legacy file and move on" so the user is not stuck with the
    // legacy file blocking new registry creation.
    let migrated = match std::fs::read_to_string(&active_path) {
        Ok(content) => match serde_json::from_str::<super::daemon_active::DaemonActive>(&content) {
            Ok(active) if is_pid_alive(active.pid) => Some(DaemonRegistryEntry {
                project: active.project,
                pid: active.pid,
                socket: active.socket,
                started_at: chrono::Utc::now().to_rfc3339(),
            }),
            _ => None,
        },
        Err(_) => None,
    };

    if let Some(entry) = migrated {
        let registry = DaemonRegistry {
            daemons: vec![entry],
        };
        let _ = write_registry_atomic(&registry);
    }
    // Always delete the legacy file once migration has been attempted —
    // a dead-PID record is stale and should not block future registry
    // creation.
    let _ = std::fs::remove_file(&active_path);
}

/// Cross-platform PID liveness check via the `process_alive` crate.
/// Returns `true` if the process is alive OR if the state is unknown
/// (e.g. insufficient permissions) — erring on the side of keeping the entry.
pub(crate) fn is_pid_alive(pid: u32) -> bool {
    let state = process_alive::state(process_alive::Pid::from(pid));
    !matches!(state, process_alive::State::Dead)
}

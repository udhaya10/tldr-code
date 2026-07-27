//! PID file locking for daemon singleton enforcement
//!
//! This module provides cross-platform file locking to ensure only one daemon
//! instance runs per project. It addresses these security mitigations:
//!
//! - TIGER-P1-01: Atomic lock acquisition before PID write (prevents startup race)
//! - TIGER-P3-02: Acquire lock BEFORE reading existing PID (prevents TOCTOU attacks)
//!
//! # Security Pattern
//!
//! The lock acquisition follows this secure pattern:
//! 1. Create/open PID file
//! 2. Acquire exclusive non-blocking lock FIRST (before any reads)
//! 3. If lock fails, read PID and check if process is running
//! 4. If lock succeeds, truncate and write our PID
//! 5. Return guard that releases lock on drop
//!
//! This order is critical - acquiring the lock before reading prevents TOCTOU
//! (time-of-check to time-of-use) vulnerabilities where an attacker could
//! manipulate the PID file between our check and lock acquisition.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::commands::daemon::error::{DaemonError, DaemonResult};

// =============================================================================
// Path Computation
// =============================================================================

/// Compute a deterministic hash for a project path.
///
/// Uses MD5 hash of the canonicalized path, truncated to 8 hex characters.
/// This ensures the same project always gets the same PID/socket files.
pub fn compute_hash(project: &Path) -> String {
    // Canonicalize path if possible, otherwise use as-is
    let project_str = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf())
        .to_string_lossy()
        .to_string();

    let digest = md5::compute(project_str.as_bytes());

    // Take first 8 hex characters
    format!("{:x}", digest)[..8].to_string()
}

/// Compute the PID file path for a project.
///
/// Path format: `{temp_dir}/tldr-{hash}.pid`
/// where hash = MD5(canonicalized_project_path)[:8]
pub fn compute_pid_path(project: &Path) -> PathBuf {
    let hash = compute_hash(project);
    let tmp_dir = std::env::temp_dir();
    tmp_dir.join(format!("tldr-{}.pid", hash))
}

// =============================================================================
// PID Guard (RAII lock holder)
// =============================================================================

/// Guard that holds the PID file lock and releases it on drop.
///
/// The guard ensures:
/// - Lock is held for the daemon's entire lifetime
/// - PID file is properly cleaned up on normal shutdown
/// - Lock is automatically released even on panic
pub struct PidGuard {
    /// The locked file handle
    _file: File,
    /// Path to the PID file (for cleanup)
    path: PathBuf,
    /// Our PID
    pid: u32,
}

impl PidGuard {
    /// Get the PID stored in this guard
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Get the path to the PID file
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        // Try to remove the PID file on cleanup
        // Ignore errors - the file might already be gone
        let _ = std::fs::remove_file(&self.path);

        // Lock is automatically released when file handle is dropped
    }
}

// =============================================================================
// Process Detection
// =============================================================================

/// Cross-platform check if a process with the given PID is currently running.
/// Delegates to `process_alive` which uses `kill(pid, 0)` on Unix and
/// `OpenProcess` on Windows.
pub fn is_process_running(pid: u32) -> bool {
    super::daemon_registry::is_pid_alive(pid)
}

// =============================================================================
// Lock Acquisition
// =============================================================================

/// Try to acquire an exclusive lock on the PID file.
///
/// # Security Pattern (TIGER-P1-01, TIGER-P3-02)
///
/// This function follows a secure lock acquisition pattern:
/// 1. Create/open file with read+write
/// 2. Acquire exclusive non-blocking lock FIRST
/// 3. If lock fails, read existing PID and check process status
/// 4. If lock succeeds, truncate file and write our PID
/// 5. Return guard that releases lock on drop
///
/// # Errors
///
/// - `AlreadyRunning { pid }` - Another daemon is running
/// - `LockFailed` - Could not acquire lock for other reasons
/// - `Io` - File system errors
pub fn try_acquire_lock(pid_path: &Path) -> DaemonResult<PidGuard> {
    // Ensure parent directory exists
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Open or create the PID file
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false) // Don't truncate yet - we might fail to lock
        .open(pid_path)?;

    // Try to acquire exclusive lock FIRST (before reading)
    // This is critical for security - prevents TOCTOU attacks
    match try_lock_file(&file) {
        Ok(()) => {
            // Lock acquired successfully
            let our_pid = std::process::id();

            // Now safe to truncate and write our PID
            let mut file = file;
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
            writeln!(file, "{}", our_pid)?;
            file.sync_all()?;

            Ok(PidGuard {
                _file: file,
                path: pid_path.to_path_buf(),
                pid: our_pid,
            })
        }
        Err(_) => {
            // Lock failed - another process holds it
            // Read the PID to report in error
            let existing_pid = read_pid_from_file(&file).unwrap_or(0);

            // Double-check the process is actually running
            if existing_pid > 0 && is_process_running(existing_pid) {
                Err(DaemonError::AlreadyRunning { pid: existing_pid })
            } else {
                // Stale lock - this shouldn't normally happen since we check the lock
                // But the process might have just died. Report as stale.
                Err(DaemonError::StalePidFile { pid: existing_pid })
            }
        }
    }
}

/// Read PID from an already-open file
fn read_pid_from_file(file: &File) -> Option<u32> {
    let mut file = file;
    let mut content = String::new();

    // Seek to start before reading
    if file.seek(SeekFrom::Start(0)).is_err() {
        return None;
    }

    if file.read_to_string(&mut content).is_err() {
        return None;
    }

    content.trim().parse().ok()
}

// =============================================================================
// File Locking
// =============================================================================

/// Try to acquire an exclusive non-blocking lock on a file.
/// Cross-platform: uses flock on Unix, LockFileEx on Windows (via std).
fn try_lock_file(file: &File) -> Result<(), std::io::Error> {
    file.try_lock().map_err(|e| match e {
        std::fs::TryLockError::WouldBlock => {
            std::io::Error::new(std::io::ErrorKind::WouldBlock, "file is locked")
        }
        std::fs::TryLockError::Error(io_err) => io_err,
    })
}

// =============================================================================
// Stale Detection
// =============================================================================

/// Check if a PID file contains a stale PID (process no longer running).
///
/// Returns `true` if the file exists and contains a PID of a non-running process.
/// Returns `false` if file doesn't exist, is empty, or process is running.
pub fn check_stale_pid(pid_path: &Path) -> DaemonResult<bool> {
    // Try to read existing PID file
    let content = match std::fs::read_to_string(pid_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(DaemonError::Io(e)),
    };

    // Parse PID
    let pid: u32 = match content.trim().parse() {
        Ok(p) => p,
        Err(_) => return Ok(true), // Unparseable = stale
    };

    // Check if process is running
    Ok(!is_process_running(pid))
}

/// Clean up a stale PID file if it exists.
///
/// Only removes the file if it contains a PID of a non-running process.
/// This is safe to call even if the daemon is running - it will only
/// remove truly stale files.
pub fn cleanup_stale_pid(pid_path: &Path) -> DaemonResult<bool> {
    if check_stale_pid(pid_path)? {
        std::fs::remove_file(pid_path)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

// =============================================================================
// Tests
// =============================================================================

//! Daemon-specific error types
//!
//! Errors for daemon lifecycle, IPC, and cache operations.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Daemon-specific errors
#[derive(Debug, Error)]
pub enum DaemonError {
    /// Daemon is already running for this project
    #[error("daemon already running (PID: {pid})")]
    AlreadyRunning { pid: u32 },

    /// Daemon is not running for this project
    #[error("daemon not running")]
    NotRunning,

    /// Failed to acquire PID file lock
    #[error("failed to acquire PID file lock: {0}")]
    LockFailed(io::Error),

    /// Failed to bind to socket
    #[error("failed to bind socket: {0}")]
    SocketBindFailed(io::Error),

    /// Address/socket is already in use
    #[error("address already in use: {addr}")]
    AddressInUse { addr: String },

    /// Connection to daemon was refused
    #[error("connection refused")]
    ConnectionRefused,

    /// Connection to daemon timed out
    #[error("connection timeout after {timeout_secs}s")]
    ConnectionTimeout { timeout_secs: u64 },

    /// Invalid IPC message received
    #[error("invalid IPC message: {0}")]
    InvalidMessage(String),

    /// Unknown command received
    #[error("unknown command: {cmd}")]
    UnknownCommand { cmd: String },

    /// Required parameter is missing
    #[error("missing required parameter: {param}")]
    MissingParameter { param: String },

    /// Permission denied for path
    #[error("permission denied: {}", path.display())]
    PermissionDenied { path: PathBuf },

    /// PID file exists but process is not running
    #[error("stale PID file (process {pid} not running)")]
    StalePidFile { pid: u32 },

    /// Generic IO error
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    /// JSON serialization/deserialization error
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Authoritative artifact store could not be opened.
    #[error("artifact store initialization failed: {0}")]
    ArtifactStore(String),
}

/// Result type for daemon operations
pub type DaemonResult<T> = Result<T, DaemonError>;

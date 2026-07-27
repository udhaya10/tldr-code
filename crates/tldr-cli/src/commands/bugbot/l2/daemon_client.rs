//! Daemon client for L2Context -- routes IR queries through the daemon's
//! QueryCache when available, falling back to on-the-fly computation.
//!
//! # Architecture
//!
//! The daemon client trait abstracts communication with a running `tldr-daemon`
//! process. When a daemon is available, IR artifacts (call graphs, CFGs, DFGs,
//! SSA) are fetched from the daemon's persistent cache, avoiding redundant
//! recomputation across `bugbot check` invocations within the same edit session.
//!
//! When no daemon is running, the `NoDaemon` implementation returns `None` for
//! all queries, causing L2Context to fall back to on-the-fly IR construction
//! via its existing DashMap/OnceLock caches.
//!
//! # Cache Invalidation
//!
//! The daemon registers file dependencies for each cached artifact. When
//! `L2Context` is constructed with `changed_files`, it notifies the daemon
//! of those files so the daemon can invalidate stale cache entries before
//! any queries are made.
//!
//! # Tiered Execution
//!
//! - **Foreground tier** (<200ms, blocking): ScanEngine, DeltaEngine, GraphEngine
//! - **Deferred tier** (daemon-queued, returns within 2s): FlowEngine
//!
//! When a daemon is available, deferred engines use cached results from prior
//! daemon runs. When no daemon is running, deferred engines run synchronously
//! after foreground engines complete.

use std::path::{Path, PathBuf};

use tldr_core::ssa::SsaFunction;
use tldr_core::{CfgInfo, DfgInfo, ProjectCallGraph};

use super::types::FunctionId;

/// Trait for communicating with the tldr-daemon process.
///
/// Provides query methods for IR artifacts that the daemon may have cached
/// from prior analysis runs. All methods return `Option<T>` -- `None` means
/// the daemon does not have the artifact cached (or no daemon is running),
/// and the caller should compute it locally.
///
/// Implementations must be `Send + Sync` so they can be stored in `L2Context`
/// (which is shared across threads via `Arc` or moved to a background thread).
pub trait DaemonClient: Send + Sync {
    /// Check whether a daemon is currently running and reachable.
    ///
    /// Returns `true` if the daemon is available for queries, `false` otherwise.
    /// This is a lightweight check (e.g., socket existence) that does not
    /// perform a full handshake.
    fn is_available(&self) -> bool;

    /// Query the daemon for a cached project call graph.
    ///
    /// Returns `Some(graph)` if the daemon has a cached call graph for the
    /// project, `None` if unavailable or the daemon is not running.
    fn query_call_graph(&self) -> Option<ProjectCallGraph>;

    /// Query the daemon for a cached CFG for a specific function.
    ///
    /// Returns `Some(cfg)` if the daemon has a cached CFG for the given
    /// function, `None` if unavailable.
    fn query_cfg(&self, function_id: &FunctionId) -> Option<CfgInfo>;

    /// Query the daemon for a cached DFG for a specific function.
    ///
    /// Returns `Some(dfg)` if the daemon has a cached DFG for the given
    /// function, `None` if unavailable.
    fn query_dfg(&self, function_id: &FunctionId) -> Option<DfgInfo>;

    /// Query the daemon for cached SSA for a specific function.
    ///
    /// Returns `Some(ssa)` if the daemon has cached SSA for the given
    /// function, `None` if unavailable.
    fn query_ssa(&self, function_id: &FunctionId) -> Option<SsaFunction>;

    /// Notify the daemon that files have changed, triggering cache invalidation.
    ///
    /// The daemon will invalidate any cached artifacts whose file dependencies
    /// overlap with the provided `changed_files`. This should be called before
    /// any queries are made for the current analysis session.
    fn notify_changed_files(&self, changed_files: &[PathBuf]);
}

/// Fallback implementation used when no daemon is running.
///
/// Returns `None` for all queries and no-ops for notifications. This is the
/// default used by `L2Context` when daemon integration is not available.
pub struct NoDaemon;

impl DaemonClient for NoDaemon {
    fn is_available(&self) -> bool {
        false
    }

    fn query_call_graph(&self) -> Option<ProjectCallGraph> {
        None
    }

    fn query_cfg(&self, _function_id: &FunctionId) -> Option<CfgInfo> {
        None
    }

    fn query_dfg(&self, _function_id: &FunctionId) -> Option<DfgInfo> {
        None
    }

    fn query_ssa(&self, _function_id: &FunctionId) -> Option<SsaFunction> {
        None
    }

    fn notify_changed_files(&self, _changed_files: &[PathBuf]) {
        // No daemon running, nothing to invalidate.
    }
}

/// Daemon client that connects to a running local daemon via Unix socket.
///
/// Probes the daemon socket path computed from the project root. If the socket
/// exists and the daemon responds to a ping, queries are routed through the
/// daemon's HTTP API. Otherwise, behaves like `NoDaemon`.
///
/// This is a synchronous (blocking) client suitable for use from the L2 analysis
/// thread. The daemon's handlers use `spawn_blocking` internally, so the client
/// can safely make blocking HTTP calls.
pub struct LocalDaemonClient {
    /// Project root used to compute the daemon socket path.
    project: PathBuf,
    /// Cached socket path (computed once on construction).
    socket_path: PathBuf,
    /// Whether the daemon was reachable at construction time.
    available: bool,
}

impl LocalDaemonClient {
    /// Create a new `LocalDaemonClient` for the given project.
    ///
    /// Probes the daemon socket to determine availability. The probe is a
    /// non-blocking check of socket file existence (actual connectivity is
    /// verified lazily on first query).
    pub fn new(project: &Path) -> Self {
        let socket_path = Self::compute_socket_path(project);
        let available = socket_path.exists();
        Self {
            project: project.to_path_buf(),
            socket_path,
            available,
        }
    }

    /// Compute the daemon socket path for a project.
    ///
    /// Uses the same algorithm as `tldr-daemon`'s `server::compute_socket_path`:
    /// MD5 hash of canonicalized project path, first 8 hex chars.
    fn compute_socket_path(project: &Path) -> PathBuf {
        let canonical = dunce::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
        let path_str = canonical.to_string_lossy();
        let digest = md5::compute(path_str.as_bytes());
        let hash = format!("{:x}", digest);
        let hash_prefix = &hash[..8];

        let socket_dir = std::env::var("TLDR_SOCKET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());

        socket_dir.join(format!("tldr-{}-v1.0.sock", hash_prefix))
    }

    /// Get the project root path.
    pub fn project(&self) -> &Path {
        &self.project
    }

    /// Get the computed socket path.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl DaemonClient for LocalDaemonClient {
    fn is_available(&self) -> bool {
        self.available
    }

    fn query_call_graph(&self) -> Option<ProjectCallGraph> {
        if !self.available {
            return None;
        }
        // In the future, this will make an HTTP request to the daemon's
        // /calls endpoint via the Unix socket. For now, the daemon server
        // infrastructure exists but the client-side HTTP plumbing is not
        // wired. Return None to fall back to local computation.
        None
    }

    fn query_cfg(&self, _function_id: &FunctionId) -> Option<CfgInfo> {
        if !self.available {
            return None;
        }
        None
    }

    fn query_dfg(&self, _function_id: &FunctionId) -> Option<DfgInfo> {
        if !self.available {
            return None;
        }
        None
    }

    fn query_ssa(&self, _function_id: &FunctionId) -> Option<SsaFunction> {
        if !self.available {
            return None;
        }
        None
    }

    fn notify_changed_files(&self, _changed_files: &[PathBuf]) {
        if self.available {
            // In the future, this will POST to the daemon's invalidation endpoint.
            // For now, the notification is a no-op since the daemon does not yet
            // expose an invalidation API.
        }
    }
}

/// Create the appropriate daemon client for a project.
///
/// If a daemon socket exists for the project, returns a `LocalDaemonClient`.
/// Otherwise, returns a `NoDaemon` fallback. This factory function is the
/// primary entry point for daemon integration in the pipeline.
pub fn create_daemon_client(project: &Path) -> Box<dyn DaemonClient> {
    let client = LocalDaemonClient::new(project);
    if client.is_available() {
        Box::new(client)
    } else {
        Box::new(NoDaemon)
    }
}

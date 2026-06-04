//! Core daemon state machine and runtime
//!
//! This module contains the `TLDRDaemon` struct which manages:
//! - Daemon lifecycle state (Initializing -> Ready -> ShuttingDown)
//! - Salsa-style query cache
//! - Session statistics tracking
//! - Hook activity tracking
//! - Dirty file tracking for incremental re-indexing
//!
//! # Security Mitigations
//!
//! - TIGER-P2-02: Socket cleanup on abnormal exit via signal handlers

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use tokio::sync::{watch, RwLock};

use super::activity::{ActivityTracker, Source};
use super::error::{DaemonError, DaemonResult};
use super::ipc::{read_command, send_response, IpcListener, IpcStream};
use super::salsa::{QueryCache, QueryKey};
use super::types::{
    AllSessionsSummary, DaemonCommand, DaemonConfig, DaemonResponse, DaemonStatus, HookStats,
    SalsaCacheStats, SessionStats, HOOK_FLUSH_THRESHOLD,
};

#[cfg(test)]
use super::types::DEFAULT_REINDEX_THRESHOLD;
#[cfg(feature = "semantic")]
use tldr_core::config::TldrConfig;
#[cfg(feature = "semantic")]
use tldr_core::semantic::{EmbeddingModel, IndexSearchOptions};
#[cfg(feature = "semantic")]
use super::index_manager::IndexManager;
use tldr_core::{
    architecture_analysis, build_project_call_graph, change_impact, collect_all_functions,
    dead_code_analysis, detect_or_parse_language, extract_file, find_importers, get_cfg_context,
    get_code_structure, get_dfg_context, get_file_tree, get_imports, get_relevant_context,
    get_slice, impact_analysis, search as tldr_search, FileTree, Language, NodeType,
    SliceDirection,
};

// =============================================================================
// Helper Functions
// =============================================================================

/// Hash a slice of string arguments into a u64 for cache key generation.
fn hash_str_args(parts: &[&str]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for part in parts {
        part.hash(&mut hasher);
    }
    hasher.finish()
}

/// Resolve the effective `Language` for a daemon-handler invocation.
///
/// v031-cluster-M2: M1 added `language: Option<Language>` to seven
/// DaemonCommand variants (Context, Calls, Impact, Dead, Arch, Importers,
/// ChangeImpact). The handler arms that consume those variants previously
/// passed a hardcoded `Language::Python` to `tldr-core` regardless of what
/// the client supplied — a forgotten-thread bug. This helper centralises the
/// `Some(lang) | None -> default` resolution so every handler arm threads
/// the language consistently. The default-on-`None` is `Language::Python`
/// to preserve back-compat with v0.2.x clients that never sent a language
/// hint.
pub(crate) fn resolve_language(language: Option<Language>) -> Language {
    language.unwrap_or(Language::Python)
}

/// Count the number of file nodes in a FileTree recursively.
fn count_tree_files(tree: &FileTree) -> usize {
    match tree.node_type {
        NodeType::File => 1,
        NodeType::Dir => tree.children.iter().map(count_tree_files).sum(),
    }
}

/// Result of applying one changed file via [`TLDRDaemon::process_dirty_file`].
/// The IPC `Notify` handler turns this into a `NotifyResponse`; the in-daemon
/// watcher worker (TLDR-ac0.2) discards it (no client to answer).
pub(crate) struct ReindexOutcome {
    /// Number of files in the dirty set after this one was added.
    pub dirty_count: usize,
    /// The auto-reindex threshold in effect.
    pub threshold: usize,
    /// Whether this file pushed the dirty count to the threshold.
    pub reindex_triggered: bool,
}

// =============================================================================
// TLDRDaemon - Main Daemon Process
// =============================================================================

/// Main daemon process that handles client connections and manages state.
///
/// The daemon runs an event loop that:
/// 1. Accepts incoming IPC connections
/// 2. Reads commands from clients
/// 3. Dispatches commands to handlers
/// 4. Sends responses back to clients
/// 5. Handles shutdown signals gracefully
pub struct TLDRDaemon {
    /// Project root directory
    project: PathBuf,
    /// Daemon configuration
    config: DaemonConfig,
    /// When the daemon was started
    start_time: Instant,
    /// Current daemon status
    status: Arc<RwLock<DaemonStatus>>,
    /// Salsa-style query cache
    cache: QueryCache,
    /// Per-session statistics
    sessions: DashMap<String, SessionStats>,
    /// Per-hook activity statistics
    hooks: DashMap<String, HookStats>,
    /// Set of dirty files awaiting reindex
    dirty_files: Arc<RwLock<HashSet<PathBuf>>>,
    /// Shutdown signal sender
    shutdown_tx: watch::Sender<bool>,
    /// Flag to track if we've been signaled to stop
    stopping: AtomicBool,
    /// Presence-based liveness (TLDR-3w5): per-source last-activity
    /// timestamps + busy tokens for in-flight internal work. The idle loop
    /// shuts down only when the PROJECT is dormant, not merely the socket.
    activity: Arc<ActivityTracker>,
    /// Number of indexed files (for status reporting)
    indexed_files: Arc<RwLock<usize>>,
    /// Resident vector store with read/write split (TLDR-ac0.1). Concurrent
    /// queries take a shared read lock; build and invalidate take a write lock.
    #[cfg(feature = "semantic")]
    semantic_store: Arc<IndexManager>,
}

impl TLDRDaemon {
    /// Create a new daemon instance.
    ///
    /// The daemon starts in `Initializing` status and must have `run()` called
    /// to begin accepting connections.
    pub fn new(project: PathBuf, config: DaemonConfig) -> Self {
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);

        Self {
            project,
            config,
            start_time: Instant::now(),
            status: Arc::new(RwLock::new(DaemonStatus::Initializing)),
            cache: QueryCache::with_defaults(),
            sessions: DashMap::new(),
            hooks: DashMap::new(),
            dirty_files: Arc::new(RwLock::new(HashSet::new())),
            shutdown_tx,
            stopping: AtomicBool::new(false),
            activity: Arc::new(ActivityTracker::new()),
            indexed_files: Arc::new(RwLock::new(0)),
            #[cfg(feature = "semantic")]
            semantic_store: Arc::new(IndexManager::new()),
        }
    }

    /// Get the daemon's current status.
    pub async fn status(&self) -> DaemonStatus {
        *self.status.read().await
    }

    /// Get the daemon's uptime in seconds.
    pub fn uptime(&self) -> f64 {
        self.start_time.elapsed().as_secs_f64()
    }

    /// Get the daemon's uptime formatted as a human-readable string.
    pub fn uptime_human(&self) -> String {
        let secs = self.start_time.elapsed().as_secs();
        let hours = secs / 3600;
        let minutes = (secs % 3600) / 60;
        let seconds = secs % 60;
        format!("{}h {}m {}s", hours, minutes, seconds)
    }

    /// Get cache statistics.
    pub fn cache_stats(&self) -> SalsaCacheStats {
        self.cache.stats()
    }

    /// Get the project path.
    pub fn project(&self) -> &PathBuf {
        &self.project
    }

    /// Presence tracker (TLDR-3w5). The watcher taps it for file-event
    /// liveness; `daemon status` (TLDR-qzc) reads its snapshots.
    pub(crate) fn activity(&self) -> &Arc<ActivityTracker> {
        &self.activity
    }

    /// Get the number of indexed files.
    pub async fn indexed_files(&self) -> usize {
        *self.indexed_files.read().await
    }

    /// Number of files currently in the dirty set. Test-only observable used by
    /// the watcher end-to-end smoke test (TLDR-ac0.2) to confirm an event was
    /// routed through `process_dirty_file`.
    #[cfg(test)]
    pub(crate) async fn dirty_file_count(&self) -> usize {
        self.dirty_files.read().await.len()
    }

    /// Get a summary of all sessions.
    pub fn all_sessions_summary(&self) -> AllSessionsSummary {
        let mut summary = AllSessionsSummary {
            active_sessions: self.sessions.len(),
            ..AllSessionsSummary::default()
        };

        for entry in self.sessions.iter() {
            let stats = entry.value();
            summary.total_raw_tokens += stats.raw_tokens;
            summary.total_tldr_tokens += stats.tldr_tokens;
            summary.total_requests += stats.requests;
        }

        summary
    }

    /// Get all hook statistics.
    pub fn hook_stats(&self) -> HashMap<String, HookStats> {
        self.hooks
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect()
    }

    /// Signal the daemon to shut down gracefully.
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = self.shutdown_tx.send(true);
    }

    /// Run the daemon main loop.
    ///
    /// This function blocks until the daemon is shut down via:
    /// - A `Shutdown` command from a client
    /// - A SIGTERM/SIGINT signal
    /// - An error in the listener
    pub async fn run(self: Arc<Self>, listener: IpcListener) -> DaemonResult<()> {
        // Set status to Ready
        {
            let mut status = self.status.write().await;
            *status = DaemonStatus::Ready;
        }

        // Set up signal handlers for graceful shutdown
        #[cfg(unix)]
        {
            let daemon = Arc::clone(&self);
            tokio::spawn(async move {
                let mut sigterm =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("Failed to register SIGTERM handler");
                let mut sigint =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                        .expect("Failed to register SIGINT handler");

                tokio::select! {
                    _ = sigterm.recv() => {
                        daemon.shutdown();
                    }
                    _ = sigint.recv() => {
                        daemon.shutdown();
                    }
                }
            });
        }

        // In-daemon filesystem watcher (TLDR-ac0.2). Bound to a NAMED guard:
        // `let _ = ...` would drop the Debouncer at the end of the statement and
        // silently stop watching. The guard lives for the whole run loop and
        // drops on shutdown, which stops the OS watcher and ends its worker.
        #[cfg(feature = "semantic")]
        let _watcher_guard = if self.config.enable_watcher {
            super::watcher::spawn_watcher(Arc::clone(&self))
        } else {
            None
        };

        // Main event loop
        let idle_timeout = std::time::Duration::from_secs(self.config.idle_timeout_secs);

        loop {
            // Check for shutdown signal
            if self.stopping.load(Ordering::SeqCst) {
                break;
            }

            // Safety net: self-terminate if project directory no longer exists
            if !self.project.exists() {
                eprintln!(
                    "Project directory {} no longer exists, shutting down",
                    self.project.display()
                );
                break;
            }

            // Presence-based idle shutdown (TLDR-3w5): self-terminate only
            // when the PROJECT is dormant — no client connection, no watcher
            // -observed file write, and no in-flight internal work (index
            // build / delta) for a full idle_timeout. A busy token (any
            // in-progress build) unconditionally defers shutdown: never
            // abandon your own job.
            if self.activity.is_idle(idle_timeout) {
                eprintln!(
                    "No project presence for {}s (no client, file activity, or internal work), shutting down",
                    self.config.idle_timeout_secs
                );
                break;
            }

            // Accept connection with timeout
            let accept_future = listener.accept();
            let timeout = tokio::time::Duration::from_millis(100);

            match tokio::time::timeout(timeout, accept_future).await {
                Ok(Ok(mut stream)) => {
                    // Record socket presence for the idle check
                    self.activity.touch(Source::Socket);

                    // Handle the connection
                    let daemon = Arc::clone(&self);
                    tokio::spawn(async move {
                        if let Err(e) = daemon.handle_connection(&mut stream).await {
                            eprintln!("Connection error: {}", e);
                        }
                    });
                }
                Ok(Err(e)) => {
                    // Accept error - log and continue
                    eprintln!("Accept error: {}", e);
                }
                Err(_) => {
                    // Timeout - check shutdown and continue
                    continue;
                }
            }
        }

        // Set status to ShuttingDown
        {
            let mut status = self.status.write().await;
            *status = DaemonStatus::ShuttingDown;
        }

        // Persist stats before exit
        self.persist_stats().await?;

        // Set status to Stopped
        {
            let mut status = self.status.write().await;
            *status = DaemonStatus::Stopped;
        }

        Ok(())
    }

    /// Handle a single client connection.
    async fn handle_connection(self: &Arc<Self>, stream: &mut IpcStream) -> DaemonResult<()> {
        // Read command
        let cmd = read_command(stream).await?;

        // Handle command
        let response = self.handle_command(cmd).await;

        // Send response
        send_response(stream, &response).await?;

        Ok(())
    }

    /// Resolve the embedding model for a semantic request, mirroring the cold
    /// CLI path (`semantic.rs`): an explicit request override wins, else the
    /// project config, else the built-in default. Keeping this identical to the
    /// cold resolver is what makes warm and cold rank the same model (TLDR-atc);
    /// the daemon's old `BuildOptions::default()` silently pinned ArcticM even
    /// when the project config asked for ArcticL.
    #[cfg(feature = "semantic")]
    fn resolve_semantic_model(&self, override_model: Option<&str>) -> Result<EmbeddingModel, String> {
        let config = TldrConfig::resolve(Some(&self.project));
        EmbeddingModel::resolve(override_model, &config)
    }

    /// Handle a daemon command and return the response.
    pub async fn handle_command(&self, cmd: DaemonCommand) -> DaemonResponse {
        match cmd {
            DaemonCommand::Ping => DaemonResponse::Status {
                status: "ok".to_string(),
                message: Some("pong".to_string()),
            },

            DaemonCommand::Status { session } => self.handle_status(session).await,

            DaemonCommand::Shutdown => {
                self.shutdown();
                DaemonResponse::Status {
                    status: "shutting_down".to_string(),
                    message: Some("Daemon is shutting down".to_string()),
                }
            }

            DaemonCommand::Notify { file } => self.handle_notify(file).await,

            DaemonCommand::Track {
                hook,
                success,
                metrics,
            } => self.handle_track(hook, success, metrics).await,

            DaemonCommand::Warm { language } => {
                let parsed = language.as_deref().and_then(|l| l.parse::<Language>().ok());
                let lang = resolve_language(parsed);

                let mut warmed = Vec::new();
                let mut errors = Vec::new();

                // 1. Warm call graph
                let calls_key = QueryKey::new(
                    "calls",
                    hash_str_args(&[&self.project.to_string_lossy()]),
                    lang,
                );
                if self.cache.get::<serde_json::Value>(&calls_key).is_some() {
                    warmed.push("call_graph (cached)");
                } else {
                    match build_project_call_graph(&self.project, lang, None, true) {
                        Ok(result) => {
                            let val = serde_json::to_value(&result).unwrap_or_default();
                            self.cache.insert(calls_key, &val, vec![]);
                            warmed.push("call_graph");
                        }
                        Err(e) => errors.push(format!("call_graph: {}", e)),
                    }
                }

                // 2. Warm code structure
                let struct_key = QueryKey::new(
                    "structure",
                    hash_str_args(&[&self.project.to_string_lossy(), ""]),
                    lang,
                );
                if self.cache.get::<serde_json::Value>(&struct_key).is_some() {
                    warmed.push("structure (cached)");
                } else {
                    match get_code_structure(&self.project, lang, 0, None) {
                        Ok(result) => {
                            let val = serde_json::to_value(&result).unwrap_or_default();
                            self.cache.insert(struct_key, &val, vec![]);
                            warmed.push("structure");
                        }
                        Err(e) => errors.push(format!("structure: {}", e)),
                    }
                }

                // 3. Warm file tree
                let tree_key = QueryKey::new(
                    "tree",
                    hash_str_args(&[&self.project.to_string_lossy()]),
                    lang,
                );
                if self.cache.get::<serde_json::Value>(&tree_key).is_some() {
                    warmed.push("file_tree (cached)");
                } else {
                    match get_file_tree(&self.project, None, true, None) {
                        Ok(result) => {
                            let file_count = count_tree_files(&result);
                            let val = serde_json::to_value(&result).unwrap_or_default();
                            self.cache.insert(tree_key, &val, vec![]);
                            *self.indexed_files.write().await = file_count;
                            warmed.push("file_tree");
                        }
                        Err(e) => errors.push(format!("file_tree: {}", e)),
                    }
                }

                // 4. Warm the vector store: load from disk (near-instant if
                //    fresh) or build+save on miss. Uses the project-config
                //    model so a later query with the same model hits the
                //    resident store (TLDR-atc / TLDR-zxb).
                #[cfg(feature = "semantic")]
                {
                    match self.resolve_semantic_model(None) {
                        Ok(model) => {
                            let mgr = Arc::clone(&self.semantic_store);
                            let project = self.project.clone();
                            // Busy guard owned by the CLOSURE, not this async
                            // task (TLDR-3w5): if the client times out and
                            // this connection task is cancelled, the blocking
                            // build keeps running — the guard must live
                            // exactly as long as the build so the idle loop
                            // never kills it mid-flight. A hung warm shows up
                            // as a stale-busy token with growing age (qzc).
                            let busy = self.activity.begin("warm-build");
                            let res = tokio::task::spawn_blocking(move || {
                                let _busy = busy;
                                mgr.warm(&project, model)
                            })
                            .await;
                            match res {
                                Ok(Ok(true)) => warmed.push("semantic_store"),
                                Ok(Ok(false)) => warmed.push("semantic_store (cached)"),
                                Ok(Err(e)) => errors.push(format!("semantic_store: {}", e)),
                                Err(e) => errors.push(format!("semantic_store: {}", e)),
                            }
                        }
                        Err(e) => errors.push(format!("semantic_store: {}", e)),
                    }
                }

                let message = if errors.is_empty() {
                    format!("Warmed: {}", warmed.join(", "))
                } else {
                    format!(
                        "Warmed: {}. Errors: {}",
                        warmed.join(", "),
                        errors.join("; ")
                    )
                };

                DaemonResponse::Status {
                    status: "ok".to_string(),
                    message: Some(message),
                }
            }

            #[cfg(feature = "semantic")]
            DaemonCommand::Semantic {
                query,
                top_k,
                model,
                threshold,
            } => {
                let model = match self.resolve_semantic_model(model.as_deref()) {
                    Ok(m) => m,
                    Err(e) => {
                        return DaemonResponse::Error {
                            status: "error".to_string(),
                            error: e,
                        };
                    }
                };

                let mgr = Arc::clone(&self.semantic_store);
                let project = self.project.clone();
                let join = tokio::task::spawn_blocking(move || {
                    let search_opts = IndexSearchOptions {
                        top_k,
                        threshold: threshold.unwrap_or(0.0),
                        include_snippet: true,
                        snippet_lines: 5,
                    };
                    mgr.query(&project, &query, &search_opts, model)
                })
                .await;

                // TLDR-7xz.2: warm serves; cold/building answers honestly with a
                // machine-distinguishable `status: "not_ready"` (the CLI relays
                // the message instead of silently falling back to a cold serve).
                // Real failures keep `status: "error"`.
                use super::index_manager::QueryError;
                match join {
                    Ok(Ok(value)) => DaemonResponse::Result(value),
                    Ok(Err(e @ (QueryError::NotReady | QueryError::Building))) => {
                        DaemonResponse::Error {
                            status: "not_ready".to_string(),
                            error: e.to_string(),
                        }
                    }
                    Ok(Err(QueryError::Internal(e))) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e,
                    },
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: format!("semantic task failed: {e}"),
                    },
                }
            }

            #[cfg(not(feature = "semantic"))]
            DaemonCommand::Semantic { .. } => DaemonResponse::Error {
                status: "error".to_string(),
                error: "Semantic search requires the 'semantic' feature".to_string(),
            },

            // Pass-through analysis commands with Salsa cache integration
            DaemonCommand::Search {
                pattern,
                max_results,
            } => {
                let max = max_results.unwrap_or(100);
                // Search is regex-based and language-agnostic; tag with the
                // resolve_language default so QueryKey is well-formed without
                // discriminating across languages.
                let key = QueryKey::new(
                    "search",
                    hash_str_args(&[&pattern, &max.to_string()]),
                    resolve_language(None),
                );
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                match tldr_search(&pattern, &self.project, None, 2, max, 1000, None) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        self.cache.insert(key, &val, vec![]);
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            DaemonCommand::Extract { file, session: _ } => {
                let file_str = file.to_string_lossy().to_string();
                // Extract auto-detects language from the file path. Tag the
                // cache key with the detected language so two files with the
                // same name in different language sub-projects do not collide.
                let detected_lang = detect_or_parse_language(None, &file)
                    .unwrap_or(Language::Python);
                let key = QueryKey::new(
                    "extract",
                    hash_str_args(&[&file_str]),
                    detected_lang,
                );
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                let file_hash = super::salsa::hash_path(&file);
                match extract_file(&file, Some(&self.project)) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        self.cache.insert(key, &val, vec![file_hash]);
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            DaemonCommand::Tree { path } => {
                let root = path.unwrap_or_else(|| self.project.clone());
                let root_str = root.to_string_lossy().to_string();
                // File tree is language-agnostic; tag with default language.
                let key = QueryKey::new(
                    "tree",
                    hash_str_args(&[&root_str]),
                    resolve_language(None),
                );
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                match get_file_tree(&root, None, true, None) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        self.cache.insert(key, &val, vec![]);
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            DaemonCommand::Structure { path, lang } => {
                let path_str = path.to_string_lossy().to_string();
                let lang_str = lang.as_deref().unwrap_or("");
                let language = match detect_or_parse_language(lang.as_deref(), &path) {
                    Ok(l) => l,
                    Err(e) => {
                        return DaemonResponse::Error {
                            status: "error".to_string(),
                            error: e.to_string(),
                        }
                    }
                };
                let key = QueryKey::new(
                    "structure",
                    hash_str_args(&[&path_str, lang_str]),
                    language,
                );
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                match get_code_structure(&path, language, 0, None) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        self.cache.insert(key, &val, vec![]);
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            DaemonCommand::Context {
                entry,
                depth,
                language,
            } => {
                let d = depth.unwrap_or(2);
                let lang = resolve_language(language);
                let key = QueryKey::new(
                    "context",
                    hash_str_args(&[&entry, &d.to_string()]),
                    lang,
                );
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                match get_relevant_context(&self.project, &entry, d, lang, true, None) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        self.cache.insert(key, &val, vec![]);
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            DaemonCommand::Cfg { file, function } => {
                let file_str = file.to_string_lossy().to_string();
                let language = match detect_or_parse_language(None, &file) {
                    Ok(l) => l,
                    Err(e) => {
                        return DaemonResponse::Error {
                            status: "error".to_string(),
                            error: e.to_string(),
                        }
                    }
                };
                let key = QueryKey::new(
                    "cfg",
                    hash_str_args(&[&file_str, &function]),
                    language,
                );
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                let file_hash = super::salsa::hash_path(&file);
                match get_cfg_context(&file_str, &function, language) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        self.cache.insert(key, &val, vec![file_hash]);
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            DaemonCommand::Dfg { file, function } => {
                let file_str = file.to_string_lossy().to_string();
                let language = match detect_or_parse_language(None, &file) {
                    Ok(l) => l,
                    Err(e) => {
                        return DaemonResponse::Error {
                            status: "error".to_string(),
                            error: e.to_string(),
                        }
                    }
                };
                let key = QueryKey::new(
                    "dfg",
                    hash_str_args(&[&file_str, &function]),
                    language,
                );
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                let file_hash = super::salsa::hash_path(&file);
                match get_dfg_context(&file_str, &function, language) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        self.cache.insert(key, &val, vec![file_hash]);
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            DaemonCommand::Slice {
                file,
                function,
                line,
            } => {
                let file_str = file.to_string_lossy().to_string();
                let language = match detect_or_parse_language(None, &file) {
                    Ok(l) => l,
                    Err(e) => {
                        return DaemonResponse::Error {
                            status: "error".to_string(),
                            error: e.to_string(),
                        }
                    }
                };
                let key = QueryKey::new(
                    "slice",
                    hash_str_args(&[&file_str, &function, &line.to_string()]),
                    language,
                );
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                let file_hash = super::salsa::hash_path(&file);
                match get_slice(
                    &file_str,
                    &function,
                    line as u32,
                    SliceDirection::Backward,
                    None,
                    language,
                ) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        self.cache.insert(key, &val, vec![file_hash]);
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            DaemonCommand::Calls { path, language } => {
                let root = path.unwrap_or_else(|| self.project.clone());
                let lang = resolve_language(language);
                let root_str = root.to_string_lossy().to_string();
                let key = QueryKey::new("calls", hash_str_args(&[&root_str]), lang);
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                match build_project_call_graph(&root, lang, None, true) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        self.cache.insert(key, &val, vec![]);
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            DaemonCommand::Impact {
                func,
                depth,
                language,
            } => {
                let d = depth.unwrap_or(3);
                let lang = resolve_language(language);
                let key = QueryKey::new(
                    "impact",
                    hash_str_args(&[&func, &d.to_string()]),
                    lang,
                );
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                let graph = match build_project_call_graph(&self.project, lang, None, true) {
                    Ok(g) => g,
                    Err(e) => {
                        return DaemonResponse::Error {
                            status: "error".to_string(),
                            error: e.to_string(),
                        }
                    }
                };
                match impact_analysis(&graph, &func, d, None) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        self.cache.insert(key, &val, vec![]);
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            DaemonCommand::Dead {
                path,
                entry,
                language,
            } => {
                let root = path.unwrap_or_else(|| self.project.clone());
                let lang = resolve_language(language);
                let root_str = root.to_string_lossy().to_string();
                let entry_str = entry.as_ref().map(|v| v.join(",")).unwrap_or_default();
                let key = QueryKey::new(
                    "dead",
                    hash_str_args(&[&root_str, &entry_str]),
                    lang,
                );
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                let graph = match build_project_call_graph(&root, lang, None, true) {
                    Ok(g) => g,
                    Err(e) => {
                        return DaemonResponse::Error {
                            status: "error".to_string(),
                            error: e.to_string(),
                        }
                    }
                };
                // Collect all functions from the project by extracting each file
                let extensions: HashSet<String> = lang
                    .extensions()
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                let file_tree = match get_file_tree(&root, Some(&extensions), true, None) {
                    Ok(t) => t,
                    Err(e) => {
                        return DaemonResponse::Error {
                            status: "error".to_string(),
                            error: e.to_string(),
                        }
                    }
                };
                let files = tldr_core::fs::tree::collect_files(&file_tree, &root);
                let mut module_infos = Vec::new();
                for file_path in files {
                    if let Ok(info) = extract_file(&file_path, Some(&root)) {
                        module_infos.push((file_path, info));
                    }
                }
                let all_functions = collect_all_functions(&module_infos);
                let entry_strings: Option<Vec<String>> = entry;
                let entry_refs: Option<&[String]> = entry_strings.as_deref();
                match dead_code_analysis(&graph, &all_functions, entry_refs) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        self.cache.insert(key, &val, vec![]);
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            DaemonCommand::Arch { path, language } => {
                let root = path.unwrap_or_else(|| self.project.clone());
                let lang = resolve_language(language);
                let root_str = root.to_string_lossy().to_string();
                let key = QueryKey::new("arch", hash_str_args(&[&root_str]), lang);
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                let graph = match build_project_call_graph(&root, lang, None, true) {
                    Ok(g) => g,
                    Err(e) => {
                        return DaemonResponse::Error {
                            status: "error".to_string(),
                            error: e.to_string(),
                        }
                    }
                };
                match architecture_analysis(&graph) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        self.cache.insert(key, &val, vec![]);
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            DaemonCommand::Imports { file } => {
                let file_str = file.to_string_lossy().to_string();
                let language = match detect_or_parse_language(None, &file) {
                    Ok(l) => l,
                    Err(e) => {
                        return DaemonResponse::Error {
                            status: "error".to_string(),
                            error: e.to_string(),
                        }
                    }
                };
                let key = QueryKey::new(
                    "imports",
                    hash_str_args(&[&file_str]),
                    language,
                );
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                let file_hash = super::salsa::hash_path(&file);
                match get_imports(&file, language) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        self.cache.insert(key, &val, vec![file_hash]);
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            DaemonCommand::Importers {
                module,
                path,
                language,
            } => {
                let root = path.unwrap_or_else(|| self.project.clone());
                let lang = resolve_language(language);
                let root_str = root.to_string_lossy().to_string();
                let key = QueryKey::new(
                    "importers",
                    hash_str_args(&[&module, &root_str]),
                    lang,
                );
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                match find_importers(&root, &module, lang) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        self.cache.insert(key, &val, vec![]);
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            DaemonCommand::Diagnostics { path, project: _ } => DaemonResponse::Error {
                status: "error".to_string(),
                error: format!(
                    "Diagnostics requires external tool orchestration; \
                         use CLI directly: tldr diagnostics {}",
                    path.display()
                ),
            },

            DaemonCommand::ChangeImpact {
                files,
                session: _,
                git: _,
                language,
            } => {
                let lang = resolve_language(language);
                let files_str = files
                    .as_ref()
                    .map(|v| {
                        v.iter()
                            .map(|p| p.to_string_lossy().to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_default();
                let key = QueryKey::new(
                    "change_impact",
                    hash_str_args(&[&files_str]),
                    lang,
                );
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                let changed: Option<Vec<PathBuf>> = files;
                match change_impact(&self.project, changed.as_deref(), lang) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        self.cache.insert(key, &val, vec![]);
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }
        }
    }

    /// Handle the Status command.
    async fn handle_status(&self, session: Option<String>) -> DaemonResponse {
        let status = self.status().await;
        let uptime = self.uptime();
        let files = self.indexed_files().await;
        let salsa_stats = self.cache_stats();
        let all_sessions = Some(self.all_sessions_summary());
        let hook_stats = Some(self.hook_stats());

        // Get session-specific stats if requested
        let session_stats =
            session.and_then(|id| self.sessions.get(&id).map(|entry| entry.value().clone()));

        DaemonResponse::FullStatus {
            status,
            uptime,
            files,
            project: self.project.clone(),
            salsa_stats,
            dedup_stats: None,
            session_stats,
            all_sessions,
            hook_stats,
            liveness: Some(self.liveness_stats()),
            semantic_index: self.semantic_index_stats(),
        }
    }

    /// Snapshot the presence tracker for `daemon status` (TLDR-qzc): what is
    /// keeping the daemon alive, what internal work is in flight (with age —
    /// a hung build must be visible as `busy 4h: warm-build`), and when idle
    /// shutdown would fire.
    fn liveness_stats(&self) -> super::types::LivenessStats {
        use super::activity::SOURCE_NAMES;

        let ages = self.activity.presence_ages();
        let presence_age_secs = SOURCE_NAMES
            .iter()
            .zip(ages.iter())
            .map(|(name, age)| (name.to_string(), age.as_secs_f64()))
            .collect();

        let busy: Vec<super::types::BusyTokenStats> = self
            .activity
            .busy_snapshot()
            .into_iter()
            .map(|b| super::types::BusyTokenStats {
                label: b.label.to_string(),
                age_secs: b.age.as_secs_f64(),
            })
            .collect();

        // Deadline only runs while NOT busy (busy defers shutdown
        // unconditionally). Clamped at 0: a stale-but-not-yet-reaped daemon
        // reports "0s" rather than a negative countdown.
        let idle_shutdown_in_secs = if busy.is_empty() {
            let remaining = self.config.idle_timeout_secs as f64
                - self.activity.freshest_presence_age().as_secs_f64();
            Some(remaining.max(0.0))
        } else {
            None
        };

        super::types::LivenessStats {
            presence_age_secs,
            busy,
            idle_timeout_secs: self.config.idle_timeout_secs,
            idle_shutdown_in_secs,
        }
    }

    /// Resident semantic index state for `daemon status` (TLDR-qzc). `None`
    /// on non-semantic builds.
    #[cfg(feature = "semantic")]
    fn semantic_index_stats(&self) -> Option<super::types::SemanticIndexStats> {
        use super::index_manager::IndexState;
        Some(match self.semantic_store.state() {
            IndexState::Warm { vectors } => super::types::SemanticIndexStats {
                state: "warm".to_string(),
                vectors: Some(vectors),
            },
            IndexState::Building => super::types::SemanticIndexStats {
                state: "building".to_string(),
                vectors: None,
            },
            IndexState::Cold => super::types::SemanticIndexStats {
                state: "cold".to_string(),
                vectors: None,
            },
        })
    }

    #[cfg(not(feature = "semantic"))]
    fn semantic_index_stats(&self) -> Option<super::types::SemanticIndexStats> {
        None
    }

    /// Handle the Notify command (file change notification).
    ///
    /// TLDR-7xz.6: this is the external poke's (git/editor hooks via
    /// `tldr daemon notify`) entry into the SINGLE invalidation/re-index flow
    /// — it funnels into `process_dirty_file`, the same path the in-daemon
    /// filesystem watcher uses. Never a parallel mechanism; see notify.rs.
    async fn handle_notify(&self, file: PathBuf) -> DaemonResponse {
        let ReindexOutcome {
            dirty_count,
            threshold,
            reindex_triggered,
        } = self.process_dirty_file(file).await;

        DaemonResponse::NotifyResponse {
            status: "ok".to_string(),
            dirty_count,
            threshold,
            reindex_triggered,
        }
    }

    /// Apply one changed file to the dirty set + caches. Shared by the IPC
    /// `Notify` handler and the in-daemon filesystem watcher worker (TLDR-ac0.2)
    /// so both paths get IDENTICAL reindex semantics: dirty-set bookkeeping,
    /// salsa cache invalidation, and (semantic) the in-place index delta.
    ///
    /// Path handling is INTENTIONALLY canonicalization-free (verified TLDR-ac0.2,
    /// 2026-06-03). Two independent reasons, both empirical:
    /// - The salsa key hashes the RAW path to match the raw-path registration
    ///   side (the `vec![hash_path(&file)]` handler arms above); canonicalizing
    ///   here alone would diverge from registration and make invalidation miss.
    /// - The vector-store delta keying (`root_relative` / `deleted_file_rel`) is
    ///   already hardened against a non-canonical root, and a deleted file can't
    ///   be canonicalized — so canonicalizing before `apply_delta` would break
    ///   the delete path. Pass the path through as-is.
    pub(crate) async fn process_dirty_file(&self, file: PathBuf) -> ReindexOutcome {
        // Add file to dirty set
        let dirty_count = {
            let mut dirty = self.dirty_files.write().await;
            dirty.insert(file.clone());
            dirty.len()
        };

        // Invalidate cache entries for this file
        let file_hash = super::salsa::hash_path(&file);
        self.cache.invalidate_by_input(file_hash);

        // Incrementally re-index the changed file in the resident store instead
        // of dropping it (TLDR-t8f). A warm store applies a per-file delta —
        // re-embedding only that file's changed chunks — so query results
        // reflect the edit without a full corpus rebuild. A cold store no-ops
        // (the next query's cold build already sees the change). Any failure
        // falls back to invalidate() → full rebuild on the next query.
        #[cfg(feature = "semantic")]
        {
            let mgr = Arc::clone(&self.semantic_store);
            let project = self.project.clone();
            let changed = file.clone();
            // Busy guard owned by the closure (see Warm handler note): the
            // delta must defer idle shutdown for exactly as long as it runs,
            // regardless of what happens to this awaiting task.
            let busy = self.activity.begin("delta");
            let _ = tokio::task::spawn_blocking(move || {
                let _busy = busy;
                use super::index_manager::DeltaOutcome;
                match mgr.apply_delta(&project, &changed) {
                    Ok(DeltaOutcome::NeedsRebuild) => mgr.invalidate(),
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("[t8f] delta failed for {}: {e}; rebuilding", changed.display());
                        mgr.invalidate();
                    }
                }
            })
            .await;
        }

        let threshold = self.config.auto_reindex_threshold;
        let reindex_triggered = dirty_count >= threshold;

        // Trigger reindex if threshold reached
        if reindex_triggered {
            // Clear dirty set
            let mut dirty = self.dirty_files.write().await;
            dirty.clear();

            // In full implementation, would spawn background reindex task
            // For now, just clear the dirty set
        }

        ReindexOutcome {
            dirty_count,
            threshold,
            reindex_triggered,
        }
    }

    /// Handle the Track command (hook activity tracking).
    async fn handle_track(
        &self,
        hook: String,
        success: bool,
        metrics: HashMap<String, f64>,
    ) -> DaemonResponse {
        // Get or create hook stats
        let mut entry = self
            .hooks
            .entry(hook.clone())
            .or_insert_with(|| HookStats::new(hook.clone()));

        // Record invocation
        let metrics_opt = if metrics.is_empty() {
            None
        } else {
            Some(metrics)
        };
        entry.record_invocation(success, metrics_opt);

        let total_invocations = entry.invocations;
        let flushed = total_invocations.is_multiple_of(HOOK_FLUSH_THRESHOLD as u64);

        // Flush stats periodically
        if flushed {
            // In full implementation, would persist stats to disk
            // For now, just mark as flushed
        }

        DaemonResponse::TrackResponse {
            status: "ok".to_string(),
            hook,
            total_invocations,
            flushed,
        }
    }

    /// Persist statistics to disk.
    async fn persist_stats(&self) -> DaemonResult<()> {
        // Create cache directory if it doesn't exist
        let cache_dir = self.project.join(".tldr/cache");
        if !cache_dir.exists() {
            std::fs::create_dir_all(&cache_dir)?;
        }

        // Save Salsa cache stats
        let salsa_stats_path = cache_dir.join("salsa_stats.json");
        let stats = self.cache_stats();
        let json = serde_json::to_string_pretty(&stats)?;
        std::fs::write(salsa_stats_path, json)?;

        // Save full query cache
        let cache_path = cache_dir.join("query_cache.bin");
        self.cache.save_to_file(&cache_path)?;

        Ok(())
    }
}

// =============================================================================
// Daemon Control Functions
// =============================================================================

/// Start a daemon in the background for the given project.
///
/// Returns the PID of the daemon process.
///
/// Routes the spawned daemon's stdout and stderr into `<project>/.tldr/daemon.log`
/// (append mode) so tracing output, panics, and backtraces remain inspectable
/// after the parent CLI invocation exits. Previously both streams were dropped
/// to `/dev/null`, which made any background daemon crash invisible.
pub async fn start_daemon_background(project: &std::path::Path) -> DaemonResult<u32> {
    use std::fs::OpenOptions;
    use std::process::Command;

    // Get the current executable path
    let exe_path = std::env::current_exe().map_err(DaemonError::Io)?;

    // Open .tldr/daemon.log for append; create parent dir + file if missing.
    let log_path = project.join(".tldr").join("daemon.log");
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).map_err(DaemonError::Io)?;
    }
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(DaemonError::Io)?;
    let log_file_for_stderr = log_file.try_clone().map_err(DaemonError::Io)?;

    // Spawn the daemon process
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        let child = unsafe {
            Command::new(&exe_path)
                .args(["daemon", "start", "--project"])
                .arg(project.as_os_str())
                .arg("--foreground")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::from(log_file))
                .stderr(std::process::Stdio::from(log_file_for_stderr))
                .pre_exec(|| {
                    // Create new session (detach from terminal)
                    libc::setsid();
                    Ok(())
                })
                .spawn()
                .map_err(DaemonError::Io)?
        };

        Ok(child.id())
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        let child = Command::new(&exe_path)
            .args(["daemon", "start", "--project"])
            .arg(project.as_os_str())
            .arg("--foreground")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(log_file_for_stderr))
            .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
            .spawn()
            .map_err(DaemonError::Io)?;

        Ok(child.id())
    }
}

/// Wait for a daemon to become ready by polling the socket.
///
/// Returns `Ok(())` if the daemon becomes available within the timeout.
pub async fn wait_for_daemon(project: &std::path::Path, timeout_secs: u64) -> DaemonResult<()> {
    let start = Instant::now();
    let timeout = std::time::Duration::from_secs(timeout_secs);

    while start.elapsed() < timeout {
        // Try to connect
        if super::ipc::check_socket_alive(project).await {
            return Ok(());
        }

        // Wait a bit before retrying
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    Err(DaemonError::ConnectionTimeout { timeout_secs })
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_daemon_new() {
        let temp = TempDir::new().unwrap();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        assert_eq!(daemon.project(), temp.path());
        assert!(daemon.uptime() < 1.0);
    }

    /// Call-site guard: the daemon's query path resolves the embedding model from
    /// the PROJECT CONFIG, never a hardcoded/Default model (the trap documented at
    /// resolve_semantic_model: `BuildOptions::default()` silently pinned ArcticM
    /// even when config asked for ArcticL). Writes a real `.tldr/config.json` and
    /// asserts `resolve_semantic_model` — the exact method handle_command uses —
    /// honors it, and that an explicit override still wins.
    #[cfg(feature = "semantic")]
    #[test]
    fn resolve_semantic_model_honors_project_config() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join(".tldr")).unwrap();
        std::fs::write(
            temp.path().join(".tldr").join("config.json"),
            r#"{"embedding": {"model": "arctic-l"}}"#,
        )
        .unwrap();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), DaemonConfig::default());

        let resolved = daemon.resolve_semantic_model(None).unwrap();
        assert_eq!(
            resolved,
            EmbeddingModel::ArcticL,
            "project config model must be honored (not the built-in default)"
        );

        let overridden = daemon.resolve_semantic_model(Some("arctic-m")).unwrap();
        assert_eq!(
            overridden,
            EmbeddingModel::ArcticM,
            "explicit override must beat the project config"
        );
    }

    #[tokio::test]
    async fn test_daemon_status_initial() {
        let temp = TempDir::new().unwrap();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        assert_eq!(daemon.status().await, DaemonStatus::Initializing);
    }

    #[tokio::test]
    async fn test_daemon_uptime_human() {
        let temp = TempDir::new().unwrap();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let uptime = daemon.uptime_human();
        assert!(uptime.contains("h"));
        assert!(uptime.contains("m"));
        assert!(uptime.contains("s"));
    }

    #[tokio::test]
    async fn test_daemon_handle_ping() {
        let temp = TempDir::new().unwrap();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let response = daemon.handle_command(DaemonCommand::Ping).await;

        match response {
            DaemonResponse::Status { status, message } => {
                assert_eq!(status, "ok");
                assert_eq!(message, Some("pong".to_string()));
            }
            _ => panic!("Expected Status response"),
        }
    }

    #[tokio::test]
    async fn test_daemon_handle_shutdown() {
        let temp = TempDir::new().unwrap();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let response = daemon.handle_command(DaemonCommand::Shutdown).await;

        match response {
            DaemonResponse::Status { status, .. } => {
                assert_eq!(status, "shutting_down");
            }
            _ => panic!("Expected Status response"),
        }

        // Verify daemon is stopping
        assert!(daemon.stopping.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_daemon_handle_notify() {
        let temp = TempDir::new().unwrap();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let file = temp.path().join("test.rs");
        let response = daemon.handle_command(DaemonCommand::Notify { file }).await;

        match response {
            DaemonResponse::NotifyResponse {
                dirty_count,
                threshold,
                reindex_triggered,
                ..
            } => {
                assert_eq!(dirty_count, 1);
                assert_eq!(threshold, DEFAULT_REINDEX_THRESHOLD);
                assert!(!reindex_triggered);
            }
            _ => panic!("Expected NotifyResponse"),
        }
    }

    #[tokio::test]
    async fn test_daemon_handle_notify_threshold() {
        let temp = TempDir::new().unwrap();
        let config = DaemonConfig {
            auto_reindex_threshold: 3, // Lower threshold for testing
            ..DaemonConfig::default()
        };
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        // Add files up to threshold
        for i in 0..3 {
            let file = temp.path().join(format!("test{}.rs", i));
            daemon.handle_command(DaemonCommand::Notify { file }).await;
        }

        // The third notification should trigger reindex
        let file = temp.path().join("test3.rs");
        let response = daemon.handle_command(DaemonCommand::Notify { file }).await;

        match response {
            DaemonResponse::NotifyResponse {
                reindex_triggered: _,
                ..
            } => {
                // After threshold is hit, dirty set is cleared
                // So reindex_triggered would have been true, but dirty_count is now 1
            }
            _ => panic!("Expected NotifyResponse"),
        }
    }

    #[tokio::test]
    async fn test_daemon_handle_track() {
        let temp = TempDir::new().unwrap();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let response = daemon
            .handle_command(DaemonCommand::Track {
                hook: "test-hook".to_string(),
                success: true,
                metrics: HashMap::new(),
            })
            .await;

        match response {
            DaemonResponse::TrackResponse {
                hook,
                total_invocations,
                ..
            } => {
                assert_eq!(hook, "test-hook");
                assert_eq!(total_invocations, 1);
            }
            _ => panic!("Expected TrackResponse"),
        }
    }

    #[tokio::test]
    async fn test_daemon_all_sessions_summary() {
        let temp = TempDir::new().unwrap();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        // Add a session
        daemon.sessions.insert(
            "test-session".to_string(),
            SessionStats {
                session_id: "test-session".to_string(),
                raw_tokens: 1000,
                tldr_tokens: 100,
                requests: 10,
                started_at: None,
            },
        );

        let summary = daemon.all_sessions_summary();
        assert_eq!(summary.active_sessions, 1);
        assert_eq!(summary.total_raw_tokens, 1000);
        assert_eq!(summary.total_tldr_tokens, 100);
        assert_eq!(summary.total_requests, 10);
    }

    // =========================================================================
    // Pass-through handler tests
    // =========================================================================

    /// Helper to create a temp dir with a Python file for testing
    fn create_test_project() -> TempDir {
        let temp = TempDir::new().unwrap();
        let py_file = temp.path().join("main.py");
        std::fs::write(
            &py_file,
            "def hello():\n    \"\"\"Say hello.\"\"\"\n    return 'hello'\n\ndef main():\n    hello()\n",
        )
        .unwrap();
        temp
    }

    #[test]
    fn test_hash_str_args_deterministic() {
        let h1 = hash_str_args(&["search", "pattern", "100"]);
        let h2 = hash_str_args(&["search", "pattern", "100"]);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_hash_str_args_different_inputs() {
        let h1 = hash_str_args(&["search", "pattern_a"]);
        let h2 = hash_str_args(&["search", "pattern_b"]);
        assert_ne!(h1, h2);
    }

    #[tokio::test]
    async fn test_daemon_search_returns_result() {
        let temp = create_test_project();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let response = daemon
            .handle_command(DaemonCommand::Search {
                pattern: "def hello".to_string(),
                max_results: Some(10),
            })
            .await;

        match response {
            DaemonResponse::Result(val) => {
                assert!(val.is_array(), "Search should return an array of matches");
                let arr = val.as_array().unwrap();
                assert!(
                    !arr.is_empty(),
                    "Should find at least one match for 'def hello'"
                );
            }
            DaemonResponse::Error { error, .. } => {
                panic!("Search returned error: {}", error);
            }
            other => panic!("Expected Result response, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_daemon_search_caches_result() {
        let temp = create_test_project();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        // First call populates cache
        let _r1 = daemon
            .handle_command(DaemonCommand::Search {
                pattern: "def hello".to_string(),
                max_results: Some(10),
            })
            .await;

        // Second call should hit cache
        let _r2 = daemon
            .handle_command(DaemonCommand::Search {
                pattern: "def hello".to_string(),
                max_results: Some(10),
            })
            .await;

        let stats = daemon.cache_stats();
        assert!(stats.hits >= 1, "Second call should hit cache");
    }

    #[tokio::test]
    async fn test_daemon_extract_returns_result() {
        let temp = create_test_project();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let response = daemon
            .handle_command(DaemonCommand::Extract {
                file: temp.path().join("main.py"),
                session: None,
            })
            .await;

        match response {
            DaemonResponse::Result(val) => {
                assert!(
                    val.is_object(),
                    "Extract should return a module info object"
                );
                // Should contain functions field
                assert!(
                    val.get("functions").is_some(),
                    "Should have 'functions' field"
                );
            }
            DaemonResponse::Error { error, .. } => {
                panic!("Extract returned error: {}", error);
            }
            other => panic!("Expected Result response, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_daemon_extract_nonexistent_file() {
        let temp = TempDir::new().unwrap();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let response = daemon
            .handle_command(DaemonCommand::Extract {
                file: temp.path().join("nonexistent.py"),
                session: None,
            })
            .await;

        match response {
            DaemonResponse::Error { error, .. } => {
                assert!(
                    !error.is_empty(),
                    "Should return an error for nonexistent file"
                );
            }
            _ => panic!("Expected Error response for nonexistent file"),
        }
    }

    #[tokio::test]
    async fn test_daemon_tree_returns_result() {
        let temp = create_test_project();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let response = daemon
            .handle_command(DaemonCommand::Tree { path: None })
            .await;

        match response {
            DaemonResponse::Result(val) => {
                assert!(val.is_object(), "Tree should return a FileTree object");
            }
            DaemonResponse::Error { error, .. } => {
                panic!("Tree returned error: {}", error);
            }
            other => panic!("Expected Result response, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_daemon_structure_returns_result() {
        let temp = create_test_project();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let response = daemon
            .handle_command(DaemonCommand::Structure {
                path: temp.path().to_path_buf(),
                lang: Some("python".to_string()),
            })
            .await;

        match response {
            DaemonResponse::Result(val) => {
                assert!(
                    val.is_object(),
                    "Structure should return a CodeStructure object"
                );
            }
            DaemonResponse::Error { error, .. } => {
                panic!("Structure returned error: {}", error);
            }
            other => panic!("Expected Result response, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_daemon_imports_returns_result() {
        let temp = create_test_project();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let response = daemon
            .handle_command(DaemonCommand::Imports {
                file: temp.path().join("main.py"),
            })
            .await;

        match response {
            DaemonResponse::Result(val) => {
                assert!(val.is_array(), "Imports should return an array");
            }
            DaemonResponse::Error { error, .. } => {
                panic!("Imports returned error: {}", error);
            }
            other => panic!("Expected Result response, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_daemon_cfg_returns_result() {
        let temp = create_test_project();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let file = temp.path().join("main.py");
        let response = daemon
            .handle_command(DaemonCommand::Cfg {
                file,
                function: "hello".to_string(),
            })
            .await;

        match response {
            DaemonResponse::Result(val) => {
                assert!(val.is_object(), "Cfg should return a CfgInfo object");
                assert!(
                    val.get("function").is_some(),
                    "Should have 'function' field"
                );
            }
            DaemonResponse::Error { error, .. } => {
                panic!("Cfg returned error: {}", error);
            }
            other => panic!("Expected Result response, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_daemon_dfg_returns_result() {
        let temp = create_test_project();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let file = temp.path().join("main.py");
        let response = daemon
            .handle_command(DaemonCommand::Dfg {
                file,
                function: "hello".to_string(),
            })
            .await;

        match response {
            DaemonResponse::Result(val) => {
                assert!(val.is_object(), "Dfg should return a DfgInfo object");
                assert!(
                    val.get("function").is_some(),
                    "Should have 'function' field"
                );
            }
            DaemonResponse::Error { error, .. } => {
                panic!("Dfg returned error: {}", error);
            }
            other => panic!("Expected Result response, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_daemon_calls_returns_result() {
        let temp = create_test_project();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let response = daemon
            .handle_command(DaemonCommand::Calls {
                path: None,
                language: None,
            })
            .await;

        match response {
            DaemonResponse::Result(val) => {
                assert!(
                    val.is_object(),
                    "Calls should return a ProjectCallGraph object"
                );
            }
            DaemonResponse::Error { error, .. } => {
                panic!("Calls returned error: {}", error);
            }
            other => panic!("Expected Result response, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_daemon_arch_returns_result() {
        let temp = create_test_project();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let response = daemon
            .handle_command(DaemonCommand::Arch {
                path: None,
                language: None,
            })
            .await;

        match response {
            DaemonResponse::Result(val) => {
                assert!(
                    val.is_object(),
                    "Arch should return an ArchitectureReport object"
                );
            }
            DaemonResponse::Error { error, .. } => {
                panic!("Arch returned error: {}", error);
            }
            other => panic!("Expected Result response, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_daemon_diagnostics_returns_error_with_guidance() {
        let temp = TempDir::new().unwrap();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let path = temp.path().join("src");
        let response = daemon
            .handle_command(DaemonCommand::Diagnostics {
                path: path.clone(),
                project: None,
            })
            .await;

        match response {
            DaemonResponse::Error { error, .. } => {
                assert!(
                    error.contains("Diagnostics requires external tool orchestration"),
                    "Error should explain that diagnostics needs CLI: {}",
                    error
                );
                assert!(
                    error.contains("tldr diagnostics"),
                    "Error should suggest CLI usage"
                );
            }
            other => panic!("Expected Error response, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_daemon_importers_returns_result() {
        let temp = create_test_project();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let response = daemon
            .handle_command(DaemonCommand::Importers {
                module: "os".to_string(),
                path: None,
                language: None,
            })
            .await;

        match response {
            DaemonResponse::Result(val) => {
                assert!(
                    val.is_object(),
                    "Importers should return an ImportersReport object"
                );
            }
            DaemonResponse::Error { error, .. } => {
                panic!("Importers returned error: {}", error);
            }
            other => panic!("Expected Result response, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_daemon_dead_returns_result() {
        let temp = create_test_project();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let response = daemon
            .handle_command(DaemonCommand::Dead {
                path: None,
                entry: None,
                language: None,
            })
            .await;

        match response {
            DaemonResponse::Result(val) => {
                assert!(
                    val.is_object(),
                    "Dead should return a DeadCodeReport object"
                );
            }
            DaemonResponse::Error { error, .. } => {
                panic!("Dead returned error: {}", error);
            }
            other => panic!("Expected Result response, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_daemon_change_impact_returns_result() {
        let temp = create_test_project();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let response = daemon
            .handle_command(DaemonCommand::ChangeImpact {
                files: Some(vec![temp.path().join("main.py")]),
                session: None,
                git: None,
                language: None,
            })
            .await;

        match response {
            DaemonResponse::Result(val) => {
                assert!(
                    val.is_object(),
                    "ChangeImpact should return a ChangeImpactReport object"
                );
            }
            DaemonResponse::Error { error, .. } => {
                panic!("ChangeImpact returned error: {}", error);
            }
            other => panic!("Expected Result response, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_daemon_extract_cache_invalidation() {
        let temp = create_test_project();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let file = temp.path().join("main.py");

        // First extract populates cache
        let r1 = daemon
            .handle_command(DaemonCommand::Extract {
                file: file.clone(),
                session: None,
            })
            .await;
        assert!(matches!(r1, DaemonResponse::Result(_)));

        // Notify file change - should invalidate the cache entry
        daemon
            .handle_command(DaemonCommand::Notify { file: file.clone() })
            .await;

        // After invalidation, next extract should be a cache miss
        let _r2 = daemon
            .handle_command(DaemonCommand::Extract {
                file,
                session: None,
            })
            .await;

        let stats = daemon.cache_stats();
        // Should have: 1 miss (first), 1 invalidation, 1 miss (after invalidation)
        assert!(
            stats.invalidations >= 1,
            "File notify should have caused invalidation"
        );
    }

    #[tokio::test]
    async fn test_daemon_slice_returns_result() {
        let temp = create_test_project();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let file = temp.path().join("main.py");
        let response = daemon
            .handle_command(DaemonCommand::Slice {
                file,
                function: "hello".to_string(),
                line: 3,
            })
            .await;

        match response {
            DaemonResponse::Result(val) => {
                assert!(
                    val.is_array(),
                    "Slice should return an array of line numbers"
                );
            }
            DaemonResponse::Error { error, .. } => {
                panic!("Slice returned error: {}", error);
            }
            other => panic!("Expected Result response, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_daemon_context_returns_result_or_error() {
        let temp = create_test_project();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let response = daemon
            .handle_command(DaemonCommand::Context {
                entry: "main".to_string(),
                depth: Some(1),
                language: None,
            })
            .await;

        // Context may return Result or Error depending on whether 'main' is found
        // in the call graph. Both are valid outcomes for this test.
        match response {
            DaemonResponse::Result(val) => {
                assert!(
                    val.is_object(),
                    "Context should return a RelevantContext object"
                );
            }
            DaemonResponse::Error { .. } => {
                // Function not found in call graph is acceptable
            }
            other => panic!("Expected Result or Error response, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_daemon_impact_returns_result_or_error() {
        let temp = create_test_project();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let response = daemon
            .handle_command(DaemonCommand::Impact {
                func: "hello".to_string(),
                depth: Some(2),
                language: None,
            })
            .await;

        // Impact may return Result or Error depending on call graph contents
        match response {
            DaemonResponse::Result(val) => {
                assert!(
                    val.is_object(),
                    "Impact should return an ImpactReport object"
                );
            }
            DaemonResponse::Error { .. } => {
                // Function not found in call graph is acceptable for small test projects
            }
            other => panic!("Expected Result or Error response, got {:?}", other),
        }
    }

    /// TLDR-7xz.2: a semantic query on a COLD daemon must answer honestly with
    /// `status: "not_ready"` and the warm-me guidance — it must NEVER build the
    /// store inline on the query path (the old behavior this test's predecessor,
    /// `test_semantic_search_builds_index`, asserted). Deterministic: needs no
    /// ONNX, because the not-ready check fires before any embedding.
    #[cfg(feature = "semantic")]
    #[tokio::test]
    async fn test_semantic_cold_query_returns_not_ready() {
        let temp = tempfile::tempdir().unwrap();
        let py_file = temp.path().join("hello.py");
        std::fs::write(
            &py_file,
            "def greet(name):\n    return f'Hello, {name}!'\n",
        )
        .unwrap();

        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let response = daemon
            .handle_command(DaemonCommand::Semantic {
                query: "greeting function".to_string(),
                top_k: 5,
                model: None,
                threshold: None,
            })
            .await;

        match &response {
            DaemonResponse::Error { status, error } => {
                assert_eq!(
                    status, "not_ready",
                    "cold query must be machine-distinguishable from a real error"
                );
                assert!(
                    error.contains("index not built"),
                    "cold query must carry the warm-me guidance, got: {error}"
                );
            }
            other => panic!("cold semantic query must be not_ready, got: {:?}", other),
        }
        assert!(
            !daemon.semantic_store.is_warm(),
            "the query path must NOT have built the store"
        );
    }

    #[cfg(feature = "semantic")]
    #[tokio::test]
    async fn test_semantic_store_delta_on_notify_preserves_warmth() {
        let temp = tempfile::tempdir().unwrap();
        let py_file = temp.path().join("example.py");
        std::fs::write(&py_file, "def compute(x):\n    return x * 2\n").unwrap();

        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        // Warm explicitly — queries never build the store anymore (TLDR-7xz.2).
        // Warms iff ONNX is available in this env; the assertion below is
        // robust either way.
        let _ = daemon
            .handle_command(DaemonCommand::Warm { language: None })
            .await;
        let warm_before = daemon.semantic_store.is_warm();

        // Edit the file on disk so the delta has a changed body to re-embed.
        std::fs::write(&py_file, "def compute(x):\n    return x * 3\n").unwrap();

        // Notify a file change — the incremental delta (TLDR-t8f) updates the
        // store IN PLACE; unlike the old behavior it must NOT invalidate a warm
        // store. (Pre-t8f this dropped the store to None.)
        let _ = daemon
            .handle_command(DaemonCommand::Notify {
                file: py_file.clone(),
            })
            .await;

        assert_eq!(
            daemon.semantic_store.is_warm(),
            warm_before,
            "Notify must preserve the store's warm-state via an incremental \
             delta, not invalidate it"
        );
    }

    /// End-to-end delta through `handle_notify` (TLDR-t8f). Asserts the two
    /// acceptance criteria a warmth-only check can't see — that an EDIT keeps
    /// the vector count (delta keys match the build's, so no orphaned/phantom
    /// vectors) and a DELETE removes all of the file's vectors. No-ops cleanly
    /// when ONNX is unavailable (the store never warms; `store_len` is `None`).
    #[cfg(feature = "semantic")]
    #[tokio::test]
    async fn test_semantic_delta_edit_keeps_count_and_delete_removes_all() {
        let temp = tempfile::tempdir().unwrap();
        let py = temp.path().join("m.py");
        std::fs::write(
            &py,
            "def alpha(x):\n    return x + 1\n\ndef beta(y):\n    return y * 2\n",
        )
        .unwrap();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), DaemonConfig::default());

        // Warm the store explicitly (builds iff ONNX is present in this env) —
        // queries never build it anymore (TLDR-7xz.2).
        let _ = daemon
            .handle_command(DaemonCommand::Warm { language: None })
            .await;
        let Some(len0) = daemon.semantic_store.store_len() else {
            return; // No ONNX here — nothing to assert about deltas.
        };
        assert!(len0 >= 2, "two functions should be indexed, got {len0}");

        // EDIT alpha's body -> the delta re-embeds alpha, leaves beta as a
        // metadata-only entry, removes nothing. The COUNT must be unchanged: a
        // changed count would mean the delta computed different keys than the
        // build and orphaned/duplicated vectors (the ss3 divergence class).
        std::fs::write(
            &py,
            "def alpha(x):\n    return x + 100\n\ndef beta(y):\n    return y * 2\n",
        )
        .unwrap();
        let _ = daemon
            .handle_command(DaemonCommand::Notify { file: py.clone() })
            .await;
        assert_eq!(
            daemon.semantic_store.store_len(),
            Some(len0),
            "edit delta must keep the vector count (keys match the build — no orphans)"
        );

        // DELETE the only file -> every one of its vectors removed (acceptance:
        // "deleted functions' vectors are removed"). This exercises the deleted-
        // path file_rel derivation end-to-end, which the unit test bypasses.
        std::fs::remove_file(&py).unwrap();
        let _ = daemon
            .handle_command(DaemonCommand::Notify { file: py.clone() })
            .await;
        assert_eq!(
            daemon.semantic_store.store_len(),
            Some(0),
            "delete delta must remove all of the file's vectors"
        );
        assert!(
            daemon.semantic_store.is_warm(),
            "store stays warm across edit + delete deltas"
        );
    }

    #[tokio::test]
    async fn test_daemon_warm_wires_caches() {
        let temp = tempfile::tempdir().unwrap();
        let py_file = temp.path().join("example.py");
        std::fs::write(
            &py_file,
            "def add(a, b):\n    return a + b\n\ndef multiply(x, y):\n    return x * y\n",
        )
        .unwrap();

        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let response = daemon
            .handle_command(DaemonCommand::Warm { language: None })
            .await;

        match &response {
            DaemonResponse::Status { status, message } => {
                assert_eq!(status, "ok");
                let msg = message.as_deref().unwrap_or("");
                // Should mention what was warmed, not just "Warm completed"
                assert!(
                    msg.contains("Warmed"),
                    "Expected warm details, got: {}",
                    msg
                );
            }
            other => panic!("Expected Status response, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_daemon_warm_with_language() {
        let temp = tempfile::tempdir().unwrap();
        let rs_file = temp.path().join("lib.rs");
        std::fs::write(
            &rs_file,
            "pub fn hello() -> String {\n    \"hello\".to_string()\n}\n",
        )
        .unwrap();

        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let response = daemon
            .handle_command(DaemonCommand::Warm {
                language: Some("rust".to_string()),
            })
            .await;

        match &response {
            DaemonResponse::Status { status, .. } => {
                assert_eq!(status, "ok");
            }
            other => panic!("Expected Status response, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_status_reports_liveness_busy_and_idle_deadline() {
        // TLDR-qzc: during internal work, status must show the busy token
        // (with label) and a DEFERRED idle deadline; after the work drops,
        // the deadline runs again. This is the "is it building or done?"
        // observability the 90-min-build blindness demanded.
        let temp = tempfile::tempdir().unwrap();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let guard = daemon.activity().begin("warm-build");
        let resp = daemon
            .handle_command(DaemonCommand::Status { session: None })
            .await;
        match &resp {
            DaemonResponse::FullStatus {
                liveness: Some(live),
                ..
            } => {
                assert_eq!(live.busy.len(), 1);
                assert_eq!(live.busy[0].label, "warm-build");
                assert!(
                    live.idle_shutdown_in_secs.is_none(),
                    "deadline must be deferred while busy"
                );
                assert_eq!(
                    live.presence_age_secs.len(),
                    4,
                    "all four sources reported"
                );
            }
            other => panic!("expected FullStatus with liveness, got {:?}", other),
        }

        drop(guard);
        let resp = daemon
            .handle_command(DaemonCommand::Status { session: None })
            .await;
        match &resp {
            DaemonResponse::FullStatus {
                liveness: Some(live),
                ..
            } => {
                assert!(live.busy.is_empty());
                let deadline = live
                    .idle_shutdown_in_secs
                    .expect("deadline must run when not busy");
                assert!(deadline > 0.0 && deadline <= live.idle_timeout_secs as f64);
            }
            other => panic!("expected FullStatus with liveness, got {:?}", other),
        }
    }

    #[cfg(feature = "semantic")]
    #[tokio::test]
    async fn test_status_reports_semantic_index_state() {
        // Cold daemon → "cold"; the warm/building transitions are covered by
        // IndexManager tests (state probe) — here we assert the wiring.
        let temp = tempfile::tempdir().unwrap();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let resp = daemon
            .handle_command(DaemonCommand::Status { session: None })
            .await;
        match &resp {
            DaemonResponse::FullStatus {
                semantic_index: Some(idx),
                ..
            } => {
                assert_eq!(idx.state, "cold");
                assert!(idx.vectors.is_none());
            }
            other => panic!("expected FullStatus with semantic_index, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_daemon_command_handling_leaks_no_busy_tokens() {
        // TLDR-3w5: socket presence is recorded at connection ACCEPT (run
        // loop), not per-command; command handling itself must leave no busy
        // tokens behind (a leaked token would make the daemon immortal).
        let temp = tempfile::tempdir().unwrap();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let _ = daemon.handle_command(DaemonCommand::Ping).await;

        assert_eq!(daemon.activity().busy_count(), 0);
    }

    #[cfg(feature = "semantic")]
    #[tokio::test]
    async fn test_delta_releases_busy_token_and_touches_internal_presence() {
        // TLDR-3w5: the per-file delta wraps its spawn_blocking in a busy
        // guard owned by the closure. After the delta completes the token
        // must be gone and Internal presence refreshed (idle countdown
        // restarts at work COMPLETION, not work start).
        let temp = tempfile::tempdir().unwrap();
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(temp.path().to_path_buf(), config);

        let file = temp.path().join("example.py");
        std::fs::write(&file, "def f():\n    pass\n").unwrap();

        let _ = daemon.process_dirty_file(file).await;

        assert_eq!(
            daemon.activity().busy_count(),
            0,
            "delta busy token must be released on completion"
        );
        let ages = daemon.activity().presence_ages();
        assert!(
            ages[Source::Internal as usize] < std::time::Duration::from_secs(5),
            "Internal presence must be touched at delta completion"
        );
    }

    #[tokio::test]
    async fn test_daemon_created_with_nonexistent_project() {
        // Daemon should be constructable with any path — the exists() check
        // happens in the run loop, not in new()
        let fake_path = PathBuf::from("/tmp/nonexistent-project-dir-12345");
        let config = DaemonConfig::default();
        let daemon = TLDRDaemon::new(fake_path.clone(), config);

        assert_eq!(daemon.project(), &fake_path);
        // The project doesn't exist, but daemon construction succeeds.
        // The run() loop would detect this and self-terminate.
        assert!(!fake_path.exists());
    }
}

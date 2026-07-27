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
use super::artifact_manager::ArtifactManager;
use super::error::{DaemonError, DaemonResult};
use super::hot_cache::{hash_path, HotQueryKey, HotResponseCache};
use super::ipc::{read_command, send_response, IpcListener, IpcStream};
use super::types::{
    AllSessionsSummary, ContextPack, DaemonCommand, DaemonConfig, DaemonResponse, DaemonStatus,
    HookStats, SalsaCacheStats, SessionStats, HOOK_FLUSH_THRESHOLD,
};

#[cfg(feature = "semantic")]
use super::index_manager::IndexManager;
#[cfg(feature = "semantic")]
use tldr_core::config::TldrConfig;
#[cfg(feature = "semantic")]
use tldr_core::semantic::{EmbeddingModel, IndexSearchOptions};
use tldr_core::{
    analyze_smells_aggregated_with_walker_opts, calculate_complexity,
    detect_smells_with_walker_opts, SmellsWalkerOpts,
};
use tldr_core::{
    architecture_analysis, change_impact, detect_or_parse_language, find_importers, get_file_tree,
    get_relevant_context, get_slice, search as tldr_search, Language, SliceDirection,
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

fn apply_artifact_delta_batch(artifacts: &ArtifactManager, files: Vec<PathBuf>) -> Vec<PathBuf> {
    files
        .into_iter()
        .filter_map(|changed| match artifacts.apply_delta(&changed) {
            Ok(_) => Some(changed),
            Err(error) => {
                eprintln!(
                    "[artifact-store] delta failed for {}: {error}",
                    changed.display()
                );
                None
            }
        })
        .collect()
}

#[cfg(feature = "semantic")]
fn apply_semantic_delta_batch(
    artifacts: &ArtifactManager,
    mgr: &IndexManager,
    project: &std::path::Path,
    applied: Vec<PathBuf>,
) {
    use super::index_manager::DeltaOutcome;

    if applied.is_empty() {
        return;
    }
    let snapshot = match artifacts.snapshot() {
        Ok(snapshot) => snapshot,
        Err(state) => {
            eprintln!(
                "[artifact-store] semantic delta batch skipped: generation is not ready: {state:?}"
            );
            mgr.invalidate();
            return;
        }
    };
    let artifact_generation = snapshot.generation();
    let mut source_chunks: HashMap<_, _> = snapshot
        .semantic_source_chunks(project)
        .into_iter()
        .map(|chunk| (chunk.file_path.clone(), chunk))
        .collect();

    for changed in applied {
        let source_chunk = source_chunks.remove(&changed);
        match mgr.apply_delta(project, &changed, source_chunk) {
            Ok(DeltaOutcome::NeedsRebuild) => {
                mgr.invalidate();
                return;
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!(
                    "[t8f] delta failed for {}: {error}; rebuilding",
                    changed.display()
                );
                mgr.invalidate();
                return;
            }
        }
    }

    match mgr.active_generation(project) {
        Ok(Some(generation)) => {
            if let Err(error) = artifacts.attach_vector_generation(artifact_generation, generation)
            {
                mgr.invalidate();
                eprintln!("[artifact-store] semantic generation join failed: {error}");
            }
        }
        Ok(None) => {}
        Err(error) => eprintln!("[artifact-store] semantic generation read failed: {error}"),
    }
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

/// Clears the warm single-flight latch when the background build task ends —
/// including via panic-unwind, so a crashed build never wedges Warm into
/// permanent `already_building`.
struct ClearFlagOnDrop(Arc<AtomicBool>);

impl Drop for ClearFlagOnDrop {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The background warm build (TLDR-utj.7). Carries CLONED handles of the
/// daemon components it needs rather than the daemon itself: the Warm handler
/// only has `&self`, and the detached task must be `'static`.
struct WarmJob {
    project: PathBuf,
    artifact_manager: Arc<ArtifactManager>,
    indexed_files: Arc<RwLock<usize>>,
    #[cfg(feature = "semantic")]
    semantic_store: Arc<IndexManager>,
    /// `None` when model resolution failed at ack time (already reported in
    /// the ack); the semantic step is skipped.
    #[cfg(feature = "semantic")]
    model: Option<EmbeddingModel>,
}

impl WarmJob {
    /// Run all warm steps; returns (warmed, errors) for the daemon log. Same
    /// steps as the old inline handler — only the execution context changed.
    async fn run(self) -> (Vec<&'static str>, Vec<String>) {
        let mut warmed = Vec::new();
        let mut errors = Vec::new();

        // 1. Build or resume the authoritative shared generation. This step
        // reuses unchanged revisions and is the only structural ingestion
        // entry point used by both startup and watcher deltas.
        let manager = Arc::clone(&self.artifact_manager);
        match tokio::task::spawn_blocking(move || manager.warm()).await {
            Ok(Ok(report)) => {
                *self.indexed_files.write().await = self.artifact_manager.stats().hot_files;
                if report.parsed_files == 0 {
                    warmed.push("artifact_store (cached)");
                } else if report.resumed {
                    warmed.push("artifact_store (resumed)");
                } else {
                    warmed.push("artifact_store");
                }
            }
            Ok(Err(error)) => errors.push(format!("artifact_store: {error}")),
            Err(error) => errors.push(format!("artifact_store: {error}")),
        }

        // 2. Warm the vector store: load from disk (near-instant if fresh)
        //    or build+save on miss. Uses the project-config model so a later
        //    query with the same model hits the resident store
        //    (TLDR-atc / TLDR-zxb).
        #[cfg(feature = "semantic")]
        if let Some(model) = self.model {
            let mgr = Arc::clone(&self.semantic_store);
            let artifacts = Arc::clone(&self.artifact_manager);
            let project = self.project.clone();
            let res = tokio::task::spawn_blocking(move || {
                let snapshot = artifacts
                    .snapshot()
                    .map_err(|state| format!("artifact generation is not ready: {state:?}"))?;
                let artifact_generation = snapshot.generation();
                let source_chunks = snapshot.semantic_source_chunks(&project);
                let built = mgr.warm(&project, model, source_chunks)?;
                let vector_generation = mgr.active_generation(&project)?;
                Ok::<_, String>((built, artifact_generation, vector_generation))
            })
            .await;
            match res {
                Ok(Ok((built, artifact_generation, Some(vector_generation)))) => {
                    if let Err(error) = self
                        .artifact_manager
                        .attach_vector_generation(artifact_generation, vector_generation)
                    {
                        self.semantic_store.invalidate();
                        errors.push(format!("semantic_generation_join: {error}"));
                    } else if built {
                        warmed.push("semantic_store");
                    } else {
                        warmed.push("semantic_store (cached)");
                    }
                }
                Ok(Ok((_, _, None))) => {
                    errors.push("semantic_store: no published generation".into())
                }
                Ok(Err(e)) => errors.push(format!("semantic_store: {}", e)),
                Err(e) => errors.push(format!("semantic_store: {}", e)),
            }
        }

        (warmed, errors)
    }
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
    /// Authoritative redb-backed project artifacts and resumable ingestion.
    artifact_manager: Arc<ArtifactManager>,
    /// Salsa-style query cache. Behind `Arc` so the detached warm-build task
    /// (TLDR-utj.7) can own a handle without holding the whole daemon.
    cache: Arc<HotResponseCache>,
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
    /// Single-flight latch for the background warm build (TLDR-utj.7): a
    /// second Warm while one is in flight is acked with `already_building`
    /// instead of stacking builds.
    warm_in_flight: Arc<AtomicBool>,
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
    pub fn new(project: PathBuf, config: DaemonConfig) -> DaemonResult<Self> {
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let artifact_manager = Arc::new(
            ArtifactManager::open(&project)
                .map_err(|error| DaemonError::ArtifactStore(error.to_string()))?,
        );

        let sessions = DashMap::new();
        for session in super::session_context::load_sessions(&project) {
            sessions.insert(session.session_id.clone(), session);
        }
        Ok(Self {
            project,
            config,
            start_time: Instant::now(),
            status: Arc::new(RwLock::new(DaemonStatus::Initializing)),
            artifact_manager,
            cache: Arc::new(HotResponseCache::with_defaults()),
            sessions,
            hooks: DashMap::new(),
            dirty_files: Arc::new(RwLock::new(HashSet::new())),
            shutdown_tx,
            stopping: AtomicBool::new(false),
            activity: Arc::new(ActivityTracker::new()),
            warm_in_flight: Arc::new(AtomicBool::new(false)),
            indexed_files: Arc::new(RwLock::new(0)),
            #[cfg(feature = "semantic")]
            semantic_store: Arc::new(IndexManager::new()),
        })
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

    /// Effective daemon configuration, including project watcher overrides.
    pub(crate) fn config(&self) -> &DaemonConfig {
        &self.config
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
            summary.total_input_tokens += stats.input_tokens;
            summary.total_output_tokens += stats.output_tokens;
            summary.total_injected_tokens += stats.injected_tokens;
            summary.total_cost_usd += stats.cost_usd;
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
        eprintln!("daemon_ready project={}", self.project.display());

        // Lifecycle / tldr init: warm-if-cold on start so users (and LaunchAgent
        // restarts) do not need a separate `tldr warm` for first readiness.
        // Full rebuild is skipped when a warm is already in flight; WarmJob
        // itself short-circuits cached graph/structure/tree/semantic steps.
        {
            let lang = resolve_language(None);
            let _ = self.start_warm_build(lang);
            eprintln!("[lifecycle] ensure-warm queued (warm-if-cold on start)");
        }

        // One-line effective liveness policy (TLDR-d26): idle_timeout_secs
        // changed meaning from client-idle to project-presence-idle (epic
        // TLDR-cxa) — state it where an operator reading the daemon log will
        // see it.
        eprintln!(
            "[liveness] presence-based idle: shutdown after {}s with no client, \
             tldr/MCP invocation, or project file event — never during an \
             in-flight build/delta (epic TLDR-cxa)",
            self.config.idle_timeout_secs
        );

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

        // CLI-wide liveness poke receiver (TLDR-nke): datagram side channel at
        // `<socket>.poke`; every `tldr` invocation in this project defers idle
        // shutdown. NAMED guard — dropping it on shutdown removes the socket
        // file (Unix socket files don't vanish on close).
        #[cfg(unix)]
        let _poke_guard =
            super::poke::spawn_poke_receiver(listener.socket_path(), Arc::clone(&self.activity));

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
    fn resolve_semantic_model(
        &self,
        override_model: Option<&str>,
    ) -> Result<EmbeddingModel, String> {
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

            DaemonCommand::Inject {
                session,
                event,
                prompt,
                source,
                files,
                symbols,
                max_tokens,
                input_tokens,
                output_tokens,
                cost_usd,
            } => self.handle_inject(
                session,
                event,
                prompt,
                source,
                files,
                symbols,
                max_tokens,
                input_tokens,
                output_tokens,
                cost_usd,
            ),

            DaemonCommand::Warm { language } => {
                let parsed = language.as_deref().and_then(|l| l.parse::<Language>().ok());
                let lang = resolve_language(parsed);
                self.start_warm_build(lang)
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
                // resolve_language default so HotQueryKey is well-formed without
                // discriminating across languages.
                let key = HotQueryKey::new(
                    "search",
                    hash_str_args(&[&pattern, &max.to_string()]),
                    resolve_language(None),
                );
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                match tldr_search(&pattern, &self.project, None, 2, max, 1000) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        // TLDR-fct freshness: search scans self.project — register
                        // the project hash so process_dirty_file evicts on any edit.
                        self.cache.insert(key, &val, vec![hash_path(&self.project)]);
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            DaemonCommand::Extract {
                file,
                session: _,
                language,
            } => {
                match self.artifact_manager.file_facts(&file) {
                    Ok(facts) => {
                        if language.is_some_and(|requested| requested != facts.module.language) {
                            return DaemonResponse::Error {
                                status: "error".to_string(),
                                error: format!(
                                    "stored file facts use {}, requested {}",
                                    facts.module.language,
                                    language.expect("checked Some")
                                ),
                            };
                        }
                        let mut module = facts.module.to_module_info();
                        // Match the one-shot extraction surface, which reports
                        // the caller's path rather than the store-relative key.
                        module.file_path = file;
                        DaemonResponse::Result(serde_json::to_value(module).unwrap_or_default())
                    }
                    Err(state) => DaemonResponse::Error {
                        status: "not_ready".to_string(),
                        error: format!("artifact generation is not ready: {state:?}"),
                    },
                }
            }

            DaemonCommand::Tree {
                path,
                extensions,
                include_hidden,
            } => {
                let root = path.unwrap_or_else(|| self.project.clone());
                let root_str = root.to_string_lossy().to_string();
                // File tree is language-agnostic; tag with default language.
                // TLDR-7pp.1.5: extensions + include_hidden are part of the key
                // so flag-varied requests don't collide.
                let key = HotQueryKey::new(
                    "tree",
                    hash_str_args(&[
                        &root_str,
                        &extensions.join(","),
                        &include_hidden.to_string(),
                    ]),
                    resolve_language(None),
                );
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                // Mirror the CLI local path EXACTLY (tree.rs): extension set
                // and skip-hidden = !include_hidden. (TLDR-boa.4 retired the
                // caller-supplied IgnoreSpec; exclusion is on-disk only.)
                let ext_set: Option<std::collections::HashSet<String>> = if extensions.is_empty() {
                    None
                } else {
                    Some(extensions.iter().cloned().collect())
                };
                match get_file_tree(&root, ext_set.as_ref(), !include_hidden) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        // TLDR-fct freshness: mirror Calls — register root+project
                        // hashes; never cache a root outside this daemon's project.
                        if root == self.project || root.starts_with(&self.project) {
                            self.cache.insert(
                                key,
                                &val,
                                vec![hash_path(&root), hash_path(&self.project)],
                            );
                        }
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            DaemonCommand::Structure {
                path,
                lang,
                max_results,
            } => {
                let requested_path = path;
                let path =
                    dunce::canonicalize(&requested_path).unwrap_or_else(|_| requested_path.clone());
                let language = match detect_or_parse_language(lang.as_deref(), &path) {
                    Ok(l) => l,
                    Err(e) => {
                        return DaemonResponse::Error {
                            status: "error".to_string(),
                            error: e.to_string(),
                        }
                    }
                };
                if path != self.project && !path.starts_with(&self.project) {
                    return DaemonResponse::Error {
                        status: "error".to_string(),
                        error: "artifact structure query requires a path inside the daemon project"
                            .to_string(),
                    };
                }
                match self.artifact_manager.snapshot() {
                    Ok(snapshot) => {
                        let mut structure =
                            snapshot.code_structure(&self.project, &path, language, max_results);
                        structure.root = requested_path;
                        DaemonResponse::Result(serde_json::to_value(structure).unwrap_or_default())
                    }
                    Err(state) => DaemonResponse::Error {
                        status: "not_ready".to_string(),
                        error: format!("artifact generation is not ready: {state:?}"),
                    },
                }
            }

            DaemonCommand::Context {
                entry,
                depth,
                language,
                path,
                include_docstrings,
                file,
            } => {
                let d = depth.unwrap_or(2);
                let lang = resolve_language(language);
                let project = path.unwrap_or_else(|| self.project.clone());
                let project_str = project.to_string_lossy().to_string();
                let file_str = file
                    .as_ref()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
                // TLDR-7pp.1.5: project path + include_docstrings + file are
                // part of the key so flag-varied requests don't collide.
                let key = HotQueryKey::new(
                    "context",
                    hash_str_args(&[
                        &entry,
                        &d.to_string(),
                        &project_str,
                        &include_docstrings.to_string(),
                        &file_str,
                    ]),
                    lang,
                );
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                // Mirror context.rs compute_local EXACTLY: caller-supplied
                // project root, include_docstrings, and --file disambiguator.
                // Run off the async runtime — the call-graph build is
                // CPU-heavy (consistent with the Calls/Dead handlers).
                let project_for_build = project.clone();
                let entry_owned = entry.clone();
                let file_owned = file.clone();
                let built = tokio::task::spawn_blocking(move || {
                    get_relevant_context(
                        &project_for_build,
                        &entry_owned,
                        d,
                        lang,
                        include_docstrings,
                        file_owned.as_deref(),
                    )
                })
                .await
                .unwrap_or_else(|e| {
                    Err(tldr_core::TldrError::DaemonError(format!(
                        "context task failed: {e}"
                    )))
                });
                match built {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        // TLDR-fct freshness: context spans the project —
                        // register both the supplied project root and the
                        // daemon project so process_dirty_file evicts on edits.
                        if project == self.project || project.starts_with(&self.project) {
                            self.cache.insert(
                                key,
                                &val,
                                vec![hash_path(&project), hash_path(&self.project)],
                            );
                        }
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            DaemonCommand::Cfg { file, function } => {
                let language = match detect_or_parse_language(None, &file) {
                    Ok(l) => l,
                    Err(e) => {
                        return DaemonResponse::Error {
                            status: "error".to_string(),
                            error: e.to_string(),
                        }
                    }
                };
                match self.artifact_manager.cfg(&file, &function, language) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            DaemonCommand::Dfg { file, function } => {
                let language = match detect_or_parse_language(None, &file) {
                    Ok(l) => l,
                    Err(e) => {
                        return DaemonResponse::Error {
                            status: "error".to_string(),
                            error: e.to_string(),
                        }
                    }
                };
                match self.artifact_manager.dfg(&file, &function, language) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
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
                let key = HotQueryKey::new(
                    "slice",
                    hash_str_args(&[&file_str, &function, &line.to_string()]),
                    language,
                );
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                let file_hash = super::hot_cache::hash_path(&file);
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

            DaemonCommand::Calls {
                path,
                language,
                respect_ignore,
                max_items,
            } => {
                let requested_root = path.unwrap_or_else(|| self.project.clone());
                let root =
                    dunce::canonicalize(&requested_root).unwrap_or_else(|_| requested_root.clone());
                let detected_language = language;
                if root != self.project || !respect_ignore {
                    return DaemonResponse::Error {
                        status: "error".to_string(),
                        error: "artifact call graph requires the daemon project and canonical ignore policy"
                            .to_string(),
                    };
                }
                match self.artifact_manager.snapshot() {
                    Ok(snapshot) => DaemonResponse::Result(
                        serde_json::to_value(
                            crate::commands::calls::call_graph_output_from_artifacts(
                                &requested_root,
                                detected_language,
                                max_items,
                                &snapshot,
                            ),
                        )
                        .unwrap_or_default(),
                    ),
                    Err(state) => DaemonResponse::Error {
                        status: "not_ready".to_string(),
                        error: format!("artifact generation is not ready: {state:?}"),
                    },
                }
            }

            DaemonCommand::Hubs {
                algorithm,
                language,
                top,
                threshold,
            } => {
                let algorithm = match algorithm.parse::<tldr_core::analysis::hubs::HubAlgorithm>() {
                    Ok(algorithm) => algorithm,
                    Err(error) => {
                        return DaemonResponse::Error {
                            status: "error".into(),
                            error,
                        }
                    }
                };
                match self.artifact_manager.snapshot() {
                    Ok(snapshot) => {
                        let graph = snapshot.call_graph(Some(language));
                        let forward = tldr_core::callgraph::build_forward_graph(&graph);
                        let reverse = tldr_core::callgraph::build_reverse_graph(&graph);
                        let nodes = tldr_core::callgraph::collect_nodes(&graph);
                        let mut lines = HashMap::new();
                        for facts in snapshot.files() {
                            let file = PathBuf::from(&facts.path);
                            for function in &facts.module.functions {
                                lines.insert(
                                    (file.clone(), function.name.clone()),
                                    function.line_number,
                                );
                            }
                            for class in &facts.module.classes {
                                for method in &class.methods {
                                    lines.insert(
                                        (file.clone(), format!("{}.{}", class.name, method.name)),
                                        method.line_number,
                                    );
                                    lines
                                        .entry((file.clone(), method.name.clone()))
                                        .or_insert(method.line_number);
                                }
                            }
                        }
                        let report = tldr_core::analysis::hubs::compute_hub_report_with_lines(
                            &nodes,
                            &forward,
                            &reverse,
                            algorithm,
                            top,
                            threshold,
                            Some(&lines),
                        );
                        DaemonResponse::Result(serde_json::to_value(report).unwrap_or_default())
                    }
                    Err(state) => DaemonResponse::Error {
                        status: "not_ready".to_string(),
                        error: format!("artifact generation is not ready: {state:?}"),
                    },
                }
            }

            DaemonCommand::Impact {
                func,
                depth,
                language: _,
                file,
            } => {
                let d = depth.unwrap_or(3);
                let snapshot = match self.artifact_manager.snapshot() {
                    Ok(snapshot) => snapshot,
                    Err(state) => {
                        return DaemonResponse::Error {
                            status: "not_ready".to_string(),
                            error: format!("artifact generation is not ready: {state:?}"),
                        }
                    }
                };
                let file_filter = file
                    .as_deref()
                    .map(|path| path.to_string_lossy().replace('\\', "/"));
                match snapshot
                    .graph()
                    .impact_report(&func, d, file_filter.as_deref())
                {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
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
                call_graph,
                no_default_ignore,
            } => {
                let requested_root = path.unwrap_or_else(|| self.project.clone());
                let root =
                    dunce::canonicalize(&requested_root).unwrap_or_else(|_| requested_root.clone());
                let lang = resolve_language(language);
                if no_default_ignore {
                    return DaemonResponse::Error {
                        status: "error".to_string(),
                        error: "resident dead-code analysis uses the canonical ignore policy; use --oneshot with --no-default-ignore"
                            .to_string(),
                    };
                }
                if !root.starts_with(&self.project) {
                    return DaemonResponse::Error {
                        status: "error".to_string(),
                        error:
                            "resident dead-code analysis requires a path inside the daemon project"
                                .to_string(),
                    };
                }
                let snapshot = match self.artifact_manager.snapshot() {
                    Ok(snapshot) => snapshot,
                    Err(state) => {
                        return DaemonResponse::Error {
                            status: "not_ready".to_string(),
                            error: format!("artifact generation is not ready: {state:?}"),
                        }
                    }
                };
                match snapshot.dead_report(&self.project, &root, lang, entry.as_deref(), call_graph)
                {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
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
                let _language = language;
                if root != self.project {
                    return DaemonResponse::Error {
                        status: "error".to_string(),
                        error: "artifact architecture query requires the daemon project"
                            .to_string(),
                    };
                }
                let snapshot = match self.artifact_manager.snapshot() {
                    Ok(snapshot) => snapshot,
                    Err(state) => {
                        return DaemonResponse::Error {
                            status: "not_ready".to_string(),
                            error: format!("artifact generation is not ready: {state:?}"),
                        }
                    }
                };
                let graph = snapshot.intra_file_call_graph();
                match architecture_analysis(&graph) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            DaemonCommand::Imports { file, language } => {
                // Mirror imports.rs: explicit --lang wins; otherwise detect.
                let language =
                    match detect_or_parse_language(language.as_ref().map(|l| l.as_str()), &file) {
                        Ok(l) => l,
                        Err(e) => {
                            return DaemonResponse::Error {
                                status: "error".to_string(),
                                error: e.to_string(),
                            }
                        }
                    };
                match self.artifact_manager.file_facts(&file) {
                    Ok(facts) if facts.module.language == language => DaemonResponse::Result(
                        serde_json::to_value(facts.module.imports).unwrap_or_default(),
                    ),
                    Ok(facts) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: format!(
                            "stored file facts use {}, requested {language}",
                            facts.module.language
                        ),
                    },
                    Err(state) => DaemonResponse::Error {
                        status: "not_ready".to_string(),
                        error: format!("artifact generation is not ready: {state:?}"),
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
                let key = HotQueryKey::new("importers", hash_str_args(&[&module, &root_str]), lang);
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                match find_importers(&root, &module, lang) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        // TLDR-fct freshness: mirror Calls — register root+project
                        // hashes; never cache a root outside this daemon's project.
                        if root == self.project || root.starts_with(&self.project) {
                            self.cache.insert(
                                key,
                                &val,
                                vec![hash_path(&root), hash_path(&self.project)],
                            );
                        }
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
                let key = HotQueryKey::new("change_impact", hash_str_args(&[&files_str]), lang);
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                let changed: Option<Vec<PathBuf>> = files;
                match change_impact(&self.project, changed.as_deref(), lang) {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        // TLDR-fct freshness: change-impact spans self.project —
                        // register the project hash so edits evict it.
                        self.cache.insert(key, &val, vec![hash_path(&self.project)]);
                        DaemonResponse::Result(val)
                    }
                    Err(e) => DaemonResponse::Error {
                        status: "error".to_string(),
                        error: e.to_string(),
                    },
                }
            }

            // TLDR-7pp.1.3: real handler for `tldr complexity` (was a missing
            // variant => silent local fallback). Compute-on-miss + per-file
            // freshness, mirroring the CLI local path exactly.
            DaemonCommand::Complexity {
                file,
                function,
                language,
            } => {
                let file_str = file.to_string_lossy().to_string();
                // Mirror complexity.rs: language hint > auto-detect from path.
                let lang = match detect_or_parse_language(language.map(|l| l.as_str()), &file) {
                    Ok(l) => l,
                    Err(e) => {
                        return DaemonResponse::Error {
                            status: "error".to_string(),
                            error: e.to_string(),
                        }
                    }
                };
                let key =
                    HotQueryKey::new("complexity", hash_str_args(&[&file_str, &function]), lang);
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                let file_hash = hash_path(&file);
                match calculate_complexity(&file_str, &function, lang) {
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

            // TLDR-7pp.1.3: real handler for `tldr smells`. Returns the raw
            // SmellsReport; the deep-only advisory warning is injected
            // CLI-side (presentation concern) so daemon and --oneshot paths
            // stay byte-identical.
            DaemonCommand::Smells {
                path,
                threshold,
                smell_type,
                suggest,
                deep,
                no_default_ignore,
                files,
                include_tests,
                language,
            } => {
                let path_str = path.to_string_lossy().to_string();
                // smell_type Display/Debug + the bool flags fully determine the
                // result, so fold them all into the cache key.
                let st_str = smell_type
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "all".to_string());
                let key = HotQueryKey::new(
                    "smells",
                    hash_str_args(&[
                        &path_str,
                        &format!("{threshold:?}"),
                        &st_str,
                        &suggest.to_string(),
                        &deep.to_string(),
                        &no_default_ignore.to_string(),
                        &include_tests.to_string(),
                        &files
                            .iter()
                            .map(|p| p.to_string_lossy().to_string())
                            .collect::<Vec<_>>()
                            .join(","),
                    ]),
                    resolve_language(language),
                );
                if let Some(cached) = self.cache.get::<serde_json::Value>(&key) {
                    return DaemonResponse::Result(cached);
                }
                let walker_opts = SmellsWalkerOpts {
                    no_default_ignore,
                    lang: language,
                    files,
                    include_tests,
                };
                let computed = if deep {
                    analyze_smells_aggregated_with_walker_opts(
                        &path,
                        threshold,
                        smell_type,
                        suggest,
                        walker_opts,
                    )
                } else {
                    detect_smells_with_walker_opts(
                        &path,
                        threshold,
                        smell_type,
                        suggest,
                        walker_opts,
                    )
                };
                match computed {
                    Ok(result) => {
                        let val = serde_json::to_value(&result).unwrap_or_default();
                        // Freshness: smells scans `path`; register both the
                        // query path and the daemon project root (only when the
                        // path is inside the project — outside paths aren't
                        // covered by the watcher, so never cache them).
                        if path == self.project || path.starts_with(&self.project) {
                            self.cache.insert(
                                key,
                                &val,
                                vec![hash_path(&path), hash_path(&self.project)],
                            );
                        }
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
        let artifact_stats = self.artifact_manager.stats();
        let (artifact_state, target_generation, last_error) = match self.artifact_manager.state() {
            super::artifact_manager::ArtifactState::Cold => ("cold", None, None),
            super::artifact_manager::ArtifactState::Building { target_generation } => {
                ("building", Some(target_generation), None)
            }
            super::artifact_manager::ArtifactState::Ready { .. } => ("ready", None, None),
            super::artifact_manager::ArtifactState::Failed { error, .. } => {
                ("failed", None, Some(error))
            }
        };

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
            memory: Some(super::types::MemoryStats {
                rss_bytes: super::rss::current_rss_bytes(),
                peak_rss_bytes: super::rss::peak_rss_bytes(),
            }),
            artifact_store: Some(super::types::ArtifactStoreStats {
                state: artifact_state.into(),
                active_generation: artifact_stats.active_generation,
                target_generation,
                files: artifact_stats.hot_files,
                parse_errors: artifact_stats.parse_errors,
                redb_bytes: artifact_stats.redb_bytes,
                last_error,
            }),
        }
    }

    /// Start the warm build as a DETACHED background task and ack immediately
    /// (TLDR-utj.7). The old inline shape was structurally doomed: the
    /// handler awaited the full build while the client blocked on a 30s IPC
    /// read timeout — any build over 30s printed a misleading "Failed to
    /// send" while the daemon kept building. Now the ack returns in
    /// microseconds and the client is pointed at `tldr daemon status`
    /// (busy `warm-build` + semantic_index state, TLDR-qzc) for progress.
    fn start_warm_build(&self, _lang: Language) -> DaemonResponse {
        // Single-flight: a second Warm during a build is answered honestly
        // instead of stacking a duplicate build behind the store write lock.
        if self
            .warm_in_flight
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return DaemonResponse::Status {
                status: "already_building".to_string(),
                message: Some(
                    "warm build already in progress — poll 'tldr daemon status' for progress"
                        .to_string(),
                ),
            };
        }

        // Resolve the embedding model BEFORE spawning so a config error is
        // reported synchronously in the ack instead of buried in the log.
        #[cfg(feature = "semantic")]
        let (model, model_note) = match self.resolve_semantic_model(None) {
            Ok(m) => (Some(m), String::new()),
            Err(e) => (None, format!(" (semantic skipped: {e})")),
        };
        #[cfg(not(feature = "semantic"))]
        let model_note = String::new();

        let job = WarmJob {
            project: self.project.clone(),
            artifact_manager: Arc::clone(&self.artifact_manager),
            indexed_files: Arc::clone(&self.indexed_files),
            #[cfg(feature = "semantic")]
            semantic_store: Arc::clone(&self.semantic_store),
            #[cfg(feature = "semantic")]
            model,
        };

        // Busy guard created BEFORE the ack (no status-misses-busy window)
        // and owned by the DETACHED task — unlike the requesting connection
        // task, a tokio::spawn'd task is never cancelled by a client
        // timeout/disconnect, so the guard lives exactly as long as the
        // build (TLDR-3w5 invariant preserved).
        let busy = self.activity.begin("warm-build");
        let in_flight = Arc::clone(&self.warm_in_flight);
        tokio::spawn(async move {
            let _busy = busy;
            let _clear = ClearFlagOnDrop(in_flight);
            let (warmed, errors) = job.run().await;
            if errors.is_empty() {
                eprintln!("[warm] background build complete: {}", warmed.join(", "));
            } else {
                eprintln!(
                    "[warm] background build finished — warmed: {}; errors: {}",
                    warmed.join(", "),
                    errors.join("; ")
                );
            }
        });

        DaemonResponse::Status {
            status: "started".to_string(),
            message: Some(format!(
                "warm build started — poll 'tldr daemon status' for progress{model_note}"
            )),
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
        let runners = self
            .semantic_store
            .runner_states()
            .into_iter()
            .map(|runner| super::types::InferenceRunnerStats {
                workload: runner.workload.as_str().to_string(),
                state: runner.state,
                model: runner.model,
                sessions_built: runner.sessions_built,
                requests: runner.requests,
                failures: runner.failures,
                exact_shapes: runner.exact_shapes,
            })
            .collect();
        Some(match self.semantic_store.state() {
            IndexState::Warm { vectors } => super::types::SemanticIndexStats {
                state: "warm".to_string(),
                vectors: Some(vectors),
                runners,
            },
            IndexState::Building => super::types::SemanticIndexStats {
                state: "building".to_string(),
                vectors: None,
                runners,
            },
            IndexState::Cold => super::types::SemanticIndexStats {
                state: "cold".to_string(),
                vectors: None,
                runners,
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
        self.process_dirty_files(vec![file]).await
    }

    /// Apply a deduplicated watcher batch through one blocking job.
    ///
    /// Cache bookkeeping stays on the async side and contains no blocking I/O.
    /// Authoritative artifact ingestion and resident semantic deltas execute
    /// serially inside one `spawn_blocking` closure, so an N-file flush pays
    /// one scheduler crossing and cannot reorder generations.
    pub(crate) async fn process_dirty_files(&self, files: Vec<PathBuf>) -> ReindexOutcome {
        let mut files: Vec<_> = files
            .into_iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        files.sort();

        if files.is_empty() {
            return ReindexOutcome {
                dirty_count: self.dirty_files.read().await.len(),
                threshold: self.config.auto_reindex_threshold,
                reindex_triggered: false,
            };
        }

        // Filesystem edits are session context too: every active conversation
        // should prefer recently changed code on its next turn even when the
        // editor, rather than a Read/Edit hook, produced the write.
        let relative_files = files
            .iter()
            .map(|file| {
                file.strip_prefix(&self.project)
                    .unwrap_or(file)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect::<Vec<_>>();
        for mut session in self.sessions.iter_mut() {
            session.touch_context(
                relative_files.iter().map(String::as_str),
                std::iter::empty(),
            );
        }
        if !self.sessions.is_empty() {
            let persisted = self
                .sessions
                .iter()
                .map(|entry| entry.value().clone())
                .collect::<Vec<_>>();
            let _ = super::session_context::persist_sessions(&self.project, &persisted);
        }

        // Add the whole batch to dirty accounting in one write-lock hold.
        let dirty_count = {
            let mut dirty = self.dirty_files.write().await;
            dirty.extend(files.iter().cloned());
            dirty.len()
        };

        for file in &files {
            self.cache
                .invalidate_by_input(super::hot_cache::hash_path(file));
        }

        // TLDR-iqr freshness: project-level answers (calls/structure/tree/
        // dead/arch/impact) register against the project-root hash; any save
        // invalidates them so the next query lazily recomposes from the memo
        // (a 20-save burst = 20 cheap evictions + ONE recompose). Before this,
        // they registered vec![] and were PERMANENTLY stale (finding on iqr).
        self.cache
            .invalidate_by_input(super::hot_cache::hash_path(&self.project));

        let artifacts = Arc::clone(&self.artifact_manager);
        #[cfg(feature = "semantic")]
        let mgr = Arc::clone(&self.semantic_store);
        #[cfg(feature = "semantic")]
        let project = self.project.clone();
        let busy = self.activity.begin("delta-batch");
        let _ = tokio::task::spawn_blocking(move || {
            let _busy = busy;
            let applied = apply_artifact_delta_batch(&artifacts, files);

            #[cfg(feature = "semantic")]
            apply_semantic_delta_batch(&artifacts, &mgr, &project, applied);

            #[cfg(not(feature = "semantic"))]
            {
                let _ = applied;
            }
        })
        .await;

        let threshold = self.config.auto_reindex_threshold;
        let reindex_triggered = dirty_count >= threshold;

        if reindex_triggered {
            let mut dirty = self.dirty_files.write().await;
            dirty.clear();
        }

        ReindexOutcome {
            dirty_count,
            threshold,
            reindex_triggered,
        }
    }

    /// Supersede queued watcher deltas with the existing single-flight warm.
    pub(crate) async fn schedule_full_rebuild(&self) {
        self.dirty_files.write().await.clear();
        #[cfg(feature = "semantic")]
        self.semantic_store.invalidate();

        match self.start_warm_build(resolve_language(None)) {
            DaemonResponse::Status {
                status,
                message: Some(message),
            } => eprintln!("[ac0.7] burst rebuild {status}: {message}"),
            DaemonResponse::Status { status, .. } => {
                eprintln!("[ac0.7] burst rebuild {status}")
            }
            response => {
                eprintln!("[ac0.7] unexpected burst rebuild response: {response:?}")
            }
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

    #[allow(clippy::too_many_arguments)]
    fn handle_inject(
        &self,
        session_id: String,
        event: String,
        prompt: String,
        source: Option<String>,
        files: Vec<String>,
        symbols: Vec<String>,
        max_tokens: usize,
        input_tokens: u64,
        output_tokens: u64,
        cost_usd: f64,
    ) -> DaemonResponse {
        let event_label = source
            .as_deref()
            .map_or_else(|| event.clone(), |source| format!("{event}:{source}"));
        {
            let mut session = self
                .sessions
                .entry(session_id.clone())
                .or_insert_with(|| SessionStats::new(session_id.clone()));
            session.record_lifecycle(&event_label, input_tokens, output_tokens, cost_usd);
            session.touch_context(
                files.iter().map(String::as_str),
                symbols.iter().map(String::as_str),
            );
        }

        let produces_context = event.eq_ignore_ascii_case("UserPromptSubmit")
            || event.eq_ignore_ascii_case("SessionStart")
            || event.eq_ignore_ascii_case("PreCompact")
            || event.eq_ignore_ascii_case("PostCompact");
        let mut pack = ContextPack {
            content: String::new(),
            tokens: 0,
            files: Vec::new(),
            symbols: Vec::new(),
            generation: 0,
            elapsed_ms: 0.0,
            truncated: false,
            source: "project".into(),
        };
        if produces_context {
            if let Ok(snapshot) = self.artifact_manager.snapshot() {
                let sessions = self
                    .sessions
                    .iter()
                    .map(|entry| entry.value().clone())
                    .collect::<Vec<_>>();
                let hot = super::session_context::aggregate_hot_files(sessions.iter());
                let current = self
                    .sessions
                    .get(&session_id)
                    .map(|entry| entry.value().clone())
                    .unwrap_or_else(|| SessionStats::new(session_id.clone()));
                pack = super::session_context::build_context_pack(
                    &snapshot,
                    &current,
                    &hot,
                    &prompt,
                    &event,
                    source.as_deref(),
                    max_tokens,
                );
                if let Some(mut current) = self.sessions.get_mut(&session_id) {
                    current.record_injection(pack.tokens as u64);
                    current.touch_context(
                        pack.files.iter().map(String::as_str),
                        pack.symbols.iter().map(String::as_str),
                    );
                }
            }
        }

        let persisted = self
            .sessions
            .iter()
            .map(|entry| entry.value().clone())
            .collect::<Vec<_>>();
        let _ = super::session_context::persist_sessions(&self.project, &persisted);
        let mut hook = self
            .hooks
            .entry(event.clone())
            .or_insert_with(|| HookStats::new(event));
        let mut metrics = HashMap::new();
        metrics.insert("context_tokens".into(), pack.tokens as f64);
        metrics.insert("latency_ms".into(), pack.elapsed_ms);
        hook.record_invocation(true, Some(metrics));
        DaemonResponse::Result(serde_json::to_value(pack).unwrap_or_default())
    }

    /// Persist statistics to disk.
    async fn persist_stats(&self) -> DaemonResult<()> {
        // Long-lived derived state is committed transactionally by
        // ArtifactManager. Hook session continuity has its own small,
        // atomically replaced JSON ledger; derived analysis never does.
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
    use super::{DaemonConfig, TLDRDaemon};

    #[tokio::test]
    async fn dirty_file_batch_publishes_each_final_revision() {
        let project = tempfile::tempdir().expect("temp project");
        let root = project.path().canonicalize().expect("canonical root");
        let first = root.join("first.py");
        let second = root.join("second.py");
        std::fs::write(&first, "def first():\n    return 1\n").expect("write first");
        std::fs::write(&second, "def second():\n    return 1\n").expect("write second");

        let daemon = TLDRDaemon::new(root.clone(), DaemonConfig::default()).expect("create daemon");
        daemon
            .artifact_manager
            .warm()
            .expect("publish baseline generation");
        let baseline = daemon
            .artifact_manager
            .snapshot()
            .expect("baseline snapshot");
        let first_revision = baseline
            .file("first.py")
            .expect("baseline first.py")
            .revision;
        let second_revision = baseline
            .file("second.py")
            .expect("baseline second.py")
            .revision;

        std::fs::write(&first, "def first():\n    return 2\n").expect("edit first");
        std::fs::write(&second, "def second():\n    return 2\n").expect("edit second");
        let outcome = daemon
            .process_dirty_files(vec![first.clone(), second.clone(), first])
            .await;

        let current = daemon
            .artifact_manager
            .snapshot()
            .expect("current snapshot");
        assert_eq!(outcome.dirty_count, 2);
        assert_ne!(
            current.file("first.py").expect("current first.py").revision,
            first_revision
        );
        assert_ne!(
            current
                .file("second.py")
                .expect("current second.py")
                .revision,
            second_revision
        );
    }
}

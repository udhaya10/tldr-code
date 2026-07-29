//! Core types for the TLDR daemon subsystem
//!
//! Types for daemon configuration, status, statistics, and IPC messages.
//! All types are serializable for JSON IPC communication.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tldr_core::analysis::references::{ReferenceKind, SearchScope};
use tldr_core::{config::TldrConfig, Language, SmellType, ThresholdPreset};

const MAX_SESSION_HOT_ITEMS: usize = 64;

// =============================================================================
// Constants
// =============================================================================

/// Idle timeout before daemon auto-shutdown (30 minutes)
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Idle timeout in seconds for serialization
pub const IDLE_TIMEOUT_SECS: u64 = 30 * 60;

/// Default threshold for triggering semantic re-index
pub const DEFAULT_REINDEX_THRESHOLD: usize = 20;

/// Legacy watcher delay, aligned with the fixed collection window.
pub const DEFAULT_WATCHER_DEBOUNCE_MS: u64 = 5_000;

/// Fixed watcher batch lifetime measured from its first accepted event.
pub const DEFAULT_WATCHER_MAX_WAIT_MS: u64 = 5_000;

/// Pending unique-file count above which a full rebuild supersedes deltas.
pub const DEFAULT_WATCHER_BURST_FILE_CAP: usize = 200;

/// Accepted-event count above which a full rebuild supersedes deltas.
pub const DEFAULT_WATCHER_BURST_EVENT_CAP: usize = 1_000;

/// Rolling window for the watcher event cap.
pub const DEFAULT_WATCHER_BURST_WINDOW_MS: u64 = 2_000;

/// Default flush interval for hook stats (every N invocations)
pub const HOOK_FLUSH_THRESHOLD: usize = 5;

// =============================================================================
// Configuration Types
// =============================================================================

/// Serde default for [`DaemonConfig::enable_watcher`]: the in-daemon watcher is
/// ON by default, so a config that predates the field (where serde would
/// otherwise fill `bool::default()` == `false`) keeps the self-watch behavior.
fn default_enable_watcher() -> bool {
    true
}

/// Daemon configuration loaded from .tldr/config.json or .claude/settings.json
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DaemonConfig {
    /// Whether semantic search is enabled
    pub semantic_enabled: bool,

    /// Number of dirty files before auto re-index
    pub auto_reindex_threshold: usize,

    /// Embedding model for semantic search
    pub semantic_model: String,

    /// PROJECT-PRESENCE idle timeout in seconds (default: 1800 = 30 min).
    ///
    /// SEMANTICS CHANGE (epic TLDR-cxa, 2026-06-04; migration note
    /// TLDR-d26): this used to be a CLIENT idle timeout — the daemon died
    /// after this long without a socket connection, even mid-build. It now
    /// measures PROJECT dormancy: the countdown resets on any client
    /// connection, any `tldr`/`tldr_mcp` invocation in the project (liveness
    /// poke), any watcher-observed file write, and is suspended entirely
    /// while internal work (index build, delta) is in flight. The key is
    /// deliberately UNCHANGED — the duration concept is the same; only what
    /// counts as "activity" broadened. Consequence (accepted trade-off): on
    /// machines with long-running builds the daemon effectively never idles
    /// out — warm availability is chosen over memory thrift (escape hatch:
    /// TLDR-yll).
    pub idle_timeout_secs: u64,

    /// Whether the in-daemon filesystem watcher is active (TLDR-ac0.2).
    /// DEFAULT ON: the daemon self-watches its project root on start (the
    /// recorded cutover plan — TLDR-4vb). During the window before the C++
    /// fsnotifier is disabled (cross-repo, TLDR-ejm) both watchers may feed
    /// `process_dirty_file` for one edit; that overlap is wasteful but
    /// harmless — `apply_delta`'s content-hash check makes the second delta a
    /// no-op. Set to `false` (or `TLDR_IN_DAEMON_WATCH=0`, if wired) to opt out.
    /// `#[serde(default = "default_enable_watcher")]` keeps older persisted
    /// configs (which lack the field) defaulting to the ON behavior.
    #[serde(default = "default_enable_watcher")]
    pub enable_watcher: bool,

    /// Legacy-compatible watcher delay. Fixed-window collection now uses
    /// `watcher_max_wait_ms`; the default remains aligned at five seconds.
    #[serde(default = "default_watcher_debounce_ms")]
    pub watcher_debounce_ms: u64,

    /// Fixed batch window measured from its first accepted event.
    #[serde(default = "default_watcher_max_wait_ms")]
    pub watcher_max_wait_ms: u64,

    /// Unique pending-file cap before escalating to a full rebuild.
    #[serde(default = "default_watcher_burst_file_cap")]
    pub watcher_burst_file_cap: usize,

    /// Accepted-event cap inside `watcher_burst_window_ms`.
    #[serde(default = "default_watcher_burst_event_cap")]
    pub watcher_burst_event_cap: usize,

    /// Rolling window used by `watcher_burst_event_cap`.
    #[serde(default = "default_watcher_burst_window_ms")]
    pub watcher_burst_window_ms: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            semantic_enabled: true,
            auto_reindex_threshold: DEFAULT_REINDEX_THRESHOLD,
            semantic_model: "snowflake-arctic-embed-m".to_string(),
            idle_timeout_secs: IDLE_TIMEOUT_SECS,
            enable_watcher: default_enable_watcher(),
            watcher_debounce_ms: default_watcher_debounce_ms(),
            watcher_max_wait_ms: default_watcher_max_wait_ms(),
            watcher_burst_file_cap: default_watcher_burst_file_cap(),
            watcher_burst_event_cap: default_watcher_burst_event_cap(),
            watcher_burst_window_ms: default_watcher_burst_window_ms(),
        }
    }
}

impl DaemonConfig {
    /// Resolve global + project `.tldr/config.json` watcher overrides.
    pub fn resolve(project: &std::path::Path) -> Self {
        let resolved = TldrConfig::resolve(Some(project));
        Self {
            semantic_enabled: resolved.semantic.enabled,
            semantic_model: resolved
                .embedding
                .model
                .unwrap_or_else(|| "snowflake-arctic-embed-m".to_string()),
            enable_watcher: resolved
                .watcher
                .enabled
                .unwrap_or_else(default_enable_watcher),
            watcher_debounce_ms: resolved
                .watcher
                .debounce_ms
                .unwrap_or_else(default_watcher_debounce_ms),
            watcher_max_wait_ms: resolved
                .watcher
                .max_wait_ms
                .unwrap_or_else(default_watcher_max_wait_ms),
            watcher_burst_file_cap: resolved
                .watcher
                .burst_file_cap
                .unwrap_or_else(default_watcher_burst_file_cap),
            watcher_burst_event_cap: resolved
                .watcher
                .burst_event_cap
                .unwrap_or_else(default_watcher_burst_event_cap),
            watcher_burst_window_ms: resolved
                .watcher
                .burst_window_ms
                .unwrap_or_else(default_watcher_burst_window_ms),
            ..Self::default()
        }
    }
}

fn default_watcher_debounce_ms() -> u64 {
    DEFAULT_WATCHER_DEBOUNCE_MS
}

fn default_watcher_max_wait_ms() -> u64 {
    DEFAULT_WATCHER_MAX_WAIT_MS
}

fn default_watcher_burst_file_cap() -> usize {
    DEFAULT_WATCHER_BURST_FILE_CAP
}

fn default_watcher_burst_event_cap() -> usize {
    DEFAULT_WATCHER_BURST_EVENT_CAP
}

fn default_watcher_burst_window_ms() -> u64 {
    DEFAULT_WATCHER_BURST_WINDOW_MS
}

// =============================================================================
// Status Types
// =============================================================================

/// Daemon runtime status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonStatus {
    /// Daemon is starting up, acquiring locks
    Initializing,
    /// Daemon is building initial indexes
    Indexing,
    /// Daemon is ready to accept queries
    Ready,
    /// Daemon is shutting down
    ShuttingDown,
    /// Daemon has stopped
    Stopped,
}

// =============================================================================
// Statistics Types
// =============================================================================

/// Statistics for Salsa-style query cache
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SalsaCacheStats {
    /// Number of cache hits (query result reused)
    pub hits: u64,

    /// Number of cache misses (query recomputed)
    pub misses: u64,

    /// Number of invalidations (file changed)
    pub invalidations: u64,

    /// Number of recomputations triggered by invalidation
    pub recomputations: u64,
}

impl SalsaCacheStats {
    /// Calculate hit rate as percentage (0-100)
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            return 0.0;
        }
        (self.hits as f64 / total as f64) * 100.0
    }
}

/// Statistics for content-hash deduplication
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DedupStats {
    /// Number of unique content hashes
    pub unique_hashes: usize,

    /// Number of duplicate content blocks avoided
    pub duplicates_avoided: usize,

    /// Bytes saved through deduplication
    pub bytes_saved: u64,
}

/// Per-session statistics for token tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStats {
    /// Session identifier (8-char truncated UUID)
    pub session_id: String,

    /// Raw tokens (what vanilla Claude would use)
    pub raw_tokens: u64,

    /// TLDR tokens (what was actually returned)
    pub tldr_tokens: u64,

    /// Number of requests in this session
    pub requests: u64,

    /// Model input tokens reported by lifecycle hooks, when available.
    #[serde(default)]
    pub input_tokens: u64,

    /// Model output tokens reported by lifecycle hooks, when available.
    #[serde(default)]
    pub output_tokens: u64,

    /// Context tokens injected by tldr.
    #[serde(default)]
    pub injected_tokens: u64,

    /// Provider-reported session cost, when available.
    #[serde(default)]
    pub cost_usd: f64,

    /// Files served/read/edited in this conversation, weighted by frequency.
    #[serde(default)]
    pub hot_files: std::collections::BTreeMap<String, u64>,

    /// Symbols served in this conversation, weighted by frequency.
    #[serde(default)]
    pub hot_symbols: std::collections::BTreeMap<String, u64>,

    /// Most recent lifecycle event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event: Option<String>,

    /// Last update timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,

    /// When session started (ISO 8601 timestamp)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl SessionStats {
    /// Create a new session with the given ID
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            raw_tokens: 0,
            tldr_tokens: 0,
            requests: 0,
            input_tokens: 0,
            output_tokens: 0,
            injected_tokens: 0,
            cost_usd: 0.0,
            hot_files: Default::default(),
            hot_symbols: Default::default(),
            last_event: None,
            updated_at: Some(chrono::Utc::now()),
            started_at: Some(chrono::Utc::now()),
        }
    }

    /// Record a request's token usage
    pub fn record_request(&mut self, raw_tokens: u64, tldr_tokens: u64) {
        self.raw_tokens += raw_tokens;
        self.tldr_tokens += tldr_tokens;
        self.requests += 1;
        self.updated_at = Some(chrono::Utc::now());
    }

    /// Record one pushed hook context without mixing it into pull-query token
    /// savings (`raw_tokens`/`tldr_tokens` have a different baseline).
    pub fn record_injection(&mut self, tokens: u64) {
        self.requests = self.requests.saturating_add(1);
        self.injected_tokens = self.injected_tokens.saturating_add(tokens);
        self.updated_at = Some(chrono::Utc::now());
    }

    /// Record a lifecycle event and its optional provider usage.
    pub fn record_lifecycle(
        &mut self,
        event: &str,
        input_tokens: u64,
        output_tokens: u64,
        cost_usd: f64,
    ) {
        self.last_event = Some(event.to_string());
        self.input_tokens = self.input_tokens.saturating_add(input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(output_tokens);
        self.cost_usd += cost_usd.max(0.0);
        self.updated_at = Some(chrono::Utc::now());
    }

    /// Increase recency/frequency weight for touched files and symbols.
    pub fn touch_context<'a>(
        &mut self,
        files: impl IntoIterator<Item = &'a str>,
        symbols: impl IntoIterator<Item = &'a str>,
    ) {
        for file in files {
            *self.hot_files.entry(file.to_string()).or_default() += 1;
        }
        for symbol in symbols {
            *self.hot_symbols.entry(symbol.to_string()).or_default() += 1;
        }
        trim_hot_items(&mut self.hot_files);
        trim_hot_items(&mut self.hot_symbols);
        self.updated_at = Some(chrono::Utc::now());
    }

    /// Tokens saved
    pub fn savings_tokens(&self) -> i64 {
        self.raw_tokens as i64 - self.tldr_tokens as i64
    }

    /// Savings as percentage (0-100)
    pub fn savings_percent(&self) -> f64 {
        if self.raw_tokens == 0 {
            return 0.0;
        }
        (self.savings_tokens() as f64 / self.raw_tokens as f64) * 100.0
    }
}

fn trim_hot_items(items: &mut std::collections::BTreeMap<String, u64>) {
    if items.len() <= MAX_SESSION_HOT_ITEMS {
        return;
    }
    let mut ranked = items
        .iter()
        .map(|(key, weight)| (key.clone(), *weight))
        .collect::<Vec<_>>();
    ranked.sort_unstable_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    ranked.truncate(MAX_SESSION_HOT_ITEMS);
    *items = ranked.into_iter().collect();
}

/// Per-hook activity statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookStats {
    /// Hook name
    pub hook_name: String,

    /// Total invocations
    pub invocations: u64,

    /// Successful invocations
    pub successes: u64,

    /// Failed invocations
    pub failures: u64,

    /// Hook-specific metrics (e.g., errors_found, queries_routed)
    #[serde(default)]
    pub metrics: HashMap<String, f64>,

    /// When tracking started (ISO 8601 timestamp)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl HookStats {
    /// Create a new hook stats tracker
    pub fn new(hook_name: String) -> Self {
        Self {
            hook_name,
            invocations: 0,
            successes: 0,
            failures: 0,
            metrics: HashMap::new(),
            started_at: Some(chrono::Utc::now()),
        }
    }

    /// Record a hook invocation
    pub fn record_invocation(&mut self, success: bool, metrics: Option<HashMap<String, f64>>) {
        self.invocations += 1;
        if success {
            self.successes += 1;
        } else {
            self.failures += 1;
        }
        if let Some(m) = metrics {
            for (key, value) in m {
                *self.metrics.entry(key).or_insert(0.0) += value;
            }
        }
    }

    /// Success rate as percentage (0-100)
    pub fn success_rate(&self) -> f64 {
        if self.invocations == 0 {
            return 100.0;
        }
        (self.successes as f64 / self.invocations as f64) * 100.0
    }
}

/// Aggregated global stats (from JSONL store)
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GlobalStats {
    /// Total number of invocations across all sessions
    pub total_invocations: u64,

    /// Estimated tokens saved across all sessions
    pub estimated_tokens_saved: i64,

    /// Total raw tokens processed
    pub raw_tokens_total: u64,

    /// Total TLDR tokens returned
    pub tldr_tokens_total: u64,

    /// Savings percentage (0-100)
    pub savings_percent: f64,
}

/// Cache file info for cache stats
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CacheFileInfo {
    /// Number of cache files
    pub file_count: usize,

    /// Total size in bytes
    pub total_bytes: u64,

    /// Size formatted as human-readable
    pub total_size_human: String,
}

/// Summary of all active sessions
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AllSessionsSummary {
    /// Number of active sessions
    pub active_sessions: usize,

    /// Total raw tokens across all sessions
    pub total_raw_tokens: u64,

    /// Total TLDR tokens across all sessions
    pub total_tldr_tokens: u64,

    /// Total requests across all sessions
    pub total_requests: u64,

    /// Provider-reported input tokens.
    #[serde(default)]
    pub total_input_tokens: u64,

    /// Provider-reported output tokens.
    #[serde(default)]
    pub total_output_tokens: u64,

    /// Context tokens injected by tldr.
    #[serde(default)]
    pub total_injected_tokens: u64,

    /// Provider-reported cost.
    #[serde(default)]
    pub total_cost_usd: f64,
}

// =============================================================================
// IPC Message Types
// =============================================================================

/// Command sent to daemon via socket
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum DaemonCommand {
    /// Health check
    Ping,

    /// Get daemon status
    Status {
        /// Optional session ID to get session-specific stats
        #[serde(skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },

    /// Graceful shutdown
    Shutdown,

    /// File change notification.
    ///
    /// TLDR-7xz.6: the IPC leg of the external poke (`tldr daemon notify`,
    /// driven by git/editor hooks). Lands in `handle_notify ->
    /// process_dirty_file` — the same single invalidation/re-index funnel the
    /// in-daemon watcher uses. See notify.rs for the full role description.
    Notify {
        /// Path to the changed file
        file: PathBuf,
    },

    /// Track hook activity
    Track {
        /// Hook name
        hook: String,
        /// Whether invocation was successful
        #[serde(default = "default_true")]
        success: bool,
        /// Hook-specific metrics
        #[serde(default)]
        metrics: HashMap<String, f64>,
    },

    /// Build a bounded context pack for an agent lifecycle hook.
    Inject {
        /// Stable agent conversation identifier.
        session: String,
        /// Lifecycle event (`UserPromptSubmit`, `SessionStart`, `PostCompact`,
        /// `PostToolUse`, or `SessionEnd`).
        event: String,
        /// User prompt for prompt-time relevance ranking.
        #[serde(default)]
        prompt: String,
        /// Session-start/compaction source (`startup`, `resume`, `compact`, ...).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        /// Files observed in tool input or external hook telemetry.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<String>,
        /// Symbols observed in hook telemetry.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        symbols: Vec<String>,
        /// Maximum context tokens, estimated conservatively at four chars/token.
        #[serde(default = "default_context_tokens")]
        max_tokens: usize,
        /// Provider usage fields, when the host exposes them.
        #[serde(default)]
        input_tokens: u64,
        #[serde(default)]
        output_tokens: u64,
        #[serde(default)]
        cost_usd: f64,
    },

    /// Warm call graph cache
    Warm {
        /// Optional language filter
        #[serde(default)]
        language: Option<String>,
        /// Optional final correlated metrics report.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metrics_path: Option<PathBuf>,
        /// Optional exact per-unit JSONL stream.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metrics_detail_path: Option<PathBuf>,
        /// Owning build run identity.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
    },

    /// Semantic search (if model loaded)
    Semantic {
        /// Search query
        query: String,
        /// Number of results to return
        #[serde(default = "default_top_k")]
        top_k: usize,
        /// Optional embedding-model override (e.g. `"arctic-l"`). `None` resolves
        /// from project config — kept identical to the cold CLI path so warm and
        /// cold rank the same model (TLDR-atc). Backward-compatible: pre-atc
        /// clients that omit this still deserialize.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Minimum similarity threshold. `None` => 0.0 (no score cutoff),
        /// matching the cold CLI default (TLDR-h27) so the warm path does not
        /// silently hide correct top-ranked matches.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        threshold: Option<f64>,
        /// Fuse the resident lexical and dense rankings with RRF.
        #[serde(default)]
        hybrid: bool,
        /// Lexical/dense language scope for hybrid search.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        languages: Vec<Language>,
    },

    // Pass-through analysis commands
    /// Enriched lexical search over a generation-pinned resident index.
    Search {
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
        language: Language,
        #[serde(default = "default_top_k")]
        top_k: usize,
        #[serde(default = "default_true")]
        include_callgraph: bool,
        #[serde(default)]
        regex: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<String>,
    },

    /// Find references from stored identifier occurrences.
    References {
        symbol: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<Language>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        kinds: Vec<ReferenceKind>,
        #[serde(default)]
        scope: SearchScope,
        #[serde(default = "default_reference_limit")]
        limit: usize,
        #[serde(default)]
        include_definition: bool,
    },

    /// Resolve a project-wide symbol definition from stored definitions.
    Definition {
        symbol: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<Language>,
    },

    /// Build the import dependency graph from stored imports.
    Deps {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<Language>,
        #[serde(default)]
        include_external: bool,
        #[serde(default)]
        collapse_packages: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_depth: Option<usize>,
        #[serde(default)]
        show_cycles_only: bool,
        #[serde(default = "default_cycle_length")]
        max_cycle_length: usize,
    },

    /// Compute project-wide coupling from stored call edges and imports.
    Coupling {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<Language>,
        #[serde(default = "default_max_pairs")]
        max_pairs: usize,
        #[serde(default)]
        martin_top: usize,
        #[serde(default)]
        cycles_only: bool,
    },

    /// Extract file information
    Extract {
        file: PathBuf,
        session: Option<String>,
        /// Optional language hint (CLI `--lang` / sibling-aware widening).
        /// TLDR-7pp.1.5 flag parity: previously the daemon path dropped the
        /// hint and used plain extension detection. Accepts the legacy `lang`
        /// key for v0.2.x clients.
        #[serde(default, alias = "lang", skip_serializing_if = "Option::is_none")]
        language: Option<Language>,
    },

    /// Get file tree
    Tree {
        path: Option<PathBuf>,
        /// Normalized extension filters (each already includes a leading dot,
        /// e.g. ".py"). Empty = no filter. TLDR-7pp.1.5 flag parity: the CLI's
        /// `--ext` was previously dropped on the daemon path.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        extensions: Vec<String>,
        /// Include hidden files/dirs (CLI `--include-hidden`). Previously the
        /// daemon always skipped hidden, diverging from local compute.
        #[serde(default)]
        include_hidden: bool,
    },

    /// Get code structure
    Structure {
        path: PathBuf,
        /// Optional language hint. Canonical wire name is `language` (matches
        /// the seven M1-threaded variants); the legacy `lang` form is still
        /// accepted via serde alias for v0.2.x clients.
        #[serde(
            default,
            rename = "language",
            alias = "lang",
            skip_serializing_if = "Option::is_none"
        )]
        lang: Option<String>,
        /// Maximum number of files to process (0 = unlimited). TLDR-7pp.1.5
        /// flag parity: the CLI's `--max-results` was previously dropped on the
        /// daemon path (the handler hardcoded 0).
        #[serde(default)]
        max_results: usize,
    },

    /// Get context for entry point
    Context {
        entry: String,
        depth: Option<usize>,
        /// Optional language override. Falls back to auto-detection when
        /// `None`. Accepts the legacy `lang` key for v0.2.x clients.
        #[serde(default, alias = "lang", skip_serializing_if = "Option::is_none")]
        language: Option<Language>,
        /// Project root to build context over. TLDR-7pp.1.5 flag parity: the
        /// CLI resolves a project path (positional / `--project` / inferred
        /// from a `<file>:<func>` shorthand) that the daemon previously ignored
        /// in favor of its own project root. Falls back to the daemon project
        /// when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
        /// Include function docstrings (CLI `--include-docstrings`). TLDR-7pp.1.5
        /// flag parity: the daemon path previously hardcoded `true`, diverging
        /// from the CLI default (`false`).
        #[serde(default)]
        include_docstrings: bool,
        /// Restrict to functions in this file (CLI `--file` / `<file>:<func>`
        /// shorthand). TLDR-7pp.1.5 flag parity: previously dropped on the wire.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<PathBuf>,
    },

    /// Get call graph
    Calls {
        path: Option<PathBuf>,
        /// Optional language override. Falls back to auto-detection when
        /// `None`. Accepts the legacy `lang` key for v0.2.x clients.
        #[serde(default, alias = "lang", skip_serializing_if = "Option::is_none")]
        language: Option<Language>,
        /// Respect .gitignore/.tldrignore patterns (CLI `--respect-ignore`,
        /// default true). TLDR-7pp.1.5 flag parity: previously dropped on the
        /// daemon path. `#[serde(default = ...)]` keeps the CLI default for
        /// clients that omit it.
        #[serde(default = "default_true")]
        respect_ignore: bool,
        /// Maximum edges in output (CLI `--max-items`, default 200).
        /// TLDR-7pp.1.5 flag parity: previously the daemon path neither
        /// truncated nor reported truncation.
        #[serde(default = "default_max_items")]
        max_items: usize,
    },

    /// Detect graph hubs from the resident generation.
    Hubs {
        /// `all`, `indegree`, `outdegree`, `pagerank`, or `betweenness`.
        algorithm: String,
        /// Language projection selected by the CLI.
        language: Language,
        #[serde(default = "default_top_k")]
        top: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        threshold: Option<f64>,
    },

    /// Get impact analysis
    Impact {
        func: String,
        depth: Option<usize>,
        /// Optional root-relative or suffix file discriminator.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<PathBuf>,
        /// Optional language override. Falls back to auto-detection when
        /// `None`. Accepts the legacy `lang` key for v0.2.x clients.
        #[serde(default, alias = "lang", skip_serializing_if = "Option::is_none")]
        language: Option<Language>,
    },

    /// Find dead code
    Dead {
        path: Option<PathBuf>,
        entry: Option<Vec<String>>,
        /// Optional language override. Falls back to auto-detection when
        /// `None`. Accepts the legacy `lang` key for v0.2.x clients.
        #[serde(default, alias = "lang", skip_serializing_if = "Option::is_none")]
        language: Option<Language>,
        /// Use the call-graph analyzer instead of the default reference
        /// counting (CLI `--call-graph`). TLDR-7pp.1.5 flag parity: previously
        /// the daemon path ALWAYS used the call-graph analyzer, diverging from
        /// the CLI's default refcount analysis.
        #[serde(default)]
        call_graph: bool,
        /// Walk vendored/build dirs normally skipped (CLI `--no-default-ignore`).
        #[serde(default)]
        no_default_ignore: bool,
    },

    /// Get imports for a file
    Imports {
        file: PathBuf,
        /// Optional language hint (CLI `--lang`). TLDR-7pp.1.5 flag parity:
        /// previously the daemon path dropped the hint and re-detected from the
        /// extension. Accepts the legacy `lang` key for v0.2.x clients.
        #[serde(default, alias = "lang", skip_serializing_if = "Option::is_none")]
        language: Option<Language>,
    },

    /// Find files that import a module
    Importers {
        module: String,
        path: Option<PathBuf>,
        /// Optional language override. Falls back to auto-detection when
        /// `None`. Accepts the legacy `lang` key for v0.2.x clients.
        #[serde(default, alias = "lang", skip_serializing_if = "Option::is_none")]
        language: Option<Language>,
    },

    /// Calculate complexity metrics for one function in a file.
    ///
    /// TLDR-7pp.1.3: previously `tldr complexity` routed to the endpoint
    /// `"complexity"` which had NO variant here, so the daemon dropped the
    /// connection and the CLI silently computed locally. This variant gives it
    /// a real compute-on-miss handler.
    Complexity {
        file: PathBuf,
        function: String,
        /// Optional language override. Falls back to auto-detection from the
        /// file path when `None`. Accepts the legacy `lang` key.
        #[serde(default, alias = "lang", skip_serializing_if = "Option::is_none")]
        language: Option<Language>,
    },

    /// Detect code smells over a path (file or directory).
    ///
    /// TLDR-7pp.1.3: companion fix to `Complexity` — `tldr smells` had the same
    /// missing-variant silent-fallback bug. The full flag envelope travels on
    /// the wire so the daemon produces output identical to local compute.
    Smells {
        path: PathBuf,
        /// Threshold preset. Serializes as "strict"/"default"/"relaxed".
        #[serde(default)]
        threshold: ThresholdPreset,
        /// Optional smell-type filter (snake_case value, e.g. "god_class").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        smell_type: Option<SmellType>,
        /// Include fix suggestions.
        #[serde(default)]
        suggest: bool,
        /// Deep analysis (aggregate cohesion/coupling/dead/clone/cognitive).
        #[serde(default)]
        deep: bool,
        /// Walk vendored/build dirs that are normally ignored.
        #[serde(default)]
        no_default_ignore: bool,
        /// Explicit file list (already validated by the CLI). Empty => walk.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<PathBuf>,
        /// Include findings from test files.
        #[serde(default)]
        include_tests: bool,
        /// Optional language filter.
        #[serde(default, alias = "lang", skip_serializing_if = "Option::is_none")]
        language: Option<Language>,
    },
}

fn default_true() -> bool {
    true
}

fn default_top_k() -> usize {
    10
}

fn default_reference_limit() -> usize {
    20
}

fn default_max_pairs() -> usize {
    20
}

fn default_cycle_length() -> usize {
    10
}

fn default_context_tokens() -> usize {
    2_000
}

/// Bounded context returned to an agent lifecycle hook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPack {
    /// Hook-ready factual context. Empty means graceful no-op.
    pub content: String,
    /// Conservative token estimate.
    pub tokens: usize,
    /// Files represented in the pack.
    pub files: Vec<String>,
    /// Symbols represented in the pack.
    pub symbols: Vec<String>,
    /// Artifact generation pinned for the request.
    pub generation: u64,
    /// Context construction latency.
    pub elapsed_ms: f64,
    /// Whether the token/character boundary elided candidates.
    pub truncated: bool,
    /// `prompt`, `session`, `compaction`, or `project`.
    pub source: String,
}

/// Serde default for [`DaemonCommand::Calls::max_items`] — mirrors the CLI
/// `--max-items` default so clients that omit it get identical truncation.
fn default_max_items() -> usize {
    200
}

/// Liveness observability (TLDR-qzc): answers "what is keeping the daemon
/// alive" and "when will it idle out" — per-source presence ages, live busy
/// tokens with age (a hung build is VISIBLE as `busy 4h: warm-build`, not
/// silently immortal), and the computed idle deadline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LivenessStats {
    /// Seconds since each source last proved presence, keyed by source name
    /// (`socket` / `cli_poke` / `watcher` / `internal`). BTreeMap for stable
    /// key order in output.
    pub presence_age_secs: std::collections::BTreeMap<String, f64>,
    /// Live internal work, oldest first. Non-empty means idle shutdown is
    /// unconditionally deferred ("never abandon your own job").
    pub busy: Vec<BusyTokenStats>,
    /// The configured idle timeout.
    pub idle_timeout_secs: u64,
    /// Seconds until idle shutdown if no further presence arrives. `None`
    /// while busy (the deadline does not run during internal work).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_shutdown_in_secs: Option<f64>,
}

/// One live unit of internal daemon work (TLDR-qzc).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusyTokenStats {
    /// What the work is (`warm-build`, `delta`).
    pub label: String,
    /// How long it has been running.
    pub age_secs: f64,
}

/// Resident semantic index state (TLDR-qzc): kills the "is it building or
/// done?" blindness during a multi-minute warm build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticIndexStats {
    /// `warm` | `building` | `cold`.
    pub state: String,
    /// Vector count when warm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vectors: Option<usize>,
    /// Workload-specific embedding session state.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runners: Vec<InferenceRunnerStats>,
    /// Latest reconciled semantic build progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<tldr_core::semantic::BuildProgress>,
}

/// One query, delta, or bulk inference runner in daemon status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceRunnerStats {
    pub workload: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub sessions_built: u64,
    pub requests: u64,
    pub failures: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exact_shapes: Vec<(usize, usize)>,
}

/// Daemon process memory (TLDR-yll): the observability counterweight to
/// presence-based liveness — a never-idle daemon's footprint must be a
/// visible number. Best-effort per platform; absent fields mean unreadable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStats {
    /// Current resident set size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_bytes: Option<u64>,
    /// Peak (high-water) resident set size in bytes since daemon start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_rss_bytes: Option<u64>,
}

/// Shared artifact-store status. Optional on the wire for old-daemon
/// compatibility; authoritative in the new runtime.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactStoreStats {
    /// `cold`, `building`, `ready`, or `failed`.
    pub state: String,
    /// Complete generation currently visible to readers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_generation: Option<u64>,
    /// Generation currently being built.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_generation: Option<u64>,
    /// Normalized files in the resident generation snapshot.
    pub files: usize,
    /// Aggregate tree-sitter recovery nodes in the resident generation.
    #[serde(default)]
    pub parse_errors: usize,
    /// Authoritative redb bytes on disk.
    pub redb_bytes: u64,
    /// Last ingestion failure, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Response from daemon
///
/// IMPORTANT: Variant order matters for serde(untagged)!
/// Variants are tried in declaration order, so more specific variants
/// (with more required fields) must come BEFORE less specific ones.
///
/// Key design: Error uses "error" field, Status uses "message" field.
/// This makes them structurally distinguishable for serde untagged.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
// `FullStatus` is intentionally the large, field-rich variant; this enum is
// constructed infrequently (one response per daemon request) so boxing its
// fields would add indirection and serde churn for no real benefit.
#[allow(clippy::large_enum_variant)]
pub enum DaemonResponse {
    /// Full status response (5 required fields including typed enum status)
    FullStatus {
        status: DaemonStatus,
        uptime: f64,
        files: usize,
        project: PathBuf,
        salsa_stats: SalsaCacheStats,
        #[serde(skip_serializing_if = "Option::is_none")]
        dedup_stats: Option<DedupStats>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_stats: Option<SessionStats>,
        #[serde(skip_serializing_if = "Option::is_none")]
        all_sessions: Option<AllSessionsSummary>,
        #[serde(skip_serializing_if = "Option::is_none")]
        hook_stats: Option<HashMap<String, HookStats>>,
        /// Liveness observability (TLDR-qzc). OPTIONAL-WITH-DEFAULT for
        /// untagged compat both ways: an old server's payload (field absent)
        /// still decodes as FullStatus here, and an old client simply ignores
        /// the extra key. Required-field count is unchanged, preserving the
        /// untagged variant decode order.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        liveness: Option<LivenessStats>,
        /// Resident semantic index state (TLDR-qzc). Same compat rules as
        /// `liveness`. `None` on non-semantic builds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        semantic_index: Option<SemanticIndexStats>,
        /// Daemon process memory (TLDR-yll). Same compat rules as `liveness`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        memory: Option<MemoryStats>,
        /// Authoritative shared artifact generation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_store: Option<ArtifactStoreStats>,
    },

    /// Notify response (4 required fields)
    NotifyResponse {
        status: String,
        dirty_count: usize,
        threshold: usize,
        reindex_triggered: bool,
    },

    /// Track response
    TrackResponse {
        status: String,
        hook: String,
        total_invocations: u64,
        flushed: bool,
    },

    /// Error response (uses "error" field to distinguish from Status)
    Error { status: String, error: String },

    /// Simple status response (catch-all with only 1 required field)
    Status {
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    /// Generic JSON result (for analysis commands) - MUST be last (catch-all)
    Result(serde_json::Value),
}

#[cfg(test)]
mod tests {
    use super::{DaemonConfig, SessionStats, MAX_SESSION_HOT_ITEMS};

    #[test]
    fn daemon_resolves_project_watcher_overrides() {
        let project = tempfile::tempdir().expect("temp project");
        let config_dir = project.path().join(".tldr");
        std::fs::create_dir_all(&config_dir).expect("create .tldr");
        std::fs::write(
            config_dir.join("config.json"),
            r#"{
                "watcher": {
                    "enabled": false,
                    "debounce_ms": 125,
                    "max_wait_ms": 900,
                    "burst_file_cap": 7,
                    "burst_event_cap": 11,
                    "burst_window_ms": 250
                }
            }"#,
        )
        .expect("write project config");

        let config = DaemonConfig::resolve(project.path());

        assert!(!config.enable_watcher);
        assert_eq!(config.watcher_debounce_ms, 125);
        assert_eq!(config.watcher_max_wait_ms, 900);
        assert_eq!(config.watcher_burst_file_cap, 7);
        assert_eq!(config.watcher_burst_event_cap, 11);
        assert_eq!(config.watcher_burst_window_ms, 250);
    }

    #[test]
    fn session_hot_sets_remain_bounded() {
        let mut session = SessionStats::new("bounded".into());
        let files = (0..MAX_SESSION_HOT_ITEMS * 2)
            .map(|index| format!("src/{index}.rs"))
            .collect::<Vec<_>>();
        session.touch_context(files.iter().map(String::as_str), std::iter::empty());
        assert_eq!(session.hot_files.len(), MAX_SESSION_HOT_ITEMS);
    }
}

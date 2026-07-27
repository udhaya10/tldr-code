//! L2Context -- shared context for all L2 analysis engines.
//!
//! Provides function-level change data (changed, inserted, deleted functions),
//! file contents for both baseline and current revisions, and project-wide
//! configuration. Includes DashMap-based caches for CFG, DFG, SSA, and
//! contracts data, plus OnceLock-backed call graph and change impact fields.
//!
//! # Daemon Integration (Phase 8.4)
//!
//! L2Context carries an optional daemon client that routes IR queries through
//! the daemon's QueryCache when available, falling back to on-the-fly
//! construction when no daemon is running. The daemon field is populated via
//! the `with_daemon` builder method.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use dashmap::DashMap;

use tldr_core::ssa::SsaFunction;
use tldr_core::{CfgInfo, ChangeImpactReport, DfgInfo, Language, ProjectCallGraph};

use super::daemon_client::{DaemonClient, NoDaemon};
use super::types::FunctionId;
use crate::commands::contracts::types::ContractsReport;
use crate::commands::remaining::types::ASTChange;

/// A function that changed between baseline and current revisions.
#[derive(Debug, Clone)]
pub struct FunctionChange {
    /// Unique identifier for this function.
    pub id: FunctionId,
    /// Human-readable function name.
    pub name: String,
    /// Source code in the baseline revision.
    pub old_source: String,
    /// Source code in the current revision.
    pub new_source: String,
}

/// A function that was inserted (no baseline equivalent).
#[derive(Debug, Clone)]
pub struct InsertedFunction {
    /// Unique identifier for this function.
    pub id: FunctionId,
    /// Human-readable function name.
    pub name: String,
    /// Source code of the inserted function.
    pub source: String,
}

/// A function present in baseline but absent in current revision.
#[derive(Debug, Clone)]
pub struct DeletedFunction {
    /// Unique identifier for this function.
    pub id: FunctionId,
    /// Human-readable function name.
    pub name: String,
}

/// Version discriminator for contracts cache.
///
/// Used to distinguish between baseline and current versions when caching
/// analysis results (e.g., pre-/post-conditions).
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum ContractVersion {
    /// The baseline (pre-change) revision.
    Baseline,
    /// The current (post-change) revision.
    Current,
}

/// The function-level diff between baseline and current revisions.
///
/// Groups the three categories of function changes: modified, inserted, and
/// deleted. Extracted as a separate struct to keep `L2Context::new` under
/// clippy's argument limit while maintaining a flat public API on the context.
#[derive(Debug, Clone)]
pub struct FunctionDiff {
    /// Functions whose bodies changed between revisions.
    pub changed: Vec<FunctionChange>,
    /// Functions present in current but not in baseline.
    pub inserted: Vec<InsertedFunction>,
    /// Functions present in baseline but not in current.
    pub deleted: Vec<DeletedFunction>,
}

/// Shared context for all L2 analysis engines.
///
/// Carries the project root, detected language, lists of changed/inserted/deleted
/// functions, and the full file contents for both revisions. Includes lazy-initialized
/// DashMap caches for per-function CFG, DFG, SSA, and contracts data, plus
/// OnceLock-backed project-level call graph and change impact report.
///
/// The daemon client routes IR queries through the daemon's QueryCache when
/// available, falling back to on-the-fly construction via the local caches.
pub struct L2Context {
    /// Absolute path to the project root.
    pub project: PathBuf,
    /// Detected (or user-specified) programming language.
    pub language: Language,
    /// Files that have changes between baseline and current.
    pub changed_files: Vec<PathBuf>,
    /// Function-level diff between baseline and current revisions.
    pub function_diff: FunctionDiff,
    /// Full file contents for the baseline revision, keyed by path.
    pub baseline_contents: HashMap<PathBuf, String>,
    /// Full file contents for the current revision, keyed by path.
    pub current_contents: HashMap<PathBuf, String>,
    /// AST-level changes per file from the diff phase.
    ///
    /// Maps each changed file to its list of `ASTChange` entries (Insert, Update,
    /// Delete). Used by DeltaEngine for finding extractors that need node-level
    /// diff data (e.g., `param-renamed`, `signature-regression`).
    pub ast_changes: HashMap<PathBuf, Vec<ASTChange>>,
    /// Per-function CFG cache (Sync-safe via DashMap).
    cfg_cache: DashMap<FunctionId, CfgInfo>,
    /// Per-function DFG cache (Sync-safe via DashMap).
    dfg_cache: DashMap<FunctionId, DfgInfo>,
    /// Per-function SSA cache.
    ssa_cache: DashMap<FunctionId, SsaFunction>,
    /// Per-function contracts cache keyed by (FunctionId, version).
    contracts_cache: DashMap<(FunctionId, ContractVersion), ContractsReport>,
    /// Project-level call graph (computed once, shared).
    call_graph: OnceLock<ProjectCallGraph>,
    /// Change impact report (computed once, shared).
    change_impact: OnceLock<ChangeImpactReport>,
    /// Whether this is the first run (no prior `.bugbot/state.db`).
    ///
    /// When true, delta engines that require prior state (guard-removed,
    /// contract-regression) should suppress their findings because there
    /// is no baseline to compare against (PM-34 baseline policy).
    pub is_first_run: bool,
    /// Git base reference for baseline comparison (e.g. "HEAD", "main").
    /// Used by flow engines to create baseline worktrees for project-wide diffing.
    pub base_ref: String,
    /// Optional daemon client for routing IR queries through the daemon's
    /// QueryCache. When `is_available()` returns true, cache methods check
    /// the daemon first before falling back to local computation.
    daemon: Box<dyn DaemonClient>,
}

impl L2Context {
    /// Create a new L2Context with the provided data.
    ///
    /// All cache fields (CFG, DFG, SSA, contracts, call graph, change impact)
    /// are initialized empty and populated lazily on first access. The daemon
    /// client defaults to `NoDaemon` (local-only computation).
    pub fn new(
        project: PathBuf,
        language: Language,
        changed_files: Vec<PathBuf>,
        function_diff: FunctionDiff,
        baseline_contents: HashMap<PathBuf, String>,
        current_contents: HashMap<PathBuf, String>,
        ast_changes: HashMap<PathBuf, Vec<ASTChange>>,
    ) -> Self {
        Self {
            project,
            language,
            changed_files,
            function_diff,
            baseline_contents,
            current_contents,
            ast_changes,
            cfg_cache: DashMap::new(),
            dfg_cache: DashMap::new(),
            ssa_cache: DashMap::new(),
            contracts_cache: DashMap::new(),
            call_graph: OnceLock::new(),
            change_impact: OnceLock::new(),
            is_first_run: false,
            base_ref: String::from("HEAD"),
            daemon: Box::new(NoDaemon),
        }
    }

    /// Set whether this context represents a first-run analysis.
    ///
    /// When `is_first_run` is true, delta engines that require prior state
    /// (guard-removed, contract-regression) suppress their findings because
    /// there is no baseline to compare against (PM-34 baseline policy).
    pub fn with_first_run(mut self, is_first_run: bool) -> Self {
        self.is_first_run = is_first_run;
        self
    }

    /// Set the git base reference for baseline comparison.
    ///
    /// Used by flow engines (e.g. TldrDifferentialEngine) to create
    /// baseline worktrees for project-wide diffing of call graphs,
    /// dependencies, coupling, and cohesion.
    pub fn with_base_ref(mut self, base_ref: String) -> Self {
        self.base_ref = base_ref;
        self
    }

    /// Attach a daemon client to this context.
    ///
    /// When the daemon client reports `is_available() == true`, IR cache
    /// methods (cfg_for, dfg_for, ssa_for, call_graph) will check the daemon
    /// first before falling back to local computation. The daemon is also
    /// notified of `changed_files` for cache invalidation.
    pub fn with_daemon(mut self, daemon: Box<dyn DaemonClient>) -> Self {
        // Notify daemon of changed files so it can invalidate stale caches
        // before any queries are made in this analysis session.
        daemon.notify_changed_files(&self.changed_files);
        self.daemon = daemon;
        self
    }

    /// Check whether a daemon is available for this context.
    pub fn daemon_available(&self) -> bool {
        self.daemon.is_available()
    }

    /// Get a reference to the daemon client.
    pub fn daemon(&self) -> &dyn DaemonClient {
        self.daemon.as_ref()
    }

    /// Convenience accessor: functions whose bodies changed between revisions.
    pub fn changed_functions(&self) -> &[FunctionChange] {
        &self.function_diff.changed
    }

    /// Convenience accessor: functions present in current but not in baseline.
    pub fn inserted_functions(&self) -> &[InsertedFunction] {
        &self.function_diff.inserted
    }

    /// Convenience accessor: functions present in baseline but not in current.
    pub fn deleted_functions(&self) -> &[DeletedFunction] {
        &self.function_diff.deleted
    }

    /// Get or build the CFG for a function.
    ///
    /// Checks the local cache first, then queries the daemon if available.
    /// On miss, builds via `ir::build_cfg_for_function()` and stores the result.
    pub fn cfg_for(
        &self,
        file_contents: &str,
        function_id: &FunctionId,
        language: Language,
    ) -> anyhow::Result<dashmap::mapref::one::Ref<'_, FunctionId, CfgInfo>> {
        if let Some(entry) = self.cfg_cache.get(function_id) {
            return Ok(entry);
        }
        // Check daemon cache before computing locally
        if let Some(cached) = self.daemon.query_cfg(function_id) {
            self.cfg_cache.insert(function_id.clone(), cached);
            return Ok(self.cfg_cache.get(function_id).unwrap());
        }
        let cfg = super::ir::build_cfg_for_function(file_contents, function_id, language)?;
        self.cfg_cache.insert(function_id.clone(), cfg);
        Ok(self.cfg_cache.get(function_id).unwrap())
    }

    /// Get or build the DFG for a function.
    ///
    /// Checks the local cache first, then queries the daemon if available.
    /// On miss, builds via `ir::build_dfg_for_function()` and stores the result.
    pub fn dfg_for(
        &self,
        file_contents: &str,
        function_id: &FunctionId,
        language: Language,
    ) -> anyhow::Result<dashmap::mapref::one::Ref<'_, FunctionId, DfgInfo>> {
        if let Some(entry) = self.dfg_cache.get(function_id) {
            return Ok(entry);
        }
        // Check daemon cache before computing locally
        if let Some(cached) = self.daemon.query_dfg(function_id) {
            self.dfg_cache.insert(function_id.clone(), cached);
            return Ok(self.dfg_cache.get(function_id).unwrap());
        }
        let dfg = super::ir::build_dfg_for_function(file_contents, function_id, language)?;
        self.dfg_cache.insert(function_id.clone(), dfg);
        Ok(self.dfg_cache.get(function_id).unwrap())
    }

    /// Get or build the SSA for a function.
    ///
    /// Checks the local cache first, then queries the daemon if available.
    /// On miss, builds via `ir::build_ssa_for_function()` and stores the result.
    pub fn ssa_for(
        &self,
        file_contents: &str,
        function_id: &FunctionId,
        language: Language,
    ) -> anyhow::Result<dashmap::mapref::one::Ref<'_, FunctionId, SsaFunction>> {
        if let Some(entry) = self.ssa_cache.get(function_id) {
            return Ok(entry);
        }
        // Check daemon cache before computing locally
        if let Some(cached) = self.daemon.query_ssa(function_id) {
            self.ssa_cache.insert(function_id.clone(), cached);
            return Ok(self.ssa_cache.get(function_id).unwrap());
        }
        let ssa = super::ir::build_ssa_for_function(file_contents, function_id, language)?;
        self.ssa_cache.insert(function_id.clone(), ssa);
        Ok(self.ssa_cache.get(function_id).unwrap())
    }

    /// Get or insert a contracts report for a (function, version) pair.
    ///
    /// Checks the cache first; on miss, calls `build_fn` to produce the report
    /// and stores it.
    pub fn contracts_for(
        &self,
        function_id: &FunctionId,
        version: ContractVersion,
        build_fn: impl FnOnce() -> anyhow::Result<ContractsReport>,
    ) -> anyhow::Result<dashmap::mapref::one::Ref<'_, (FunctionId, ContractVersion), ContractsReport>>
    {
        let key = (function_id.clone(), version);
        if let Some(entry) = self.contracts_cache.get(&key) {
            return Ok(entry);
        }
        let report = build_fn()?;
        self.contracts_cache.insert(key.clone(), report);
        Ok(self.contracts_cache.get(&key).unwrap())
    }

    /// Get the cached call graph, if available.
    ///
    /// Checks the local OnceLock cache first. If empty and a daemon is
    /// available, queries the daemon for a cached call graph and stores
    /// it locally for subsequent accesses.
    pub fn call_graph(&self) -> Option<&ProjectCallGraph> {
        if let Some(cg) = self.call_graph.get() {
            return Some(cg);
        }
        // Check daemon cache before giving up
        if let Some(cached) = self.daemon.query_call_graph() {
            // OnceLock::set may fail if another thread set it concurrently
            let _ = self.call_graph.set(cached);
            return self.call_graph.get();
        }
        None
    }

    /// Set the call graph (can only be set once).
    pub fn set_call_graph(&self, cg: ProjectCallGraph) -> Result<(), ProjectCallGraph> {
        self.call_graph.set(cg)
    }

    /// Get the cached change impact report, if available.
    pub fn change_impact(&self) -> Option<&ChangeImpactReport> {
        self.change_impact.get()
    }

    /// Set the change impact report (can only be set once).
    ///
    /// Returns `Err` with the boxed report if the value was already set.
    pub fn set_change_impact(
        &self,
        report: ChangeImpactReport,
    ) -> Result<(), Box<ChangeImpactReport>> {
        self.change_impact.set(report).map_err(Box::new)
    }
}

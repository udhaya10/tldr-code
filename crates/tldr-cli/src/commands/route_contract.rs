//! Authoritative execution-owner contract for top-level CLI commands.
//!
//! This registry is deliberately independent of clap rendering.  It exists so
//! adding a command also requires an explicit architecture decision instead of
//! silently inheriting whichever local or daemon path happens to be convenient.

use std::collections::HashSet;

/// The subsystem that owns a supported command's authoritative execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionOwner {
    /// Query an immutable [`tldr_core::artifact_store::GenerationSnapshot`].
    ArtifactProjection,
    /// Query a generation-pinned resident derived index.
    ResidentIndex,
    /// Deliberately compute in the CLI because the work is file-local or the
    /// daemon protocol cannot represent it without changing semantics.
    IntentionalLocal,
    /// Orchestrate an external compiler, linter, coverage tool, or git process.
    ExternalTool,
    /// Manage daemon/session/setup/cache lifecycle.
    Lifecycle,
    /// Mutate code or build/publish an index.
    Mutation,
    /// Exposed command that intentionally fails closed pending a typed runtime
    /// implementation.
    Parked,
}

/// One architecture decision for a top-level [`crate::Command`] variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandCapability {
    pub variant: &'static str,
    pub owner: ExecutionOwner,
    pub daemon_command: Option<&'static str>,
    pub rationale: &'static str,
}

/// Supported caller class for a daemon protocol variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolClient {
    /// A classified top-level analysis command.
    TopLevelCommand,
    /// Daemon lifecycle/status/notification plumbing.
    Lifecycle,
    /// Agent hook/session integration.
    AgentHook,
    /// The CLI and MCP server share this query.
    CliAndMcp,
}

/// One explicit consumer contract for a [`DaemonCommand`](super::daemon::DaemonCommand).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolCapability {
    pub variant: &'static str,
    pub wire_name: &'static str,
    pub client: ProtocolClient,
}

macro_rules! protocol {
    ($variant:literal, $wire:literal, $client:ident) => {
        ProtocolCapability {
            variant: $variant,
            wire_name: $wire,
            client: ProtocolClient::$client,
        }
    };
}

/// Exhaustive daemon protocol inventory. The integration test compares this
/// directly with the serde enum, so adding a handler without declaring its
/// supported caller fails CI.
pub const PROTOCOL_CAPABILITIES: &[ProtocolCapability] = &[
    protocol!("Ping", "ping", Lifecycle),
    protocol!("Status", "status", Lifecycle),
    protocol!("Shutdown", "shutdown", Lifecycle),
    protocol!("Notify", "notify", Lifecycle),
    protocol!("Track", "track", AgentHook),
    protocol!("Inject", "inject", AgentHook),
    protocol!("Warm", "warm", Lifecycle),
    protocol!("Semantic", "semantic", CliAndMcp),
    protocol!("Search", "search", CliAndMcp),
    protocol!("References", "references", TopLevelCommand),
    protocol!("Definition", "definition", TopLevelCommand),
    protocol!("Deps", "deps", TopLevelCommand),
    protocol!("Coupling", "coupling", TopLevelCommand),
    protocol!("Extract", "extract", TopLevelCommand),
    protocol!("Tree", "tree", TopLevelCommand),
    protocol!("Structure", "structure", TopLevelCommand),
    protocol!("Context", "context", TopLevelCommand),
    protocol!("Calls", "calls", TopLevelCommand),
    protocol!("Hubs", "hubs", TopLevelCommand),
    protocol!("Impact", "impact", TopLevelCommand),
    protocol!("Dead", "dead", TopLevelCommand),
    protocol!("Imports", "imports", TopLevelCommand),
    protocol!("Importers", "importers", TopLevelCommand),
    protocol!("Complexity", "complexity", TopLevelCommand),
    protocol!("Smells", "smells", TopLevelCommand),
];

macro_rules! capability {
    ($variant:literal, $owner:ident, $daemon:expr, $rationale:literal) => {
        CommandCapability {
            variant: $variant,
            owner: ExecutionOwner::$owner,
            daemon_command: $daemon,
            rationale: $rationale,
        }
    };
}

/// Complete top-level command inventory.
///
/// The integration test compares this list with the clap enum in `main.rs`.
/// Keep entries ordered like the enum so reviews can see ownership changes.
pub const COMMAND_CAPABILITIES: &[CommandCapability] = &[
    capability!(
        "Tree",
        ArtifactProjection,
        Some("tree"),
        "stored project file inventory"
    ),
    capability!(
        "Structure",
        ArtifactProjection,
        Some("structure"),
        "stored definitions and imports"
    ),
    capability!(
        "Calls",
        ArtifactProjection,
        Some("calls"),
        "stored call edges"
    ),
    capability!(
        "Impact",
        ArtifactProjection,
        Some("impact"),
        "stored reverse call graph"
    ),
    capability!(
        "Dead",
        ArtifactProjection,
        Some("dead"),
        "stored definitions and references"
    ),
    capability!(
        "ReachingDefs",
        IntentionalLocal,
        None,
        "single-function data-flow analysis"
    ),
    capability!("Taint", IntentionalLocal, None, "function CFG/DFG analysis"),
    capability!(
        "Available",
        IntentionalLocal,
        None,
        "single-function expression analysis"
    ),
    capability!(
        "Slice",
        IntentionalLocal,
        None,
        "direction and variable-sensitive local analysis"
    ),
    capability!(
        "SmartSearch",
        ResidentIndex,
        Some("search"),
        "resident BM25 with stored enrichment"
    ),
    capability!(
        "Context",
        ArtifactProjection,
        Some("context"),
        "stored graph and definitions"
    ),
    capability!(
        "Smells",
        ArtifactProjection,
        Some("smells"),
        "daemon-owned project analysis"
    ),
    capability!(
        "Extract",
        ArtifactProjection,
        Some("extract"),
        "stored file facts"
    ),
    capability!(
        "Imports",
        ArtifactProjection,
        Some("imports"),
        "stored imports"
    ),
    capability!(
        "Importers",
        ArtifactProjection,
        Some("importers"),
        "stored reverse imports"
    ),
    capability!(
        "Complexity",
        ArtifactProjection,
        Some("complexity"),
        "daemon-owned file analysis"
    ),
    capability!("Churn", ExternalTool, None, "git history orchestration"),
    capability!("Debt", IntentionalLocal, None, "aggregate local analysis"),
    capability!(
        "Health",
        IntentionalLocal,
        None,
        "aggregate analysis with mixed inputs"
    ),
    capability!(
        "Hubs",
        ArtifactProjection,
        Some("hubs"),
        "stored call graph"
    ),
    capability!(
        "Whatbreaks",
        IntentionalLocal,
        None,
        "git target resolution plus graph analysis"
    ),
    capability!(
        "Patterns",
        IntentionalLocal,
        None,
        "project convention analysis"
    ),
    capability!(
        "Inheritance",
        IntentionalLocal,
        None,
        "specialized hierarchy extraction"
    ),
    capability!(
        "ChangeImpact",
        ExternalTool,
        None,
        "git/session selection plus extended analysis"
    ),
    capability!(
        "Deps",
        ArtifactProjection,
        Some("deps"),
        "stored import graph"
    ),
    capability!(
        "Diagnostics",
        ExternalTool,
        None,
        "compiler and linter subprocesses"
    ),
    capability!(
        "Doctor",
        ExternalTool,
        None,
        "tool installation diagnostics"
    ),
    capability!(
        "References",
        ArtifactProjection,
        Some("references"),
        "stored reference facts"
    ),
    capability!(
        "Clones",
        IntentionalLocal,
        None,
        "bounded pairwise token analysis"
    ),
    capability!("Dice", IntentionalLocal, None, "two-fragment comparison"),
    capability!("Loc", IntentionalLocal, None, "streaming file-local counts"),
    capability!(
        "Cognitive",
        IntentionalLocal,
        None,
        "file/function complexity analysis"
    ),
    capability!(
        "Halstead",
        IntentionalLocal,
        None,
        "file/function token metrics"
    ),
    capability!("Coverage", ExternalTool, None, "coverage report ingestion"),
    capability!(
        "Hotspots",
        ExternalTool,
        None,
        "git churn joined with complexity"
    ),
    capability!(
        "Embed",
        Mutation,
        None,
        "build and publish the vector index"
    ),
    capability!(
        "Semantic",
        ResidentIndex,
        Some("semantic"),
        "resident vector index"
    ),
    capability!(
        "Similar",
        Parked,
        None,
        "seeded resident-vector API not implemented"
    ),
    capability!("Daemon", Lifecycle, None, "daemon lifecycle"),
    capability!("Init", Lifecycle, None, "project lifecycle"),
    capability!("Hook", Lifecycle, None, "agent lifecycle bridge"),
    capability!("Setup", Lifecycle, None, "agent integration setup"),
    capability!(
        "Session",
        Lifecycle,
        None,
        "session lifecycle and telemetry"
    ),
    capability!("Cache", Lifecycle, None, "cache inspection and clearing"),
    capability!(
        "Embeddings",
        Lifecycle,
        None,
        "global embedding cache lifecycle"
    ),
    capability!("Warm", Mutation, None, "build and publish artifacts"),
    capability!("Stats", Lifecycle, None, "runtime telemetry"),
    capability!("Surface", IntentionalLocal, None, "package API extraction"),
    capability!(
        "Contracts",
        IntentionalLocal,
        None,
        "function contract extraction"
    ),
    capability!(
        "DeadStores",
        IntentionalLocal,
        None,
        "single-function SSA analysis"
    ),
    capability!(
        "Chop",
        IntentionalLocal,
        None,
        "single-function bidirectional slicing"
    ),
    capability!(
        "Specs",
        IntentionalLocal,
        None,
        "test specification extraction"
    ),
    capability!(
        "Invariants",
        ExternalTool,
        None,
        "test execution trace ingestion"
    ),
    capability!("Verify", IntentionalLocal, None, "aggregate verification"),
    capability!(
        "Cohesion",
        IntentionalLocal,
        None,
        "single-file class analysis"
    ),
    capability!(
        "Temporal",
        IntentionalLocal,
        None,
        "project sequence mining"
    ),
    capability!(
        "Resources",
        IntentionalLocal,
        None,
        "function resource-flow analysis"
    ),
    capability!(
        "Coupling",
        ArtifactProjection,
        Some("coupling"),
        "stored project call graph; pair mode stays local"
    ),
    capability!(
        "Interface",
        IntentionalLocal,
        None,
        "file/package interface extraction"
    ),
    capability!(
        "Explain",
        IntentionalLocal,
        None,
        "mixed file and graph analysis"
    ),
    capability!(
        "Todo",
        IntentionalLocal,
        None,
        "aggregate improvement report"
    ),
    capability!(
        "Secure",
        IntentionalLocal,
        None,
        "aggregate security analysis"
    ),
    capability!(
        "Definition",
        ArtifactProjection,
        Some("definition"),
        "stored global definitions; cursor scope stays local"
    ),
    capability!("Diff", IntentionalLocal, None, "two-file AST comparison"),
    capability!(
        "ApiCheck",
        IntentionalLocal,
        None,
        "specialized misuse rules"
    ),
    capability!(
        "Vuln",
        IntentionalLocal,
        None,
        "taint vulnerability analysis"
    ),
    capability!("Fix", Mutation, None, "optional deterministic source edits"),
    capability!(
        "Bugbot",
        ExternalTool,
        None,
        "git diff and installed quality tools"
    ),
];

/// Validate invariants useful to production diagnostics and tests.
pub fn validate_command_capabilities() -> Result<(), String> {
    let mut variants = HashSet::with_capacity(COMMAND_CAPABILITIES.len());
    let mut daemon_commands = HashSet::new();
    for capability in COMMAND_CAPABILITIES {
        if !variants.insert(capability.variant) {
            return Err(format!(
                "duplicate command capability for {}",
                capability.variant
            ));
        }
        if capability.rationale.trim().is_empty() {
            return Err(format!(
                "command capability {} has no rationale",
                capability.variant
            ));
        }
        if let Some(command) = capability.daemon_command {
            if !daemon_commands.insert(command) {
                return Err(format!("duplicate daemon command owner for {command}"));
            }
            if !matches!(
                capability.owner,
                ExecutionOwner::ArtifactProjection | ExecutionOwner::ResidentIndex
            ) {
                return Err(format!(
                    "{} declares daemon command {command} without a resident owner",
                    capability.variant
                ));
            }
        }
    }

    let mut protocol_variants = HashSet::with_capacity(PROTOCOL_CAPABILITIES.len());
    let mut wire_names = HashSet::with_capacity(PROTOCOL_CAPABILITIES.len());
    for capability in PROTOCOL_CAPABILITIES {
        if !protocol_variants.insert(capability.variant) {
            return Err(format!(
                "duplicate daemon protocol capability for {}",
                capability.variant
            ));
        }
        if !wire_names.insert(capability.wire_name) {
            return Err(format!(
                "duplicate daemon protocol wire name {}",
                capability.wire_name
            ));
        }
    }
    for daemon_command in daemon_commands {
        if !wire_names.contains(daemon_command) {
            return Err(format!(
                "top-level daemon command {daemon_command} has no protocol capability"
            ));
        }
    }
    Ok(())
}

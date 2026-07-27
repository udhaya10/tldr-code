//! L2 deep-analysis engine framework for bugbot.
//!
//! This module provides the trait, types, and registry for L2 engines that
//! perform deeper static analysis beyond diff-level heuristics. Each engine
//! targets specific finding types (e.g., null-deref, use-after-move).
//!
//! # Architecture
//!
//! ```text
//! l2_engine_registry() -> Vec<Box<dyn L2Engine>>
//!       |
//!       v
//!   for engine in engines {
//!       engine.analyze(&ctx) -> L2AnalyzerOutput
//!   }
//! ```

pub mod composition;
pub mod context;
pub mod daemon_client;
pub mod dedup;
pub mod engines;
pub mod findings;
pub mod ir;
pub mod types;

pub use context::L2Context;
pub use types::*;

/// Trait for L2 deep-analysis engines.
///
/// Implementations must be object-safe so they can be stored as
/// `Box<dyn L2Engine>` in the engine registry. Each engine declares:
///
/// - A unique name for logging and identification
/// - The finding types it can produce (e.g., `["null-deref", "uninitialized-read"]`)
/// - Which languages it supports (empty means language-agnostic)
/// - The analysis entry point
pub trait L2Engine: Send + Sync {
    /// Unique human-readable name for this engine (used in logging and reports).
    fn name(&self) -> &'static str;

    /// The set of finding type identifiers this engine can produce.
    fn finding_types(&self) -> &[&'static str];

    /// Languages this engine supports. An empty slice means language-agnostic
    /// (the engine handles all languages or performs language-independent analysis).
    fn languages(&self) -> &[tldr_core::Language] {
        &[]
    }

    /// Run analysis on the provided context and return findings.
    fn analyze(&self, ctx: &context::L2Context) -> types::L2AnalyzerOutput;
}

/// Returns the set of all registered L2 engines.
///
/// Contains the TldrDifferentialEngine that invokes `tldr` CLI commands
/// for differential analysis. The pipeline orchestrator invokes each
/// engine's `analyze` method.
pub fn l2_engine_registry() -> Vec<Box<dyn L2Engine>> {
    vec![Box::new(
        engines::tldr_differential::TldrDifferentialEngine::new(),
    )]
}

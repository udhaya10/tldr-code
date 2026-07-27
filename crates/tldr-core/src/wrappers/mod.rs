//! Wrappers module - Base infrastructure for GVN migration
//!
//! This module provides shared types and utilities for orchestrating
//! multiple sub-analyses, including:
//!
//! - `SubAnalysisResult`: Captures result of a single analysis with timing
//! - `safe_call`: Executes a closure with timing and error handling
//! - `progress`: Prints progress messages to stderr
//! - `Severity`: Enum for finding severity levels
//! - `secure`: Security analysis orchestrator (Phase P6)
//! - `todo`: Todo orchestrator for prioritized improvement suggestions (Phase P7)

mod base;
pub mod secure;
pub mod todo;
mod types;

pub use base::{progress, safe_call, SubAnalysisResult};
pub use secure::{run_secure, SecureFinding, SecureReport};
pub use todo::{run_todo, TodoItem, TodoReport, TodoSummary};
pub use types::Severity;

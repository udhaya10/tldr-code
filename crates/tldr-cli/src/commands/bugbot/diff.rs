//! Function-level AST diff for bugbot
//!
//! Wraps the existing `DiffArgs::run_to_report()` infrastructure to compare
//! a baseline file (from git) against the current working-tree version.
//! Exposes convenience helpers to categorize changes by type (inserted,
//! updated, deleted functions).

use std::path::Path;

use anyhow::Result;

use crate::commands::remaining::diff::DiffArgs;
use crate::commands::remaining::types::{
    ASTChange, ChangeType, DiffGranularity, DiffReport, NodeKind,
};

/// Compute function-level AST diff between a baseline file and the current file.
///
/// Both paths must point to existing files with the same language extension.
/// The diff is performed at function granularity with `semantic_only` enabled
/// so that whitespace/comment-only changes are excluded.
pub fn diff_functions(baseline_path: &Path, current_path: &Path) -> Result<DiffReport> {
    let diff_args = DiffArgs {
        file_a: baseline_path.to_path_buf(),
        file_b: current_path.to_path_buf(),
        granularity: DiffGranularity::Function,
        semantic_only: true,
        output: None,
    };
    diff_args.run_to_report()
}

/// Compute function-level AST diff without the semantic-only filter.
///
/// This variant preserves formatting-only changes in the report, which
/// can be useful when the caller needs to see all changes including
/// whitespace and comment modifications.
pub fn diff_functions_raw(baseline_path: &Path, current_path: &Path) -> Result<DiffReport> {
    let diff_args = DiffArgs {
        file_a: baseline_path.to_path_buf(),
        file_b: current_path.to_path_buf(),
        granularity: DiffGranularity::Function,
        semantic_only: false,
        output: None,
    };
    diff_args.run_to_report()
}

/// Returns true if the given `NodeKind` represents a function-like construct.
///
/// Currently matches `Function` and `Method`.
fn is_function_like(kind: &NodeKind) -> bool {
    matches!(kind, NodeKind::Function | NodeKind::Method)
}

/// Filter to only inserted functions (new functions added in the current file).
pub fn inserted_functions(changes: &[ASTChange]) -> Vec<&ASTChange> {
    changes
        .iter()
        .filter(|c| matches!(c.change_type, ChangeType::Insert))
        .filter(|c| is_function_like(&c.node_kind))
        .collect()
}

/// Filter to only updated functions (modified function bodies).
pub fn updated_functions(changes: &[ASTChange]) -> Vec<&ASTChange> {
    changes
        .iter()
        .filter(|c| matches!(c.change_type, ChangeType::Update))
        .filter(|c| is_function_like(&c.node_kind))
        .collect()
}

/// Filter to only deleted functions (functions removed in the current file).
pub fn deleted_functions(changes: &[ASTChange]) -> Vec<&ASTChange> {
    changes
        .iter()
        .filter(|c| matches!(c.change_type, ChangeType::Delete))
        .filter(|c| is_function_like(&c.node_kind))
        .collect()
}

/// Filter to only renamed functions (same body, different name).
pub fn renamed_functions(changes: &[ASTChange]) -> Vec<&ASTChange> {
    changes
        .iter()
        .filter(|c| matches!(c.change_type, ChangeType::Rename))
        .filter(|c| is_function_like(&c.node_kind))
        .collect()
}

/// Filter to all function-like changes regardless of change type.
pub fn all_function_changes(changes: &[ASTChange]) -> Vec<&ASTChange> {
    changes
        .iter()
        .filter(|c| is_function_like(&c.node_kind))
        .collect()
}

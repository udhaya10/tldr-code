//! Error handling pattern detection
//!
//! Detects error handling patterns across languages:
//! - Python: try/except, custom Exception classes
//! - Rust: Result<T, E>, ? operator, error enums
//! - Go: if err != nil pattern
//! - TypeScript: try/catch blocks

use super::signals::PatternSignals;
use crate::types::ErrorHandlingPattern;

/// Convert signals to error handling pattern
pub fn signals_to_pattern(
    signals: &PatternSignals,
    evidence_limit: usize,
) -> Option<ErrorHandlingPattern> {
    let error_handling = &signals.error_handling;

    if !error_handling.has_signals() {
        return None;
    }

    let confidence = error_handling.calculate_confidence();

    // Detect patterns
    let mut patterns = Vec::new();

    if !error_handling.try_except_blocks.is_empty() || !error_handling.try_catch_blocks.is_empty() {
        patterns.push("try_catch".to_string());
    }

    if !error_handling.result_types.is_empty() {
        patterns.push("result_type".to_string());
    }

    if !error_handling.question_mark_ops.is_empty() {
        patterns.push("question_mark_operator".to_string());
    }

    if !error_handling.err_nil_checks.is_empty() {
        patterns.push("err_nil_check".to_string());
    }

    if !error_handling.custom_exceptions.is_empty() || !error_handling.error_enums.is_empty() {
        patterns.push("custom_errors".to_string());
    }

    // Collect exception types
    let mut exception_types: Vec<String> = error_handling
        .custom_exceptions
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    exception_types.extend(
        error_handling
            .error_enums
            .iter()
            .map(|(name, _)| name.clone()),
    );
    exception_types.sort();
    exception_types.dedup();

    // Collect evidence (limited)
    let mut evidence = Vec::new();
    evidence.extend(
        error_handling
            .try_except_blocks
            .iter()
            .take(evidence_limit)
            .cloned(),
    );
    evidence.extend(
        error_handling
            .try_catch_blocks
            .iter()
            .take(evidence_limit)
            .cloned(),
    );
    evidence.extend(
        error_handling
            .result_types
            .iter()
            .take(evidence_limit)
            .cloned(),
    );
    evidence.extend(
        error_handling
            .err_nil_checks
            .iter()
            .take(evidence_limit)
            .cloned(),
    );
    evidence.extend(
        error_handling
            .custom_exceptions
            .iter()
            .take(evidence_limit)
            .map(|(_, e)| e.clone()),
    );
    evidence.truncate(evidence_limit);

    Some(ErrorHandlingPattern {
        confidence,
        patterns,
        exception_types,
        evidence,
    })
}

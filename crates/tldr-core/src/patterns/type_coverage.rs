//! Type annotation coverage pattern detection
//!
//! Detects type annotation patterns:
//! - Function parameter type coverage
//! - Return type coverage
//! - Variable annotation coverage
//! - Generic type usage (TypeVar, Generic[])

use super::signals::PatternSignals;
use crate::types::{Evidence, TypeCoveragePattern};

/// Convert signals to type coverage pattern
pub fn signals_to_pattern(
    signals: &PatternSignals,
    evidence_limit: usize,
) -> Option<TypeCoveragePattern> {
    let type_coverage = &signals.type_coverage;

    if !type_coverage.has_signals() {
        return None;
    }

    let coverage_overall = type_coverage.calculate_overall_coverage();
    let coverage_functions = type_coverage.calculate_function_coverage();
    let coverage_variables = type_coverage.calculate_variable_coverage();

    let typevar_usage = !type_coverage.generic_usage.is_empty();
    let generic_patterns: Vec<String> = type_coverage.generic_patterns.iter().cloned().collect();

    // Collect evidence (limited)
    let evidence: Vec<Evidence> = type_coverage
        .generic_usage
        .iter()
        .take(evidence_limit)
        .cloned()
        .collect();

    Some(TypeCoveragePattern {
        coverage_overall,
        coverage_functions,
        coverage_variables,
        typevar_usage,
        generic_patterns,
        evidence,
    })
}

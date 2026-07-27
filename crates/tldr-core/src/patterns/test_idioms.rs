//! Test idiom pattern detection
//!
//! Detects testing patterns:
//! - pytest fixtures
//! - mock.patch usage
//! - Jest describe/it blocks
//! - Go table-driven tests
//! - Arrange-Act-Assert structure

use super::signals::PatternSignals;
use crate::types::TestIdiomPattern;

/// Convert signals to test idiom pattern
pub fn signals_to_pattern(
    signals: &PatternSignals,
    evidence_limit: usize,
) -> Option<TestIdiomPattern> {
    let test_idioms = &signals.test_idioms;

    if !test_idioms.has_signals() {
        return None;
    }

    let confidence = test_idioms.calculate_confidence();

    // Detect patterns
    let mut patterns = Vec::new();

    if !test_idioms.pytest_fixtures.is_empty() {
        patterns.push("fixtures".to_string());
    }

    if !test_idioms.mock_patches.is_empty() {
        patterns.push("mocking".to_string());
    }

    if !test_idioms.jest_blocks.is_empty() {
        patterns.push("describe_it".to_string());
    }

    if !test_idioms.go_table_tests.is_empty() {
        patterns.push("table_driven".to_string());
    }

    if !test_idioms.aaa_patterns.is_empty() {
        patterns.push("arrange_act_assert".to_string());
    }

    // Fixture and mock usage
    let fixture_usage = !test_idioms.pytest_fixtures.is_empty();
    let mock_usage = !test_idioms.mock_patches.is_empty();

    // Collect evidence (limited)
    let mut evidence = Vec::new();
    evidence.extend(
        test_idioms
            .pytest_fixtures
            .iter()
            .take(evidence_limit)
            .cloned(),
    );
    evidence.extend(
        test_idioms
            .mock_patches
            .iter()
            .take(evidence_limit)
            .cloned(),
    );
    evidence.extend(test_idioms.jest_blocks.iter().take(evidence_limit).cloned());
    evidence.extend(
        test_idioms
            .go_table_tests
            .iter()
            .take(evidence_limit)
            .cloned(),
    );
    evidence.truncate(evidence_limit);

    Some(TestIdiomPattern {
        confidence,
        framework: test_idioms.detected_framework.clone(),
        patterns,
        fixture_usage,
        mock_usage,
        evidence,
    })
}

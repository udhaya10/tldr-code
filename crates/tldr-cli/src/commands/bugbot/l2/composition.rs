//! Cross-dimensional composition engine for L2 findings.
//!
//! When findings from different analysis dimensions (e.g., taint analysis +
//! guard removal, impact analysis + contract regression) co-locate on the same
//! code region, this module composes them into higher-confidence findings.
//!
//! # Composition rules
//!
//! | Finding A         | Finding B            | Composed type                    | Severity | Confidence |
//! |-------------------|----------------------|----------------------------------|----------|------------|
//! | taint-flow        | guard-removed        | unguarded-injection-path         | critical | LIKELY     |
//! | impact-blast-radius | contract-regression | high-impact-contract-regression  | high     | LIKELY     |
//! | unreachable-code  | born-dead            | broken-link                      | high     | LIKELY     |
//! | complexity-increase | (any with churn)   | hotspot                          | medium   | LIKELY     |
//! | resource-leak     | guard-removed        | resource-leak-on-error           | high     | LIKELY     |
//!
//! # Behavior
//!
//! - Composed findings REPLACE their constituent findings (no double-counting).
//! - Composed findings get confidence = "LIKELY" (two dimensions agree).
//! - Composed findings inherit the higher constituent severity or the rule
//!   severity, whichever is greater.
//! - Constituent evidence is merged into the composed finding as
//!   `constituent_a` and `constituent_b` keys.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

use crate::commands::bugbot::types::BugbotFinding;

/// Map severity string to a numeric rank for sorting (higher = more severe).
fn severity_rank(severity: &str) -> u8 {
    match severity {
        "critical" => 5,
        "high" => 4,
        "medium" => 3,
        "low" => 2,
        "info" => 1,
        _ => 0,
    }
}

/// Convert a severity rank back to a string.
fn severity_from_rank(rank: u8) -> &'static str {
    match rank {
        5 => "critical",
        4 => "high",
        3 => "medium",
        2 => "low",
        1 => "info",
        _ => "info",
    }
}

/// Compute a deterministic finding ID for composed findings.
///
/// Uses `DefaultHasher` (SipHash) over `(composed_type, file_path, function_name, line)`
/// and formats as a lowercase hex string.
fn compute_finding_id(finding_type: &str, file: &Path, function: &str, line: usize) -> String {
    let mut hasher = DefaultHasher::new();
    finding_type.hash(&mut hasher);
    file.to_string_lossy().as_ref().hash(&mut hasher);
    function.hash(&mut hasher);
    line.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

/// Determine whether two findings overlap by location.
///
/// Two findings overlap when they share the same file and function, and their
/// line numbers are within 5 of each other (composition threshold).
fn lines_overlap(a: &BugbotFinding, b: &BugbotFinding) -> bool {
    a.file == b.file
        && a.function == b.function
        && (a.line as isize - b.line as isize).unsigned_abs() <= 5
}

/// A composition rule matching two finding types to produce a composed finding.
struct CompositionRule {
    type_a: &'static str,
    type_b: &'static str,
    composed_type: &'static str,
    composed_severity: &'static str,
    /// If true, type_b is a wildcard that matches any finding with churn data in evidence.
    b_is_churn_wildcard: bool,
}

/// The set of composition rules.
const COMPOSITION_RULES: &[CompositionRule] = &[
    CompositionRule {
        type_a: "taint-flow",
        type_b: "guard-removed",
        composed_type: "unguarded-injection-path",
        composed_severity: "critical",
        b_is_churn_wildcard: false,
    },
    CompositionRule {
        type_a: "impact-blast-radius",
        type_b: "contract-regression",
        composed_type: "high-impact-contract-regression",
        composed_severity: "high",
        b_is_churn_wildcard: false,
    },
    CompositionRule {
        type_a: "unreachable-code",
        type_b: "born-dead",
        composed_type: "broken-link",
        composed_severity: "high",
        b_is_churn_wildcard: false,
    },
    CompositionRule {
        type_a: "complexity-increase",
        type_b: "",
        composed_type: "hotspot",
        composed_severity: "medium",
        b_is_churn_wildcard: true,
    },
    CompositionRule {
        type_a: "resource-leak",
        type_b: "guard-removed",
        composed_type: "resource-leak-on-error",
        composed_severity: "high",
        b_is_churn_wildcard: false,
    },
];

/// Check if a finding has churn data in its evidence.
fn has_churn_data(finding: &BugbotFinding) -> bool {
    if let Some(obj) = finding.evidence.as_object() {
        obj.contains_key("churn")
            || obj.contains_key("churn_count")
            || obj.contains_key("git_churn")
    } else {
        false
    }
}

/// Try to match a pair of findings against the composition rules.
///
/// Returns the matching rule if found, along with which finding is A and which is B.
fn match_rule(f1: &BugbotFinding, f2: &BugbotFinding) -> Option<&'static CompositionRule> {
    for rule in COMPOSITION_RULES {
        if rule.b_is_churn_wildcard {
            // type_a must match one finding, and the other must have churn data
            if f1.finding_type == rule.type_a && has_churn_data(f2) {
                return Some(rule);
            }
            if f2.finding_type == rule.type_a && has_churn_data(f1) {
                return Some(rule);
            }
        } else {
            // Both types must match (in either order)
            if f1.finding_type == rule.type_a && f2.finding_type == rule.type_b {
                return Some(rule);
            }
            if f2.finding_type == rule.type_a && f1.finding_type == rule.type_b {
                return Some(rule);
            }
        }
    }
    None
}

/// Compose a new finding from two constituents according to a composition rule.
fn compose_finding(rule: &CompositionRule, a: &BugbotFinding, b: &BugbotFinding) -> BugbotFinding {
    // Determine which is the "first" constituent (for file/function/line)
    let first = if a.finding_type == rule.type_a { a } else { b };

    // Severity: max of constituents or rule severity, whichever is higher
    let constituent_max = std::cmp::max(severity_rank(&a.severity), severity_rank(&b.severity));
    let rule_sev = severity_rank(rule.composed_severity);
    let final_severity = severity_from_rank(std::cmp::max(constituent_max, rule_sev));

    // Merge evidence
    let evidence = serde_json::json!({
        "constituent_a": {
            "finding_type": a.finding_type,
            "severity": a.severity,
            "line": a.line,
            "message": a.message,
            "evidence": a.evidence,
        },
        "constituent_b": {
            "finding_type": b.finding_type,
            "severity": b.severity,
            "line": b.line,
            "message": b.message,
            "evidence": b.evidence,
        },
    });

    let finding_id =
        compute_finding_id(rule.composed_type, &first.file, &first.function, first.line);

    BugbotFinding {
        finding_type: rule.composed_type.to_string(),
        severity: final_severity.to_string(),
        file: first.file.clone(),
        function: first.function.clone(),
        line: first.line,
        message: format!(
            "Composed: {} + {} -> {}",
            a.finding_type, b.finding_type, rule.composed_type,
        ),
        evidence,
        confidence: Some("LIKELY".to_string()),
        finding_id: Some(finding_id),
    }
}

/// Compose findings from different analysis dimensions into higher-confidence
/// findings when they co-locate on the same code region.
///
/// See module-level docs for the composition rules. Composed findings replace
/// their constituents (no double-counting).
///
/// # Arguments
/// * `findings` - The input findings (typically after dedup)
///
/// # Returns
/// A list of findings where matched pairs have been replaced by composed findings.
pub fn compose_findings(findings: Vec<BugbotFinding>) -> Vec<BugbotFinding> {
    if findings.len() < 2 {
        return findings;
    }

    let n = findings.len();
    let mut consumed = vec![false; n];
    let mut composed: Vec<BugbotFinding> = Vec::new();

    // O(N^2) scan for pairs that share location and match a rule
    for i in 0..n {
        if consumed[i] {
            continue;
        }
        for j in (i + 1)..n {
            if consumed[j] {
                continue;
            }
            if !lines_overlap(&findings[i], &findings[j]) {
                continue;
            }
            if let Some(rule) = match_rule(&findings[i], &findings[j]) {
                let new_finding = compose_finding(rule, &findings[i], &findings[j]);
                composed.push(new_finding);
                consumed[i] = true;
                consumed[j] = true;
                break; // finding i is consumed, move on
            }
        }
    }

    // Add all unconsumed findings
    for (i, finding) in findings.into_iter().enumerate() {
        if !consumed[i] {
            composed.push(finding);
        }
    }

    composed
}

//! Text output formatter for bugbot check reports
//!
//! Produces human-readable text output for terminal display, as an alternative
//! to the default JSON output. Used when `--format text` is specified.

use std::fmt::Write;

use super::types::{BugbotCheckReport, L2AnalyzerResult};

/// Format a `BugbotCheckReport` as human-readable text.
///
/// Output structure:
/// - Summary line with finding counts by severity
/// - Stats line with files/functions analyzed and elapsed time
/// - One block per finding with severity tag, location, message, and evidence
/// - Optional errors section
/// - Optional truncation note
pub fn format_bugbot_text(report: &BugbotCheckReport) -> String {
    let mut out = String::new();

    // Summary line
    if report.findings.is_empty() {
        writeln!(out, "bugbot check -- no issues found").unwrap();
    } else {
        let severity_breakdown = format_severity_breakdown(&report.summary.by_severity);
        writeln!(
            out,
            "bugbot check -- {} findings ({})",
            report.summary.total_findings, severity_breakdown
        )
        .unwrap();
    }

    // Stats line
    writeln!(
        out,
        "  {} files analyzed, {} functions, {}ms",
        report.summary.files_analyzed, report.summary.functions_analyzed, report.elapsed_ms
    )
    .unwrap();

    // Individual findings
    for finding in &report.findings {
        writeln!(out).unwrap(); // blank line separator

        // PM-42: Critical findings use [!!!CRITICAL] marker for visibility
        let tag = if finding.severity == "critical" {
            "!!!CRITICAL".to_string()
        } else {
            finding.severity.to_uppercase()
        };
        writeln!(
            out,
            "[{}] {} in {}",
            tag,
            finding.finding_type,
            finding.file.display()
        )
        .unwrap();
        // PM-4: L1 findings have empty function field. Show "line N" directly
        // instead of "functionName (line N)" when function is empty.
        if finding.function.is_empty() {
            writeln!(out, "  line {}", finding.line).unwrap();
        } else {
            writeln!(out, "  {} (line {})", finding.function, finding.line).unwrap();
        }
        writeln!(out, "  {}", finding.message).unwrap();

        // Confidence line for L2 findings (when confidence is Some)
        if let Some(ref confidence) = finding.confidence {
            writeln!(out, "  Confidence: {}", confidence).unwrap();
        }

        // Evidence lines -- type-specific rendering
        format_finding_evidence(&mut out, finding);
    }

    // Critical summary line -- appears before tools/engines sections
    let critical_count = report
        .findings
        .iter()
        .filter(|f| f.severity == "critical")
        .count();
    if critical_count > 0 {
        writeln!(out).unwrap();
        writeln!(
            out,
            "CRITICAL: {} finding(s) require immediate attention",
            critical_count
        )
        .unwrap();
    }

    // Tool results section -- shows which L1 tools ran and their status
    if !report.tool_results.is_empty() || !report.tools_missing.is_empty() {
        writeln!(out).unwrap();
        writeln!(out, "tools:").unwrap();
        for result in &report.tool_results {
            let status = if result.success {
                format!(
                    "ok ({} findings, {}ms)",
                    result.finding_count, result.duration_ms
                )
            } else {
                let err_detail = result.error.as_deref().unwrap_or("unknown error");
                format!("failed ({})", err_detail)
            };
            writeln!(out, "  {} - {}", result.name, status).unwrap();
        }
        for name in &report.tools_missing {
            writeln!(out, "  {} - skipped (not installed)", name).unwrap();
        }
        if !report.tools_missing.is_empty() {
            writeln!(
                out,
                "  hint: run `tldr doctor --install {}` to set up missing tools",
                report.language
            )
            .unwrap();
        }
    }

    // L2 engine results section
    if !report.l2_engine_results.is_empty() {
        writeln!(out).unwrap();
        writeln!(out, "L2 engines:").unwrap();
        for result in &report.l2_engine_results {
            let status_label = format_engine_status(result);
            writeln!(
                out,
                "  {} - {} ({} findings, {}ms)",
                result.name, status_label, result.finding_count, result.duration_ms
            )
            .unwrap();
            // Append partial/error detail inline
            if !result.errors.is_empty() {
                for err_detail in &result.errors {
                    writeln!(out, "    [{}]", err_detail).unwrap();
                }
            }
        }
    }

    // ANALYSIS GAPS section -- shown when any engine has errors
    let engines_with_errors: Vec<&L2AnalyzerResult> = report
        .l2_engine_results
        .iter()
        .filter(|r| !r.errors.is_empty())
        .collect();
    if !engines_with_errors.is_empty() {
        writeln!(out).unwrap();
        writeln!(out, "ANALYSIS GAPS ({}):", engines_with_errors.len()).unwrap();
        for result in engines_with_errors {
            for error in &result.errors {
                writeln!(out, "  {}: {}", result.name, error).unwrap();
            }
        }
    }

    // Errors section
    if !report.errors.is_empty() {
        writeln!(out).unwrap();
        writeln!(out, "errors:").unwrap();
        for error in &report.errors {
            writeln!(out, "  - {}", error).unwrap();
        }
    }

    // Truncation note
    for note in &report.notes {
        if let Some(rest) = note.strip_prefix("truncated_to_") {
            writeln!(out).unwrap();
            writeln!(out, "(output truncated to {} findings)", rest).unwrap();
        }
    }

    // Remove trailing newline to let write_text add its own
    let trimmed = out.trim_end_matches('\n');
    trimmed.to_string()
}

/// Format severity counts as "N high, M medium, L low", omitting zeroes.
///
/// Severities are always printed in high, medium, low order regardless of
/// HashMap iteration order.
fn format_severity_breakdown(by_severity: &std::collections::HashMap<String, usize>) -> String {
    let mut parts = Vec::new();
    // Known severities in descending order (PM-8: includes "info", PM-42: includes "critical")
    for level in &["critical", "high", "medium", "low", "info"] {
        if let Some(&count) = by_severity.get(*level) {
            if count > 0 {
                parts.push(format!("{} {}", count, level));
            }
        }
    }
    // Include any unknown severity levels
    let mut keys: Vec<&String> = by_severity
        .keys()
        .filter(|k| !["critical", "high", "medium", "low", "info"].contains(&k.as_str()))
        .collect();
    keys.sort();
    for key in keys {
        if let Some(&count) = by_severity.get(key) {
            if count > 0 {
                parts.push(format!("{} {}", count, key));
            }
        }
    }
    parts.join(", ")
}

/// Format type-specific evidence lines for a finding.
///
/// Renders evidence differently depending on finding_type:
/// - `signature-regression`: Before/After signature comparison
/// - `secret-exposed`: Masked value display
/// - `taint-flow`: Source -> Sink flow with types
/// - `born-dead`: Reference count if available
/// - `complexity-increase` / `maintainability-drop`: Before/after values
/// - `resource-leak`: Sub-type and resource name
/// - `new-clone`: Clone type and similarity percentage
/// - `impact-blast-radius`: Caller counts
/// - `temporal-violation`: Expected vs actual call order
/// - `guard-removed`: Removed variable and constraint
/// - `contract-regression`: Category, variable, and constraint
/// - Other types: Show all evidence values (strings, numbers, booleans, arrays)
fn format_finding_evidence(out: &mut String, finding: &super::types::BugbotFinding) {
    match finding.finding_type.as_str() {
        "signature-regression" => {
            if let Some(before) = finding
                .evidence
                .get("before_signature")
                .and_then(|v| v.as_str())
            {
                writeln!(out, "  Before: {}", before).unwrap();
            }
            if let Some(after) = finding
                .evidence
                .get("after_signature")
                .and_then(|v| v.as_str())
            {
                writeln!(out, "  After:  {}", after).unwrap();
            }
        }
        "secret-exposed" => {
            if let Some(val) = finding
                .evidence
                .get("masked_value")
                .and_then(|v| v.as_str())
            {
                writeln!(out, "  Value: {}", val).unwrap();
            }
        }
        "taint-flow" => {
            // Production evidence uses source_var/sink_var/source_type/sink_type keys.
            // Legacy test evidence uses source/sink keys.
            let source_var = finding
                .evidence
                .get("source_var")
                .and_then(|v| v.as_str())
                .or_else(|| finding.evidence.get("source").and_then(|v| v.as_str()));
            let sink_var = finding
                .evidence
                .get("sink_var")
                .and_then(|v| v.as_str())
                .or_else(|| finding.evidence.get("sink").and_then(|v| v.as_str()));
            let source_type = finding.evidence.get("source_type").and_then(|v| v.as_str());
            let sink_type = finding.evidence.get("sink_type").and_then(|v| v.as_str());

            match (source_var, sink_var) {
                (Some(src), Some(snk)) => {
                    let src_label = match source_type {
                        Some(st) => format!("{} ({})", src, st),
                        None => src.to_string(),
                    };
                    let snk_label = match sink_type {
                        Some(st) => format!("{} ({})", snk, st),
                        None => snk.to_string(),
                    };
                    writeln!(out, "  Flow: {} -> {}", src_label, snk_label).unwrap();
                }
                _ => {
                    if let Some(src) = source_var {
                        writeln!(out, "  Source: {}", src).unwrap();
                    }
                    if let Some(snk) = sink_var {
                        writeln!(out, "  Sink: {}", snk).unwrap();
                    }
                }
            }
        }
        "born-dead" => {
            if let Some(count) = finding.evidence.get("ref_count").and_then(|v| v.as_u64()) {
                writeln!(out, "  References: {}", count).unwrap();
            }
        }
        "complexity-increase" | "maintainability-drop" => {
            let before = finding.evidence.get("before").and_then(|v| v.as_u64());
            let after = finding.evidence.get("after").and_then(|v| v.as_u64());
            if let (Some(b), Some(a)) = (before, after) {
                let label = if finding.finding_type == "complexity-increase" {
                    "Complexity"
                } else {
                    "Maintainability"
                };
                writeln!(out, "  {}: {} -> {}", label, b, a).unwrap();
            }
        }
        "resource-leak" => {
            let sub_type = finding
                .evidence
                .get("sub_type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let resource = finding
                .evidence
                .get("resource")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            writeln!(out, "  Resource: {} ({})", resource, sub_type).unwrap();
        }
        "new-clone" => {
            if let Some(clone_type) = finding.evidence.get("clone_type").and_then(|v| v.as_str()) {
                writeln!(out, "  Clone type: {}", clone_type).unwrap();
            }
            if let Some(similarity) = finding.evidence.get("similarity").and_then(|v| v.as_f64()) {
                writeln!(out, "  Similarity: {:.0}%", similarity * 100.0).unwrap();
            }
        }
        "impact-blast-radius" => {
            let total = finding
                .evidence
                .get("total_callers")
                .and_then(|v| v.as_u64());
            let direct = finding
                .evidence
                .get("direct_callers")
                .and_then(|v| v.as_u64());
            if let Some(t) = total {
                writeln!(out, "  Total callers: {}", t).unwrap();
            }
            if let Some(d) = direct {
                writeln!(out, "  Direct callers: {}", d).unwrap();
            }
        }
        "temporal-violation" => {
            let expected = finding
                .evidence
                .get("expected_order")
                .and_then(|v| v.as_array());
            let actual = finding
                .evidence
                .get("actual_order")
                .and_then(|v| v.as_array());
            if let Some(exp) = expected {
                let items: Vec<&str> = exp.iter().filter_map(|v| v.as_str()).collect();
                if !items.is_empty() {
                    writeln!(out, "  Expected order: {}", items.join(" -> ")).unwrap();
                }
            }
            if let Some(act) = actual {
                let items: Vec<&str> = act.iter().filter_map(|v| v.as_str()).collect();
                if !items.is_empty() {
                    writeln!(out, "  Actual order: {}", items.join(" -> ")).unwrap();
                }
            }
        }
        "guard-removed" => {
            let variable = finding
                .evidence
                .get("removed_variable")
                .and_then(|v| v.as_str());
            let constraint = finding
                .evidence
                .get("removed_constraint")
                .and_then(|v| v.as_str());
            if let (Some(var), Some(con)) = (variable, constraint) {
                writeln!(out, "  Removed guard: {} {}", var, con).unwrap();
            } else {
                format_generic_evidence(out, &finding.evidence);
            }
        }
        "contract-regression" => {
            let category = finding.evidence.get("category").and_then(|v| v.as_str());
            let variable = finding
                .evidence
                .get("removed_variable")
                .and_then(|v| v.as_str());
            let constraint = finding
                .evidence
                .get("removed_constraint")
                .and_then(|v| v.as_str());
            if let (Some(cat), Some(var), Some(con)) = (category, variable, constraint) {
                writeln!(out, "  Removed {}: {} {}", cat, var, con).unwrap();
            } else {
                format_generic_evidence(out, &finding.evidence);
            }
        }
        _ => {
            format_generic_evidence(out, &finding.evidence);
        }
    }
}

/// Format evidence generically by showing all non-null values from a JSON object.
///
/// Handles strings, numbers (integer and float), booleans, and arrays of strings.
/// Nested objects are shown as compact JSON. Null values are skipped.
fn format_generic_evidence(out: &mut String, evidence: &serde_json::Value) {
    if let Some(obj) = evidence.as_object() {
        for (key, value) in obj {
            if value.is_null() {
                continue;
            }
            if let Some(s) = value.as_str() {
                writeln!(out, "  {}: {}", key, s).unwrap();
            } else if let Some(n) = value.as_u64() {
                writeln!(out, "  {}: {}", key, n).unwrap();
            } else if let Some(n) = value.as_i64() {
                writeln!(out, "  {}: {}", key, n).unwrap();
            } else if let Some(n) = value.as_f64() {
                // Avoid trailing zeros for clean display
                if n.fract() == 0.0 {
                    writeln!(out, "  {}: {}", key, n as i64).unwrap();
                } else {
                    writeln!(out, "  {}: {}", key, n).unwrap();
                }
            } else if let Some(b) = value.as_bool() {
                writeln!(out, "  {}: {}", key, b).unwrap();
            } else if let Some(arr) = value.as_array() {
                let items: Vec<String> = arr
                    .iter()
                    .map(|v| {
                        if let Some(s) = v.as_str() {
                            s.to_string()
                        } else {
                            v.to_string()
                        }
                    })
                    .collect();
                writeln!(out, "  {}: {}", key, items.join(", ")).unwrap();
            } else if value.is_object() {
                // Nested objects: show as compact JSON
                writeln!(out, "  {}: {}", key, value).unwrap();
            }
        }
    }
}

/// Format engine status label for display.
///
/// Returns a short lowercase status string: "complete", "partial", "skipped",
/// or "timed out" based on the engine result's success flag and status string.
fn format_engine_status(result: &L2AnalyzerResult) -> String {
    if result.success {
        "complete".to_string()
    } else if result.status.starts_with("partial") || result.status.starts_with("Partial") {
        "partial".to_string()
    } else if result.status.starts_with("skipped") || result.status.starts_with("Skipped") {
        "skipped".to_string()
    } else if result.status.contains("timed out") || result.status.contains("TimedOut") {
        "timed out".to_string()
    } else {
        "failed".to_string()
    }
}

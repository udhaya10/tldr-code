//! Swift diagnostic tool output parsers.
//!
//! Supports:
//! - `swiftc`: Swift compiler with GCC-like text output
//!   Format: `file.swift:line:col: error: message`
//! - `swiftlint`: Linter with JSON output (`--reporter json`)
//!   JSON array of objects with file, line, column, severity, reason, rule_id

use crate::diagnostics::{Diagnostic, Severity};
use crate::error::TldrError;
use regex::Regex;
use serde::Deserialize;
use std::path::PathBuf;

// =============================================================================
// swiftc parser (text-based, GCC-like format)
// =============================================================================

/// Parse swiftc text output into unified Diagnostic structs.
///
/// swiftc outputs errors in GCC-like format:
/// `file.swift:line:col: severity: message`
///
/// # Arguments
/// * `output` - The raw text output from `swiftc -typecheck`
///
/// # Returns
/// A vector of Diagnostic structs. Malformed lines are skipped.
pub fn parse_swiftc_output(output: &str) -> Result<Vec<Diagnostic>, TldrError> {
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }

    // Pattern: file.swift:line:col: severity: message
    let regex = Regex::new(r"^(.+\.swift):(\d+):(\d+):\s*(error|warning|note):\s*(.+)$")
        .expect("Invalid swiftc regex pattern");

    let mut diagnostics = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(captures) = regex.captures(line) {
            let file = captures.get(1).map(|m| m.as_str()).unwrap_or("");
            let line_num: u32 = captures
                .get(2)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(1);
            let column: u32 = captures
                .get(3)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(1);
            let severity_str = captures.get(4).map(|m| m.as_str()).unwrap_or("error");
            let message = captures
                .get(5)
                .map(|m| m.as_str())
                .unwrap_or("")
                .to_string();

            let severity = match severity_str {
                "error" => Severity::Error,
                "warning" => Severity::Warning,
                "note" => Severity::Information,
                _ => Severity::Error,
            };

            diagnostics.push(Diagnostic {
                file: PathBuf::from(file),
                line: line_num,
                column,
                end_line: None,
                end_column: None,
                severity,
                message,
                code: None,
                source: "swiftc".to_string(),
                url: None,
            });
        }
    }

    Ok(diagnostics)
}

// =============================================================================
// swiftlint parser (JSON-based)
// =============================================================================

/// SwiftLint JSON output structure
#[derive(Debug, Deserialize)]
struct SwiftLintDiagnostic {
    file: String,
    line: u32,
    #[serde(default = "default_column")]
    column: u32,
    severity: String,
    reason: String,
    rule_id: String,
}

fn default_column() -> u32 {
    1
}

/// Parse swiftlint JSON output into unified Diagnostic structs.
///
/// SwiftLint outputs JSON via `--reporter json`:
/// ```json
/// [
///   {
///     "file": "/path/to/file.swift",
///     "line": 42,
///     "column": 5,
///     "severity": "Warning",
///     "reason": "Force unwrapping should be avoided.",
///     "rule_id": "force_unwrapping"
///   }
/// ]
/// ```
///
/// # Arguments
/// * `output` - The raw JSON output from `swiftlint lint --reporter json`
///
/// # Returns
/// A vector of Diagnostic structs, or an error if JSON parsing fails.
pub fn parse_swiftlint_output(output: &str) -> Result<Vec<Diagnostic>, TldrError> {
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }

    if output.trim() == "[]" {
        return Ok(Vec::new());
    }

    let parsed: Vec<SwiftLintDiagnostic> =
        serde_json::from_str(output).map_err(|e| TldrError::ParseError {
            file: PathBuf::from("<swiftlint-output>"),
            line: None,
            message: format!("Failed to parse swiftlint JSON: {}", e),
        })?;

    let diagnostics = parsed
        .into_iter()
        .map(|d| {
            let severity = match d.severity.to_lowercase().as_str() {
                "error" => Severity::Error,
                "warning" => Severity::Warning,
                _ => Severity::Warning,
            };

            Diagnostic {
                file: PathBuf::from(&d.file),
                line: d.line,
                column: d.column,
                end_line: None,
                end_column: None,
                severity,
                message: d.reason,
                code: Some(d.rule_id),
                source: "swiftlint".to_string(),
                url: None,
            }
        })
        .collect();

    Ok(diagnostics)
}

//! Kotlin diagnostic tool output parsers.
//!
//! Supports:
//! - `kotlinc`: Kotlin compiler with GCC-like text output
//!   Format: `file.kt:line:col: error: message`
//! - `detekt`: Static analysis tool with text output
//!   Format: `file.kt:line:col - [RuleName] message`

use crate::diagnostics::{Diagnostic, Severity};
use crate::error::TldrError;
use regex::Regex;
use std::path::PathBuf;

/// Parse kotlinc text output into unified Diagnostic structs.
///
/// kotlinc outputs errors in GCC-like format:
/// `file.kt:line:col: severity: message`
///
/// # Arguments
/// * `output` - The raw text output from `kotlinc`
///
/// # Returns
/// A vector of Diagnostic structs. Malformed lines are skipped.
pub fn parse_kotlinc_output(output: &str) -> Result<Vec<Diagnostic>, TldrError> {
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }

    // Pattern: file.kt:line:col: severity: message
    let regex = Regex::new(r"^(.+\.kts?):(\d+):(\d+):\s*(error|warning|info):\s*(.+)$")
        .expect("Invalid kotlinc regex pattern");

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
                "info" => Severity::Information,
                _ => Severity::Warning,
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
                source: "kotlinc".to_string(),
                url: None,
            });
        }
    }

    Ok(diagnostics)
}

/// Parse detekt text output into unified Diagnostic structs.
///
/// detekt outputs issues in the format:
/// `file.kt:line:col - [RuleName] message`
///
/// # Arguments
/// * `output` - The raw text output from `detekt-cli --report txt:stdout`
///
/// # Returns
/// A vector of Diagnostic structs. Malformed lines are skipped.
pub fn parse_detekt_output(output: &str) -> Result<Vec<Diagnostic>, TldrError> {
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }

    // Pattern: file.kt:line:col - [RuleName] message
    let regex = Regex::new(r"^(.+\.kts?):(\d+):(\d+)\s*-\s*\[([^\]]+)\]\s*(.+)$")
        .expect("Invalid detekt regex pattern");

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
            let rule = captures.get(4).map(|m| m.as_str().to_string());
            let message = captures
                .get(5)
                .map(|m| m.as_str())
                .unwrap_or("")
                .to_string();

            diagnostics.push(Diagnostic {
                file: PathBuf::from(file),
                line: line_num,
                column,
                end_line: None,
                end_column: None,
                severity: Severity::Warning, // detekt issues are warnings
                message,
                code: rule,
                source: "detekt".to_string(),
                url: None,
            });
        }
    }

    Ok(diagnostics)
}

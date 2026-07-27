//! Java diagnostic tool output parsers.
//!
//! Supports:
//! - `javac`: Java compiler with text output
//!   Format: `file.java:line: error: message`
//! - `checkstyle`: Style checker with plain text output
//!   Format: `[WARN] file.java:line:col: message [CheckName]`

use crate::diagnostics::{Diagnostic, Severity};
use crate::error::TldrError;
use regex::Regex;
use std::path::PathBuf;

/// Parse javac text output into unified Diagnostic structs.
///
/// javac outputs errors in the format:
/// `file.java:line: error: message`
///
/// Note: javac does not include column numbers in its default output.
///
/// # Arguments
/// * `output` - The raw text output from `javac -Xlint:all`
///
/// # Returns
/// A vector of Diagnostic structs. Malformed lines are skipped.
pub fn parse_javac_output(output: &str) -> Result<Vec<Diagnostic>, TldrError> {
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }

    // Pattern: file.java:line: severity: message
    // javac format: "File.java:42: error: ';' expected"
    let regex = Regex::new(r"^(.+\.java):(\d+):\s*(error|warning):\s*(.+)$")
        .expect("Invalid javac regex pattern");

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
            let severity_str = captures.get(3).map(|m| m.as_str()).unwrap_or("error");
            let message = captures
                .get(4)
                .map(|m| m.as_str())
                .unwrap_or("")
                .to_string();

            let severity = match severity_str {
                "error" => Severity::Error,
                "warning" => Severity::Warning,
                _ => Severity::Error,
            };

            diagnostics.push(Diagnostic {
                file: PathBuf::from(file),
                line: line_num,
                column: 1, // javac doesn't provide column numbers
                end_line: None,
                end_column: None,
                severity,
                message,
                code: None, // javac doesn't provide error codes
                source: "javac".to_string(),
                url: None,
            });
        }
    }

    Ok(diagnostics)
}

/// Parse checkstyle plain text output into unified Diagnostic structs.
///
/// checkstyle plain format:
/// `[WARN] file.java:line:col: message [CheckName]`
/// or without column:
/// `[WARN] file.java:line: message [CheckName]`
///
/// # Arguments
/// * `output` - The raw text output from `checkstyle -f plain`
///
/// # Returns
/// A vector of Diagnostic structs. Malformed lines are skipped.
pub fn parse_checkstyle_output(output: &str) -> Result<Vec<Diagnostic>, TldrError> {
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }

    // Pattern: [SEVERITY] file.java:line:col: message [CheckName]
    // The col part is optional, and the CheckName at the end is optional
    let regex = Regex::new(
        r"^\[(WARN|ERROR|INFO)\]\s+(.+\.java):(\d+)(?::(\d+))?:\s*(.+?)(?:\s+\[([^\]]+)\])?\s*$",
    )
    .expect("Invalid checkstyle regex pattern");

    let mut diagnostics = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(captures) = regex.captures(line) {
            let severity_str = captures.get(1).map(|m| m.as_str()).unwrap_or("WARN");
            let file = captures.get(2).map(|m| m.as_str()).unwrap_or("");
            let line_num: u32 = captures
                .get(3)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(1);
            let column: u32 = captures
                .get(4)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(1);
            let message = captures
                .get(5)
                .map(|m| m.as_str())
                .unwrap_or("")
                .to_string();
            let check_name = captures.get(6).map(|m| m.as_str().to_string());

            let severity = match severity_str {
                "ERROR" => Severity::Error,
                "WARN" => Severity::Warning,
                "INFO" => Severity::Information,
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
                code: check_name,
                source: "checkstyle".to_string(),
                url: None,
            });
        }
    }

    Ok(diagnostics)
}

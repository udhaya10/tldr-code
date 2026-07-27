//! Scala compiler (scalac) diagnostic output parser.
//!
//! Parses scalac text output in GCC-like format (Scala 2 style):
//! ```text
//! file.scala:line: error: message
//! ```
//!
//! Scala 3 uses a different format (`-- [E001] Error: file.scala:line:col`),
//! but we parse the simpler Scala 2 format which is still commonly used.
//! Column information is optional in scalac output.

use crate::diagnostics::{Diagnostic, Severity};
use crate::error::TldrError;
use regex::Regex;
use std::path::PathBuf;

/// Parse scalac text output into unified Diagnostic structs.
///
/// scalac outputs errors in a GCC-like format:
/// - With column: `file.scala:line:col: severity: message`
/// - Without column: `file.scala:line: severity: message`
///
/// Both formats are supported.
///
/// # Arguments
/// * `output` - The raw text output from `scalac`
///
/// # Returns
/// A vector of Diagnostic structs. Malformed lines are skipped.
pub fn parse_scalac_output(output: &str) -> Result<Vec<Diagnostic>, TldrError> {
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }

    // Pattern with column: file.scala:line:col: severity: message
    let regex_with_col = Regex::new(r"^(.+\.scala):(\d+):(\d+):\s*(error|warning):\s*(.+)$")
        .expect("Invalid scalac regex pattern (with col)");

    // Pattern without column: file.scala:line: severity: message
    let regex_no_col = Regex::new(r"^(.+\.scala):(\d+):\s*(error|warning):\s*(.+)$")
        .expect("Invalid scalac regex pattern (no col)");

    let mut diagnostics = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Try with-column pattern first (more specific)
        if let Some(captures) = regex_with_col.captures(line) {
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
                source: "scalac".to_string(),
                url: None,
            });
        } else if let Some(captures) = regex_no_col.captures(line) {
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
                column: 1, // Default column when not provided
                end_line: None,
                end_column: None,
                severity,
                message,
                code: None,
                source: "scalac".to_string(),
                url: None,
            });
        }
    }

    Ok(diagnostics)
}

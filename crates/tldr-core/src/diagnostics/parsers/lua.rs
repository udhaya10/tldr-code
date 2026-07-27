//! Luacheck diagnostic output parser.
//!
//! Parses luacheck plain text output format:
//! ```text
//! file.lua:line:col: (W611) line is too long
//! ```
//!
//! Error codes follow the pattern:
//! - W### for warnings
//! - E### for errors
//!
//! Use `luacheck --formatter plain --no-color` for consistent output.

use crate::diagnostics::{Diagnostic, Severity};
use crate::error::TldrError;
use regex::Regex;
use std::path::PathBuf;

/// Parse luacheck plain text output into unified Diagnostic structs.
///
/// luacheck outputs issues in the format:
/// `file.lua:line:col: (CODE) message`
///
/// Where CODE is:
/// - `W###` for warnings
/// - `E###` for errors
///
/// # Arguments
/// * `output` - The raw text output from `luacheck --formatter plain --no-color`
///
/// # Returns
/// A vector of Diagnostic structs. Malformed lines are skipped.
pub fn parse_luacheck_output(output: &str) -> Result<Vec<Diagnostic>, TldrError> {
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }

    // Pattern: file.lua:line:col: (CODE) message
    let regex = Regex::new(r"^(.+\.lua):(\d+):(\d+):\s*\(([EW]\d+)\)\s*(.+)$")
        .expect("Invalid luacheck regex pattern");

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
            let code = captures.get(4).map(|m| m.as_str().to_string());
            let message = captures
                .get(5)
                .map(|m| m.as_str())
                .unwrap_or("")
                .to_string();

            // Determine severity from code prefix
            let severity = match code.as_ref().map(|c| c.chars().next()) {
                Some(Some('E')) => Severity::Error,
                Some(Some('W')) => Severity::Warning,
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
                code,
                source: "luacheck".to_string(),
                url: None,
            });
        }
    }

    Ok(diagnostics)
}

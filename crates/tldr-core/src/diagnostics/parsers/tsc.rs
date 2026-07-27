//! TypeScript Compiler (tsc) text output parser.
//!
//! tsc outputs text in the following format (no native JSON):
//! ```text
//! src/auth.ts(42,5): error TS2339: Property 'foo' does not exist on type 'Bar'.
//! ```
//!
//! Pattern: `file(line,col): severity TScode: message`

use crate::diagnostics::{Diagnostic, Severity};
use crate::error::TldrError;
use regex::Regex;
use std::path::PathBuf;

/// Get the regex pattern for parsing tsc output lines.
///
/// Pattern captures:
/// 1. File path
/// 2. Line number
/// 3. Column number
/// 4. Severity (error/warning)
/// 5. Error code (TS####)
/// 6. Message
pub fn tsc_output_regex() -> Regex {
    // Pattern: file(line,col): severity TScode: message
    Regex::new(r"^(.+)\((\d+),(\d+)\):\s*(error|warning)\s+(TS\d+):\s*(.+)$")
        .expect("Invalid tsc regex pattern")
}

/// Parse tsc text output into unified Diagnostic structs.
///
/// # Arguments
/// * `output` - The raw text output from `tsc --noEmit --pretty false`
///
/// # Returns
/// A vector of Diagnostic structs, or an error if parsing fails.
pub fn parse_tsc_text(output: &str) -> Result<Vec<Diagnostic>, TldrError> {
    let regex = tsc_output_regex();
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
            let code = captures.get(5).map(|m| m.as_str().to_string());
            let message = captures
                .get(6)
                .map(|m| m.as_str())
                .unwrap_or("")
                .to_string();

            let severity = match severity_str.to_lowercase().as_str() {
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
                code,
                source: "tsc".to_string(),
                url: None,
            });
        }
        // Lines that don't match the pattern are ignored (e.g., continuation lines)
    }

    Ok(diagnostics)
}

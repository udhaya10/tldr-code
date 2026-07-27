//! Clang/GCC diagnostic tool output parsers.
//!
//! Supports:
//! - `clang`/`gcc`: C/C++ compilers with GCC-style text output
//!   Format: `file.c:line:col: warning: message [-Wflag]`
//! - `clang-tidy`: Static analysis tool with same GCC-style output
//!   Format: `file.c:line:col: warning: message [check-name]`
//!
//! Both clang and clang-tidy use the same output format, so a single
//! parser handles both. The source field differentiates them.

use crate::diagnostics::{Diagnostic, Severity};
use crate::error::TldrError;
use regex::Regex;
use std::path::PathBuf;

/// Parse GCC/clang-style text output into unified Diagnostic structs.
///
/// This handles output from clang, gcc, and clang-tidy, which all share
/// the same format:
/// `file.c:line:col: severity: message [-Wflag]`
/// or
/// `file.c:line:col: severity: message [check-name]`
///
/// # Arguments
/// * `output` - The raw text output from clang/gcc/clang-tidy
/// * `source` - The tool name to use in the Diagnostic source field
///
/// # Returns
/// A vector of Diagnostic structs. Malformed lines are skipped.
pub fn parse_clang_output(output: &str, source: &str) -> Result<Vec<Diagnostic>, TldrError> {
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }

    // Pattern: file:line:col: severity: message [-Wflag] or [check-name]
    // The bracket part at the end is optional
    let regex = Regex::new(
        r"^(.+):(\d+):(\d+):\s*(error|warning|note|fatal error):\s*(.+?)(?:\s+\[([^\]]+)\])?\s*$",
    )
    .expect("Invalid clang regex pattern");

    let mut diagnostics = Vec::new();
    let source = source.to_string();

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
            let flag = captures.get(6).map(|m| m.as_str().to_string());

            let severity = match severity_str {
                "error" | "fatal error" => Severity::Error,
                "warning" => Severity::Warning,
                "note" => Severity::Information,
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
                code: flag,
                source: source.clone(),
                url: None,
            });
        }
    }

    Ok(diagnostics)
}

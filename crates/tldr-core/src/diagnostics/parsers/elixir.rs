//! Elixir diagnostic tool output parsers.
//!
//! Supports:
//! - `mix compile`: Elixir compiler with text output
//!   Error format: `** (CompileError) file.ex:line: message`
//!   Warning format: `warning: message\n  file.ex:line`
//! - `credo`: Static analysis tool with JSON output (`--format json`)
//!   JSON has `issues` array with filename, line_no, column, message, category, priority

use crate::diagnostics::{Diagnostic, Severity};
use crate::error::TldrError;
use regex::Regex;
use serde::Deserialize;
use std::path::PathBuf;

// =============================================================================
// mix compile parser (text-based)
// =============================================================================

/// Parse mix compile text output into unified Diagnostic structs.
///
/// mix compile outputs errors and warnings in different formats:
/// - Errors: `** (CompileError) file.ex:line: message`
/// - Warnings: `warning: message\n  file.ex:line`
///
/// We also handle the simpler format:
/// - `file.ex:line: warning: message`
///
/// # Arguments
/// * `output` - The raw text output from `mix compile`
///
/// # Returns
/// A vector of Diagnostic structs. Malformed lines are skipped.
pub fn parse_mix_compile_output(output: &str) -> Result<Vec<Diagnostic>, TldrError> {
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mut diagnostics = Vec::new();

    // Pattern for CompileError: ** (CompileError) file.ex:line: message
    let error_regex = Regex::new(r"^\*\*\s*\(CompileError\)\s*(.+\.exs?):(\d+):\s*(.+)$")
        .expect("Invalid mix compile error regex");

    // Pattern for inline warnings: file.ex:line: warning: message
    let warning_inline_regex = Regex::new(r"^(.+\.exs?):(\d+):\s*warning:\s*(.+)$")
        .expect("Invalid mix compile warning regex");

    // Pattern for multi-line warnings: "warning: message" then "  file.ex:line"
    let warning_prefix_regex =
        Regex::new(r"^warning:\s*(.+)$").expect("Invalid mix compile warning prefix regex");

    let location_regex =
        Regex::new(r"^\s+(.+\.exs?):(\d+)").expect("Invalid mix compile location regex");

    let lines: Vec<&str> = output.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i].trim();

        // Try CompileError pattern
        if let Some(captures) = error_regex.captures(line) {
            let file = captures.get(1).map(|m| m.as_str()).unwrap_or("");
            let line_num: u32 = captures
                .get(2)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(1);
            let message = captures
                .get(3)
                .map(|m| m.as_str())
                .unwrap_or("")
                .to_string();

            diagnostics.push(Diagnostic {
                file: PathBuf::from(file),
                line: line_num,
                column: 1,
                end_line: None,
                end_column: None,
                severity: Severity::Error,
                message,
                code: None,
                source: "mix compile".to_string(),
                url: None,
            });
            i += 1;
            continue;
        }

        // Try inline warning pattern
        if let Some(captures) = warning_inline_regex.captures(line) {
            let file = captures.get(1).map(|m| m.as_str()).unwrap_or("");
            let line_num: u32 = captures
                .get(2)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(1);
            let message = captures
                .get(3)
                .map(|m| m.as_str())
                .unwrap_or("")
                .to_string();

            diagnostics.push(Diagnostic {
                file: PathBuf::from(file),
                line: line_num,
                column: 1,
                end_line: None,
                end_column: None,
                severity: Severity::Warning,
                message,
                code: None,
                source: "mix compile".to_string(),
                url: None,
            });
            i += 1;
            continue;
        }

        // Try multi-line warning pattern: "warning: message" followed by "  file.ex:line"
        if let Some(captures) = warning_prefix_regex.captures(line) {
            let message = captures
                .get(1)
                .map(|m| m.as_str())
                .unwrap_or("")
                .to_string();

            // Look ahead for the location line
            if i + 1 < lines.len() {
                if let Some(loc_captures) = location_regex.captures(lines[i + 1]) {
                    let file = loc_captures.get(1).map(|m| m.as_str()).unwrap_or("");
                    let line_num: u32 = loc_captures
                        .get(2)
                        .and_then(|m| m.as_str().parse().ok())
                        .unwrap_or(1);

                    diagnostics.push(Diagnostic {
                        file: PathBuf::from(file),
                        line: line_num,
                        column: 1,
                        end_line: None,
                        end_column: None,
                        severity: Severity::Warning,
                        message,
                        code: None,
                        source: "mix compile".to_string(),
                        url: None,
                    });
                    i += 2; // Skip both lines
                    continue;
                }
            }
        }

        i += 1;
    }

    Ok(diagnostics)
}

// =============================================================================
// credo parser (JSON-based)
// =============================================================================

/// Credo JSON output root structure
#[derive(Debug, Deserialize)]
struct CredoOutput {
    issues: Vec<CredoIssue>,
}

/// A single Credo issue
#[derive(Debug, Deserialize)]
struct CredoIssue {
    filename: String,
    line_no: u32,
    #[serde(default)]
    column: Option<u32>,
    message: String,
    category: String,
    priority: i32,
}

/// Parse credo JSON output into unified Diagnostic structs.
///
/// Credo outputs JSON via `--format json`:
/// ```json
/// {
///   "issues": [
///     {
///       "filename": "lib/my_app.ex",
///       "line_no": 42,
///       "column": 5,
///       "message": "Modules should have a @moduledoc tag.",
///       "category": "readability",
///       "priority": 10
///     }
///   ]
/// }
/// ```
///
/// Priority mapping:
/// - priority >= 20: Error
/// - priority >= 10: Warning
/// - priority >= 1: Information
/// - otherwise: Hint
///
/// # Arguments
/// * `output` - The raw JSON output from `mix credo --format json`
///
/// # Returns
/// A vector of Diagnostic structs, or an error if JSON parsing fails.
pub fn parse_credo_output(output: &str) -> Result<Vec<Diagnostic>, TldrError> {
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }

    let parsed: CredoOutput = serde_json::from_str(output).map_err(|e| TldrError::ParseError {
        file: PathBuf::from("<credo-output>"),
        line: None,
        message: format!("Failed to parse credo JSON: {}", e),
    })?;

    let diagnostics = parsed
        .issues
        .into_iter()
        .map(|issue| {
            let severity = if issue.priority >= 20 {
                Severity::Error
            } else if issue.priority >= 10 {
                Severity::Warning
            } else if issue.priority >= 1 {
                Severity::Information
            } else {
                Severity::Hint
            };

            Diagnostic {
                file: PathBuf::from(&issue.filename),
                line: issue.line_no,
                column: issue.column.unwrap_or(1),
                end_line: None,
                end_column: None,
                severity,
                message: issue.message,
                code: Some(issue.category),
                source: "credo".to_string(),
                url: None,
            }
        })
        .collect();

    Ok(diagnostics)
}

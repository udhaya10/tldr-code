//! PHP diagnostic tool output parsers.
//!
//! Supports:
//! - `php -l`: PHP syntax checker with text output
//!   Format: `PHP Parse error: ... in file.php on line N`
//!   Format: `PHP Fatal error: ... in file.php on line N`
//!   Success: `No syntax errors detected in file.php`
//! - `phpstan`: Static analysis tool with JSON output
//!   Uses `--error-format=json` for structured output.

use crate::diagnostics::{Diagnostic, Severity};
use crate::error::TldrError;
use regex::Regex;
use serde::Deserialize;
use std::path::PathBuf;

/// Parse `php -l` text output into unified Diagnostic structs.
///
/// php -l outputs errors in the format:
/// `PHP Parse error: syntax error, unexpected ... in file.php on line N`
/// `PHP Fatal error: ... in file.php on line N`
///
/// On success, it outputs:
/// `No syntax errors detected in file.php`
///
/// # Arguments
/// * `output` - The raw text output from `php -l`
///
/// # Returns
/// A vector of Diagnostic structs. Non-error lines are skipped.
pub fn parse_php_lint_output(output: &str) -> Result<Vec<Diagnostic>, TldrError> {
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }

    // Pattern: PHP (Parse|Fatal) error: message in file.php on line N
    let regex = Regex::new(
        r"^PHP\s+(Parse error|Fatal error|Warning|Notice|Deprecated):\s*(.+?)\s+in\s+(.+?)\s+on\s+line\s+(\d+)"
    ).expect("Invalid php -l regex pattern");

    let mut diagnostics = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("No syntax errors") {
            continue;
        }

        if let Some(captures) = regex.captures(line) {
            let error_type = captures.get(1).map(|m| m.as_str()).unwrap_or("Parse error");
            let message = captures
                .get(2)
                .map(|m| m.as_str())
                .unwrap_or("")
                .to_string();
            let file = captures.get(3).map(|m| m.as_str()).unwrap_or("");
            let line_num: u32 = captures
                .get(4)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(1);

            let severity = match error_type {
                "Parse error" | "Fatal error" => Severity::Error,
                "Warning" => Severity::Warning,
                "Notice" | "Deprecated" => Severity::Information,
                _ => Severity::Error,
            };

            diagnostics.push(Diagnostic {
                file: PathBuf::from(file),
                line: line_num,
                column: 1, // php -l doesn't provide column numbers
                end_line: None,
                end_column: None,
                severity,
                message,
                code: None,
                source: "php".to_string(),
                url: None,
            });
        }
    }

    Ok(diagnostics)
}

/// PHPStan JSON output structure
#[derive(Debug, Deserialize)]
struct PhpstanOutput {
    #[serde(rename = "totals")]
    _totals: Option<PhpstanTotals>,
    files: std::collections::HashMap<String, PhpstanFile>,
}

/// PHPStan totals section
#[derive(Debug, Deserialize)]
struct PhpstanTotals {
    #[allow(dead_code)]
    errors: u32,
    #[allow(dead_code)]
    file_errors: u32,
}

/// PHPStan file entry with messages
#[derive(Debug, Deserialize)]
struct PhpstanFile {
    #[serde(rename = "errors")]
    _errors: u32,
    messages: Vec<PhpstanMessage>,
}

/// Individual PHPStan error message
#[derive(Debug, Deserialize)]
struct PhpstanMessage {
    message: String,
    line: Option<u32>,
    #[serde(default, rename = "ignorable")]
    _ignorable: bool,
}

/// Parse phpstan JSON output into unified Diagnostic structs.
///
/// PHPStan JSON format (via `--error-format=json`):
/// ```json
/// {
///   "totals": {"errors": 0, "file_errors": 2},
///   "files": {
///     "src/Controller.php": {
///       "errors": 2,
///       "messages": [
///         {"message": "Parameter $id has no type.", "line": 15, "ignorable": true},
///         {"message": "Method foo() has no return type.", "line": 20, "ignorable": true}
///       ]
///     }
///   }
/// }
/// ```
///
/// # Arguments
/// * `output` - The raw JSON output from `phpstan analyse --error-format=json --no-progress`
///
/// # Returns
/// A vector of Diagnostic structs, or an error if JSON parsing fails.
pub fn parse_phpstan_output(output: &str) -> Result<Vec<Diagnostic>, TldrError> {
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }

    let parsed: PhpstanOutput =
        serde_json::from_str(output).map_err(|e| TldrError::ParseError {
            file: PathBuf::from("<phpstan-output>"),
            line: None,
            message: format!("Failed to parse phpstan JSON: {}", e),
        })?;

    let mut diagnostics = Vec::new();

    for (file_path, file_data) in &parsed.files {
        for msg in &file_data.messages {
            diagnostics.push(Diagnostic {
                file: PathBuf::from(file_path),
                line: msg.line.unwrap_or(1),
                column: 1, // phpstan doesn't provide column numbers
                end_line: None,
                end_column: None,
                severity: Severity::Error, // phpstan reports are errors by default
                message: msg.message.clone(),
                code: None,
                source: "phpstan".to_string(),
                url: None,
            });
        }
    }

    Ok(diagnostics)
}

//! Ruby diagnostic tool output parser.
//!
//! Supports:
//! - `rubocop`: Ruby linter/formatter with JSON output
//!   Uses `--format json` which outputs structured JSON with files and offenses.

use crate::diagnostics::{Diagnostic, Severity};
use crate::error::TldrError;
use serde::Deserialize;
use std::path::PathBuf;

/// Top-level RuboCop JSON output structure
#[derive(Debug, Deserialize)]
struct RubocopOutput {
    files: Vec<RubocopFile>,
}

/// A file entry in RuboCop JSON output
#[derive(Debug, Deserialize)]
struct RubocopFile {
    path: String,
    offenses: Vec<RubocopOffense>,
}

/// An individual offense/violation in RuboCop output
#[derive(Debug, Deserialize)]
struct RubocopOffense {
    severity: String,
    message: String,
    cop_name: String,
    location: RubocopLocation,
}

/// Location information for a RuboCop offense
#[derive(Debug, Deserialize)]
struct RubocopLocation {
    start_line: u32,
    start_column: u32,
    last_line: Option<u32>,
    last_column: Option<u32>,
}

/// Parse rubocop JSON output into unified Diagnostic structs.
///
/// RuboCop JSON format (via `--format json`):
/// ```json
/// {
///   "files": [
///     {
///       "path": "src/app.rb",
///       "offenses": [
///         {
///           "severity": "convention",
///           "message": "Line is too long.",
///           "cop_name": "Layout/LineLength",
///           "location": {
///             "start_line": 10,
///             "start_column": 1,
///             "last_line": 10,
///             "last_column": 120
///           }
///         }
///       ]
///     }
///   ]
/// }
/// ```
///
/// # Severity Mapping
/// - `fatal`, `error` -> Error
/// - `warning` -> Warning
/// - `convention`, `refactor` -> Information
///
/// # Arguments
/// * `output` - The raw JSON output from `rubocop --format json`
///
/// # Returns
/// A vector of Diagnostic structs, or an error if JSON parsing fails.
pub fn parse_rubocop_output(output: &str) -> Result<Vec<Diagnostic>, TldrError> {
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }

    let parsed: RubocopOutput =
        serde_json::from_str(output).map_err(|e| TldrError::ParseError {
            file: PathBuf::from("<rubocop-output>"),
            line: None,
            message: format!("Failed to parse rubocop JSON: {}", e),
        })?;

    let mut diagnostics = Vec::new();

    for file in parsed.files {
        for offense in file.offenses {
            let severity = match offense.severity.as_str() {
                "fatal" | "error" => Severity::Error,
                "warning" => Severity::Warning,
                "convention" | "refactor" => Severity::Information,
                _ => Severity::Warning,
            };

            diagnostics.push(Diagnostic {
                file: PathBuf::from(&file.path),
                line: offense.location.start_line,
                column: offense.location.start_column,
                end_line: offense.location.last_line,
                end_column: offense.location.last_column,
                severity,
                message: offense.message,
                code: Some(offense.cop_name),
                source: "rubocop".to_string(),
                url: None,
            });
        }
    }

    Ok(diagnostics)
}

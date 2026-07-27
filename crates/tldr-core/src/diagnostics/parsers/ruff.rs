//! Ruff JSON output parser.
//!
//! Ruff outputs JSON via `ruff check --output-format json` with the following structure:
//! ```json
//! [
//!   {
//!     "cell": null,
//!     "code": "E501",
//!     "filename": "src/auth.py",
//!     "location": {"column": 1, "row": 58},
//!     "end_location": {"column": 121, "row": 58},
//!     "message": "Line too long (120 > 100 characters)",
//!     "noqa_row": 58,
//!     "url": "https://docs.astral.sh/ruff/rules/line-too-long"
//!   }
//! ]
//! ```
//!
//! Ruff issues are mapped to Warning severity by default (they are lint issues, not type errors).

use crate::diagnostics::{Diagnostic, Severity};
use crate::error::TldrError;
use serde::Deserialize;
use std::path::PathBuf;

/// Ruff JSON output is an array of diagnostics
#[derive(Debug, Deserialize)]
struct RuffDiagnostic {
    #[allow(dead_code)]
    cell: Option<u32>,
    code: String,
    filename: String,
    location: RuffLocation,
    end_location: Option<RuffLocation>,
    message: String,
    #[allow(dead_code)]
    noqa_row: Option<u32>,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RuffLocation {
    column: u32,
    row: u32,
}

/// Parse ruff JSON output into unified Diagnostic structs.
///
/// # Arguments
/// * `output` - The raw JSON output from `ruff check --output-format json`
///
/// # Returns
/// A vector of Diagnostic structs, or an error if parsing fails.
///
/// # Severity
/// All ruff issues are mapped to Warning severity by default,
/// as they are lint issues rather than type errors.
pub fn parse_ruff_output(output: &str) -> Result<Vec<Diagnostic>, TldrError> {
    // Handle empty output
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }

    // Handle empty array
    if output.trim() == "[]" {
        return Ok(Vec::new());
    }

    let parsed: Vec<RuffDiagnostic> =
        serde_json::from_str(output).map_err(|e| TldrError::ParseError {
            file: std::path::PathBuf::from("<ruff-output>"),
            line: None,
            message: format!("Failed to parse ruff JSON: {}", e),
        })?;

    let diagnostics = parsed
        .into_iter()
        .map(|d| Diagnostic {
            file: PathBuf::from(&d.filename),
            line: d.location.row,
            column: d.location.column,
            end_line: d.end_location.as_ref().map(|l| l.row),
            end_column: d.end_location.as_ref().map(|l| l.column),
            severity: Severity::Warning, // Ruff issues are warnings
            message: d.message,
            code: Some(d.code),
            source: "ruff".to_string(),
            url: d.url,
        })
        .collect();

    Ok(diagnostics)
}

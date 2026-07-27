//! Cargo/Clippy JSON output parser.
//!
//! Cargo outputs NDJSON (one JSON object per line) via `--message-format=json`:
//! ```json
//! {"reason":"compiler-message","message":{"level":"warning","message":"unused variable",...}}
//! ```
//!
//! Only messages with "reason": "compiler-message" are diagnostics.

use crate::diagnostics::{Diagnostic, Severity};
use crate::error::TldrError;
use serde::Deserialize;
use std::path::PathBuf;

/// Cargo JSON line (NDJSON)
#[derive(Debug, Deserialize)]
struct CargoLine {
    reason: String,
    message: Option<CargoMessage>,
}

/// Cargo compiler message
#[derive(Debug, Deserialize)]
struct CargoMessage {
    code: Option<CargoCode>,
    level: String,
    message: String,
    spans: Vec<CargoSpan>,
    #[allow(dead_code)]
    rendered: Option<String>,
}

/// Cargo error code
#[derive(Debug, Deserialize)]
struct CargoCode {
    code: String,
    #[allow(dead_code)]
    explanation: Option<String>,
}

/// Cargo source span
#[derive(Debug, Deserialize)]
struct CargoSpan {
    file_name: String,
    line_start: u32,
    line_end: u32,
    column_start: u32,
    column_end: u32,
    is_primary: bool,
    #[allow(dead_code)]
    label: Option<String>,
}

/// Parse cargo NDJSON output into unified Diagnostic structs.
///
/// # Arguments
/// * `output` - The raw NDJSON output from `cargo check --message-format=json`
///
/// # Returns
/// A vector of Diagnostic structs, or an error if parsing fails.
///
/// # Format
/// Each line is a separate JSON object. Only lines with
/// "reason": "compiler-message" are processed.
pub fn parse_cargo_output(output: &str) -> Result<Vec<Diagnostic>, TldrError> {
    let mut diagnostics = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Parse each line as JSON
        let cargo_line: CargoLine = match serde_json::from_str(line) {
            Ok(l) => l,
            Err(_) => continue, // Skip lines that don't parse (e.g., summary lines)
        };

        // Only process compiler messages
        if cargo_line.reason != "compiler-message" {
            continue;
        }

        let message = match cargo_line.message {
            Some(m) => m,
            None => continue,
        };

        // Find the primary span
        let primary_span = message.spans.iter().find(|s| s.is_primary);
        let span = match primary_span.or(message.spans.first()) {
            Some(s) => s,
            None => continue, // No span information
        };

        let severity = match message.level.as_str() {
            "error" => Severity::Error,
            "warning" => Severity::Warning,
            "note" => Severity::Information,
            "help" => Severity::Hint,
            _ => Severity::Warning,
        };

        let code = message.code.map(|c| c.code);

        diagnostics.push(Diagnostic {
            file: PathBuf::from(&span.file_name),
            line: span.line_start,
            column: span.column_start,
            end_line: Some(span.line_end),
            end_column: Some(span.column_end),
            severity,
            message: message.message,
            code,
            source: "cargo".to_string(),
            url: None,
        });
    }

    Ok(diagnostics)
}

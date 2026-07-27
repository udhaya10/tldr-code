//! C# (dotnet build) diagnostic output parser.
//!
//! Parses MSBuild text output format from `dotnet build`:
//! ```text
//! file.cs(line,col): error CODE: message [project.csproj]
//! ```
//!
//! The `[project.csproj]` suffix is optional and stripped during parsing.

use crate::diagnostics::{Diagnostic, Severity};
use crate::error::TldrError;
use regex::Regex;
use std::path::PathBuf;

/// Parse dotnet build MSBuild text output into unified Diagnostic structs.
///
/// MSBuild output format:
/// `file.cs(line,col): severity CODE: message [project.csproj]`
///
/// The project reference in brackets at the end is optional and ignored.
///
/// # Arguments
/// * `output` - The raw text output from `dotnet build`
///
/// # Returns
/// A vector of Diagnostic structs. Malformed lines are skipped.
pub fn parse_dotnet_build_output(output: &str) -> Result<Vec<Diagnostic>, TldrError> {
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }

    // Pattern: file.cs(line,col): severity CODE: message [optional project]
    // The code format is typically CS#### or CA#### for analyzers
    let regex = Regex::new(
        r"^\s*(.+?)\((\d+),(\d+)\):\s*(error|warning|info)\s+([A-Z]+\d+):\s*(.+?)(?:\s*\[.+\])?\s*$"
    ).expect("Invalid dotnet build regex pattern");

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

            let severity = match severity_str {
                "error" => Severity::Error,
                "warning" => Severity::Warning,
                "info" => Severity::Information,
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
                source: "dotnet build".to_string(),
                url: None,
            });
        }
    }

    Ok(diagnostics)
}

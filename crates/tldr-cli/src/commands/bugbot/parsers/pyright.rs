//! Parser for `pyright --outputjson` output.
//!
//! Pyright outputs a JSON object with a `generalDiagnostics` array.
//! Each diagnostic has `file`, `severity`, `message`, `range`, and `rule`.

use std::path::PathBuf;

use super::super::tools::{L1Finding, ToolCategory};
use super::ParseError;

/// Parse pyright JSON output into L1 findings.
///
/// Pyright emits JSON like:
/// ```json
/// {"version": "1.1", "generalDiagnostics": [
///   {"file": "/abs/path.py", "severity": "error",
///    "message": "...", "range": {"start": {"line": 5, "character": 0}},
///    "rule": "reportMissingImports"}
/// ]}
/// ```
pub fn parse_pyright_output(stdout: &str) -> Result<Vec<L1Finding>, ParseError> {
    let stdout = stdout.trim();
    if stdout.is_empty() {
        return Ok(Vec::new());
    }

    let root: serde_json::Value = serde_json::from_str(stdout)?;

    let diagnostics = match root.get("generalDiagnostics").and_then(|v| v.as_array()) {
        Some(diags) => diags,
        None => return Ok(Vec::new()),
    };

    let mut findings = Vec::new();

    for diag in diagnostics {
        let file = diag.get("file").and_then(|v| v.as_str()).unwrap_or("");
        let pyright_severity = diag
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("information");
        let message = diag.get("message").and_then(|v| v.as_str()).unwrap_or("");
        let rule = diag.get("rule").and_then(|v| v.as_str()).unwrap_or("");

        // Pyright uses 0-based line numbers
        let line = diag
            .pointer("/range/start/line")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let column = diag
            .pointer("/range/start/character")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let severity = pyright_severity_to_bugbot(pyright_severity);

        findings.push(L1Finding {
            tool: String::new(), // Set by runner [PM-6]
            category: ToolCategory::TypeChecker,
            file: PathBuf::from(file),
            line: (line + 1) as u32, // Convert 0-based to 1-based
            column: (column + 1) as u32,
            native_severity: pyright_severity.to_string(),
            severity,
            message: message.to_string(),
            code: if rule.is_empty() {
                None
            } else {
                Some(rule.to_string())
            },
        });
    }

    Ok(findings)
}

/// Map pyright severity to bugbot severity.
fn pyright_severity_to_bugbot(severity: &str) -> String {
    match severity {
        "error" => "high".to_string(),
        "warning" => "medium".to_string(),
        "information" => "low".to_string(),
        _ => "info".to_string(),
    }
}

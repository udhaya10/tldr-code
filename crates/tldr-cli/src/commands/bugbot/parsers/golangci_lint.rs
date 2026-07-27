//! Parser for `golangci-lint run --out-format json` output.

use std::path::PathBuf;

use super::super::tools::{L1Finding, ToolCategory};
use super::ParseError;

pub fn parse_golangci_lint_output(stdout: &str) -> Result<Vec<L1Finding>, ParseError> {
    let stdout = stdout.trim();
    if stdout.is_empty() {
        return Ok(Vec::new());
    }

    let root: serde_json::Value = serde_json::from_str(stdout)?;
    let issues = match root.get("Issues").and_then(|v| v.as_array()) {
        Some(issues) => issues,
        None => return Ok(Vec::new()),
    };

    let mut findings = Vec::new();
    for issue in issues {
        let file = issue
            .pointer("/Pos/Filename")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let line = issue
            .pointer("/Pos/Line")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let column = issue
            .pointer("/Pos/Column")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let text = issue.get("Text").and_then(|v| v.as_str()).unwrap_or("");
        let linter = issue
            .get("FromLinter")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let native_sev = issue
            .get("Severity")
            .and_then(|v| v.as_str())
            .unwrap_or("warning");

        let severity = match native_sev {
            "error" => "high",
            "warning" => "medium",
            _ => "low",
        };

        findings.push(L1Finding {
            tool: String::new(),
            category: ToolCategory::Linter,
            file: PathBuf::from(file),
            line: line as u32,
            column: column as u32,
            native_severity: native_sev.to_string(),
            severity: severity.to_string(),
            message: text.to_string(),
            code: if linter.is_empty() {
                None
            } else {
                Some(linter.to_string())
            },
        });
    }

    Ok(findings)
}

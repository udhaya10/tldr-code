//! Parser for `ktlint --reporter=json` output.

use std::path::PathBuf;

use super::super::tools::{L1Finding, ToolCategory};
use super::ParseError;

pub fn parse_ktlint_output(stdout: &str) -> Result<Vec<L1Finding>, ParseError> {
    let stdout = stdout.trim();
    if stdout.is_empty() || stdout == "[]" {
        return Ok(Vec::new());
    }

    let files: Vec<serde_json::Value> = serde_json::from_str(stdout)?;
    let mut findings = Vec::new();

    for file_entry in &files {
        let path = file_entry
            .get("file")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let errors = match file_entry.get("errors").and_then(|v| v.as_array()) {
            Some(e) => e,
            None => continue,
        };

        for err in errors {
            let line = err.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
            let column = err.get("column").and_then(|v| v.as_u64()).unwrap_or(0);
            let message = err.get("message").and_then(|v| v.as_str()).unwrap_or("");
            let rule = err.get("rule").and_then(|v| v.as_str()).unwrap_or("");

            findings.push(L1Finding {
                tool: String::new(),
                category: ToolCategory::Linter,
                file: PathBuf::from(path),
                line: line as u32,
                column: column as u32,
                native_severity: "error".to_string(),
                severity: "medium".to_string(),
                message: message.to_string(),
                code: if rule.is_empty() {
                    None
                } else {
                    Some(rule.to_string())
                },
            });
        }
    }

    Ok(findings)
}

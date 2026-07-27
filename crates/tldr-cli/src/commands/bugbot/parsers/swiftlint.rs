//! Parser for `swiftlint lint --reporter json` output.

use std::path::PathBuf;

use super::super::tools::{L1Finding, ToolCategory};
use super::ParseError;

pub fn parse_swiftlint_output(stdout: &str) -> Result<Vec<L1Finding>, ParseError> {
    let stdout = stdout.trim();
    if stdout.is_empty() || stdout == "[]" {
        return Ok(Vec::new());
    }

    let items: Vec<serde_json::Value> = serde_json::from_str(stdout)?;
    let mut findings = Vec::new();

    for item in &items {
        let file = item.get("file").and_then(|v| v.as_str()).unwrap_or("");
        let line = item.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
        let column = item.get("character").and_then(|v| v.as_u64()).unwrap_or(0);
        let native_sev = item
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("Warning");
        let reason = item.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        let rule_id = item.get("rule_id").and_then(|v| v.as_str()).unwrap_or("");

        let severity = match native_sev {
            "Error" => "high",
            _ => "medium",
        };

        findings.push(L1Finding {
            tool: String::new(),
            category: ToolCategory::Linter,
            file: PathBuf::from(file),
            line: line as u32,
            column: column as u32,
            native_severity: native_sev.to_lowercase(),
            severity: severity.to_string(),
            message: reason.to_string(),
            code: if rule_id.is_empty() {
                None
            } else {
                Some(rule_id.to_string())
            },
        });
    }

    Ok(findings)
}

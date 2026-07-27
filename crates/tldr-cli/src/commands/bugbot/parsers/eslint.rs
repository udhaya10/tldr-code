//! Parser for `eslint --format json` output.
//!
//! ESLint outputs a JSON array of file results. Each has `filePath` and
//! `messages` array with `ruleId`, `severity` (1=warning, 2=error),
//! `message`, `line`, and `column`.

use std::path::PathBuf;

use super::super::tools::{L1Finding, ToolCategory};
use super::ParseError;

/// Parse eslint JSON output into L1 findings.
///
/// ESLint emits a JSON array like:
/// ```json
/// [{"filePath": "/abs/path.js", "messages": [
///   {"ruleId": "no-unused-vars", "severity": 2, "message": "...",
///    "line": 5, "column": 7}
/// ]}]
/// ```
pub fn parse_eslint_output(stdout: &str) -> Result<Vec<L1Finding>, ParseError> {
    let stdout = stdout.trim();
    if stdout.is_empty() || stdout == "[]" {
        return Ok(Vec::new());
    }

    let files: Vec<serde_json::Value> = serde_json::from_str(stdout)?;
    let mut findings = Vec::new();

    for file_entry in &files {
        let file_path = file_entry
            .get("filePath")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let messages = match file_entry.get("messages").and_then(|v| v.as_array()) {
            Some(msgs) => msgs,
            None => continue,
        };

        for msg in messages {
            let rule_id = msg.get("ruleId").and_then(|v| v.as_str()).unwrap_or("");
            let eslint_severity = msg.get("severity").and_then(|v| v.as_u64()).unwrap_or(0);
            let message = msg.get("message").and_then(|v| v.as_str()).unwrap_or("");
            let line = msg.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
            let column = msg.get("column").and_then(|v| v.as_u64()).unwrap_or(0);

            let severity = eslint_severity_to_bugbot(eslint_severity, rule_id);

            findings.push(L1Finding {
                tool: String::new(), // Set by runner [PM-6]
                category: ToolCategory::Linter,
                file: PathBuf::from(file_path),
                line: line as u32,
                column: column as u32,
                native_severity: if eslint_severity == 2 {
                    "error".to_string()
                } else {
                    "warning".to_string()
                },
                severity,
                message: message.to_string(),
                code: if rule_id.is_empty() {
                    None
                } else {
                    Some(rule_id.to_string())
                },
            });
        }
    }

    Ok(findings)
}

/// Map eslint severity + rule to bugbot severity.
///
/// ESLint severity: 1 = warning, 2 = error.
/// Security-related rules (no-eval, no-implied-eval) get bumped to high.
fn eslint_severity_to_bugbot(eslint_severity: u64, rule_id: &str) -> String {
    // Security-sensitive rules are always high
    let security_rules = ["no-eval", "no-implied-eval", "no-new-func", "no-script-url"];
    if security_rules.contains(&rule_id) {
        return "high".to_string();
    }

    match eslint_severity {
        2 => "medium".to_string(), // error
        1 => "low".to_string(),    // warning
        _ => "info".to_string(),
    }
}

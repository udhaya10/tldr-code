//! Parser for `phpstan analyse --error-format=json` output.

use std::path::PathBuf;

use super::super::tools::{L1Finding, ToolCategory};
use super::ParseError;

pub fn parse_phpstan_output(stdout: &str) -> Result<Vec<L1Finding>, ParseError> {
    let stdout = stdout.trim();
    if stdout.is_empty() {
        return Ok(Vec::new());
    }

    let root: serde_json::Value = serde_json::from_str(stdout)?;
    let files = match root.get("files").and_then(|v| v.as_object()) {
        Some(f) => f,
        None => return Ok(Vec::new()),
    };

    let mut findings = Vec::new();
    for (path, file_data) in files {
        let messages = match file_data.get("messages").and_then(|v| v.as_array()) {
            Some(m) => m,
            None => continue,
        };

        for msg in messages {
            let message = msg.get("message").and_then(|v| v.as_str()).unwrap_or("");
            let line = msg.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
            let tip = msg.get("tip").and_then(|v| v.as_str());

            // PHPStan doesn't have severity levels — all findings are errors
            findings.push(L1Finding {
                tool: String::new(),
                category: ToolCategory::Linter,
                file: PathBuf::from(path),
                line: line as u32,
                column: 0,
                native_severity: "error".to_string(),
                severity: "medium".to_string(),
                message: if let Some(t) = tip {
                    format!("{} (tip: {})", message, t)
                } else {
                    message.to_string()
                },
                code: None,
            });
        }
    }

    Ok(findings)
}

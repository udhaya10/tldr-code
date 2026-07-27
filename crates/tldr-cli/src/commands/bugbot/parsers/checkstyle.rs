//! Parser for checkstyle plain-format output.
//!
//! Checkstyle with `-f plain` outputs:
//! ```text
//! Starting audit...
//! [ERROR] /path/File.java:10:5: Missing Javadoc. [MissingJavadoc]
//! [WARN] /path/File.java:20: Line too long. [LineLength]
//! Audit done.
//! ```

use std::path::PathBuf;

use super::super::tools::{L1Finding, ToolCategory};
use super::ParseError;

pub fn parse_checkstyle_output(stdout: &str) -> Result<Vec<L1Finding>, ParseError> {
    let mut findings = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();

        // Only parse lines starting with [ERROR] or [WARN]
        let (native_sev, rest) = if let Some(rest) = line.strip_prefix("[ERROR] ") {
            ("error", rest)
        } else if let Some(rest) = line.strip_prefix("[WARN] ") {
            ("warning", rest)
        } else {
            continue;
        };

        // Format: /path/File.java:10:5: message [RuleName]
        // or:     /path/File.java:10: message [RuleName]
        let parts: Vec<&str> = rest.splitn(4, ':').collect();
        if parts.len() < 3 {
            continue;
        }

        let file = parts[0];
        let line_num: u32 = parts[1].trim().parse().unwrap_or(0);

        // parts[2] could be column or start of message
        let (column, message_part) = if parts.len() >= 4 {
            // Try to parse as column
            match parts[2].trim().parse::<u32>() {
                Ok(col) => (col, parts[3].trim().to_string()),
                Err(_) => (0, format!("{}:{}", parts[2].trim(), parts[3].trim())),
            }
        } else {
            (0, parts[2].trim().to_string())
        };

        // Extract rule name from trailing [RuleName]
        let (message, code) = if let Some(bracket_start) = message_part.rfind('[') {
            if message_part.ends_with(']') {
                let rule = &message_part[bracket_start + 1..message_part.len() - 1];
                let msg = message_part[..bracket_start].trim();
                (msg.to_string(), Some(rule.to_string()))
            } else {
                (message_part, None)
            }
        } else {
            (message_part, None)
        };

        let severity = match native_sev {
            "error" => "high",
            _ => "medium",
        };

        findings.push(L1Finding {
            tool: String::new(),
            category: ToolCategory::Linter,
            file: PathBuf::from(file),
            line: line_num,
            column,
            native_severity: native_sev.to_string(),
            severity: severity.to_string(),
            message,
            code,
        });
    }

    Ok(findings)
}

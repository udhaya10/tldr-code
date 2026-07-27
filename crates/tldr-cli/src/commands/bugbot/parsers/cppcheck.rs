//! Parser for cppcheck output using `--template` format.
//!
//! We use `--template='{file}\t{line}\t{column}\t{severity}\t{id}\t{message}'`
//! to get tab-separated output that's easy to parse without XML dependencies.

use std::path::PathBuf;

use super::super::tools::{L1Finding, ToolCategory};
use super::ParseError;

pub fn parse_cppcheck_output(stdout: &str) -> Result<Vec<L1Finding>, ParseError> {
    let mut findings = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.splitn(6, '\t').collect();
        if parts.len() < 6 {
            continue; // Skip malformed lines (e.g., "Checking ..." progress output)
        }

        let file = parts[0];
        let line_num: u32 = parts[1].parse().unwrap_or(0);
        let column: u32 = parts[2].parse().unwrap_or(0);
        let native_sev = parts[3];
        let id = parts[4];
        let message = parts[5];

        // Skip "information" severity (file-level notes, not bugs)
        if native_sev == "information" {
            continue;
        }

        let severity = match native_sev {
            "error" => "high",
            "warning" => "medium",
            "style" | "performance" | "portability" => "low",
            _ => "info",
        };

        findings.push(L1Finding {
            tool: String::new(),
            category: ToolCategory::Linter,
            file: PathBuf::from(file),
            line: line_num,
            column,
            native_severity: native_sev.to_string(),
            severity: severity.to_string(),
            message: message.to_string(),
            code: if id.is_empty() {
                None
            } else {
                Some(id.to_string())
            },
        });
    }

    Ok(findings)
}

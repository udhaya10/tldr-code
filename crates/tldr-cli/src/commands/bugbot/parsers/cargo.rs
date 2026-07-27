//! Parser for cargo clippy NDJSON output
//!
//! Parses the `--message-format=json` output from `cargo clippy`, where each
//! line is a separate JSON object. Non-JSON lines and non-compiler-message
//! lines are silently skipped (match/continue, not `?` abort) per PM-5.
//!
//! The `tool` field on produced findings is set to an empty string. The runner
//! fills it in after parsing. [PM-6]

use serde::Deserialize;
use std::path::PathBuf;

use super::super::tools::{L1Finding, ToolCategory};

/// Top-level cargo JSON message
#[derive(Deserialize)]
struct CargoMessage {
    reason: String,
    #[serde(default)]
    message: Option<CargoCompilerMessage>,
}

/// Inner compiler message with level, message text, optional code, and spans
#[derive(Deserialize)]
struct CargoCompilerMessage {
    level: String,
    message: String,
    #[serde(default)]
    code: Option<CargoCode>,
    #[serde(default)]
    spans: Vec<CargoSpan>,
}

/// Diagnostic code (e.g., "unused_variables", "clippy::needless_return")
#[derive(Deserialize)]
struct CargoCode {
    code: String,
}

/// Source location span within the diagnostic
#[derive(Deserialize)]
struct CargoSpan {
    file_name: String,
    line_start: u32,
    column_start: u32,
    is_primary: bool,
}

/// Map cargo severity level strings to normalized severity strings.
///
/// - `"error"` and `"error: internal compiler error"` map to `"high"`
/// - `"warning"` maps to `"medium"`
/// - `"note"` and `"help"` map to `"info"`
/// - All other values map to `"low"`
pub fn map_cargo_severity(level: &str) -> &'static str {
    match level {
        "error" | "error: internal compiler error" => "high",
        "warning" => "medium",
        "note" | "help" => "info",
        _ => "low",
    }
}

/// Maximum number of findings to collect from a single tool run.
///
/// This is a safety limit to prevent unbounded memory growth when parsing
/// output from a very large project. 10,000 findings is far more than any
/// developer can act on; beyond this point we stop parsing and return what
/// we have.
pub const MAX_FINDINGS: usize = 10_000;

/// Parse cargo/clippy NDJSON output into L1 findings.
///
/// # Contract
/// - Skips non-JSON lines (continue, not abort) [PM-5]
/// - Skips non "compiler-message" lines
/// - Skips messages without spans
/// - Uses primary span if available, falls back to first span
/// - `tool` field is set to empty string (runner fills it in later) [PM-6]
/// - `category` is always `ToolCategory::Linter` (clippy is a linter)
/// - Severity mapping: error -> high, warning -> medium, note/help -> info, other -> low
/// - Stops collecting after `MAX_FINDINGS` to prevent unbounded growth [F2]
pub fn parse_cargo_output(stdout: &str) -> Vec<L1Finding> {
    let mut findings = Vec::new();

    for line in stdout.lines() {
        // F2: Stop collecting once we hit the safety limit
        if findings.len() >= MAX_FINDINGS {
            eprintln!(
                "bugbot: cargo parser hit MAX_FINDINGS limit ({}), stopping parse",
                MAX_FINDINGS
            );
            break;
        }

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Step 1: Try to parse as JSON -- skip on failure (continue, NOT ?) [PM-5]
        let cargo_msg: CargoMessage = match serde_json::from_str(line) {
            Ok(msg) => msg,
            Err(_) => continue,
        };

        // Step 2: Only process "compiler-message" reason
        if cargo_msg.reason != "compiler-message" {
            continue;
        }

        // Step 3: Extract the inner compiler message
        let compiler_msg = match cargo_msg.message {
            Some(msg) => msg,
            None => continue,
        };

        // Step 4: Find the span to use -- skip if no spans at all
        if compiler_msg.spans.is_empty() {
            continue;
        }

        // Prefer the primary span; fall back to the first span
        let span = compiler_msg
            .spans
            .iter()
            .find(|s| s.is_primary)
            .unwrap_or(&compiler_msg.spans[0]);

        // Step 5: Map severity
        let severity = map_cargo_severity(&compiler_msg.level);

        // Step 6: Extract code if present
        let code = compiler_msg.code.map(|c| c.code);

        // Step 7: Build L1Finding
        findings.push(L1Finding {
            tool: String::new(), // Runner fills this in [PM-6]
            category: ToolCategory::Linter,
            file: PathBuf::from(&span.file_name),
            line: span.line_start,
            column: span.column_start,
            native_severity: compiler_msg.level,
            severity: severity.to_string(),
            message: compiler_msg.message,
            code,
        });
    }

    findings
}

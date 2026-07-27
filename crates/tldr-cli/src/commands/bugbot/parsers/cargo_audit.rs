//! Parser for `cargo audit --json` output
//!
//! Parses the single JSON object output from `cargo audit --json`, which
//! reports known vulnerabilities in dependency crates listed in `Cargo.lock`.
//!
//! Unlike the cargo/clippy parser (NDJSON, one JSON object per line), this
//! parser handles a single top-level JSON object containing all results.
//!
//! The `tool` field on produced findings is set to an empty string. The runner
//! fills it in after parsing. [PM-6]

use serde::Deserialize;
use std::path::PathBuf;

use super::super::tools::{L1Finding, ToolCategory};
use super::ParseError;

/// Top-level cargo-audit JSON report
#[derive(Deserialize)]
struct AuditReport {
    vulnerabilities: AuditVulnerabilities,
}

/// Vulnerabilities section of the audit report
#[derive(Deserialize)]
struct AuditVulnerabilities {
    #[serde(default)]
    list: Vec<AuditVulnerability>,
}

/// A single vulnerability entry
#[derive(Deserialize)]
struct AuditVulnerability {
    advisory: AuditAdvisory,
}

/// Advisory metadata for a vulnerability
#[derive(Deserialize)]
struct AuditAdvisory {
    /// RUSTSEC advisory identifier (e.g., "RUSTSEC-2020-0071")
    id: String,
    /// Affected crate name
    package: String,
    /// Human-readable vulnerability title
    title: String,
}

/// Parse `cargo audit --json` output into L1 findings.
///
/// # Contract
/// - Empty output -> `ParseError::Format` (not empty Vec -- audit should always produce JSON)
/// - Valid JSON with 0 vulnerabilities -> empty Vec
/// - Each vulnerability -> one `L1Finding` with severity `"high"`
/// - File is always `"Cargo.lock"` (vulnerabilities are dependency issues)
/// - Line is always 0 (no specific line for dependency issues)
/// - `tool` field is empty string (runner fills it in later) [PM-6]
pub fn parse_cargo_audit_output(stdout: &str) -> Result<Vec<L1Finding>, ParseError> {
    let stdout = stdout.trim();
    if stdout.is_empty() {
        return Err(ParseError::Format("Empty cargo-audit output".into()));
    }

    let report: AuditReport = serde_json::from_str(stdout)?;

    let findings = report
        .vulnerabilities
        .list
        .into_iter()
        .map(|vuln| L1Finding {
            tool: String::new(), // Runner fills this in [PM-6]
            category: ToolCategory::SecurityScanner,
            file: PathBuf::from("Cargo.lock"),
            line: 0,
            column: 0,
            native_severity: "vulnerability".to_string(),
            severity: "high".to_string(),
            message: format!("{}: {}", vuln.advisory.package, vuln.advisory.title),
            code: Some(vuln.advisory.id),
        })
        .collect();

    Ok(findings)
}

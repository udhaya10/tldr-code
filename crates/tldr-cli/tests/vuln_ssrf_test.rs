//! VAL-007 (M7): SSRF detection rule end-to-end through `tldr vuln`.
//!
//! Background. The `VulnType::Ssrf` variant exists in
//! `crates/tldr-core/src/security/vuln.rs:48` and is correctly mapped at
//! the CLI boundary as of v0.2.1 hotfix M2 (commit
//! `crates/tldr-cli/src/commands/remaining/vuln.rs:712` —
//! `CoreVulnType::Ssrf => VulnType::Ssrf`). However, the upstream
//! detection rule `get_sinks(VulnType::Ssrf, lang)` at
//! `crates/tldr-core/src/security/vuln.rs:609-628` returned `vec![]` for
//! every language, AND `VulnType::Ssrf` was not part of the default
//! `vuln_types` list at `vuln.rs:838-845`. So `tldr vuln` never emitted
//! an SSRF finding even when scanning code with obvious SSRF sinks.
//!
//! Fix: populate the per-language sink patterns in the
//! `VulnType::Ssrf => match language` block AND include `VulnType::Ssrf`
//! in the default vuln_types list.
//!
//! This test exercises TypeScript and Go through the real CLI:
//!
//! - TS/Go are dispatched to `tldr_core::security::vuln::scan_vulnerabilities`
//!   from `crates/tldr-cli/src/commands/remaining/vuln.rs:641`.
//! - Python is intentionally NOT exercised here because the Python
//!   path uses `analyze_python_file` (a tree-sitter intra-procedural
//!   tracker with its own SSRF sinks at `vuln.rs:305-326` in the CLI
//!   crate), bypassing the core scanner. Python SSRF coverage of the
//!   core path is asserted via a unit test in `tldr_core::security::vuln::tests`.
//!
//! Reference shape: `crates/tldr-cli/tests/vuln_sarif_deserialization_test.rs`
//! (the M2-of-v0.2.1 sibling SARIF test).

use assert_cmd::Command;
use serde_json::Value;
use std::path::PathBuf;

fn tldr_cmd() -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("tldr"));
    command.env("TLDR_ONESHOT", "1");
    command
}

fn fixture_path(dir: &str, file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(dir)
        .join(file)
}

/// Run `tldr vuln <fixture> --lang <lang> --format json --quiet` and
/// return the parsed JSON. Mirrors the helper used in the deserialization
/// SARIF test (M2 of v0.2.1-hotfix); the vuln command exits 2 when
/// findings are present, so we use `output()` rather than `success()`.
fn run_vuln_json(fixture: &PathBuf, lang: &str) -> Value {
    assert!(
        fixture.exists(),
        "fixture missing: {} — did you delete it?",
        fixture.display()
    );

    let mut cmd = tldr_cmd();
    cmd.arg("vuln")
        .arg(fixture)
        .arg("--lang")
        .arg(lang)
        .arg("--format")
        .arg("json")
        .arg("--quiet");

    let output = cmd.output().expect("failed to execute tldr vuln");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "failed to parse `tldr vuln --lang {} --format json` stdout as JSON: {}\n--- stdout (len={}) ---\n{}\n--- stderr ---\n{}",
            lang,
            e,
            stdout.len(),
            stdout,
            String::from_utf8_lossy(&output.stderr),
        )
    })
}

/// Assert the JSON report contains at least one SSRF finding (vuln_type ==
/// "ssrf", per `#[serde(rename_all = "snake_case")]` on
/// `tldr_core::security::vuln::VulnType`). On unfixed HEAD this fails
/// because `findings` is empty (no sink patterns registered for SSRF).
fn assert_has_ssrf_finding(report: &Value, lang_label: &str) {
    let findings = report
        .get("findings")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| {
            panic!(
                "[{}] expected a `findings` array in JSON output; full report:\n{}",
                lang_label,
                serde_json::to_string_pretty(report).unwrap_or_default()
            )
        });

    assert!(
        !findings.is_empty(),
        "[{}] expected at least one SSRF finding; got `findings: []`. \
         VAL-007: the SSRF detection rule has no sink patterns registered in \
         crates/tldr-core/src/security/vuln.rs:609-628 (`VulnType::Ssrf => \
         match language` returns vec![] for every language), so `tldr vuln` \
         never emits an SSRF finding even when scanning code with obvious \
         SSRF sinks. Full report:\n{}",
        lang_label,
        serde_json::to_string_pretty(report).unwrap_or_default()
    );

    let has_ssrf = findings.iter().any(|f| {
        f.get("vuln_type")
            .and_then(|v| v.as_str())
            .map(|s| s == "ssrf")
            .unwrap_or(false)
    });

    assert!(
        has_ssrf,
        "[{}] expected at least one finding with vuln_type == \"ssrf\" (the \
         snake_case wire form of `tldr_core::security::vuln::VulnType::Ssrf`); \
         got vuln_types: {:?}. Full report:\n{}",
        lang_label,
        findings
            .iter()
            .filter_map(|f| f.get("vuln_type").and_then(|v| v.as_str()))
            .collect::<Vec<_>>(),
        serde_json::to_string_pretty(report).unwrap_or_default()
    );
}

/// VAL-007: TypeScript SSRF detection through `tldr vuln`.
///
/// Fixture sinks: `fetch(`, `axios.get(`, `axios.post(`, `http.get(`,
/// `http.request(`. Tainted source: `req.query` (Express query
/// parameter from `get_sources(Language::TypeScript)`).
#[test]
fn vuln_typescript_emits_ssrf_finding() {
    let fixture = fixture_path("ssrf_typescript", "Vuln.ts");
    let report = run_vuln_json(&fixture, "typescript");
    assert_has_ssrf_finding(&report, "typescript");
}

/// VAL-007: Go SSRF detection through `tldr vuln`.
///
/// Fixture sinks: `http.Get(`, `http.Post(`, `http.NewRequest(`. Tainted
/// source: `r.URL.Query()` (HTTP query parameters from
/// `get_sources(Language::Go)`).
#[test]
fn vuln_go_emits_ssrf_finding() {
    let fixture = fixture_path("ssrf_go", "Vuln.go");
    let report = run_vuln_json(&fixture, "go");
    assert_has_ssrf_finding(&report, "go");
}

/// VAL-007 cross-check: every SSRF finding emitted carries CWE-918, the
/// canonical CWE for Server-Side Request Forgery. Guards against a future
/// regression where someone adds the rule but forgets the CWE wire-up
/// (`get_cwe_id(VulnType::Ssrf)` at `vuln.rs:724` already returns
/// "CWE-918" — this test ensures the value reaches the JSON output).
#[test]
fn vuln_ssrf_findings_carry_cwe_918() {
    let fixture = fixture_path("ssrf_go", "Vuln.go");
    let report = run_vuln_json(&fixture, "go");

    let findings = report
        .get("findings")
        .and_then(|v| v.as_array())
        .expect("findings array");

    let ssrf_findings: Vec<&Value> = findings
        .iter()
        .filter(|f| {
            f.get("vuln_type")
                .and_then(|v| v.as_str())
                .map(|s| s == "ssrf")
                .unwrap_or(false)
        })
        .collect();

    assert!(
        !ssrf_findings.is_empty(),
        "expected at least one SSRF finding to verify CWE-918 wire-up"
    );

    for f in &ssrf_findings {
        let cwe = f.get("cwe_id").and_then(|v| v.as_str());
        assert_eq!(
            cwe,
            Some("CWE-918"),
            "SSRF finding must carry cwe_id = \"CWE-918\"; got {:?} on finding:\n{}",
            cwe,
            serde_json::to_string_pretty(f).unwrap_or_default(),
        );
    }
}

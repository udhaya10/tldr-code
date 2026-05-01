//! js-test-file-suppression-v1 — integration test for the `--include-tests`
//! flag added to `tldr vuln`.
//!
//! Closes a medium-severity FP class empirically reproed on
//! `/tmp/repos/express`: `tldr vuln --lang javascript /tmp/repos/express`
//! emitted 2 path_traversal findings, BOTH inside the project's `test/`
//! directory (synthetic fixtures exercising sink behavior). Mirrors the
//! Rust `is_rust_test_file` mask in `analyze_rust_file`; applies at the
//! filter layer in `VulnArgs::run` so file collection is unchanged
//! (the canonical taint engine's own self-tests still drive their
//! fixtures), only post-analysis findings are masked.
//!
//! Tests:
//!   - `js_test_file_findings_suppressed_by_default`: synthetic fixture
//!     with `test/` subdir containing JS file with a known FileOpen sink;
//!     expect ZERO findings on default invocation.
//!   - `js_test_file_findings_emitted_with_include_tests`: same fixture,
//!     pass `--include-tests`; expect ≥1 finding (flag is opt-in,
//!     verifying it's not a one-way drop).
//!   - `ts_test_file_findings_suppressed_by_default`: TypeScript parity.
//!   - `js_production_file_findings_unaffected`: regression guard —
//!     a JS file OUTSIDE any test directory MUST still emit findings
//!     on default invocation (verifies the predicate is not over-broad).

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use tempfile::TempDir;

fn run_tldr_vuln(path: &std::path::Path, lang: &str, include_tests: bool) -> Value {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("tldr"));
    cmd.arg("vuln")
        .arg(path)
        .arg("--lang")
        .arg(lang)
        .arg("--format")
        .arg("json")
        .arg("--quiet");
    if include_tests {
        cmd.arg("--include-tests");
    }

    let output = cmd.output().expect("failed to execute tldr vuln");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "failed to parse `tldr vuln --lang {} --format json` JSON output: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            lang,
            e,
            stdout,
            String::from_utf8_lossy(&output.stderr),
        )
    })
}

fn findings_count(report: &Value) -> usize {
    report
        .get("findings")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0)
}

/// Build a minimal JS file containing a confirmed taint flow:
/// `req.query.p` (HttpParam source) -> `fs.readFileSync(p, ...)` (FileOpen sink).
/// This is the canonical positive shape from
/// `tests/fixtures/vuln_migration_v1/javascript/path_traversal_positive.js`.
fn js_taint_source() -> &'static str {
    "export function handler(req, res) {\n    const p = req.query.p;\n    fs.readFileSync(p, 'utf8');\n}\n"
}

fn ts_taint_source() -> &'static str {
    "export function handler(req: any, res: any) {\n    const p = req.query.p;\n    fs.readFileSync(p, 'utf8');\n}\n"
}

#[test]
fn js_test_file_findings_suppressed_by_default() {
    let temp = TempDir::new().unwrap();
    let test_dir = temp.path().join("test");
    fs::create_dir_all(&test_dir).unwrap();
    let fixture = test_dir.join("synthetic_handler.js");
    fs::write(&fixture, js_taint_source()).unwrap();

    let report = run_tldr_vuln(temp.path(), "javascript", false);
    assert_eq!(
        findings_count(&report),
        0,
        "default invocation MUST suppress findings on JS files under `test/`; got report: {}",
        report
    );
}

#[test]
fn js_test_file_findings_emitted_with_include_tests() {
    let temp = TempDir::new().unwrap();
    let test_dir = temp.path().join("test");
    fs::create_dir_all(&test_dir).unwrap();
    let fixture = test_dir.join("synthetic_handler.js");
    fs::write(&fixture, js_taint_source()).unwrap();

    let report = run_tldr_vuln(temp.path(), "javascript", true);
    let count = findings_count(&report);
    assert!(
        count >= 1,
        "--include-tests MUST restore findings on JS files under `test/`; got 0 findings, report: {}",
        report
    );
}

#[test]
fn ts_test_file_findings_suppressed_by_default() {
    let temp = TempDir::new().unwrap();
    let tests_dir = temp.path().join("tests");
    fs::create_dir_all(&tests_dir).unwrap();
    let fixture = tests_dir.join("synthetic_handler.ts");
    fs::write(&fixture, ts_taint_source()).unwrap();

    let report = run_tldr_vuln(temp.path(), "typescript", false);
    assert_eq!(
        findings_count(&report),
        0,
        "default invocation MUST suppress findings on TS files under `tests/`; got report: {}",
        report
    );
}

#[test]
fn js_dotted_test_filename_suppressed_by_default() {
    // Same fixture but at top level with a `.test.js` filename.
    let temp = TempDir::new().unwrap();
    let fixture = temp.path().join("synthetic_handler.test.js");
    fs::write(&fixture, js_taint_source()).unwrap();

    let report = run_tldr_vuln(temp.path(), "javascript", false);
    assert_eq!(
        findings_count(&report),
        0,
        "default invocation MUST suppress findings on `*.test.js` files; got report: {}",
        report
    );
}

#[test]
fn js_production_file_findings_unaffected() {
    // Regression guard: a JS file OUTSIDE any test directory must still
    // emit the path_traversal finding on default invocation. This is the
    // proof that the suppression predicate is not over-broad and that
    // the canonical taint pipeline is still firing on production code.
    let temp = TempDir::new().unwrap();
    let src_dir = temp.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let fixture = src_dir.join("handler.js");
    fs::write(&fixture, js_taint_source()).unwrap();

    let report = run_tldr_vuln(temp.path(), "javascript", false);
    assert!(
        findings_count(&report) >= 1,
        "production JS file under `src/` MUST still emit findings on default invocation; got 0 findings, report: {}",
        report
    );
}

//! js-res-json-fp-narrowing-v1 — RED guard for the dropped
//! `(res|response|Response|NextResponse).json` PathTraversal/FileWrite
//! sink entries.
//!
//! Pre-fix repro (Express, post-M4.1):
//!   `tldr vuln --lang javascript /tmp/repos/express/lib/express.raw.js`
//!   would emit a `path_traversal` finding on
//!   `res.json({ buf: req.body.toString('hex') })` (line 506 in the
//!   Express test fixture). M4.1 suppresses test files by default —
//!   but the underlying pattern bank still classifies `res.json` as a
//!   FileWrite sink, so the FP would re-fire on PRODUCTION code
//!   anywhere `res.json` appears with tainted input.
//!
//! Fix shape: drop `(res, json)`, `(response, json)`, `(Response, json)`,
//! `(NextResponse, json)` from the JS/TS FileWrite sink bank entirely.
//! These are framework JSON-response writers (Express / NestJS /
//! Next.js App Router), never file operations. Reflected JSON
//! `Content-Type: application/json` is also NOT an XSS vector when
//! the browser respects the content type, so the entries are not
//! reclassified to HtmlOutput either.
//!
//! Validation:
//!   - JS FP fixture: ZERO path_traversal findings.
//!   - TS FP fixture: ZERO path_traversal findings.
//!   - JS positive fixture (`fs.readFileSync(p, ...)`): STILL emits ≥1
//!     path_traversal — verifies the FileOpen bank is untouched.
//!   - TS positive fixture: same.

use assert_cmd::Command;
use serde_json::Value;
use std::path::PathBuf;

fn fixture_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("vuln_migration_v1")
        .join(rel)
}

fn run_tldr_vuln(rel_fixture: &str, lang: &str) -> Value {
    let path = fixture_path(rel_fixture);
    assert!(path.exists(), "fixture missing: {}", path.display());

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("tldr"));
    cmd.env("TLDR_ONESHOT", "1");
    cmd.arg("vuln")
        .arg(&path)
        .arg("--lang")
        .arg(lang)
        .arg("--format")
        .arg("json")
        .arg("--quiet");

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

fn count_findings_of_type(report: &Value, vt_wire: &str) -> usize {
    report
        .get("findings")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|f| {
                    f.get("vuln_type")
                        .and_then(|v| v.as_str())
                        .map(|s| s == vt_wire)
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn js_path_traversal_res_json_fp_zero_findings() {
    let report = run_tldr_vuln("javascript/path_traversal_res_json_fp.js", "javascript");
    let count = count_findings_of_type(&report, "path_traversal");
    assert_eq!(
        count, 0,
        "res.json/response.json/Response.json/NextResponse.json MUST NOT emit path_traversal findings; got {} in report: {}",
        count, report
    );
}

#[test]
fn ts_path_traversal_res_json_fp_zero_findings() {
    let report = run_tldr_vuln("typescript/path_traversal_res_json_fp.ts", "typescript");
    let count = count_findings_of_type(&report, "path_traversal");
    assert_eq!(
        count, 0,
        "res.json/response.json/Response.json/NextResponse.json MUST NOT emit path_traversal findings; got {} in report: {}",
        count, report
    );
}

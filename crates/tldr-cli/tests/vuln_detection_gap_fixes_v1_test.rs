//! detection-gap-fixes-v1 — regression-guard tests for the two detection
//! gaps closed by this milestone:
//!
//!   1. Python Flask f-string XSS via view-function return (canonical
//!      `return f"<h1>{user}</h1>"` shape).
//!   2. Next.js `NextResponse.json(tainted)` reflected XSS — restored
//!      after `js-res-json-fp-narrowing-v1` removed the (incorrect)
//!      FileWrite/PathTraversal classification.
//!
//! Plus an FP guard preserving the prior milestone's fix:
//!
//!   3. Express `res.json(req.body)` — still emits ZERO `path_traversal`
//!      findings (preserves `js-res-json-fp-narrowing-v1`'s narrowing).
//!
//! Test pattern mirrors `vuln_js_res_json_fp_narrowing_v1_test.rs` —
//! command-line invocation of the `tldr` binary against an inline
//! fixture, JSON parsing, finding-type counting.

use assert_cmd::Command;
use serde_json::Value;
use std::path::PathBuf;

fn run_tldr_vuln_inline(source: &str, file_name: &str, lang: &str) -> Value {
    // Write fixture to a tempdir and invoke `tldr vuln`.
    let tempdir = tempfile::tempdir().expect("failed to create tempdir");
    let path: PathBuf = tempdir.path().join(file_name);
    std::fs::write(&path, source).expect("failed to write fixture");

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

/// Gap 1 — Python Flask f-string XSS via view-function return.
///
/// `request.args.get(...)` is a tainted source; the f-string return
/// reflects the tainted variable as HTML. Pre-fix the canonical taint
/// pipeline did not see the `return f"..."` shape as a sink (the bare
/// f-string is neither a call-shape nor a member-access shape, and the
/// `string` AST node is filtered by `is_in_string` upstream of the AST
/// pattern matcher). Post-fix: dedicated dispatch arm in
/// `detect_sinks_ast` classifies `return_statement` -> `string` (with
/// at least one `interpolation`) as an `HtmlOutput` sink.
#[test]
fn test_xss_python_fstring_view_return() {
    let src = r#"
from flask import Flask, request

app = Flask(__name__)

@app.route('/echo')
def echo():
    name = request.args.get('name')
    return f"<h1>Hello {name}</h1>"
"#;
    let report = run_tldr_vuln_inline(src, "fstring_xss.py", "python");
    let xss = count_findings_of_type(&report, "xss");
    assert!(
        xss >= 1,
        "expected ≥1 xss finding for Python Flask f-string view return; got {} in report: {}",
        xss,
        report
    );
}

/// Gap 2 — Next.js `NextResponse.json(tainted)` reflected XSS.
///
/// Pre-fix: `js-res-json-fp-narrowing-v1` removed
/// `(NextResponse, json)` from the FileWrite bank (correct: not a
/// path-traversal sink) but did not add an HtmlOutput equivalent —
/// reflected user input emitted as a JSON response body went undetected.
/// Post-fix: dedicated AstSinkPattern entry classifies the call as
/// HtmlOutput (Xss / CWE-79).
#[test]
fn test_xss_nextjs_response_json_reflected() {
    let src = r#"
export async function POST(request) {
    const data = await request.json();
    return NextResponse.json(data);
}
"#;
    let report = run_tldr_vuln_inline(src, "nextresponse_json.ts", "typescript");
    let xss = count_findings_of_type(&report, "xss");
    assert!(
        xss >= 1,
        "expected ≥1 xss finding for NextResponse.json(tainted); got {} in report: {}",
        xss,
        report
    );
}

/// FP guard — Express `res.json(req.body)` still emits ZERO
/// `path_traversal` findings.
///
/// `js-res-json-fp-narrowing-v1` removed `(res, json)` from FileWrite to
/// kill the `path_traversal` FP class on every Express handler that
/// echoed user input as JSON. detection-gap-fixes-v1 only adds
/// `(NextResponse, json)` (and `redirect`) to HtmlOutput — the Express
/// `res.json` shape is intentionally NOT reclassified. This guard
/// preserves the prior milestone's narrowing.
#[test]
fn test_xss_express_res_json_no_path_traversal() {
    let src = r#"
function handler(req, res) {
    res.json(req.body);
}
"#;
    let report = run_tldr_vuln_inline(src, "express_res_json.js", "javascript");
    let path_traversal = count_findings_of_type(&report, "path_traversal");
    assert_eq!(
        path_traversal, 0,
        "Express res.json(req.body) MUST NOT emit path_traversal findings (preserves js-res-json-fp-narrowing-v1); got {} in report: {}",
        path_traversal, report
    );
}

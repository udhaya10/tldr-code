//! VAL-011 (M12): `tldr vuln <ts-fixture>` (no `--lang`) autodetects
//! TypeScript and runs taint analysis successfully.
//!
//! ## Background
//!
//! VAL-006 of v0.2.0 added the `is_natively_analyzed(Language)` gate at
//! `crates/tldr-cli/src/commands/remaining/vuln.rs:586` to prevent
//! `tldr vuln .` (no `--lang`) from silently delivering weaker analysis
//! on a non-Python/Rust tree. The original gate listed only `Python` and
//! `Rust`, so autodetected TypeScript exited with code 2 and the message
//! `"taint analysis for typescript is not yet supported by autodetect"`.
//!
//! That gate is now overly conservative. The taint engine at
//! `crates/tldr-core/src/security/taint.rs:909` already routes
//! `Language::TypeScript | Language::JavaScript` through
//! `TYPESCRIPT_PATTERNS` (sources at `taint.rs:451-464`, sinks at
//! `taint.rs:465-480`, sanitizers at `taint.rs:481-486` — all populated).
//! VAL-007 of v0.2.2 (M7) expanded the TypeScript sink set further by
//! adding SSRF patterns. The CLI gate just hadn't been told.
//!
//! ## Reference: GitHub issue parcadei/tldr-code#1, sub-issue #1.C
//! (2026-04-26 retest).
//!
//! ## Test shape
//!
//! Reuses the v0.2.2 M7 SSRF TypeScript fixture
//! (`tests/fixtures/ssrf_typescript/Vuln.ts`) — a small TS file with
//! tainted user input (`req.query.url`) flowing into a URL-fetching
//! sink (`fetch(target)`, `axios.get(target)`, etc.). The fixture is
//! stable on `main` from VAL-007's commit `372b206`.
//!
//! On unfixed HEAD `tldr vuln <fixture>` (NO `--lang`) MUST exit 2
//! with `"not yet supported"` in stderr. After the fix, it MUST exit
//! 0 with at least one finding whose `file` field ends in `.ts`
//! (proving autodetect picked TypeScript and the scan actually
//! produced a finding).

use assert_cmd::Command;
use serde_json::Value;
use std::path::PathBuf;

fn tldr_cmd() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin!("tldr"))
}

fn fixture_path(dir: &str, file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(dir)
        .join(file)
}

/// VAL-011: `tldr vuln <ts-file>` (NO `--lang`) autodetects TypeScript
/// and produces a successful scan.
///
/// On unfixed HEAD this fails with exit code 2 and stderr containing
/// `"not yet supported"` because `is_natively_analyzed` returns `false`
/// for `Language::TypeScript`. After the fix, exit code MUST be 0 and
/// stdout MUST parse as JSON containing a non-empty `findings` array
/// where at least one finding's `file` ends in `.ts`.
#[test]
fn vuln_typescript_autodetects_without_explicit_lang() {
    let fixture = fixture_path("ssrf_typescript", "Vuln.ts");
    assert!(
        fixture.exists(),
        "fixture missing: {} — did v0.2.2 M7 (VAL-007) fixture get \
         deleted? this test depends on it.",
        fixture.display()
    );

    let mut cmd = tldr_cmd();
    cmd.arg("vuln")
        .arg(&fixture)
        .arg("--format")
        .arg("json")
        .arg("--quiet");
    // Intentionally NOT passing --lang — that's the whole point: VAL-011
    // is about the autodetect path. An explicit --lang typescript
    // already worked pre-fix (test_vuln_honors_explicit_lang_typescript
    // in vuln_autodetect_tests.rs covers that).

    let output = cmd.output().expect("failed to execute tldr vuln");
    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    // The vuln command exits 2 in two unrelated cases:
    //   (a) autodetect rejected the language ("not yet supported")
    //   (b) findings were present in the scan output
    //
    // Pre-VAL-011 (RED): case (a) — stderr contains "not yet supported"
    //   AND stdout is empty.
    // Post-VAL-011 (GREEN): case (b) — stderr does NOT contain
    //   "not yet supported" AND stdout is a valid JSON report.
    //
    // We disambiguate by inspecting stderr+stdout, NOT the bare exit
    // code (the existing v0.2.2 M7 SSRF integration test in
    // vuln_ssrf_test.rs uses the same shape: `output()` not
    // `success()`, then parses stdout).
    assert!(
        !stderr.contains("not yet supported"),
        "VAL-011: stderr MUST NOT contain 'not yet supported' after the \
         autodetect gate is widened to include TypeScript. Pre-fix RED \
         message hit. Got stderr:\n{}\n--- stdout ---\n{}",
        stderr,
        stdout
    );
    assert!(
        exit_code == 0 || exit_code == 2,
        "VAL-011: `tldr vuln <ts-file>` (no --lang) must exit 0 (clean) \
         or 2 (findings present, per CLI convention). Got exit code {}.\n\
         --- stderr ---\n{}\n--- stdout ---\n{}",
        exit_code,
        stderr,
        stdout
    );

    let report: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "VAL-011: stdout from `tldr vuln --format json --quiet` must \
             parse as JSON; got error: {}\n--- stdout (len={}) ---\n{}\n\
             --- stderr ---\n{}",
            e,
            stdout.len(),
            stdout,
            stderr,
        )
    });

    let findings = report
        .get("findings")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| {
            panic!(
                "VAL-011: JSON report must have a `findings` array; \
                 full report:\n{}",
                serde_json::to_string_pretty(&report).unwrap_or_default()
            )
        });

    assert!(
        !findings.is_empty(),
        "VAL-011: `tldr vuln <ts-file>` autodetect must produce at least \
         one finding for the SSRF fixture (which has 5 tainted-fetch sinks). \
         Got `findings: []`. Full report:\n{}",
        serde_json::to_string_pretty(&report).unwrap_or_default()
    );

    // The CLI's VulnFinding struct (crates/tldr-cli/src/commands/remaining/types.rs:1435)
    // does NOT carry a per-finding `language` field — language is
    // implicit in the file extension. Asserting the file path ends in
    // `.ts` is the strongest direct signal that the TypeScript path
    // was actually taken (rather than e.g. a stray Python sibling
    // fixture being scanned by mistake).
    let any_ts_finding = findings.iter().any(|f| {
        f.get("file")
            .and_then(|v| v.as_str())
            .map(|s| s.ends_with(".ts") || s.ends_with(".tsx"))
            .unwrap_or(false)
    });
    assert!(
        any_ts_finding,
        "VAL-011: at least one finding's `file` field must end in `.ts` \
         or `.tsx` (proving autodetect picked TypeScript and the scanner \
         actually flagged the fixture). Got files: {:?}.\nFull report:\n{}",
        findings
            .iter()
            .filter_map(|f| f.get("file").and_then(|v| v.as_str()))
            .collect::<Vec<_>>(),
        serde_json::to_string_pretty(&report).unwrap_or_default()
    );
}

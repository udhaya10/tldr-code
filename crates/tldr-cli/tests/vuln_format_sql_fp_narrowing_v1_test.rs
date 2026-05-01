//! rust-format-sql-fp-narrowing-v1 — RED guards for the narrowed
//! `format!()` SqlInjection trigger in `analyze_rust_file`.
//!
//! Closes a high-severity false-positive class empirically reproed on
//! `tldr vuln --lang rust /tmp/repos/ripgrep/crates`: 4 critical-severity
//! SqlInjection findings on plain `format!()` macros containing ZERO SQL
//! keywords. Root cause: the legacy `contains_sql_keyword` predicate
//! uppercased the WHOLE line and substring-matched against {SELECT,
//! INSERT, UPDATE, DELETE, FROM, WHERE} — `char::from(` and
//! `Box::<...>::from(format!(...))` substring-matched `FROM`.
//!
//! This file ships TWO tests:
//!   - `rust_format_sql_no_keyword_fp` — FP regression-guard. Three
//!     pre-fix FP shapes (bash/fish/powershell-style `char::from(...)`
//!     interpolation, `err!`-macro `Box::<...>::from(format!(...))`,
//!     plain interpolation with NO SQL anywhere) MUST produce ZERO
//!     SqlInjection findings post-fix.
//!   - `rust_format_sql_keyword_positive` — TP guard. The legitimate
//!     case `format!("SELECT * FROM users WHERE id = {}", id)` MUST
//!     STILL emit ≥1 SqlInjection finding.
//!
//! Both tests drive the binary via `tldr vuln --lang rust --format json`,
//! mirroring the integration pattern in `vuln_migration_v1_red.rs`.

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

/// Run `tldr vuln <fixture> --lang rust --format json --quiet` and parse JSON.
fn run_tldr_vuln_rust(rel_fixture: &str) -> Value {
    let path = fixture_path(rel_fixture);
    assert!(path.exists(), "fixture missing: {}", path.display());

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("tldr"));
    cmd.arg("vuln")
        .arg(&path)
        .arg("--lang")
        .arg("rust")
        .arg("--format")
        .arg("json")
        .arg("--quiet");

    let output = cmd.output().expect("failed to execute tldr vuln");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "failed to parse `tldr vuln --lang rust --format json` JSON output: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            e,
            stdout,
            String::from_utf8_lossy(&output.stderr),
        )
    })
}

fn findings_of_type<'a>(report: &'a Value, vt_wire: &str) -> Vec<&'a Value> {
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
                .collect()
        })
        .unwrap_or_default()
}

/// FP regression-guard. Three pre-fix FP shapes — `format!("-{}",
/// char::from(...))` flag formatters (bash/fish/powershell), the
/// `Box::<...>::from(format!(...))` err! macro pass-through, and a
/// plain interpolation with NO SQL keyword anywhere — MUST emit ZERO
/// SqlInjection findings post-fix.
#[test]
fn rust_format_sql_no_keyword_fp() {
    let report = run_tldr_vuln_rust("rust/sql_injection_format_no_keyword_fp.rs");
    let sql = findings_of_type(&report, "sql_injection");
    assert!(
        sql.is_empty(),
        "expected ZERO sql_injection findings (rust-format-sql-fp-narrowing-v1 regression-guard) for fixture rust/sql_injection_format_no_keyword_fp.rs (lang=rust); got {} findings: {}",
        sql.len(),
        serde_json::to_string_pretty(&report).unwrap_or_default()
    );
}

/// TP guard. `format!("SELECT * FROM users WHERE id = {}", id)` MUST
/// still fire the SqlInjection trigger after the FP narrowing.
#[test]
fn rust_format_sql_keyword_positive() {
    let report = run_tldr_vuln_rust("rust/sql_injection_format_keyword_positive.rs");
    let sql = findings_of_type(&report, "sql_injection");
    assert!(
        !sql.is_empty(),
        "expected ≥1 sql_injection finding (rust-format-sql-fp-narrowing-v1 TP guard) for fixture rust/sql_injection_format_keyword_positive.rs (lang=rust); got: {}",
        serde_json::to_string_pretty(&report).unwrap_or_default()
    );
}

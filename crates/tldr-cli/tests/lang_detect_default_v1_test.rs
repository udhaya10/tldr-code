//! lang-detect-default-v1 — RED guard for the misleading "(Python)"
//! progress banner that printed BEFORE the path-existence check fired.
//!
//! Pre-fix repro:
//!   `tldr structure /tmp/this/does/not/exist`
//!   would emit:
//!       "Extracting structure from /tmp/this/does/not/exist (Python)..."
//!       "Error: Path not found: /tmp/this/does/not/exist"
//!   The "(Python)" banner falsely implies the lang detector ran and
//!   chose Python, when in fact `Language::from_directory` returned
//!   `None` (path doesn't exist → empty walk) and the call site
//!   silently fell back to `Python` via `.unwrap_or(Language::Python)`.
//!
//! Fix: validate the path BEFORE language detection and the progress
//! banner in every directory-rooted subcommand that uses
//! `Language::from_directory(...).unwrap_or(Language::Python)`.
//!
//! Validation: no language parenthetical in stderr/stdout, and the
//! "Path not found: <path>" error MUST be present.
//!
//! Subcommands covered (must all be GREEN after fix):
//!   - structure
//!   - calls
//!   - dead
//!   - impact
//!   - importers
//!   - search

use assert_cmd::Command;

const MISSING_PATH: &str = "/tmp/tldr-lang-detect-default-v1-does-not-exist-xyz123";

fn run_tldr(args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("tldr"));
    cmd.env("TLDR_ONESHOT", "1");
    let output = cmd.args(args).output().expect("tldr binary missing");
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (code, stdout, stderr)
}

/// The only language names that would appear in a `({:?})` or `({})` banner
/// — Debug repr of the `Language` enum or the lowercase `as_str()` form.
const LANG_TOKENS: &[&str] = &[
    "(Python)",
    "(python)",
    "(TypeScript)",
    "(typescript)",
    "(JavaScript)",
    "(javascript)",
    "(Rust)",
    "(rust)",
    "(Go)",
    "(go)",
    "(Java)",
    "(java)",
    "(Cpp)",
    "(cpp)",
    "(C)",
    "(c)",
];

fn assert_no_lang_banner(stream: &str, label: &str) {
    for tok in LANG_TOKENS {
        assert!(
            !stream.contains(tok),
            "{label} contained misleading lang banner {tok:?}; full output:\n{stream}"
        );
    }
}

fn assert_path_not_found(stderr: &str) {
    assert!(
        stderr.contains("Path not found"),
        "expected 'Path not found' in stderr, got:\n{stderr}"
    );
}

// =============================================================================
// structure (the canonical repro)
// =============================================================================

#[test]
fn structure_missing_path_no_lang_banner() {
    let (code, stdout, stderr) = run_tldr(&["structure", MISSING_PATH, "-q"]);
    assert_ne!(code, 0, "expected failure on missing path");
    assert_no_lang_banner(&stdout, "stdout");
    assert_no_lang_banner(&stderr, "stderr");
    assert_path_not_found(&stderr);
}

// =============================================================================
// calls
// =============================================================================

#[test]
fn calls_missing_path_no_lang_banner() {
    let (code, stdout, stderr) = run_tldr(&["calls", MISSING_PATH, "-q"]);
    assert_ne!(code, 0, "expected failure on missing path");
    assert_no_lang_banner(&stdout, "stdout");
    assert_no_lang_banner(&stderr, "stderr");
    assert_path_not_found(&stderr);
}

// =============================================================================
// dead
// =============================================================================

#[test]
fn dead_missing_path_no_lang_banner() {
    let (code, stdout, stderr) = run_tldr(&["dead", MISSING_PATH, "-q"]);
    assert_ne!(code, 0, "expected failure on missing path");
    assert_no_lang_banner(&stdout, "stdout");
    assert_no_lang_banner(&stderr, "stderr");
    assert_path_not_found(&stderr);
}

// =============================================================================
// impact
// =============================================================================

#[test]
fn impact_missing_path_no_lang_banner() {
    let (code, stdout, stderr) = run_tldr(&["impact", "some_func", MISSING_PATH, "-q"]);
    assert_ne!(code, 0, "expected failure on missing path");
    assert_no_lang_banner(&stdout, "stdout");
    assert_no_lang_banner(&stderr, "stderr");
    assert_path_not_found(&stderr);
}

// =============================================================================
// importers
// =============================================================================

#[test]
fn importers_missing_path_no_lang_banner() {
    let (code, stdout, stderr) = run_tldr(&["importers", "some_module", MISSING_PATH, "-q"]);
    assert_ne!(code, 0, "expected failure on missing path");
    assert_no_lang_banner(&stdout, "stdout");
    assert_no_lang_banner(&stderr, "stderr");
    assert_path_not_found(&stderr);
}

// =============================================================================
// search
// =============================================================================

#[test]
fn search_missing_path_no_lang_banner() {
    let (code, stdout, stderr) = run_tldr(&["search", "needle", MISSING_PATH, "-q"]);
    assert_ne!(code, 0, "expected failure on missing path");
    assert_no_lang_banner(&stdout, "stdout");
    assert_no_lang_banner(&stderr, "stderr");
    assert_path_not_found(&stderr);
}

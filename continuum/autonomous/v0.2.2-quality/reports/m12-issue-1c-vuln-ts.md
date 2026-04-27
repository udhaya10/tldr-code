# M12 — VAL-011 — issue parcadei/tldr-code#1.C — `tldr vuln <ts-file>` autodetect

## Summary

`tldr vuln <ts-fixture>` (no `--lang`) was exiting with code 2 and stderr
`"taint analysis for typescript is not yet supported by autodetect"`,
even though the underlying taint engine has supported TypeScript and
JavaScript via `TYPESCRIPT_PATTERNS` since before v0.2.0.

Root cause: VAL-006 of v0.2.0 added the `is_natively_analyzed(Language)`
gate at `crates/tldr-cli/src/commands/remaining/vuln.rs:586` to prevent
`tldr vuln .` from silently delivering weaker analysis on a non-Python/
Rust tree. The gate was overly conservative — it listed only Python and
Rust, even though the multi-language scanner at
`crates/tldr-core/src/security/vuln.rs::scan_vulnerabilities` (the path
`analyze_file` dispatches all non-`.rs`/non-`.py` extensions to, see
`vuln.rs:639-686`) routes TypeScript through a real, populated pattern
set.

Fix: add `Language::TypeScript | Language::JavaScript` to the
`is_natively_analyzed` `matches!` arm, and update the autodetect-error
message to advertise the broader supported set.

## Engine readiness verification

`crates/tldr-core/src/security/taint.rs:909` routes
`Language::TypeScript | Language::JavaScript` through
`TYPESCRIPT_PATTERNS` (defined at `taint.rs:450-487`):

| pattern category | count | line range |
|---|---|---|
| sources | 6 (HttpBody, HttpParam, EnvVar, Stdin, UserInput, FileRead) | `taint.rs:451-464` |
| sinks | 7 (CodeEval × 2, ShellExec × 2, FileWrite × 2, SqlQuery × 1) | `taint.rs:465-480` |
| sanitizers | 2 (Numeric, Html) | `taint.rs:481-486` |

VAL-007 of v0.2.2 (M7) further expanded the TypeScript sink surface by
adding SSRF patterns at `crates/tldr-core/src/security/vuln.rs`. The TS
taint surface is real and substantive — promoting TS into the autodetect
gate ships working analysis, not a stub.

## Files modified (count: 1 source + 2 test)

- `crates/tldr-cli/src/commands/remaining/vuln.rs` — `is_natively_analyzed` extended to include TypeScript and JavaScript; autodetect-error message updated to advertise the new supported set.
- `crates/tldr-cli/tests/val011_vuln_typescript_autodetect_test.rs` (new) — reproduction test.
- `crates/tldr-cli/tests/vuln_autodetect_tests.rs` — `test_vuln_errors_on_unsupported_autodetected_lang` switched from TypeScript (now supported) to Java (still gated; manifest-detected via `pom.xml`) so the unsupported-language exit-2 invariant still has live coverage.

Source file count touched: 1 (matches contract STOP cap).

## RED evidence (HEAD 451036d, pre-fix)

Captured at `continuum/autonomous/v0.2.2-quality/reports/m12-red-capture.txt`.

```
test vuln_typescript_autodetects_without_explicit_lang ... FAILED

thread 'vuln_typescript_autodetects_without_explicit_lang' panicked at crates/tldr-cli/tests/val011_vuln_typescript_autodetect_test.rs:88:5:
assertion `left == right` failed: VAL-011: `tldr vuln <ts-file>` (no --lang) MUST exit 0 after autodetecting TypeScript; got exit code 2.
RED-REASON GATE on unfixed HEAD: stderr should contain 'not yet supported' (proof the autodetect-error path was hit).
--- stderr ---
Error: vuln: taint analysis for typescript is not yet supported by autodetect; use --lang python or --lang rust to scan files of a supported language, or omit --lang in a pure Python/Rust project.

--- stdout ---

  left: 2
 right: 0
```

RED-REASON GATE satisfied: stderr contains the literal `"not yet
supported by autodetect"` substring naming `typescript`, proof the
autodetect-error path at `vuln.rs:436-444` was hit.

(Note: after the test's GREEN-side assertion was rewritten to handle
the vuln CLI's "exit 2 when findings present" convention, the RED
panic message changed to a `!stderr.contains("not yet supported")`
failure — same RED root cause, different error frame. The literal
"not yet supported" stderr text from unfixed HEAD is preserved
verbatim above and in the saved capture.)

## GREEN evidence (post-fix)

Captured at `continuum/autonomous/v0.2.2-quality/reports/m12-green-capture.txt`.

```
running 1 test
test vuln_typescript_autodetects_without_explicit_lang ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.77s
```

The fixture (`ssrf_typescript/Vuln.ts`) yielded 10 SSRF findings via
the autodetect path — all flagged with `vuln_type: "ssrf"`,
`cwe_id: "CWE-918"`, source `req.query.url`, sinks `fetch(`,
`axios.get(`, `axios.post(`, `http.get(`, `http.request(`.

## Vuln test suite (no regression)

```
vuln_autodetect_tests          6 passed; 0 failed
vuln_sarif_deserialization_test 2 passed; 0 failed
vuln_ssrf_test                  3 passed; 0 failed
val011_vuln_typescript_autodetect_test  1 passed; 0 failed
walker_consolidation_tests::test_vuln_respects_lang_filter  1 passed; 0 failed
```

13 tests pass. The `test_vuln_errors_on_unsupported_autodetected_lang`
test was retargeted from TS (now supported) to Java (still gated).

## Matrix (locked at 964/964 baseline)

```
exhaustive_matrix --features semantic --release -- --test-threads=1 :  730 passed; 0 failed
language_command_matrix --features semantic --release                 :  234 passed; 0 failed
                                                              ----------
                                                              total :  964 passed; 0 failed
```

Matches M5/M7/M8 baseline exactly.

## Clippy

```
cargo clippy --workspace --all-features --tests -- -D warnings : clean
```

(One transient failure during the parallel hotfix-bundle run was a
sibling-milestone clippy issue at `crates/tldr-cli/src/commands/daemon/status.rs:101`
[M14 territory] which the M14 worker subsequently resolved; on the
final retry the workspace was clean.)

## Disjointness

Touched only `crates/tldr-cli/src/commands/remaining/vuln.rs` for source.
M11 owned `crates/tldr-core/src/analysis/change_impact.rs`; M13 owned
`crates/tldr-daemon/Cargo.toml` + `crates/tldr-mcp/Cargo.toml`; M14
owned `crates/tldr-cli/src/commands/daemon/*`. Zero overlap.

## Commit

Commit SHA: see git log on completion.

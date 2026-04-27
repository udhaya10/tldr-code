# M1 — VAL-001 — `tldr smells` PR-Focused Signal Filter

**Worker:** kraken (M1 VAL-001 v0.2.3)
**Issue:** parcadei/tldr-code#1.D
**Starting HEAD:** `69fe94c` (v0.2.2 release tag)
**Status:** PASSED

---

## Pre-flight

- HEAD verified `69fe94c932fad1da4882a4dbd3882d0b1e667cee`.
- Pre-existing dirty files in `continuum/autonomous/*` from prior orchestrators were left untouched. None entered the M1 commit (verified post-commit via explicit-staging guard).

## Problem

`tldr smells .` on the canonical repo (v0.2.2, 69fe94c) produces 5497 findings; 1045 (19%) come from test files that sort to the top of the alphabetical output, making the default useless as a PR review signal. Additionally there is no way to scope a smells run to a caller-supplied list of files — every invocation walks the full project tree.

## Solution Overview (R1–R6 step-by-step)

### R1 — Reuse the existing `is_test_file` helper
Used the public function at `crates/tldr-core/src/analysis/clones/filter.rs:20` via the FULL path `crate::analysis::clones::is_test_file` (NOT re-exported through `analysis::mod.rs`). No new helper added; `util.rs` not touched.

### R2 — Extend `SmellsWalkerOpts`
`crates/tldr-core/src/quality/smells.rs:293`. Dropped `Copy` derive (Vec<PathBuf> is not Copy), kept `Clone`. Added `pub files: Vec<PathBuf>` and `pub include_tests: bool`. Audited callers: 3 sites — `crates/tldr-cli/src/commands/smells.rs`, `crates/tldr-core/src/quality/smells.rs:2552` (cloned now), `crates/tldr-daemon/src/handlers/quality.rs`. None used `Copy` semantics; only fix needed was a `.clone()` on the aggregated path because we read `walker_opts.include_tests` after passing it.

### R3 — Bypass walker when `--files` is non-empty
`detect_smells_with_walker_opts` now branches on `!walker_opts.files.is_empty()` — uses the explicit list (subject to existence + language + size filters), otherwise the original walker logic.

### R4 — Apply test-file filter post-flatten
After parallel `analyze_file` collection, partition by `crate::analysis::clones::is_test_file(&s.file)` when `!walker_opts.include_tests`. Excluded count populates the new `SmellsReport.excluded_test_smells` field. Mirrored at the END of `analyze_smells_aggregated_with_walker_opts` (line 2651) so deep mode also filters.

### R5 — Add `excluded_test_smells: usize` to `SmellsReport`
With `#[serde(default)]` for backward-compat with v0.2.2 daemon JSON payloads.

### R6 — Wire CLI + daemon
- `SmellsArgs` gets `--files` (repeatable Vec<PathBuf>) and `--include-tests` (bool). When `--files` is non-empty, `include_tests` is forced to true (caller picked the list).
- Each `--files` entry is validated through `tldr_core::validation::validate_file_path(f_str, Some(&project_root))`. Failures bubble up as `anyhow!` → clap-style non-zero exit (NOT silent skip).
- New helper `daemon_router::params_for_smells(path, files, include_tests)` (re-exported in `commands/mod.rs`) replaces the old `params_with_path` call in smells.rs.
- Daemon `SmellsRequest` extended with `files: Option<Vec<PathBuf>>` and `include_tests: Option<bool>` (both `#[serde(default)]`). Handler at `quality.rs:80` upgraded from bare `detect_smells()` to `detect_smells_with_walker_opts()` with a `SmellsWalkerOpts` built from the request. Safety net: handler ORs in `!request_files.is_empty()` to derive `include_tests`.

## Files Modified

| File | Change | +/- |
|------|--------|-----|
| `crates/tldr-core/src/quality/smells.rs` | `SmellsWalkerOpts` (drop Copy, add `files`/`include_tests`); `SmellsReport` (add `excluded_test_smells`); `detect_smells_with_walker_opts` file-collection branch + test-filter; aggregated path mirrors filter | source |
| `crates/tldr-cli/src/commands/smells.rs` | `SmellsArgs` (add `files`/`include_tests`); `run()` validates `--files` via `validate_file_path` and passes through `params_for_smells` | source |
| `crates/tldr-cli/src/commands/daemon_router.rs` | New helper `params_for_smells(path, files, include_tests)` | source |
| `crates/tldr-cli/src/commands/mod.rs` | Re-export `params_for_smells` | source |
| `crates/tldr-daemon/src/handlers/quality.rs` | `SmellsRequest` (add `files`/`include_tests`); handler upgraded from `detect_smells` to `detect_smells_with_walker_opts` | source |
| `crates/tldr-cli/src/output_tests.rs` | Add `excluded_test_smells: 0` to test fixture (additive struct field) | test fixture |
| `crates/tldr-cli/tests/unicode_truncation_test.rs` | Add `excluded_test_smells: 0` to test fixture (additive struct field) | test fixture |
| `crates/tldr-daemon/tests/handler_path_traversal_audit_test.rs` | Add `files: None`, `include_tests: None` to test fixture (additive request fields) | test fixture |
| `crates/tldr-cli/tests/smells_pr_focused_filter_test.rs` | NEW — 4 RED→GREEN tests | new test |

`git diff --shortstat HEAD`: 14 files changed, 494 insertions(+), 80 deletions(-) (includes the pre-existing dirty `continuum/autonomous/*` artifacts which DO NOT enter the commit).

## RED Capture

Captured from `cargo test -p tldr-cli --test smells_pr_focused_filter_test` on starting HEAD (without source changes). Symptom strings:

- `assertion 'left == right' failed: expected 1 smell, got 4 (test smells leaking into PR review signal)` — proves test-file noise leakage.
- `tldr smells --files should succeed; stderr=error: unexpected argument '--files' found` (clap rejection) — proves the flag was missing.

Full capture: `continuum/autonomous/v0.2.3-quality/reports/m1-red-capture.txt`.

## GREEN Capture

After applying GREEN edits:

```
running 4 tests
test smells_files_path_validation_blocks_system_dirs ... ok
test smells_files_filter_includes_tests_by_default ... ok
test smells_files_filter_limits_scan ... ok
test smells_default_excludes_test_files ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.59s
```

Full capture: `continuum/autonomous/v0.2.3-quality/reports/m1-green-capture.txt`.

## Validation Gate

| Gate | Result |
|------|--------|
| `cargo clippy --workspace --all-features --tests -- -D warnings` | clean |
| `cargo test -p tldr-core --lib` | 4736 passed; 0 failed |
| `cargo test -p tldr-cli --test smells_pr_focused_filter_test` | 4/4 passed |
| `cargo test -p tldr-cli --test exhaustive_matrix --features semantic --release -- --test-threads=1` | 730/730 |
| `cargo test -p tldr-cli --test language_command_matrix --features semantic --release` | 234/234 |
| Existing `smells_tests::*` (cli_quality_tests) | 17/17 ok |

Note: pre-existing failures unrelated to M1 exist in ssa_cli_tests, gvn_cli_tests, churn_tests::test_churn_help, etc. — these tests do NOT touch smells/walker/report. Verified untouched via `git diff HEAD -- <file>` (zero lines).

## STOP Conditions Checked

| # | Condition | Status |
|---|-----------|--------|
| 1 | More than 8 source files modified | OK — 5 source + 3 test fixtures = 8 |
| 2 | `SmellsReport` requires non-additive serde change | OK — `excluded_test_smells` added with `#[serde(default)]` (backward-compat) |
| 3 | Walker requires fundamental restructure | OK — added a new branch only |
| 4 | `is_test_file` matches >5% of `crates/tldr-core/src/` production files | OK — only `*_test.rs` suffix matches; production files use no test suffix |
| 5 | Cargo.lock changes outside dep additions | OK — Cargo.lock not modified |
| 6 | Prior-orchestrator artifact in commit | OK (post-commit guard verified) |

## Constraints Honoured

- No `#[allow(...)]` suppression.
- No commented-out failing assertions.
- No `_`-prefix on used variables.
- `crates/tldr-core/src/analysis/whatbreaks.rs` (M2 territory) NOT touched.
- `crates/tldr-core/src/security/taint.rs` (M3 territory) NOT touched.
- `crates/tldr-core/src/util.rs` NOT touched.
- No new `is_test_path` helper introduced; reused existing `crate::analysis::clones::is_test_file`.
- Explicit `git add <listed-files>` staging only — no `git add -A`/`.`.

## Disjointness from M2 / M3

- M2 (whatbreaks): touches `analysis/whatbreaks.rs` only. M1 does NOT touch this file.
- M3 (taint): touches `security/taint.rs` only. M1 does NOT touch this file.
- All three milestones reuse the existing `is_test_file` helper read-only.

## Commit SHA

`4e0b312b46a744d8fe10d4ebc5be2fbbc4cacec5`

# workspace-test-infrastructure-v1 — Plan

**Milestone**: workspace-test-infrastructure-v1
**Status**: PLANNING (planning loop output)
**Predecessor HEAD**: `671a984` (vuln-migration-v1 M6 supporting artifacts)
**Sibling milestone (parallel)**: vuln-source-parity-v1 (M3-CF-01 / M4-CF-01 carry-forward closure)
**Pre-publish-gate**: YES — both this milestone AND vuln-source-parity-v1 must land before external `cargo publish`
**Tag on completion**: `workspace-test-infrastructure-v1` (local only; no push)

---

## §0 Investigation summary (read first)

Live capture at HEAD `671a984` reveals **THREE critical reframes** of the milestone scope as originally chartered:

### Reframe 0.1 — There are ZERO build failures

The M6 pre-publish-binary-verification.json claim of "26 test-target build failures" is a **category mislabel**. Live verification:
- `cargo build --workspace --tests --features semantic` → exit 0; 0 errors; 0 warnings
- `cargo build --workspace --tests --all-features` → exit 0; 0 errors; 0 warnings
- `cargo test --workspace --features semantic --no-run` → exit 0; all test executables linked

The "26" matches the count of test BINARIES with at least one FAILED test result (live capture: 26 failed binaries × varying counts = 276 total runtime failures). Build is fully GREEN. Therefore: **no fix-build-failures sub-milestone is needed**.

### Reframe 0.2 — Pre-existing test failures are far more numerous than the canonical 16-name list

The `pre_existing_orthogonal_failures` list in `regex-removal-v1-plan/reports/W2-pre-report.json:60-77` enumerates 16 named failures + "(4 doctest failures)". Live capture surfaces **276 runtime test failures**. The 16-name list is a SUBSET of the failures that prior milestone workers happened to encounter and document.

### Reframe 0.3 — The bulk of failures (~200/276) are tests for archived subcommands

`ls crates/tldr-cli/src/commands/archived/` confirms 17 subcommands have been moved to the archive directory (`cfg`, `dfg`, `ssa`, `alias`, `dominators`, `live_vars`, `abstract_interp`, `arch`, `behavioral`, `bounds`, `diff_impact`, `equivalence`, `maintainability`, `mutability`, `purity`, `search`, `secrets`). The `Subcommand` enum in `crates/tldr-cli/src/main.rs` has NO variants for these — confirmed via grep. But the test files (`cli_graph_tests.rs`, `cli_remaining_tests.rs`, `cli_quality_tests.rs`, `ssa_cli_tests.rs`, `gvn_cli_tests.rs`, `p2_multilang_tests.rs`, `exhaustive_matrix.rs`, etc.) still invoke `tldr cfg ...`, `tldr dfg ...`, `tldr ssa ...` etc. Every such invocation returns clap-unknown-subcommand and the test panics on `assert!(output.status.success())`.

**Disambiguation note (per premortem ELEPHANT-1)**: of the 17 archived modules, **only 16 are Cat-A deletion targets**. The `search` module is presence-only — at `crates/tldr-cli/src/main.rs:141-142`, the active `SmartSearch(SmartSearchArgs)` enum variant is renamed via `#[command(name = "search")]` so that `tldr search ...` invocations route to **active** SmartSearch, NOT to anything in `archived/search.rs`. Tests invoking `args(["search", ...])` are therefore ACTIVE and PRESERVED. M1 enumeration MUST exclude `search` from the archived candidate set; the dispatch contract carries a `m1_classification_by_clap_name` validator mandate to enforce this. The 16 actual Cat-A deletion targets are: `cfg`, `dfg`, `ssa`, `alias`, `dominators`, `live_vars`, `abstract_interp`, `arch`, `behavioral`, `bounds`, `diff_impact`, `equivalence`, `maintainability`, `mutability`, `purity`, `secrets`.

This is a clean DELETE bucket: tests targeting functionality that no longer exists in the binary. Per the no-gaming rule (CLAUDE.md global): deletion of tests is justified ONLY when the target functionality has been intentionally removed AND modern equivalents are covered by other active tests. Both conditions hold here (archived deliberately in prior milestones; modern equivalents like `taint`, `slice`, `whatbreaks`, `references` have their own active test coverage).

### Failure breakdown at HEAD `671a984`

| Category | Count | In-scope here? | Disposition |
|----------|-------|----------------|-------------|
| Build failures | 0 | n/a | n/a |
| Doctest failures (4 named in M6 report) | 4 | YES | FIX (4 doctests) |
| Cat-A: Archived-cmd test invocations | ~200 | YES | DELETE |
| Cat-B: vuln_migration_v1_red `<lang>_<vuln_type>_positive` (M3-CF-01/M4-CF-01) | 33 | **NO** | OUT-OF-SCOPE — vuln-source-parity-v1 owns these |
| Cat-C: Orthogonal-real failures (W2-pre canonical 15 + 4-10 newly-observed) | ~16-25 | YES | FIX or DELETE-on-stale |
| **TOTAL in-scope** | **~220-230** | | |

Detailed enumeration: `reports/investigation.json` §investigation_summary.runtime_test_failures.categorization.

---

## §1 Bundle scope

In-scope:
- **F-1**: 4 doctest fixes (kotlin, luau, FuncIndexProxy, surface::triggers).
- **F-2**: Bulk DELETE of all test functions targeting archived subcommands (~200 functions across ~10-12 test files).
- **F-3**: Targeted FIX of Category-C orthogonal-real failures (~16-25, including the 15 from canonical W2-pre and the 4-10 newly-observed).
- **F-4**: CHANGELOG entry + local tag.

Out-of-scope:
- Category-B `vuln_migration_v1_red` `<lang>_<vuln_type>_positive` failures (33 — owned by vuln-source-parity-v1).
- Any source code changes that aren't test-fixture or test-correctness driven.
- Adding new test coverage (this is a HYGIENE milestone, not a feature one).
- External `cargo publish` (pre-publish operator owns that step).
- Version bumps; `Cargo.lock` staging.

---

## §2 Sub-milestone list

This milestone has **5 sub-milestones**. All are non-atomic except **M3** (atomic; mirrors vuln-migration-v1 M5 ATOMIC pattern).

### M1 — RED capture + authoritative enumeration

**Depends**: nothing (HEAD `671a984` is the start)
**Atomic**: false
**LOC delta**: +30 (artifact JSON only, no source changes)

**Red tests**: capture full failure picture at HEAD via `cargo test --workspace --features semantic --no-fail-fast --release 2>&1 | tee reports/M1-red-capture.txt`.

**Green files**: none — purely documentary.

**Additional artifacts** (GATING for M2-M4):
- `reports/M1-red-capture.txt` — full cargo test output at HEAD.
- `reports/M1-doctest-red-capture.txt` — `cargo test --workspace --features semantic --doc --no-fail-fast` output.
- `reports/M1-archived-cmd-test-enumeration.json` — authoritative list of every archived-cmd test function by file + line. Schema: `{file: <path>, line: <usize>, fn_name: <str>, archived_subcommand: <str>}`. Constructed by walking each `cli_*_tests.rs` and `*_cli_tests.rs` file, identifying every `Command::new(assert_cmd::cargo::cargo_bin!("tldr")).args([\"<archived>\"...])` invocation, where `<archived>` is in the **16-element** Cat-A set: `{cfg, dfg, ssa, gvn, alias, dominators, live_vars, abstract_interp, arch, behavioral, bounds, diff_impact, equivalence, maintainability, mutability, purity, secrets}` (note: `search` is EXCLUDED — it is the ACTIVE SmartSearch CLI alias per `crates/tldr-cli/src/main.rs:141-142`). Estimated 218-225 entries (premortem ELEPHANT-2 — supersedes the legacy ~200 narrative). The M1 enumeration JSON is the binding count; do NOT panic if the count exceeds 200.
- `reports/M1-orthogonal-real-failures.json` — authoritative list of Category-C failures. Each entry: `{test: <name>, file: <path>, line: <usize>, root_cause: <str>, disposition: FIX|DELETE, fix_sketch: <str>}`. Estimated 16-25 entries.
- `reports/M1-out-of-scope-confirmation.json` — list of all 33 `vuln_migration_v1_red` Category-B failures, EXPLICITLY excluded from this milestone's scope; cross-reference to vuln-source-parity-v1.

**Stop thresholds**:
- All four artifacts committed.
- Sum of Cat-A + Cat-C + 4 (doctests) + Cat-B = 276 (live capture total). Numerical reconciliation must match within +/- 5.
- M1's authoritative count from `M1-archived-cmd-test-enumeration.json` SUPERSEDES any narrative estimate (per validator mandate `m1_enumeration_authoritative_over_plan_estimate`). The +/- 5 tolerance applies only between M1 final enumeration and M3's actual delete count, **not** between the plan's narrative ~200 and the M1 enumeration. Workers must not flag a M1 count of 218-225 as "drift".
- Spot-check (per validator mandate `m1_classification_by_clap_name`): grep `M1-archived-cmd-test-enumeration.json` for `"archived_subcommand": "search"` — any match is a M1 BLOCKER (re-enumerate; `search` is the active SmartSearch alias).
- `cargo build --workspace --tests --features semantic` exit 0 (re-confirm clean build).

### M2 — Doctest fixes

**Depends**: M1
**Atomic**: false
**LOC delta**: +/-15

**Red tests**: 4 doctest failures from M1-doctest-red-capture.txt.

**Green files**:
| Path | Anchor | Edit |
|------|--------|------|
| `crates/tldr-core/src/callgraph/cross_file_types.rs` | doctest at L1025-L1037 | Rewrite example to use `FuncIndexProxyMut` (the working impl at L1147) instead of the unimplemented! stub `FuncIndexProxy`. Per no-gaming rule: provide a working example, not just `ignore`. |
| `crates/tldr-core/src/callgraph/languages/kotlin.rs` | doc-comment fence at L74 | Change bare ` ``` ` to ` ```text ` (preserves readability of pseudo-grammar block). |
| `crates/tldr-core/src/callgraph/languages/luau.rs` | doc-comment fence at L217 | Same as kotlin: bare ` ``` ` → ` ```text `. |
| `crates/tldr-core/src/surface/triggers.rs` | doctest at L15-L21 | Change `use tldr_core::contracts::triggers::extract_name_triggers;` → `use tldr_core::surface::triggers::extract_name_triggers;`. The `contracts/` and `surface/` modules are siblings; the function lives in `surface/`. |

**Stop thresholds**:
- `cargo test --workspace --features semantic --doc --no-fail-fast` → 0 failed.
- `cargo build --workspace --tests --features semantic` exit 0 (no regression).

### M3 — ATOMIC: Bulk DELETE of archived-subcommand tests

**Depends**: M1
**Atomic**: true (mirrors vuln-migration-v1 M5 atomic pattern)
**release_commit_group**: `milestone_3_atomic`
**LOC delta**: -3500 estimate (200 functions × ~17 LOC avg)

**Rationale for atomicity**: deleting some but not all archived-cmd tests will leave compile-clean but test-runtime-failing intermediate state, plus partial deletions in some files but not others trigger spurious diff churn. Single-commit delete keeps the tree consistent.

**Red tests** (compile-gate only): `cargo build --workspace --tests --features semantic` after deletion (must remain exit 0; no orphaned helper imports).

**Green files** (per M1-archived-cmd-test-enumeration.json — list authoritative; per validator mandate `m1_enumeration_authoritative_over_plan_estimate`, M3 actual delete count MUST match the M1 enumeration line-for-line):
- `crates/tldr-cli/tests/cli_graph_tests.rs` — DELETE all `test_cfg_*`, `test_dfg_*`, `test_ssa_*` test functions (~16-25 functions).
- `crates/tldr-cli/tests/cli_remaining_tests.rs` — DELETE archived subset (alias, dominators, live_vars, arch tests). PRESERVE active subset (taint, slice, whatbreaks, hubs, references, deps, inheritance, clones, dice, daemon-related).
- `crates/tldr-cli/tests/cli_quality_tests.rs` — DELETE archived (maintainability, mutability, purity, secrets); PRESERVE active.
- `crates/tldr-cli/tests/cli_search_context_tests.rs` — **PRESERVE entire file** (per validator mandate `m1_classification_by_clap_name`). Every `args(["search", ...])` invocation routes to ACTIVE SmartSearch via `#[command(name = "search")]` at `crates/tldr-cli/src/main.rs:141-142`. The `search` token is NOT in the 16-element Cat-A set.
- `crates/tldr-cli/tests/cli_tests.rs` — DELETE archived subset; PRESERVE active. Active includes every `args(["search", ...])` SmartSearch invocation (do NOT confuse with archived/search.rs).
- `crates/tldr-cli/tests/ssa_cli_tests.rs` — DELETE entire file (all 26 tests target archived `ssa`; whole-file `git rm` permitted — this file is purely-archived).
- `crates/tldr-cli/tests/gvn_cli_tests.rs` — DELETE entire file (gvn is archived-equivalent — not in main.rs Subcommand enum; whole-file `git rm` permitted).
- `crates/tldr-cli/tests/p2_multilang_tests.rs` — **PER-TEST DELETE ONLY** (per validator mandate `m3_no_whole_file_delete_for_mixed_files`). 45 tests: ~37 archived-cmd (Cat-A delete) vs ~8 active (preserve). Whole-file `git rm` is FORBIDDEN. Use surgical `fast_edit` per test function.
- `crates/tldr-cli/tests/exhaustive_matrix.rs` — **PER-TEST DELETE ONLY** (per validator mandate `m3_no_whole_file_delete_for_mixed_files`). 729+ tests: 53 archived-cmd (Cat-A delete) vs the rest active (preserve). Whole-file `git rm` is FORBIDDEN. Use surgical `fast_edit` per test function.
- `crates/tldr-cli/tests/cli_patterns_contracts_tests.rs` — DELETE archived subset; PRESERVE active.
- `crates/tldr-cli/src/commands/bugbot/text_format.rs` — DELETE 1 unit test `test_text_format_generic_evidence_shows_numbers` if M1 classifies it as Cat-A (numeric drift on archived-cmd evidence) or as Cat-C (real fixable bug).

**Preserved**: ALL test functions that target ACTIVE subcommands (visible in `crates/tldr-cli/src/main.rs` Subcommand enum: `Tree`, `Structure`, `Calls`, `Impact`, `Dead`, `Hubs`, `Whatbreaks`, `Slice`, `Chop`, `Taint`, `Resources`, `Vuln`, `ApiCheck`, `Patterns`, `Inheritance`, `Deps`, `Cohesion`, `Coupling`, `Contracts`, `Specs`, `Invariants`, `Verify`, `Interface`, `Diagnostics`, `Doctor`, `ChangeImpact`, `Coverage`, `Search`, `Semantic`, `Similar`, `Context`, `Definition`, `References`, `Explain`, `Todo`, `Diff`, `Embed`, `Daemon*`, `Warm`, `Cache*`, `Loc`, `Complexity`, `Cognitive`, `Halstead`, `Churn`, `Debt`, `Health`, `Hotspots`, `Clones`, `Dice`, `Smells`, `Imports`, `Importers`, `Extract`, `Temporal`, `ReachingDefs`, `Available`, `DeadStores`).

**Stop thresholds**:
- M1-archived-cmd-test-enumeration.json fully drained (every entry deleted).
- `cargo build --workspace --tests --features semantic` exit 0 (no orphaned helper imports; `cargo` itself surfaces unused imports / dead helpers as warnings — all must be silenced via deletion of helpers if they were archived-cmd-test-only).
- `cargo clippy --workspace --tests --features semantic -- -D warnings` exit 0.
- `cargo test --workspace --features semantic --no-fail-fast --release` failure count drops by ~200 (Cat-A failures eliminated; remaining failures = Cat-B + Cat-C + 4 doctests + 0 = ~33 + 16-25 + 4 = ~53-62 expected).

**Rollback rule**: If post-commit verification surfaces compile errors or clippy warnings, REVERT entire M3 commit, re-run M1 enumeration to identify the missed dependency, then re-stage. **Enumeration authority** (per validator mandate `m1_enumeration_authoritative_over_plan_estimate`): M3's actual delete set MUST match `M1-archived-cmd-test-enumeration.json` line-for-line. Any mismatch (under-delete or over-delete) is a M3 stop-threshold violation, regardless of whether the M1 enumeration count diverges from the plan-narrative ~200 estimate. The +/- 5 tolerance applies only between M1 final enumeration and M3 actual delete count, NOT against the legacy narrative estimate.

### M4 — Targeted Category-C orthogonal-real fixes

**Depends**: M3
**Atomic**: false
**LOC delta**: +/-200 (mix of fixture creation, parser logic, error-path hardening)

**Red tests**: M1-orthogonal-real-failures.json entries (Category-C subset — ~16-25 tests).

**Green files** (initial sketch — M1 RED capture is the authoritative input):

| Path | Edit | Disposition |
|------|------|-------------|
| `crates/tldr-core/tests/fixtures/empty-dir/.gitkeep` | Create empty file (so the directory exists post-checkout) | FIX (`tree_handles_empty_directory`) |
| `crates/tldr-core/tests/bench_surface_search_multilang.rs:876-893` | Change `Some("ruby")` → `Some("haskell")` (or another genuinely-unsupported language; verify against `crates/tldr-core/src/surface/mod.rs:90-118` match arm) | FIX (`test_surface_unsupported_language_errors`) |
| `crates/tldr-core/src/quality/churn.rs` (or wherever `git_log` lives) | Make `git_log` swallow the no-commits-yet exit-128 stderr and return Ok(String::new()) on empty repo | FIX (`test_git_log_empty_repo`, `test_git_log_numstat_empty_repo`) |
| `crates/tldr-core/tests/language_parity_test.rs:1925-1948` | DELETE `test_kotlin_returns_unsupported` and `test_swift_returns_unsupported` test functions (both languages are now supported per `parser.rs:136-137`); the file's own preserved comment at L1922-23 confirms deletion was planned. Replacement parse-success tests already exist at `parser.rs:426-432`. | DELETE-on-stale |
| `crates/tldr-core/src/metrics/cognitive.rs` (or wherever `analyze_cognitive_source` lives) | Audit else-clause handling — per SonarQube spec, else does NOT add to cognitive complexity. Align analyzer with spec. | FIX (`test_cognitive_else_not_counted`) |
| `crates/tldr-core/src/quality/dead_code.rs` (or wherever `analyze_dead_code` lives) | On empty input directory, return `Ok(<empty report>)` instead of `Err`. Vacuous-truth handling. | FIX (`test_analyze_dead_code_empty`) |
| `crates/tldr-core/src/quality/martin.rs` (or wherever `compute_martin_metrics` lives) | Same shape as analyze_dead_code: empty input → Ok with empty report | FIX (`test_compute_martin_metrics_empty`) |
| `crates/tldr-core/src/quality/coverage.rs` (or wherever `parse_coverage` lives) | (a) Empty input → Ok with empty report; (b) audit cobertura/lcov parser logic against fixture (`coverage.xml` filename + `CoverageFormat::Cobertura` hint, etc.) | FIX (`test_parse_coverage_empty`, `test_parse_coverage_cobertura`, `test_parse_coverage_lcov`) |
| `crates/tldr-core/tests/quality_tests.rs:571-598` | Make the two test fixture functions structurally distinct enough that they fall below 0.8 similarity threshold. NOT lower the threshold (per no-gaming rule). | FIX (`test_find_similar_no_clones`) |
| `crates/tldr-core/src/analysis/change_impact.rs` (or wherever NoBaseline error is constructed) | When only-remote-tracking-ref-exists case detected, include `origin/<branch>` substring in the NoBaseline error reason | FIX (`detection_error_hints_at_origin_branch_when_only_remote_exists`) |
| `crates/tldr-core/tests/fs_tests.rs:468-499` (or related code) | M1 RED-capture analysis required to identify exact root cause | FIX TBD (`test_realistic_python_project_structure`) |
| (Newly-identified) `crates/tldr-cli/tests/cli_remaining_tests.rs:1093-...` (`test_change_impact_*`) | Investigate: does `change-impact` require a real git repo in fixture? Likely test fixture needs `git init` + at least one commit. Add fixture-setup helper. | FIX (`test_change_impact_basic` and siblings) |
| (Newly-identified) `crates/tldr-cli/src/commands/bugbot/text_format.rs:2356` (`test_text_format_generic_evidence_shows_numbers`) | M1 RED capture must classify; numeric drift in expected output | FIX or DELETE TBD |
| (Newly-identified) `crates/tldr-cli/tests/val013_daemon_status_cross_cwd_test.rs` (`daemon_status_from_other_cwd_reports_running`) | Investigate; may be order-dependent — if so, fix the daemon status check; if cleanly-flaky, IGNORE is acceptable per no-gaming rule (only environmental dependencies qualify) | FIX TBD |

**Stop thresholds**:
- All M1-orthogonal-real-failures.json entries → GREEN.
- `cargo test --workspace --features semantic --no-fail-fast --release` failure count = 33 (Category-B vuln-source-parity-v1 carry-forwards only; everything else GREEN).
- `cargo clippy --workspace --tests --features semantic -- -D warnings` exit 0.
- `cargo test --workspace --features semantic --doc --no-fail-fast` → 0 failed (M2 doctest fix preserved).

### M5 — CHANGELOG entry + local tag

**Depends**: M4
**Atomic**: false
**LOC delta**: +30

**Red tests**: none (documentation-only).

**Green files**:
- `CHANGELOG.md` — entry per regex-removal-v1 / sanitizer-removal-v1 / vuln-migration-v1 precedent. See §6 below for draft.
- `continuum/autonomous/workspace-test-infrastructure-v1-plan/reports/M5-release-prep.md` — final summary including LOC delta, archived-cmd-test count actually deleted, orthogonal-real-failure count actually fixed.

**Tag**: `workspace-test-infrastructure-v1` (annotated, local only, NO push).

**Stop thresholds**:
- CHANGELOG entry merged.
- Local annotated tag applied.
- NO push, NO publish, NO version bump.
- `reports/M5-release-prep.md` committed.

---

## §3 Per-failure-category risk

| Category | Risk | Mitigation |
|----------|------|------------|
| Cat-A (archived-cmd test deletion) | Bulk delete may inadvertently remove a test for an ACTIVE subcommand if M1 enumeration mis-classifies | M1 enumeration cross-checks every test function against the active Subcommand variants in main.rs. Active variants are SOURCE-OF-TRUTH; deletions only happen for invocations of cmds NOT in that enum. |
| Cat-A `search`-vs-SmartSearch disambiguation | The 17 archived modules in `crates/tldr-cli/src/commands/archived/` include `search.rs`, but `tldr search ...` is the ACTIVE CLI alias for SmartSearch via `#[command(name = "search")]` at `crates/tldr-cli/src/main.rs:141-142`. A literal-reading worker would mis-classify `args(["search", ...])` invocations as Cat-A and delete active SmartSearch coverage. | Validator mandate `m1_classification_by_clap_name`: classification by CLI-name-after-clap-rename, NOT module presence in archived/. The 16-element Cat-A set EXCLUDES `search`. M1 spot-check: any `archived_subcommand: "search"` entry is a BLOCKER. Plan §M3 explicitly marks `cli_search_context_tests.rs` as PRESERVE entire file. |
| Cat-A intra-file mixed deletions (`p2_multilang_tests.rs`, `exhaustive_matrix.rs`) | These two files contain a mix of archived-cmd tests (Cat-A delete) and active-cmd tests (preserve). `p2_multilang_tests.rs` has 45 tests / 37 fail; `exhaustive_matrix.rs` has 729+ tests / 53 fail. A worker doing a whole-file `git rm` would delete the active subset and silently lose coverage. | Validator mandate `m3_no_whole_file_delete_for_mixed_files`: PER-TEST DELETE ONLY for mixed files. Whole-file `git rm` is FORBIDDEN. Surgical `fast_edit` per test function. Whole-file delete remains permitted only for purely-archived files (`ssa_cli_tests.rs`, `gvn_cli_tests.rs`). |
| Cat-A (helper-import orphans post-deletion) | A test-only helper used by both archived and active tests may end up unused if we don't track helpers | Post-M3, `cargo build --workspace --tests` exit 0 + `cargo clippy -D warnings` catches every orphan. M3 stop-threshold gates on this. |
| Cat-A (atomic deletion failure) | Partial deletion across multiple files leaves intermediate state in compile-fail | M3 marked atomic_commit:true. All archived-cmd test deletions go in ONE commit. Mirror vuln-migration-v1 M5. |
| Cat-C (FIX cascades) | Fixing one test may break another (e.g., changing `git_log` empty-repo behavior may break a positive-case test elsewhere) | Each Cat-C fix verified individually — `cargo test --workspace --features semantic --no-fail-fast --release` after each Cat-C fix; expected delta = -1 per fix. |
| Cat-C (root cause uncertainty) | Some hypotheses (e.g., test_realistic_python_project_structure cause) are inferred without live verification | M1 RED capture is the GATE — actual stdout / stderr from each failing test must be analyzed before fix work begins. |
| Doctest M2 (regression) | Changing kotlin/luau fence to `text` may render doc differently | Visual inspection in M2: rendered doc output preserved; the fence semantic just changes from "compile-as-rust" to "render-as-text". |
| CHANGELOG drift (M5) | Wrong claims in CHANGELOG (e.g., undercount of deletions) | M5 sources counts FROM the actual M3 commit's `git diff --stat` — no manual estimation. |

---

## §4 Test fixtures

New fixtures required:
- `crates/tldr-core/tests/fixtures/empty-dir/.gitkeep` — empty file so the directory survives `git checkout`. (Fixes `tree_handles_empty_directory`.)
- (Possibly, M1 RED dependent) Updated test fixtures for `test_change_impact_*` family that include a real `git init` + commit step in the fixture-setup helper. Should be inline test-helper code rather than disk fixtures.

No fixture deletions needed (deletions only occur on test functions, not their on-disk inputs).

---

## §5 Decision matrix for genuinely-flaky tests

Per the no-gaming rule (`/Users/cosimo/.claude/CLAUDE.md` global): prefer FIX. Use IGNORE/DELETE only when:
- DELETE: target functionality has been intentionally archived AND modern equivalents exist in the active codebase (Kotlin/Swift unsupported tests; archived-cmd test bulk).
- IGNORE: failure is genuinely environmental (real network, real external infrastructure). **NONE qualify.**

| Test | Disposition | Rationale |
|------|-------------|-----------|
| `tree_tests::tree_handles_empty_directory` | FIX | Fixture gap; create `.gitkeep`. Deterministic local test. |
| `test_surface_unsupported_language_errors` | FIX | Stale Ruby premise; switch lang. Deterministic local test. |
| `test_realistic_python_project_structure` | FIX | Numeric/semantic drift; M1 RED capture identifies. |
| `test_git_log_empty_repo` | FIX | Self-init tempdir git repo; harden `git_log`. **NO** real git server needed — DO NOT IGNORE. |
| `test_git_log_numstat_empty_repo` | FIX | Same as above. |
| `p2_language_status_tests::test_kotlin_returns_unsupported` | DELETE | Genuine obsolescence; preserved comment confirms; replacement parse-success test exists. |
| `p2_language_status_tests::test_swift_returns_unsupported` | DELETE | Same as Kotlin sibling. |
| `test_cognitive_else_not_counted` | FIX | Real bug; align analyzer with SonarQube spec. |
| `test_analyze_dead_code_empty` | FIX | Behavior-contract drift; empty input → graceful Ok. |
| `test_compute_martin_metrics_empty` | FIX | Same shape. |
| `test_parse_coverage_empty` | FIX | Same shape. |
| `test_parse_coverage_cobertura` | FIX | Parser regression OR fixture-naming alignment. |
| `test_parse_coverage_lcov` | FIX | Same shape. |
| `test_find_similar_no_clones` | FIX | Make fixture functions structurally distinct (do NOT lower threshold). |
| `detection_error_hints_at_origin_branch_when_only_remote_exists` | FIX | Self-init tempdir + `git update-ref`; **NO** real remote needed. Real UX bug. |
| (Doctests × 4) | FIX | Per §M2. |
| (Cat-A archived-cmd tests × ~200) | DELETE | Genuine obsolescence; archived in prior milestones; modern equivalents exist. |
| (Cat-B `vuln_migration_v1_red` × 33) | OUT-OF-SCOPE | vuln-source-parity-v1 sibling milestone owns these. NOT touched here. |

**Total IGNORE count: 0.** Every in-scope failure is either FIX or DELETE-on-genuine-obsolescence.

---

## §6 CHANGELOG draft

```markdown
## workspace-test-infrastructure-v1 — internal milestone

### Removed: ~200 obsolete CLI integration tests for subcommands archived in prior milestones (`cfg`, `dfg`, `ssa`, `gvn`, `alias`, `dominators`, `live_vars`, `abstract_interp`, `arch`, `behavioral`, `bounds`, `diff_impact`, `equivalence`, `maintainability`, `mutability`, `purity`, `secrets` — already moved to `crates/tldr-cli/src/commands/archived/` in earlier internal milestones; their CLI test invocations had been left dangling). Modern equivalents (`taint`, `slice`, `whatbreaks`, `references`, `dead`, `hubs`, etc.) retain full active test coverage.

### Removed: 2 obsolete `test_*_returns_unsupported` tests for Kotlin and Swift in `crates/tldr-core/tests/language_parity_test.rs`. Both languages are now SUPPORTED via `tree_sitter_kotlin_ng` and `tree_sitter_swift` (per `crates/tldr-core/src/ast/parser.rs:136-137`); replacement parse-success tests already exist (`parser.rs:420-432`). The preserved comment at L1922-23 confirmed deletion was planned.

### Fixed: 4 doctest failures in `tldr-core` — `callgraph::cross_file_types::FuncIndexProxy` (rewritten to use `FuncIndexProxyMut` working impl), `callgraph::languages::kotlin::KotlinHandler::parse_import_node` and `callgraph::languages::luau::LuauHandler::extract_aliased_require` (bare ` ``` ` → ` ```text ` fence), `surface::triggers::extract_name_triggers` (stale `tldr_core::contracts::triggers::...` import path → `tldr_core::surface::triggers::...`).

### Fixed: ~16 orthogonal-real test failures across `tldr-core` — empty-directory tree handling fixture gap (`tree_handles_empty_directory`), stale Ruby-unsupported assertion (`test_surface_unsupported_language_errors`), git-log on no-commits-yet repo (`test_git_log_empty_repo` + `test_git_log_numstat_empty_repo`), cognitive-complexity else-not-counted SonarQube-spec alignment (`test_cognitive_else_not_counted`), empty-input handling for `analyze_dead_code` / `compute_martin_metrics` / `parse_coverage` (Ok with empty report instead of Err), cobertura/lcov coverage parser regression (`test_parse_coverage_cobertura`, `test_parse_coverage_lcov`), similarity-threshold fixture distinctness (`test_find_similar_no_clones`), `change-impact` only-remote-exists hint substring in NoBaseline error reason (`detection_error_hints_at_origin_branch_when_only_remote_exists`), and `change-impact` CLI test fixture git-repo setup (`test_change_impact_basic` + siblings).

### Retained: ALL active-subcommand CLI integration tests; ALL `test_e2e_*` vuln tests; ALL daemon-related tests; ALL semantic / fastembed / embedding tests; ALL non-archived `tldr-core` library tests.

### Test infrastructure baseline restored: After this milestone + `vuln-source-parity-v1` (sibling), `cargo test --workspace --features semantic --no-fail-fast --release` is fully GREEN (modulo any newly-introduced regressions). The pre-publish baseline is restored.

### Architectural note: This is a HYGIENE milestone — no new features, no new test coverage, no public API changes. The single coherent external `cargo publish` (closing #7, #23, #24, #27, #28 + tldr vuln FP class + sanitizer correctness) is gated on this milestone + `vuln-source-parity-v1` both landing.
```

---

## §7 Premortem / risk register

| ID | Risk | Severity | Mitigation |
|----|------|----------|------------|
| R1 | Cat-A enumeration mis-classifies an active-cmd test as archived → deleted by mistake | HIGH if not careful | M1 enumeration cross-checks each test against `crates/tldr-cli/src/main.rs` Subcommand enum (source of truth); active-cmd tests preserved unconditionally. |
| R2 | M3 atomic deletion leaves orphan helper imports → compile fail | MEDIUM | M3 stop-threshold gates on `cargo build --workspace --tests` + `cargo clippy -D warnings` exit 0. Rollback rule documented. |
| R3 | A "fix" in M4 (e.g., `git_log` empty-repo behavior change) breaks a passing test elsewhere | MEDIUM | Per-fix verification: `cargo test` after each fix; expected delta = -1; if more fail, revert and re-investigate. |
| R4 | Cat-C root cause turns out to be a regression introduced by one of the prior 4 internal milestones (regex-removal, FAI, sanitizer, vuln) | HIGH if surfaces | M1 RED capture must read each failing test's expected vs actual output. If a regression-from-prior-milestone is identified, ESCALATE to operator before fixing — may require revert of a prior commit (much bigger scope). |
| R5 | M5 CHANGELOG numbers diverge from actual M3 + M4 git diff stats | LOW | M5 sources counts from `git diff --stat HEAD~N HEAD --` for each milestone-commit boundary. No manual estimation. |
| R6 | A test the milestone DELETES (Cat-A or stale-Kotlin/Swift) is actually meaningful and should have been migrated, not deleted | MEDIUM | Per no-gaming rule: deletion requires (a) target functionality archived/removed AND (b) modern equivalent exists. Both conditions must be EXPLICITLY documented per deleted test in M3 commit message — operator can spot-audit. |
| R7 | Cargo.lock or sibling-milestone files accidentally staged | LOW | Plan §10 staging_method explicit add only — only files under `continuum/autonomous/workspace-test-infrastructure-v1-plan/` and the surgical source/test edits per milestone. `git checkout HEAD -- Cargo.lock` before each commit if dirty. |

---

## §8 Self-validation — validator_mandates baked into the dispatch contract

The dispatch contract (`dispatch-contract.json`) carries these binding mandates:

- `no_test_gaming_rule` — every disposition is FIX or DELETE-on-genuine-obsolescence; ZERO `#[ignore]` markers; ZERO weakened assertions.
- `m1_enumeration_authoritative` — M1 produces `M1-archived-cmd-test-enumeration.json` and `M1-orthogonal-real-failures.json`. M3 + M4 source from these artifacts. Numerical reconciliation gate: Cat-A + Cat-B + Cat-C + 4 doctests = 276 total at M1.
- `m3_atomic_deletion_required` — M3 carries `atomic_commit:true` and `release_commit_group:milestone_3_atomic`. Without atomicity, intermediate-state compile failures.
- `m3_active_subcmd_preservation_mandatory` — M3 source-of-truth for "ACTIVE subcommand" is the `Subcommand` enum in `crates/tldr-cli/src/main.rs`. Any test invoking a cmd in that enum is PRESERVED unconditionally. Any test invoking a cmd NOT in that enum is DELETED.
- `category_b_out_of_scope` — vuln_migration_v1_red's 33 `<lang>_<vuln_type>_positive` failures are EXCLUDED. The vuln-source-parity-v1 sibling milestone owns them. Plan §1.OUT-OF-SCOPE.
- `cli_output_shape_unchanged` — no source-code edits beyond test files, doctests, the helpers explicitly named in M4 (cognitive analyzer, parse_coverage, analyze_dead_code, compute_martin_metrics, git_log empty-repo handling, change-impact NoBaseline hint). NO public API changes.
- `pre_publish_baseline_restored` — M4 stop-threshold: `cargo test --workspace --features semantic --no-fail-fast --release` returns failure-count = 33 (the vuln-source-parity-v1 carry-forwards) and ZERO additional failures.
- `no_test_deletion_silently` — every M3 + M4 deletion documented in the commit message body with file + function name + rationale. Operator audit-friendly.
- `cargo_lock_never_staged` — `git checkout HEAD -- Cargo.lock` before each commit if dirty.
- `staging_method_explicit_add_only` — only files under `continuum/autonomous/workspace-test-infrastructure-v1-plan/` + the surgical source/test edits.
- `no_push_no_publish` — local commits + local tag only.

---

## §9 /autonomous-readiness assessment

**Verdict**: READY for /autonomous loop consumption, with the following conditions:

1. **M1 RED-capture is GATING**: M3 + M4 must NOT begin until M1 produces both authoritative enumeration JSONs. The numerical reconciliation gate (Cat-A + Cat-B + Cat-C + 4 = 276) must hold.
2. **Operator-confirmation point at M4 R4 escalation**: if M1 RED capture identifies a Cat-C failure that turns out to be a regression-from-prior-milestone (per R4), the worker must STOP and escalate before fixing — may require revert of a prior commit (much bigger scope; out of this milestone's bounds).
3. **vuln-source-parity-v1 sibling milestone progress is independent**: this milestone can land BEFORE, AFTER, or IN PARALLEL with vuln-source-parity-v1. Both must land before external publish.
4. **No external infrastructure dependencies**: every fix is local, self-contained, deterministic. Worker can run M2-M5 entirely on the dev box.
5. **Atomicity discipline verified**: M3 carries the same atomic_commit pattern that vuln-migration-v1 M5 used successfully. No new infrastructure required.

The plan reduces the apparent scope (originally chartered as 26 build failures + 4 doctests + ~16 runtime tests = 46 issues) to a more realistic profile: 0 build failures, 4 doctests, ~16-25 orthogonal-real fixes, ~200 obsolete-test deletions, 33 out-of-scope. Net LOC delta is approximately -3300 (massive deletion bias driven by archived-cmd cleanup) — the milestone is mostly cleanup, partly real bug fixes, all within the no-gaming rule.

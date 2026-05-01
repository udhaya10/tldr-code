# rust-panic-suppression-v1 — Plan

**Type:** Hardening milestone (production UX). Closes ZERO existing RED tests; improves JSON noise floor on production Rust codebases.

**Origin:** rust-vuln-taint-pipeline-v1 R2 sub-elephant — production Rust codebases flood `tldr vuln` JSON output with per-`.unwrap()` Panic findings; the existing `is_rust_test_file` mask only covers `/tests/`, `_test.rs`, `tests.rs` paths.

**HEAD baseline:** `7a36df3` (post-rust-vuln-taint-pipeline-v1; 167/168 vuln_migration_v1_red GREEN).

---

## §0 Investigation

Verified facts (full audit at `reports/investigation.json`):

- `analyze_rust_file` (vuln.rs:494-675) emits **7 trigger categories** in order:
  1. T1 `UnsafeCode` (CWE-242, High) — `unsafe {` without nearby SAFETY comment.
  2. T2 `MemorySafety` (CWE-119, Critical) — `mem::transmute(`.
  3. T3 `MemorySafety` (CWE-119, High) — `std::ptr::` / `core::ptr::` / `ptr::read(` / `ptr::write(`.
  4. **T4 `Panic` (CWE-703, Medium) — `.unwrap()` (smell; the trigger this milestone gates).**
  5. T5 `SqlInjection` (CWE-89, Critical) — `format!(...SQL...)` interpolation.
  6. T6 `MemorySafety` (CWE-20, High) — `from_utf8_unchecked(` / `as_bytes()[` / `as_bytes().get_unchecked(`.
  7. T7 `CommandInjection` (CWE-78, Critical) — `Command::new( ... .arg(` non-literal.

- `is_rust_test_file` (vuln.rs:724-730) is consulted ONLY by T4 (Panic). The other 6 triggers emit unconditionally regardless of test-file status.

- `VulnArgs` struct (vuln.rs:73-101) has 7 fields; closest precedent for the new flag is `include_informational: bool` (line 92, default false). Filter pipeline applies it at vuln.rs:194-196.

- 9 `#[test]` fns in `commands::remaining::vuln::tests` module. Of the 6 prefix-matching `test_analyze_rust_*`, only 1 (`test_analyze_rust_detects_unwrap_in_non_test_code`, L1057) asserts Panic findings. The other 5 are unaffected.

- `analyze_rust_file` is private (non-pub) — no external consumers; signature stability is not an API concern, but it IS a test-churn concern.

---

## §1 Bundle scope

Add a single CLI bool flag `--include-smells` (default `false`) to `VulnArgs`. When omitted, suppress only `Panic` findings emitted by `analyze_rust_file`'s line scanner. When passed, restore the current emission behavior.

UnsafeCode, MemorySafety×3, SqlInjection, CommandInjection from `analyze_rust_file` continue emitting unconditionally — they are documented security findings, not smells.

NO change to `analyze_rust_file`'s body. NO change to `analyze_rust_file`'s signature. The gating layer is `VulnArgs::run`'s filter pipeline (post-`analyze_file`, pre-`build_summary`), parallel to the `include_informational` filter.

NO change to JSON / SARIF schema. Per-invocation finding **count** drops by default; per-finding **shape** is identical.

NO new VulnType / TaintSinkType / TaintSourceType variants. NO change to public API.

---

## §2 Sub-milestones

| ID  | Wave | Title                                                          | Atomic | Depends |
|-----|------|----------------------------------------------------------------|--------|---------|
| M1  | 1    | RED capture + flag-design audit confirmation                   | no     | —       |
| M2  | 2    | Implement `--include-smells` flag + Panic-emission gate        | yes    | M1      |
| M3  | 3    | Verify analyze_rust_* tests + binary smoke + JSON delta        | no     | M2      |
| M4  | 4    | CHANGELOG entry + local tag `rust-panic-suppression-v1`        | no     | M3      |

Detailed scope/files-modified/stop-thresholds in `dispatch-contract.json`.

---

## §3 Design decision

**Selected: Option A — single bool flag `--include-smells` (default false).**

Why A wins (full audit at `reports/investigation.json` §design_option_eval):

- T4 is the **only** smell-class trigger in `analyze_rust_file`. The other 6 are real findings. A single bool gates the singular noise source.
- `include_informational` (vuln.rs:92) is the established precedent: same default-false bool, same filter-pipeline gating layer at L194-196. Following the precedent means zero new patterns.
- Per-call `.unwrap()` Panic is high-FP, low-precision (0.70 confidence in the rust_finding call at vuln.rs:589 — the LOWEST confidence of any analyze_rust_file emission). Gating it by default does not lose security signal.

Why not B/C/D:

- **B (smell-level enum)** — overkill for 1 trigger. Migrate to enum if/when ≥3 smell triggers exist.
- **C (delete Panic emission)** — Per the user's no-gaming rule, deleting a real feature to fix UX is too aggressive. The line-scanner Panic finding has value when scoped; gating preserves that value behind opt-in.
- **D (taint-cross-reference)** — the genuinely-correct long-term fix, but huge scope (new TaintSinkType, taint state plumbing into analyze_rust_file). Defer to a separate post-publish milestone (`panic-taint-cross-ref-v1` or similar). Out of scope here.

**Layer choice (where to gate):** the **filter pipeline** in `VulnArgs::run` (mirrors `include_informational` at vuln.rs:194-196), NOT inside `analyze_rust_file`. Rationale: zero churn to the 6 existing test_analyze_rust_* tests. They call `analyze_rust_file` directly, which still emits Panic; only `VulnArgs::run` filters.

**Smell predicate:** `f.vuln_type == VulnType::Panic && f.title.starts_with("Potential Panic")`. Tight title prefix prevents over-suppression if a future canonical layer emits a different VulnType::Panic finding (defensive — the canonical pipeline does not currently emit Panic, but the predicate must not assume that invariant forever).

---

## §4 RED test contract

This milestone closes ZERO existing RED tests. The contract is:

- **All 9 `commands::remaining::vuln::tests` `#[test]` fns** GREEN throughout. They call `analyze_rust_file` directly; the filter is at `VulnArgs::run` layer. ZERO test signature churn for the existing 9.
- **All 167/168 vuln_migration_v1_red GREEN tests** STAY GREEN (the 1 RED is the Bucket B nested-constructor / Ruby carry-forward, untouched here).
- **All 36 `test_e2e_*` in tldr-core/security/vuln.rs** STAY GREEN (out-of-scope guard).

NEW tests added by M2 (small, ≤2 fns):

1. `test_vuln_run_default_suppresses_panic_findings` — drives `VulnArgs::run` with a temp-dir Rust file containing `.unwrap()` and `unsafe { ... }`. Asserts JSON output has zero `Panic` findings AND ≥1 `UnsafeCode` finding.
2. `test_vuln_run_with_include_smells_emits_panic_findings` — same setup, with `include_smells: true`. Asserts ≥1 `Panic` finding emerges.

These two tests close the unit-test gap that the layer choice (filter at `VulnArgs::run`) would otherwise leave open.

If `test_analyze_rust_detects_unwrap_in_non_test_code` (vuln.rs:1057) starts failing during M2, that's a SIGNAL the gating was wrongly placed inside `analyze_rust_file` — STOP and re-examine layer choice.

---

## §5 Risk register

| ID  | Risk                                                                                                  | Severity | Mitigation                                                                                                                         |
|-----|-------------------------------------------------------------------------------------------------------|----------|------------------------------------------------------------------------------------------------------------------------------------|
| R1  | CLI flag bikeshedding (`--include-smells` vs `--include-panic` vs `--smell-level=...`)                | low      | §3 selects `--include-smells` for forward-compat with future smell triggers + alignment with `include_informational` precedent. Document choice in M2 commit message; lock by ship.       |
| R2  | Existing test_analyze_rust_detects_unwrap_in_non_test_code regresses                                  | medium   | Layer choice (filter at `VulnArgs::run`) keeps `analyze_rust_file` body unchanged. M2 stop-threshold gates explicitly on this test plus all 8 others.                                     |
| R3  | Downstream JSON consumers piping `tldr vuln`'s output assume current finding counts                    | medium   | Schema is unchanged — only count drops. M3 captures before/after counts on a representative Rust corpus. M4 CHANGELOG entry calls out the count delta as a behavior change in `Changed:` section. NO schema change to call out. |
| R4  | Predicate over-suppresses: future canonical pipeline emits VulnType::Panic with different title       | low      | Predicate uses title-prefix `starts_with("Potential Panic")` — ties suppression to the line-scanner-specific title. If canonical emits Panic with a different title, it passes through. Documented in M2 inline comment.     |
| R5  | Predicate under-suppresses: line-scanner future Panic emissions with different title                   | low      | Single emission site at vuln.rs:574-590 has fixed title "Potential Panic From unwrap()". If future smell triggers add new Panic emissions, they MUST share the prefix or update the predicate. Document the contract in the rust_finding call's surrounding comment in M2.        |
| R6  | The 2 new tests fail on Linux/Windows CI due to path separators                                        | low      | Use `tempfile::TempDir` + `Path::new` constructions; mirror `test_collect_files_includes_rust` (vuln.rs:986) which already handles this correctly. M2 test author follows that idiom.  |
| R7  | M2 implementation accidentally also gates UnsafeCode/MemorySafety/Sql/Cmd                              | high     | M2 stop-threshold: run `tldr vuln <fixture-with-unsafe>` and assert UnsafeCode finding emerges with default flags (no `--include-smells`). M3 binary smoke verifies independently.          |
| R8  | Flag name conflicts with future `tldr smells` command sharing the `vuln` arg parser                    | low      | `tldr smells` has its own subcommand (separate Args struct). No shared parser. Verified at planning time via `grep -rn "struct SmellsArgs"`.                                            |
| R9  | The `test_vuln_run_default_suppresses_panic_findings` test creates temp files that races CI parallel    | low      | Use `tempfile::TempDir` (auto-cleaned, unique per test). Cargo test default isolation handles this.                                |
| R10 | rust-vuln-taint-pipeline-v1 R2 sub-elephant doesn't actually quantify "flood" — could be ≤5 findings    | low      | M3 binary smoke quantifies on the workspace. If the real-world drop is <10 findings on the tldr-code corpus itself, the milestone STILL ships — the hardening is correct in principle even if the magnitude is smaller than feared. |

---

## §6 CHANGELOG draft

```markdown
## rust-panic-suppression-v1 — internal milestone

NOT a published release. Hardening milestone closing the rust-vuln-taint-pipeline-v1
R2 sub-elephant: production Rust codebases flooded `tldr vuln` output with per-`.unwrap()`
Panic findings, since the existing `is_rust_test_file` mask only suppressed `/tests/`,
`_test.rs`, and `tests.rs` paths.

### Added

- **vuln**: `--include-smells` CLI flag on `tldr vuln` (default `false`). When passed,
  restores legacy emission of line-scanner Panic findings (CWE-703 "Potential Panic
  From unwrap()") on Rust files. Mirrors the `--include-informational` opt-in pattern.

### Changed

- **vuln**: Default `tldr vuln` invocations on Rust files no longer emit per-`.unwrap()`
  Panic findings. The 6 other `analyze_rust_file` triggers (UnsafeCode, MemorySafety×3,
  SqlInjection, CommandInjection) continue emitting unconditionally — they are documented
  security findings, not smells. JSON / SARIF schema is UNCHANGED; only the per-invocation
  finding count drops on Rust files containing `.unwrap()` calls. Pass `--include-smells`
  to restore prior counts.

### Internal

- New filter step in `VulnArgs::run` (filter pipeline, post-`analyze_file`, pre-`build_summary`).
- Predicate: `f.vuln_type == VulnType::Panic && f.title.starts_with("Potential Panic")`.
- Test coverage: 2 new `#[test]` fns in `commands::remaining::vuln::tests` exercising
  both flag values via `VulnArgs::run`. All 9 pre-existing tests in the module remain
  GREEN unchanged.
```

---

## §7 Self-validation

This plan satisfies validator mandates:

| Mandate                                          | Compliance                                                                                                                          |
|--------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------|
| `no_breaking_api_change_to_json_schema`          | VulnFinding struct unchanged. Per-finding shape identical. Only emission count delta on Rust files. SARIF rules array reflects findings present (not a schema change). |
| `default_suppressed_panic`                       | `--include-smells` defaults to false. Filter at vuln.rs:194-196 layer (parallel to `include_informational`).                         |
| `panic_recoverable_via_flag`                     | `--include-smells true` restores legacy emission. Round-trip-tested by the 2 new `#[test]` fns added in M2.                          |
| `preserve_unsafecode_memorysafety_emission`      | Predicate is title+vuln_type-bound to Panic. UnsafeCode/MemorySafety/Sql/Cmd unaffected. M2 stop-threshold + M3 binary smoke verify. |
| `no_push_no_publish`                             | M4 applies local tag `rust-panic-suppression-v1` only. NO `cargo publish`, NO `git push`, NO Cargo.toml version bump. USER STANDING RULE.          |
| `test_analyze_rust_unit_tests_preserved`         | Layer choice (filter at `VulnArgs::run`) keeps `analyze_rust_file` body and signature unchanged. All 9 `commands::remaining::vuln::tests` GREEN.                       |

---

## §8 /autonomous-readiness

- **Scope precision:** binary contract (2 new tests pass + 9 existing tests GREEN + JSON schema unchanged). Verifiable at M2 stop-threshold.
- **Atomicity:** M2 ships flag definition + filter step + 2 new tests in one commit. Splitting creates intermediate states with broken/missing tests.
- **Reversibility:** M2 commit is single `git revert`-able. Flag is opt-in (default-false), so even if reverted, no user breakage from prior workflows.
- **Test inventory verified:** `reports/investigation.json` enumerates all 9 tests at exact line numbers; the 1 Panic-asserting test (`test_analyze_rust_detects_unwrap_in_non_test_code` at L1057) is documented as call-site-isolated from the filter layer.
- **Premortem applicable:** R1-R10 cover flag bikeshed, regression risk, downstream consumer count delta, predicate scope, CI flake, accidental over-gating, name conflict, race conditions, and the empirical-magnitude uncertainty. R7 (accidental over-gating of non-Panic types) is HIGH severity with explicit M2/M3 stop-threshold mitigation.
- **No carry-forward:** zero RED tests closed; zero RED tests reopened. Aggregate carry-forward is unchanged.
- **Standing rules adhered:** NO push, NO publish, NO version bump. Tag is local-only. Staging is explicit `git add <path>...`; Cargo.lock never staged. NO `git stash`. NO `#[cfg(test)]` masking — gating is at runtime via the CLI flag, not at compile time.

**Verdict (self-assessed):** PASS_AUTONOMOUS_READY pending /premortem on the dispatch contract.

# rust-vuln-taint-pipeline-v1 — Plan

**Status:** PLANNING (no source-code changes in this commit)
**Closes carry-forward:** vuln-source-parity-v1 M5 Bucket A Rust subset (4 RED tests)
**Reframe-of-record:** Reframe C from vuln-migration-v1 plan §0 — `analyze_rust_file` dispatch interaction with canonical taint pipeline.
**HEAD-at-planning:** `5d46628` (vuln-source-parity-v1 M6 followup; 158/166 GREEN on `vuln_migration_v1_red`).

---

## §0 Architectural reframe — verified at HEAD `5d46628`

### What was empirically verified during planning

The vuln-source-parity-v1 M1 investigation (reports/M1-investigation.json line 250-279) established:

- `tldr taint <fixture> <fn>` correctly detects source/sink/flow on all 4 Rust positive fixtures (env_var → ShellExec / FileOpen / Deserialize) at HEAD; ssrf has a sink-bank gap on `reqwest::blocking::get` (RUST_AST_SINKS HttpRequest at taint.rs:2464-2477 lists `reqwest::get` but not the `::blocking::` qualifier).
- `tldr vuln <fixture>` returns zero findings on the same fixtures.
- Root cause empirically isolated: `crates/tldr-cli/src/commands/remaining/vuln.rs:368-370` (function `analyze_file`) routes `.rs` files exclusively to `analyze_rust_file` and short-circuits before `tldr_core::security::vuln::scan_vulnerabilities`.

The dispatch shape at HEAD (read at planning time):

```rust
fn analyze_file(path: &Path) -> Result<Vec<VulnFinding>, RemainingError> {
    let source = fs::read_to_string(path).map_err(|_| RemainingError::file_not_found(path))?;
    if matches!(path.extension().and_then(|e| e.to_str()), Some("rs")) {
        return Ok(analyze_rust_file(path, &source));    // <-- early return; canonical never runs
    }
    match tldr_core::security::vuln::scan_vulnerabilities(path, None, None) { ... }
}
```

The doc-comment above `analyze_file` (lines 354-365) explicitly references "Reframe C — `analyze_rust_file` is a distinct concern (UnsafeCode/MemorySafety/Panic line-scanner, not taint flow)". The early return is intentional but blocks the 4 carry-forward tests.

### What `analyze_rust_file` actually emits (line-scanner, lines 449-630)

Six categories of emissions, by VulnType:

| Trigger pattern | VulnType emitted | CWE | Severity | Confidence |
|-----------------|------------------|-----|----------|------------|
| `unsafe {` block without nearby `// SAFETY:` comment | `UnsafeCode` | CWE-242 | High | 0.80 |
| `std::mem::transmute(` / `mem::transmute(` | `MemorySafety` | CWE-119 | Critical | 0.90 |
| `std::ptr::*` / `core::ptr::*` / `ptr::read(` / `ptr::write(` | `MemorySafety` | CWE-119 | High | 0.85 |
| `.unwrap()` in non-test code | `Panic` | CWE-703 | Medium | 0.70 |
| `format!(` with SQL keywords + `{}`/`{`/`+` | `SqlInjection` | CWE-89 | Critical | 0.88 |
| `from_utf8_unchecked(` / `.as_bytes()[` / `.as_bytes().get_unchecked(` | `MemorySafety` | CWE-119 | High | 0.82 |
| `Command::new(` + `.arg(non-string-literal)` | `CommandInjection` | CWE-78 | Critical | 0.80 |

CRITICAL OBSERVATION FOR DESIGN: `analyze_rust_file` already emits two of the six base canonical VulnType variants (`SqlInjection`, `CommandInjection`) — it is NOT cleanly orthogonal to the canonical 6-type taxonomy. Domain-partition (Option C) cannot be a pure type-set partition; it must accept that both layers can emit `SqlInjection`/`CommandInjection` and the merged finding list will potentially contain duplicates for the same line.

### What scan_vulnerabilities emits (canonical pipeline, vuln.rs:366-433)

Six VulnType variants only: `SqlInjection`, `Xss`, `CommandInjection`, `PathTraversal`, `Ssrf`, `Deserialization` (per `tldr_core::security::vuln::VulnType` enum; mapped through `map_core_vuln_type` at remaining/vuln.rs:437-447 with NO `_` arm — exhaustive match prevents silent variant additions).

The taxonomy gap covered ONLY by `analyze_rust_file`: `UnsafeCode`, `MemorySafety`, `Panic` (3 Rust-specific smell variants).

### Assertion

The dispatch architecture at HEAD is the simple single-line route at vuln.rs:368-370. There is no nested complication; the early-return is the single point of intervention.

---

## §1 Bundle scope — RECOMMENDED OPTION C with overlap acknowledgement

### Recommended design: Option C — DUAL DISPATCH WITH DOMAIN-AWARE MERGE

```rust
fn analyze_file(path: &Path) -> Result<Vec<VulnFinding>, RemainingError> {
    let source = fs::read_to_string(path).map_err(|_| RemainingError::file_not_found(path))?;
    let is_rust = matches!(path.extension().and_then(|e| e.to_str()), Some("rs"));

    // Run the canonical taint pipeline for ALL extensions (.rs included).
    let mut findings = match tldr_core::security::vuln::scan_vulnerabilities(path, None, None) {
        Ok(report) => report.findings.into_iter().map(convert_core_finding).collect::<Vec<_>>(),
        Err(_) => Vec::new(),
    };

    // For .rs additionally run the line scanner — emits the 3 Rust-specific
    // smell variants (UnsafeCode/MemorySafety/Panic) plus 2 overlapping
    // categories (SqlInjection/CommandInjection) handled by the domain
    // de-duplication step below.
    if is_rust {
        let mut line_findings = analyze_rust_file(path, &source);
        // Domain-aware de-duplication: drop line-scanner SqlInjection/CommandInjection
        // findings on lines where the canonical pipeline already emitted the
        // SAME VulnType. Preserves the line-scanner's UnsafeCode/MemorySafety/Panic
        // findings and any non-overlapping SqlInjection/CommandInjection findings
        // (e.g., when the canonical pipeline misses a sink due to bank coverage).
        dedupe_overlap(&mut line_findings, &findings);
        findings.extend(line_findings);
    }
    Ok(findings)
}
```

### Why Option C (vs A/B/D)

| Option | Verdict | Rationale |
|--------|---------|-----------|
| A — naive dual dispatch (no dedup) | REJECTED | Duplicate `SqlInjection`/`CommandInjection` findings on the same line. UX degradation vs current behavior. |
| B — scan_vuln first, fallback only on zero findings | REJECTED | Files with valid taint flow lose UnsafeCode/Panic smell coverage entirely. Non-additive regression on existing line-scanner behavior. |
| **C — DUAL with domain-aware dedup** | **RECOMMENDED** | Closes the 4 carry-forward tests, preserves all line-scanner-only smells, dedupes overlap on (line, VulnType). Smallest behavior delta from HEAD that closes the tests. |
| D — port line scanner to AST | REJECTED (out of scope) | Would require rewriting `analyze_rust_file`'s 6 trigger-patterns as canonical AST patterns (UnsafeCode would need a TaintSinkType variant or a parallel emission path) — multi-milestone scope, not a 4-test-closure fix. |

### What Option C explicitly preserves

- All 6 line-scanner emit categories continue to fire on `.rs` files.
- Test fixtures asserting `≥1 command_injection` see canonical findings emerge — the line scanner's CommandInjection finding may be deduped, but at least one finding survives.
- `tldr taint` behavior unchanged (no plumbing change at the canonical layer).
- `map_core_vuln_type` exhaustive-match contract preserved.
- All 9 `#[test]` fns in the `commands::remaining::vuln::tests` module at vuln.rs:925-1037 continue to GREEN — they call `analyze_rust_file` directly and don't observe the dispatcher's merged output. Of the 9, 6 prefix-match `test_analyze_rust_detects_*` (the line-scanner emit-category coverage); the remaining 3 are `test_collect_files_includes_rust`, `test_vuln_type_cwe_mapping`, `test_vuln_type_severity` (auxiliary). The regression guard is the full module, NOT just the 6 prefix-matched tests — see A4 amendment in §11.

### What Option C explicitly does NOT preserve

- `tldr vuln` output for `.rs` files now includes BOTH canonical taint findings AND line-scanner smell findings. JSON/SARIF consumers see ~2x finding count on Rust files containing both taint flow AND `.unwrap()` / `unsafe` etc.
- The current "Rust files only emit smell findings" implicit contract is broken (it was never declared as a public contract; the doc-comment at vuln.rs:354-365 calls it Reframe C and explicitly anticipates this milestone closing it).

---

## §2 Sub-milestone list

5 milestones. M2 is the atomic dispatch flip; M3 closes the SSRF bank gap (must serialize after M2 to avoid taint.rs touch race with the dispatch flip — actually independent files but routed sequentially per workflow precedent). M4 verifies + smokes; M5 CHANGELOG/tag.

### M1 — Investigation, RED capture, line-scanner emit-category audit

- **depends:** []
- **atomic_commit:** false
- **files_modified:**
  - `continuum/autonomous/rust-vuln-taint-pipeline-v1-plan/reports/M1-report.json`
  - `continuum/autonomous/rust-vuln-taint-pipeline-v1-plan/reports/M1-red-capture.txt`
  - `continuum/autonomous/rust-vuln-taint-pipeline-v1-plan/reports/M1-line-scanner-audit.json`
- **scope:**
  - Run `cargo test -p tldr-cli --release --test vuln_migration_v1_red rust_` (capture: 4 RED, 4 GREEN string-literal-FP regression-guards expected).
  - Build release binary; for each of the 4 positive fixtures run BOTH `tldr taint <fixture> handler` AND `tldr vuln <fixture> --lang rust`. Confirm taint output matches M1-investigation.json claims.
  - Also run `tldr vuln` on each of the 4 string-literal-FP fixtures; record finding counts to compare against post-M2 (regression baseline).
  - Capture the 9 `test_analyze_rust_*` unit tests passing (baseline before M2 dispatch flip; M2 must not regress them).
- **red_tests:** rust_command_injection_positive, rust_deserialization_positive, rust_path_traversal_positive, rust_ssrf_positive
- **green_files (regression baselines):**
  - rust_command_injection_string_literal_fp (must STAY GREEN at every gate)
  - rust_deserialization_string_literal_fp
  - rust_path_traversal_string_literal_fp
  - rust_ssrf_string_literal_fp
  - all 9 test_analyze_rust_* in remaining/vuln.rs
- **loc_delta:** 0 source code; ~150 LOC across 3 report artifacts.
- **stop_thresholds:**
  - All 4 RED tests confirmed RED at HEAD.
  - All 4 string-literal-FP regression-guards confirmed GREEN at HEAD.
  - 9/9 `test_analyze_rust_*` passing.
  - `tldr taint` output on each of the 4 positive fixtures recorded verbatim in M1-line-scanner-audit.json.
  - `tldr vuln` output on each of the 4 positive fixtures recorded (expecting smell findings only — no taint findings).
  - `tldr vuln` output on each of the 4 FP fixtures recorded (expecting zero findings — pre-M2 baseline).

### M2 — ATOMIC dispatch flip + dedupe helper + SSRF bank patch + test verification

- **depends:** [M1]
- **atomic_commit:** TRUE — must ship the dispatch flip + dedupe helper + SSRF bank entry + any required adjustments in a SINGLE commit. Splitting them creates intermediate states where (a) Rust files emit 2x findings unchecked, or (b) ssrf_positive stays RED because the bank gap blocks the canonical pipeline even with the dispatch wired.
- **files_modified:**
  - `crates/tldr-cli/src/commands/remaining/vuln.rs` (analyze_file dispatch + new `dedupe_overlap` helper)
  - `crates/tldr-core/src/security/taint.rs` (ONE additive patch: extend `RUST_AST_SINKS` HttpRequest member_patterns to include `("", "reqwest::blocking::get")` — required to close `rust_ssrf_positive`)
- **scope:**
  - Replace the early-return at vuln.rs:368-370 with the dual-dispatch shape from §1.
  - Add `dedupe_overlap(line_findings: &mut Vec<VulnFinding>, canonical: &[VulnFinding])` helper: drops a line-scanner finding if there exists any canonical finding with the same `(line, vuln_type)`. Apply ONLY to overlap categories `SqlInjection`/`CommandInjection`; leave `UnsafeCode`/`MemorySafety`/`Panic` line findings always attached (no canonical analog exists).
  - Update doc-comment at vuln.rs:354-365 to retire the Reframe C carry-forward note.
  - Extend `RUST_AST_SINKS` HttpRequest pattern (taint.rs:2464-2477) `member_patterns` to include `("", "reqwest::blocking::get")`. This single AST sink-bank addition is the smallest possible patch to close `rust_ssrf_positive`. Document it as bundled with the dispatch flip per the atomic-commit rationale.
- **red_tests (must transition GREEN by M2 stop):**
  - rust_command_injection_positive
  - rust_deserialization_positive
  - rust_path_traversal_positive
  - rust_ssrf_positive
- **green_files (must remain GREEN):**
  - All 4 rust_*_string_literal_fp tests
  - All 9 test_analyze_rust_* unit tests
  - All 158 currently-GREEN tests in vuln_migration_v1_red.rs
  - All test_e2e_* tests in tldr-core/security/vuln.rs
- **loc_delta:** +30 to +50 LOC source code (~10 LOC dispatch shape, ~15 LOC dedupe helper, +1 LOC bank entry, ~10 LOC doc-comment update).
- **stop_thresholds:**
  - All 4 carry-forward tests transition GREEN.
  - 0 currently-GREEN tests transition RED — verified by `cargo test -p tldr-cli --release --test vuln_migration_v1_red`.
  - All 9 `test_analyze_rust_*` unit tests still GREEN (they call `analyze_rust_file` directly; dispatch flip doesn't touch that path).
  - 4 rust_*_string_literal_fp tests STAY GREEN (FP-clean regression-guard preserved).
  - `cargo check --workspace` PASS.
  - `cargo clippy --workspace -- -D warnings` PASS.
  - Workspace `cargo test --workspace` PASS (no regression in tldr-core or other crates).
  - SARIF/JSON output schema remains unchanged for `.rs` files (VulnFinding shape unchanged; only count delta).
- **rollback:** `git revert <M2_sha>` reverts dispatch + dedupe + bank atomically. Tests return to RED state cleanly; no orphan partial states.

### M3 — Public-API + binary smoke + dedupe behavior verification

- **depends:** [M2]
- **atomic_commit:** false
- **files_modified:**
  - `continuum/autonomous/rust-vuln-taint-pipeline-v1-plan/reports/M3-report.json`
  - `continuum/autonomous/rust-vuln-taint-pipeline-v1-plan/reports/M3-binary-smoke.json`
  - `continuum/autonomous/rust-vuln-taint-pipeline-v1-plan/reports/M3-dedupe-verification.json`
- **scope:**
  - Build release binary with `--features semantic`; smoke-test against all 4 RED-now-GREEN positive fixtures (each must produce ≥1 finding of the asserted vuln_type) AND all 4 FP fixtures (each must produce zero findings — preserves the closing-#24 zero-FP guarantee).
  - Run a representative non-Rust file (e.g., `python/sql_injection_positive.py`) through the binary to verify non-Rust dispatch is unchanged.
  - Verify dedupe behavior empirically: smoke-test on a constructed Rust file that contains both `let cmd = std::env::var(...).unwrap(); std::process::Command::new("sh").arg(&cmd).output();` (canonical CommandInjection emits) AND a line-scanner CommandInjection trigger on the same line. Confirm exactly ONE CommandInjection finding emerges (dedupe verified).
  - Capture JSON/SARIF output schema diffs for `.rs` file findings pre/post M2 — confirm shape unchanged, count increased.
- **red_tests:** none (all 4 already GREEN by M2; M3 verifies only).
- **green_files:** entire workspace test suite + all binary smoke fixtures.
- **loc_delta:** 0 source code; ~200 LOC across 3 report JSONs.
- **stop_thresholds:**
  - cargo test -p tldr-cli --release --test vuln_migration_v1_red: 162/166 GREEN (was 158/166 at HEAD; +4 from M2).
  - All 36 test_e2e_* tests in tldr-core remain GREEN.
  - Binary smoke outputs all 4 RED→GREEN transitions verified.
  - Dedupe verification fixture produces exactly 1 CommandInjection finding (not 2).
  - JSON/SARIF schema diff: zero structural changes; count delta only.
  - Non-Rust dispatch sanity: python_sql_injection_positive still produces ≥1 sql_injection finding.

### M4 — CHANGELOG entry

- **depends:** [M3]
- **atomic_commit:** false
- **files_modified:**
  - `CHANGELOG.md` (new entry under unreleased / next-version)
  - `continuum/autonomous/rust-vuln-taint-pipeline-v1-plan/reports/M4-report.json`
- **scope:** Add CHANGELOG entry per §7. No source code change.
- **red_tests:** none.
- **green_files:** workspace.
- **loc_delta:** ~25 LOC CHANGELOG; ~30 LOC report JSON.
- **stop_thresholds:**
  - CHANGELOG entry merged in (NOT published — internal milestone).
  - cargo build --workspace PASS (sanity).

### M5 — Local tag `rust-vuln-taint-pipeline-v1`

- **depends:** [M4]
- **atomic_commit:** false
- **files_modified:**
  - `continuum/autonomous/rust-vuln-taint-pipeline-v1-plan/reports/M5-tags-state.json`
- **scope:** Apply local tag at the M4 commit SHA. No push, no publish.
- **red_tests:** none.
- **stop_thresholds:**
  - Local tag `rust-vuln-taint-pipeline-v1` applied.
  - No push, no publish, no version bump.
  - `git tag --list 'rust-vuln-taint-pipeline-v1'` returns the tag.

---

## §3 Design decision matrix

| Option | Closes 4 RED? | Preserves smell findings? | Duplicate-finding risk? | LOC delta | Public-API change? | Verdict |
|--------|---------------|---------------------------|-------------------------|-----------|--------------------|---------|
| A — naive dual (no dedup) | YES | YES | HIGH (CmdInj/SqlInj overlap) | ~10 LOC | None | REJECTED — UX-degrading dup findings |
| B — canonical-first, fallback on zero | PARTIAL (smell-only files lose smell coverage if canonical fires anywhere) | NO | NONE | ~5 LOC | None | REJECTED — non-additive regression on existing behavior |
| **C — dual + domain-aware dedup** | **YES** | **YES** | **NONE (deduped)** | **~30-50 LOC** | **None** | **RECOMMENDED** |
| D — migrate line scanner to AST canonical | YES | YES (rewritten) | NONE | ~300+ LOC; new TaintSinkType variants | YES (TaintSinkType enum) | OUT OF SCOPE — multi-milestone effort |

### Why C's overlap risk is manageable

The line scanner's `SqlInjection` trigger is `format!(... SQL keyword ...)` on a single line — narrow pattern. Its `CommandInjection` trigger is `Command::new + .arg(non-string-literal)` across a 2-line block. Neither fires on lines that aren't already structural sinks.

**Empirically (per premortem `dab0766` verification, A1):** dedupe is a NO-OP on ALL 4 RED positive fixtures, NOT a 1-finding-collapse on `command_injection_positive`. The line scanner emits ZERO findings on `rust_command_injection_positive` because the fixture's `.arg("-c").arg(&cmd)` lives on a single line — the substring `.arg("` (from `.arg("-c")`) trips the line-scanner's string-literal guard at `crates/tldr-cli/src/commands/remaining/vuln.rs:603-604` (`!trimmed.contains(".arg(\"")` evaluates false → guard fires → `CommandInjection` not emitted). The other 3 RED fixtures (deserialization / path_traversal / ssrf) lack any line-scanner trigger entirely (no `Command::new`, no `format!(...SQL...)`, no `unsafe`, no `transmute`, no `ptr::*`; `.unwrap()` is suppressed by `is_rust_test_file` because the fixture path contains `/tests/`). Net line-scanner output for ALL 4 RED fixtures at HEAD: ZERO findings. Therefore dedupe correctness MUST be verified via a CONSTRUCTED M3 ad-hoc fixture (single-line `Command::new + .arg(&cmd)` without preceding `.arg("...")`) — NOT against any of the 4 RED fixtures. This is the basis for A2/A3 amendments (M3-dedupe-verification.json scope).

---

## §4 Test fixtures

### The 4 RED tests are the contract

`crates/tldr-cli/tests/vuln_migration_v1_red.rs` (no edits required by this milestone):

| Test | Line | Asserts |
|------|------|---------|
| rust_command_injection_positive | 1003 | findings_of_type(report, "command_injection").is_empty() == false |
| rust_path_traversal_positive | 1417 | findings_of_type(report, "path_traversal").is_empty() == false |
| rust_ssrf_positive | 1647 | findings_of_type(report, "ssrf").is_empty() == false |
| rust_deserialization_positive | 1923 | findings_of_type(report, "deserialization").is_empty() == false |

### The 4 string-literal FP regression-guards

| Test | Line | Asserts |
|------|------|---------|
| rust_command_injection_string_literal_fp | 1014 | all_findings(report).is_empty() — must be zero post-M2 |
| rust_path_traversal_string_literal_fp | 1428 | all_findings(report).is_empty() |
| rust_ssrf_string_literal_fp | 1658 | all_findings(report).is_empty() |
| rust_deserialization_string_literal_fp | 1934 | all_findings(report).is_empty() |

These assert `all_findings(report).is_empty()` — they're WHOLE-REPORT FP guards, not per-vuln-type. Reading `command_injection_string_literal_fp.rs`: it contains `format!("{} {}", doc, more)` (no SQL keyword), no `.unwrap()`, no `unsafe`, no `Command::new` — analyze_rust_file emits zero. The canonical pipeline emits zero (no env_var source detected on string-literal mentions). Confirmed FP-clean post-M2. M2 stop-threshold gates explicitly on "0 currently-GREEN tests transition RED" which subsumes this.

### Dedupe-verification fixture (M3 only)

M3 constructs an ad-hoc fixture file (NOT added to the test corpus) with both canonical CommandInjection trigger AND line-scanner CommandInjection trigger on overlapping lines, runs the binary, and verifies exactly 1 finding emerges. Result captured in M3-dedupe-verification.json.

### No new test fixtures added to the test suite

The 4 existing RED tests are sufficient as the binary contract. Adding new fixtures would scope-creep this milestone. The 9 `test_analyze_rust_*` unit tests in vuln.rs already cover the line-scanner's per-trigger emit behavior and continue to be the definitive contract for line-scanner output.

---

## §5 Dispatch wiring spec — exact change at remaining/vuln.rs:354-419

### Pre-M2 (HEAD)

```rust
fn analyze_file(path: &Path) -> Result<Vec<VulnFinding>, RemainingError> {
    let source = fs::read_to_string(path).map_err(|_| RemainingError::file_not_found(path))?;
    if matches!(path.extension().and_then(|e| e.to_str()), Some("rs")) {
        return Ok(analyze_rust_file(path, &source));
    }
    match tldr_core::security::vuln::scan_vulnerabilities(path, None, None) {
        Ok(report) => { /* convert and return findings */ }
        Err(_) => Ok(Vec::new()),
    }
}
```

### Post-M2 (target)

```rust
fn analyze_file(path: &Path) -> Result<Vec<VulnFinding>, RemainingError> {
    let source = fs::read_to_string(path).map_err(|_| RemainingError::file_not_found(path))?;
    let is_rust = matches!(path.extension().and_then(|e| e.to_str()), Some("rs"));

    // RUST-VULN-TAINT-PIPELINE-V1 M2 (Reframe C closure): canonical taint
    // pipeline now runs on .rs alongside the legacy line scanner. The
    // line scanner emits Rust-specific smell variants (UnsafeCode,
    // MemorySafety, Panic) plus narrow SqlInjection/CommandInjection
    // patterns; the canonical pipeline emits the 6 base taint VulnTypes.
    // dedupe_overlap drops line-scanner SqlInjection/CommandInjection
    // findings on (line, VulnType) tuples already covered by canonical
    // — preserving smell-only findings and unique line-scanner emissions.
    let mut findings: Vec<VulnFinding> = match tldr_core::security::vuln::scan_vulnerabilities(path, None, None) {
        Ok(report) => report.findings.into_iter().map(/* same converter as the existing Ok arm */).collect(),
        Err(_) => Vec::new(),
    };

    if is_rust {
        let mut line_findings = analyze_rust_file(path, &source);
        dedupe_overlap(&mut line_findings, &findings);
        findings.extend(line_findings);
    }
    Ok(findings)
}

/// Drop line-scanner findings whose (line, vuln_type) tuple is already
/// covered by a canonical finding. Applies only to vuln_type values
/// shared between both layers (SqlInjection, CommandInjection). Other
/// line-scanner-only types (UnsafeCode, MemorySafety, Panic) are never
/// dropped.
fn dedupe_overlap(line_findings: &mut Vec<VulnFinding>, canonical: &[VulnFinding]) {
    line_findings.retain(|line_f| {
        match line_f.vuln_type {
            VulnType::SqlInjection | VulnType::CommandInjection => {
                !canonical.iter().any(|c| c.vuln_type == line_f.vuln_type && c.line == line_f.line)
            }
            _ => true, // UnsafeCode / MemorySafety / Panic / etc.: always keep
        }
    });
}
```

### Doc-comment update at lines 354-365

Replace the current Reframe C reference with: "RUST-VULN-TAINT-PIPELINE-V1 M2 (Reframe C closure): post-M2, `.rs` files run the canonical `scan_vulnerabilities` pipeline AND the line-scanner `analyze_rust_file`, with domain-aware dedup on (line, VulnType) tuples for the overlapping `SqlInjection` / `CommandInjection` categories. The legacy 'Rust files emit smell findings only' implicit contract is retired."

### SSRF bank patch at taint.rs:2464-2477

```rust
// Pre-M2:
member_patterns: &[
    ("*", "get"),
    ("*", "post"),
    ("", "reqwest::get"),
    ("", "reqwest::Client"),
    ("", "ureq::get"),
    ("", "ureq::post"),
    ("", "hyper::Client"),
    ("", "Url::parse"),
],
// Post-M2: append ("", "reqwest::blocking::get") + ("", "reqwest::blocking::Client") for
// completeness with the blocking variant of the reqwest API.
```

---

## §6 Output schema impact

### JSON output

`VulnFinding` struct shape (crates/tldr-cli/src/commands/remaining/types.rs:1437) is unchanged. Pre-M2 a `.rs` file analysis returned only line-scanner findings; post-M2 it returns canonical findings + deduped line-scanner findings. The fields `vuln_type`, `severity`, `cwe_id`, `title`, `description`, `file`, `line`, `column`, `taint_flow`, `remediation`, `confidence` are unchanged. Count of findings increases.

### SARIF output

`generate_sarif` at vuln.rs:806 iterates findings and emits `results[]` entries; rules array is keyed off encountered vuln_types. Post-M2 a `.rs` file may add `CWE-78` / `CWE-89` / `CWE-22` / `CWE-502` / `CWE-918` rules where pre-M2 only `CWE-242` / `CWE-119` / `CWE-703` were emitted. The exhaustive `map_core_vuln_type` ensures no silent variant relabel — closing-#11 protection preserved.

### Risk: SARIF rule-id stability

A consumer that pinned to "Rust files always emit CWE-242 / CWE-119 / CWE-703 only" will see new rule ids. We do NOT consider this a public-API break — the Reframe C doc-comment at vuln.rs:354-365 explicitly anticipated this milestone. CHANGELOG entry §7 surfaces the behavior change.

---

## §7 CHANGELOG draft

```markdown
### Changed

- **vuln**: Rust file analysis now runs the canonical taint pipeline alongside the legacy line scanner. `tldr vuln file.rs` emits canonical taint findings (SqlInjection, Xss, CommandInjection, PathTraversal, Ssrf, Deserialization) AND line-scanner smell findings (UnsafeCode, MemorySafety, Panic). Findings on the same `(line, vuln_type)` are domain-deduped to a single entry; line-scanner-only smells are always preserved. Closes the 4 Rust positive fixtures carried forward from `vuln-source-parity-v1` M5 Bucket A (rust_command_injection_positive, rust_deserialization_positive, rust_path_traversal_positive, rust_ssrf_positive). Reframe C from `vuln-migration-v1` plan §0 closure.

### Added

- **taint banks**: `RUST_AST_SINKS` HttpRequest patterns extended with `reqwest::blocking::get` and `reqwest::blocking::Client` to close the SSRF bank gap surfaced in `vuln-source-parity-v1` M1 investigation.
```

---

## §8 Atomic-commit checklist

### M2 atomic boundary (single commit)

The dispatch flip + dedupe helper + SSRF bank patch + doc-comment update MUST land in ONE commit. Splitting creates intermediate states with regressions:

1. **Dispatch flip without SSRF bank patch:** `rust_ssrf_positive` stays RED because canonical pipeline runs on .rs but the bank gap blocks sink detection. Intermediate commit shows 3/4 closure — confusing.
2. **SSRF bank patch without dispatch flip:** Bank entry is dead code (never reached for `.rs` files). Clippy may complain about unused-pattern; harmless but messy.
3. **Dispatch flip without dedupe helper:** `rust_command_injection_positive` produces 2 CommandInjection findings on the same line (one canonical, one line-scanner). Test asserts `≥1` so it's GREEN, but UX is degraded for any file containing both `let cmd = …unwrap();` AND `Command::new + .arg(&cmd)`.

Atomic-commit gate prevents all three intermediate-states from existing as a SHA in the history.

### Files staged for M2 (single commit, explicit add):

```
git add crates/tldr-cli/src/commands/remaining/vuln.rs \
        crates/tldr-core/src/security/taint.rs
```

Cargo.lock `git checkout HEAD -- Cargo.lock` if dirty. Never staged.

### M3, M4, M5 commits

Non-atomic per-milestone (each is documentation-only).

---

## §9 Premortem / risk register

### R1 — Dedupe logic incorrectly drops a unique line-scanner finding (TIGER, MEDIUM)

**Failure mode:** dedupe_overlap is too aggressive, drops a line-scanner CommandInjection finding on a line where canonical also emits — but the user wanted the line-scanner-specific message ("Unsanitized Process Argument" with confidence 0.80). We lose the line-scanner's specific phrasing.

**Mitigation:** Test with the dedupe-verification fixture in M3. Document that on overlap, the canonical finding's title/description wins (it's more taint-flow-aware). If user research surfaces a need for line-scanner-message-preservation, future milestone can switch to "merge by union with description-concatenation"; this milestone does NOT solve that.

### R2 — `.unwrap()` Panic findings flood Rust file output (ELEPHANT, HIGH)

**Failure mode:** Every Rust positive fixture has 1-3 `.unwrap()` calls; analyze_rust_file emits one `Panic` finding per. Post-M2 a `.rs` file with 50 `.unwrap()` calls returns 50 Panic findings PLUS canonical taint findings. UX noise on real-world Rust codebases.

**Mitigation:** Pre-existing behavior — `tldr vuln file.rs` emits 50 Panic findings TODAY. M2 doesn't change that; it only ADDS canonical findings on top. CHANGELOG note flags the behavior. Future milestone could add `--no-smells` flag or move Panic to severity-Info default-suppressed; out of scope here.

**Sub-elephant (per premortem dab0766 E3, surfaced post-planning):** `is_rust_test_file` at `crates/tldr-cli/src/commands/remaining/vuln.rs:679-685` returns TRUE for any path containing `/tests/` (and `\\tests\\`, `_test.rs`, `tests.rs`) — this masks Panic emission on the 4 RED fixtures (which all live under `crates/tldr-cli/tests/fixtures/`), giving a misleading "clean" picture during M1/M2 verification. On user codebases (typical paths like `crates/foo/src/main.rs`, `src/lib.rs`), `is_rust_test_file` returns FALSE → every `.unwrap()` emits 1 Panic finding. Magnitude is unmeasured at planning time. **Carry-forward acknowledged**: out of scope for this milestone (no scope expansion); flagged for post-publish hardening (potential `--no-smells` flag or Panic→Info default-suppressed move).

### R8 — Wildcard `("*", "get")` HttpRequest sink becomes live on .rs (ELEPHANT, MEDIUM-HIGH)

**Failure mode (per premortem dab0766 T2/E1, upgraded LOW → MEDIUM-HIGH):** `RUST_AST_SINKS` HttpRequest member_patterns at `crates/tldr-core/src/security/taint.rs:2467` contains `("*", "get")` — wildcard receiver + method=`get`. At HEAD this entry is DEAD CODE for `.rs` files because vuln.rs:368-370 dispatch routes `.rs` away from canonical. **Post-M2 it becomes LIVE**: every `.rs` file's field-expression `<receiver>.get(<args>)` with a tainted receiver/argument emits an `Ssrf` finding. This includes `HashMap::get`, `Vec::get`, `Option::get`, `BTreeMap::get`, `IndexMap::get`, etc. — none of which are HTTP requests. Real-world Rust codebases use `.get()` extensively; production UX may degrade significantly. The 4 RED fixtures don't exercise this (no `.get(` field-expression on tainted data) and the 4 string-literal-FP fixtures don't either, so M2 stop-thresholds will pass — but the false-positive rate on user code is unknown.

**Mitigation:** Out of M2 scope per atomic-commit boundary (bank cleanup is a follow-on). **A2 amendment requires M3 to QUANTIFY the FP rate**: M3 binary smoke MUST construct synthetic Rust fixtures exercising newly-live patterns (HashMap::get on tainted, Vec::get on tainted, Option::get on tainted, std::fs::read_to_string on tainted FileOpen, reqwest::blocking::get on tainted as the SSRF closure path). M3-binary-smoke.json records FP count per pattern. If FP rate >10% on synthetic patterns, surface as a follow-on bank-tightening milestone (e.g., constrain `("*", "get")` to receivers with HTTP-client types). Document as known carry-forward in M3-report.json if rate is high; ship M2 atomic commit regardless (4-RED closure is the binary contract).

### R3 — String-literal FP regression-guard tests fail (TIGER, HIGH)

**Failure mode:** Canonical pipeline produces an unexpected finding on a string-literal FP fixture. E.g., the `format!("{} {}", doc, more)` line in command_injection_string_literal_fp.rs has no canonical source/sink so should emit zero — but a future bank entry could change that.

**Mitigation:** M1 captures FP-fixture finding count at HEAD; M2 stop-threshold gates on "all 4 rust_*_string_literal_fp tests STAY GREEN". Empirically verified safe at planning time: each FP fixture lacks env_var source AND lacks shell/file/http/deser sinks AND lacks `.unwrap()`/`unsafe`/`Command::new`/`format!(...SQL...)` triggers. Both layers emit zero.

### R4 — `test_analyze_rust_*` unit tests regress (TIGER, LOW)

**Failure mode:** dispatch flip changes the path through which `analyze_rust_file` is invoked; if M2 accidentally modifies analyze_rust_file's behavior, the 9 unit tests at vuln.rs:925-1037 fail.

**Mitigation:** M2 explicitly does NOT modify `analyze_rust_file`. The dispatch shape change is in `analyze_file` only. The 9 unit tests call `analyze_rust_file` directly; they're decoupled from `analyze_file`. M2 stop-threshold verifies them GREEN.

### R5 — Public API consumer pins on Rust-only-emits-smells contract (ELEPHANT, LOW)

**Failure mode:** A downstream tool consuming `tldr vuln --format json` filters out CWE-242/119/703 (line-scanner smell ids) to get "real" findings; post-M2 it sees CWE-78/89/22/502/918 which were never expected on `.rs` files, breaks parsing assumptions.

**Mitigation:** Reframe C in vuln-migration-v1 §0 was an INTERNAL routing decision, not a public contract. CHANGELOG entry §7 announces the behavior change. JSON shape unchanged — only the rule-id distribution shifts. Consumers using exhaustive vuln_type match handle this transparently.

### R6 — Performance regression: 2x parsing for Rust files (ELEPHANT, LOW-MED)

**Failure mode:** Pre-M2: `.rs` files parsed once via line scanner (no tree-sitter parse). Post-M2: `.rs` files parsed twice — line scanner walks `lines.iter()`, canonical pipeline runs tree-sitter parse + CFG + DFG + taint construction.

**Mitigation:** Quantify baseline at M3 binary smoke. The canonical pipeline's per-file cost is ~10-50ms on small fixtures; the line scanner's cost is ~0.1ms (just string ops). Post-M2 `.rs` files cost roughly the same as `.py` / `.js` / etc. files (which already use canonical). Acceptable per pre-existing test_e2e_* perf carry-forward — no NEW regression class introduced by this milestone.

### R7 — Bank-additive change for SSRF accidentally introduces a non-Rust regression (TIGER, LOW)

**Failure mode:** Adding `("", "reqwest::blocking::get")` to RUST_AST_SINKS HttpRequest member_patterns is Rust-language-scoped (the `RUST_AST_SINKS` static is consulted only for Rust). But the wildcard `("*", "get")` already exists; a misread of code might add a wildcard that fires on JS/TS/etc.

**Mitigation:** §5 explicitly specifies the literal text of the new entry as scoped to the Rust bank. M2 stop-threshold verifies "0 currently-GREEN tests transition RED" across the workspace, including JS/TS/etc. SSRF tests.

---

## §10 Carry-forward exceptions

**EXPECTED EMPTY post-M3.** All 4 RED tests close in M2.

If M2 binary smoke surfaces an unexpected RED — for example, the canonical pipeline finds an additional bank gap not surfaced in M1 — surface in M3-report.json with a non-additive-resolution rationale. Do NOT extend M2 scope to fix unanticipated bank gaps; route to a follow-on milestone.

The aggregate carry-forward count after this milestone should be 4 (Bucket B `var-extract-nested-constructor-v1` cpp/java/scala_deserialization_positive — from vuln-source-parity-v1 M5; not addressed here). M5-aggregate goes from 8 retained RED to 4 retained RED.

---

## §11 Self-validation — validator_mandates

```yaml
validator_mandates:
  red_first_harness_required: |
    The 4 RED tests in vuln_migration_v1_red.rs (rust_*_positive) are the
    binary-verifiable success contract. No new tests authored by this
    milestone. M1 captures RED state at HEAD; M2 stop-threshold gates on
    all 4 transitioning GREEN; M3 binary-smoke verifies independently.

  no_public_api_change: |
    VulnFinding shape unchanged. map_core_vuln_type exhaustive-match
    contract preserved. tldr_core::security::vuln public exports
    unchanged. Public-API surface is binary-compatible pre/post M2.

  no_enum_variant_extension: |
    VulnType, TaintSinkType, TaintSourceType — no new variants.

  atomic_dispatch_commit: |
    M2 ships dispatch flip + dedupe helper + SSRF bank patch + doc-comment
    update in a SINGLE commit. Splitting creates intermediate states with
    regressions per §8.

  string_literal_regression_zero_new_fps: |
    All 4 rust_*_string_literal_fp tests REMAIN GREEN. Verified empirically
    at planning time (each FP fixture lacks env_var sources, taint sinks,
    and line-scanner triggers; both layers emit zero on FP fixtures).

  e2e_test_preservation_mandatory: |
    All 36 test_e2e_* tests at tldr-core/security/vuln.rs:1568-2100 must
    remain GREEN. Pre-existing regression-guard from vuln-migration-v1.

  test_analyze_rust_unit_tests_preserved: |
    All 9 #[test] fns in the `commands::remaining::vuln::tests` module
    at remaining/vuln.rs:925-1037 must remain GREEN post-merge (A4
    amendment per premortem dab0766). The 9 fns are:
      1. test_vuln_type_cwe_mapping
      2. test_vuln_type_severity
      3. test_collect_files_includes_rust
      4. test_analyze_rust_detects_unsafe_without_safety_comment
      5. test_analyze_rust_detects_command_and_sql_patterns
      6. test_analyze_rust_detects_transmute_usage
      7. test_analyze_rust_detects_raw_pointer_operation
      8. test_analyze_rust_detects_unwrap_in_non_test_code
      9. test_analyze_rust_detects_unchecked_bytes_patterns
    Of these, 6 prefix-match `test_analyze_rust_detects_*` and form the
    line-scanner emit-category coverage; the other 3 are auxiliary. The
    M3 stop-threshold regression guard is the FULL 9, run via
    `cargo test -p tldr-cli --release --lib commands::remaining::vuln::tests`.
    They call analyze_rust_file directly; M2's dispatch-only change
    does not touch analyze_rust_file's body.

  bank_addition_additive_only: |
    The SSRF bank patch (RUST_AST_SINKS HttpRequest member_patterns
    extension) is purely additive. M2 verification report MUST diff the
    HttpRequest member_patterns array before/after.

  no_perf_regression: |
    Post-M2 .rs files cost roughly the same as .py/.js/etc. (which
    already use canonical pipeline). M3 binary smoke captures wall-clock
    on representative Rust fixtures; if median time-per-file exceeds
    current Python median by >2x, surface in M3-report.json and triage.

  carry_forward_max_0: |
    All 4 Bucket A Rust subset tests must close in M2. Aggregate
    carry-forward count for THIS milestone is 0. (Bucket B — 3
    nested-constructor tests — is OUT OF SCOPE here; closes in
    var-extract-nested-constructor-v1.)

  staging_method_explicit: |
    Each commit stages exactly the listed files via `git add <path>...`.
    NO `git add -A` / `git add .`. Cargo.lock NEVER staged
    (`git checkout HEAD -- Cargo.lock` if dirty).

  no_push_no_publish_no_version_bump: |
    All milestones internal. Local tag `rust-vuln-taint-pipeline-v1` only
    in M5. NO `cargo publish`, NO `git push`, NO version bump in
    Cargo.toml.
```

---

## §12 /autonomous-readiness

**Recommended:** Run `/premortem` ONCE before launching M2 implementation worker. The dedupe logic is the highest-risk surface; a premortem worker can stress-test the dedupe correctness against constructed adversarial Rust files (line-scanner emit on line 5, canonical emit on line 5 with same VulnType; line-scanner emit on line 5, canonical emit on line 6 with same VulnType; etc.) and catch any boundary bug before M2 lands.

After premortem: ready for autonomous M2 implementation under standard kraken-builder pipeline.

### Conditions to declare ready

- [x] §0 architectural reframe verified empirically against HEAD
- [x] §1 design choice locked (Option C — DUAL DISPATCH WITH DOMAIN-AWARE MERGE)
- [x] §2 milestones enumerate 5 sub-milestones with stop_thresholds
- [x] §3 design decision matrix tabulates A/B/C/D rejected/accepted
- [x] §4 4 RED tests identified as binary contract; FP regression-guards enumerated
- [x] §5 dispatch wiring spec includes verbatim Rust-code pre/post diffs
- [x] §6 output schema impact analyzed; no shape changes
- [x] §7 CHANGELOG draft prepared
- [x] §8 atomic-commit boundary defined
- [x] §9 premortem identifies 8 risks with mitigations (R1-R7 from planning + R8 wildcard-get added per A2/E1; R2 expanded with E3 is_rust_test_file sub-elephant)
- [x] §10 carry-forward expected empty
- [x] §11 validator_mandates declared
- [x] **CONDITION:** /premortem completed (commit `dab0766`, verdict GO_WITH_AMENDMENTS); 5 non-blocking amendments A1-A5 applied per this commit
- [x] **CONDITION:** plan §3 dedupe-no-op claim corrected (A1)
- [x] **CONDITION:** §11 unit-test count clarified to 9 #[test] fns total in module (A4)
- [x] **CONDITION:** §9 risk register expanded with R8 wildcard-get + R2 E3 is_rust_test_file sub-elephant
- [x] **CONDITION:** dispatch-contract.json M3 stop-thresholds extended with constructed-fixture dedupe verification (A2/A3) + M1 baseline-drift check (A5)

Declared **/autonomous-ready** for autonomous M2 execution. Recommended worker: kraken (Opus, full implementation context).

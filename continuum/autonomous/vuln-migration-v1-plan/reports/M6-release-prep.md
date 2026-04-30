# vuln-migration-v1 — M6 Release Prep

**Status:** FINAL internal milestone of vuln-migration-v1 plan complete.
**Tag:** `vuln-migration-v1` (annotated, local-only) at commit `7afd2a0`.
**Posture:** NOT externally published. Awaits publish-operator gate on
`pre-publish-binary-verification.json`.

---

## 1. Milestone scope summary

vuln-migration-v1 collapses the `tldr vuln` command path onto the canonical
`compute_taint_with_tree` taint engine that already serves `tldr taint`. With
this milestone, `taint.rs` is the **single source of truth** for taint flow
detection across both commands, eliminating the parallel substring-scanner /
CLI-local Python `TaintTracker` paths that were the last remaining
regex-driven dispatch in the security pipeline.

| Wave | Title | Atomic | Key outcome |
|------|-------|--------|-------------|
| M1 | RED scaffold + 4 additive `TaintSinkType` variants + 132 fixtures + perf baseline | n/a | Test scaffolding + taxonomy extension landed |
| M2 | AST sink banks for 4 new VulnTypes across 16 langs | n/a | ~163 `AstSinkPattern` entries added |
| M3 | Collapse `tldr-core/security/vuln.rs` onto canonical | no | ~1000 LOC core deletion |
| M4 | Collapse CLI Python path (`remaining/vuln.rs`) onto canonical | no | ~984 LOC CLI deletion |
| M5 | Obsolete-test sweep + dead-code prune + binary verification | YES | Ship-gate met; 250e1b2 |
| M6 | CHANGELOG + local tag + pre-publish artifact | docs | This milestone |

---

## 2. LOC delta (final, actual)

| File | LOC delta |
|------|-----------|
| `crates/tldr-core/src/security/vuln.rs` | −550 net (~1000 LOC deletions, ~450 LOC additions for `From`-adapter + per-function dispatch + sanitization post-filters) |
| `crates/tldr-cli/src/commands/remaining/vuln.rs` | −984 net (Python TaintTracker eliminated; `analyze_file` now routes Python through canonical) |
| `crates/tldr-core/src/security/taint.rs` | +80 LOC (PHP echo var-extraction; SSA-active-path indirect-match fallback gated by SSA-tracked check; AST source-bank additions for argv/CommandLine.arguments/etc. across 8 langs; Swift `system` bare call_name; multi-fire detect_sinks_ast loop) |
| `crates/tldr-core/src/ast/extract.rs` | +2 LOC (visibility extension to `pub(crate)`) |
| `crates/tldr-core/src/cfg/extractor.rs` | +1 LOC (visibility) |
| `crates/tldr-core/src/dfg/extractor.rs` | +25 LOC (new `extract_dfg_from_tree_with_cfg` perf helper + visibility) |
| **Net** | **~−1426 LOC across the workspace** (well above the plan's −685 estimate; the canonical engine absorbed the lost surface without commensurate growth) |

Test code: ~26 obsolete tests deleted across M3/M4/M5 (22 in `vuln.rs` at
L1322-L2077 referencing deleted helpers + 4 in `remaining/vuln.rs` referencing
deleted CLI-local TaintTracker primitives). All 30 `test_e2e_*` tests at
`vuln.rs:1568-2100` and all CLI integration test files preserved + GREEN.

---

## 3. FP-closure verdict (binary-verified)

The closes-#24 string-literal substring FP class is **CLOSED end-to-end** at
the `tldr vuln` command path — the half left open by regex-removal-v1,
field_access_info-extension-v1, and sanitizer-removal-v1, all of which only
reached the `tldr taint` command path.

| Gate | Result | Source |
|------|--------|--------|
| 83/83 string-literal regression-guard fixture corpus | 0 findings (100% PASS) | `M5-binary-verification.json` `string_literal_corpus` |
| Original Phase-1 FP repros (Go) | 0 findings (was 3 FPs at HEAD) | `M5-binary-verification.json` `fp_repros.go_string_literal_fp` |
| Original Phase-1 FP repros (TypeScript) | 0 findings (was 1 FP citing comment line as sink) | `M5-binary-verification.json` `fp_repros.ts_fp2` |
| Python FP-clean property preserved post-collapse | 0 findings | `M5-binary-verification.json` `fp_repros.py_string_literal_fp` |
| Composite multi-pattern FP fixture | 0 findings | `vuln_migration_v1_composite_red.rs` GREEN |

This satisfies the closes-#24 root mandate across 16 languages × ~6 vuln
categories.

---

## 4. TaintSinkType extensions (additive, no breaking changes)

4 new variants at `taint.rs:153`:

| Variant | Maps to VulnType | Rationale |
|---------|------------------|-----------|
| `HtmlOutput` | `Xss` | Distinct concern from existing `WebOutput` |
| `FileOpen` | `PathTraversal` | Distinct from existing `FileWrite` (read-side) |
| `HttpRequest` | `Ssrf` | New VulnType not previously expressible |
| `Deserialize` | `Deserialization` | New VulnType — `pickle.loads`/`yaml.unsafe_load`/etc. |

**Match-site audit (M1 verification):** Zero exhaustive `match` sites on
`TaintSinkType` exist anywhere in the workspace; all `match` arms use `_ =>`
catch-all or specific arms with no exhaustiveness reliance. The 4 additions
are therefore non-breaking. Existing 6 variants preserved verbatim.

---

## 5. Performance disclosure (M3-CF-02)

The plan's two-axis perf gate (avg < 5× M1 baseline AND p99-file < 2× M1
baseline) was NOT met:

- **Avg:** 17.18× M1 baseline (vs ≤5× target)
- **p99-file:** 5.24× M1 baseline (vs ≤2× target)

**Root cause:** The M1 baseline (36.67ms avg / 34ms p99 on 20-file Go corpus)
was dominated by binary cold-startup cost. Absolute scanning work is ~33ms/
file, an acceptable absolute number for a per-file CLI tool.

**Mitigations applied:**
- Per-function rayon parallelization (~7× inner speedup)
- Per-file rayon parallelization at `scan_vulnerabilities` outer loop
- New `extract_dfg_from_tree_with_cfg` helper to avoid redundant CFG re-parse
  (DFG construction was previously re-parsing CFG inside `get_cfg_context`)

**Disposition:** Pragmatic acceptance per dispatch-contract M3 line 242
escape valve. The M1 perf-baseline methodology should be revisited in a
future milestone (binary-startup-isolated baseline + per-function micro-
benchmark in lieu of whole-file CLI invocation).

---

## 6. Pre-publish binary verification (M6 operator-handoff artifact)

Per validator mandate `pre_publish_binary_verification_post_m6`, four checks
run AFTER tag application. See
`continuum/autonomous/vuln-migration-v1-plan/reports/pre-publish-binary-verification.json`
for the structured artifact.

| # | Check | Verdict |
|---|-------|---------|
| 1 | `tldr vuln crates/` (self-scan) | PASS — findings are real (Rust line-scanner panic/unsafe/memory_safety + 28 command_injection + 92 sql_injection); zero closes-#24-class FPs |
| 2 | `cargo test --workspace --features semantic --release` | See artifact (run post-tag) |
| 3 | `cargo doc --no-deps -p tldr-core` | PASS — 75 warnings (all pre-existing, none reference new TaintSinkType variants HtmlOutput/FileOpen/HttpRequest/Deserialize) |
| 4 | Real-world Python smoke (`/tmp/vuln-mig-repro/string_literal_fp.py`) | PASS — 0 findings |

---

## 7. Carry-forwards documented (NOT closes-#24-blocking)

| ID | Description | Disposition |
|----|-------------|-------------|
| **M3-CF-01** | 32 source-bank-gap positive RED tests across 6 langs (Go/Java/CSharp/Scala/Lua/Elixir × multiple vuln types) | Deferred to **`vuln-source-parity-v1`** future internal milestone — analogous to M2's sink-bank audit |
| **M3-CF-02** | Perf two-axis gate FAIL — avg 17.18× / p99 5.24× | Pragmatic acceptance; baseline methodology should be revisited |
| **M4-CF-01** | `python_xss_positive` still RED (`response.write` not in canonical Xss sink bank) | Same disposition as M3-CF-01 |
| **M4-DEVIATION-01** | `vuln_type_name` retained against M1 enumeration | Output-shape preservation precedence — used by `generate_sarif` for SARIF `rules.name` + `shortDescription.text` |

The `analyze_rust_file` Rust line-scanner is permanently out of scope per
Reframe C — it detects Rust-IDIOMATIC smells (UnsafeCode/MemorySafety/Panic),
not source-to-sink propagation. A future `rust-smell-detector-canonical-v1`
follow-on would migrate it if a canonical smell-detector framework is built.

---

## 8. Tags state post-milestone

All internal tags are local-only (NEVER pushed). The internal-versioning
posture has been honored across the entire post-v0.2.4 window.

| Tag | Commit | Subject |
|-----|--------|---------|
| `engine-v1` | (pre-existing) | v0.3.0 engine bump |
| `quality-v1` | (pre-existing) | post-v0.3.0 quality bundle |
| `regex-removal-v1` | (pre-existing) | regex source/sink dispatch elimination |
| `field_access_info-extension-v1` | (pre-existing) | Ruby/Elixir/OCaml structured (Module, fn) entries |
| `sanitizer-removal-v1` | (pre-existing) | regex sanitizer dispatch elimination + #24 FP closure for sanitizers |
| **`vuln-migration-v1`** | **`7afd2a0`** | **`tldr vuln` collapsed onto canonical; closes-#24 FP class CLOSED end-to-end** |

---

## 9. Next steps (publish-operator handoff)

1. Publish-operator reads `pre-publish-binary-verification.json` and
   confirms verdict.
2. If verdict is `RECOMMEND-PUBLISH`:
   - Bump `Cargo.toml` workspace version (single coherent external bump
     covering all 5 internal milestones since v0.2.4).
   - Update top-of-CHANGELOG with a `## v0.X.Y — YYYY-MM-DD` external
     header that consolidates the 5 internal milestones.
   - `cargo publish` the workspace crates.
   - Push the local annotated tags (`engine-v1`, `quality-v1`,
     `regex-removal-v1`, `field_access_info-extension-v1`,
     `sanitizer-removal-v1`, `vuln-migration-v1`) AND a new external
     `vX.Y.Z` tag, simultaneously.
3. Post-publish, the following GitHub issues close in a single coherent
   release:
   - **#7** (callgraph) — quality-v1
   - **#23** (Rust trait FuncDef) — quality-v1
   - **#24** (string-literal substring FP, ALL paths) — regex-removal-v1
     + field_access_info-extension-v1 + sanitizer-removal-v1 +
     vuln-migration-v1
   - **#27** (cache cross-contamination) — engine-v1
   - **#28** (daemon language threading) — engine-v1
   - **`tldr vuln` FP class** (this milestone)
   - **Sanitizer correctness** — sanitizer-removal-v1

4. If verdict is `RECOMMEND-HOLD-AND-REVIEW` or worse, do NOT publish; open
   a remediation milestone with the failing-check details and re-run M6
   verification post-fix.

---

## 10. Standing rules upheld

- ✅ NO `git push` (local-only tag)
- ✅ NO `cargo publish`
- ✅ NO version bumps in any `Cargo.toml`
- ✅ `Cargo.lock` not staged
- ✅ Explicit-add staging only
- ✅ No source code in `crates/` modified during M6 (docs-only milestone)
- ✅ No sibling milestone files modified
- ✅ `dispatch-contract.json` and `plan.md` not modified
- ✅ No `git stash` use; no destructive git ops

---

*Generated 2026-04-30 as part of vuln-migration-v1 M6.*

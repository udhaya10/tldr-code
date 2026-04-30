# vuln-migration-v1 — Final Internal Milestone Plan

**Predecessor:** `sanitizer-removal-v1` (HEAD `c842962`)
**Tag-on-completion:** `vuln-migration-v1` (local only; no push)
**External-publish gate:** TRUE — this is the final internal milestone before the external `cargo publish` that closes #7, #23, #24, #27, #28 + the `tldr vuln` FP class + sanitizer correctness in one coherent release.

---

## §0 Architectural Reframe

The original handoff framing said:
> "vuln.rs has duplicate TaintSource/TaintSink types + inline propagation. M1a/M1b/M2 NEVER reach `tldr vuln`. The full anti-product elimination requires vuln-migration-v1 collapsing duplicate types and routing through compute_taint_with_tree."

Phase-1 investigation at HEAD `c842962` confirms the FP class but **falsifies two load-bearing premises** of that framing.

### Reframe A — duplicate types span TWO files, not one

There are **TWO** duplicate type families, not one:

1. **`crates/tldr-core/src/security/vuln.rs`** — has its own `VulnType`, `TaintSource`, `TaintSink`, `VulnFinding`, `VulnSummary`, `VulnReport` (lines 38–133). The `TaintSource`/`TaintSink` here are duplicates of canonical `taint.rs`'s types but with semantically-divergent fields (`String` source_type/sink_type vs canonical's `TaintSourceType`/`TaintSinkType` enums; `expression` vs canonical's `Option<statement>`; `function` vs canonical's `var`).

2. **`crates/tldr-cli/src/commands/remaining/vuln.rs`** — has its OWN `TaintSource` and `TaintSink` (different shape — static pattern tables: `module: &'static str, attr: &'static str, description: &'static str`), plus a CLI-local `TaintTracker` with its OWN `propagate()` and `expression_is_tainted()` methods (~700 LOC of intra-procedural taint). These drive the **Python tree-sitter path** (`analyze_python_file` → `analyze_node`).

The handoff implied a single duplicate-type collapse. Investigation shows: **two distinct collapses required**, on independent files, with independent test surfaces.

### Reframe B — canonical TaintSinkType is INSUFFICIENT for the vuln-product ontology

Canonical `TaintSinkType` (`taint.rs:153`) has **6 variants only**:
`SqlQuery, CodeEval, CodeExec, CodeCompile, ShellExec, FileWrite`.

But `tldr vuln` emits **6 user-facing `VulnType`s** (`SqlInjection, Xss, CommandInjection, PathTraversal, Ssrf, Deserialization`) and 4 of those have **NO clean `TaintSinkType` equivalent**:

| `VulnType` | `TaintSinkType` mapping | Status |
|---|---|---|
| `SqlInjection` | `SqlQuery` | 1:1 |
| `CommandInjection` | `ShellExec` (+ `CodeEval`/`CodeExec` for eval/exec) | 1:1 with overlap |
| `Xss` | NO VARIANT | needs `HtmlOutput` |
| `PathTraversal` | `FileWrite` is too narrow (write-only; misses read+open) | needs `FileOpen` |
| `Ssrf` | NO VARIANT | needs `HttpRequest` |
| `Deserialization` | NO VARIANT | needs `Deserialize` |

**Implication:** The migration is not a cosmetic refactor — it requires extending `TaintSinkType` with **+4 new variants** and adding AST sink-bank entries for those categories across all 16 supported languages. This is an additive public-API extension (existing variants unchanged) but it widens scope vs. what the handoff implied.

### Reframe C — Rust line-scanner is OUT OF SCOPE

`analyze_rust_file` (`remaining/vuln.rs:747`) is a line-scanner for **Rust-idiomatic security smells** (`UnsafeCode`, `MemorySafety`, `Panic` VulnTypes) — `unsafe { }`, `transmute()`, raw-pointer ops, `unwrap()` in non-test code, `mysql_query`. These are NOT taint flows from sources to sinks; they are pattern-detector outputs. `compute_taint_with_tree` is a CFG-based source-to-sink propagation engine; it is the wrong tool for "detect `unwrap()` in non-test code."

**Decision:** Rust line-scanner stays as-is. Migration scope is **taint-flow detection only** (the 6 taint-flow `VulnType`s above). `UnsafeCode`/`MemorySafety`/`Panic` remain `analyze_rust_file`'s responsibility.

### FP repro confirmed at HEAD

```
$ tldr vuln /tmp/vuln-mig-repro/string_literal_fp.go --lang go
Found 3 vulnerabilities:  ← ALL FALSE POSITIVES
  1. CommandInjection — source line is a STRING LITERAL containing "os.Args"
  2. CommandInjection — same string-literal source
  3. CommandInjection — source line is a STRING LITERAL containing "os.Args"
```

```
$ tldr vuln /tmp/vuln-mig-repro/fp2.ts --lang typescript
Found 1 vulnerabilities:  ← FALSE POSITIVE
  Sink line cited is a COMMENT (// ...) — substring scanner does not strip comments.
```

Negative-control on Python (which uses tree-sitter, not the substring path):
```
$ tldr vuln /tmp/vuln-mig-repro/string_literal_fp.py
No vulnerabilities found.  ← CORRECT
```

Confirmed: **FP class is concentrated in the 14 fall-through languages** routed via `tldr_core::security::vuln::scan_vulnerabilities` (Go, Java, Ruby, C, C++, PHP, Kotlin, Swift, C#, Scala, Elixir, Lua, Luau, OCaml + TS/JS). Python (tree-sitter) and Rust (line-scanner) paths are FP-clean for the substring-FP class.

---

## §1 Bundle Scope

### In-scope migrations

**Type collapses (Reframe A — both files):**

| File | Type | Disposition | Rationale |
|---|---|---|---|
| `tldr-core/security/vuln.rs:68` | `TaintSource` | DELETE | Replace with canonical `taint::TaintSource` |
| `tldr-core/security/vuln.rs:81` | `TaintSink` | DELETE | Replace with canonical `taint::TaintSink` |
| `tldr-core/security/vuln.rs:38` | `VulnType` | RETAIN as user-facing ontology | Already mapped to CLI VulnType via `map_core_vuln_type` (exhaustive match per VAL-002 issue #11) |
| `tldr-core/security/vuln.rs:94` | `VulnFinding` | RETAIN as output record | Constructed from canonical `TaintFlow` post-migration |
| `tldr-core/security/vuln.rs:115/126` | `VulnSummary, VulnReport` | RETAIN | Output shape consumed by CLI |
| `tldr-cli/remaining/vuln.rs:114` | CLI `TaintSource` const struct | DELETE | Replace with canonical AST source banks at `taint.rs:~1700` |
| `tldr-cli/remaining/vuln.rs:~210` | CLI `TaintSink` const struct + `PYTHON_SINKS` | DELETE | Replace with canonical AST sink banks |
| `tldr-cli/remaining/vuln.rs:335` | `TaintTracker` | DELETE | Replace with canonical `compute_taint_with_tree` invocation |
| `tldr-cli/remaining/vuln.rs:343` | CLI-local `TaintInfo` | DELETE | Replace with canonical `taint::TaintInfo` |

**TaintSinkType extension (Reframe B):**

Add 4 new variants to `taint::TaintSinkType` (`taint.rs:153`):
- `HtmlOutput` — XSS sink (innerHTML, raw(), Markup, html_safe, ...)
- `FileOpen` — Path-traversal sink (open, Path, File.read, fs.readFile, ...)
- `HttpRequest` — SSRF sink (requests.get, fetch, axios.get, http.Get, ...)
- `Deserialize` — Deserialization sink (pickle.load, yaml.load, readObject, unserialize, ...)

Plus AST sink-bank entries for those categories across all 16 `LanguagePatterns` (additive — mirrors the regex-removal-v1 / sanitizer-removal-v1 AST-bank precedent).

**Dispatch routing (handoff §2):**

Replace `tldr-core/security/vuln.rs:903 scan_file_vulns` (substring scanner) and `tldr-cli/remaining/vuln.rs:730 analyze_python_file` (CLI-local tree-sitter) with **per-function calls to `compute_taint_with_tree`** mirroring the proven pattern at `crates/tldr-cli/src/commands/taint.rs:128`:

```rust
let cfg = get_cfg_context(file, fn_name, language)?;
let dfg = get_dfg_context(file, fn_name, language)?;
let pool = ParserPool::new();
let tree = pool.parse(&source, language).ok();
let ssa = construct_minimal_ssa(&cfg, &dfg).ok();
let taint = compute_taint_with_tree(&cfg, &dfg.refs, &statements, tree.as_ref(), Some(source.as_bytes()), language, ssa.as_ref())?;
// taint.flows: Vec<TaintFlow> — one entry per source→sink path
// Project each TaintFlow into a VulnFinding via VulnType-from-TaintSinkType map.
```

Per-file flow becomes: walk file's functions → for each function, build CFG/DFG/SSA/tree → invoke `compute_taint_with_tree` → project `TaintFlow`s into `VulnFinding`s using a `TaintSinkType → VulnType` map.

**AST-aware match classification (handoff §3):**

The classification today is regex/substring-driven (`if line.contains("eval(")` → `CodeEval`). Post-migration: each `TaintFlow.sink.sink_type` is a typed `TaintSinkType` enum value, and `VulnType` is computed from it via a static map. Eliminates the comment/string-literal misclassification at the root.

### Out of scope (deferred or unaffected)

- **`analyze_rust_file`** — Rust line-scanner stays (Reframe C). UnsafeCode/MemorySafety/Panic remain its responsibility.
- **`tldr_core::security::vuln::scan_vulnerabilities` public API signature** — preserved for external library consumers. Internals are fully replaced with a `compute_taint_with_tree`-driven implementation.
- **CLI `VulnArgs` struct / clap flags** — preserved exactly. No CLI surface change.
- **JSON / SARIF output shape** — preserved exactly. `VulnReport.findings` schema unchanged.
- **Exit codes** — preserved (exit 2 on findings detected).
- **Python-only tree-sitter optimizations** specific to the CLI's local path (e.g., `is_string_interpolation_tainted` at `remaining/vuln.rs:1570`) — fold into canonical AST source detection or DELETE if obviated by canonical AST.
- **`patterns-shell-cleanup` followon** — the now-empty per-language source/sink Vec shells in `vuln.rs::get_sources`/`get_sinks` after migration are deferred to a future cleanup milestone (parallel to `sanitizer-removal-v1`'s preserved `LanguagePatterns` shells).

---

## §2 Sub-milestone List

### M1 — RED tests + investigation reports + TaintSinkType extension scaffold

**Depends:** []
**Atomic commit:** false
**LOC delta:** +280 (RED tests + 4 enum variants + AST-bank scaffolding)

**RED tests** (target file: `crates/tldr-cli/tests/vuln_migration_v1_red.rs`):

For each of the **14 substring-fall-through languages** (Go, Java, Ruby, C, C++, PHP, Kotlin, Swift, C#, Scala, Elixir, Lua, Luau, OCaml) and **TS/JS** plus **Python** (regression guard for the AST path):

- 6 positive fixtures (one per `VulnType`: SqlInjection, Xss, CommandInjection, PathTraversal, Ssrf, Deserialization) — **assert ≥1 finding** of the expected `VulnType` with the expected `taint_flow` source/sink lines.
- 6 string-literal regression-guard fixtures — string containing the source pattern + sink pattern in COMMENTS or string literals → **assert ZERO findings**.

Total: ~16 langs × 12 fixtures = **~190 tests**. Each fixture lives in `crates/tldr-cli/tests/fixtures/vuln_migration_v1/<lang>/<vuln_type>_{positive,string_literal_fp}.<ext>`.

**Pre-M2 RED capture:**
- All 96 string-literal regression tests EXPECTED FAIL at HEAD (FP class active).
- All 96 positive tests EXPECTED PASS at HEAD.
- Document per-language pass/fail in `reports/M1-red-capture.json`.

**TaintSinkType extension scaffold** (additive — green from M1):
- ADD 4 new variants to `taint::TaintSinkType` at `taint.rs:153`.
- ADD doc-comments for each new variant.
- ADD `pub use` exports through `security::mod.rs:24`.
- DO NOT yet wire AST detection for the new variants (M2 work).

**Deliverables:**
- `crates/tldr-cli/tests/vuln_migration_v1_red.rs` (~700 LOC).
- `crates/tldr-cli/tests/fixtures/vuln_migration_v1/` with ~190 fixture files.
- `taint.rs:153` extended with 4 variants (compile-clean, no behavior change yet).
- `reports/M1-red-capture.json` and `reports/M1-test-enumeration.json`.

**Stop thresholds:**
- All ~190 fixtures compile + new RED tests compile.
- `cargo check --workspace` passes.
- `cargo clippy --all-targets --workspace -- -D warnings` passes (TaintSinkType extension is complete + documented).
- RED capture: 14-lang × 6-vuln-types × 1 string-literal fixture = **84 tests EXPECTED FAIL deterministically** at HEAD; record per-test outcome.

### M2 — Extend AST sink banks for HtmlOutput / FileOpen / HttpRequest / Deserialize across 16 LanguagePatterns

**Depends:** [M1]
**Atomic commit:** false
**LOC delta:** +400 (AST sink-bank entries × 16 langs × ~4 patterns avg per category × 4 categories = ~256 entries; plus member_patterns helper extension)

**Anchors:**
- `taint.rs:~1700` — `PYTHON_AST_PATTERNS` and the 15 sibling `LanguagePatterns` constants.
- `taint.rs:~3700` — `detect_sinks_ast` walk-once dispatch in `compute_taint_with_tree`.

**Strategy:**
For each of the 16 `LanguagePatterns`, ADD AST sink-bank entries for the 4 new sink categories. Source-of-truth for the patterns: `tldr-core/src/security/vuln.rs:381–778` (the existing per-language sink tables for Xss/PathTraversal/Ssrf/Deserialization). Each existing `(pattern, description)` substring-tuple maps to an `AstSinkPattern` with appropriate `call_names` or `member_patterns`.

Example for Python `HttpRequest` (Ssrf):
```rust
AstSinkPattern { call_names: vec!["requests.get", "requests.post", "urlopen", "httpx.get", ...], sink_type: TaintSinkType::HttpRequest, ... }
```

**Deliverables:**
- `taint.rs` AST sink banks extended; M1 RED tests for the 4 new categories transition GREEN under the **regular `tldr taint` command** (not yet `tldr vuln`).
- `reports/M2-parity-audit.json` — per-language audit confirming each new variant has at least one AST sink pattern.

**Stop thresholds:**
- `cargo check --workspace` passes.
- `cargo clippy --all-targets --workspace -- -D warnings` passes.
- `tldr taint` smoke test on canonical fixtures: detects `HtmlOutput`/`FileOpen`/`HttpRequest`/`Deserialize` sinks correctly.
- All existing val001a/val001b/val002/val003/sanitize_breaks_flow_per_language/security_tests GREEN.

### M3 — Collapse `tldr-core/security/vuln.rs` onto canonical taint via `compute_taint_with_tree`

**Depends:** [M1, M2]
**Atomic commit:** false
**LOC delta:** −500 (delete `scan_file_vulns` substring scanner + `extract_assigned_variable` + `extract_propagation` + `is_type_coerced` + `is_sanitized_sink` + `is_sanitized_sql` + `has_named_param` + `is_sanitized_command` + `get_sources` + `get_sinks` + the now-redundant inline propagation; replace with a per-function `compute_taint_with_tree`-based scanner)

**Anchors:**
- `vuln.rs:903 scan_file_vulns` — the substring scanner. REPLACE with a per-function loop.
- `vuln.rs:140 get_sources` — DELETE (canonical AST source banks at `taint.rs:1700+` cover all 16 langs).
- `vuln.rs:290 get_sinks` — DELETE (canonical AST sink banks post-M2 cover all 6 vuln categories).
- `vuln.rs:1029-1280` — DELETE all the inline-propagation helpers (8 `extract_*` / `is_*` private functions).
- `vuln.rs:68 TaintSource`, `vuln.rs:81 TaintSink` — DELETE.
- `vuln.rs:783 get_remediation`, `vuln.rs:801 get_cwe_id` — RETAIN (output-record helpers).
- `vuln.rs:38 VulnType` — RETAIN.

**New `scan_file_vulns` body (sketch):**
```rust
fn scan_file_vulns(path: &Path, vuln_filter: Option<VulnType>) -> TldrResult<Vec<VulnFinding>> {
    let source = std::fs::read_to_string(path)?;
    let language = Language::from_path(path).ok_or(...)?;

    // Discover functions in file via codemap.
    let codemap = build_codemap(&source, language)?;

    let mut findings = Vec::new();
    let pool = ParserPool::new();
    let tree = pool.parse(&source, language).ok();

    for fn_def in codemap.functions {
        let cfg = get_cfg_context(path, &fn_def.name, language)?;
        let dfg = get_dfg_context(path, &fn_def.name, language)?;
        let ssa = construct_minimal_ssa(&cfg, &dfg).ok();
        let statements: HashMap<u32, String> = source.lines().enumerate()
            .filter(|(i, _)| within_fn_range(i, &fn_def))
            .map(|(i, line)| ((i + 1) as u32, line.to_string()))
            .collect();

        let taint = compute_taint_with_tree(&cfg, &dfg.refs, &statements, tree.as_ref(),
                                             Some(source.as_bytes()), language, ssa.as_ref())?;

        for flow in &taint.flows {
            let vuln_type = vuln_type_from_sink(flow.sink.sink_type);
            if let Some(filter) = vuln_filter { if vuln_type != filter { continue; } }
            findings.push(VulnFinding {
                vuln_type, file: path.to_path_buf(),
                source: flow.source.clone().into(),  // From<canonical::TaintSource> for output record (or inline projection)
                sink: flow.sink.clone().into(),
                flow_path: project_path(&flow.path, &cfg),
                severity: severity_for(vuln_type).to_string(),
                remediation: get_remediation(vuln_type).to_string(),
                cwe_id: Some(get_cwe_id(vuln_type).to_string()),
            });
        }
    }
    Ok(findings)
}
```

**`vuln_type_from_sink`** (the AST-aware classification — handoff §3):
```rust
fn vuln_type_from_sink(s: TaintSinkType) -> VulnType {
    match s {
        TaintSinkType::SqlQuery => VulnType::SqlInjection,
        TaintSinkType::ShellExec | TaintSinkType::CodeEval | TaintSinkType::CodeExec | TaintSinkType::CodeCompile => VulnType::CommandInjection,
        TaintSinkType::HtmlOutput => VulnType::Xss,
        TaintSinkType::FileOpen | TaintSinkType::FileWrite => VulnType::PathTraversal,
        TaintSinkType::HttpRequest => VulnType::Ssrf,
        TaintSinkType::Deserialize => VulnType::Deserialization,
    }
}
```

**Stop thresholds:**
- `cargo check --workspace` passes.
- `cargo clippy --all-targets --workspace -- -D warnings` passes.
- All ~190 M1 RED tests transition GREEN — both positive (vuln expected, found) AND string-literal regression (zero vuln expected, none reported).
- 14 of 14 fall-through languages pass FP-class regression: `tldr vuln <fixture> --lang <L>` returns 0 findings on string-literal fixtures.
- Pre-existing `test_e2e_*` E2E tests at `vuln.rs:1568–2100` ALL still GREEN — they are the primary regression guard.
- Performance: 20-file Go fixture runs in < 2× pre-migration wall-clock (baseline captured in M1).

### M4 — Collapse `tldr-cli/remaining/vuln.rs` Python path onto canonical taint

**Depends:** [M3]
**Atomic commit:** false
**LOC delta:** −650 (delete CLI's `TaintSource`/`TaintSink` const tables + `PYTHON_SOURCES`/`PYTHON_SINKS` + `TaintTracker` + `analyze_python_file` + 7 `analyze_*` helpers + 5 `is_*`/`find_*`/`vuln_type_name` helpers)

**Anchors:**
- `remaining/vuln.rs:114-327` — `TaintSource`/`TaintSink` structs + `PYTHON_SOURCES` (~30 entries) + `PYTHON_SINKS` (~25 entries). DELETE.
- `remaining/vuln.rs:335-390` — `TaintTracker`/`TaintInfo` structs + methods. DELETE.
- `remaining/vuln.rs:730-1500` — `analyze_python_file` + 7 recursive analysis helpers (`analyze_node`, `analyze_function`, `analyze_block`, `analyze_statement`, `analyze_assignment`, `analyze_augmented_assignment`, `analyze_expression`, `analyze_call`, `check_xss_return`). DELETE.
- `remaining/vuln.rs:1469-1700` — `is_taint_source`, `is_taint_sink`, `is_parameterized_query`, `is_string_interpolation_tainted`, `find_taint_in_string`. DELETE.
- `remaining/vuln.rs:644 analyze_file` — REWRITE: route `.py` through canonical `compute_taint_with_tree` directly (or simply drop the per-extension dispatch and let the M3-rewritten `scan_vulnerabilities` handle Python like every other language).

**New `analyze_file` body (post-M4):**
```rust
fn analyze_file(path: &Path) -> Result<Vec<VulnFinding>, RemainingError> {
    if matches!(path.extension().and_then(|e| e.to_str()), Some("rs")) {
        return Ok(analyze_rust_file(path, &fs::read_to_string(path)?));  // RETAINED
    }
    // ALL other languages (Python included post-M4) route through canonical scan_vulnerabilities,
    // which post-M3 is itself a thin wrapper over per-function compute_taint_with_tree.
    match tldr_core::security::vuln::scan_vulnerabilities(path, None, None) {
        Ok(report) => Ok(report.findings.into_iter().map(|f| project_to_cli_finding(f)).collect()),
        Err(_) => Ok(Vec::new()),
    }
}
```

**Stop thresholds:**
- `cargo check --workspace` passes.
- `cargo clippy --all-targets --workspace -- -D warnings` passes.
- All M1 RED tests still GREEN.
- All `vuln_autodetect_tests`, `vuln_ssrf_test`, `vuln_sarif_deserialization_test` GREEN.
- Python-specific E2E parity: pre-existing tests at `vuln.rs:1568-1672` (parameterized query, subprocess list, type coercion, real SQLi, real CmdI) — ALL still GREEN.
- The `test_string_interpolation_detection` regression-guard for f-string taint flows: replicate as a fixture in M1 RED tests; verify GREEN post-M4.

### M5 ATOMIC — Delete obsolete CLI unit tests + dispatch flip + dead-code sweep

**Depends:** [M4]
**Atomic commit:** TRUE
**Must ship in same commit:** YES (`release_commit_group: milestone_5_atomic`)
**LOC delta:** −250

**Deletions (atomic):**
- `remaining/vuln.rs:1844 test_is_taint_source` — bank-content assertion. DELETE.
- `remaining/vuln.rs:1862 test_is_taint_sink` — bank-content assertion. DELETE.
- `remaining/vuln.rs:1900 test_taint_tracker` — TaintTracker behavior. DELETE.
- `remaining/vuln.rs:test_string_interpolation_detection` — DELETE if covered by canonical (M4 verifies).
- `remaining/vuln.rs:test_vuln_type_cwe_mapping` — KEEP (cli VulnType cwe map, not migrated).
- `remaining/vuln.rs:test_vuln_type_severity` — KEEP.
- `vuln.rs:1322 test_get_sources_python`, `vuln.rs:1329 test_get_sinks_sql_injection`, `vuln.rs:1335`, `vuln.rs:1343`, `vuln.rs:2077` — ALL DELETE (assert non-empty regex Vec content; Vecs deleted in M3).
- `vuln.rs:1379 test_extract_propagation`, `vuln.rs:1392 test_extract_assigned_variable*` — DELETE (helpers deleted in M3).
- `vuln.rs:1399 test_type_coercion_*` — DELETE (`is_type_coerced` deleted in M3; type-coercion semantics now live in canonical sanitizer dispatch via `Numeric` SanitizerType from sanitizer-removal-v1).
- `vuln.rs:1497 test_sanitized_sql_*`, `test_sanitized_command_*`, `test_unsanitized_*` — DELETE (sanitizer detection now flows through canonical AST sanitizer dispatch already in place since sanitizer-removal-v1).

**Estimated obsolete-test count:** ~30 tests across `vuln.rs` inline + ~6 in `remaining/vuln.rs`. Exact list verified in M1 enumeration (`reports/M1-test-enumeration.json`) — gating for M5.

**Atomic commit reasoning:** the M3 deletion of `extract_assigned_variable`/`is_type_coerced` and the M3 introduction of new `scan_file_vulns` would leave dangling unit tests if not bundled. Atomic semantics ensure cargo test --workspace stays GREEN throughout.

**Stop thresholds:**
- `cargo check --workspace` PASS.
- `cargo clippy --all-targets --workspace -- -D warnings` PASS — NO `dead_code` warnings on now-unreachable helpers; NO unused imports.
- `cargo test --workspace` PASS — all M1 RED tests GREEN; all retained pre-existing tests GREEN.
- `tldr vuln /tmp/vuln-mig-repro/string_literal_fp.go --lang go` returns ZERO findings.
- `tldr vuln /tmp/vuln-mig-repro/fp2.ts --lang typescript` returns ZERO findings.

### M6 — CHANGELOG entry + local tag

**Depends:** [M5]
**Atomic commit:** false
**LOC delta:** +35

**Deliverables:**
- `CHANGELOG.md` entry (see §6 below).
- Local annotated tag `vuln-migration-v1` (no push, no publish, no version bump).
- `reports/M6-release-prep.md` — final summary of LOC delta, FP-closure verdict, TaintSinkType extensions added, performance regression results.

**Stop thresholds:**
- CHANGELOG entry merged.
- Local tag applied.
- NO push, NO publish, NO version bump.

---

## §3 Per-Vulnerability-Type Risk Matrix

| `VulnType` | Detection today (vuln.rs) | Canonical sink_type | M2 AST bank addition | Risk | Carry-forward? |
|---|---|---|---|---|---|
| SqlInjection | `(.execute(`, `.query(`, etc. — substring | `SqlQuery` (existing) | none — already in 16 banks | LOW | NO |
| Xss | `innerHTML`, `Markup(`, `mark_safe(`, etc. — substring | NONE → **add `HtmlOutput`** (M1+M2) | +9 langs (Python, JS, TS, Ruby, PHP, C#, Elixir, Lua, Luau) | MEDIUM — XSS is HTML-context-sensitive; AST patterns may miss Razor/JSX nuances | POSSIBLY — Razor `@Html.Raw(` is callable ALSO via the operator-prefix `@` shape — needs raw-fallback like sanitizer-removal-v1's PHP `(int)` cast |
| CommandInjection | `os.system(`, `exec.Command(`, etc. — substring | `ShellExec` + `CodeEval`/`CodeExec`/`CodeCompile` (existing) | none — already in 16 banks | LOW | NO |
| PathTraversal | `open(`, `os.path.join(`, `fopen(`, `Path(`, etc. — substring | `FileWrite` exists but is NARROW. **Add `FileOpen`** (M1+M2) | +16 langs | MEDIUM — `open()` in Python is overloaded (read AND write); `Path()` constructs paths but doesn't access them. Patterns must distinguish actual filesystem syscalls. | POSSIBLY — Java `new File(` is constructor not call; needs raw-fallback for the constructor-shape (canonical AST `extract_call_name` may emit different name) |
| Ssrf | `requests.get(`, `urlopen(`, `fetch(`, etc. — substring | NONE → **add `HttpRequest`** (M1+M2) | +16 langs | LOW | NO |
| Deserialization | `pickle.load(`, `unserialize(`, etc. — substring | NONE → **add `Deserialize`** (M1+M2) | +13 langs (matches vuln.rs's existing tables; TS/JS/Go/C/Swift/Lua/Luau have empty deser tables in vuln.rs — preserve that empty-set behavior) | LOW | NO |

---

## §4 Test Fixtures

### 4.1 Per-language × per-vuln-type fixtures

For each (language, vuln_type) pair where `vuln.rs::get_sinks(vuln_type, lang)` returns non-empty, write **2 fixtures**:

**Positive fixture:** real source-to-sink flow → assert ≥1 finding.

Example (`fixtures/vuln_migration_v1/go/sql_injection_positive.go`):
```go
package main

import "database/sql"
import "net/http"

func handler(w http.ResponseWriter, r *http.Request) {
    db, _ := sql.Open("postgres", "...")
    user_id := r.URL.Query().Get("id")  // source
    db.Query("SELECT * FROM users WHERE id = " + user_id)  // sink
}
```

**String-literal regression fixture:** source/sink names appear ONLY inside string literals or comments → assert ZERO findings.

Example (`fixtures/vuln_migration_v1/go/sql_injection_string_literal_fp.go`):
```go
package main

import "fmt"

func handler() {
    docs := "API: r.URL.Query().Get(\"id\") returns the user ID; pass to db.Query()"  // source pattern in string literal
    fmt.Println(docs)
    // also reference db.Query() in this comment line
    _ = docs
}
```

### 4.2 Coverage matrix

```
                Python  TS/JS  Go  Java  Rust  C  C++  Ruby  Kotlin  Swift  C#  Scala  PHP  Lua  Luau  Elixir  OCaml
SqlInjection      X      X    X    X     -    X   X    X     X       X     X    X     X    X    X     X       X
Xss               X      X    -    -     -    -   -    X     -       -     X    -     X    X    X     X       -
CommandInjection  X      X    X    X     X    X   X    X     X       X     X    X     X    X    X     X       X
PathTraversal     X      X    X    X     X    X   X    X     X       X     X    X     X    X    X     X       X
Ssrf              X      X    X    X     X    -   -    X     -       -     -    -     X    -    -     -       -
Deserialization   X      X    -    X     X    -   X    X     X       -     X    X     X    -    -     X       X
```
- `X` = positive + regression-guard fixture pair
- `-` = empty bank in vuln.rs (no fixture, no test)

Total fixture pairs: ~66 X-marks × 2 fixtures = **~132 fixture files**.
Plus the existing 30+ E2E tests at `vuln.rs:1568-2100` that act as the primary regression guard.

### 4.3 Composite FP fixture

`fixtures/vuln_migration_v1/composite/multi_pattern_fp.go` — single file with ALL 6 source-pattern strings inside string literals + ALL 6 sink-pattern strings inside comments → assert ZERO findings (the closes-#24 root pattern at file-scale).

---

## §5 Dispatch Wiring Spec

### 5.1 `scan_file_vulns` rewrite (M3)

**Old (vuln.rs:903):** linear two-pass per-line scan with substring matching + `extract_propagation` for ad-hoc taint chain.

**New (post-M3):**
```
1. Parse file with tree-sitter (single tree, reused across functions).
2. Build codemap → list of function definitions (name + line range).
3. For each function:
   a. cfg = get_cfg_context(...)
   b. dfg = get_dfg_context(...)
   c. ssa = construct_minimal_ssa(&cfg, &dfg).ok()
   d. statements = source lines scoped to function range
   e. taint = compute_taint_with_tree(&cfg, &dfg.refs, &statements, Some(&tree), Some(source_bytes), language, ssa.as_ref())?
   f. For each flow in taint.flows:
      - vuln_type = vuln_type_from_sink(flow.sink.sink_type)
      - apply vuln_filter
      - construct VulnFinding from flow + path projection
4. Aggregate findings, return.
```

### 5.2 `TaintFlow → VulnFinding` projection

The canonical `TaintFlow` has fields: `source: TaintSource`, `sink: TaintSink`, `path: Vec<usize>` (block IDs). The `VulnFinding` output record needs:

| `VulnFinding` field | Source |
|---|---|
| `vuln_type` | `vuln_type_from_sink(flow.sink.sink_type)` |
| `file` | known at scanner site |
| `source.variable` | `flow.source.var` |
| `source.source_type` | `format!("{:?}", flow.source.source_type)` (enum → human string for output compat) |
| `source.line` | `flow.source.line` |
| `source.expression` | `flow.source.statement.unwrap_or_default()` |
| `sink.function` | extract from `flow.sink.statement` (or store as new field) |
| `sink.sink_type` | `format!("{:?}", flow.sink.sink_type)` |
| `sink.line` | `flow.sink.line` |
| `sink.expression` | `flow.sink.statement.unwrap_or_default()` |
| `flow_path` | `flow.path.iter().map(|bid| format!("block-{}", bid)).collect()` (or richer projection from cfg block lines) |
| `severity` | `severity_for(vuln_type)` (preserve existing "HIGH" etc.) |
| `remediation` | `get_remediation(vuln_type).to_string()` (EXISTING helper at vuln.rs:783) |
| `cwe_id` | `Some(get_cwe_id(vuln_type).to_string())` (EXISTING helper at vuln.rs:801) |

**Output shape preserved exactly** — no JSON/SARIF schema break.

### 5.3 Per-file vs per-project semantics

`tldr vuln` is per-file (walker emits files; analyze_file produces findings; aggregated into VulnReport). `compute_taint_with_tree` is per-FUNCTION (CFG + DFG are function-scoped). The new `scan_file_vulns` adds an inner per-function loop. Tree is reused across functions in the same file (parsed once); CFG/DFG/SSA are constructed per function (existing `get_cfg_context`/`get_dfg_context` API).

**Performance implication:** function-scoped CFG construction is O(N_blocks * N_vars) per function. Files with many small functions may see overhead; files with few large functions may see speedup (early termination of the per-file substring loop is gone, but per-function early termination from CFG-bounded propagation kicks in). Mitigation: parallelize per-file with rayon (already a workspace dep). Document the wall-clock baseline in M1 and gate M3 on < 2× degradation.

---

## §6 CHANGELOG Draft

```markdown
## vuln-migration-v1 — internal milestone

### Changed
- `tldr vuln` command now routes through canonical `compute_taint_with_tree`
  for all 16 supported languages (was: per-language substring scanner in
  `tldr-core/security/vuln.rs` for 14 languages + CLI-local tree-sitter
  TaintTracker for Python). Eliminates the string-literal substring FP class
  (closes #24's `tldr vuln` half — the half left open by regex-removal-v1,
  field_access_info-extension-v1, and sanitizer-removal-v1, all of which only
  reached the `tldr taint` command path).

### Added
- `TaintSinkType::HtmlOutput` variant (XSS sink — innerHTML, raw(), Markup(), html_safe).
- `TaintSinkType::FileOpen` variant (path-traversal sink — open, Path, fopen, fs.readFile).
- `TaintSinkType::HttpRequest` variant (SSRF sink — requests.get, fetch, axios, http.Get).
- `TaintSinkType::Deserialize` variant (deserialization sink — pickle.load, yaml.load, readObject, unserialize).
- AST sink-bank entries for the 4 new sink_types across all 16 LanguagePatterns.

### Removed
- `tldr-core/security/vuln.rs::TaintSource` / `TaintSink` duplicate structs (collapsed onto canonical `taint::TaintSource` / `taint::TaintSink`).
- `tldr-cli/commands/remaining/vuln.rs::TaintSource` / `TaintSink` const-pattern structs + `PYTHON_SOURCES` (~30) / `PYTHON_SINKS` (~25) const tables.
- `tldr-cli/commands/remaining/vuln.rs::TaintTracker` + CLI-local `TaintInfo` struct + 5 propagation methods.
- `analyze_python_file` + 7 recursive analysis helpers + 5 is/find helpers in CLI vuln command (~700 LOC).
- `scan_file_vulns` substring scanner + `get_sources` / `get_sinks` per-language Vec tables + 8 inline-propagation helpers in core vuln.rs (~500 LOC).
- ~36 obsolete unit tests across `vuln.rs` inline + `remaining/vuln.rs` (bank-content assertions, taint-tracker behavior, type-coercion / sanitization helpers — all subsumed by canonical pipeline).

### Retained
- `VulnType` enum + `VulnFinding` / `VulnSummary` / `VulnReport` output records (user-facing ontology).
- `scan_vulnerabilities` public API (now thin wrapper over per-function `compute_taint_with_tree`).
- `get_remediation` / `get_cwe_id` helpers (output-record construction).
- `analyze_rust_file` line-scanner for Rust-idiomatic security smells (UnsafeCode/MemorySafety/Panic — distinct concern from taint flow).
- All existing E2E tests at `vuln.rs:1568-2100` (primary regression guard).
- All existing CLI integration tests (`vuln_autodetect_tests`, `vuln_ssrf_test`, `vuln_sarif_deserialization_test`).
- JSON / SARIF output shape (no consumer-facing schema change).
- Exit-code-2-on-findings behavior.

### Architectural note
- This is the FINAL internal milestone before external publish. Together with regex-removal-v1, field_access_info-extension-v1, and sanitizer-removal-v1, the canonical `tldr-core/security/taint.rs` is now the SINGLE SOURCE OF TRUTH for taint flow detection across both `tldr taint` and `tldr vuln`. The string-literal substring FP class (closes-#24-shaped) is eliminated end-to-end.
- After this milestone lands + binary verification confirms zero FPs across all fixtures, a single coherent external `cargo publish` closes #7, #23, #24, #27, #28 + tldr vuln FP class + sanitizer correctness in one release.
```

---

## §7 Atomic-commit Checklist

| Milestone | atomic_commit | Why |
|---|---|---|
| M1 | NO | RED tests + TaintSinkType extension are independently verifiable. |
| M2 | NO | AST sink-bank additions are additive — green from M1 onwards. |
| M3 | NO | core vuln.rs rewrite — large but each helper deletion + dispatch swap is independently green. |
| M4 | NO | CLI vuln.rs rewrite — independently green. |
| **M5** | **YES** | Obsolete-test deletion + dead-code sweep MUST be atomic. Without atomicity, cargo test/clippy fail intermittently. Mirrors sanitizer-removal-v1 M4 ATOMIC pattern. |
| M6 | NO | Doc-only. |

---

## §8 Premortem / Risk Register

| Risk | Severity | Mitigation |
|---|---|---|
| **R1 — `TaintSinkType` extension breaks external library consumers** | HIGH if not careful, LOW with mitigation | Variants are additive (no rename, no removal). All existing `match` exhaustiveness on `TaintSinkType` in `tldr_core` and `tldr_cli` becomes non-exhaustive at compile time → compiler errors immediately surface every site. M1 stop-threshold runs `cargo check --workspace` to enumerate ALL match sites (predicted: ~3 sites in `tldr-cli/commands/taint.rs` + `bugbot/l2/ir.rs` + tests). |
| **R2 — `compute_taint_with_tree` does not produce sufficient flow metadata for `VulnFinding` reconstruction** | MEDIUM | `TaintFlow` includes `source`, `sink`, `path`. The path is `Vec<usize>` (block IDs); current `flow_path: Vec<String>` in vuln.rs is hand-built source/sink line strings. Project block IDs to block-line-ranges via `cfg.blocks[bid].lines`. Verify in M3 that the projected output matches the existing E2E test expectations (test_e2e_real_sqli_still_detected etc.) |
| **R3 — Performance regression** | MEDIUM | Per-function CFG/DFG/SSA construction is heavier than substring scanning. Mitigation: M1 captures baseline wall-clock on a 20-file Go fixture; M3 stop-threshold gates on < 2× degradation. If exceeded: parallelize per-file with rayon (already a workspace dep). |
| **R4 — Tree parsing unavailable for some `--from-stdin`-style flow** | LOW | `tldr vuln` does NOT have a `--from-stdin` variant (verified: `VulnArgs.path: PathBuf` is required). All input reaches the scanner as a file path → tree always parseable. The fallback to non-tree mode at `taint.rs:3753` is preserved. |
| **R5 — Razor `@Html.Raw(` / Java `new File(` / Ruby `Rack::Utils.escape_html` shapes don't fit AST patterns cleanly** | MEDIUM | M2 parity audit verifies each new sink_type bank against the existing vuln.rs substring tables. Carry-forward exception pattern (sanitizer-removal-v1 retained Ruby `\bgets\b`) available for unanticipated shapes. |
| **R6 — `scan_vulnerabilities` external library callers depend on legacy `VulnFinding.source.source_type: String` free-form descriptions ("Flask request.args (GET parameters)" vs "HttpParam")** | MEDIUM | M3 preserves the descriptive string by formatting `TaintSourceType` enum + a per-language description map. The existing `get_sources` table at vuln.rs:140-286 holds the descriptions; retain it as a `descriptions_for(TaintSourceType, Language)` lookup even though the source-pattern Vec is deleted. |
| **R7 — Python path's `is_string_interpolation_tainted` (f-string detection) is more nuanced than canonical `extract_interpolated_vars`** | LOW-MEDIUM | M1 RED test covers f-string flow; M4 verifies parity. If gap surfaces, extend canonical `extract_interpolated_vars` (the same shape that handled it for `tldr taint`) rather than maintaining a CLI-local fork. |
| **R8 — Performance regression specifically from per-function CFG construction in many-small-function files** | MEDIUM | Codemap-based function discovery is fast. CFG+DFG+SSA are amortized over the function body. Per-file walker already iterates files; parallelism is per-file. If small-function regression surfaces in M1 baseline, batch CFG construction across functions in the same file (single tree-sitter walk → multi-CFG output). |
| **R9 — `analyze_rust_file` line-scanner sharing some signals with the taint-flow path causes contradictory findings** | LOW | analyze_rust_file emits `VulnType::UnsafeCode/MemorySafety/Panic`; the taint-flow path emits `VulnType::SqlInjection/Xss/CommandInjection/PathTraversal/Ssrf/Deserialization`. The two sets are DISJOINT. Audit confirmed: no overlap at HEAD. |
| **R10 — Premature M5 atomic commit before all M3+M4 deletions land — cargo test fails** | HIGH if not careful | M5's `depends: [M4]` and `atomic_commit: true` enforce sequencing. Worker-loop must verify M4 GREEN before staging M5. |

**Recommendation:** run discriminative premortem before /autonomous launch. The pattern caught real blockers in 2 of 2 prior milestones (sanitizer-removal-v1 reframed M3, regex-removal-v1 caught W3 carry-forward).

---

## §9 Carry-forward Exceptions

Anticipated carry-forwards (subject to M2 parity audit verdict):

1. **Razor `@Html.Raw(`** — the `@` operator-prefix shape may not parse as a normal call_expression in tree-sitter-c-sharp. If so, raw-substring fallback like sanitizer-removal-v1's PHP `(int)` precedent.
2. **Java `new File(`** — constructor-shape may not match `extract_call_name` output. Raw-fallback or extend extract_call_name_java.
3. **Ruby `<%= raw(` template-syntax** — embedded-language; raw-fallback.

For each, document in `reports/M2-parity-audit.json` and amend the CHANGELOG `### Retained` section in M6.

If M2 audit surfaces a gap that no AST or raw-fallback shape can address, mirror the field_access_info-extension-v1 Ruby `\bgets\b` carry-forward pattern: document in `reports/M2-carry-forward.json` + CHANGELOG.

---

## §10 Self-Validation

Validator mandates encoded into the dispatch contract:

- `taintsinktype_extension_additive_only` — M1 enum extension MUST NOT rename or remove existing variants.
- `compute_taint_with_tree_dispatch_required` — M3 + M4 MUST replace inline propagation with `compute_taint_with_tree` calls. Manual line-scanner residues are forbidden in the taint-flow path.
- `m5_atomic` — M5 obsolete-test deletion + dead-code sweep MUST ship in ONE commit.
- `e2e_test_preservation_mandatory` — All `test_e2e_*` E2E tests at `vuln.rs:1568-2100` MUST stay GREEN throughout. They are the user-facing behavior contract.
- `cli_output_shape_unchanged` — `tldr vuln --format json` and `--format sarif` outputs MUST match HEAD-c842962 schema byte-for-byte (modulo `taint_flow.path` projection — see §5.2).
- `rust_line_scanner_out_of_scope` — `analyze_rust_file` MUST NOT be touched. UnsafeCode/MemorySafety/Panic detection stays.
- `red_first_harness_required` — M1 RED capture MUST predate M2/M3 GREEN flips.
- `m1_test_enumeration_required` — M1 `reports/M1-test-enumeration.json` MUST list ALL obsolete tests (~36) with file + line numbers; gates M5.
- `performance_baseline_captured_m1` — M1 establishes wall-clock baseline on canonical 20-file Go fixture; M3 stop-threshold gates on < 2× degradation.
- `string_literal_fp_regression_mandatory` — All ~66 string-literal regression-guard fixtures MUST emit ZERO findings post-M3. NO carry-forward exceptions for the string-literal class.
- `public_api_unchanged_external` — `pub use` exports at `tldr-core/security/mod.rs` preserve same names; `scan_vulnerabilities` signature preserved; `tldr vuln` clap args preserved; JSON/SARIF schema preserved.
- `vuln_rs_internal_collapse_only` — `tldr-core/security/vuln.rs` may be reduced to a thin wrapper but MUST remain a module (don't delete the file, don't rename, don't remove from mod.rs).
- `closes_issues_verified_binary` — M5 stop-threshold runs `tldr vuln` against `/tmp/vuln-mig-repro/string_literal_fp.go` (--lang go) and `fp2.ts` (--lang typescript) and asserts ZERO findings on each.

---

## §11 /autonomous-readiness Assessment

**Verdict: READY with discriminative-premortem prerequisite.**

The plan is concrete: 6 milestones, atomic boundary clear (M5), test coverage plan complete (~190 RED tests + ~30 retained E2Es as primary regression guard), LOC delta modeled (~−1170 net), risk register populated (R1-R10), 3 prior-milestone precedents directly applicable (regex-removal-v1, field_access_info-extension-v1, sanitizer-removal-v1).

**Pre-/autonomous prerequisites:**

1. **Run discriminative premortem** — pattern caught real blockers in 2 of 2 prior milestones. Specific scenarios to test:
   - Does M3's per-function CFG construction trigger O(N²) regression on large files?
   - Does the `TaintSinkType` extension actually enumerate all caller match sites in M1's `cargo check`, or does some site use wildcard `_`?
   - Is the canonical `extract_interpolated_vars` truly equivalent to `is_string_interpolation_tainted` for Python f-strings, or are there shape differences?
   - Does Java `new File(` really not match `extract_call_name`? (could land before M2 if confirmed)
   - Does the FP-repro reproduce on EVERY 14 fall-through language, or only some? (Investigation only verified Go + TS.)

2. **Capture wall-clock baseline at HEAD** — create canonical 20-file Go fixture; record `tldr vuln` wall-clock for use as M3 stop-threshold gate.

3. **Verify external publish gate alignment** — confirm with maintainer that the post-vuln-migration-v1 release will close #7, #23, #24, #27, #28 + the tldr vuln FP class. If any of those issues need additional non-vuln-related work, surface BEFORE /autonomous launch.

**No conditions blocking /autonomous launch beyond the above prerequisites.** The plan is ready.

---

## Summary

This is a **6-milestone, ~1170-LOC-net-deletion** migration that closes the FINAL internal milestone before external publish. It is the third and final pass over the security pipeline:

- regex-removal-v1 → eliminated regex sources/sinks for `tldr taint` (13 langs).
- field_access_info-extension-v1 → eliminated regex sources/sinks for `tldr taint` (Ruby/Elixir/OCaml).
- sanitizer-removal-v1 → eliminated regex sanitizers for `tldr taint` (all 16 langs).
- **vuln-migration-v1 (this) → eliminates the parallel substring scanner and CLI-local TaintTracker for `tldr vuln`, routing it through canonical `compute_taint_with_tree`. Closes the closes-#24-shaped FP class for the vuln command path.**

After this lands + binary verification, a single coherent external `cargo publish` ships closing #7, #23, #24, #27, #28 + tldr vuln FP class + sanitizer correctness in one release.

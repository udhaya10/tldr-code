# cpp-deser-declaration-v1 — Plan

**Closes the LAST remaining carry-forward from vuln-source-parity-v1** —
`cpp_deserialization_positive` (1 of 3 Bucket B fixtures; Java + Scala
already closed by var-extract-nested-constructor-v1). Brings
`vuln_migration_v1_red` from 167/168 GREEN → 168/168 GREEN.

Predecessor: `var-extract-nested-constructor-v1` (commit `b577796`,
locally tagged). Predecessor explicitly deferred Cpp scope to this
follow-on per premortem amendment A1 (commit `88f5620`). HEAD at planning
time: `7a36df3`.

---

## §0 Investigation

**Empirical state at HEAD `7a36df3` (build verified):**
`tldr vuln <fixture> --lang cpp` returns `findings: []`. `tldr taint
<fixture> handler` returns `sources: [{var:"d", source_type:"env_var",
line:6}]`, `sinks: []`, `flows: []`. Source `d` (EnvVar via
`std::getenv`) is detected; **sink emits zero flows** because
var-extraction returns `None` for the matched descendant.

**Tree-sitter-cpp 0.23.4 parse of fixture L7**
(`boost::archive::text_iarchive ia(std::stringstream(d) >> obj);`),
empirically refuted in predecessor premortem (commit `88f5620`):

```
declaration
├── type:       qualified_identifier  "boost::archive::text_iarchive"
└── declarator: init_declarator
    ├── declarator: identifier "ia"
    └── value:      argument_list
        └── binary_expression       (operator: ">>")
            ├── left:  call_expression
            │   ├── function: qualified_identifier "std::stringstream"
            │   └── arguments: argument_list
            │       └── identifier "d"
            └── right: identifier "obj"
```

There is **NO `function_declarator`**. The `declaration` is the most-vexing-
parse declaration form, with the constructor call captured as `init_declarator`'s
`value` field — which is `argument_list` (not `arguments`).

**Why current sink dispatch finds the descendant but drops the var:**

1. `detect_sinks_ast` (taint.rs L4420) iterates all descendants. The
   `declaration` descendant's `text` contains `boost::archive::text_iarchive`.
2. `member_patterns_match` (taint.rs L3856) → raw-substring fallback at
   L3924-3928 fires (pattern `("", "boost::archive::text_iarchive")` from
   `CPP_AST_SINKS` L2655-2662).
3. Var-extraction is invoked on the `declaration` descendant:
   - `extract_first_identifier_arg_ast` (L3945): `child_by_field_name("arguments")`
     returns None; positional fallback scans direct children for
     `kind.contains("argument")` — `declaration`'s direct children are
     `qualified_identifier` (type) and `init_declarator` — neither match. Returns None.
   - `extract_assignment_rhs_ident` (L4248): scans line text for `=` —
     fixture line has no `=`. Returns None.
   - call_kinds-based synthetic-identifier (L4496-4507): `declaration` ∉
     `call_node_kinds(Cpp) = ["call_expression"]`. Returns None.
4. `var = None` → sink not emitted (L4509 `if let Some(var) = var`).

**CPP_AST_SINKS bank is correct** (L2655-2662 already lists
`boost::archive::text_iarchive` and `cereal::BinaryInputArchive`). No bank
extension needed. The gap is purely in **var-extraction shape coverage** for
Cpp `declaration → init_declarator → argument_list`.

**Decision (Option A — helper-level Cpp arm):** add a Cpp-specific arm to
`extract_first_identifier_arg_ast` that recognises `declaration`,
descends `declaration → init_declarator → argument_list`, then invokes
the existing `extract_first_identifier_arg_ast_descent` helper
(introduced by var-extract-nested-constructor-v1 at taint.rs L4180; depth
5; string-kind filter at every level) over the argument_list's named
children, with the descend-through set extended for Cpp to traverse
`binary_expression`, `call_expression`, `parenthesized_expression`.

Options B (sink-dispatch-level descend) and C (`declaration` →
`call_node_kinds(Cpp)`) rejected — see §3 design decision.

---

## §1 Bundle scope

**SINGLE source file edit:** `crates/tldr-core/src/security/taint.rs`.

Modifications confined to:

1. **`extract_first_identifier_arg_ast`** (L3945-4168): add a new Cpp arm
   placed BEFORE the generic `args` lookup at L4076 (mirrors the existing
   PHP/Ruby/OCaml arm placement). Arm body walks `declaration → init_declarator
   → argument_list` and delegates to `extract_first_identifier_arg_ast_descent`.
2. **`extract_first_identifier_arg_ast`** per-language descend-through
   set (L4117-4129, inside the OUTER helper, NOT inside the inner
   descent helper): extend it with a `Language::Cpp` branch covering
   `binary_expression`, `call_expression`, `parenthesized_expression`,
   `argument_list`. (Java/Scala arms unchanged.) **Note:** the inner
   descent helper `extract_first_identifier_arg_ast_descent` at L4180+
   performs unconditional BFS and has NO per-language extension point;
   the per-language descend-through set lives in the OUTER helper.
   Extending the outer descend_kinds match arm is COSMETIC for
   `cpp_deserialization_positive` (the new Cpp entry arm short-circuits
   before reaching the L4076 args-list lookup) but provides
   FORWARD-COVERAGE for future Cpp `call_expression`-rooted
   nested-constructor sinks that would reach the descend-through path.

**Out of scope (explicit):**
- NO changes to `CPP_AST_SINKS` (bank already correct).
- NO changes to `call_node_kinds()` (would over-broaden to sources/sanitizers/references).
- NO changes to `member_patterns_match` (sink matching already fires).
- NO changes to `extract_call_name_*`.
- NO public API change.
- NO new `VulnType` / `TaintSinkType` / `TaintSourceType` variants.
- NO new test fixtures authored — `cpp_deserialization_positive` (already
  RED at HEAD `7a36df3`) IS the contract.
- NO push, NO publish, NO version bump (USER STANDING RULE).

---

## §2 Sub-milestone list

### M1 — RED capture + investigation memo + design choice
- Run `cargo test -p tldr-cli --release --test vuln_migration_v1_red cpp_deserialization_positive` at HEAD `7a36df3` → expect FAIL. Capture stdout/stderr to `reports/M1-red-capture.txt`.
- Run `cargo test -p tldr-cli --release --test vuln_migration_v1_red` (full suite) → expect 167/168 GREEN. Capture summary in `reports/M1-green-baseline.txt`.
- Empirical re-confirmation of tree-sitter-cpp shape (already captured in `reports/investigation.json`); write `reports/M1-disposition.json` recording chosen Option A and rationale.
- LOC delta: 0 source.

### M2 — Implement Cpp arm + descend-through extension
- Add a new Cpp arm to `extract_first_identifier_arg_ast` at taint.rs L3945+, AFTER the OCaml `application_expression` arm (L4043) and BEFORE the generic `args` lookup at L4076. Arm body: when `language == Cpp && descendant.kind() == "declaration"`, walk named children for `init_declarator` → `child_by_field_name("value")` (must be `argument_list`) → invoke `extract_first_identifier_arg_ast_descent(value, source, language, 0)`. On miss, fall through to outer fallback chain so behaviour for non-matching `declaration` shapes is unchanged.
- Extend the per-language descend-through `descend_kinds` match arm INSIDE the OUTER `extract_first_identifier_arg_ast` (taint.rs L4117-L4129) with a `Language::Cpp` arm covering `binary_expression`, `call_expression`, `parenthesized_expression`, `argument_list`. Java/Scala arms unchanged. **NOTE:** the inner descent helper `extract_first_identifier_arg_ast_descent` at L4180+ does unconditional BFS and has NO per-language extension point — only the outer helper's descend_kinds set is extended. This is FORWARD-COVERAGE for future Cpp `call_expression`-rooted nested-constructor sinks; the new Cpp entry arm above short-circuits before L4076 for `cpp_deserialization_positive`, so the descend_kinds extension is not on the critical path for this fixture.
- Doc-comment block citing premortem `88f5620`, the cpp-deser-declaration-v1 anchor, and that `CPP_AST_SINKS` L2655-2662 is unchanged (the gap was var-extraction shape coverage, not bank entries).
- LOC delta: ~30-45 (arm body + per-language descend-through entry + doc).

### M3 — Verify + binary smoke + CHANGELOG + local tag
- `cargo test -p tldr-cli --release --test vuln_migration_v1_red --no-fail-fast` → expect 168/168 GREEN.
- `cargo test -p tldr-cli --release --test vuln_migration_v1_composite_red` → expect 1/1.
- `cargo test -p tldr-core --release --test rr_framework_integ_test` → expect 18/18.
- `cargo test -p tldr-core --release --lib security::vuln` → expect 36/36 `test_e2e_*`.
- `cargo test --workspace --release --no-fail-fast` → expect overall PASS.
- Binary smoke on **all 13 deserialization-specific** (or **84 total via glob**) `*/deserialization_string_literal_fp.*` fixtures: `for f in <fixtures>; do tldr vuln "$f"; done` → expect 0 findings each (closes-#24 regression-guard). Glob `crates/tldr-cli/tests/fixtures/vuln_migration_v1/*/deserialization_string_literal_fp.*` is authoritative.
- Binary smoke on `cpp/deserialization_positive.cpp` directly → expect ≥1 deserialization finding.
- CHANGELOG entry (see §5).
- Local tag `cpp-deser-declaration-v1`.
- LOC delta: ~25 (CHANGELOG only).

---

## §3 Design decision

**Option A chosen: helper-level Cpp arm in `extract_first_identifier_arg_ast`.**

| Option | Description | Verdict |
|---|---|---|
| **A** | Add Cpp-specific arm in `extract_first_identifier_arg_ast` walking `declaration → init_declarator → argument_list`, then delegate to existing `extract_first_identifier_arg_ast_descent` with extended Cpp descend-through set. | **CHOSEN** |
| B | Add `declaration` to a per-language "constructor-call wrapper" list in `detect_sinks_ast`'s dispatch loop; recursively re-dispatch sink matching on inner descendants. | REJECTED — modifies sink-dispatch core; affects ALL Cpp sink patterns, broader blast radius. |
| C | Add `declaration` to `call_node_kinds(Cpp)`. | REJECTED — over-broad. `call_node_kinds` is consumed by sources / sanitizers / `references.rs::is_call`; expanding it changes semantics across all those paths. |

**Why A is correct (re-evaluating predecessor's premortem A1):**

The predecessor's premortem (commit `88f5620`) noted that the existing helper's `child_by_field_name("arguments")` and positional `kind.contains("argument")` fallback do not reach the argument_list under a `declaration`. **TRUE OF THE EXISTING HELPER.** The fix is to **add a Cpp arm** that knows the cpp shape, exactly as the existing helper has arms for PHP echo_statement (L3954-3982), Ruby subshell (L4013-4036), OCaml application_expression (L4043-4070). All three are language-specific entries at the top of the helper. The Cpp `declaration` arm is the same shape of extension.

The descent helper itself (`extract_first_identifier_arg_ast_descent` at L4180+) is reused unchanged — its body performs unconditional BFS and has no per-language extension point. The per-language `descend_kinds` set that DOES gate descent lives in the OUTER helper at L4117-4129, and that is where the new Cpp entry is added. Cpp's case requires `binary_expression` (because `>>` between the two arg expressions is a binary_expression node), `call_expression` (for the inner `std::stringstream(d)`), `parenthesized_expression` (for any wrapping parens), and `argument_list` (the entry shape itself).

**Var-extraction trace under Option A:** entry arm fires on
`declaration` → walks `named_child = init_declarator` →
`child_by_field_name("value") = argument_list` → invokes descent helper
→ descent traverses `binary_expression` (in descend-through set) → left:
`call_expression(std::stringstream)` (in set) → arguments[0] =
`identifier("d")` → `is_valid_identifier("d")` → returns `Some("d")` ✓.

---

## §4 Risk register (top 5)

| ID | Tier | Description | Mitigation |
|---|---|---|---|
| **R1** | TIGER | Cpp arm fires on UNRELATED `declaration` nodes (e.g., `int x = foo(d);`) and surfaces false positives across pre-existing GREEN cpp fixtures. | Arm is invoked ONLY when var-extraction is invoked, which itself only fires when `member_patterns_match` matched a sink pattern. The arm matches the structural chain `declaration → init_declarator(value=argument_list)`. For sinks: arm runs ONLY for descendants already filtered to sink-pattern matches. M3 GREEN-regression sweep across all 167 currently-GREEN tests is gating. |
| **R2** | TIGER | `extract_first_identifier_arg_ast_descent` Cpp descend-through additions (`binary_expression`, etc.) regress var-extraction for Java/Scala or other-Cpp call_expression sinks (descent now traverses through wrong shapes). | Per-language descend-through set is explicitly keyed on `Language::Cpp`. Java/Scala/other-language descent paths unchanged. M3 sweep gating. |
| **R3** | TIGER | string-literal regression-guard regresses (closes-#24): a `declaration` with `init_declarator(value=argument_list)` where the inner identifier is inside a string literal. | `extract_first_identifier_arg_ast_descent` (taint.rs L4180+) ALREADY applies `string_node_kinds(language)` filter at every recursion level — unchanged. Cpp arm reuses that helper. M3 binary smoke on all 13 deserialization-specific (or 84 total via glob) `*_string_literal_fp` fixtures (including `cpp/deserialization_string_literal_fp.cpp`) is gating. |
| **R4** | ELEPHANT | Tree-sitter-cpp grammar version drift between HEAD's pinned 0.23.4 and a future bump renames `init_declarator` or `argument_list`. | Helper comment cites pinned grammar version; if grammar changes, arm fails closed (returns None) — not regress, fixture re-RED would surface in CI. Tunable. |
| **R5** | ELEPHANT | `init_declarator` shape with a chain like `T x(...)` where value is a single nested constructor call (no binary_expression) — descent works but doesn't surface a different shape. | Descend-through set covers `call_expression` directly, so single-constructor case is also handled. No additional risk. |

---

## §5 CHANGELOG draft

```markdown
## cpp-deser-declaration-v1 — internal milestone

### Changed

- `extract_first_identifier_arg_ast` (`crates/tldr-core/src/security/taint.rs`): added Cpp-specific arm handling tree-sitter-cpp's `declaration → init_declarator(value=argument_list)` shape (the C++ "most-vexing-parse" declaration form used by `boost::archive::text_iarchive ia(...);` and similar direct-init constructors). Arm walks the named-child chain to reach the inner argument_list, then delegates to `extract_first_identifier_arg_ast_descent` (depth 5; string-kind filter at every level).
- `extract_first_identifier_arg_ast_descent`: extended per-language descend-through set with a `Language::Cpp` branch covering `binary_expression`, `call_expression`, `parenthesized_expression`, `argument_list` so the descent reaches the leftmost-source-order identifier inside expressions like `std::stringstream(d) >> obj`.

### Closed carry-forward

- `cpp_deserialization_positive` (vuln-source-parity-v1 M5 Bucket B partial; deferred from var-extract-nested-constructor-v1 per premortem amendment A1). `vuln_migration_v1_red` red-count drops from 1 (167/168 GREEN) to 0 (168/168 GREEN). Closes the LAST remaining vuln-source-parity-v1 carry-forward.

### Retained

- closes-#24 string-literal regression-guard preserved at every descent level (`string_node_kinds(Cpp)` filter applied recursively).
- `CPP_AST_SINKS` bank unchanged — `boost::archive::text_iarchive` and `cereal::BinaryInputArchive` entries already correct; the gap was purely in var-extraction shape coverage, not sink-pattern matching.
- All 167 currently-GREEN tests in `vuln_migration_v1_red` remain GREEN.
- `vuln_migration_v1_composite_red` 1/1, `rr_framework_integ_test` 18/18, `test_e2e_in_security_vuln` 36/36 remain GREEN.

### Architectural note

NO public API change. NO new `VulnType` / `TaintSinkType` / `TaintSourceType` variants. NO bank entry additions. Helper extension only — new Cpp arm mirrors existing PHP echo_statement (L3954-3982), Ruby subshell (L4013-4036), and OCaml application_expression (L4043-4070) language-specific arms. NO push, NO publish, NO version bump (local tag `cpp-deser-declaration-v1`).
```

---

## §6 Self-validation

- [x] Plan size 150–250 lines (this file).
- [x] 3 milestones (M1 investigate + RED capture; M2 implement; M3 verify + CHANGELOG + tag).
- [x] §0 Investigation: empirical taint-output capture from HEAD-built binary, tree-sitter parse confirmed.
- [x] §1 Bundle scope: single source file, explicit out-of-scope list including USER STANDING RULES (no push, no publish, no version bump).
- [x] §2 Sub-milestone list with stop-thresholds, LOC deltas, gating tests.
- [x] §3 Design decision: 3 options compared (A/B/C), Option A chosen with var-extraction trace.
- [x] §4 Risk register: 3 TIGER (R1 false positives, R2 cross-language descent regression, R3 closes-#24) + 2 ELEPHANT (R4 grammar drift, R5 chain shape).
- [x] §5 CHANGELOG draft: matches predecessor (`var-extract-nested-constructor-v1`, `ruby-backtick-extraction-v1`, `rust-vuln-taint-pipeline-v1`) section structure.
- [x] §6 Self-validation: this list.
- [x] Validator mandates surfaced: no_public_api_change; no_new_enum_variants; additive_or_minimal_change (single file, single helper region); m2_atomic_if_multi_file (M2 is single-file by design); no_push_no_publish (USER STANDING RULE); preserve_existing_cpp_tests_green (M3 GREEN-regression sweep gating).
- [x] Predecessor inheritance documented: var-extract-nested-constructor-v1's `extract_first_identifier_arg_ast_descent` is the reused descent primitive; this milestone adds ONE per-language descend-through entry and ONE language-specific entry arm.

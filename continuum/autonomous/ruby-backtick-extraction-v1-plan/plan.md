# ruby-backtick-extraction-v1 — Plan

## Status

- **Wave**: smallest of three follow-on milestones from `vuln-source-parity-v1` M5 carry-forward.
  Sibling milestones: `var-extract-nested-constructor-v1` (Bucket B, ~30-60 LOC),
  `rust-vuln-taint-pipeline-v1` (Bucket A Rust subset, design milestone).
- **Closes**: 1 carry-forward from `vuln-source-parity-v1` M5 Bucket A Ruby subset
  (`ruby_command_injection_positive`).
- **Pre-state HEAD**: `997557b` (rust-vuln-taint-pipeline-v1 plan landed).
  Working tree CLEAN with respect to source code; non-source modifications in
  `continuum/autonomous/` predate this loop and are not staged by this plan.
- **Empirical RED count at HEAD**: 8 of 166 RED in `vuln_migration_v1_red`
  (158 GREEN). This milestone closes 1 of those 8.
- **Estimated LOC**: ~10-20 (taint.rs only; no ast_utils.rs change; no public API change).

---

## §0 Investigation summary

### Tree-sitter-ruby subshell node-kind audit (verified)

`tree-sitter-ruby 0.23.1`
(`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tree-sitter-ruby-0.23.1/src/node-types.json`):

```json
{
  "type": "subshell",
  "named": true,
  "fields": {},
  "children": {
    "multiple": true,
    "required": false,
    "types": [
      { "type": "escape_sequence", "named": true },
      { "type": "interpolation", "named": true },
      { "type": "string_content", "named": true }
    ]
  }
}
```

Both lexical forms collapse onto this single node kind:
- Backtick form: `` `cmd` ``
- Percent-x form: `%x{cmd}`, `%x[cmd]`, `%x(cmd)`

There is **no separate `%x` node kind** in the grammar — both forms produce a
`subshell` named-node containing `interpolation` / `string_content` /
`escape_sequence` children. **Implication: a single dispatch addition closes
both forms.** The existing carry-forward fixture exercises the backtick form;
M2 adds a `%x{...}` regression-guard fixture to lock the extra coverage in.

### Why current dispatch misses subshell

1. `call_node_kinds(Ruby) = ["call", "method_call"]` at
   `crates/tldr-core/src/security/ast_utils.rs:18-36`. `subshell` is not in
   the slice.
2. `extract_call_name_ruby` at `crates/tldr-core/src/security/ast_utils.rs:707-728`
   matches only `"call" | "method_call"` and returns `None` for any other
   kind.
3. The detect_sinks_ast call_names path at `crates/tldr-core/src/security/taint.rs:4276-4282`
   gates on `call_kinds.contains(&descendant.kind())` AND `extract_call_name(...).is_some()`.
   For a `subshell` node both gates fail.
4. The W2-pre call-shape path inside `member_patterns_match` at
   `taint.rs:3886-3906` has the same gate. Same outcome.
5. The raw-substring fallback at `taint.rs:3913-3917` would only fire if a
   `member_patterns` entry like `("", "`")` existed AND the subshell's
   `descendant_text` contained the pattern. No such pattern exists; adding one
   keyed on a single backtick character is high FP risk and not pursued.

### `is_in_string` filter safety (verified)

`string_node_kinds(Ruby) = ["string", "string_content"]` at `ast_utils.rs:48`.
`subshell` is **not** a string kind. `is_in_string` walking up from the
subshell node returns `false`. The interpolation's inner identifier
(`cmd` for `` `#{cmd}` ``) walks up: `identifier` → `interpolation` → `subshell`
→ `method` block → … — no ancestor is a string kind. **The descendants-loop's
`is_in_string` skip will NOT filter the subshell node nor an inner identifier
inside `#{...}`.**

### `extract_first_identifier_arg_ast` audit

`taint.rs:3934-3982` has language-specific arms (PHP echo / OCaml application_expression)
followed by a generic body that walks named children seeking the first
identifier. For a `subshell` containing `#{cmd}`, the walk yields:
`subshell` → `interpolation` → `identifier(cmd)`. The generic body picks up
the identifier. **Empirically verified by reading the body and the
descendants walk pattern** (no language-specific arm needed for subshell —
the generic path suffices).

If for some reason the generic body fails on subshell (M1 will verify), M2
extends it with a Ruby-specific arm at the same site (still localized to
`taint.rs`, no ast_utils.rs cross-cutting change). Tracked as a contingency
in §7 R3.

### FP fixture safety

`crates/tldr-cli/tests/fixtures/vuln_migration_v1/ruby/command_injection_string_literal_fp.rb`
mentions the WORD `backtick cmd` inside a string literal but contains **no
real `` `…` `` subshell**. The descendants-walk over its parsed AST yields
zero `subshell` nodes → **zero FPs introduced by this milestone**.

---

## §1 Bundle scope

### Binary-verifiable success criteria

```bash
# RED → GREEN
cargo test -p tldr-cli --release --test vuln_migration_v1_red ruby_command_injection_positive

# String-literal FP regression-guard stays GREEN
cargo test -p tldr-cli --release --test vuln_migration_v1_red ruby_command_injection_string_literal_fp

# Full vuln_migration_v1_red suite: 159/166 GREEN (was 158/166)
cargo test -p tldr-cli --release --test vuln_migration_v1_red

# Workspace-wide regression: no test that depended on RUBY_AST_SINKS shape
# breaks (rr_baseline_per_language_test, rr_module_function_integ_test, etc.)
cargo test --workspace

# Binary smoke
tldr vuln crates/tldr-cli/tests/fixtures/vuln_migration_v1/ruby/command_injection_positive.rb \
  --lang ruby --format json
# Expected: ≥1 finding of type "command_injection"
```

### Chosen design — Option B (dedicated subshell dispatch arm)

Add a localized dispatch arm in `detect_sinks_ast`
(`taint.rs:4256-4360`) that fires when:

```
descendant.kind() == "subshell" AND language == Language::Ruby
```

producing a synthetic `TaintSink { sink_type: ShellExec, var: <inner-ident>, ... }`.
Var-extraction reuses the existing `extract_first_identifier_arg_ast` helper
(generic descend-and-find-identifier path).

**No change to `call_node_kinds(Ruby)`.** No change to `extract_call_name_ruby`.
No change to any AST bank's `call_names` / `member_patterns` (no new
`AstSinkPattern` entry). The dispatch arm is the smallest possible patch.

### RUBY_AST_SINKS entry — NOT REQUIRED

Reasoning: the existing `RUBY_AST_SINKS` (`taint.rs:2758-2843`) keys all
entries on `call_names` / `member_patterns` paths inside the
`for pattern in patterns.sinks { … }` loop. Subshell does not match any of
those structural shapes (no `extract_call_name` to compare against
`call_names`; no `(receiver, field)` for `member_patterns_match`). Adding a
bank entry would be **silently dead** (matched by no code path).

The dispatch arm in §5 IS the wire — it produces a `TaintSink` directly,
bypassing the `for pattern in patterns.sinks` loop for this specific shape.
Documented inline as a comment explaining "subshell is not call-shaped, so no
AstSinkPattern entry exists; this arm is the entire matcher".

### Out of scope

- `extract_call_name_ruby` extension to handle `subshell` — rejected (Option A+
  side-effect surface, see investigation.json `design_decision`).
- New AST bank entry in `RUBY_AST_SINKS` — rejected (would be dead code; see
  above).
- `%x{...}` parsing as a different kind — verified to produce `subshell`
  (single dispatch addition closes both forms).
- Heredoc + backtick combinations — no carry-forward test demands it.
- Ruby `Kernel#` `system_under_subshell` — no carry-forward test demands it.
- Public API extensions — none.
- New `TaintSourceType` / `TaintSinkType` / `VulnType` variants — none.
- Cross-cutting changes to `ast_utils.rs` — none (this milestone touches
  ONLY `taint.rs`).

### Why this milestone

- Closes the LAST Ruby-specific carry-forward from `vuln-source-parity-v1` M5.
- Demonstrates that AST-only dispatch can handle non-call-shaped sinks
  (subshell) via a localized dispatch arm without polluting cross-cutting
  helpers (`call_node_kinds`, `extract_call_name`).
- Sets a precedent for any future non-call-shaped sink (e.g., Ruby `eval`
  in heredoc form, JavaScript template literals as sinks, OCaml `Format.printf`
  as a non-call sink) — same Option-B-style localized arm.

---

## §2 Sub-milestone list

### Wave structure

```mermaid
graph TD
  M1[M1: RED capture + node-kind verification + helper audit] --> M2
  M2[M2: ATOMIC — Implement subshell dispatch arm + %x{} fixture] --> M3
  M3[M3: Verification + CHANGELOG + local tag]
```

**SERIALIZED.** All three milestones edit `crates/tldr-core/src/security/taint.rs`
or `crates/tldr-cli/tests/fixtures/vuln_migration_v1/ruby/`. No parallelism
opportunity — milestone is too small.

### M1: RED capture + node-kind verification + helper audit

- **depends**: []
- **atomic_commit**: false
- **red_tests**:
  - `crates/tldr-cli/tests/vuln_migration_v1_red.rs::ruby_command_injection_positive`
    (already-RED at HEAD `997557b`; M1 captures the failure to prove the
    pipeline misses subshell.)
- **green_files**:
  - `continuum/autonomous/ruby-backtick-extraction-v1-plan/reports/M1-red-capture.txt`
    — output of `cargo test -p tldr-cli --release --test vuln_migration_v1_red ruby_command_injection_positive`
    showing RED.
  - `continuum/autonomous/ruby-backtick-extraction-v1-plan/reports/M1-node-audit.json`
    — empirical verification (via tree-sitter-ruby parser exercised in a
    one-off Rust test or script) that:
    1. `` `#{cmd}` `` parses as `subshell` node containing `interpolation`
       containing `identifier(cmd)`.
    2. `%x{ls #{x}}` parses as `subshell` (same kind).
    3. `extract_first_identifier_arg_ast(subshell, source, Ruby)` returns
       `Some("cmd")` for the backtick fixture's subshell. If it returns
       `None`, M2 must extend the helper.
- **loc_delta**: 0 source LOC; ~80 LOC in reports/.
- **stop_thresholds**:
  - `cargo test -p tldr-cli --release --test vuln_migration_v1_red ruby_command_injection_positive`
    REPORTS RED at HEAD (pre-M2).
  - Node-audit JSON empirically confirms `subshell` kind for both backtick
    and `%x{...}` shapes.
  - extract_first_identifier_arg_ast verdict (empirical) recorded — either
    "GREEN — generic path returns Some(\"cmd\")" or "RED — generic path returns
    None; M2 must extend".
  - Working tree clean except for `continuum/autonomous/ruby-backtick-extraction-v1-plan/reports/`.

### M2: ATOMIC — Implement subshell dispatch arm + %x{} fixture

- **depends**: [M1]
- **atomic_commit**: true
- **must_ship_in_same_release_commit**: true
- **release_commit_group**: `milestone_2_atomic`
- **red_tests**:
  - `ruby_command_injection_positive` — RED → GREEN.
  - `ruby_command_injection_string_literal_fp` — REMAINS GREEN (regression-guard).
  - NEW: `ruby_command_injection_percent_x_positive` — fixture
    `crates/tldr-cli/tests/fixtures/vuln_migration_v1/ruby/command_injection_percent_x_positive.rb`
    using `%x{...}` shape; expected GREEN post-M2 (locks %x{} coverage).
  - NEW: `ruby_command_injection_percent_x_string_literal_fp` — fixture
    using the WORD `%x{cmd}` inside a string; expected GREEN regression-guard
    (zero findings).
- **green_files**:
  - `crates/tldr-core/src/security/taint.rs` (~10-20 LOC).
    - **Anchor**: `detect_sinks_ast` at `taint.rs:4256-4360` (the
      `for descendant in &descendants` loop, immediately AFTER the
      `for pattern in patterns.sinks { … }` block at L4275-L4374).
    - **Addition**: dedicated dispatch arm. Pseudo-code:
      ```rust
      // Ruby backtick / %x{} subshell dispatch
      // (ruby-backtick-extraction-v1 — see plan.md §5).
      //
      // tree-sitter-ruby parses `…` and %x{…} as a `subshell` named-node
      // containing `interpolation` / `string_content` / `escape_sequence`
      // children. It is NOT call-shaped, so call_names/member_patterns
      // structural matches in the loop above cannot fire. Direct arm:
      if language == Language::Ruby && descendant.kind() == "subshell" {
          let stmt_text = std::str::from_utf8(source)
              .unwrap_or("")
              .lines()
              .nth((line - 1) as usize)
              .unwrap_or("");
          if let Some(var) = extract_first_identifier_arg_ast(descendant, source, language)
              .or_else(|| extract_assignment_rhs_ident(descendant, source, stmt_text))
              .or_else(|| extract_source_var_from_statement(stmt_text))
          {
              sinks.push(TaintSink {
                  var,
                  line,
                  sink_type: TaintSinkType::ShellExec,
                  statement: Some(stmt_text.to_string()),
              });
              continue; // Only one sink per node
          }
      }
      ```
    - **Comment block** (mandatory) explaining:
      (a) why the arm exists outside `for pattern in patterns.sinks`,
      (b) the carry-forward source from `vuln-source-parity-v1 M5
          carry-forward Bucket A Ruby`,
      (c) the FAI-v1 `\bgets\b` precedent (cross-reference),
      (d) that `extract_call_name_ruby` and `call_node_kinds(Ruby)` are
          UNCHANGED (Option A rejected — see investigation.json).
  - `crates/tldr-cli/tests/fixtures/vuln_migration_v1/ruby/command_injection_percent_x_positive.rb`
    — NEW fixture; ~6 lines:
    ```ruby
    require 'net/http'
    class DemoController
      def handler(params)
        cmd = params[:cmd]
        %x{#{cmd}}
      end
    end
    ```
  - `crates/tldr-cli/tests/fixtures/vuln_migration_v1/ruby/command_injection_percent_x_string_literal_fp.rb`
    — NEW fixture (FP regression-guard); ~6 lines containing a STRING that
    *mentions* `%x{cmd}`:
    ```ruby
    class DocsOnly
      def docs
        s = "use %x{cmd} for inline shell"
        s
      end
    end
    ```
  - `crates/tldr-cli/tests/vuln_migration_v1_red.rs` — add 2 test functions
    (mirror existing `ruby_command_injection_*` patterns).
- **loc_delta**: source ~12-20 LOC + 2 fixtures (~12 LOC each) + 2 tests (~10 LOC each).
  Total ~60 LOC.
- **stop_thresholds**:
  - `cargo check --workspace` PASS.
  - `cargo clippy --all-targets --workspace -- -D warnings` PASS.
  - `cargo test -p tldr-cli --release --test vuln_migration_v1_red ruby_command_injection_positive` GREEN.
  - `cargo test -p tldr-cli --release --test vuln_migration_v1_red ruby_command_injection_string_literal_fp` GREEN.
  - `cargo test -p tldr-cli --release --test vuln_migration_v1_red ruby_command_injection_percent_x_positive` GREEN.
  - `cargo test -p tldr-cli --release --test vuln_migration_v1_red ruby_command_injection_percent_x_string_literal_fp` GREEN.
  - `cargo test -p tldr-cli --release --test vuln_migration_v1_red` reports
    159/166 GREEN (improvement from 158/166; new percent_x tests are NET-NEW
    so the denominator becomes 168 → 161/168 GREEN).
  - `cargo test --workspace` no regressions.
- **rollback_rule**: If any post-commit assertion fails, REVERT the M2 atomic
  commit and re-investigate. NO partial-fix follow-up commits.

### M3: Verification + CHANGELOG + local tag

- **depends**: [M2]
- **atomic_commit**: false
- **red_tests**: []
- **green_files**:
  - `CHANGELOG.md` — add entry per `vuln-source-parity-v1 M6` precedent
    (see §6 below).
  - `continuum/autonomous/ruby-backtick-extraction-v1-plan/reports/M3-binary-smoke.json` —
    `tldr vuln <fixture> --lang ruby --format json` output for both backtick
    and `%x{...}` fixtures, asserting ≥1 finding of type `command_injection`.
  - `continuum/autonomous/ruby-backtick-extraction-v1-plan/reports/M3-final-report.json` —
    summary: `vuln_migration_v1_red` count delta, workspace test count, LOC
    delta from `git diff HEAD~N --stat` (where N = M2 commit count).
  - Local annotated tag `ruby-backtick-extraction-v1`.
- **loc_delta**: ~25 LOC in CHANGELOG + reports.
- **stop_thresholds**:
  - CHANGELOG entry merged.
  - Binary smoke produces ≥1 `command_injection` finding for both fixtures.
  - Local annotated tag `ruby-backtick-extraction-v1` applied.
  - **NO push, NO publish, NO version bump** (per release_constraints).

---

## §3 Per-form analysis

| Form              | Tree-sitter kind | Children                                  | Closed by this milestone? |
|-------------------|------------------|-------------------------------------------|---------------------------|
| `` `cmd` ``        | `subshell`       | `string_content`                          | Yes (no interpolation case — `extract_first_identifier_arg_ast` returns None → no sink emitted; pure-static command, NOT a tainted-flow case anyway) |
| `` `#{x}` ``       | `subshell`       | `interpolation` containing identifier `x` | **YES — primary closure target** |
| `%x{cmd}`          | `subshell`       | `string_content`                          | Yes (no interpolation — same as above) |
| `%x{#{x}}`         | `subshell`       | `interpolation` containing identifier `x` | **YES — secondary closure target** |
| `%x[cmd]`          | `subshell`       | `string_content`                          | Yes (same kind) |
| `%x(cmd)`          | `subshell`       | `string_content`                          | Yes (same kind) |
| `%x{}` heredoc combo | n/a (separate `heredoc_body` ancestry; subshell still inside it) | — | Out of scope; no carry-forward test demands. |

The key invariant: tree-sitter-ruby 0.23.1 collapses ALL six lexical forms
above onto the single `subshell` named-node kind. A single dispatch arm
keyed on `descendant.kind() == "subshell"` covers all six. M2's `%x{...}`
fixture provides a regression-guard for the secondary closure target.

---

## §4 Test fixtures

### Existing (RED at HEAD)

| File | Lines | Shape | Status pre-M2 | Status post-M2 |
|------|-------|-------|---------------|----------------|
| `ruby/command_injection_positive.rb` | 9 | `` `#{cmd}` `` | RED | **GREEN** |
| `ruby/command_injection_string_literal_fp.rb` | 11 | `"… backtick cmd …"` (string only) | GREEN (no subshell node) | **GREEN** (no FP regression) |

### NEW (added in M2)

| File | Lines | Shape | Expected post-M2 |
|------|-------|-------|------------------|
| `ruby/command_injection_percent_x_positive.rb` | ~7 | `%x{#{cmd}}` | GREEN (≥1 command_injection finding) |
| `ruby/command_injection_percent_x_string_literal_fp.rb` | ~6 | `"use %x{cmd} for inline shell"` (string only) | GREEN (zero findings) |

Test runner: `crates/tldr-cli/tests/vuln_migration_v1_red.rs` — 2 new test
functions using the existing `run_tldr_vuln` / `findings_of_type` /
`all_findings` helpers, mirroring the existing `ruby_command_injection_*`
test pair at L980-L1000.

---

## §5 Dispatch extension spec

**File**: `crates/tldr-core/src/security/taint.rs`
**Function**: `detect_sinks_ast` (signature unchanged)
**Anchor**: immediately after the `for pattern in patterns.sinks { … }` block at L4275-L4374, BEFORE the closing brace of the descendant loop iteration (so the same `descendant`, `line`, `is_in_comment` / `is_in_string` filters remain in scope).

```rust
// ruby-backtick-extraction-v1 §5 — Ruby backtick / %x{} subshell dispatch.
//
// Closes the carry-forward from vuln-source-parity-v1 M5 Bucket A Ruby
// subset (ruby_command_injection_positive). Predecessor precedent:
// field_access_info-extension-v1 retained `\bgets\b` for the bare-call
// AST shape gap — same shape of carry-forward, different node kind.
//
// tree-sitter-ruby 0.23.1 parses BOTH `…` and %x{…}/%x[…]/%x(…) as a
// `subshell` named-node containing `interpolation` /
// `string_content` / `escape_sequence` children (verified in
// reports/investigation.json). subshell is NOT call-shaped — it has
// no `method` / `receiver` field and `extract_call_name_ruby` returns
// None for it. The for-pattern-in-patterns.sinks loop above cannot
// match; this dispatch arm is the entire matcher.
//
// Adding `subshell` to call_node_kinds(Ruby) (Option A) would require
// extending extract_call_name_ruby with a synthetic name AND would
// affect every consumer of call_node_kinds (sources, sanitizers,
// references.rs:3325 is_call). Localized arm here (Option B) is
// surgically scoped to ShellExec sink detection only.
//
// Var-extraction reuses extract_first_identifier_arg_ast's generic
// descend-named-children path — for `\`#{cmd}\``, the walk yields
// subshell → interpolation → identifier(cmd). Pure-static subshells
// without interpolation (e.g., `\`ls\``) yield None and emit no sink
// — correct (no taint flow possible).
if language == Language::Ruby && descendant.kind() == "subshell" {
    let stmt_text = std::str::from_utf8(source)
        .unwrap_or("")
        .lines()
        .nth((line - 1) as usize)
        .unwrap_or("");
    let var = extract_first_identifier_arg_ast(descendant, source, language)
        .or_else(|| extract_assignment_rhs_ident(descendant, source, stmt_text))
        .or_else(|| extract_source_var_from_statement(stmt_text));
    if let Some(var) = var {
        sinks.push(TaintSink {
            var,
            line,
            sink_type: TaintSinkType::ShellExec,
            statement: Some(stmt_text.to_string()),
        });
        continue; // Only one sink per node — same convention as the loop above.
    }
}
```

**Edits required**: 1 file (`taint.rs`). 1 dispatch arm (~25 LOC including
comment). LOC delta source-only ~25; LOC delta after counting fixtures and
test functions ~60.

**No edit required to**:
- `ast_utils.rs::call_node_kinds` (Option A rejected).
- `ast_utils.rs::extract_call_name_ruby` (Option A+ rejected).
- `RUBY_AST_SINKS` (would be dead code; see §1).
- Any other AST bank or helper.

---

## §6 CHANGELOG draft

```md
## ruby-backtick-extraction-v1 — internal milestone

### Added
- AST dispatch arm in `detect_sinks_ast` for Ruby `subshell` nodes
  (backtick `` `cmd` `` and `%x{cmd}` / `%x[cmd]` / `%x(cmd)` forms).
  Treats subshells as `ShellExec` sinks; var-extraction reuses
  `extract_first_identifier_arg_ast`'s generic descend-and-find-identifier
  path. Closes 1 carry-forward from `vuln-source-parity-v1` M5 Bucket A
  Ruby subset (`ruby_command_injection_positive`).
- Two new positive/FP fixture pairs covering the `%x{...}` shape:
  `command_injection_percent_x_positive.rb` and
  `command_injection_percent_x_string_literal_fp.rb`.

### Architectural note
The dispatch arm is keyed on the tree-sitter-ruby `subshell` node-kind
directly, NOT via `call_node_kinds(Ruby)` extension. This isolates the
change to ShellExec sink detection and avoids polluting `call_node_kinds` /
`extract_call_name_ruby` consumers (sources, sanitizers, references.rs
is_call gate, rr_baseline_per_language_test). Predecessor pattern
reference: `field_access_info-extension-v1` retained `\bgets\b` for the
bare-call AST shape gap — same shape of carry-forward (raw-substring/AST
node-kind mismatch), different localized resolution.

### Retained
- `call_node_kinds(Ruby)` unchanged (still `["call", "method_call"]`).
- `extract_call_name_ruby` unchanged (still matches `"call" | "method_call"`).
- `RUBY_AST_SINKS` unchanged (no new `AstSinkPattern` entry — the dispatch
  arm is the entire matcher for subshell shapes).
- Public API unchanged.
```

---

## §7 Premortem / risk register

### R1 — `extract_first_identifier_arg_ast` doesn't descend through `interpolation` (MEDIUM)
- **Risk**: The generic body of the helper at `taint.rs:3934-3982` may not
  descend through `interpolation` named-children to reach the inner
  identifier. If so, `var` resolves to None → no sink emitted → fixture
  stays RED.
- **Mitigation**: M1 explicitly verifies via empirical node-audit. If the
  helper fails, M2 extends it with a Ruby-specific subshell descent arm at
  the same site. Still localized to `taint.rs`. LOC delta bumps from ~25 to
  ~40 (still inside the §0 LOC bound).
- **Tiger / elephant**: tiger if M1 audit is skipped; elephant if M1 catches
  it.

### R2 — Side-effect on Ruby source detection (LOW)
- **Risk**: `RUBY_AST_SOURCES` has `("", "params[")` raw-fallback. A
  subshell node whose `descendant_text` includes `params[...]` (e.g.,
  `` `#{params[:cmd]}` ``) would match the raw-fallback for source
  detection ALREADY at HEAD. This milestone does not change that.
- **Mitigation**: Out of scope. Documented in `investigation.json`
  `side_effect_audit.sources`. If the canonical fixture had `` `#{params[:cmd]}` ``
  shape it would already detect a source via raw-fallback; the fixture uses
  `cmd = params[:cmd]; \`#{cmd}\`` so source detection is on the assignment
  line, not the subshell line — unaffected.
- **Tiger / elephant**: elephant.

### R3 — Build-sinks-ast-index parallel loop (MEDIUM-LOW)
- **Risk**: There may be a second sink-dispatch loop (e.g., a future
  per-line index helper analogous to `build_sanitizer_ast_index` from
  sanitizer-removal-v1 M2) that also iterates descendants and applies the
  same pattern matching. If the dispatch arm is added only to
  `detect_sinks_ast` and a parallel helper exists, the fixture transitions
  GREEN under cargo test (which calls `detect_sinks_ast` via
  `compute_taint_with_tree`) but stays RED under `tldr vuln` binary
  if the binary's path uses the parallel helper.
- **Investigation note**: As of HEAD `997557b` there is NO parallel
  build_sinks_ast_index — sinks dispatch flows through `detect_sinks_ast`
  exclusively (verified by grepping for `for pattern in patterns.sinks` and
  finding 1 occurrence in `detect_sinks_ast`). Sanitizers DO have a
  parallel `build_sanitizer_ast_index` helper added in sanitizer-removal-v1,
  but sinks do not.
- **Mitigation**: M3 binary-smoke gate (`tldr vuln` invocation) catches
  any drift. If a future milestone adds `build_sinks_ast_index`, that
  milestone is responsible for replicating the subshell arm there.
- **Tiger / elephant**: elephant.

### R4 — `%x{...}` parsing variance across tree-sitter-ruby versions (LOW)
- **Risk**: tree-sitter-ruby 0.23.1 collapses both forms onto `subshell`.
  An earlier or later version might split them.
- **Mitigation**: `Cargo.lock` pins the version. Any future version-bump PR
  is responsible for re-verifying the M1 node-audit. Documented in
  `investigation.json`.
- **Tiger / elephant**: elephant (out-of-scope).

### R5 — FP class via the raw-substring fallback for member_patterns (LOW)
- **Risk**: Adding a `member_patterns` entry like `("", "`")` to catch
  subshell would FP on any line containing a backtick character (e.g.,
  Markdown-quoted Ruby docstrings). This milestone does NOT add such a
  pattern (Option B uses a node-kind dispatch arm, not a raw-substring
  pattern).
- **Mitigation**: Built into the design choice. R5 is informational —
  documents why the reasonable-looking shortcut was rejected.
- **Tiger / elephant**: elephant.

---

## §8 Self-validation

### validator_mandates (this plan)

| Mandate | Verdict | Notes |
|---------|---------|-------|
| `ast_dispatch_localized_to_taint_rs` | PASS | No edits to `ast_utils.rs`. Node-kind-keyed arm only in `detect_sinks_ast`. |
| `no_call_node_kinds_extension` | PASS | `call_node_kinds(Ruby)` unchanged. Avoids broad side-effect surface. |
| `no_extract_call_name_extension` | PASS | `extract_call_name_ruby` unchanged. |
| `no_dead_bank_entries` | PASS | No `RUBY_AST_SINKS` entry added (would be dead code; the dispatch arm is the entire matcher). |
| `fp_regression_guard_preserved` | PASS | `ruby_command_injection_string_literal_fp` stays GREEN; new `%x{}` FP fixture added. |
| `red_first_harness_required` | PASS | M1 captures RED before M2 implements GREEN. |
| `atomic_m2` | PASS | M2 ships dispatch arm + 2 fixtures + 2 tests in ONE commit; no partial-fix follow-ups. |
| `binary_smoke_required_in_m3` | PASS | `tldr vuln` invocation in M3 stop_thresholds. |
| `no_push_no_publish_no_version_bump` | PASS | Local tag only. |
| `staging_method_explicit_add` | PASS | `git add <listed-files>` per file; no `git add -A`. |
| `cargo_lock_never_staged` | PASS | `git checkout HEAD -- Cargo.lock` if dirty before each commit. |
| `predecessor_precedent_documented` | PASS | FAI-v1 `\bgets\b` cross-referenced in code comment AND CHANGELOG note. |
| `node_audit_empirical` | PASS | M1 produces `M1-node-audit.json` from a real tree-sitter-ruby parse. |

---

## §9 /autonomous-readiness

**Verdict**: READY.

**Conditions**: None.

**Rationale**:
1. Investigation is complete and empirically verified against
   `tree-sitter-ruby 0.23.1`'s `node-types.json`.
2. Design choice (Option B — localized dispatch arm) is documented with
   2 alternatives explicitly rejected and rationale.
3. M1 / M2 / M3 gates are clear, deterministic, and binary-verifiable.
4. LOC bound (~10-20 source LOC) is conservative; even worst-case (R1
   triggers, helper extension required) stays under ~40 LOC.
5. No dependencies on other in-flight milestones (`var-extract-nested-constructor-v1`
   and `rust-vuln-taint-pipeline-v1` are independent — they touch
   different fixtures and different code paths).
6. Carry-forward documentation (vuln-source-parity-v1 M5-carry-forward.json
   `follow_on_milestones[2]`) explicitly names this milestone with the same
   scope; we are honoring the written contract.

**Pipeline metadata**:
- Source loop: `ruby-backtick-extraction-v1-plan` (this).
- Workers spawned: 0 (single-worker investigation; smallest of three
  follow-on plans).
- Consolidator outputs:
  - `continuum/autonomous/ruby-backtick-extraction-v1-plan/plan.md` (this file)
  - `continuum/autonomous/ruby-backtick-extraction-v1-plan/dispatch-contract.json`
  - `continuum/autonomous/ruby-backtick-extraction-v1-plan/reports/investigation.json`
- Predecessor: `vuln-source-parity-v1` M5 (HEAD `997557b` includes it
  transitively via merged tag).
- Tag on completion: `ruby-backtick-extraction-v1` (local only; no push).

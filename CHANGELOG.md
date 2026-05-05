# Changelog

## schema-cleanup-v2 — internal milestone

NOT a published release. Third and final milestone of phase 2: closes
four LOW-severity bugs that all share the surface "schema/UX polish".
Individually small; together they cover JSON-shape bugs (a missing
field, a stale literal placeholder), a format-dispatch oversight (a
DOT advertised but silently emitting JSON), and an exit-code-parity
regression on empty directories. None block any workflow, but each
one made the tool feel slightly less coherent than it should.

### Fixed

- **P2.BUG-6: `tldr clones --format dot` silently emitted JSON, exit
  0.** The clones command's run loop dispatched on the legacy
  `--output` flag for DOT but had no `OutputFormat::Dot` arm in the
  format-match block below — so the canonical `--format dot` path
  fell through to the JSON arm. Meanwhile
  `validate_format_for_command` (output.rs) and `secure --format dot`'s
  error message both advertised clones as DOT-supported, leaving users
  to wonder why their `dot -Tpng` pipeline produced nothing useful. The
  dedicated emitter `format_clones_dot` was already present and
  reachable via `--output dot`; the fix wires the canonical
  `--format dot` route to it.
  - `crates/tldr-cli/src/commands/clones.rs` — add `OutputFormat::Dot`
    arm in the run loop's match block.

- **P2.BUG-7: `tldr clones`'s JSON `language` field always echoed
  `"auto"`.** Pre-fix, the report's `language` field was filled with
  `options.language.clone().unwrap_or_else(|| "auto".to_string())` —
  meaning the field reported either the user's `--lang` flag verbatim
  or the literal placeholder `"auto"`, regardless of what the
  autodetector actually picked. Consumers programmatically reading
  the field (e.g. CI integrators routing per-language analysis) had
  no way to tell what the autodetector chose. The field now resolves
  to the dominant language string across the discovered files via a
  new `resolve_dominant_language` helper that tallies extensions
  against `get_language_from_path` and returns the most-frequent
  match (insertion-order tiebreak for determinism). Falls back to
  `"auto"` only when the file set has no recognised extensions, an
  effectively unreachable case post-`is_source_file_for_clones`.
  - `crates/tldr-core/src/analysis/clones/mod.rs` — new
    `resolve_dominant_language` helper; both report-construction sites
    use it instead of the `"auto"` fallback.

- **P2.BUG-9: `tldr vuln` findings had no enclosing `function` field.**
  Pre-fix, vuln finding records carried only `(file, line)`, blocking
  clean piping into `tldr taint <file> <function>` and `tldr slice
  <file> <function> <line>` — the user had to manually scan source
  for the enclosing def before chaining further analysis. Findings
  now carry an `Option<String>` `function` field populated post-filter
  via `extract_file` (the same AST extractor `taint`/`slice` rely on).
  Findings at module scope leave `function = None` and the field is
  omitted from JSON via `skip_serializing_if = "Option::is_none"`,
  preserving forward compatibility for module-level findings.
  Performance: enrichment groups findings by file path so
  `extract_file` runs once per unique file across the post-filter
  slice, regardless of how many findings target that file.
  - `crates/tldr-cli/src/commands/remaining/types.rs` — add
    `function: Option<String>` to `VulnFinding`.
  - `crates/tldr-cli/src/commands/remaining/vuln.rs` — new
    `enrich_with_enclosing_function` + `lookup_enclosing_function`
    helpers; called once after sort/filter, before summary build.
    All four `VulnFinding` construction sites set `function: None`
    initially.

- **P2.BUG-10: empty-directory handling was inconsistent across
  commands; `calls` silently defaulted to `language: "python"` for
  empty input.** Pre-fix exit codes for an empty directory were
  scattered: `structure`/`calls`/`vuln` returned 0; `health` returned
  23 (`No supported files found`); `deps` returned 11 (`Unsupported
  language: unknown`); `churn` returned 1 (`Not a git repository`).
  An empty directory is a benign edge case (e.g. fresh `mktemp -d`,
  a docs-only tree), not an error condition. `calls` additionally
  used `unwrap_or(Language::Python)` for autodetect failure, so an
  empty tree was reported as `language: "python"` with zero edges —
  silently picking a default that misrepresented the input.
  - `crates/tldr-cli/src/commands/calls.rs` — change `CallGraphOutput.
    language` from `Language` to `Option<Language>`. Use the autodetect
    result directly (None when no analyzable files), preserving the
    existing Python-fallback for the call-graph builder which requires
    a concrete language. Text output now prints "unknown" rather than
    `Some(Python)` for an unresolved language.
  - `crates/tldr-cli/src/commands/health.rs` — short-circuit before
    invoking `run_health` when the user passed no `--lang` AND
    `Language::from_directory` finds no analyzable files; emit a stub
    JSON with `language: null` and `warnings: ["Empty directory: ..."]`,
    exit 0.
  - `crates/tldr-cli/src/commands/deps.rs` — same short-circuit pattern
    for the `Unsupported language: unknown` case.
  - `crates/tldr-cli/src/commands/churn.rs` — short-circuit only when
    the directory is *empty* (`std::fs::read_dir` yields nothing).
    Non-empty non-git directories still surface the original
    actionable error.

### Tests

- `crates/tldr-cli/tests/schema_cleanup_v2.rs` — 5 new regression
  tests: clones DOT output is valid + has edges, clones language
  resolves to `"python"` not `"auto"`, vuln finding has the
  `function` field set on an in-function fixture, all 6 sample
  commands exit 0 on an empty directory, and `calls` does not
  default `language: "python"` for empty input.

## cli-error-clarity-v2 — internal milestone

NOT a published release. Second milestone of phase 2: closes three
MED-severity bugs that all share the surface "the CLI says one thing, the
runtime does another". When users get conflicting signals from `--help`,
error messages, and command behaviour, they lose trust in the tool. Each
bug is a small footgun on its own; together they make the surface feel
unreliable.

### Fixed

- **P2.BUG-4: `tldr hubs|impact|whatbreaks|change-impact <file>` returned
  confusing errors when given a regular file instead of a directory.**
  `hubs`, `impact`, and `whatbreaks` checked `path.exists()` only, so
  passing a file produced `Error: Path not found: <file>` — false, the
  file *does* exist. `change-impact` had no validation at all; the file
  reached the git invocation downstream and surfaced
  `Git: Not a directory (os error 20)`, which gives the user no clue
  what to fix. New shared helper `path_validation::require_directory`
  centralises the check and produces a single, actionable message:
  `<command> requires a directory; got file '<path>'. Pass the project
  root or omit the argument to use the current directory.` All four
  commands now use the helper before any expensive work runs.
  - `crates/tldr-cli/src/path_validation.rs` — new module + unit tests.
  - `crates/tldr-cli/src/lib.rs` — register the module.
  - `crates/tldr-cli/src/commands/hubs.rs` — replace bare-`exists` check.
  - `crates/tldr-cli/src/commands/impact.rs` — replace bare-`exists` check.
  - `crates/tldr-cli/src/commands/whatbreaks.rs` — replace bare-`exists`
    check.
  - `crates/tldr-cli/src/commands/change_impact.rs` — add validation
    upstream of the git invocation.

- **P2.BUG-5: per-command `--help` advertised `sarif` / `dot` formats
  the runtime rejects.** The global `--format` flag is a `clap::ValueEnum`
  with five variants (json, text, compact, sarif, dot). clap's auto-generated
  help dutifully listed all five under "Possible values" on every
  subcommand. But `validate_format_for_command` (output.rs) gates SARIF
  to `vuln`/`clones` and DOT to `clones`/`deps`/`calls`/`impact`/`hubs`/
  `inheritance`, so `tldr structure --format sarif` advertised support
  in `--help` and then bailed at runtime with "not supported by
  structure". Fix: hide possible values on the global flag
  (`hide_possible_values = true`) and replace the auto-generated list
  with explicit long-help text that names which commands actually emit
  each command-specific format. The runtime is unchanged — it remains
  the source of truth for which format/command pairs are valid.
  - `crates/tldr-cli/src/main.rs` — `Cli.format` definition: hide
    possible values + add long help describing universal vs
    command-specific formats.

- **P2.BUG-8: `tldr context <fn> <repo-root>` returned 0 functions when
  the same call from `<repo-root>/src` returned several.** Root cause
  was in `find_function_in_graph` (context/builder.rs): it iterated
  call-graph edges and returned the FIRST match. When a project root
  is walked (e.g. flask/), test fixtures often contain placeholder
  classes with the same name as the real implementation but no method
  bodies (`class Flask: pass` in `tests/test_config.py`). The first
  edge could therefore land on the placeholder, BFS would collect a
  single key under that path, `find_function_info` would return None
  for every key, and the entire function list would come back empty.
  Walking from `<root>/src` happened to put the real edge first, so
  the bug only surfaced from the project root. Fix: collect ALL
  candidate locations, deprioritise paths under `tests/`/`__tests__`/
  `spec`/etc., and verify each candidate by extracting the module and
  confirming the function is actually defined there. Only fall back to
  the first edge match if no candidate verifies.
  - `crates/tldr-core/src/context/builder.rs` —
    `find_function_in_graph` rewritten as a verified candidate
    selector with a test-path deprioritisation heuristic.

### Tests

- New: `crates/tldr-cli/tests/cli_error_clarity_v2.rs` — seven tests:
  `hubs_on_file_clear_error`, `impact_on_file_clear_error`,
  `whatbreaks_on_file_clear_error`, `change_impact_on_file_clear_error`
  (all assert: failure exit + stderr mentions "requires a directory"
  and the file path), `format_help_matches_runtime_calls` and
  `format_help_matches_runtime_structure` (assert help no longer
  advertises sarif/dot for non-supporting commands AND runtime still
  rejects them), and `context_works_from_repo_root` (synthetic mini-
  repo with a placeholder `Foo` in `tests/` and the real `Foo.bar` in
  `src/pkg/core.py`; asserts `tldr context Foo.bar <root>` returns
  >= 1 function and `>=` the count returned from `<root>/src`).
- New unit tests: `crates/tldr-cli/src/path_validation.rs::tests`
  (3 tests covering directory pass, file fail, missing-path fail).

## cross-language-extraction-v2 — internal milestone

NOT a published release. First milestone of phase 2: closes the three
HIGH-severity cross-language extractor coverage gaps surfaced by the
phase-2 audit. All three bugs share the surface "extractor logic that
worked for the audit's headline languages (Python / Rust / TS) but
silently produced empty fields on languages plumbed in later".

### Fixed

- **P2.BUG-1: `tldr structure.method_infos` empty for Go receiver-methods.**
  Go method declarations have the form `func (r *T) Foo()` — the receiver
  is part of the function declaration, not a struct-body nesting, so
  `is_inside_class_or_impl` returned false and methods were classified
  as `kind: "function"` (and consequently never made it into
  `method_infos`, which is filtered by `kind == "method"`). New helper
  `is_go_method_with_receiver` keys off the `method_declaration` node
  kind that tree-sitter-go emits only when a receiver is present
  (regular functions emit `function_declaration`). On
  `/tmp/repos/go-httprouter` this restores 32 method_infos entries
  (router.go: 20, router_test.go: 6, tree.go: 6).
  - `crates/tldr-core/src/ast/extractor.rs` — added `is_go_method_with_receiver`
    helper + extended the `entry_kind` classification chain in
    `collect_definitions` to consult it.

- **P2.BUG-2: `tldr imports` returned `[]` for Swift and Kotlin.** Both
  languages were explicitly stubbed out with `Vec::new()` in the
  `extract_imports_from_tree` match arm. We now ship full extractors for
  both: Swift parses `import_declaration` nodes (handling submodule kind
  keywords like `import struct Foo.Bar` and attribute prefixes like
  `@testable import X`); Kotlin parses both the `tree-sitter-kotlin-ng`
  `import` statement node (workspace-pinned grammar — uses
  `qualified_identifier` children) AND the legacy `import_header` node
  used by vanilla `tree-sitter-kotlin`. Both extractors handle wildcard
  (`.*`) and aliased (`as`) imports.
  - `crates/tldr-core/src/ast/imports.rs` — new
    `extract_swift_imports` / `extract_kotlin_imports` plus their
    `parse_*_import_text` parsers and `is_kotlin_import_statement`
    helper for grammar-version disambiguation.

- **P2.BUG-3: `tldr todo` autodetect was hardcoded to 5 languages.**
  `detect_language` only matched `.py` / `.ts` / `.tsx` / `.js` / `.jsx`
  / `.rs` / `.go`; for everything else it walked at most 2 directory
  levels and silently fell through to `Language::Python`. Java, Kotlin,
  Elixir, OCaml, Ruby, PHP, Scala, C#, and Lua trees consequently
  reported `items: []`. The other commands (`structure`, `vuln`,
  `secure`) had migrated to the shared `Language::from_path` /
  `Language::from_directory` helpers in the AA1 milestone; `todo` was
  missed. The replacement now routes through both helpers and only
  falls back to Python when the directory contains no recognised source
  files at all (rather than as a default for "directory contains source
  files I don't recognise").
  - `crates/tldr-cli/src/commands/remaining/todo.rs` — `detect_language`
    rewritten + dropped the unused `ProjectWalker` import.

### Tests
- New: `crates/tldr-cli/tests/cross_language_extraction_v2.rs` — six
  tests covering each fix:
  - `js_structure_method_infos_populated` (regression guard for the JS
    side of P2.BUG-1)
  - `go_structure_method_infos_populated` — asserts `Handle` and
    `Lookup` reach `method_infos` AND have `kind: "method"` in
    `definitions`, while a free function `main` stays `kind: "function"`
  - `swift_imports_extracted` — asserts `Foundation`,
    `PackageDescription` (after submodule kind keyword), and `MyMod`
    (after `@testable` attribute) all parse
  - `kotlin_imports_extracted` — asserts simple, wildcard (with
    `is_from: true`), and aliased (`as B`) imports all parse
  - `todo_autodetect_works_for_java` and `todo_autodetect_works_for_kotlin`

### Spot-verification (post-install)

```text
# P2.BUG-1
$ tldr structure /tmp/repos/go-httprouter | jq '[.files[].method_infos | length] | add'
32

# P2.BUG-2 — Swift
$ tldr imports Package@swift-6.0.swift | jq '.imports[0]'
{ "module": "PackageDescription", "is_from": false }

# P2.BUG-2 — Kotlin
$ tldr imports additionalConfiguration.kt | jq '.imports[0]'
{ "module": "jetbrains.buildServer.configs.kotlin.*", "is_from": true }

# P2.BUG-3 — autodetect sweep
java     auto=20  --lang=20
kotlin   auto=3   --lang=3
elixir   auto=2   --lang=2
ocaml    auto=5   --lang=5
ruby     auto=8   --lang=8
php      auto=20  --lang=20
scala    auto=20  --lang=20
csharp   auto=20  --lang=20
lua      auto=20  --lang=20
```

### Deferred
- **OCaml `method_infos` for `class … object … end` types.** OCaml has
  classes with methods (`object method foo = ... end`), but the existing
  `extract_classes` arm for `Language::Ocaml` is a no-op — the language
  has no `extract_ocaml_classes` function — so OCaml class detection is
  out of scope for this milestone. The handoff explicitly allowed this
  ("OCaml can stay 0 if OCaml extractor doesn't model methods"). Closing
  this gap requires building the OCaml class extractor end-to-end and
  is tracked separately.

### Baselines (all GREEN)
- `vuln_migration_v1_red`: 168/168
- `determinism_and_stderr_hygiene_v1`: 5/5
- `cross_command_consistency_v1`: 7/7
- `detection_accuracy_v1`: 4/4
- `schema_cleanup_v1`: 11/11
- `surface_gaps_v1`: 6/6
- `cli_basic_tests`: 70/70 (6 ignored — unchanged)
- `rr_framework_integ_test`: 18/18 (in tldr-core)
- `cross_language_extraction_v2`: 6/6 (new)

## test-fixture-realignment-v1 — internal milestone

NOT a published release. Realigns 6 unit-test assertions left stale by
two prior milestones: M3's `detection-accuracy-v1` correctly reclassified
JS/TS redirect sinks from `FileWrite`/CWE-22 (the wrong category — open
redirects are CWE-601, not path traversal) to `OpenRedirect`/CWE-601, and
M4's `schema-cleanup-v1` removed the redundant `functions` string array
from `tldr structure` JSON in favour of the canonical `definitions`
array. The two milestones each introduced their semantic fix correctly,
but did not update tests in adjacent files that asserted on the
pre-existing-buggy behaviour. Five `rr_framework_integ_test` cases
(`nextjs_response_redirect_open_redirect_via_compute_taint`,
`nextjs_redirect_helper_via_compute_taint`,
`fastify_reply_redirect_via_compute_taint`,
`nestjs_res_redirect_open_redirect_via_compute_taint`,
`nestjs_response_builder_redirect_via_compute_taint`) — each of whose
*name* already encoded the correct "open redirect" intent — and one
`cli_basic_tests::structure_tests::test_structure_default_path` case
were updated to match the new (correct) semantics. No production code
changed.

### Changed
- `crates/tldr-core/tests/rr_framework_integ_test.rs` — five redirect-flow
  tests updated from `TaintSinkType::FileWrite` to
  `TaintSinkType::OpenRedirect` with correspondingly retitled assertion
  messages. The other two `FileWrite` assertions in this file
  (`fastify_reply_header_injection_via_compute_taint` line 218 and
  `nextjs_dangerously_set_inner_html_via_compute_taint` line 138) were
  left intact — they were passing under M3 and target separate sink
  categories (header-injection and JSX-XSS); they are out of scope for
  this realignment.
- `crates/tldr-cli/tests/cli_basic_tests.rs:282` — `test_structure_default_path`
  now asserts the canonical `"definitions"` substring instead of the
  removed-by-design `"functions"` string array.

### Architectural note
This is purely a test-file realignment. The pattern is the same one used
by `detection-gap-fixes-v1` (commit 18a3680) earlier in the project: when
a milestone fixes a *semantic* bug, follow up on stale test assertions
in adjacent suites whose *name* already encodes the correct intent. The
test names served as the source of truth — `..._open_redirect_...` is
unambiguous about what the test was meant to assert.

### Retained
- All M3 + M4 production code intact (no changes to
  `crates/tldr-core/src/security/` or `crates/tldr-core/src/types.rs`).
- The two non-redirect `FileWrite` assertions in `rr_framework_integ_test`
  (header-injection, JSX `dangerouslySetInnerHTML`) remain unchanged —
  out of scope.
- All 6 prior milestone test files stay GREEN: M0 168, M1 5, M2 7, M3 4,
  M4 11, M5 6 = 201/201.

### Quantification

| Suite | Before this commit | After this commit |
| --- | --- | --- |
| `cargo test -p tldr-core --test rr_framework_integ_test` | 13 passed / 5 failed | 18 passed / 0 failed |
| `cargo test -p tldr-cli --test cli_basic_tests structure_tests::test_structure_default_path` | FAIL | OK |
| `cargo test -p tldr-cli --test cli_basic_tests` | 69 passed / 1 failed | 70 passed / 0 failed |

### Standing rules upheld
- One atomic commit, one CHANGELOG entry, one local annotated tag
- Cargo.lock not staged
- No push, no `cargo publish`, no version bump (manifest stays at 0.3.0)
- Explicit-add only (3 files)
- 168/168 master regression `vuln_migration_v1_red` preserved

## surface-gaps-v1 — internal milestone

NOT a published release. Closes 2 audit-found bugs that share the
"advertised but missing/wrong feature" surface — `tldr` told users about
behaviors that were either not implemented or referenced flags that did
not exist. **BUG-6**: when `tldr impact` could not find callers for a
function it knew was exported, the helpful note read "If this is a
monorepo, run from the workspace root or pass `--workspace-root <path>`."
— but `--workspace-root` is not an argument on `tldr impact` (or any
other subcommand); `tldr impact --help` makes no mention of it, and
`tldr impact --workspace-root /tmp foo` errors with `unexpected argument`.
The note has been rewritten to describe the actual analyzed root and
the canonical monorepo workflow without dangling a phantom flag.
**BUG-19**: `tldr calls`, `tldr inheritance`, `tldr hubs`, and `tldr
impact` all rejected `--format dot` with the format-strictness gate
(`DOT is only emitted by: clones, deps`) — even though call graphs and
class hierarchies are the canonical Graphviz use cases and exactly
what users want to pipe into `dot -Tsvg`. Each command now emits a
real Graphviz `digraph` document: `calls` produces one
caller→callee edge per resolved call site, `impact` produces a reverse
graph (caller flowing toward the analyzed target), `hubs` produces a
node-only document where each hub's label carries its composite score,
and `inheritance` (whose underlying `tldr_core::inheritance::format_dot`
already existed but was wired only to a hidden legacy `-o dot` flag)
now also honors the global `--format dot` flag. Binary-verified
against `/tmp/repos/flask` and gated by 6 regression tests in
`crates/tldr-cli/tests/surface_gaps_v1.rs`. M0
(`vuln_migration_v1_red`): 168/168 GREEN. M1
(`determinism_and_stderr_hygiene_v1`): 5/5 GREEN. M2
(`cross_command_consistency_v1`): 7/7 GREEN. M3
(`detection_accuracy_v1`): 4/4 GREEN. M4
(`schema_cleanup_v1`): 11/11 GREEN.

| Bug | File:Line | Before | After |
|-----|-----------|--------|-------|
| BUG-6 | `crates/tldr-core/src/analysis/impact.rs:378-386` (rewrite the exported-but-no-callers note) | `tldr impact <fn> /tmp/repos/flask \| jq -r '.targets[].note'` → `"Function is exported but no callers found in /private/tmp/repos/flask. If this is a monorepo, run from the workspace root or pass --workspace-root <path>."` (mentions a flag that does not exist on impact and is not parsed by clap) | note now reads `"Function is exported but no callers found within the analyzed root '<path>'. In monorepo workflows, ensure you run tldr from the directory that contains all callers."`; sweep across 12 flask functions confirms zero occurrences of the literal `workspace-root` substring in any impact JSON |
| BUG-19 | `crates/tldr-cli/src/output.rs:113-160` (extended `DOT_SUPPORTED` to `["clones","deps","calls","impact","hubs","inheritance"]`); `crates/tldr-cli/src/output.rs` (added `format_calls_dot`, `format_impact_dot`, `format_hubs_dot` + `DotCallEdge` carrier struct + recursive `emit_impact_caller_edges`); `crates/tldr-cli/src/commands/calls.rs` (DOT arms in both daemon-route and direct-compute paths); `crates/tldr-cli/src/commands/impact.rs` (DOT arms in both paths); `crates/tldr-cli/src/commands/hubs.rs` (DOT arm); `crates/tldr-cli/src/commands/inheritance.rs:108-118` (global `--format dot` now selects `InheritanceFormat::Dot` instead of falling through to JSON); `crates/tldr-cli/tests/format_flag_strictness_v1.rs:89-101, 240-253` (removed `dot_errors_on_calls` regression guard, expanded `validator_unit_dot_allowlist` to assert the four newly-allowed commands and to confirm `smells/tree/structure/taint/vuln/secrets` still reject DOT) | `tldr calls /tmp/repos/flask --format dot 2>&1 \| head -1` → `Error: --format dot not supported by calls. Use --format json. DOT is only emitted by: clones, deps.`; same for `inheritance`, `hubs`, `impact` | `tldr calls /tmp/repos/flask --format dot` → `digraph calls { rankdir=LR; ...` with one labeled `caller -> callee` edge per resolved call site (200 edges on flask after default truncation); `tldr inheritance --format dot` → `digraph inheritance { rankdir=BT; ...` with subclass→superclass edges (45 edges on flask); `tldr impact url_for --format dot` → `digraph impact { rankdir=RL; ...` reverse-call-graph (34 edges on flask); `tldr hubs --format dot` → `digraph hubs { ...` with each top hub annotated `(score=0.123)` and a synthetic invisible chain so layout engines render in rank order; output is deterministic across runs |

### Changed

- **BUG-6** — `crates/tldr-core/src/analysis/impact.rs:378-386`: the
  exported-but-no-callers branch of `note_for_target` advertised a
  `--workspace-root` flag that does not exist anywhere in `tldr`'s
  argument parser. Inspection of `tldr impact --help` shows no such
  flag, and clap rejects it explicitly with `unexpected argument`.
  This was a documentation drift bug — at some point a workspace-root
  feature was contemplated and the user-facing note was written ahead
  of the implementation, but the implementation never landed and the
  note remained. The fix rewrites the note to describe the actual
  invariant ("the analyzed root is `<path>`") and the workflow that
  users should follow ("run tldr from the directory that contains all
  callers"). The note still helps users reason about monorepo
  scenarios; it just no longer points them at a phantom flag.
- **BUG-19** — `crates/tldr-cli/src/output.rs:113-160` +
  `crates/tldr-cli/src/commands/{calls,impact,hubs,inheritance}.rs`:
  the `format-flag-strictness-v1` milestone (October 2025) hardened
  the format-flag dispatch so commands could no longer silently fall
  back to JSON when given an unsupported format — important security
  property because CI integrations gating on SARIF would otherwise
  trust JSON output as if it were SARIF. But the strictness gate was
  intentionally conservative: it allowed DOT only on `clones` and
  `deps`, even though call graphs and class hierarchies are precisely
  the canonical Graphviz use case. surface-gaps-v1 extends the
  allowlist to four more commands and wires each one's `is_dot()` arm
  to a real Graphviz emitter:
  - `format_calls_dot`: emits one labeled `caller -> callee` edge per
    resolved call site, with the call_type (`Direct`, `Indirect`,
    `MethodCall`, etc.) carried in the edge label. Node IDs are
    `<file>:<func>` so functions of the same name in different files
    do not collide.
  - `format_impact_dot`: emits a reverse-call-graph rooted at each
    target function. Edges flow `caller -> callee` (RL layout — the
    target sits on the right). Recursive over the `CallerTree` so the
    full transitive caller closure is rendered, not just the direct
    callers.
  - `format_hubs_dot`: hub reports do not carry the surrounding
    call-graph edges, so this emitter produces a node-only document
    where each top hub is labeled `<name> (score=<composite>)`.
    Synthetic invisible edges form a rank chain so layout engines
    render hubs in score order without misrepresenting non-existent
    call relations. For a true call-graph view, users should run
    `tldr calls --format dot`.
  - `inheritance` already had a real DOT emitter
    (`tldr_core::inheritance::format_dot`) but it was reachable only
    via a hidden legacy `-o dot` flag; the global `--format dot` flag
    fell through to JSON. The fix extends the format-resolution
    fallback in `commands/inheritance.rs` to map `is_dot()` →
    `InheritanceFormat::Dot`.

  All four emitters use the existing `escape_dot_id` helper for
  Windows-path normalization, internal-quote escaping, and unconditional
  quoting of node IDs. Edge labels containing literal `"` are also
  escaped. Output is deterministic — call-edge ordering is the same
  across runs because the upstream `IR.edges` already sorts by
  `(src_file, src_func)`, and impact target keys are sorted before
  emission.



NOT a published release. Closes 9 audit-found schema/dead-UI bugs that
gave consumers null/empty/redundant fields across `tldr health`,
`tldr patterns`, `tldr deps`, `tldr churn`, `tldr structure`,
`tldr semantic`, `tldr search`, `tldr chop`, `tldr interface`, and
`tldr extract`: `tldr health` printed `Metrics: no data` for every
non-Java/.NET repo (Robert C. Martin's package abstractness/instability
metric does not apply to Python / TypeScript / Go / Rust / etc., yet
the row was always rendered) — dead UI on six languages out of seven;
`tldr patterns.naming.violations[].line` was hard-coded `0` because
the `NamingSignals` collector tuples never tracked a line position;
`tldr deps` JSON had `root: ""` and the text header was
`Dependency Analysis: ` (trailing space, no path) because
`make_relative_path(&root, &root)` collapsed to an empty `PathBuf`
when the root was its own root; `tldr churn.summary.most_churned_file`
was blanked on shallow clones even though `files[0]` carried a clean
top-N rank by `lines_changed`; `tldr structure` JSON emitted both
`functions` (strings) AND `definitions` (objects), and both `methods`
(strings) AND `method_infos` (objects) — duplicate views of the same
data with the string arrays carrying no extra information; `tldr
semantic` and `tldr search` JSON omitted `total_results` entirely
(`jq '.total_results'` returned `null` because the key didn't exist);
`tldr chop` schema diverged from `tldr slice` (`count` vs
`line_count`, no `file` field, broke schema parity); `tldr
interface.all_exports` was emitted as `null` for any module without
an explicit Python `__all__` — even when the module clearly had public
classes/functions; `tldr extract` method/function objects emitted
both `line` and `line_number` (duplicate values from the BUG-17
alias) and lacked `line_end`. Binary-verified against
`/tmp/repos/flask` and gated by 11 regression tests in
`crates/tldr-cli/tests/schema_cleanup_v1.rs`. M0
(`vuln_migration_v1_red`): 168/168 GREEN. M1
(`determinism_and_stderr_hygiene_v1`): 5/5 GREEN. M2
(`cross_command_consistency_v1`): 7/7 GREEN. M3
(`detection_accuracy_v1`): 4/4 GREEN. M4
(`schema_cleanup_v1`): 11/11 GREEN.

| Bug | File:Line | Before | After |
|-----|-----------|--------|-------|
| BUG-9 | `crates/tldr-core/src/quality/health.rs:601-624` (text formatter) | `tldr health /tmp/repos/flask --format text` → `Metrics:     no data` row appears on every non-Java repo | `Metrics:` row suppressed unless `summary.avg_distance` is `Some` (real data) OR the metrics sub-result reports a hard failure; JSON sub-result unchanged so consumers can still inspect `details.metrics.details.packages_analyzed` |
| BUG-10 | `crates/tldr-core/src/patterns/signals.rs:198-205` (added 4th `u32` element to the three naming tuples); `crates/tldr-core/src/patterns/naming.rs:71-120` (consume the new line element + plumb into `NamingViolation.line`); `crates/tldr-core/src/patterns/language_profile.rs:160-200, 276-294` (capture `name_node.start_position().row + 1` at `ExtractNamed` dispatch); plus per-language collectors in `languages/{c,cpp,csharp,elixir,kotlin,lua,ocaml,php,ruby,scala}.rs` | `tldr patterns /tmp/repos/flask \| jq '.naming.violations \| map(.line) \| unique'` → `[0]` (every violation reports line=0) | violations carry their actual AST start_position; on flask `min=90, max=1025` (real lines into `app.py`); 0-line violations now imply "synthetic / no AST source" rather than "we forgot to plumb the line" |
| BUG-11 | `crates/tldr-core/src/analysis/deps.rs:462, 647` (use `root.clone()` instead of `make_relative_path(&root, &root)`) | `tldr deps /tmp/repos/flask \| jq -r .root` → `""`; text header → `Dependency Analysis: ` | `.root` → `/private/tmp/repos/flask`; text header → `Dependency Analysis: /private/tmp/repos/flask` |
| BUG-12 | `crates/tldr-cli/src/commands/churn.rs:156-184` (refill `most_churned_file` from highest-`lines_changed` file even on degenerate-shallow clones) | `tldr churn /tmp/repos/flask \| jq -r .summary.most_churned_file` → `""` (blanked, even though `files[0].lines_changed = 682`) | populated from the file with highest `lines_changed`; on flask → `tests/test_basic.py`; the warning about degenerate ranks is preserved so JSON consumers still see the shallow-clone caveat |
| BUG-13 | `crates/tldr-core/src/types.rs:1067-1110` (FileStructure `functions`/`methods` now `#[serde(skip_serializing)]`); `crates/tldr-core/src/types.rs:1112-1136` (MethodInfo gained `line_end`); `crates/tldr-core/src/ast/extractor.rs:218-226` (populate `MethodInfo.line_end` from `DefinitionInfo.line_end`) | `tldr structure /tmp/repos/flask \| jq '.files[0] \| keys'` → both `functions` (strings) AND `definitions` (objects) AND `methods` (strings) AND `method_infos` (objects) — 7 keys total | redundant string arrays gone from JSON (in-memory struct retained for internal back-compat); `method_infos[]` entries now carry `line_end`; canonical schema is `definitions` (objects, full kind + range + signature) and `method_infos` (the overload-distinguishing companion) |
| BUG-15 | `crates/tldr-core/src/semantic/types.rs:213-241` (new `total_results` field on `SemanticSearchReport`); `crates/tldr-core/src/semantic/index.rs:430-440` (populate from `results.len()`); `crates/tldr-core/src/search/enriched.rs:84-100` (mirror on `EnrichedSearchReport`); `:1229-1255` (populate at every return site) | `tldr semantic "x" /tmp/repos/flask \| jq '.total_results, (.results \| length)'` → `null, 10` (key doesn't exist); same for `tldr search` | both report types carry `total_results: usize` populated from `results.len()`; on flask flask `total_results: 10` matches `results \| length: 10` |
| BUG-21 | `crates/tldr-cli/src/commands/contracts/types.rs:622-700` (added `file: String` and `line_count: u32` to `ChopResult`); `crates/tldr-cli/src/commands/contracts/chop.rs:127-152, 328-352` (CLI populates `file` from canonical path; `compute_chop` populates `line_count` from the same source as `count`) | `tldr chop /tmp/repos/flask/src/flask/app.py make_response 1230 1235 \| jq 'keys'` → 7 keys, no `file`, no `line_count` (vs `tldr slice` which has 9 keys including both) | `keys` includes `file` (full canonical path) and `line_count` (matches `count` for back-compat); on `make_response 1230 1235` → 60 lines on the dependency path; degenerate / no-path / same-line cases also return a populated `file` |
| BUG-22 | `crates/tldr-cli/src/commands/patterns/types.rs:497-510` (changed `all_exports: Option<Vec<String>>` → `Vec<String>`); `crates/tldr-cli/src/commands/patterns/interface.rs:1294-1320` (fall back to union of public function/class names when `__all__` is absent); `:1411-1418` (text formatter no longer wraps in `if let Some(...)`) | `tldr interface /tmp/repos/flask/src/flask/app.py \| jq '.all_exports'` → `null` | `.all_exports` → array (never null); explicit `__all__` (Python only) is preferred; otherwise the union of public function and class names is emitted (sorted, deduped); empty modules → `[]` (empty array) |
| BUG-23 | `crates/tldr-core/src/types.rs:1184-1259` (added `line_end: u32` to `FunctionInfo` + manual `Serialize` no longer emits `line_number`); `:1265-1338` (same for `ClassInfo`); `:1347-1416` (same for `FieldInfo`); `crates/tldr-core/src/ast/extract.rs` (49 sites updated to derive `line_end` from `node.end_position().row + 1` and pass it through every `FunctionInfo` / `ClassInfo` / `FieldInfo` constructor) | `tldr extract /tmp/repos/flask/src/flask/app.py \| jq '.classes[0].methods[0] \| keys'` → contains `line` AND `line_number` (duplicate of `line`), missing `line_end` | JSON keys for methods/functions/classes/fields now contain `line` AND `line_end`; `line_number` no longer appears in serialized output (the in-memory field name is preserved with `#[serde(alias = "line")]` so deserializing legacy snapshots still works) |

### Changed

- **BUG-9** — `crates/tldr-core/src/quality/health.rs:601-624`: the
  Martin (Robert C. Martin) package-level abstractness/instability
  metric is computed only for languages whose module model resembles a
  packaged JAR / .NET assembly — Java mostly, and even there only when
  the project follows a clear package convention. For Python /
  TypeScript / Go / Rust / Ruby / Elixir / OCaml / Lua / Scala the
  analyzer always emits zero packages, which historically rendered as
  `Metrics:     no data` in the text dashboard (and `details.metrics.
  details.packages_analyzed: 0` in JSON). The row is now suppressed in
  text output unless either (a) `summary.avg_distance` is `Some`
  (real data was computed) OR (b) the sub-analysis produced a hard
  failure that the user should see. The "no data" silently-suppressed
  case is the common path for the languages where Martin's framework
  doesn't apply. JSON consumers can still inspect
  `details.metrics.details.packages_analyzed` directly — the underlying
  sub-analysis structure is unchanged. **Decision rationale**: per the
  M4 spec, "the dashboard is more honest with 6 working metrics than
  7 with a dead one"; computing real Martin metrics for Python /
  TypeScript / Go / Rust would have meaningfully expanded scope into
  per-language package-boundary heuristics that are themselves a
  research question, so suppression is the correct minimum-viable
  fix.
- **BUG-10** — `crates/tldr-core/src/patterns/signals.rs:198-205`:
  `NamingSignals.{function,class,constant}_names` changed from
  `Vec<(String, NamingCase, String)>` to `Vec<(String, NamingCase,
  String, u32)>` — appended a 4th `u32` element carrying the AST
  start_position line. `crates/tldr-core/src/patterns/naming.rs:71-120`
  (the four iteration sites: `detect_majority_convention`,
  `calculate_consistency`, `find_violations`, plus pattern destructuring)
  was updated to consume the new tuple shape. `find_violations` now
  reads the line element and writes it into
  `NamingViolation.line` instead of the hard-coded `0`. The two
  data-driven `ExtractNamed` sites in `language_profile.rs:160-200`
  capture `name_node.start_position().row + 1` at the dispatch level;
  the per-language semantic extractors in
  `languages/{c,cpp,csharp,elixir,kotlin,lua,ocaml,php,ruby,scala}.rs`
  push `node.start_position().row + 1` directly. The shared
  `push_named` helper also gained a `line: u32` parameter so the new
  data flows uniformly through every dispatch path. Test fixtures
  in `crates/tldr-core/src/patterns/naming.rs` and
  `crates/tldr-core/tests/language_profile_tests.rs` were updated to
  destructure the 4-tuple and exercise the new line invariant.
- **BUG-11** — `crates/tldr-core/src/analysis/deps.rs:462, 647`: the
  two `Ok(DepsReport { ... })` sites in `analyze_dependencies` (one
  for the empty-directory branch, one for the success branch) replaced
  `root: make_relative_path(&root, &root)` with `root: root.clone()`.
  `make_relative_path(&p, &p)` collapses to `PathBuf::new()` because
  it strips the prefix and the prefix is the entire path. The
  canonicalized `root` (line 427) is what consumers actually want as
  "the analyzed directory", so we emit it verbatim. The text
  formatter at `:2683` already does `format!("Dependency Analysis:
  {}\n", report.root.display())`, so the trailing-space header
  resolved itself once the JSON field was populated.
- **BUG-12** — `crates/tldr-cli/src/commands/churn.rs:156-184`: the
  pre-fix code suppressed `summary.most_churned_file` (set to `""`)
  whenever the repo was a degenerate-shallow clone (`is_shallow=true`
  AND `total_unique_commits <= 1`), on the rationale that ranking by
  `commit_count` is meaningless when every file has commit_count=1.
  But the per-file data still has a clean `lines_changed` rank. We
  now refill `most_churned_file` from the file with the highest
  `lines_changed` (descending). The degenerate-rank warning is
  preserved unchanged, so JSON consumers still see the shallow-clone
  caveat — they just also see the actual top-churned file by
  line-count, which is the data they wanted in the first place.
- **BUG-13** — `crates/tldr-core/src/types.rs:1067-1110`: the
  `FileStructure.functions: Vec<String>` and `FileStructure.methods:
  Vec<String>` fields were marked `#[serde(skip_serializing)]` (with
  `#[serde(default)]` so existing snapshots still deserialize), so
  JSON output emits only the canonical object arrays
  (`definitions: Vec<DefinitionInfo>`, `method_infos:
  Vec<MethodInfo>`). The in-memory struct retains both fields for
  internal callers that haven't migrated. `MethodInfo` (lines
  1112-1136) gained `line_end: u32` (`#[serde(default)]`) for parity
  with `DefinitionInfo`, and `crates/tldr-core/src/ast/extractor.rs:
  218-226` populates `MethodInfo.line_end` from
  `DefinitionInfo.line_end` when deriving `method_infos` from
  `definitions`. The `extract` command's method/function objects also
  benefit (see BUG-23 below).
- **BUG-15** — `crates/tldr-core/src/semantic/types.rs:213-241`:
  `SemanticSearchReport` gained `total_results: usize` (`#[serde(
  default)]`). `crates/tldr-core/src/semantic/index.rs:430-440`
  populates it from `results.len()` (alongside the existing
  `matches_above_threshold`). `crates/tldr-core/src/search/enriched.rs:
  84-100` mirrors the same field on `EnrichedSearchReport`, and the
  six early-return sites (empty BM25 index, empty regex matches,
  hybrid empty paths) plus the three success-return sites at
  `:1229-1255` populate `total_results` from `sorted.len()` /
  `sorted_enriched.len()`. The two report types now share the
  `total_results` schema shape — what was previously a `null` key on
  both is now a populated integer.
- **BUG-21** — `crates/tldr-cli/src/commands/contracts/types.rs:622-700`:
  `ChopResult` gained two fields: `file: String` (the analyzed file
  path, mirroring `tldr slice`'s `file` field) and `line_count: u32`
  (alias of `count` matching `tldr slice`'s `line_count` field, kept
  alongside the legacy `count` for back-compat). The two helper
  constructors `ChopResult::same_line` and `ChopResult::no_path`
  initialize `file: String::new()` and the CLI backfills it at the
  call site (`crates/tldr-cli/src/commands/contracts/chop.rs:127-152`)
  with the canonicalized path so every public-facing `ChopResult` has
  a populated `file`. `compute_chop`'s success path
  (`:328-352`) sets `line_count: count` and `file:
  source_or_path.to_string()`. The schemas of `slice` and `chop` are
  now aligned: both expose `file`, `function`, `lines`, `line_count`,
  and a 1-indexed range — consumers can switch between them without
  reshaping the JSON.
- **BUG-22** — `crates/tldr-cli/src/commands/patterns/types.rs:497-510`:
  `InterfaceInfo.all_exports` changed from `Option<Vec<String>>` to
  `Vec<String>` (with `#[serde(default)]` so legacy snapshots that
  carried `null` still deserialize as `[]`). `crates/tldr-cli/src/
  commands/patterns/interface.rs:1294-1320` populates the field with
  a non-null fallback chain: prefer the explicit Python `__all__`
  (still extracted by `extract_all_exports` for Python only); else
  fall back to the union of public function names and public class
  names (sorted, deduped — mirroring the "import *" semantics that
  apply when `__all__` is absent). For non-Python languages the
  fallback is the same union of public symbols. Empty modules return
  `[]` (empty array), never `null`. The text formatter
  (`:1411-1418`) was updated to print "Exports:" (not "Exports
  (`__all__`):") since the field no longer carries explicit-only
  semantics. **Scope note**: per the M4 spec, the implementation is
  Python-first (preferring `__all__`) but the union-fallback path
  works identically for every language with public functions/classes
  in the report — no separate language-specific extractor is needed.
- **BUG-23** — `crates/tldr-core/src/types.rs:1184-1259, 1265-1338,
  1347-1416`: `FunctionInfo`, `ClassInfo`, and `FieldInfo` each gained
  a `line_end: u32` field (`#[serde(default)]`), and their manual
  `Serialize` impls were rewritten to emit `line` and `line_end` (in
  that order) and intentionally OMIT `line_number` (which was
  redundant with `line` since the BUG-17 alias). `Deserialize` now
  carries `#[serde(alias = "line")]` on the in-memory `line_number`
  field so legacy snapshots that emitted `line_number` still
  round-trip. `crates/tldr-core/src/ast/extract.rs` got 49 mechanical
  updates: every `let line_number = X.start_position().row as u32 +
  1;` now emits a paired `let line_end = X.end_position().row as u32
  + 1;`, and every constructor adds `line_end,` next to the existing
  `line_number,`. The same pattern was applied to test fixtures in
  `crates/tldr-cli/src/output_tests.rs`,
  `crates/tldr-core/src/{analysis/dead.rs,callgraph/builder.rs,context/builder.rs,search/enriched.rs,surface/{python,typescript,go,rust_lang}.rs}`,
  and the integration tests in `crates/tldr-core/tests/{types_base_tests,
  field_extraction_test}.rs` and
  `crates/tldr-cli/tests/{unicode_truncation_test,patterns_test}.rs`.
  Net effect: the canonical schema for any line-bearing object in
  tldr is `{ line, line_end }` with no `line_number` duplicate, and
  `extract`/`structure` consumers can compute function/class length
  without an additional AST query.

### Tests added

- `crates/tldr-cli/tests/schema_cleanup_v1.rs` — 11 regression tests
  (one per bug, +1 split between structure schema and method_infos
  line_end), all GREEN. Covers BUG-9 through BUG-23.

### Verification

- M0 (`vuln_migration_v1_red`): 168/168 GREEN
- M1 (`determinism_and_stderr_hygiene_v1`): 5/5 GREEN
- M2 (`cross_command_consistency_v1`): 7/7 GREEN
- M3 (`detection_accuracy_v1`): 4/4 GREEN
- M4 (`schema_cleanup_v1`): 11/11 GREEN
- `tldr --help \| grep -E '^  (similar\|semantic\|embed)' \| wc -l` → 3
- Reinstalled at both `~/.cargo/bin/tldr` and `~/.local/bin/tldr`,
  codesigned

## detection-accuracy-v1 — internal milestone

NOT a published release. Closes 4 audit-found bugs that gave wrong or
misleading answers to security and dead-code questions: `tldr dead`
flagged 259 functions on ripgrep — 100% of which were
`#[test]`-marked or lived inside a `#[cfg(test)] mod tests {}` block,
because the Rust function extractor never read attribute siblings or
walked enclosing `mod_item` ancestors, so every `dead`-marked entry
arrived at the dead-code analyzer with `is_test: false`; `tldr vuln`
labelled Express/NestJS/Fastify/Next.js redirect sinks
(`res.redirect`, `reply.redirect`, `NextResponse.redirect`, bare
`redirect()`) as `path_traversal` / CWE-22 / "FileWrite with
unsanitized input", because every redirect pattern in the JS sink
bank was wired to `TaintSinkType::FileWrite` which projected to
`VulnType::PathTraversal` via `vuln_type_from_sink` — wrong CWE,
wrong vuln-type, wrong remediation; the canonical taint engine emitted
"degenerate" findings whose source and sink were the SAME statement
(e.g. `let file = File::open(path)?;` — `path` is tainted untrusted
data and `File::open` is the FileOpen sink, both on one line),
producing JSON `taint_flow` arrays with two identical entries that
misrepresented a one-step direct invocation as a multi-step
propagation; and `tldr references` emitted `definition: <single
object>` even when multiple definitions existed (flask
`_make_timedelta` is defined in BOTH `src/flask/sansio/app.py:52`
AND `src/flask/app.py:73`), AND the text formatter hard-coded
"Definition:" (singular) regardless of count, even when listing two
or more entries underneath. Binary-verified against
`/tmp/repos/ripgrep`, `/tmp/repos/express`, `/tmp/repos/ts-dom-gen`,
`/tmp/repos/flask` and gated by 4 regression tests in
`crates/tldr-cli/tests/detection_accuracy_v1.rs`.
`vuln_migration_v1_red`: 168/168 GREEN. M1
(`determinism_and_stderr_hygiene_v1`): 5/5 GREEN. M2
(`cross_command_consistency_v1`): 7/7 GREEN.

| Bug | File:Line | Before | After |
|-----|-----------|--------|-------|
| BUG-4 | `crates/tldr-core/src/ast/extract.rs:2671-2783` (new `extract_rust_function_attributes` + `parse_rust_attribute_item`); `crates/tldr-core/src/analysis/dead.rs:567-590` (extended `has_test_decorator`) | `tldr dead /tmp/repos/ripgrep` → 259 entries in `possibly_dead`, ALL with `is_test: false`, including `config_error_heap_limit` at `crates/searcher/src/searcher/mod.rs:1053` (a `#[test] fn` inside `#[cfg(test)] mod tests {}`) | 13 entries (down 95%); `config_error_heap_limit` is gone; every test-marked function (`#[test]`, `#[tokio::test]`, fns inside `mod tests {}` / `#[cfg(test)] mod ...`) is excluded |
| BUG-16 | `crates/tldr-core/src/security/taint.rs:174-179` (new `TaintSinkType::OpenRedirect`); `crates/tldr-core/src/security/taint.rs:1995-2098` (rerouted JS redirect entries from `FileWrite` to `OpenRedirect`); `crates/tldr-core/src/security/vuln.rs:60-95`, `:213-225`, `:357-385` (new `VulnType::OpenRedirect` + projection + CWE + remediation); `crates/tldr-cli/src/commands/remaining/vuln.rs:592-595` (CLI map) | `tldr vuln /tmp/repos/express \| jq .findings[0]` → `vuln_type: "path_traversal"`, `cwe_id: "CWE-22"`, `description: "FileWrite with unsanitized input"`, `Sink: FileWrite` for `res.redirect('/user/' + id)` | `vuln_type: "open_redirect"`, `cwe_id: "CWE-601"`, `description: "OpenRedirect with unsanitized input"`, `Sink: OpenRedirect`; FileWrite/PathTraversal regressions guarded by 168/168 RED suite |
| BUG-17 | `crates/tldr-core/src/security/vuln.rs:774-810` (engine-level same-line+same-var+same-statement suppression for the source-equals-source double-counting case); `crates/tldr-cli/src/commands/remaining/vuln.rs:493-554` (CLI-level direct-sink annotation: collapses `taint_flow` to a single entry + sets `direct_sink: true` when source.expression == sink.expression on the same line) | `tldr vuln --lang typescript /tmp/repos/ts-dom-gen \| jq .findings[0].taint_flow` → two identical entries with `code_snippet: "const content = await fs.readFile(...)"` differing only by `description: "Source: Untrusted file read"` vs `"Sink: FileOpen"` | single-element `taint_flow` with `description: "Direct sink: FileOpen (source: Untrusted file read)"` and a top-level `direct_sink: true` field on the finding; ZERO findings on `/tmp/repos/ripgrep` and `/tmp/repos/ts-dom-gen` have a degenerate dual-entry flow |
| BUG-20 | `crates/tldr-core/src/analysis/references.rs:65-127` (added `definitions: Vec<Definition>` to `ReferencesReport`); `:2724-2780` (new `find_definitions` plural API; singular `find_definition` is now a first-element view); `:3270-3340` (`find_references` populates both fields); `crates/tldr-cli/src/commands/references.rs:233-300` (text formatter prefers `definitions`, prints "Definitions:" plural when count > 1) | `tldr references _make_timedelta /tmp/repos/flask --format text` → header "Definition:" (singular) followed by 2 entries; JSON `.definition` is a single object | text header is "Definitions:" with both entries listed; JSON now exposes `definitions: [...]` as a 2-entry array (singular `definition` retained for back-compat as the first element) |

### Changed

- **BUG-4** — `crates/tldr-core/src/ast/extract.rs`: every Rust
  `function_item` extracted by `extract_rust_function_info` now runs
  through a new `extract_rust_function_attributes` helper that (a)
  walks `prev_sibling` for `attribute_item` nodes (skipping
  doc-comment `line_comment` / `block_comment` siblings interleaved
  between attributes) and parses each `#[ ... ]` form via
  `parse_rust_attribute_item`, and (b) walks the chain of enclosing
  `mod_item` ancestors. For each ancestor module it (i) checks the
  module name against a heuristic — `test`, `tests`, `test_*`,
  `*_test`, `*_tests`, anything containing `testutil` — and (ii)
  inspects that module's own `prev_sibling` attributes for
  `#[cfg(test)]` / `#[cfg_attr(test, ...)]`. Either signal causes a
  synthetic `cfg(test)` decorator string to be appended. The dead-
  code analyzer's existing `has_test_decorator` predicate (now in
  `crates/tldr-core/src/analysis/dead.rs:567-590`) is extended to
  recognise `cfg(test)`, `cfg_attr(test, ...)`, `tokio::test`,
  `async_std::test`, `wasm_bindgen_test`, `rstest`, `proptest`, plus
  any decorator containing `::test`. The downstream chain
  (`collect_all_functions` → `is_test = … || has_test_decorator(...)`
  → dead-code filter) was already correct; the entire bug was in the
  empty-decorator pipeline plumbing.
- **BUG-16** — `crates/tldr-core/src/security/taint.rs`: introduced
  `TaintSinkType::OpenRedirect` as a peer to the existing
  `FileOpen`/`FileWrite`/`HttpRequest`/`HtmlOutput`/etc. variants and
  rerouted every JS redirect sink pattern there:
  `(NextResponse|Response, redirect)`, bare `redirect(...)` from
  `next/navigation`, `(reply, redirect)` (Fastify),
  `(res|response, redirect)` (Express/NestJS). `(reply, header)`
  stays under `FileWrite` — there is no dedicated header-injection
  sink yet. `crates/tldr-core/src/security/vuln.rs`: added
  `VulnType::OpenRedirect`, extended `vuln_type_from_sink`,
  `get_remediation`, `get_cwe_id` (CWE-601), `Display`, and
  `sink_type_precedence` (rank 35, below SSRF). The CLI mapping in
  `crates/tldr-cli/src/commands/remaining/vuln.rs:584-595` (the
  match is deliberately exhaustive — no `_` arm — so adding a new
  core variant is a hard compile error) was extended with the
  `OpenRedirect → OpenRedirect` arm. The CLI's `VulnType` enum
  already had an `OpenRedirect` variant from a prior milestone; the
  display name, CWE, and severity defaults flow through unchanged.
- **BUG-17** — `crates/tldr-core/src/security/vuln.rs`: added the
  same-line+same-var+same-statement suppression in `scan_file_vulns`
  before the dedupe phase. The `source.var == sink.var` guard is
  load-bearing: a single statement can legitimately host BOTH a
  source (`id = params[:id]` introduces tainted `id`) AND a sink
  consuming that `id` later on the same line (e.g. Ruby/Lua's
  `db.execute("... " + id)` from the v1 RED suite); empirically
  these have distinct `sink.var` (the call-expression text vs the
  source variable), so the guard leaves them alone while killing the
  Python-style same-var double-counting class. The complementary
  CLI-level fix in `crates/tldr-cli/src/commands/remaining/vuln.rs`
  detects the `source.expression == sink.expression && source.line ==
  sink.line && !source.expression.is_empty()` shape that survives
  the engine-level filter (typically Rust `let f = File::open(path)?`
  where `source.var = file` ≠ `sink.var = path` but both records
  point to the same statement) and emits a single-element `taint_flow`
  with `description: "Direct sink: <Sink> (source: <Source>)"` plus
  `direct_sink: true` on the finding. The new `direct_sink: bool`
  field on `crates/tldr-cli/src/commands/remaining/types.rs:1551-1592`
  is `#[serde(default, skip_serializing_if = "is_false")]` so it
  vanishes from the JSON for non-degenerate findings (zero schema
  bloat on the common path).
- **BUG-20** — `crates/tldr-core/src/analysis/references.rs`: split
  `find_definition` into a thin first-element view over the new
  `find_definitions` plural API. Both walk every source file under
  the project root, collect `Definition` candidates, sort by
  canonical-def tier (src > non-test > test) then path then line.
  Pre-fix the singular helper would `break` on the first match,
  hiding additional definitions in lower-priority files. The
  `ReferencesReport` struct gains a `definitions: Vec<Definition>`
  field (always serialized) and retains `definition: Option<Definition>`
  populated as `definitions.first().cloned()` for backward
  compatibility with every existing JSON consumer (the singular
  field is `#[serde(skip_serializing_if = "Option::is_none")]` so
  no-match cases continue to emit `definitions: []` without the
  singular key). `crates/tldr-cli/src/commands/references.rs:233-300`:
  the text formatter now prefers `report.definitions` over
  `report.definition`, prints `"Definitions:"` (plural) when the
  count exceeds one, and lists every entry with its file/line/column
  and signature.

### Tests

- New regression file `crates/tldr-cli/tests/detection_accuracy_v1.rs`
  with 4 tests: `rust_test_attribute_excluded_from_dead` (BUG-4),
  `js_redirect_classified_as_open_redirect` (BUG-16),
  `degenerate_source_eq_sink_suppressed_or_annotated` (BUG-17),
  `references_definitions_array_and_text_header_plural` (BUG-20).
- M2 (`cross_command_consistency_v1`): 7/7 GREEN.
- M1 (`determinism_and_stderr_hygiene_v1`): 5/5 GREEN.
- Master regression (`vuln_migration_v1_red`): 168/168 GREEN.

## cross-command-consistency-v1 — internal milestone

NOT a published release. Closes 4 audit-found bugs that made command
output disagree with itself across the surface: `tldr impact` reported
`caller_count: 0` for any function that was used only as a value
(returned, assigned, passed as kwarg, or stashed in a class-body field)
even though `tldr references` had no trouble finding the same use
sites; `tldr complexity` and `tldr cognitive` returned different
cognitive numbers for the same function because two separate
calculators existed (the standalone `cognitive` command was the
canonical SonarSource v1.4 implementation; `complexity` carried a
drifted older calculation); on macOS `tldr halstead`, `tldr cognitive`,
and `tldr dead-stores` rewrote `/tmp/...` paths to `/private/tmp/...`
in their JSON `file` field while `tldr reaching-defs` echoed the
input path unchanged, so two commands run on the same input emitted
different paths; the project-root field name was `path` for `health` /
`secure`, `project_path` for `inheritance`, and `root` for everyone
else (`structure` / `deps` / `clones` / ...), and the function-name
field was `function_name` for `taint` / `explain` while the rest of
the function-scoped surface (`slice` / `dead-stores` / `resources` /
`reaching-defs`) used `function`. Binary-verified against
`/tmp/repos/flask` and gated by 7 regression tests in
`crates/tldr-cli/tests/cross_command_consistency_v1.rs`.
`vuln_migration_v1_red`: 168/168 GREEN. M1
(`determinism_and_stderr_hygiene_v1`): 5/5 GREEN. All prior
milestones still hold.

| Bug | File:Line | Before | After |
|-----|-----------|--------|-------|
| BUG-5 | `crates/tldr-core/src/callgraph/var_types.rs:43-191` (rewrote `extract_python_definitions`; new `collect_python_value_refs`) | `tldr impact _make_timedelta /tmp/repos/flask` → `caller_count: 0` for both definitions | `caller_count: 1` (App class body) for `src/flask/sansio/app.py:_make_timedelta`; matches `tldr references` |
| BUG-7 | `crates/tldr-core/src/metrics/cognitive.rs:443-491` (new `calculate_cognitive_for_function`); `crates/tldr-core/src/metrics/complexity.rs:49-95` + `:122-175` (delegate); `crates/tldr-core/src/types.rs:2410-2430` (rename + alias) | `tldr complexity flask/app.py make_response \| jq .cognitive` → 45; `tldr cognitive flask/app.py \| jq '.functions[]\|select(.name=="make_response").cognitive'` → 26 | both → 26; verified for 3 python + 3 js functions |
| BUG-8 | `crates/tldr-cli/src/commands/halstead.rs:85-99`; `crates/tldr-cli/src/commands/cognitive.rs:85-99`; `crates/tldr-cli/src/commands/contracts/dead_stores.rs:91-107`; `crates/tldr-cli/src/commands/patterns/resources.rs:3440-3551` | `tldr halstead /tmp/repos/flask/src/flask/app.py \| jq .functions[0].file` → `/private/tmp/repos/flask/...` | echoes the user-supplied path (`/tmp/repos/flask/...`) verbatim; same for `cognitive`, `dead-stores`, `resources` |
| BUG-14 | `crates/tldr-core/src/quality/health.rs:406-415` (`path` → `root`); `crates/tldr-cli/src/commands/remaining/types.rs:327-336` (`path` → `root`); `crates/tldr-core/src/types/inheritance.rs:280-298` (`project_path` → `root`); `crates/tldr-core/src/security/taint.rs:240-250` (`function_name` → `function`); `crates/tldr-cli/src/commands/remaining/types.rs:707-765` (`function_name` → `function` in `ExplainReport` Serialize impl); `crates/tldr-core/src/types.rs:2410-2430` (`nesting_depth` → `max_nesting`) | `tldr health … \| jq .root` → null; `tldr inheritance … \| jq .root` → null; `tldr taint … \| jq .function` → null | every project-level command (`structure`/`deps`/`clones`/`health`/`secure`/`inheritance`) emits `root`; every function-scoped command (`slice`/`dead-stores`/`resources`/`reaching-defs`/`taint`/`explain`) emits `function`. Old field names accepted on deserialise via `#[serde(alias = "...")]`. |

### Changed

- **BUG-5** — `crates/tldr-core/src/callgraph/var_types.rs`: extended
  `extract_python_definitions` (the Python extractor used by the v2
  builder — distinct from `crates/tldr-core/src/callgraph/languages/python.rs`,
  which is exercised by direct unit tests but not by the project-wide
  pipeline) to (a) collect a `HashSet<String>` of locally-defined
  function/class names up-front and (b) walk function bodies AND class
  bodies for free-identifier uses that resolve to that set, emitting
  one `CallType::Ref` call site per (caller, target) pair. The new
  helper `collect_python_value_refs` skips the identifier-form of a
  call's `function` field (already handled by `parse_python_call`),
  the function/class-definition `name` field, attribute-access
  `attribute` fields, and parameter lists, so this change does not
  resurrect spurious self-edges. The resolver path was already
  correct: `crates/tldr-core/src/callgraph/resolution.rs:812-848`
  resolves `CallType::Ref` exactly like `Direct` (local + import_map
  + reexport tracer), so adding the call sites is sufficient. Scoped
  to Python only — JS/TS/Rust/Java/Go scanners were not touched.
- **BUG-7** — `crates/tldr-core/src/metrics/cognitive.rs`: exposed the
  internal `CognitiveCalculator` via a new public `CognitiveScore`
  struct + `calculate_cognitive_for_function(function_name, source,
  language, func_node)` helper that runs the canonical SonarSource
  v1.4 implementation on a single tree-sitter function node and
  returns `(cognitive, max_nesting, nesting_penalty)`.
  `crates/tldr-core/src/metrics/complexity.rs`:
  `calculate_complexity` and `calculate_all_complexities_from_tree`
  now delegate the cognitive number AND the nesting depth to that
  helper after running the existing cyclomatic / LOC pass. The
  drifted second cognitive implementation in `ComplexityCalculator`
  (lines 313-367) still computes a value but it is overwritten before
  the metrics are returned, so callers always see the canonical
  number. `crates/tldr-core/src/types.rs`: `ComplexityMetrics`'s
  nesting field is renamed Rust-side and JSON-side from
  `nesting_depth` to `max_nesting` so it matches `cognitive`'s
  field name; `#[serde(alias = "nesting_depth")]` keeps
  deserialisation of older bodies working.
- **BUG-8** — `crates/tldr-cli/src/commands/halstead.rs`,
  `crates/tldr-cli/src/commands/cognitive.rs`,
  `crates/tldr-cli/src/commands/contracts/dead_stores.rs`,
  `crates/tldr-cli/src/commands/patterns/resources.rs`: each command
  still calls `validate_file_path` for existence / traversal checks
  but DISCARDS the canonicalised return value when emitting the
  output — the `file` (or `path` / `root`) field in the JSON now
  echoes back `self.file` / `self.path` / `args.file` exactly as the
  caller typed it. On macOS this stops `/tmp/...` from being silently
  rewritten to `/private/tmp/...`. The fs/IO operations themselves
  use the user-supplied path, which `std::fs::read_to_string` resolves
  through the same symlink chain, so behaviour is unchanged. Did NOT
  touch `crates/tldr-core/src/validation.rs::validate_file_path`
  itself — that function is shared between CLI and daemon handlers
  and other call sites depend on its canonical-path return for
  internal caching.
- **BUG-14** — JSON field-name unification, all backwards-compatible
  on the deserialise side via `#[serde(alias = "...")]`:
  `crates/tldr-core/src/quality/health.rs:411` —
  `path` renamed to `root` in JSON (Rust field still `path`).
  `crates/tldr-cli/src/commands/remaining/types.rs:331` —
  `SecureReport.path` renamed to `root` in JSON.
  `crates/tldr-core/src/types/inheritance.rs:297` —
  `InheritanceReport.project_path` renamed to `root` in JSON.
  `crates/tldr-core/src/security/taint.rs:242` —
  `TaintInfo.function_name` renamed to `function` in JSON.
  `crates/tldr-cli/src/commands/remaining/types.rs:710` and `:736-765`
  — `ExplainReport.function_name` renamed to `function` in the
  custom Serialize impl that already emitted the unified `line`
  field; the deserialise side accepts both via
  `#[serde(alias = "function")]`.
  `crates/tldr-core/src/types.rs:2425` (also part of BUG-7) —
  `ComplexityMetrics.nesting_depth` renamed to `max_nesting`.
  Test assertions that previously asserted on the old keys
  (`crates/tldr-cli/tests/cli_p1_tests.rs:600-602`,
  `crates/tldr-cli/tests/cli_search_context_tests.rs:573-576`,
  `crates/tldr-core/tests/types_base_tests.rs:1042-1055`,
  `crates/tldr-core/tests/cfg_tests.rs:384-401`,
  `crates/tldr-core/tests/metrics_tests.rs:285-296` + `:1490-1497`,
  `crates/tldr-core/tests/bench_quality_multilang.rs:2185-2287`,
  `crates/tldr-core/src/quality/health.rs:1271-1279`) updated to
  assert on the canonical names.

### Added

- `crates/tldr-cli/tests/cross_command_consistency_v1.rs` — 7
  regression tests:
  - `impact_finds_function_as_value_callers` — builds a Python
    project where one helper is used direct + as a return value +
    as a kwarg + as a positional `map` arg + inside a class body and
    asserts `caller_count >= 4`. The `note` field is asserted NOT to
    contain "no callers found" when callers exist.
  - `complexity_and_cognitive_agree_on_same_function_python` — runs
    both commands against three Python functions of increasing
    cognitive complexity and asserts they emit the same number.
  - `complexity_and_cognitive_agree_on_same_function_js` — same as
    above for three JavaScript functions, satisfying the spec's
    "at least 2 languages" requirement.
  - `complexity_emits_max_nesting_field_renamed_from_nesting_depth`
    — asserts that `tldr complexity --format json` exposes
    `max_nesting` and NOT `nesting_depth`.
  - `path_canonicalization_consistent_across_commands` — runs five
    commands (`halstead`, `cognitive`, `reaching-defs`, `dead-stores`,
    `resources`) against the same single file and asserts all five
    `.file` values equal the user-supplied path string verbatim.
  - `project_root_field_name_canonical` — runs six commands
    (`structure`, `deps`, `clones`, `health`, `secure`, `inheritance`)
    against a multi-file Python project and asserts all six expose a
    top-level `root` key (and do NOT expose legacy `path` /
    `project_path` alongside).
  - `function_name_field_canonical` — runs six function-scoped
    commands (`slice`, `dead-stores`, `resources`, `reaching-defs`,
    `taint`, `explain`) and asserts all six expose `.function` (and
    NOT `.function_name`).

### Verification

- `cargo test --release --features semantic -p tldr-cli --test
  vuln_migration_v1_red`: **168/168 GREEN** (master regression suite).
- `cargo test --release --features semantic -p tldr-cli --test
  determinism_and_stderr_hygiene_v1`: **5/5 GREEN** (M1 regression
  suite from previous milestone).
- `cargo test --release --features semantic -p tldr-cli --test
  cross_command_consistency_v1`: **7/7 GREEN** (this milestone).
- Spot-verifications run against `/tmp/repos/flask`:
  - BUG-5: `tldr impact _make_timedelta /tmp/repos/flask` →
    `caller_count: 1` (from `App` class body) for the
    `src/flask/sansio/app.py` definition.
  - BUG-7: `tldr complexity .../flask/app.py make_response` and
    `tldr cognitive .../flask/app.py | jq …` both emit 26.
  - BUG-8: `tldr halstead /tmp/repos/flask/src/flask/app.py` emits
    `/tmp/repos/flask/...` (no `/private/` prefix); same for
    `cognitive`, `reaching-defs`, `dead-stores`, `resources`.
  - BUG-14: every project-level command emits `root`; every
    function-scoped command emits `function`.

### Pre-existing test failure (NOT caused by this milestone)

- `cargo test -p tldr-core --test rr_module_function_integ_test
  ruby_io_popen_with_user_input_via_compute_taint` was failing on
  HEAD (`5ba0c90`) before this milestone began work and is still
  failing after. The test is part of the
  `field_access_info-v1 M1: 24 RED integration tests` set
  (commit `49ed30c`, 2026-04-29) — they were intentionally
  introduced as RED and have not yet been turned GREEN. This
  milestone touches Python call-graph extraction and shared serde
  field names; it does not touch Ruby taint detection. Out of scope.

## determinism-and-stderr-hygiene-v1 — internal milestone

NOT a published release. Closes 4 audit-found bugs that broke CI
integrations and byte-stable output: `tldr vuln` exited with code 2 and
"Error: 1 findings detected" on stderr whenever a scan completed with
non-empty findings (every successful-with-findings run looked like a
tool failure to CI; grammar disagreed with count); `tldr clones`
HashMap-iteration order shuffled `clone_pairs[]` across runs (and, when
`max_clones` truncated, even retained DIFFERENT pairs); `tldr hubs`
PageRank produced non-deterministic top-N and last-digit float drift
because the iterative reduction walked a `HashSet<FunctionRef>` per
iteration; `tldr inheritance` and `tldr smells` leaked progress /
advisory text to stderr in JSON mode, breaking shell pipelines that
gate on stderr-empty. Binary-verified against `/tmp/repos/{express,
flask}` and gated by 5 regression tests in
`crates/tldr-cli/tests/determinism_and_stderr_hygiene_v1.rs`.
`vuln_migration_v1_red`: 168/168 GREEN. All AA/AB/AC/AD/AE/AF prior
milestones still hold.

### Changed

- **BUG-1** — `crates/tldr-cli/src/commands/remaining/vuln.rs:312-323`:
  removed the `Err(RemainingError::findings_detected(_))` return arm
  that fired whenever `filtered_findings` was non-empty. The CLI now
  exits 0 on any successful scan regardless of finding count, mirroring
  `tldr secure` (which already returned `Ok(())` on completion). The
  count is conveyed via `summary.total_findings` in the JSON / SARIF
  output for consumers that want to branch on it. Updated
  `crates/tldr-cli/tests/remaining_test.rs:test_vuln_exit_code_findings`
  to assert exit 0 + empty stderr (was: exit 2).
- **BUG-2** — `crates/tldr-core/src/analysis/clones/detect.rs:34-99`
  and `:185-289`: replaced direct `HashMap::values()` walks of the
  raw-hash and normalized-hash bucket indexes with sorted-key views
  (`raw_hash_keys.sort_unstable()` / `norm_hash_keys.sort_unstable()`).
  Sorted the `shared_counts` HashMap entries by `other_idx` before the
  bounded Type-3 loop. Final `clone_pairs[]` is then sorted in
  `crates/tldr-core/src/analysis/clones/mod.rs:117-145` by
  `(fragment1.file, fragment1.start_line, fragment1.end_line,
  fragment2.file, fragment2.start_line, fragment2.end_line, clone_type,
  similarity)` BEFORE id assignment so the 1-indexed `id` field is
  also stable.
- **BUG-3** — `crates/tldr-core/src/analysis/hubs.rs:602-685` (PageRank
  iteration): materialize a deterministic `sorted_nodes: Vec<FunctionRef>`
  ONCE (sorted by `(file, name)`, the FunctionRef identity tuple per
  the PartialEq/Hash impl at `crates/tldr-core/src/types.rs:1429-1443`)
  and walk that on every iteration instead of the input
  `HashSet<FunctionRef>` — the float reduction is now associative-stable
  across processes. Each `reverse_graph[node]` callers slice is also
  sorted before the inner reduction. Top-N selection in
  `crates/tldr-core/src/analysis/hubs.rs:1364-1395` now adds a
  `(file, name)` final tiebreaker on the primary `composite_score`
  sort and on each `by_in / by_out / by_pr / by_bc` breakdown, so
  equal-score ties no longer fall through to original-Vec order
  (which itself was HashSet-derived).
- **BUG-18** — `crates/tldr-cli/src/commands/inheritance.rs:132-156`:
  gated the `Found N classes in Mms` summary and diamond-inheritance
  warning behind `writer.is_text()` so JSON consumers see an empty
  stderr; text consumers still get the summary.
  `crates/tldr-cli/src/commands/smells.rs:219-289`: removed the
  unconditional `eprintln!` of the `--deep` advisory hint; the same
  string is now pushed into `SmellsReport.warnings[]` (new field on
  `crates/tldr-core/src/quality/smells.rs:264-289`, `#[serde(default)]`
  for daemon-cache backward compatibility), which the text formatter
  renders to stdout (`crates/tldr-cli/src/output.rs:1059-1080`). Net
  effect: stderr empty in both formats, `warnings[]` introspectable
  in JSON, hint visible to text users on stdout.

### Architectural note

All four fixes preserve existing semantics:

- BUG-1 keeps the JSON/SARIF schema identical and matches the parity
  contract already in `vuln_secure_autodetect_parity_v1.rs` (which
  accepted exit 0 OR 2; now both `vuln` and `secure` always exit 0 on
  successful scans).
- BUG-2's sort_unstable on `u64` hash keys is total and deterministic
  per-process — the surviving pair set under `max_clones` is now a
  function of bucket-key order rather than DefaultHasher seed.
- BUG-3 routes every iteration of the PageRank loop through one
  canonical `sorted_nodes` list, so `incoming_contrib` is summed in
  the same order on every run. Float values themselves are now
  byte-stable; the tiebreakers exist primarily for the integer-valued
  `by_in_degree` / `by_out_degree` breakdowns where ties are common.
- BUG-18 routes the `--deep` advisory through a structured
  `warnings[]` field rather than removing it; both JSON consumers
  (`jq '.warnings[]'`) and text users (rendered by the formatter)
  retain visibility.

### Retained

- `RemainingError::FindingsDetected` variant and `findings_detected()`
  constructor are kept in
  `crates/tldr-cli/src/commands/remaining/error.rs` — no live producer
  but the variant remains in case a future `--strict` flag wants to
  re-introduce findings-as-error semantics opt-in.
- Existing `vuln_autodetect_tests.rs:test_vuln_errors_on_unsupported_autodetected_lang`
  still asserts exit 2 for the *unsupported autodetect language* path
  (a real error condition, not a successful scan with findings). The
  fix only removes the success-with-findings exit-2 path.
- All tldr-core unit tests (4819) pass unchanged. The new
  `SmellsReport.warnings` field is `#[serde(default)]` so existing
  daemon JSON cache entries deserialize cleanly.

### Quantification

| Bug | Repro | Before | After |
| --- | --- | --- | --- |
| BUG-1 | `tldr vuln /tmp/repos/express; echo $?` | exit 2, `Error: 1 findings detected` on stderr | exit 0, stderr empty |
| BUG-2 | 5 runs of `tldr clones /tmp/repos/flask` `md5 -q` (ignoring `stats.detection_time_ms`) | 5 distinct hashes (different `clone_pairs[]` entries AND order) | 1 hash |
| BUG-3 | 5 runs of `tldr hubs /tmp/repos/{express,flask}` `md5 -q` | 5 distinct hashes (PageRank last-digit drift, top-N shuffled) | 1 hash per repo |
| BUG-18 | `tldr inheritance /tmp/repos/flask 2>err > /dev/null; wc -c err` | 23 bytes ("Found 63 classes in 39ms") | 0 bytes |
| BUG-18 | `tldr smells /tmp/repos/flask 2>err > out.json; wc -c err` | ~145 bytes ("Note: 8 smell analyzers require --deep flag …") | 0 bytes; same string now in `out.json`'s `warnings[]` |

### Standing rules upheld

- `Cargo.lock` not staged (no manifest changes).
- No push, no `cargo publish`, no version bump (still v0.3.0).
- 168/168 `vuln_migration_v1_red` GREEN before and after.
- 5/5 new `determinism_and_stderr_hygiene_v1` tests GREEN.
- All 4819 `tldr-core` unit tests GREEN.
- Pre-existing test failures in `remaining_test::definition_command::test_definition_invalid_position`,
  `remaining_test::secure_command::test_secure_detects_taint`, and
  `tldr-core::tests::rr_module_function_integ_test::ruby_io_popen_with_user_input_via_compute_taint`
  are unrelated to this milestone — none of those code paths were
  touched (verified via `git diff --stat HEAD`).

## hubs-line-population-v1 — internal milestone

NOT a published release. Surgical fix for a single MED bug surfaced by
the post-AD-bundle real-repo audit: every hub returned by `tldr hubs`
had `function_ref.line: 0`, regardless of where the function was
actually defined. Binary-verified against `/tmp/repos/flask` and gated
by 4 regression tests in
`crates/tldr-cli/tests/hubs_line_population_v1.rs` covering Python
(top-level + class method), Rust, and JavaScript.
`vuln_migration_v1_red`: 168/168 GREEN. All AA/AB/AC/AD/AE prior
milestones still hold.

### Bug — `tldr hubs` always emitted `function_ref.line: 0`

```bash
$ tldr hubs /tmp/repos/flask --quiet \
    | jq '[.hubs[].function_ref.line] | unique'
[0]
```

Every hub in the report had `line: 0`, including `Scaffold.route`,
which is actually defined at `src/flask/sansio/scaffold.py:336`.
Downstream consumers (IDE jumps, code-review summaries, AI agents
asking "where is this hub?") had no way to locate the function from
the JSON without an extra grep step.

**Root cause.** The hub analysis layer builds its node set from
`ProjectCallGraph` edges via `graph_utils::collect_nodes`, which
constructs each `FunctionRef` with only `(file, name)` — `line`
defaults to `0` (per the `FunctionRef::new` convention,
`types.rs:1401`: "0 = unknown"). No reconciliation against the AST
extractor was ever done.

**Fix.** Added a public canonical-line enumerator in
`tldr_core::analysis::hubs`:

- `enumerate_function_lines(root, language) -> FunctionLineLookup` —
  walks the project with `ProjectWalker` (so `.gitignore`,
  `node_modules`, `target` etc. are honored), parses each file with
  `crate::ast::extract_file`, and indexes every top-level function
  plus every class method (by both bare `name` and qualified
  `Class.method`) by `(relative_file_path, name) -> 1-based line`.
  File keys use forward-slashed relative paths so they match the
  `FunctionRef.file` produced by the call-graph builder
  (`cross_file_types::normalize_path_buf`).
- `compute_hub_report_with_lines(...)` — same shape as the existing
  `compute_hub_report`, plus a `function_line_lookup: Option<&...>`
  parameter. After building each `HubScore`, looks up the function
  by `(file, name)` and overwrites `function_ref.line` from the
  AST. Misses leave the field at `0` (matches existing convention).

`compute_hub_report` is preserved as a backward-compatible
delegate that passes `None` for the lookup, so existing internal
callers (`bench_l2_multilang`, hub unit tests) keep working
unchanged.

The CLI (`tldr hubs`) builds the lookup once per invocation
between `build_project_call_graph` and `compute_hub_report_with_lines`,
amortizing the AST walk across the analysis.

**Result.** Post-fix on Flask, the unique line set is
`[0, 174, 233, 336, 567, 602, 645, 701]` — `Scaffold.route` is now
correctly `336`, `Blueprint.__init__` is `174`, `App.add_url_rule`
is `602`, etc. The remaining `0`s are nodes the call-graph builder
attributed to the wrong file (e.g., `Flask.__init__` recorded as
defined in `tests/test_config.py`); that is a distinct call-graph
builder bug and out of scope for this milestone — the lookup
correctly fails closed when the file/name pair is not in the
canonical extractor.

Files:
- `crates/tldr-core/src/analysis/hubs.rs` — added
  `enumerate_function_lines`, `FunctionLineLookup`,
  `compute_hub_report_with_lines`; populates `function_ref.line`
  inside the score-construction loop.
- `crates/tldr-core/src/analysis/mod.rs` — re-exports the new
  symbols.
- `crates/tldr-cli/src/commands/hubs.rs` — calls
  `enumerate_function_lines` and switches to
  `compute_hub_report_with_lines`.

Tests: `crates/tldr-cli/tests/hubs_line_population_v1.rs` (4 tests):
- `test_hubs_line_populated_python` — top-level function at known
  line; also asserts no hub has `line == 0`.
- `test_hubs_line_populated_python_class_method` — exercises the
  exact `Scaffold.route` shape from the original bug, asserting
  qualified `Class.method` lookups land on the right line.
- `test_hubs_line_populated_rust` — Rust standalone function
  fixture with a `Cargo.toml` for project autodetect.
- `test_hubs_line_populated_javascript` — ESM `import`/`export`
  JavaScript fixture (the call-graph builder's resolvable dialect).

## med-low-schema-cleanup-v1 — internal milestone

NOT a published release. Schema-hygiene milestone fixing 6 MED-LOW JSON
schema bugs surfaced by the post-AD-bundle real-repo audit. Each bug
was binary-verified against `/tmp/repos/{flask, express}` and gated by
a regression test in
`crates/tldr-cli/tests/med_low_schema_cleanup_v1.rs` (9 tests added).
`vuln_migration_v1_red`: 168/168 GREEN. All AA/AB/AC/AD1 milestones
still hold.

### Bug 1 — `references` silently truncated with no signal (N6)

```bash
$ tldr references __init__ /tmp/repos/flask --format json --quiet \
    | jq '{total_references, len: (.references | length)}'
{ "total_references": 62, "len": 20 }
```

`total_references: 62` but the `references` array only has 20 entries
and the JSON had no `truncated` flag — downstream tooling could not
detect that 42 references were silently hidden behind the default
`--limit 20`.

**Fix.** Mirrored the `calls` schema's truncation triplet on
`ReferencesReport`:

- `total_references` — full pre-truncation count (already there).
- `shown_references` — `references[].len()` after limiting.
- `truncated` — boolean, omitted (`skip_serializing_if`) when the
  Vec was NOT truncated, so non-truncated outputs keep the same
  shape they had before.

Files: `crates/tldr-core/src/analysis/references.rs::find_references`
populates the new fields; `crates/tldr-cli/src/commands/references.rs::filter_by_min_confidence`
keeps them coherent across the post-filter rewrite.

### Bug 2 — empty directory silently reported `language: "python"` (N7)

```bash
$ mkdir /tmp/empty && tldr structure /tmp/empty --format json
{ "language": "python", "files": [] }
```

For a directory with zero source files the autodetector silently
returned `Language::Python` (the fallback default) and surfaced the
shape `{"language":"python","files":[]}`. Users had no way to tell
whether the directory was genuinely empty or whether the autodetector
mis-picked.

**Fix.** Switched `CodeStructure.language` from `Language` to
`Option<Language>` and emitted `language: null` + a
`"No source files found in directory"` warning when the dir-walk
yielded zero source files. Mirrors the M-X5/M-Y2/M-Z8 warnings
pattern.

Files: `crates/tldr-core/src/types.rs::CodeStructure`,
`crates/tldr-core/src/ast/extractor.rs::get_code_structure`,
`crates/tldr-cli/src/output.rs::format_structure_text`.

### Bug 3 — `tldr definition` exit code was a generic `1` (N9)

```bash
$ tldr definition /nonexistent.py 1 1; echo $?
1
$ tldr definition --symbol nope --file existing.py 2>/dev/null; echo $?
1
```

Both "I gave a bad path" and "the symbol genuinely isn't there"
collapsed onto exit 1, so callers couldn't tell which kind of
failure they were dealing with.

**Fix.** Standardized `RemainingError::exit_code`:

- `FileNotFound` → 5 (filesystem-class, falls in the 2-9 band already
  used by `TldrError::PathNotFound`).
- `SymbolNotFound` → 20 (analysis-class, mirrors the
  `TldrError::FunctionNotFound` exit 20 used by `tldr impact`).

Also stopped wrapping typed `RemainingError`s in
`anyhow::anyhow!(detail)` inside `definition::run`: the position-mode
branch was discarding the type, so `main` couldn't downcast and
collapsed onto `ExitCode::FAILURE`. Now `FileNotFound` and
`SymbolNotFound` propagate via `Err(e.into())` and main's downcast
chain emits the proper code.

Files: `crates/tldr-cli/src/commands/remaining/error.rs::exit_code`,
`crates/tldr-cli/src/commands/remaining/definition.rs::run`,
`crates/tldr-cli/tests/remaining_test.rs` (refreshed `.code(...)`
expectations across 6 tests, all of which previously asserted exit 1).

### Bug 4 — `tldr calls` JSON had redundant `edge_count` / `node_count` (N12)

```bash
$ tldr calls /tmp/repos/flask --format json --quiet \
    | jq '{node_count, edge_count, total_edges, shown_edges}'
{ "node_count": 132, "edge_count": 935, "total_edges": 935, "shown_edges": 200 }
```

`edge_count` was always equal to `total_edges`; `node_count` was
always equal to `nodes.len()`. Both were dead weight that confused
schema readers about which key was canonical.

**Fix.** Dropped `edge_count` and `node_count` from
`CallGraphOutput`; canonical pair is `total_edges` + `shown_edges` +
`truncated` (matches the `references` triplet). Internal text-format
emitter switched from `output.edge_count` to `output.total_edges`.

Files: `crates/tldr-cli/src/commands/calls.rs::CallGraphOutput`,
`crates/tldr-cli/tests/cli_graph_tests.rs` (updated three assertions
that expected `edge_count` / `node_count`).

### Bug 5 — `dead.total_functions` vs `health.functions_analyzed` naming drift (N13)

The same metric had two names: `dead.total_functions` vs
`health.summary.functions_analyzed`. M-B2
(`canonical-function-enumerator-v1`) defined `functions_analyzed` as
the canonical key.

**Fix.** Hand-rolled `Serialize` for `DeadCodeReport` so it emits
BOTH keys: `functions_analyzed` (canonical, N13) and `total_functions`
(deprecated alias, kept for back-compat with consumers that were
reading the old key). On deserialization either key is accepted via
`serde(alias)`. The Rust field name keeps `total_functions` to avoid
in-process API churn.

Files: `crates/tldr-core/src/types.rs::DeadCodeReport`.

### Bug 6 — `dead_percentage` had 15-decimal IEEE-754 noise (N15)

```bash
$ tldr dead /tmp/repos/flask --format json --quiet | jq .dead_percentage
0.10893246187363835
```

15 fractional digits is meaningless for a "percent dead" metric and
made snapshot tests platform-fragile.

**Fix.** Round `dead_percentage` to 2 decimal places at construction
time in both `dead_code_analysis` (call-graph path) and
`dead_code_analysis_refcount` (refcount path) in
`crates/tldr-core/src/analysis/dead.rs`, plus the parallel emitter
in `crates/tldr-core/src/quality/dead_code.rs`. New `round_pct`
helper documents the rule for any future percentage field.

### Validation

- `cargo test -p tldr-cli --test med_low_schema_cleanup_v1
  --features semantic`: 9/9 GREEN (one test per bug above + a
  `--limit 1000` non-truncation sanity check + a "structure on real
  project keeps language" sanity check).
- `cargo test -p tldr-cli --test vuln_migration_v1_red
  --features semantic`: 168/168 GREEN.
- `cargo test -p tldr-cli --test cli_graph_tests --features
  semantic`: 32/32 GREEN (post-N12 schema rewrite).
- `cargo test -p tldr-core --test canonical_function_count_v1
  --features semantic`: 3/3 GREEN — `functions_analyzed` /
  `total_functions` agree across `health` / `dead` / `structure`
  for python / javascript / rust.
- All AA/AB/AC/AD1 milestones still hold.

Two unrelated pre-existing test failures remain
(`secure_command::test_secure_detects_taint`,
`definition_command::test_definition_invalid_position`,
`ruby_io_popen_with_user_input_via_compute_taint`); none touch the
schemas changed in this milestone and they fail on the parent
commit `992063a` as well.

## high-bundle-progress-determinism-coverage-v1 — internal milestone

NOT a published release. UX-hygiene milestone fixing 5 HIGH-priority CLI
bugs surfaced by the post-AC-bundle real-repo audit. Each bug was
binary-verified against `/tmp/repos/{flask, express}` and gated by a
regression test in
`crates/tldr-cli/tests/high_bundle_progress_determinism_coverage_v1.rs`
(7 tests added). `vuln_migration_v1_red`: 168/168 GREEN. All AA/AB/AC
milestones still hold.

### Bug 1 — Progress messages polluted machine-readable output (N1)

```bash
$ tldr complexity /tmp/repos/flask/src/flask/app.py __init__ --format json
Calculating complexity for __init__ in ... (Python)...
{ "function": "__init__", ... }
```

The progress banner already wrote to **stderr** (so a JSON parser
attached only to stdout was technically OK), but every interactive
terminal — and any tool that captures both streams or runs in a context
with merged stderr/stdout — saw the banner mixed in front of the JSON.
For machine-readable formats (json / sarif / compact), the contract is
"structured output and nothing else"; the banner is noise that hurts
every downstream consumer.

**Fix.** In `run_command`, derive `effective_quiet = cli.quiet ||
auto_quiet_on(format)`. Auto-quiet kicks in for json / sarif / compact;
text and dot still see progress banners. Threaded the effective flag
through every command-arm dispatch (~62 call sites). Embedder banners
(via `TLDR_QUIET=1`) also respect the new flag.

- `crates/tldr-cli/src/main.rs::run_command` — auto-quiet derivation,
  rebind `q = effective_quiet`, replace every `cli.quiet` in the dispatch
  table.

### Bug 2 — `tldr calls` was nondeterministic (N2)

```bash
$ for i in 1 2 3; do tldr calls /tmp/repos/flask --format json --quiet \
                       | jq .total_edges; done
935
910
922
```

Three runs on identical input produced three different edge counts.
Root cause: every loop that built the call-graph indices iterated
`ir.files: HashMap<PathBuf, FileIR>` (and `walkdir` returns directory
entries in OS-defined order, which is randomized on macOS). When two
modules share a `simple_module` alias, **first-writer-wins** in
`func_index`; HashMap-iteration order therefore changed which file won
the alias slot, and that in turn changed which calls became resolvable.
A different resolved-call set produced a different edge count.

**Fix.** Two layers of canonicalization:
1. `scan_project_files` now sorts the returned `Vec<ScannedFile>` by
   path, so the parallel index-build phase always sees the same order.
2. `build_project_call_graph_v2` collects `ir.files.iter()` into a
   sorted `Vec` and uses that for every populate / merge / type-resolver
   loop. The resolution loop sorts `ir.files.keys()`. Final `ir.edges`
   is sorted by `(src_file, src_func, dst_file, dst_func, call_type)`
   for byte-stable JSON output.

- `crates/tldr-core/src/callgraph/scanner.rs::scan_project_files` — sort
  files by path before return.
- `crates/tldr-core/src/callgraph/builder_v2.rs::build_project_call_graph_v2`
  — `sorted_files` materialization, sort the resolution-phase
  `file_paths`, sort `ir.edges` before return.

### Bug 3 — `tldr health` nondeterministic and json/text disagreed (N3)

```bash
$ for i in 1 2 3; do tldr health /tmp/repos/flask --format json --quiet \
                       | jq .summary.tight_coupling_pairs; done
30
31
31
$ for i in 1 2 3; do tldr health /tmp/repos/flask --format text --quiet \
                       | grep Coupling; done
... 30 tightly coupled pairs
... 28 tightly coupled pairs
... 31 tightly coupled pairs
```

Same root cause as N2 — the health command runs the call-graph builder
under the hood, so the random edge count propagated into the coupling
sub-analyzer's `tight_coupling_count`. Within a single invocation,
text and json read the same field; across invocations they diverged.

**Fix.** Inherits the N2 sort. Verified via the new
`n3_health_format_consistency_and_determinism` test that three back-to-
back json runs produce identical `tight_coupling_pairs`, and that the
text-format coupling line reports the same number as the JSON.

### Bug 4 — `tldr diagnostics files_analyzed` always reported 1 (N4)

```bash
$ tldr diagnostics /tmp/repos/flask --format json --quiet | jq .files_analyzed
1   # repo has 83 files
```

The counter was a literal `1` left as a `// This would need proper
counting` TODO since Phase 10 of the diagnostics build-out. Useless for
dashboards that report "files scanned per CI run".

**Fix.** New `count_diagnostic_files(path, tools)` walks `path` via
`ProjectWalker` (which honors `.gitignore` / `.tldrignore`), filtering
by extensions derived from the first tool's binary name. Single-file
inputs return 1 if the extension matches, 0 otherwise; directories
return the recursive walk count.

- `crates/tldr-core/src/diagnostics/runner.rs` — `count_diagnostic_files`
  helper, `language_for_tool_binary` mapping, replace
  `files_analyzed: 1` with the computed count.

### Bug 5 — `tldr imports` did not parse JS CommonJS `require()` (N5)

```bash
$ tldr imports /tmp/repos/express/index.js --format json
{ "imports": [] }   # despite the file containing module.exports = require('./lib/express');
```

The TS/JS parser only handled `import_statement` (ESM) and
`export_statement` (re-export). Files using CommonJS exclusively —
which is most of the npm ecosystem — got an empty imports array.
Downstream consumers (call graph, dependency graphs) never saw the
edges.

**Fix.** Add a `call_expression` arm in `extract_ts_imports_recursive`
that detects `require(<string-literal>)`. New `parse_cjs_require`
helper accepts a single string or non-substituted template-string
argument; rejects dynamic forms (`require(name)`) so we don't emit
phantom modules. Recursion still descends into the call expression so
nested requires are captured.

- `crates/tldr-core/src/ast/imports.rs` — `call_expression` arm in
  `extract_ts_imports_recursive`, new `parse_cjs_require` helper.

### Validation

- 7 new tests in `high_bundle_progress_determinism_coverage_v1.rs`
  (N1×2, N2, N3, N4, N5×2 covering literal and dynamic forms).
- `vuln_migration_v1_red`: 168/168 GREEN.
- All AA / AB / AC milestones still hold.

## low-cleanup-bundle-v1 — internal milestone

NOT a published release. UX-hygiene milestone fixing 8 LOW-priority CLI
bugs surfaced by the post-MED-bundle real-repo audit. Each bug was
binary-verified against `/tmp/repos/{flask, express, scala-cats-effect}`
and gated by a regression test in
`crates/tldr-cli/tests/low_cleanup_bundle_v1.rs` (8 tests added).
`vuln_migration_v1_red`: 168/168 GREEN.

### Bug 1 — `tldr structure --format text` was too reduced (L1)

The text view emitted only top-level filenames + bare function/class
names. On flask: 21 KB text vs 523 KB JSON — no methods, no class-method
nesting, no signatures. Effectively useless for navigation.

**Fix.** Expand `format_structure_text` to consume `definitions[]` (when
present) for full function signatures with line numbers, and
`method_infos[]` to nest each class's methods under it. Roughly 2.8×
richer (flask: 21 KB → 59 KB).

- `crates/tldr-cli/src/output.rs::format_structure_text` — pull
  `(line, signature)` from `definitions[]` keyed by name; emit
  `method_infos[]` indented under their owning class for single-class
  files; flat `Methods:` block when multi-class.

### Bug 2 — `tldr stats` empty payload was opaque (L2)

```bash
$ tldr stats
{"message": "No usage recorded"}
```

The user had no idea what "usage" meant or how to record it.

**Fix.** Extend `EmptyStatsOutput` with `next_steps: Vec<String>` and
`requires: Vec<String>` so the JSON payload self-documents the daemon
prerequisite. The text branch now prints a short walk-through instead
of just one line.

- `crates/tldr-cli/src/commands/daemon/stats.rs::EmptyStatsOutput` —
  add `next_steps`, `requires` fields; new `EmptyStatsOutput::empty()`
  constructor; text branch prints `tldr daemon start` walk-through.

### Bug 3 — `tldr fix --help` did not enumerate inputs (L3)

```bash
$ tldr fix --help
Diagnose and auto-fix errors from compiler/runtime output
```

"Compiler/runtime output" is too vague — users could not tell which
toolchains were supported.

**Fix.** Replace the one-line about with an enumerated list (cargo /
rustc, gcc / clang, Python tracebacks, jest / mocha / tsc, eslint /
ruff / pylint) on the `Command::Fix` variant — clap surfaces the
variant doc comment as the subcommand's `about`/`long_about`.

- `crates/tldr-cli/src/main.rs::Command::Fix` — multi-line doc comment
  enumerating accepted error formats.
- `crates/tldr-cli/src/commands/fix.rs::FixArgs` — mirror the
  enumeration for `tldr fix <subcmd> --help` parity.

### Bug 4 — `tldr coverage /dev/null` reported success (L4)

```bash
$ tldr coverage /dev/null
{ "summary": { "line_coverage": 0.0, "total_lines": 0, ... } }
$ echo $?
0
```

An empty / non-coverage file silently produced a 0/0 success report.
Downstream "0% coverage met threshold" guards thus passed on garbage.

**Fix.** When `--report-format` was NOT explicitly specified (the
auto-detect path), validate that:
1. file content is not empty; and
2. the parsed report contains at least one parseable record (files or
   lines).

If either check fails, return a `TldrError::ParseError` pointing at
the file and naming the auto-detected format. Explicit
`--report-format <fmt>` still falls through so the parser surfaces
its own format-specific error.

- `crates/tldr-core/src/quality/coverage.rs::parse_coverage` — two
  guard clauses gated on `format_was_explicit == false`.

### Bug 5 — `tldr dead` JSON had three redundant counters (L5)

```bash
$ tldr dead . --format json | jq '{total_dead, total_count, shown_count}'
{ "total_dead": 41, "total_count": 41, "shown_count": 41 }
```

`shown_count == total_count == total_dead` always (except on the rare
`--max-items` truncation path). Three fields, one fact.

**Fix.** Drop both `total_count` (duplicate of canonical `total_dead`
already in `DeadCodeReport`) and `shown_count` (always equal to
`dead_functions.len()` post-truncation). Keep only the boolean
`truncated` flag for the rare clipped case.

- `crates/tldr-cli/src/commands/dead.rs::DeadCodeOutput` — remove the
  two redundant `usize` fields; keep `truncated: bool`. Text branch
  still uses the in-scope counters for its truncation banner.

### Bug 6 — `tldr loc by_language` shape inconsistency (L6)

`by_language` was a JSON ARRAY (`Vec<LanguageLocEntry>`), forcing
consumers to write `report.by_language[0].language` even on
single-language repos. Audit asked for a stable OBJECT shape keyed by
language name across N=1 and N>1 cases.

**Fix.** Switch the underlying type to
`BTreeMap<String, LanguageLocEntry>` so JSON serialization always
produces `{"<lang>": {...}, ...}`. CLI text formatter sorts values by
total_lines descending for a natural reading order.

- `crates/tldr-core/src/metrics/loc.rs::LocReport::by_language` —
  `Vec<...>` -> `BTreeMap<String, ...>`. Both `analyze_directory` and
  the single-file path build the map. Sort logic moved to the CLI's
  text formatter (`crates/tldr-cli/src/commands/loc.rs::format_loc_text`).
- `crates/tldr-core/tests/session15_metrics_tests.rs` — update
  `.by_language.iter()` -> `.values()` for the new shape.

Verified on `/tmp/repos/scala-cats-effect`:

```bash
$ tldr loc /tmp/repos/scala-cats-effect | jq '.by_language | keys'
["c", "java", "scala"]
```

### Bug 7 — `tldr clones` could emit `Type-2` with similarity `1.0` (L7)

By definition: similarity 1.0 = identical tokens = Type-1, not Type-2.
The Type-2 detection branch could report `(CloneType::Type2, 1.0)` when
normalized similarity hit 1.0 while raw similarity was below 0.9 — the
`raw_similarity.max(norm_sim)` arm.

**Fix.** Route every reported similarity through `classify_clone_type`
in the Type-2 detection branch so the type label always agrees with the
score (Type-1 iff `sim ~ 1.0`, Type-2 iff `sim in [0.9, 1.0)`). The
Type-3 branch already used the same classifier.

- `crates/tldr-core/src/analysis/clones/detect.rs::detect_type2` —
  drop the manual `(Type1, _) | (Type2, _)` tuple, compute similarity
  first then delegate to `classify_clone_type(similarity)`.

Verified on `/tmp/repos/express`: 22 clone pairs, 0 type/similarity
violations.

### Bug 8 — long-running commands lacked `--quiet` plumbing (L8)

`tldr semantic "..."` (and the related `embed`/`similar`) printed 7+
lines to stderr on first run — model download progress, indexing
banner. The global `--quiet` flag silenced the chunk/index progress
(via `BuildOptions::show_progress`) but did NOT silence
`Embedder::new`'s model-load banner, which sat outside that path.

**Fix.** Have the CLI propagate the global `--quiet` flag through a
`TLDR_QUIET=1` environment variable that `Embedder::new` checks before
emitting its banner. No new flag needed — the existing global
`--quiet` (`-q`) now does the right thing end-to-end.

- `crates/tldr-cli/src/main.rs::run_command` — set `TLDR_QUIET=1` at
  CLI entry when `cli.quiet` is true.
- `crates/tldr-core/src/semantic/embedder.rs::Embedder::new` — gate
  the model-load `eprintln!` on `TLDR_QUIET` being absent.

Verified on flask:

```bash
$ tldr structure /tmp/repos/flask --format text 2>/dev/null    # default: 55 B stderr
$ tldr --quiet structure /tmp/repos/flask --format text 2>/dev/null   # quiet: 0 B stderr
```

## med-cleanup-bundle-v1 — internal milestone

NOT a published release. UX-hygiene milestone fixing 8 MED-priority CLI
bugs uncovered by the post-bundle real-repo audit. Each bug below was
binary-verified against `/tmp/repos/{flask, express, rails-html-sanitizer}`
and gated by a regression test in
`crates/tldr-cli/tests/med_cleanup_bundle_v1.rs` (10 tests added).

### Bug 1 — `tldr context` rejected positional path (M1)

```bash
$ tldr context router /tmp/repos/express
error: unexpected argument '/tmp/repos/express' found
```

Sibling commands (`impact`, `whatbreaks`) accept
`tldr <cmd> <thing> [path]`. `context` only had `--project /path`,
which was inconsistent and failed for users who typed the same shape
they'd used five seconds earlier with another command.

**Fix.** Add a positional `path` argument that mirrors `impact`'s shape
and takes precedence when set. `--project` is kept as a back-compat
alias.

- `crates/tldr-cli/src/commands/context.rs::ContextArgs` — add
  positional `path: PathBuf`, demote `project` to `Option<PathBuf>`,
  add `effective_project()` resolver.

### Bug 2 — `tldr definition` returned malformed-success on bad position (M2)

```bash
$ tldr definition /tmp/repos/flask/src/flask/app.py 110 5
{ "symbol": { "name": "<invalid argument: unresolved at ... — symbol '\"\"\"' not found in scope>", ... } }
$ echo $?
0
```

The position-based resolver returned an `Err(InvalidArgument(...))`
when the cursor landed on a docstring or unresolvable token, but the
CLI caught it and emitted a fake-success JSON payload with the error
message stuffed into `symbol.name`. Exit code 0. Downstream tooling
could not detect the failure.

**Fix.** Propagate the resolver error through `anyhow::Result<()>` so
the CLI exits non-zero and writes the message to stderr.

- `crates/tldr-cli/src/commands/remaining/definition.rs::DefinitionArgs::run`
  — return `Err` instead of synthesizing a `<unknown at ...>` result.

### Bug 3 — `tldr available` parsed comments / docstrings as expressions (M3)

```bash
$ tldr available flask/cli.py shell_command
{ "avail_in": { ... "text": "When loading the env files, set the default encoding to UTF - 8.", "operands": ["8.", "When loading ..."] }
```

Line 724 of `cli.py` is inside a docstring paragraph. The text-based
parser (`parse_expression_from_line`) only stripped `#` and `//` at
the start of a segment; it had no awareness of multi-line string
literals or block comments and happily parsed
`"... encoding to UTF - 8."` as a binary expression.

**Fix.** Build a tree-sitter set of comment/string-literal lines and
skip them in both the text-based and AST-based extractors. The AST
walk also stops descending into comment/string nodes.

- `crates/tldr-core/src/dataflow/available.rs` —
  `collect_comment_and_string_lines` + skip in `extract_expressions_full_with_lang`
  and `collect_binary_exprs`.

### Bug 4 — Ruby `module Rails` reported as 27-method God Class (M7)

```bash
$ tldr smells /tmp/repos/rails-html-sanitizer
{ "smells": [{ "smell_type": "god_class", "name": "Rails", "reason": "Class has 27 methods (threshold: 20)" }] }
```

`extract_ruby_methods_from_body` recursed into nested blocks to find
methods inside `begin`/`rescue`. It also recursed into nested
`class`/`module` declarations, summing their methods against the
enclosing module — so `module Rails` inherited the method counts of
every nested class.

**Fix.** Skip nested `class` / `module` nodes during method extraction;
they spawn their own `ClassInfo` entries via
`extract_ruby_classes_detailed`.

- `crates/tldr-core/src/ast/extract.rs::extract_ruby_methods_from_body`
  — explicit `"class" | "module" => {}` arm.

### Bug 5 — `tldr smells --help` lists 18 types but 8 are silently --deep-only (M14)

```bash
$ tldr smells --help | grep "requires --deep" | wc -l
8
$ tldr smells /tmp/repos/flask    # default — silently runs only 10 of 18
```

The help text annotates eight smell types as "requires --deep" but
running `smells` without `--deep` produced no notice. Users got half
the analysis surface and had to read help to find out.

**Fix.** Emit a stderr note when `--deep` is absent and the user did
not narrow with `--smell-type`. Suppressed under `--quiet`.

- `crates/tldr-cli/src/commands/smells.rs::SmellsArgs::run` — stderr
  notice listing the 8 deep-only analyzers.

### Bug 6 — `tldr churn` warning vs text output contradicted on shallow clones (M15)

```bash
$ tldr churn /tmp/repos/flask --format text
Warning: ... per-file churn ranks and averages have been suppressed.
High-Churn Files:
   1        1       +1       -0     ...    # ← printed anyway
```

The JSON layer set a degenerate-shallow warning and zeroed
`avg_commits_per_file`. The text formatter ignored the warning and
printed the full ranked list of files-tied-at-1-commit-each.

**Fix.** Detect the well-known warning prefix in `format_churn_text`
and replace the file table with an explanatory placeholder.

- `crates/tldr-cli/src/commands/churn.rs::format_churn_text` —
  `DEGENERATE_SHALLOW_WARN_PREFIX` constant + suppression branch.

### Bug 7 — `tldr similar <file>` returned per-chunk noise instead of per-file (M16)

```bash
$ tldr similar /tmp/repos/express/lib/application.js   # 600 LOC
# 5 unrelated 4-9-line helper chunks
```

The semantic index stores one chunk per function. When the user passed
a whole file, `find_similar` matched against the FIRST chunk in that
file and returned per-chunk hits — which were typically tiny helpers
the user did not care about.

**Fix.** When `--function` is omitted, default to file-level
aggregation: for every chunk in the source file, find each candidate
chunk's similarity, group by destination file, sum, and rank by total
similarity. Add `--by-chunk` for opt-in legacy behavior.

- `crates/tldr-cli/src/commands/similar.rs` — `aggregate_similar_by_file`,
  `AggregatedSimilarityReport`, `FileSimilarityResult`,
  `format_aggregated_similar_text`. New `--by-chunk` flag.
- `crates/tldr-cli/tests/exhaustive_matrix.rs::check_similar` — accept
  either `source` (legacy) or `source_file` (aggregated) shape.

### Bug 8 — `tldr verify` `total_functions` denominator was undocumented (M18)

```bash
$ tldr verify /tmp/repos/flask | jq '.summary.coverage'
{ "total_functions": 306, "coverage_pct": 96.0 }    # ← but flask has 918 functions
```

`coverage.total_functions` counted only contract-amenable functions
(those evaluated by the contracts sub-analysis). Without explicit
scoping, the 96% looked like project-wide coverage.

**Fix.** Add `coverage.scope: String` documenting the denominator.
Surface it in both JSON and text output.

- `crates/tldr-cli/src/commands/contracts/types.rs::CoverageInfo` —
  add `scope` field.
- `crates/tldr-cli/src/commands/contracts/verify.rs::compute_coverage`
  / `format_verify_text` — populate scope, render in text.

### Validation

10 new regression tests in `crates/tldr-cli/tests/med_cleanup_bundle_v1.rs`,
all passing. `vuln_migration_v1_red`: 168/168 GREEN. Pre-existing
failures (Ruby ruby_io_popen taint AST-only, secure-autodetect missing
language coverage, csharp/c/cpp/etc.) are unchanged by this milestone
and predate it.

Binary verifications recorded against:
- `tldr context find_best_app /tmp/repos/flask` — works.
- `tldr definition /tmp/repos/flask/src/flask/app.py 110 5; echo $?` — exit 1.
- `tldr available /tmp/repos/flask/src/flask/cli.py shell_command` — no
  docstring prose in `avail_in`.
- `tldr smells /tmp/repos/rails-html-sanitizer` — no Rails God Class.
- `tldr smells /tmp/repos/flask` — stderr "8 smell analyzers require --deep".
- `tldr churn /tmp/repos/flask --format text` — file table suppressed.
- `tldr similar /tmp/repos/express/lib/application.js` — per-file
  rows (response.js, view.js, utils.js — each with total/avg/chunks).
- `tldr verify /tmp/repos/flask | jq '.summary.coverage.scope'` — scope
  string present.

## schema-naming-and-units-v1 — internal milestone

NOT a published release. Schema-hygiene milestone fixing three output
inconsistencies that forced JSON consumers of `tldr` to special-case
key naming or sibling-field units when joining or summing data.

### Bug 1 — `tldr api-check` summary key disagreed with detail key (M9)

```bash
$ tldr api-check /tmp/repos/flask --format json | jq '.summary.by_category'
{ "errorhandling": 1, "crypto": 1 }              # ← collapsed PascalCase
$ tldr api-check /tmp/repos/flask --format json | jq '[.findings[].rule.category] | unique'
[ "crypto", "error_handling" ]                   # ← serde snake_case
```

`build_summary` keyed `by_category` / `by_severity` via
`format!("{:?}", cat).to_lowercase()`, which strips PascalCase
boundaries (`ErrorHandling` → `errorhandling`). Findings, however,
serialize via serde with `#[serde(rename_all = "snake_case")]`,
emitting `error_handling`. Joining summary→findings by category
required ad-hoc normalization on the consumer side.

### Bug 2 — `tldr health` text and JSON disagreed on coupling pairs (M12)

```bash
$ tldr health /tmp/repos/flask --format json | jq '.summary.tight_coupling_pairs'
30
$ tldr health /tmp/repos/flask --format text | grep "tightly coupled"
Coupling:    31 tightly coupled pairs            # ← off-by-one vs JSON
```

Both formats already pulled from `summary.tight_coupling_pairs`, but
no regression test enforced the invariant. Adding two consistency
tests guards against future drift between the text formatter and
JSON serialization for every numeric field the dashboard prints
(coupling, similarity, dead-code count).

### Bug 3 — `tldr debt` summary mixed units across sibling fields (M13)

```bash
$ tldr debt /tmp/repos/flask --format json | jq '.summary'
{
  "total_minutes": 6105,
  "by_category": { "maintainability": 5900, ... },        # ← minutes
  "by_rule":     { "missing_docs": 5400, ... },           # ← minutes
  "by_severity": { "high": 12, "low": 540, "medium": 20 } # ← FINDING COUNTS
}
```

`by_severity` was a finding count (sums to 572) while `by_category`
and `by_rule` were minutes (sum to 6105). Sibling fields with
identical naming shape but different units, no documentation. A
consumer summing severity buckets to compare against `total_minutes`
would silently produce nonsense.

### Fix

- `crates/tldr-cli/src/commands/remaining/api_check.rs::build_summary` —
  drop the `format!("{:?}", ...).to_lowercase()` shortcut and route
  both category and severity through explicit snake_case helpers
  (`serialize_misuse_category`, `serialize_misuse_severity`) that
  match the serde representation used on `findings[].rule.category`
  and `.rule.severity`. Summary keys now equal detail keys verbatim.
- `crates/tldr-core/src/quality/debt.rs::DebtSummary` —
  `by_severity` now carries **minutes** (sums to `total_minutes`,
  consistent with `by_category` and `by_rule`), and a new sibling
  `by_severity_count` carries the per-severity finding counts
  (sums to `findings.len()`). Both are populated in `analyze_debt`
  in a single pass over the issue list.
- `crates/tldr-core/src/quality/health.rs` — added two regression
  tests (`test_health_format_consistency`,
  `test_health_format_consistency_all_summary_fields`) that build a
  `HealthReport` once and assert text and JSON agree on every
  user-visible numeric (`tight_coupling_pairs`, `similar_pairs`,
  `dead_count`).

### Tests

- `test_api_check_category_naming_consistent` (`crates/tldr-cli/tests/remaining_test.rs`) —
  exact-equality assertion on the set of category keys emitted in
  `summary.by_category` vs `findings[].rule.category`, plus a guard
  against the legacy `errorhandling` / `callorder` collapsed forms.
- `test_health_format_consistency`, `test_health_format_consistency_all_summary_fields`
  (`crates/tldr-core/src/quality/health.rs`) — single-report consistency
  between text and JSON outputs for every numeric the dashboard prints.
- `test_debt_summary_units_consistent` (`crates/tldr-core/src/quality/debt_tests.rs`) —
  confirms `by_category`, `by_rule`, `by_severity` all sum to
  `total_minutes` (units match), and `by_severity_count` sums to
  `findings.len()` (count semantics preserved). Also asserts the
  two severity maps share an identical key set.
- `test_debt_summary_by_severity_populated` updated to reflect the
  new dual-map shape.

### Binary verification on flask

```bash
$ tldr api-check /tmp/repos/flask --format json | jq '.summary.by_category'
{ "crypto": 1, "error_handling": 1 }   # ← matches findings[].rule.category

$ tldr debt /tmp/repos/flask --format json | jq '.summary | {total_minutes, by_severity, by_severity_count}'
{
  "total_minutes": 6105,
  "by_severity":       { "high": 360,  "low": 5400, "medium": 345 },  # sums to 6105 (minutes)
  "by_severity_count": { "high": 12,   "low": 540,  "medium": 20 }    # sums to 572  (count)
}
```

`vuln_migration_v1_red`: 168/168 GREEN.

## inheritance-and-dead-cleanup-v1 — internal milestone

NOT a published release. Bug-fix milestone fixing three quality issues
in `tldr inheritance` and `tldr dead` exposed by running them against
the ts-dom-gen TypeScript corpus.

### Bug 1 — `tldr inheritance` emitted duplicate edges (M5)

```bash
$ tldr inheritance --lang typescript /tmp/repos/ts-dom-gen | jq '.edges | length'
6606    # ← inflated; only 1562 nodes in the graph
```

Multiple language extractors (TS overload signatures, re-emitted
heritage clauses, Go interface embedding) called `add_edge` for the
same `(child, parent)` pair 3-4 times, producing duplicate
`InheritanceEdge` entries. Downstream consumers were forced to
deduplicate, and counts in summaries were wrong.

### Bug 2 — `tldr inheritance` reported false diamonds (M4)

```bash
$ tldr inheritance --lang typescript /tmp/repos/ts-dom-gen | jq '.diamonds | length'
1486    # ← almost all are linear chains, not real diamonds
```

`detect_diamonds` iterated `graph.parents.get(class)` directly. With
the M5 bug present, a single-parent class like `CSSTransition` had
its parent listed 3 times, so `multi_parent_classes()` mis-classified
it as multi-parent and the BFS reported a "diamond" that was actually
a linear chain `CSSTransition → Animation → EventTarget`.

A real diamond requires **two distinct immediate parents** that
converge on the same ancestor.

### Bug 3 — `tldr dead` flagged `.d.ts` symbols as possibly_dead (M6)

```bash
$ tldr dead /tmp/repos/ts-dom-gen | jq '[.possibly_dead[] | select(.file | endswith(".d.ts"))] | length'
299     # ← every declared symbol surfaced as a false positive
```

TypeScript declaration files (`.d.ts`) contain only `interface`,
`type`, and `declare` statements — no executable code. They cannot
participate in a call graph and therefore have no meaning in
dead-code analysis. Every symbol declared in `.d.ts` was inevitably
"possibly_dead" regardless of how the rest of the program used it.

### Fix

- `crates/tldr-core/src/inheritance/mod.rs::build_edges` — dedupe at
  the canonical `(child, parent, parent_file)` tuple level, plus a
  per-child `parents` dedup pass for downstream invariants
  (notably diamond detection).
- `crates/tldr-core/src/inheritance/patterns.rs::detect_diamonds` —
  filter `parents` through a `HashSet` so duplicates can never
  inflate `parents.len() >= 2`. Also dedupe the resulting `paths`
  vector so each reported diamond has at least two genuinely distinct
  paths.
- `crates/tldr-cli/src/commands/dead.rs` — new
  `is_typescript_declaration_file` helper. Skip `.d.ts` files in both
  `collect_module_infos` (call-graph path) and
  `collect_module_infos_with_refcounts` (born-dead path). Mirrors
  the M-Y3 oversize-skip pattern.

### Validation

Tests added (all GREEN):

- `crates/tldr-core/tests/inheritance_tests.rs::test_inheritance_edges_deduplicated`
- `crates/tldr-core/tests/inheritance_tests.rs::test_inheritance_edges_deduplicated_graph_level`
- `crates/tldr-core/tests/inheritance_tests.rs::test_inheritance_diamond_real_pattern`
- `crates/tldr-core/tests/inheritance_tests.rs::test_inheritance_no_false_diamond_from_duplicate_parents`
- `crates/tldr-cli/tests/inheritance_and_dead_cleanup_v1_test.rs::test_dead_skips_dts_files`

Binary verification on `/tmp/repos/ts-dom-gen` (TypeScript DOM corpus):

| Metric                                | Before | After |
| ------------------------------------- | -----: | ----: |
| `tldr inheritance` edges              | 6606   | 889   |
| `tldr inheritance` diamonds           | 1486   | 1     |
| `tldr dead` `.d.ts` in possibly_dead  | 299    | 0     |
| `tldr dead` `.d.ts` in dead_functions | (n/a)  | 0     |

The single remaining diamond is the legitimate DOM diamond
`DocumentType → {Node, ChildNode→Node} → EventTarget`.

`vuln_migration_v1_red`: 168/168 GREEN.

## vuln-secure-autodetect-parity-v1 — internal milestone

NOT a published release. Bug-fix milestone restoring `tldr vuln` ↔
`tldr secure` parity on the language-autodetect path.

### Bug — secure's autodetect path silently produced 0 taint findings

```bash
$ tldr vuln /tmp/repos/express   | jq '.findings | length'   # 1
$ tldr secure /tmp/repos/express | jq '.summary.taint_count' # 0  ← divergence
```

`/tmp/repos/express` is a JavaScript-only tree (manifest:
`package.json`). With an explicit `--lang javascript` both commands
agreed (M-Z10 `secure-test-file-suppression-v1` closed the explicit-lang
divergence). On the autodetect path (no `--lang`), secure still
diverged because its `collect_files` did NOT autodetect the dominant
language: with `lang = None`, `is_supported_secure_file` matched only
`.py` and `.rs` files, so a JS-only tree silently produced an empty
file set and the canonical taint pipeline never ran.

### Why this matters

`tldr secure` is the security dashboard the user lands on by default;
`tldr vuln` is the deeper view. The autodetect-path divergence meant
the dashboard under-reported on every JS / TS tree the user ran it on
without an explicit `--lang` flag — silently. Critical security
findings were hidden.

### Fix

`secure.rs::run` now mirrors `vuln.rs::VulnArgs::run`'s
language-resolution prelude:

1. If `--lang L` is provided, honor it as-is.
2. Else, autodetect via `Language::from_directory` (made strict by
   M-AA1 `autodetect-dominant-language-v1`: extension-majority +
   manifest-priority, skipping vendored trees).
3. If the autodetected language lies outside the natively-analyzed
   set (Python, Rust, TypeScript, JavaScript per
   `vuln::is_natively_analyzed`, promoted to `pub(super)` for reuse),
   error with `RemainingError::AutodetectUnsupported` (exit 2) — same
   contract and message shape as vuln.

The resolved language is then passed to `collect_files`, which uses
the existing per-language extension filter. The `--include-tests`
suppression mask (M-Z10) already runs post-analysis, so once the
files are collected the rest of the pipeline already agreed with
vuln.

### Validation

Binary verification (no `--lang`):

| Repo                  | `vuln.findings.length` (taint subset) | `secure.summary.taint_count` |
|-----------------------|---------------------------------------|------------------------------|
| `/tmp/repos/express`  | 1                                     | 1 ✓ (was 0)                  |
| `/tmp/repos/flask`    | 4                                     | 4 ✓                          |
| `/tmp/repos/ripgrep`  | 22                                    | 22 ✓                         |

(For ripgrep, `vuln.findings.length` is 28 raw; the taint subset
excluding Rust smell-class findings — UnsafeCode, MemorySafety —
agrees with `secure.summary.taint_count`. Smells flow into
`summary.unsafe_blocks` etc. on the secure side, not `taint_count`.)

Test:

- `crates/tldr-cli/tests/vuln_secure_autodetect_parity_v1.rs`:
  `test_vuln_secure_autodetect_parity_express` builds a synthetic
  JS-only directory (with a `package.json` manifest and an
  Express-style `req.params → res.redirect` PathTraversal flow), runs
  both `tldr vuln <dir>` and `tldr secure <dir>` with NO `--lang`,
  and asserts `vuln.findings.length == secure.summary.taint_count`.
- `vuln_migration_v1_red`: 168/168 GREEN.
- `tldr-cli` lib unit tests (vuln + secure scopes): 45/45 GREEN.

### Files changed

- `crates/tldr-cli/src/commands/remaining/vuln.rs` — promoted
  `is_natively_analyzed` to `pub(super)` so secure can reuse the
  canonical gate.
- `crates/tldr-cli/src/commands/remaining/secure.rs` — added the
  autodetect language-resolution prelude in `run` mirroring vuln's
  contract; passes the resolved `Option<Language>` to `collect_files`.
- `crates/tldr-cli/tests/vuln_secure_autodetect_parity_v1.rs` — new
  RED→GREEN guard for the synthetic-dir parity invariant.

## format-flag-strictness-v1 — internal milestone

NOT a published release. UX bug-fix milestone for the global `--format` flag.

### Bug — `--format sarif` silently emitted plain JSON

```bash
$ tldr smells --format sarif /tmp/repos/flask | jq '"$schema" // "MISSING"'
"MISSING"
```

The audit identified that many subcommands silently fell back to plain JSON
when invoked with `--format sarif` or `--format dot`, instead of producing
the requested format. Affected commands per the audit:

- `--format sarif` returned plain JSON: smells, dead, health, api-check,
  secure, debt, structure, tree, halstead, complexity, extract.
- `--format sarif` returned EMPTY: complexity, extract.
- `--format dot` returned plain JSON: calls.
- `taint` and `reaching-defs` had explicit `OutputFormat::Sarif` /
  `OutputFormat::Dot` arms that fell back to JSON with a comment
  "not supported, fall back to JSON".

Currently emitting real SARIF: `vuln`, `clones`. Currently emitting real
DOT: `clones`, `deps`.

### Why this matters (security false-trust)

Users wiring up CI pipelines (GitHub code-scanning, VS Code SARIF extension)
saw a successful exit and a JSON document, and reasonably assumed SARIF was
being produced. It was not. The integration silently failed open: zero
findings ingested, no error surfaced to operators.

### Fix — Option B (error on unsupported)

Centralized validation in `crates/tldr-cli/src/output.rs`:

```rust
pub fn validate_format_for_command(cmd: &str, format: OutputFormat) -> Result<(), String>
```

Universal formats (`json`, `text`, `compact`) are always allowed. SARIF and
DOT are gated by an explicit allowlist:

| Format | Supported by                  |
| ------ | ----------------------------- |
| sarif  | `vuln`, `clones`              |
| dot    | `clones`, `deps`              |

Any other `(cmd, format)` pair now returns an error before any analysis
runs. The validator is invoked from `run_command` in `main.rs` against a
stable `command_name(&Command)` mapping, so adding a new subcommand cannot
silently bypass the check.

Example:

```text
$ tldr smells --format sarif .
Error: --format sarif not supported by smells. Use --format json.
SARIF is only emitted by: vuln, clones.
$ echo $?
1
```

### Files

- `crates/tldr-cli/src/output.rs` — added `OutputFormat::name()` and
  `validate_format_for_command()` (allowlist-based).
- `crates/tldr-cli/src/main.rs` — added `command_name(&Command)` and
  pre-dispatch validation in `run_command`.
- `crates/tldr-cli/tests/format_flag_strictness_v1.rs` — 10 new tests
  covering: SARIF rejection on 10 unsupported commands, DOT rejection on
  `calls`/`smells`, regression guards for `vuln --format sarif`,
  `clones --format sarif`, `deps --format dot`, universal `--format json`,
  plus three unit tests on the validator allowlist.
- `crates/tldr-cli/tests/cli_graph_tests.rs` — `test_calls_dot_format`
  renamed to `test_calls_dot_format_rejected` and inverted to assert the
  new error behavior. The previous assertion baked in the buggy
  silent-JSON fallback (a comment in the test even said "DOT format is
  currently output as JSON ... known limitation").

### Verification

`vuln_migration_v1_red`: 168/168 GREEN. New `format_flag_strictness_v1`:
10/10 GREEN. Binary check on installed `tldr 0.3.0`:

```bash
$ tldr smells --format sarif /tmp/repos/flask 2>&1 | head -1
Error: --format sarif not supported by smells. Use --format json. ...
$ tldr vuln --format sarif /tmp/repos/flask | jq '.["$schema"]'
"https://raw.githubusercontent.com/oasis-tcs/sarif-spec/master/Schemata/sarif-schema-2.1.0.json"
```

## references-canonical-def-v1 — internal milestone

NOT a published release. UX bug-fix milestone for `tldr references`.

### Bug — canonical definition hidden behind test subclass

```bash
$ tldr references Flask /tmp/repos/flask | jq '.definition'
{
  "file": "/tmp/repos/flask/tests/test_config.py",
  "line": 202,
  "column": 11,
  "kind": "class",
  "signature": "class Flask(flask.Flask):"
}
```

The picker returned the FIRST AST match from `walk_project`'s walker
order. On `flask` that walker happened to hit `tests/test_config.py`
before `src/flask/app.py`, so the canonical
`class Flask` at `src/flask/app.py:109` was hidden behind a
fixture subclass `class Flask(flask.Flask)` defined inside a
test file. The definition shown to the user is **not** the
definition they want — it's a test stub.

The same shape shows up across other repos: `Router` in `express`
defined twice (real `lib/...` declaration + test-fixture rebind),
`Foo` in any Rust crate with a `tests/foo_test.rs` shadow type.

In addition, `total_references` was set to the **post-truncation**
length of the references Vec, so the default `--limit 20` made
every popular symbol look like it had exactly 20 references on the
planet. Flask actually has 337 references in its own repo; the
report claimed 20.

### Fix

`crates/tldr-core/src/analysis/references.rs::find_definition`
now collects **all** AST-level matches across the workspace and
ranks them into three tiers:

| Tier | Predicate | Example |
| ---- | --------- | ------- |
| 1 (best)  | non-test AND under `src/` / `lib/` / `main/` | `src/flask/app.py:109` |
| 2         | non-test, anywhere else                     | `examples/demo.py`     |
| 3 (worst) | test file                                   | `tests/test_config.py` |

Within a tier, ties broken by lexicographic file path then line
number — fully deterministic.

Tier 3 is only picked when **every** match is in a test file
(symbol is genuinely test-only, e.g. a `pytest` helper). This
preserves correctness for genuine test-only fixtures.

The test-file predicate `is_test_file_path` is implemented as a
fresh public helper in `references.rs` rather than reusing
`vuln::is_js_test_file` / `vuln::is_rust_test_file` directly: the
vuln helpers are extension-gated to JS/Rust respectively, but
the canonical-def picker needs a generic predicate covering
Python (`tests/`, `test_*.py`, `*_test.py`, `conftest.py`),
Java/Kotlin/Scala (`src/test/`), Ruby (`spec/`, `*_spec.rb`,
`*_test.rb`), Go (`*_test.go`), and the existing Rust/JS rules.

`total_references` and `stats.verified_references` now both
reflect the **pre-truncation** count — `--limit 20` truncates
the `references` Vec but leaves the count honest, so users see
"20 of 337" implicitly.

### Investigation note: "why 20 references?"

The "ref count = 20" was not a search-scope bug. The CLI's
`--limit` defaults to 20 (`crates/tldr-cli/src/commands/references.rs`,
`ReferencesArgs::limit = 20`), and `find_references` truncates
the Vec to that limit. The pathology was that
`total_references` echoed the truncated length instead of the
real verified count — which is now fixed in this milestone.

### Verification (binary)

```bash
$ tldr references Flask /tmp/repos/flask | jq '.definition'
{ "file": ".../src/flask/app.py", "line": 109, "kind": "class", ... }

$ tldr references Flask /tmp/repos/flask | jq '.total_references'
337   # was: 20
```

### Tests

- 5 unit tests for `is_test_file_path` (Python, JS/TS, Rust,
  Java/Kotlin/Scala, Ruby/Go).
- 1 unit test for `canonical_def_tier` ranking.
- 4 end-to-end fixture tests
  (`test_references_skips_test_subclass_picks_canonical_{python,js,rust,go}`)
  covering the flask/express bug shape per language.
- 1 fallback test
  (`test_references_canonical_def_test_only_fallback`) — when
  the symbol is genuinely test-only, the picker still returns
  the test-file definition rather than `None`.
- 1 regression test
  (`test_total_references_reflects_pre_truncation_count`) for
  the truncation-count fix.

12 new unit tests, all passing. `vuln_migration_v1_red` 168/168
GREEN. Full `tldr-core` lib suite: 4754 passed.

---

## search-symbol-name-boost-v1 — internal milestone

NOT a published release. UX bug-fix milestone for `tldr search` (the
enriched BM25-based symbol-discovery command).

### Bug — typing the symbol name does not return the symbol

```bash
$ tldr search Flask /tmp/repos/flask | jq '.results[:5] | map({name})'
[
  {"name": "dumps"},
  {"name": "wsgi_errors_stream"},
  {"name": "Request"},
  {"name": "BlueprintSetupState"},
  {"name": "test_config"}
]
# class Flask at src/flask/app.py:109 was buried beyond top 50.
```

Plain BM25 ranks documents by token frequency in the FULL document
text. When the user types a short identifier query (`Flask`,
`Router`, `File`) the canonical class/function whose *name* matches
the query is outranked by docstring-heavy files that mention the
term many times. The user's most obvious mental model — "type the
symbol name, get the symbol" — fails silently.

Furthermore, the structure-enrichment pass found `app.py` in BM25's
raw results, but the matched lines (imports/module preamble, line
1) lay *outside* the class body (line 109+), so
`find_enclosing_entry` returned None and the result was filed as
`kind="module"` — hiding the canonical class entirely.

### Fix — symbol-name boost layered above BM25

`enriched.rs::search_with_inner` (Stage 5a, BM25-mode only):

1. Determine boost eligibility: query is short (≤30 chars) and
   contains no whitespace (`boost_query_for`). Multi-word queries
   like `verify jwt token` are deliberately NOT boosted because the
   user is searching for behavior, not a single symbol.

2. Pass 1 — boost results that already have a matching name:
   * `EnrichedResult.name` exact (case-insensitive) → score x5.0
   * substring match (case-insensitive)            → score x2.0
   * everything else                               → unchanged

3. Pass 2 — synthesize results for matching definitions in BM25's
   raw result files that did NOT survive enrichment because the
   enclosing-line lookup missed them. For each file already in the
   raw BM25 results, scan its structure entries; promote any
   definition whose name matches into a fresh `EnrichedResult` with
   the file's best BM25 score as the base, then apply the boost.

4. Test-file demotion: when a boosted result lives under a tests
   directory or matches a test-style file name (`test_*.py`,
   `*_test.go`, `*.test.ts`), apply x0.5 *after* the boost. This
   prevents `tests/test_config.py::Flask` (a fixture subclass) from
   outranking `src/flask/app.py::Flask` (the canonical definition).
   Mirrors the existing test-file suppression pattern in
   vuln/secure.

5. Scope: BM25 mode only. Regex mode does not produce BM25-style
   scores. Hybrid mode's scores have a documented RRF upper bound
   (`2/(k+1) ≈ 0.0328`) that downstream tests assert against —
   boosting in those modes would violate the contract.

### Result

```bash
$ tldr search Flask /tmp/repos/flask | jq '.results[0]'
{ "name": "Flask", "file": "src/flask/app.py", "line": 109 }   # ✓

$ tldr search Router /tmp/repos/express | jq '.results[0].name'
"getrouter"                                                    # Router accessor ✓

$ tldr search File /tmp/repos/ripgrep | jq '.results[0]'
{ "name": "File", "file": "crates/core/flags/defs.rs", "line": 1987 }  # ✓
```

### Coverage-penalty preservation

The M-T6 / `analysis-precision-v1` BM25 coverage penalty (BUG-20)
is preserved: a multi-word random query
(`nonexistent_term_xyz_789`) still scores well below the 0.5
ceiling because (a) the query has whitespace → name-boost does not
fire, and (b) plain BM25's coverage penalty multiplies the score
by `matched_terms / total_query_terms` when coverage < 0.5.

### Tests

* `test_search_exact_name_match_top_ranked` — class `Foo` + 10
  docstring-heavy files; query `Foo` returns the class as #1.
* `test_search_substring_name_match_boosted` — query `Bar` ranks
  `BarHelper` and `BazBar` above docstring-only `thing`.
* `test_search_low_coverage_still_penalized` — random multi-word
  query keeps M-T6's coverage penalty active.
* Helper-level tests for `boost_query_for`, `name_boost_multiplier`,
  and `is_test_path`.

### Validation

* `tldr search Flask /tmp/repos/flask` → `name=Flask`,
  `file=src/flask/app.py`, `line=109` ✓
* `tldr search Router /tmp/repos/express` → top hit is the Router
  accessor function ✓
* `tldr search File /tmp/repos/ripgrep` → exact `File` symbol at
  `crates/core/flags/defs.rs:1987` ✓
* `vuln_migration_v1_red`: 168/168 GREEN
* All search lib + integration tests: 84 + 8 + 5 + 43 GREEN

### Files

* `crates/tldr-core/src/search/enriched.rs` — boost helpers, Stage
  5a, tests.

---

## autodetect-dominant-language-v1 — internal milestone

NOT a published release. Critical bug-fix milestone for
`Language::from_directory`, the directory-level language detector that
sits underneath every `tldr` command run against a project root.

### Bug — silent wrong-language for many real repos

`Language::from_directory` ran manifest detection FIRST and let it
unconditionally win. On real repositories with stray manifest files
(tooling `package.json`, Sphinx `doc/requirements.txt`, etc.) the
detector returned a confidently wrong language:

```bash
$ tldr structure /tmp/repos/scala-cats-effect | head -3
{
  "language": "javascript",          # 457 .scala files; package.json wins
  "files_count": 0
}

$ tldr structure /tmp/repos/ocaml-dune | head -3
{
  "language": "python",              # 1818 .ml files; doc/requirements.txt wins
  "files_count": 4
}
```

A confidently-wrong language label destroys downstream trust: every
JSON output, every codemap, every diagnostic is filtered through this
choice. There is no warning — the detector just returns the wrong
answer and every consumer dutifully cooperates.

### Fix — strict extension-majority is primary, manifests are tiebreakers

Inverted the detection priority to match what users actually expect:

1. Walk the directory (existing `walker::walk_project` excludes
   `node_modules`, `target`, `build`, `dist`, `.git`, hidden, and
   gitignored paths).
2. Count files per recognised language extension. Files inside common
   docs trees (`docs`, `doc`, `documentation`, `site-docs`) are
   excluded from this count: Doxygen-shipped `docs/*.js` would
   otherwise drown out a small C++ project's actual source.
3. Identify the dominant language and the runner-up.
4. **Strict majority:** when the runner-up holds < 80% of the
   dominant count, return the dominant language. Manifests cannot
   override — a tooling `package.json` beside 457 `.scala` files
   does not flip the answer.
5. **Close-call tiebreaker:** when the runner-up is within 20%, run
   manifest detection (depth ≤ 2, precedence-ranked); honour the
   manifest's choice only when its language has at least one source
   file in the walk.
6. **C-vs-Cpp disambiguation:** `.h` is ambiguous between C and C++,
   so when the dominant pick is C or Cpp we defer to the existing
   `c_vs_cpp_tie_break` (counts cpp-family vs c-family, ignores
   `.h`). The autodetect-correctness-v1 Swift / Rust extension
   override embedded in that helper is preserved verbatim.
7. **Empty / unrecognised:** a directory with no recognised source
   files returns `None` — never a manifest-derived guess.

### Validation — 17 cloned repos, before / after

| Repo                       | Before              | After      |
|----------------------------|---------------------|------------|
| c-sds                      | c                   | c          |
| cpp-tinyxml2               | cpp                 | cpp        |
| csharp-newtonsoft-bson     | csharp              | csharp     |
| elixir-plug                | elixir              | elixir     |
| express                    | javascript          | javascript |
| flask                      | python              | python     |
| go-httprouter              | go                  | go         |
| kotlin-datetime            | kotlin              | kotlin     |
| lua-lsp                    | lua                 | lua        |
| luau-luau                  | **python (WRONG)**  | cpp        |
| ocaml-dune                 | **python (WRONG)**  | ocaml      |
| php-symfony-string         | php                 | php        |
| rails-html-sanitizer       | ruby                | ruby       |
| ripgrep                    | rust                | rust       |
| scala-cats-effect          | **javascript (WRONG)** | scala   |
| scala-example              | scala               | scala      |
| spring-petclinic           | java                | java       |
| swift-collections          | swift               | swift      |
| ts-dom-gen                 | typescript          | typescript |

luau-luau resolves to `cpp` because the repo's actual file count is
295 `.cpp` vs 122 `.luau` — Cpp is the legitimate dominant extension
and the user spec explicitly notes "if so, cpp would be CORRECT here".

### Test surface

- 17 per-language strict-majority tests (one per supported language).
- 3 real-repo regression scenarios: scala-cats-effect, ocaml-dune,
  luau-luau.
- 2 mixed-language dominance tests (Java vs Kotlin both directions).
- 1 close-call manifest tiebreaker test.
- 3 empty / manifest-only / unrecognised tests asserting `None`.
- 1 swift-collections override regression guard
  (autodetect-correctness-v1).

Plus updated existing manifest-priority tests so their semantics
reflect the new "manifest is the tiebreaker, not the override" rule.

### Test results

- `tldr-core` lib: 4731 passed, 0 failed.
- `vuln_migration_v1_red`: 168 / 168 GREEN.
- `autodetect-correctness-v1` regression preserved: swift-collections
  still detects as `swift`, not `c`.

### Files modified

- `crates/tldr-core/src/types.rs` — rewrote `Language::from_directory`,
  added 27 tests, adjusted existing manifest-priority test fixtures.
- `CHANGELOG.md` — this entry.

## deps-and-surface-graceful-degrade-v1 — internal milestone

NOT a published release. Bug-fix milestone aligning `tldr deps` and
`tldr surface` with the soft-skip semantics already used by `vuln`,
`secure`, and `structure`.

### Bug 1 — `tldr deps` aborted on oversize files

```bash
$ tldr deps --lang typescript /tmp/repos/ts-dom-gen
Error: File too large: dom.generated.d.ts is 3MB (max 1MB)   # exit 6
```

The dependency walker called `get_imports(path)` on every collected
file and propagated the resulting `TldrError::FileTooLarge` to the
caller, even though every other directory-scanning command (`vuln`,
`secure`, `structure`) soft-skips oversize files via
`tldr_core::fs::oversize::check_size`. The whole deps run was killed
by a single 2.3 MB `dom.generated.d.ts` artefact in an otherwise
healthy repo.

### Bug 2 — `tldr surface` aborted with no static entrypoint

```bash
$ tldr surface --lang typescript /tmp/repos/ts-dom-gen
Error: Parse error in /tmp/repos/ts-dom-gen: typescript package
  'ts-dom-gen' found at /tmp/repos/ts-dom-gen but no supported
  static entrypoint was found. ...                            # exit 10
```

`extract_api_surface` propagated the resolver's "no entrypoint"
parse-error to the caller, so a TypeScript build-tooling repo whose
`package.json` exposes only `scripts` (no `main`/`module`/`exports`)
could not be analysed at all — the user just got an opaque exit-10
abort instead of an empty-but-valid surface document.

### Fix

- `tldr deps` (`crates/tldr-core/src/analysis/deps.rs`): add a
  `partition_files_by_size` pre-pass that mirrors the
  `partition_utf8_clean` pattern from `secure` (M-Z8). Oversize files
  are dropped from the analysed set, counted in `DepsReport.files_skipped`,
  and surfaced as structured warnings in `DepsReport.warnings`. The
  existing parse-error recovery path is also extended to soft-skip any
  oversize file that slips past the up-front gate (e.g. files that
  grow between stat and read).
- `tldr surface` (`crates/tldr-core/src/surface/mod.rs`): when
  `resolve::resolve_target` returns the recognisable
  `"no supported static entrypoint was found"` parse-error,
  `extract_api_surface` now returns an empty `ApiSurface` populated
  with a structured warning instead of propagating exit 10. The
  language and package name are still derived from the input so the
  output remains usable by downstream tooling.

### Behaviour change

- `DepsReport` JSON now exposes two new fields: `files_skipped`
  (default 0) and `warnings` (omitted when empty). Both fields default
  via serde so older consumers continue to deserialize cleanly.
- `tldr surface` now exits 0 on entrypoint-less directories and emits
  `{ "apis": [], "warnings": [...] }` instead of exit 10.

### Validation

- `tldr deps --lang typescript /tmp/repos/ts-dom-gen` → exit 0,
  valid JSON, `files_skipped = 16`, every oversize `.d.ts` baseline
  named in `warnings`.
- `tldr surface --lang typescript /tmp/repos/ts-dom-gen` → exit 0,
  valid JSON, `apis = []`, single structured warning naming the
  missing entrypoint.
- `tldr deps /tmp/repos/flask` and `tldr surface --lang python
  /tmp/repos/flask/src/flask` regress unchanged: 83 files / 130 APIs,
  zero warnings, zero skipped.
- New tests in
  `crates/tldr-cli/tests/deps_and_surface_graceful_degrade_v1.rs`
  pin both behaviours.
- `vuln_migration_v1_red`: 168/168 GREEN.

## secure-test-file-suppression-v1 — internal milestone

NOT a published release. Bug-fix milestone restoring `tldr secure` ↔
`tldr vuln` parity on test-file suppression.

### Bug

`tldr secure` did not apply the test-file suppression filter that
`tldr vuln` applies (per M-X3 `js-test-file-suppression-v1`), so on
repos carrying JS/TS test files with taint flow the two commands
disagreed:

```bash
tldr vuln --lang javascript /tmp/repos/express | jq '.findings | length'
# 1   (index.js:21 — test/app.engine.js:9 suppressed by M-X3)

tldr secure --lang javascript /tmp/repos/express | jq '[.findings[]|select(.category=="taint")] | length'
# 2   (index.js:21 + test/app.engine.js:9 — test NOT suppressed)
```

Root cause: `secure::run` aggregated taint findings via the canonical
`scan_vulnerabilities` pipeline (post `secure-taint-aggregator-v1` and
`rust-secure-taint-aggregator-v2`) but never ran the `--include-tests`
mask that `vuln::run` applies post-analysis. The `is_rust_test_file`
check inside `analyze_rust_bounds` covered Rust unwrap-style smell
findings only; nothing covered the JS/TS taint-class path.

### Fix

`crates/tldr-cli/src/commands/remaining/secure.rs`:

1. Added `SecureArgs::include_tests: bool` (CLI flag `--include-tests`,
   default `false`), mirroring the `--include-smells` precedent — opt-in
   for noisy categories.
2. Added `apply_test_file_suppression(&mut Vec<SecureFinding>)` helper
   that runs after `all_findings` is collected and BEFORE
   `compute_summary_from_findings` (so the summary reflects the
   suppressed view, preserving the `WRAPPER-CROSS-CONSISTENCY-V1`
   invariant). The helper reuses `super::vuln::is_js_test_file` and
   `super::vuln::is_rust_test_file` (both promoted to `pub(super)` in
   `vuln.rs` for sibling-module visibility), with a universal
   `/fixtures/` exemption so any future Rust-fixture suite remains
   unsuppressed.
3. Removed the local `is_rust_test_file` definition (replaced with a
   pointer comment); the lone in-file caller in `analyze_rust_bounds`
   now delegates to `super::vuln::is_rust_test_file`. Behavior is
   byte-identical (path component `/tests/` or filename suffix
   `_test.rs` / `tests.rs`).

### Validation

* New unit tests in `secure::tests`:
  * `test_secure_default_suppresses_js_test_files` — fixture with one
    source file (`src/index.js`) and one test file
    (`test/app.test.js`), each carrying a `req.query -> res.send`
    reflected-XSS flow. Asserts default scan returns findings from the
    source file only (test file fully suppressed).
  * `test_secure_include_tests_emits_test_findings` — same fixture
    with `--include-tests=true`. Asserts findings surface from BOTH
    source and test files.
  * `test_apply_test_file_suppression_filters_js_and_rust_test_paths`
    — predicate-application unit test covering JS test paths
    (`test/`, `tests/`, `__tests__/`), JS test suffixes (`.test.{js,ts,jsx,tsx}`,
    `.spec.*`, `.e2e.*`), Rust test paths (`/tests/`, `_test.rs`,
    `tests.rs`), and the `/fixtures/` exemption.
* Binary verification on `/tmp/repos/express`:
  * Pre-fix: `vuln=1`, `secure.taint=2` (mismatch).
  * Post-fix: `vuln=1`, `secure default=1`, `secure --include-tests=2`,
    `secure.taint=1` — parity restored.
* `vuln_migration_v1_red`: 168/168 GREEN (M-X3 fixture exemption
  preserved by the universal `/fixtures/` gate in
  `apply_test_file_suppression`).
* M-X3 vuln behavior unchanged (vuln unit tests 18/18 GREEN; only
  visibility of helpers promoted, no semantic change).

## structure-json-escape-v1 — internal milestone

NOT a published release. Regression-pin milestone: adds a comprehensive
17-language JSON-validity test suite for `tldr structure` to lock in
the current correct serialization behavior of the
`FileStructure::definitions[].signature` and
`FileStructure::method_infos[].signature` fields.

### Investigation

The milestone was opened against a suspected JSON-escape bug observed
on real codebases: `tldr structure --lang rust /tmp/repos/ripgrep`
appeared to produce JSON that `jq empty` rejected with `Invalid
characters in \uXXXX escape` near a Rust source line containing
`const UTF8_BOM: &str = "\u{feff}";`. Eight languages — `cpp`,
`elixir`, `java`, `luau`, `ocaml`, `php`, `rust`, `swift` — were
flagged as suspect.

Root-cause analysis:

- `FileStructure`, `DefinitionInfo`, and `MethodInfo` all derive
  `Serialize` (see `crates/tldr-core/src/types.rs:941` for
  `DefinitionInfo` and `:999` for `MethodInfo`). The `signature` field
  is a plain `String` and is emitted via `serde_json::to_writer_pretty`
  in `OutputWriter::write` (`crates/tldr-cli/src/output.rs:97`), which
  performs RFC 8259-conformant escaping of every backslash, quote,
  control character, and non-BMP codepoint automatically.
- The `FunctionInfo` / `ClassInfo` / `FieldInfo` types in `types.rs`
  carry HAND-WRITTEN `Serialize` impls (added by
  `schema-unification-v1` BUG-17 to emit both `line_number` and `line`
  aliases). Those impls call `serializer.serialize_field` for every
  string field, which delegates to `serde_json` for proper escaping —
  no manual `format!` / `write!` shortcut exists for any string.
- Spot-checking all 17 languages against the cloned-repo corpus
  (`/tmp/repos/{ripgrep, cpp-tinyxml2, spring-petclinic,
  swift-collections, php-symfony-string, elixir-plug, ocaml-dune,
  luau-luau, c-sds, csharp-newtonsoft-bson, go-httprouter, express,
  kotlin-datetime, lua-lsp, flask, rails-html-sanitizer,
  scala-cats-effect, ts-dom-gen}`) confirmed `serde_json::from_slice`
  parses every output cleanly when stderr is properly separated from
  stdout (i.e. with `2>/dev/null` rather than `2>&1`).
- The original repro `tldr structure --lang rust /tmp/repos/ripgrep >
  out.json && jq empty out.json` failed with `Invalid numeric literal
  at line 1, column 11` — that error originates from the progress
  banner `Extracting structure from /tmp/repos/ripgrep (Rust)...`
  being captured into the output file (the banner goes to stderr but
  `>` only redirects stdout; without `2>/dev/null` it appears
  interleaved when stderr is line-buffered to a TTY). When stderr is
  separated, the JSON is well-formed.

### Fix

No source-code change is required: `tldr structure` already emits
RFC-conformant JSON across all 17 languages on `\u{feff}` (Rust),
`Pattern.compile("th:(u)?text\\\\s*=...")` (Java), `$variable`
interpolation (PHP), and every other adversarial signature content
verified.

This milestone instead lands a regression-pin test file —
`crates/tldr-cli/tests/structure_json_escape_v1.rs` — that builds
17-language fixtures, each containing the historically problematic
content (curly-brace unicode escape for Rust; backslash-regex for
Java/Scala/Kotlin/Swift/C#/Go/C/C++/OCaml; sigil + interpolation for
Elixir/Ruby/PHP; regex literal for JavaScript/TypeScript/Python/Lua;
escaped-quote string for Luau), runs `tldr structure --lang $L`, and
asserts:

1. `serde_json::from_slice(stdout)` succeeds (the `jq empty`
   contract).
2. The expected name marker (function name or constant name) is
   recoverable from the parsed JSON tree — guards against silent
   truncation at the first backslash.

A third test (`test_structure_json_handles_tab_and_backslash_quote_in_python_signature`)
pins the explicit control-char path: a Python fixture with TAB,
backslash-escaped quotes, and a regex literal in a default-value
position must round-trip through serde_json without corruption.

### Verification

- `cargo test --test structure_json_escape_v1` — 3 / 3 GREEN.
- `cargo test --test structure_method_infos_all_langs_v1` — 4 / 4
  GREEN (M-NEW2 method_infos shape contract intact across all 17
  languages — this milestone changes nothing about emission shape,
  only adds escape-validity assertions).
- `cargo test --test vuln_migration_v1_red` — 168 / 168 GREEN.
- 17-language binary sweep: `for L in c cpp csharp elixir go java
  javascript kotlin lua luau ocaml php python ruby rust scala swift
  typescript; do tldr structure --lang $L /tmp/repos/<repo> --format
  json 2>/dev/null | jq empty; done` — every language exits 0.

### Before / after JSON validity table (binary verify, 17/17 GREEN)

| Language    | Repo                       | jq empty |
|-------------|----------------------------|----------|
| c           | c-sds                      | VALID    |
| cpp         | cpp-tinyxml2               | VALID    |
| csharp      | csharp-newtonsoft-bson     | VALID    |
| elixir      | elixir-plug                | VALID    |
| go          | go-httprouter              | VALID    |
| java        | spring-petclinic           | VALID    |
| javascript  | express                    | VALID    |
| kotlin      | kotlin-datetime            | VALID    |
| lua         | lua-lsp                    | VALID    |
| luau        | luau-luau                  | VALID    |
| ocaml       | ocaml-dune                 | VALID    |
| php         | php-symfony-string         | VALID    |
| python      | flask                      | VALID    |
| ruby        | rails-html-sanitizer       | VALID    |
| rust        | ripgrep                    | VALID    |
| scala       | scala-cats-effect          | VALID    |
| swift       | swift-collections          | VALID    |
| typescript  | ts-dom-gen                 | VALID    |

### Carry-forwards

- `tldr structure` progress banners go to stderr (via
  `OutputWriter::progress`). Documentation snippets and contract
  reproductions should always pair the redirect with `2>/dev/null` to
  avoid mistaking banner-mixed output for invalid JSON. Consider
  adding a smoke test that asserts stdout-only is JSON when stderr is
  a pipe.
- The `FunctionInfo` / `ClassInfo` / `FieldInfo` hand-rolled
  `Serialize` impls remain a latent regression-vector: a future edit
  that switches any string field to a manual `serializer.serialize_str`
  path emitting pre-escaped content (or hand-rolling JSON via
  `format!`) would break the contract. The new
  `structure_json_escape_v1` test would catch any such regression at
  CI time, not at user-report time.

## secure-fastpath-v1 — internal milestone

NOT a published release. Pure performance fix: extends the M-Z4
substring/oversize fastpath (`fastpath-extend-non-vuln-v1`) to the
`secure` command's file iteration. Before this change `tldr secure
--lang typescript /tmp/repos/ts-dom-gen` ran ~154 s on the TypeScript
DOM-gen baselines tree because the 2.3 MB `dom.generated.d.ts` was
read 6 times (once per sub-analysis: taint / resources / bounds /
contracts / behavioral / mutability) and parsed 6 times into a
tree-sitter AST.

### Root cause

M-Y3 (`typescript-large-file-perf-v1`, commit `a9f3d00`) added the
oversize/auto-gen file-skipping policy to `parse_file_with_lang`, and
M-Z4 (`fastpath-extend-non-vuln-v1`, commit `b80cb9a`) extended the
substring + oversize fastpath to `patterns`, `api-check`, `debt`,
`calls`, `dead`, and `health`. `secure` was the only remaining
non-vuln command that bypassed both gates: its file walker collected
candidates and then handed them straight to `partition_utf8_clean` →
`run_security_analysis`, which read the full content into memory once
per analysis without any size policy.

### Fix

`partition_utf8_clean` (which already runs ONCE up front, before the
6 sub-analyses iterate the file set) now applies
`tldr_core::fs::oversize::check_size` BEFORE the tolerant UTF-8 read.
Files that exceed `MAX_FILE_SIZE_BYTES` (10 MB source-file cap) or
`MAX_AUTOGEN_FILE_SIZE_BYTES` (512 KB cap for `.d.ts` / `.min.js` /
`.bundle.*` auto-generated artefacts) are dropped, counted under the
existing `files_skipped` field, and surfaced via the
`format_oversize_warning` shape so consumers can distinguish oversize
skips from UTF-8 skips. Mirrors `vuln.rs::analyze_file` (covered by
M-Y3) and `api_check.rs::analyze_file` (covered by M-Z4).

### Verification

- `time tldr secure --lang typescript /tmp/repos/ts-dom-gen` — wall
  time before: 153.5 s; after: well under the 30 s budget (single-file
  stat replaces N×AST parses + N×whole-file reads).
- New `test_secure_skips_oversize_files` unit test PINS the contract:
  oversize `.d.ts` is dropped, `files_skipped` increments, warning
  uses the documented `format_oversize_warning` shape.
- `vuln_migration_v1_red`: 168/168 GREEN (unchanged).
- M-Y2 luau secure path still works: `tldr secure --lang luau
  /tmp/repos/luau-luau` exits 0 with `files_skipped=3` (the 3 corpus
  files with raw 0xFF/0xFE bytes are not oversize, so the oversize
  policy is independent of the UTF-8 policy).
- Spot-check on flask (Python), express (JavaScript), and ripgrep
  (Rust) — finding counts unchanged, no regression in detection.

## test-harness-feature-flag-v1 — internal milestone

NOT a published release. Repairs three classes of stale test failures
that had been masking real CI signal. None of these were product
defects — they were tests asserting OBSOLETE shapes after intentional
schema/feature changes had landed in earlier milestones. Per the
"No Gaming" rule, every fix updates the assertion to match the NEW
correct behavior; nothing is weakened or skipped to mask a real bug.

### Class A — 54 semantic-family tests gated on the `semantic` feature

`crates/tldr-cli/tests/exhaustive_matrix.rs` ships 18 `test_embed_on_*`,
18 `test_semantic_on_*`, and 18 `test_similar_on_*` cells (54 total)
that shell out to the `tldr` binary expecting the `embed` / `semantic`
/ `similar` subcommands to be present. Those subcommands are
`#[cfg(feature = "semantic")]` in the binary. When the test binary
was built without `--features semantic` the subcommands were absent
and every cell failed with `unrecognized subcommand 'embed'` (etc).

**Fix:** Mirror the binary's gate at the test layer. Each of the 54
test functions now carries `#[cfg(feature = "semantic")]`. The three
helpers (`check_embed`, `check_semantic`, `check_similar`), the
`embedding_mutex` `OnceLock`/`Mutex` accessor, the
`std::sync::{Mutex, OnceLock}` import, and the
`use serial_test::serial` import are all gated behind the same flag
so the no-feature build stays warning-clean.

Verification:
- `cargo test -p tldr-cli --test exhaustive_matrix -- --list | grep -E
  'test_(embed|semantic|similar)_on_' | wc -l` → 0 (no feature),
  54 (`--features semantic`).
- `cargo test -p tldr-cli --features semantic --test exhaustive_matrix
  -- test_embed_on test_semantic_on test_similar_on` → 54 passed,
  0 failed (run in isolation, ~7 min wall — fastembed cache warm).

### Class B — `test_secure_sub_results_structure` (1 test)

`crates/tldr-cli/tests/secure_sweep_tests.rs` was asserting that
secure's JSON contained `"details"` or `"sub_results"`. Per
`wrapper-cross-consistency-v1` (commit `226609d`) the secure wrapper
no longer emits per-sub-analysis records — `sub_results` is now
empty and serde-skipped, and the post-milestone shape is
`{ wrapper, path, findings[], summary{...}, total_elapsed_ms }`.

**Fix:** Rewrite to PIN the post-milestone contract. The test now
parses the JSON, asserts `sub_results` and `details` are both ABSENT
(catching regressions that re-introduce them), asserts the five
required top-level keys are present, and asserts `wrapper == "secure"`.

### Class C — 18 `test_imports_on_<lang>` matrix cells

`crates/tldr-cli/tests/language_command_matrix.rs::check_imports` was
calling `json.as_array()` on the imports output. Per
`schema-unification-v1` (commit `8d71463`) the default imports shape
is now an envelope `{ file, language, imports[] }`; the legacy
top-level array is opt-in via `--legacy-array`. All 18 cells failed
with `output is not a JSON array` after the schema change shipped.

**Fix:** Rewrite `check_imports` to parse the envelope. The helper
now asserts the three required keys (`file`, `language`, `imports`)
are present, asserts `language` matches the requested language, and
applies the existing per-language `EXPECTED_IMPORTS` exact-count
match against the envelope's `imports` array. Detection coverage
(under-counting / over-counting catch) is preserved.

### Validation results

- `cargo test -p tldr-cli --test secure_sweep_tests` → 24 passed.
- `cargo test -p tldr-cli --test language_command_matrix` → 234 passed.
- `cargo test -p tldr-cli --test exhaustive_matrix` (no feature)
  → 676 passed, 0 failed (zero `unrecognized subcommand` errors).
- `cargo test -p tldr-cli --features semantic --test
  exhaustive_matrix -- test_embed_on test_semantic_on test_similar_on`
  → 54 passed, 0 failed.
- `cargo test -p tldr-cli --features semantic --test
  vuln_migration_v1_red` → 168/168 GREEN (unchanged).

Pre-existing failures unrelated to this milestone (still RED, owned by
later milestones): `remaining_test::secure_command::test_secure_detects_taint`,
`todo_aggregation_tests::test_todo_sub_results_track_errors`,
`val003_daemon_registry_test::concurrent_add_entry_is_bounded_cas_safe`,
`rr_module_function_integ_test::ruby_io_popen_with_user_input_via_compute_taint`.
None touch the three test files modified here.

### Files modified

- `crates/tldr-cli/tests/exhaustive_matrix.rs` — 54 `#[test]` gates
  + 3 helper-fn gates + 1 `embedding_mutex` gate + 2 import gates.
- `crates/tldr-cli/tests/secure_sweep_tests.rs` — rewrote
  `test_secure_sub_results_structure` to assert the post-milestone
  envelope shape.
- `crates/tldr-cli/tests/language_command_matrix.rs` — rewrote
  `check_imports` to parse the `{ file, language, imports[] }`
  envelope and preserve EXPECTED_IMPORTS exact-count enforcement.

## detection-gap-fixes-v1 — internal milestone

NOT a published release. Closes two reflected-XSS detection gaps in the
canonical taint pipeline that allowed tainted user input to flow into
HTTP response bodies undetected.

### Gap 1 — Python Flask f-string view-function return XSS

The canonical Flask reflected-XSS shape

```python
@app.route('/echo')
def echo():
    name = request.args.get('name')
    return f"<h1>Hello {name}</h1>"   # XSS — undetected pre-fix
```

emitted ZERO `xss` findings. Root cause: `detect_sinks_ast`'s descendant
loop filters every `string`-kind node via the upstream `is_in_string`
guard (a string IS in a string), so the f-string never reached the
`AstSinkPattern` matcher. Even if it had, no entry in `PYTHON_AST_SINKS`
would have matched — the f-string is neither call-shape nor
member-access shape, just a literal returned from a function.

### Gap 2 — Next.js `NextResponse.json(tainted)` reflected XSS

`js-res-json-fp-narrowing-v1` (commit `f838387`) correctly removed
`(NextResponse, json)` from the FileWrite/PathTraversal bank — the FP
class on every Next.js App Router handler that echoed user input as
JSON. But no replacement HtmlOutput entry was added, so reflected user
input emitted via `NextResponse.json(...)` went undetected. The
companion W1-M1 #2 framework integration test
(`nextjs_response_json_reflected_xss_via_compute_taint`) was orphaned in
that state — its assertion still filtered for `FileWrite` and stayed
RED with `result.sinks=[]`.

### Fixes

**Python f-string XSS** — added a localized dispatch arm at the bottom
of `detect_sinks_ast` (mirrors the Ruby `subshell` arm) that:

1. Triggers on `descendant.kind() == "return_statement"`.
2. Walks the return's children seeking a direct `string` child.
3. Walks the string's children seeking at least one `interpolation`
   child (gates on f-strings — plain string returns carry no runtime
   variable so emit no sink, keeping FP surface minimal).
4. Walks the first interpolation's named descendants seeking the first
   plain `identifier` to extract as the sink's `var` for taint gating.
5. Pushes a `TaintSink` with `sink_type: TaintSinkType::HtmlOutput`
   (projects to `VulnType::Xss` / `CWE-79` via `vuln_type_from_sink`).

**NextResponse.json XSS** — added an additive HtmlOutput
`AstSinkPattern` entry covering BOTH `(NextResponse, json)` and
`(NextResponse, redirect)`. Wired ONLY to HtmlOutput; the prior
milestone's FileWrite removal is preserved (the FP regression-guard
fixture continues to assert ZERO `path_traversal` findings — since
`HtmlOutput` projects to `Xss`, not `PathTraversal`, the prior
narrowing is intact). `redirect` keeps its existing FileWrite entry for
open-redirect detection — the new HtmlOutput entry is additive (the
post-VULN-MIGRATION-V1-M3 for-pattern loop has no `break`, so a single
descendant emits both classifications; the downstream
`dedup_by(discriminant(sink_type))` filters same-type duplicates within
each bucket).

**Scope discipline**: Express `(res|response|Response).json` is
deliberately NOT reclassified to HtmlOutput. Express convention enforces
strict `Content-Type: application/json` and there is no ecosystem-wide
pattern of downstream HTML-interpretation that would justify the FP
cost. The narrowing is Next.js App Router-specific, where server
responses commonly feed client components that DO interpret JSON
strings as HTML (`dangerouslySetInnerHTML` reading from
`fetch().then(r => r.json())`).

### Test changes

- `crates/tldr-core/tests/rr_framework_integ_test.rs`:
  - `nextjs_response_json_reflected_xss_via_compute_taint` — assertion
    UPDATED from `FileWrite` to `HtmlOutput` (matches the test name's
    `_reflected_xss_` semantic; not a weakening — `HtmlOutput`
    projects strictly to `Xss/CWE-79`, more specific than the
    pre-narrowing `FileWrite -> PathTraversal` projection).

- `crates/tldr-cli/tests/vuln_detection_gap_fixes_v1_test.rs` (NEW):
  - `test_xss_python_fstring_view_return` — Flask f-string view return
    emits ≥1 `xss` finding.
  - `test_xss_nextjs_response_json_reflected` — `NextResponse.json(tainted)`
    emits ≥1 `xss` finding.
  - `test_xss_express_res_json_no_path_traversal` — Express
    `res.json(req.body)` emits ZERO `path_traversal` (preserves
    `js-res-json-fp-narrowing-v1`).

### Validation

- Pre-existing failing tests now GREEN:
  - `vuln_command::test_vuln_detects_xss` (Python f-string XSS).
  - `nextjs_response_json_reflected_xss_via_compute_taint`
    (NextResponse.json reflected XSS).
- `vuln_migration_v1_red`: 168/168 GREEN (no regression).
- `vuln_js_res_json_fp_narrowing_v1_test`: 2/2 GREEN (FP narrowing
  preserved).
- New regression-guard tests: 3/3 GREEN.

## cpp-method-name-extraction-v1 — internal milestone

NOT a published release. Fixes `tldr structure --lang cpp` emitting empty
strings (`""`) in the legacy flat `methods: [String]` field for inline
class methods. Pre-fix, `class Foo { void bar() {} void bar(int x) {} };`
yielded `methods: ["", "", ""]` while the companion
`method_infos: [{name,line}]` view (added by
`structure-method-infos-all-langs-v1`) correctly showed three `bar`
entries — a confusing inconsistency for JSON consumers.

### Root cause

`extract_name_from_function_declarator` in
`crates/tldr-core/src/ast/extract.rs` only matched `identifier`,
`pointer_declarator`, `qualified_identifier`, and `destructor_name` as
the leaf of a `function_declarator`'s `declarator` field. The
tree-sitter-cpp 0.23.x grammar emits `field_identifier` (NOT
`identifier`) for class-body inline method definitions:

```
function_definition
  function_declarator field=declarator
    field_identifier field=declarator: "bar"   ← leaf
```

So the chain bottomed out unmatched and returned `None`, which the
caller substituted with an empty string into
`ClassInfo.methods[].name` — which `extractor.rs::extract_structure`
then flattened into the legacy `methods: [String]` array. The
`definitions` view (which feeds `method_infos`) takes a different code
path that already handles `field_identifier`, so the two views
disagreed.

### Fix

Refactored `extract_name_from_function_declarator` to delegate to a new
recursive helper `extract_name_from_declarator_inner` that walks the
declarator chain explicitly. The helper now matches all six leaf forms
the cpp grammar can produce:

- `identifier` — plain C functions and parameters.
- `field_identifier` — C++ class/struct member declarators (the bug).
- `destructor_name` — `~Foo`.
- `operator_name` — `operator+`, `operator()`, etc. (newly handled).
- `qualified_identifier` / `scoped_identifier` — out-of-class definitions
  like `void Foo::bar() {}`. The helper recurses into the `name` field
  and returns the unqualified name (`"bar"`, not `"Foo::bar"`) so the
  out-of-class form collates with the inline form in `methods`.
- `pointer_declarator` / `reference_declarator` — recurse on inner
  `declarator`, with a children-scan fallback for grammars that omit
  the field.

The C-only call sites (`int foo(...)`, `void *get_ptr(...)`) keep
working because `identifier` and `pointer_declarator(identifier)` are
still on the matched-leaves list. No call-site changes — the helper
preserves the same `Option<String>` contract.

### Verification

```text
$ tldr structure --lang cpp /tmp/cpp_overloads.cpp \
    | jq '.files[0].methods'
# pre-fix:  ["", "", ""]
# post-fix: ["bar", "bar", "bar"]

$ tldr structure --lang cpp /tmp/repos/cpp-tinyxml2 \
    | jq '[.files[].methods[]?] | unique | .[:5]'
# post-fix: ["CloseElement","Push","TestDocLines","TestFileLines","TestParseError"]
```

Tests added:

- `crates/tldr-cli/tests/cpp_method_name_extraction_v1.rs::test_cpp_overload_method_names_extracted`
  — three-overload class, asserts `methods == ["bar","bar","bar"]` and
  three `method_infos` entries with distinct `line` values.
- `crates/tldr-cli/tests/cpp_method_name_extraction_v1.rs::test_cpp_qualified_method_name`
  — out-of-class `void Foo::bar() {}`, asserts unqualified name `"bar"`
  appears in `functions` and that no entries are empty strings.

The pre-existing
`structure_method_infos_all_langs_v1::test_structure_method_infos_distinguishes_overloads_cpp_kotlin_scala`
test now passes on its cpp leg (was failing pre-fix on the legacy
`methods` count assertion). Kotlin and Scala legs were unaffected.

Files modified:

- `crates/tldr-core/src/ast/extract.rs` — refactored helper, added doc
  comments documenting the cpp grammar shape.
- `crates/tldr-cli/tests/cpp_method_name_extraction_v1.rs` — new
  regression coverage.

### Carry-forwards

The `tldr secure -f json` test
`secure_sweep_tests::test_secure_sub_results_structure` is failing on
HEAD `b80cb9a` (pre-existing, unrelated to this milestone — caused by
the fast-path commit changing the `secure` JSON shape). Triage left to
the M-Z-FINAL re-audit.

## fastpath-extend-non-vuln-v1 — internal milestone

NOT a published release. Extends the per-function substring fast-path
proven in `vuln-fastpath-substring-prefilter-v1` (commit `7b81fa2`)
from the `tldr vuln` command to the non-vuln commands `tldr patterns`
and `tldr api-check`. The `tldr debt`, `tldr calls`, `tldr dead`, and
`tldr health` commands were measured separately and are already fast
enough on the cloned repos (see "Carry-forwards" below).

### Repro pre-fix

```text
$ time tldr patterns --lang luau /tmp/repos/luau-luau   # >60 s, timeout
$ time tldr api-check /tmp/repos/luau-luau              # 186 s
$ time tldr patterns --lang lua /tmp/repos/lua-lsp      # hangs >5 min
```

Two distinct bugs combined to produce these timeouts:

1. **`patterns` ignored `--lang`**. `patterns/mod.rs::collect_files`
   used the user's override as the file's language WITHOUT first
   checking the file's own extension. With `--lang luau` against a
   C++-heavy repo (`luau-luau`, 800+ `.cpp`/`.h` files), every
   `.cpp` was force-parsed as Luau. Tree-sitter then walked
   pathological ASTs over 200 KB+ files. Same bug class as the
   `BUG-java-debt-stackoverflow-v1` regression already fixed in
   `quality/debt.rs`.

2. **`api-check` recompiled regex per (line, rule)**. `check_regex_rule`
   called `Regex::new(spec.pattern)` *inside* the per-line loop. For
   ~800 files × thousands of lines × 5+ rules per language, the regex
   compiler dominated wall clock.

Additionally, `patterns/mod.rs::collect_files` had no oversize check,
so 1.3 MB plain-text dictionaries (`lua-lsp/meta/spell/dictionary.txt`)
were force-parsed as Lua under the override and hung tree-sitter for
five minutes. (The central `parse_file_with_lang` chokepoint enforces
the size cap, but `patterns` reads via `std::fs::read_to_string` and
dispatches through `ParserPool::parse(content, lang)` which bypasses
the path-based cap.)

### Fix

Three orthogonal changes sharing the milestone tag:

- `patterns/mod.rs::collect_files`: when `--lang` is provided, only
  include files whose detected extension matches OR whose extension
  is unknown — and even for unknown extensions we now SKIP rather
  than force-parse, because real-world Lua repos contain large
  plain-text data files. Apply the central
  `tldr_core::fs::oversize::check_size` policy at file-collection
  time. Mirrors the `BUG-java-debt-stackoverflow-v1` policy.

- `api_check.rs::analyze_file`: pre-compile each language's regex
  rules ONCE per file (the `regex_specs: Vec<(&'static RegexRuleSpec,
  Regex)>` cache) and thread the cache through `check_rule` ->
  `check_regex_rule`. Also adds a per-file substring fast-path
  (`language_fastpath_needles` + `extract_literal_from_regex`) that
  skips the per-line scan entirely when the file body contains NONE
  of the language's rule needles. The needle list is derived from
  each rule's regex pattern by emitting the longest plain-literal run
  (anchors / character-class shorthands / quantifiers / alternation
  are all handled soundly — see the
  `test_extract_literal_from_regex_recovers_useful_needles` and
  `test_fastpath_extension_no_perf_regression_on_normal_input`
  tests). For Python and Rust (which use bespoke matchers, not the
  regex spec table) the needle list is hard-coded.

  Defers oversize-skip to `tldr_core::fs::oversize::check_size` so
  generated headers / minified artefacts share the central policy.

### Perf table (BEFORE -> AFTER)

| Command                                           | BEFORE        | AFTER  | Delta     |
|---------------------------------------------------|---------------|--------|-----------|
| `patterns --lang luau /tmp/repos/luau-luau`       | timeout >60 s | 0.50 s | >120x     |
| `api-check --lang luau /tmp/repos/luau-luau`      | 186.4 s       | 0.44 s | 423x      |
| `debt --lang luau /tmp/repos/luau-luau`           | 0.40 s        | 0.38 s | (noise)   |
| `patterns --lang lua /tmp/repos/lua-lsp`          | hang >5 min   | 0.52 s | >580x     |
| `calls --lang ocaml /tmp/repos/ocaml-dune`        | 9.5 s         | 9.3 s  | (noise)   |
| `dead --lang ocaml /tmp/repos/ocaml-dune`         | 5.7 s         | 5.6 s  | (noise)   |
| `health /tmp/repos/ocaml-dune`                    | 9.6 s         | 9.5 s  | (noise)   |
| `vuln /tmp/repos/ripgrep` (M-B1 regression-guard) | 4.1 s         | 4.1 s  | unchanged |

The 168 / 168 `vuln_migration_v1_red` tests continue to pass — the
M-B1 vuln fast-path is untouched.

### Carry-forwards (commands NOT sped up)

- `calls`, `dead`, `health` on `ocaml-dune` were already <10 s before
  the milestone (call-graph builder filters by extension and reuses
  the parser cache). No change required to meet the <30 s goal.
- `debt` was already fast on luau-luau (0.4 s) because
  `BUG-java-debt-stackoverflow-v1` already restricts files by
  extension under `--lang` and applies a 500 KB MAX_FILE_SIZE cap.
- The `api-check` substring fast-path is most effective on files that
  contain NONE of the rule needles (typical for documentation /
  config / generated headers). Files that *do* contain needles still
  pay the per-line regex cost — but that cost is now O(N_lines) per
  file, not O(N_lines × N_rules) (regex-compile-once was the dominant
  factor on luau-luau).

## definition-workspace-cross-file-v1 — internal milestone

NOT a published release. Extends `tldr definition` to resolve symbols
across files automatically, without requiring the caller to pass an
explicit `--project <root>` flag.

### Repro pre-fix

```text
# Workspace: pkg/util.py defines `helper`; app.py does
#   from pkg.util import helper
#   def main(): helper()
# Cursor on the `helper()` USAGE at app.py:4:4
$ tldr definition /tmp/wsx_test/app.py 4 4
{ "symbol": { "name": "helper", "kind": "module",
  "location": { "file": ".../app.py", "line": 1 } } }
# ↑ falls through to the import line (kind=module) — never crosses files.
```

The pre-fix behaviour was: cross-file resolution only ran when the user
remembered to pass `--project <root>`. Without it, every imported usage
fell through to Pass 3 (`resolve_import_scope`) and surfaced the import
line as a `kind=module` result.

### Fix

Auto-detect the project root by walking up ancestors of the source
file looking for repository / package markers (`.git`, `Cargo.toml`,
`pyproject.toml`, `setup.py`, `package.json`, `go.mod`, `pom.xml`,
`build.gradle`, `build.gradle.kts`, `composer.json`, `Gemfile`,
`mix.exs`). The first ancestor containing any marker becomes the
implicit `--project` value.

A new `--workspace` flag (default `true`) controls auto-detection.
Pass `--workspace=false` to opt out and keep the legacy file-only
behaviour. An explicit `--project <root>` always takes precedence over
auto-detection.

The cross-file resolver itself was already wired up for all 18
languages — Python uses an import-tracing walker
(`resolve_cross_file_python`) and the other 17 use a project-wide walk
(`resolve_cross_file_walk`). Both were dormant without a workspace
root; auto-detection unlocks them.

### Validation

* Repro post-fix: cursor on `helper()` usage now resolves to
  `pkg/util.py:1` (`kind=function`) without `--project`.
* Real repo (flask): cursor on `current_app` at `flask/cli.py:396:15`
  resolves to `flask/globals.py:44` (the actual definition), not the
  import line.
* New integration test file
  `crates/tldr-cli/tests/definition_workspace_cross_file_v1.rs` —
  6 tests covering Python, TypeScript, Rust, Java, plus a backward-compat
  test for `--workspace=false` and a deeper-nested-root test.
* `vuln_migration_v1_red` 168/168 GREEN.
* M-B3 `definition-name-resolution-v1` and M-Z2
  `definition-additional-langs-v1` regression suites
  (`exhaustive_matrix::test_definition_on_*` 18/18 plus
  `remaining_test::definition_command::*` 11/11) all pass.

### Carry-forward

* For projects without any marker file (and no `--project`),
  resolution still falls back to in-file scope. Document `--project`
  as the explicit override.
* Third-party packages that live outside the workspace (e.g. `click`
  imported into flask) still resolve to the import line — by design;
  external dependency resolution requires venv/site-packages
  introspection, deferred.
* `resolve_cross_file_walk` is a brute-force project walk for
  non-Python languages. For very large monorepos a daemon-backed
  ModuleIndex would be more efficient — carry-forward.

## definition-additional-langs-v1 — internal milestone

NOT a published release. Extends the M-B3 (`definition-name-resolution-v1`)
local-scope and import-scope resolvers to the 13 supported languages that
were previously falling through to file-scope only: java, c, cpp, ruby,
kotlin, swift, scala, php, lua, luau, elixir, ocaml, csharp.

### Repro pre-fix

```text
$ tldr definition /tmp/repos/spring-petclinic/.../Vet.java 71 32
Error: symbol 'specialty' not found in scope    # param usage unresolved
$ tldr definition /tmp/repos/elixir-plug/lib/plug/html.ex 20 35
Error: symbol 'data' not found in scope         # param usage unresolved
```

Before this fix, `tldr definition` only knew how to resolve parameters,
local variables, and imports for Python, JavaScript, TypeScript, Rust,
and Go. For the other 13 languages it would walk the AST looking for
top-level functions/classes only, returning an `unresolved` error for
any local-scope or import-scope symbol — which is the common case in
real codebases (most usage sites are inside method bodies referencing
either parameters or imported names).

### Fix

`crates/tldr-cli/src/commands/remaining/definition.rs` gains:

1. **Per-language local-scope scanners** (`scan_<lang>_scope` +
   `<lang>_walk_for_binding` helpers) that walk tree-sitter ancestors
   from the cursor position and find:
   - **Java/C#**: formal parameters, `local_variable_declaration`s,
     enhanced-for loop variables, lambda parameters
   - **C/C++**: function parameters via `declarator` chain,
     `init_declarator` locals, C++ lambda parameters
   - **Ruby**: method/block parameters (regular, optional, splat,
     keyword, hash-splat, block), simple `=` assignments
   - **Kotlin**: function value parameters, lambda parameters,
     `val`/`var` property declarations
   - **Swift**: function/init parameters, lambda parameters,
     `let`/`var` property bindings
   - **Scala**: function parameters (including currying), `val`/`var`
     definitions
   - **PHP**: simple/variadic/property-promotion parameters (with
     `$` prefix tolerance), `$x = ...` assignments
   - **Lua/Luau**: function parameters (Lua flat `identifier` form,
     Luau `parameter`-wrapped form), `local` variable declarations
   - **Elixir**: `def`/`defp`/`defmacro`/`defmacrop` parameters
     including the `when guard(x)` form (left side of `binary_operator`),
     `stab_clause` anonymous function parameters, simple `=` matches
   - **OCaml**: `let_binding` parameters (`parameter` > `value_pattern`),
     anonymous `fun`/`function` parameters, top-level `let` bindings
2. **Per-language import-scope finders** (`<lang>_<keyword>_line`):
   - **Java**: `import com.foo.Bar;` → `Bar`; `import static X.Y;` → `Y`
   - **Kotlin/Scala**: `import a.b.C` → `C`; `import a.b.C as D` (Kotlin) → `D`;
     `import a.b.{X, Y => Z}` (Scala) → `X` and `Z`
   - **Swift**: `import Foundation` → `Foundation`; `import class Foo.Bar` → `Bar`
   - **PHP**: `use Foo\Bar\Baz;` → `Baz`; `use Foo\Bar\Baz as Qux;` → `Qux`;
     `use Foo\{A, B as C};` → `A` and `C`
   - **C#**: `using System;` → `System`; `using X = Foo.Bar;` → `X`
   - **Lua/Luau**: `local foo = require("...")` → `foo`
   - **Elixir**: `alias Foo.Bar` → `Bar`; `alias Foo.Bar, as: Qux` → `Qux`;
     `alias Foo.{A, B}` → `A` and `B`; same for `import`/`use`/`require`
   - **OCaml**: `open Foo.Bar` → `Bar`; `module M = Foo.Bar` → `M`

C, C++, and Ruby don't bind specific symbol names at the import/include
level (`#include` is preprocessor; Ruby's `require` registers a global
side-effect). Those languages get local-scope resolution only and fall
through to file-scope for cross-module names.

### Validation

18 new unit tests in `definition.rs::tests` cover every new language:

- 13 `test_definition_resolves_local_param_<lang>` (java, c, cpp, ruby,
  kotlin, swift, scala, php, lua, luau, elixir, ocaml, csharp) — synthetic
  source + cursor on parameter usage resolves to the parameter declaration
- 5 broader tests (`test_definition_resolves_import_alias_java`,
  `test_definition_resolves_local_var_kotlin`,
  `test_definition_resolves_param_swift`,
  `test_definition_resolves_use_statement_php`,
  `test_definition_resolves_local_var_csharp`) cover import statements
  and var-decl forms

Binary-verified on cloned repos:

- `spring-petclinic` (Java): `Vet.java:71:32` → `specialty` resolves to
  param decl at `Vet.java:70:36` (PASS)
- `kotlin-datetime` (Kotlin): `CommonFormats.kt:44:9` → `blackhole`
  resolves to param decl at `CommonFormats.kt:24:34` (PASS)
- `elixir-plug` (Elixir): `html.ex:20:35` → `data` resolves to param decl
  at `html.ex:19:18` (with `when is_binary(data)` guard, exercising the
  `binary_operator` head form) (PASS)

The full tldr-cli test suite (1418 tests) passes; M-B3's existing 5
languages are untouched.

### Carry-forward

Languages with grammars that have multiple AST shapes for the same
construct may have edge cases not covered by the synthetic tests:

- **C/C++**: function-pointer-typed parameters, K&R-style declarations,
  template parameters in C++ — the scanner recognises the common
  `parameter_declaration` + `init_declarator` shapes and falls through
  for exotic forms
- **Scala**: implicit parameter blocks (separate parameter group) work
  via the `parameters | bindings` recursion; given/extension methods are
  not specifically handled and may return None (acceptable fallback)
- **Lua**: vararg `...` parameters bind to a special name; we don't
  resolve `...` references — this is consistent with how the existing
  language handlers ignore varargs
- **Elixir**: pin operator `^x` references aren't traced back to outer
  bindings — this is a future enhancement
- **OCaml**: pattern-bound parameters (`let f (Some x) = ...`) work for
  simple cases via the recursive `ocaml_find_first_ident`; record-pattern
  destructuring isn't specially handled

These gaps are documented as future enhancements; they don't block the
canonical "param usage → param decl" and "imported name → import line"
resolutions which were the user-visible gaps motivating this milestone.

## complexity-class-method-qualified-v1 — internal milestone

NOT a published release. Per-function commands (`complexity`, `explain`,
`taint`, `slice`, `chop`, `dead-stores`, `available`, `reaching-defs`,
`contracts`) previously rejected `Class.method` qualified names with
`Function not found`. Real codebases — Flask, Django, Rails, Spring,
React class components — frequently have many classes that share a
method name (e.g. `run`, `init`, `handle`, `start`). Without
class-scoped resolution the user could only target the FIRST match,
which is a correctness footgun.

### Repro pre-fix

```text
$ tldr complexity /tmp/repos/flask/src/flask/app.py "Flask.run"
Error: Function not found
$ tldr complexity /tmp/repos/flask/src/flask/app.py "run"
{ "function": "run", ... }   # ambiguous — Flask.run vs MapAdapter.run vs ...
```

### Fix

Extend the canonical AST resolver
`crates/tldr-core/src/ast/function_finder.rs::find_function_node` to
recognise dotted names. When the input contains a `.`:

1. Split into `Class.method` (or `Outer.Inner.method` — leftmost is
   the class, the remainder is searched recursively inside it).
2. Locate the class via the new `find_class_node` /
   `get_class_node_kinds` helpers, which know about class-equivalent
   containers across all 18 supported languages (classes, structs,
   traits, impls, interfaces, records, enums, objects, protocols,
   extensions).
3. Search the method INSIDE the class body. First match wins.
4. **Graceful fallback:** if the class doesn't exist, OR the method
   isn't inside the class, fall back to bare-name lookup using the
   LAST component (`Class.method` → `method`). This preserves
   backward compatibility for users who pass dotted names that
   don't actually correspond to a class scope.

Lua/Luau are deliberately skipped from class scoping because their
dot-indexed function form (`function Kong.init() … end`) is matched
directly by the existing bare-name branch (no class node to descend
into). The new code path therefore never disturbs that resolution.

The two CLI-side duplicate resolvers
(`crates/tldr-cli/src/commands/remaining/explain.rs::find_function_node`
and `crates/tldr-cli/src/commands/contracts/contracts.rs::find_function_node`)
get the same dispatch logic so `explain` and `contracts` inherit the
fix. All other per-function commands (taint/slice/chop/dead-stores/
available/reaching-defs) route through CFG/DFG/PDG extractors which
use the canonical resolver — they inherit transparently.

### Limitation: overloaded methods

Java, C++, Kotlin, and Scala all permit method overloading. When two
methods in the same class share a name, FIRST match wins. This is the
same behaviour as the existing bare-name lookup. To disambiguate by
line range or signature, callers must add that resolution at a higher
level — `find_function_node` does NOT attempt overload resolution.

### Verification

```text
$ tldr complexity /tmp/repos/flask/src/flask/app.py "Flask.run"
{
  "function": "Flask.run",
  "cyclomatic": 13,
  "cognitive": 20,
  "nesting_depth": 3,
  "lines_of_code": 122
}
```

### Tests added

11 new tests in `function_finder::tests` covering:
- `test_qualified_class_method_python`
- `test_complexity_unqualified_still_works` (regression)
- `test_qualified_class_not_found_falls_back_to_method`
- `test_qualified_class_method_typescript`
- `test_qualified_class_method_rust_impl`
- `test_qualified_class_method_java`
- `test_qualified_lookup_via_complexity_python`
- `test_qualified_lookup_via_dfg_python` (covers taint/slice/dead-stores/available/reaching-defs)
- `test_qualified_lookup_via_cfg_python` (covers chop)
- `test_qualified_class_method_disambiguates_overloaded`
- `test_find_class_node_python` and `test_find_class_node_languages_without_classes`

`vuln_migration_v1_red`: 168/168 GREEN. All other lib + integ tests
unchanged (the 18 `test_imports_on_*` failures predate this milestone
and are unrelated to function lookup — see `git log` for
`language_command_matrix`).

## elixir-method-infos-v1 — internal milestone

NOT a published release. MED extractor parity fix completing the
intent of `structure-method-infos-all-langs-v1`. After that prior
fix, `tldr structure --lang elixir` still emitted
`method_infos: []` on every file even when the legacy
`methods: [String]` array was populated — across all 77 files of
the `elixir-plug` corpus (840 legacy methods → 0 `method_infos`).

### Bug

`crates/tldr-core/src/ast/extractor.rs::try_elixir_call_definition`
unconditionally tagged `def`/`defp` calls with `kind: "function"`.
The downstream filter
`definitions.filter(|d| d.kind == "method")` (lines 203–211 of the
same file) therefore returned an empty `Vec<MethodInfo>` for every
Elixir file — even for `def`/`defp` declared inside a
`defmodule … do … end` block, which is the conventional Elixir
analogue of class-scoped methods in Ruby/Python/Java.

Concrete repro pre-fix:

```text
$ tldr structure --lang elixir /tmp/repos/elixir-plug \
    | jq '[.files[].method_infos | length] | add'
0
$ tldr structure --lang elixir /tmp/repos/elixir-plug \
    | jq '[.files[].methods | length] | add'
840
```

### Fix

Mirror the Ruby/Python "class-scoped def is a method" classification
in the Elixir branch. A new helper `is_inside_elixir_defmodule`
walks the parent chain of a `def`/`defp` `call` node and returns
true iff any ancestor `call` has its first identifier child equal to
`defmodule`. When true, `try_elixir_call_definition` now emits
`kind: "method"` instead of `"function"`. Top-level `def`/`defp`
(legal in Mix scripts and `iex` sessions) keep the `function` tag.

Post-fix on the same corpus:

```text
$ tldr structure --lang elixir /tmp/repos/elixir-plug \
    | jq '[.files[].method_infos | length] | add'
939
```

The legacy `methods: [String]` array is unchanged (same names,
same length per file) — the contract is purely additive.

### Tests

- `crates/tldr-cli/tests/elixir_method_infos_v1.rs`:
  - `test_structure_elixir_method_infos_populated` — synthetic
    `defmodule Foo do ; def bar(x) ; defp baz ; end` fixture
    asserts both names appear in `method_infos` with positive
    `line` and a `def `/`defp ` signature, and both names also
    appear in the legacy `methods: [String]` array.
  - `test_structure_elixir_method_infos_count_matches_methods`
    pins `methods.len() == method_infos.len()` for the same
    fixture (count parity inside a single defmodule).

### Validation

- 168/168 GREEN: `vuln_migration_v1_red`.
- 4691/4691 GREEN: `tldr-core` lib.
- 1400/1400 GREEN: `tldr-cli` lib.
- M-NEW2 regression `test_structure_method_infos_emitted_all_langs`
  remains GREEN.
- Binary verify on `/tmp/repos/elixir-plug`: 77 files, 939
  `method_infos` (was 0), 840 legacy `methods` (unchanged).

### Carry-forward

- The pre-existing
  `test_structure_method_infos_distinguishes_overloads_cpp_kotlin_scala`
  test fails on the cpp leg with empty-string method names from
  the legacy `methods` array. The failure is unrelated to this
  milestone — only the Elixir branch of `extract_definitions` was
  touched, and the fixture/assertion paths in that test never
  exercise Elixir. Tracked separately.

## typescript-large-file-perf-v1 — internal milestone

NOT a published release. HIGH perf ship-blocker fix: six commands
(`structure`, `calls`, `smells`, `dead`, `secure`, plus other
parse-based scanners) timed out at 30 s on a single 2.3 MB
auto-generated TypeScript declaration file
(`/tmp/repos/ts-dom-gen/baselines/dom.generated.d.ts`). The same
repo's `src/` finished in 0.02 s. The bottleneck was super-linear
per-file analysis on a dense `.d.ts` artefact, which is rarely
valuable to analyse deeply.

### Bug fixed

A `MAX_FILE_SIZE = 10 MB` cap existed in
`crates/tldr-cli/src/commands/remaining/vuln.rs` and the
`patterns/contracts` validation modules but was NOT enforced
uniformly across all parse-based commands, and the cap was too
loose for auto-generated artefacts: a 2.3 MB `.d.ts` is well under
10 MB but takes ~40 s of per-method-info AST work because the file
holds tens of thousands of generated method declarations.

Concrete repro pre-fix:

```text
$ time timeout 30 tldr structure /tmp/repos/ts-dom-gen/baselines/dom.generated.d.ts
... 30.00s timeout, exit 124
```

### Fix

Centralised the file-size policy in a new module,
`crates/tldr-core/src/fs/oversize.rs`, and enforced it at file-read
time in `crates/tldr-core/src/ast/parser.rs::parse_file_with_lang`
— the single chokepoint every parse-based command goes through.
Two-tier policy:

- **Normal source files**: 10 MB cap (matches the historical
  per-command cap in `patterns/contracts/vuln`).
- **Auto-generated / minified files** (`.d.ts`, `.min.js`,
  `.bundle.css`, `.min.css`, `.bundle.js`, plus `.mjs`/`.cjs`
  variants): 512 KB cap. Empirically chosen against the
  `ts-dom-gen` baselines tree (60+ `*.generated.d.ts` artefacts in
  the 100 KB – 2.3 MB range): a 1 MB cap left ~12 baselines
  admitted and the whole-repo run took 58 s; 512 KB drops the run
  under 30 s while admitting every hand-authored `.d.ts` shim
  observed in `tldr-rs-canonical` (the largest is 75 KB).

Oversize files now propagate as the existing recoverable
`TldrError::FileTooLarge` (added to `is_recoverable()` so callers
treat it as a per-file skip, not a hard error). The structure
entrypoint (`get_code_structure` in
`crates/tldr-core/src/ast/extractor.rs`) catches that variant and
records the skip in two new fields on `CodeStructure`:

- `files_skipped: u32` — count of oversize files dropped
- `warnings: Vec<String>` — per-file skip messages

Both use `serde(default, skip_serializing_if = ...)` so clean
inputs see no JSON schema delta — existing consumers are
unaffected. Warning format is stable:

```text
Skipped <path>: 3MB exceeds 512KB cap for auto-generated/minified files
Skipped <path>: 12MB exceeds 10MB cap for source files
```

Sub-MB sizes render as KB; ≥1 MiB sizes render as MB (round up).

### Verification

Before fix:

| target                             | time   | exit |
|------------------------------------|--------|------|
| `structure` on `dom.generated.d.ts`| > 30 s | 124  |
| `structure` on whole `ts-dom-gen`  | > 30 s | 124  |

After fix:

| target                             | time     | exit |
|------------------------------------|----------|------|
| `structure` on `dom.generated.d.ts`| ≈ 0.6 s  | 0    |
| `structure` on whole `ts-dom-gen`  | ≈ 0.8 s  | 0    |
| `calls` on whole `ts-dom-gen`      | ≈ 0.4 s  | 0    |
| `structure` on `ts-dom-gen/src`    | ≈ 0.02 s | 0    |

The whole-repo run reports `files_skipped = 16`, with one warning
per skipped baseline.

### Tests added

- `crates/tldr-core/src/fs/oversize.rs` — 11 unit tests covering
  `is_autogen_file`, `max_size_for`, `check_size`,
  `format_oversize_warning`, and `format_size`.
- `crates/tldr-cli/tests/typescript_large_file_perf_v1.rs` — three
  binary-level tests:
  - `test_skip_oversize_file_with_warning` — synthetic dir with
    one valid file + one over-cap `.d.ts`; the scan completes,
    `files_skipped == 1`, and `warnings[0]` references the
    skipped path with the documented "exceeds" phrasing.
  - `test_dts_files_have_lower_cap` — synthetic `.d.ts` sized to
    straddle the 512 KB autogen cap and the 10 MB source cap;
    must be skipped, with the warning labelling it as
    `auto-generated/minified files` (proves the auto-gen branch
    is what fired).
  - `test_normal_ts_file_below_10mb_not_skipped` — negative
    control: a sub-10 MB normal `.ts` MUST NOT be skipped (the
    auto-gen cap doesn't apply to it).

### Carry-forwards (intentional non-scope)

- The `tldr smells`, `tldr dead`, `tldr secure` commands inherit
  the policy via `parse_file_with_lang` but do not yet surface
  their own `files_skipped` / `warnings` fields — the warning
  surfaces only on `tldr structure` for now. Extending the
  surfacing to those reports is a follow-up.
- Existing auto-gen detection is path-suffix based (`.d.ts`,
  `.min.js`, `.bundle.*`); content-based detection (e.g. minified
  source with no `.min` in the name) is out of scope.
- Pre-existing test failures unrelated to this milestone:
  - `test_secure_sub_results_structure` (asserts a JSON key that
    a prior milestone's schema simplification removed)
  - `nextjs_response_json_reflected_xss_via_compute_taint`
    (asserts `FileWrite` sink type that
    `vuln-source-parity-v1 M3 ATOMIC` reclassified to
    `HtmlOutput`)
  - `test_embed_on_*`, `test_semantic_on_*`, `test_similar_on_*`
    (54 tests in `exhaustive_matrix.rs` that assume an `embed`
    subcommand the binary doesn't expose)
  All four pre-date this commit and are not affected by the
  oversize-policy change.

## secure-utf8-tolerance-v1 — internal milestone

NOT a published release. HIGH ship-blocker fix: `tldr secure --lang luau
<repo>` aborted the entire scan with `Error: stream did not contain
valid UTF-8` and exited 1 on the first non-UTF-8 file in the tree
(e.g. the upstream luau-luau repo's `tests/conformance/literals.luau`,
`pm.luau`, `sort.luau` parser-test fixtures with raw 0xFF/0xFE bytes).

### Bug fixed

The prior `luau-utf8-tolerance-v1` (commit 4c61af8) added the tolerant
`read_to_string_tolerant` helper in `crates/tldr-core/src/fs/mod.rs`
and wired it into `surface/luau.rs` and `surface/lua.rs` only — but
`tldr secure` has its own file-iteration path
(`run_security_analysis` in
`crates/tldr-cli/src/commands/remaining/secure.rs`) that called
strict `std::fs::read_to_string(file)?`. The `?` propagated the
`io::Error("stream did not contain valid UTF-8")` returned by
`String::from_utf8` and aborted the scan on the first bad file —
losing all 111/114 perfectly-scannable files.

### Fix

Pre-filter the candidate file set ONCE in
`crates/tldr-cli/src/commands/remaining/secure.rs::run` via the new
`partition_utf8_clean` helper, which uses `read_to_string_tolerant`
and emits a structured warning (`"Skipped <path>: invalid UTF-8 at
byte <N>"`) per non-UTF-8 file. The 6 sub-analyses then iterate the
clean set; a defense-in-depth tolerant re-read inside
`run_security_analysis` covers TOCTOU races (file replaced between
the partition pass and the analysis pass).

`SecureReport` gains two backward-compatible fields
(`crates/tldr-cli/src/commands/remaining/types.rs`):

- `files_skipped: u32` — count of non-UTF-8 files dropped
- `warnings: Vec<String>` — per-file skip messages with byte offsets

Both use `serde(default, skip_serializing_if = ...)` so UTF-8-clean
inputs see no JSON schema delta — existing consumers are unaffected.

### Coverage extended to `vuln`

`tldr vuln` previously silently dropped non-UTF-8 files via an
`if let Ok(..)` guard around `analyze_file`, so the user had no
signal that coverage was degraded. Same pre-classification pattern
applied in `crates/tldr-cli/src/commands/remaining/vuln.rs`,
populating the new `files_skipped` + `warnings` fields on
`VulnReport`.

### Carry-forwards (intentional non-scope)

- `tldr structure` and `tldr calls` already route through
  `tldr_core::ast::parser::parse_file_with_lang` which uses
  `String::from_utf8_lossy` (M2 mitigation) — they continue with
  lossy decode for non-UTF-8 files. Adding `warnings`/`files_skipped`
  to those reports is left to a follow-up; binary-verified to
  succeed cleanly on luau-luau.
- `tldr smells` uses `std::fs::read_to_string(path).unwrap_or_default()`
  in `crates/tldr-core/src/quality/smells.rs:564` — non-UTF-8 files
  scan as empty source. Defensive, no abort, but no warning surfaced.
  Behavior unchanged in this milestone; binary-verified to succeed.
- `tldr api-check` succeeded on luau-luau pre-fix (it currently has no
  `.luau` rule corpus so the bad files never reach
  `analyze_file`'s `fs::read_to_string`). No fix required for this
  repro; `analyze_file` itself remains strict and would fail on a
  hypothetical non-UTF-8 file in a supported language.

### Verification

- Repro pre-fix: `tldr secure --lang luau /tmp/repos/luau-luau` →
  `Error: IO error: stream did not contain valid UTF-8`, exit 1.
- Post-fix: same command → exit 0, JSON valid, `files_skipped: 3`,
  3 warnings naming the 3 luau-luau parser-test fixtures with byte
  offsets 2112, 2335, 2772.
- M-X5 surface preserved: `tldr surface --lang luau /tmp/repos/luau-luau`
  still reports `files_skipped: 3` with the same 3 warnings.
- `vuln_migration_v1_red`: 168/168 stays GREEN.
- `tldr-core` lib: 4680/4680 pass.
- New tests:
  - `secure_sweep_tests::test_secure_continues_after_bad_file_in_dir`
  - `secure_sweep_tests::test_secure_clean_input_has_no_skip_fields`
    (schema backward-compat guard)
  - `secure_utf8_tolerance_v1::test_smells_continues_after_bad_file_in_dir`
  - `secure_utf8_tolerance_v1::test_structure_continues_after_bad_file_in_dir`
  - `secure_utf8_tolerance_v1::test_vuln_continues_after_bad_file_in_dir`

### Files modified

- `crates/tldr-cli/src/commands/remaining/secure.rs` —
  `partition_utf8_clean` helper; `run` threads warnings/skip count
  into report; `run_security_analysis` defensive tolerant read
- `crates/tldr-cli/src/commands/remaining/vuln.rs` — pre-classify
  non-UTF-8 files; thread `files_skipped` + `warnings`
- `crates/tldr-cli/src/commands/remaining/types.rs` — `SecureReport`
  + `VulnReport` gain `files_skipped: u32` and `warnings:
  Vec<String>` (both `skip_serializing_if`)
- `crates/tldr-cli/tests/secure_sweep_tests.rs` — 2 new tests
- `crates/tldr-cli/tests/secure_utf8_tolerance_v1.rs` — new file, 3
  tests covering smells / structure / vuln

## java-debt-stackoverflow-v1 — internal milestone

NOT a published release. Fixes a CRITICAL bug: `tldr debt --lang java
<repo>` aborted the entire process with `fatal runtime error: stack
overflow, aborting` (SIGABRT) on real-world Java repositories such as
spring-petclinic.

### Bug fixed

`tldr debt --lang <X>` was force-parsing every file in the tree as
language `X` — including HTML templates, `.properties` files, `.sql`
schemas, `.scss` stylesheets, `.txt` banners, etc. Tree-sitter applied
to extremely off-grammar input produced pathological deep ASTs; the
recursive walks in `crates/tldr-core/src/quality/debt.rs` (notably
`extract_java_functions_for_debt`, `walk_nesting_depth`,
`find_python_missing_docs`, `extract_python_classes_for_lcom4`) then
blew the rayon worker thread stack (~512KB on macOS) and crashed the
process. Repro: `tldr debt --lang java /tmp/repos/spring-petclinic` →
SIGABRT.

### Fix

Two-layer defence in `crates/tldr-core/src/quality/debt.rs`:

1. **Walker filter (primary).** When `--lang X` is provided, only
   include files whose extension matches language `X`, plus files
   with no detectable language (so the user override still applies to
   unknown extensions). Files of a *different* known language (e.g.
   `.html`, `.py` when `--lang java` was passed) are excluded — both
   semantically correct and prevents the pathological-AST trigger.

2. **AST recursion bound (defence-in-depth).** Introduced
   `DEBT_MAX_AST_DEPTH = 256` and threaded a `depth` parameter
   through every recursive AST walk in the debt module:
   - `extract_java_functions_for_debt`
   - `extract_python_functions_for_debt`
   - `extract_ts_functions_for_debt`
   - `extract_rust_functions_for_debt`
   - `extract_python_classes_for_lcom4`
   - `find_python_missing_docs` / `check_python_class_docs`
   - `walk_nesting_depth` (now delegates to a bounded helper).

   On hitting the depth bound, recursion stops early and any partial
   results gathered so far are returned (graceful degradation rather
   than abort).

### Tests

New tests in `crates/tldr-core/src/quality/debt_tests.rs`
(`mod java_debt_stackoverflow_v1_tests`):

- `test_debt_java_no_stack_overflow_on_mixed_tree` — synthetic mini
  spring-petclinic with 10 Java files using F-bounded polymorphism
  and mutually recursive methods, alongside `.properties`, `.html`,
  `.sql`, `.scss`, `.txt`. Asserts `analyze_debt` returns Ok rather
  than aborting.
- `test_debt_lang_override_excludes_other_known_languages` — under
  `--lang java`, a Python file's TODO must NOT appear in results.
- `test_debt_other_langs_no_regression` — debt analysis on Python,
  Rust, and TypeScript still detects TODOs after the recursion-guard
  refactor.

### Verification

- Pre-fix: `tldr debt --lang java /tmp/repos/spring-petclinic` →
  `fatal runtime error: stack overflow, aborting` (process killed).
- Post-fix: same command exits 0 with valid JSON
  (`total_minutes=45`, two findings: `complexity.very_high`,
  `long_param_list`).
- `tldr debt --lang java /tmp/repos/kotlin-datetime` → exit 0.
- `tldr debt` smoke tests across 19 repos in `/tmp/repos` → all clean.
- `cargo test -p tldr-core --lib` → 4680 passed, 0 failed.
- `cargo test -p tldr-cli --lib` → 1400 passed, 0 failed.
- `cargo test -p tldr-cli --test vuln_migration_v1_red` → 168/168.

## definition-name-resolution-v1 — internal milestone

NOT a published release. Closes deferred BUG-24: `tldr definition <file>
<line> <col>` was stubbed for usage sites — it only resolved when the
cursor sat ON a function/class declaration. Cursors on USAGE sites
returned an opaque `<unknown at FILE:LINE:COL>` payload.

### Bug fixed

- **BUG-24 — `definition` failed to resolve usage sites.** Example on
  flask:
  - `tldr definition /tmp/repos/flask/src/flask/cli.py 41 4` →
    resolved to `find_best_app` decl line 41 (DECL site, worked).
  - `tldr definition /tmp/repos/flask/src/flask/cli.py 274 4` →
    `<unknown at .../cli.py:274:4>` (USAGE site — line 274 is
    `click.echo(...)`, cursor on `click`). The expected behaviour is
    to resolve `click` to `import click` (line 17 in the current
    flask source).
  Same bug shape applied to local parameter usages, local
  `let`/`var` bindings, and aliased imports across all languages.

### Fix

Replace the stub with a three-pass resolver in
`crates/tldr-cli/src/commands/remaining/definition.rs`:

1. **Local scope** (new): walk up tree-sitter ancestors from the
   cursor; for each function/method/closure/block ancestor, scan
   parameters and `let`/`const`/`var`/`assignment` bindings. The
   first matching binding wins (innermost wins). Stops at nested
   scope boundaries so an outer name can't shadow an inner binding
   in the wrong direction.
2. **File scope** (existing): the legacy
   [`find_symbol_in_file`] handler — covers top-level functions,
   classes, and Python module-level assignments.
3. **Import scope** (new): scans `import X` / `import X as Y` /
   `from M import Y as Z` (Python), `import X from "..."` /
   `import { Y as Z } from "..."` / `import * as X from "..."`
   (JS/TS), and `use ::path::X;` / `use ::path::X as Alias;`
   (Rust). On match, returns the import line.

If all three passes miss, the result is now a clear
`<unresolved at FILE:LINE:COL — symbol 'X' not found in scope>`
payload instead of the legacy opaque `<unknown ...>`.

### Coverage by language

| Language    | Pass 1 (Local) | Pass 2 (File) | Pass 3 (Import) |
|-------------|----------------|---------------|-----------------|
| Python      | params, `=` assignments, `for` targets | full       | `import` / `from ... import`     |
| JavaScript  | params, `let`/`const`/`var`            | full       | `import { } from`, default, `* as` |
| TypeScript  | params, `let`/`const`/`var`            | full       | `import { } from`, default, `* as` |
| Rust        | params, `let` bindings                 | full       | `use ::path::Name;` (non-grouped) |
| Go          | params, `:=` short-var-decl            | full       | (carry-forward — no Go-specific import scope) |
| Java/C/C++/Ruby/Kotlin/Swift/Scala/PHP/Lua/Luau/Elixir/OCaml/C# | (carry-forward — local-scope unimplemented) | full | (carry-forward) |

### Tests

Six new unit tests in
`crates/tldr-cli/src/commands/remaining/definition.rs`:

- `test_definition_resolves_local_param` — Python parameter usage
- `test_definition_resolves_file_scope_function` — Python file-scope function
- `test_definition_resolves_import_alias` — Python import alias (BUG-24 repro shape)
- `test_definition_unresolved_message` — checks the new `unresolved at` message
- `test_definition_resolves_js_import_alias` — JS `import express from "express"`
- `test_definition_resolves_rust_let_binding` — Rust `let counter = 42` usage

### Binary verification

| Case                                                  | Before                                       | After                                |
|-------------------------------------------------------|----------------------------------------------|--------------------------------------|
| flask cli.py:274:4 (cursor on `click`)                | `<unknown at .../cli.py:274:4>`              | `import click` line 17 (Module)      |
| flask cli.py:262:19 (usage of `find_best_app`)        | `<unknown ...>`                              | decl line 41 (Function)              |
| flask cli.py:41:4 (decl of `find_best_app`)           | decl line 41                                 | decl line 41 (regression OK)         |
| express application.js:471:0 (`methods` var binding)  | `<unknown ...>`                              | `var methods = ...` line 20 (Variable) |
| /tmp/test_unresolved.py:2:11 (nonexistent name)       | `<unknown at ...>`                           | `<...unresolved at ... — symbol 'notthere' not found in scope>` |

### Carry-forwards (NOT in this milestone)

- Workspace-wide cross-file resolution from a position site (the
  CLI's existing `find_definition_by_name` already supports it via
  `--project`, but the position-mode resolver only reuses it
  inside Pass 2; for true cross-file go-to-definition with
  module-aware import following, see the daemon `ModuleIndex`).
- Local-scope resolution for the remaining 13 languages (Java,
  C, C++, Ruby, Kotlin, Swift, Scala, PHP, Lua, Luau, Elixir,
  OCaml, C#). They fall through cleanly to file/import passes,
  but param/local-binding usages still return `<unresolved at ...>`.
- Grouped Rust `use a::{b, c};` imports (we skip lines containing
  `{`).
- Multi-line JS/TS `import { a,\n b\n } from ...` (line-based
  scanner).
- Go-specific import-scope resolution (Go uses `import "path"`
  and dotted-path access; bound names are package roots, which
  vary by tooling).

## canonical-function-enumerator-v1 — internal milestone

NOT a published release. Closes deferred BUG-01: `health`, `structure`,
and `dead` reported three different function totals on the same input.

### Bug fixed

- **BUG-01 — function counts disagreed across commands.** Example on
  `/tmp/repos/flask`:
  - `tldr health` → `summary.functions_analyzed = 854`
    (complexity hotspot subset, dunders excluded, only functions with
    a metrics-map hit counted)
  - `tldr structure` → sum(`functions`) + sum(`methods`) = 857
    (separate AST walk in `extractor.rs`, missing some assigned
    function-expressions)
  - `tldr dead` → `total_functions = 918`
    (full `extract_file`-based enumeration via `collect_all_functions`)
  Three different numbers on the same input.

### Fix

Introduce a single canonical enumerator and route the three wrappers
through it.

- New: `tldr_core::ast::count_functions_canonical(path, language) -> u32`
  and `count_functions_canonical_from_modules(&module_infos) -> u32`
  in `crates/tldr-core/src/ast/count.rs`. The canonical enumerator
  walks files via `extract_file` and sums
  `info.functions.len() + Σ class.methods.len()`.
- `health` (via `quality::complexity::analyze_complexity`):
  `functions_analyzed` is now sourced from the canonical enumerator.
  Per-function complexity rows (`functions`, `hotspots`) keep their
  metrics-derived subset semantics — only the headline count is
  canonicalized.
- `structure` (via `ast::extractor::extract_file_structure`): the
  `functions` and `methods` arrays are now derived from
  `extract_from_tree`'s `ModuleInfo`, so
  `sum(files[].functions) + sum(files[].methods)` agrees with the
  canonical count. `classes` (string list) and `definitions` are
  unchanged.
- `dead` (via `quality::dead_code::analyze_dead_code`): already used
  the canonical enumeration through `collect_all_functions` — no
  change needed; the new shared utility documents and codifies that
  policy.

### Inclusion policy (canonical)

A "function" for canonical-count purposes is anything that
`extract_file` surfaces in `ModuleInfo.functions` (top-level) or
`ClassInfo.methods` (class members). This includes:

- All top-level `def` / `function` / `fn` / `func` declarations.
- All class methods (including dunder methods like `__init__`,
  `__repr__`).
- All assigned function-expression / arrow-function values from
  `js-extract-function-expressions-v1` (`const f = () => {}`,
  `const f = function() {}`).

It does NOT include:

- Anonymous lambdas / inline arrow callbacks not bound to a name.
- Computed-property method names that the AST extractor cannot
  resolve to a stable string identifier.
- Decorated stubs without a body.

### Out of scope: `verify`

`tldr verify` reports `coverage.total_functions` and is intentionally
NOT unified with the canonical count. That field is a *different*
metric — the count of functions whose contracts (pre/postconditions)
are extractable. It will routinely be smaller than the canonical
function count and that is correct: it measures contract coverage,
not raw function enumeration. Users comparing `verify`'s number to
`health`/`structure`/`dead` are comparing apples to oranges.

### Validation (binary verify, post-install)

| Repo | health | structure (Σ funcs+methods) | dead | agree? |
|------|-------:|----------------------------:|-----:|:------:|
| flask         | 918  | 918  | 918  | ✓ |
| ripgrep       | 2739 | 2739 | 2739 | ✓ |
| express       | 283  | 283  | 283  | ✓ |
| c-sds         | 51   | 51   | 51   | ✓ |
| elixir-plug   | 1788 | 1788 | 1788 | ✓ |

### Tests

- New: `crates/tldr-core/tests/canonical_function_count_v1.rs` with
  `test_canonical_count_agrees_health_structure_dead_python`,
  `_rust`, `_javascript` — each constructs a small fixture and
  asserts all four producers (canonical, health/complexity,
  structure, dead) return identical counts.
- Updated: `quality::complexity::tests::test_complexity_skips_dunder_methods`
  now asserts the new (correct) semantics: `functions_analyzed`
  reports the canonical count of 3 (incl. `__init__`/`__repr__`),
  while `report.functions` (per-function rows) still skips dunders
  for hotspot analysis. The original assertion was conflating the
  two — now disambiguated.
- All 4677 `tldr-core` lib tests pass.
- `vuln_migration_v1_red`: 168/168 GREEN.

### Files modified

- `crates/tldr-core/src/ast/mod.rs` (export the new module)
- `crates/tldr-core/src/ast/count.rs` (new)
- `crates/tldr-core/src/ast/extractor.rs` (route structure through
  `extract_from_tree`)
- `crates/tldr-core/src/quality/complexity.rs` (canonical
  `functions_analyzed`)
- `crates/tldr-core/tests/canonical_function_count_v1.rs` (new)
- `CHANGELOG.md` (this entry, top)

### Honest carry-forwards

- The `health` / `complexity` per-function rows still skip dunder
  methods for hotspot analysis. This is intentional and documented
  — only the headline count is canonical; the analysis subset is
  retained for usability.
- 54 pre-existing `test_embed_*` / `test_semantic_*` /
  `test_similar_*` failures in `crates/tldr-cli/tests/exhaustive_matrix.rs`
  are unrelated (build does not include the optional `embedding` /
  `semantic` subcommands; the matrix invokes `tldr semantic …`
  which the CLI rejects with "unrecognized subcommand"). My change
  touches no semantic / embedding code paths.

## vuln-fastpath-substring-prefilter-v1 — internal milestone

NOT a published release. Closes deferred BUG-26 (perf): `tldr vuln`
constructed a CFG + DFG + taint engine for EVERY function in every
file, regardless of whether the function body contained any
source/sink call-name at all. Most functions in typical code reference
none — the work was wasted.

### Bug fixed

- **HIGH-PERF — `tldr vuln` ran full CFG/DFG/taint construction on
  every function unconditionally.** Each function in a scanned file
  triggered `extract_cfg_from_tree` + `extract_dfg_from_tree_with_cfg`
  + `compute_taint_with_tree` even when the body contained zero
  source-call-names AND zero sink-call-names — guaranteeing zero
  `TaintFlow` results. On a small Python file (flask `cli.py`, 1.1k
  LOC) this consumed ~4.7s; on a Rust crate workspace (ripgrep
  `crates/`, 88 files) ~163s; on `lua-lsp/script` it timed out at
  >120s. Fix: added a per-function substring prefilter in
  `scan_file_vulns` (`crates/tldr-core/src/security/vuln.rs`):

  1. Per language, lazily build (via `OnceLock`) a deduplicated
     **needle set** of source-or-sink substrings derived from the
     existing `*_AST_SOURCES` / `*_AST_SINKS` static tables in
     `crates/tldr-core/src/security/taint.rs`. Construction rules:
     - `call_names: [N]` → needle `N` (e.g. `eval`, `exec`, `raw`).
     - `member_patterns: [(R, F)]` with `R` non-empty and `R != "*"`
       → needle `R.F` (e.g. `request.args`, `os.system`,
       `subprocess.run`).
     - `member_patterns: [(R, F)]` with `R == "*"` → needle `.F`
       (the leading `.` keeps the needle length ≥ 2 even for short
       fields like `("*", "get")` and prevents identifier-substring
       FPs such as `getter` matching `.get`).
     - `member_patterns: [(R, F)]` with `R == ""` → needle `F`
       directly (this is the raw-fallback shape used for Rust /
       Elixir / OCaml scoped paths like `("", "std::env::var")` →
       `std::env::var`, `("", "Code.eval_string")` →
       `Code.eval_string`; the path appears as-is in source text
       with no leading dot).
  2. Before invoking CFG/DFG/taint construction for each function,
     slice the body text from `fn_infos[i].line_number` to
     `fn_infos[i+1].line_number - 1` (or EOF) and run a simple
     `body.contains(needle)` `.any(...)` loop over the language's
     needle set.
  3. If neither any source-name nor any sink-name appears anywhere in
     the body's source text, emit empty findings for that function
     and skip the expensive analysis. Otherwise fall through to the
     existing path unchanged.

  **Correctness contract.** A `TaintFlow` requires BOTH a source AND
  a sink in the same function body. If neither call-name appears in
  the body at all, no flow is possible — the skip is a true negative.
  The substring check is a SUPERSET of the AST detector: hits inside
  string literals, comments, or unrelated identifiers admit the
  function into the full pipeline (the canonical AST `is_in_string`
  / `is_in_comment` / sanitizer dispatch resolves those FPs at the
  detector layer, yielding 0 findings — same as before). The body
  range is an over-approximation (it includes any trailing top-level
  code between functions) which is correctness-preserving for the
  prefilter — over-approximating only causes the prefilter to run
  the full analysis MORE often, never to skip it incorrectly.

  No length filter is applied to `call_names`: dropping short bare-
  call names like `raw` (Phoenix HTML helper, Ruby ERB helper) would
  risk false-negative skips when a function uses ONLY the bare-call
  form. The safe default is to include all call_names; the cost is
  just less skipping when such names happen to appear in
  non-vulnerable code.

  Profiled: simple `Vec<&str>` + `.iter().any(|n| body.contains(n))`
  is fast enough — Aho-Corasick was considered (see plan) but the
  CFG/DFG/taint avoidance dominates the savings; the linear scan
  cost is dwarfed by what we no longer pay for skipped functions.

### Performance numbers (release build, M-series, single warm run)

| Target | Before (wall) | After (wall) | Speedup |
|---|---|---|---|
| `flask/src/flask/cli.py` (1.1k LOC, 1 file, Python) | 0.55s | 0.25s | ~2.2× |
| `ripgrep/crates` (88 files, Rust) | 163.83s | 4.05s | ~40× |
| `lua-lsp/script` (Lua) | timeout (>120s) | 13.55s | ≥9× |

User-time deltas are even larger because the prefilter cuts the
inner-loop par_iter work radically: ripgrep/crates dropped from
1389s user → 17s user (82×). Same finding counts and same summary
in all three corpora (verified by `jq '.summary'` diff;
non-determinism in array ordering is a pre-existing
rayon-par_iter property, not a regression).

### Files modified

- `crates/tldr-core/src/security/taint.rs` — added
  `pub fn fastpath_pattern_strings(language) -> &'static [&'static str]`
  (per-language `OnceLock`-cached needle set), and
  `pub fn function_body_has_taint_pattern(body_text, language) -> bool`
  (the prefilter predicate). Backed by a private
  `build_fastpath_needles` helper that walks `get_ast_patterns(lang)`
  and dedupes via `HashSet<&'static str>` with `Box::leak` for
  composed needles.
- `crates/tldr-core/src/security/vuln.rs` — `scan_file_vulns`
  pre-computes per-function `(start_line, end_line)` body ranges and
  a flat `line_offsets: Vec<usize>` table once before the rayon
  par_iter; each map closure body slices its function's text via
  byte-offset lookup (O(1) per slice) and runs
  `function_body_has_taint_pattern` as the first step. On miss, it
  returns `Vec::new()` immediately — bypassing CFG, DFG, taint, and
  the post-analysis dedupe phase.
- `CHANGELOG.md` — this entry.

### Tests added

- `test_fastpath_skip_function_with_no_taint_patterns` — pure
  arithmetic body produces 0 findings AND the prefilter predicate
  returns `false` (proves the skip path actually fires).
- `test_fastpath_no_skip_function_with_source_or_sink` — three
  shapes: source-only body (`request.args.get`), sink-only body
  (`cursor.execute`), and source+sink body. The first two prove the
  prefilter ADMITS into the full pipeline (predicate returns
  `true`); the third proves end-to-end that
  `assert_detects_vuln(SqlInjection)` still finds the flow with
  the prefilter active.
- `test_fastpath_runs_full_analysis_on_string_literal_match` — body
  in which `request.args` appears ONLY inside a string literal:
  prefilter must admit (substring match is a superset), and the
  end-to-end `scan_file_vulns` must return 0 findings (canonical
  AST `is_in_string` suppresses the FP at the detector layer, NOT
  via prefilter skip).
- `test_fastpath_needle_set_python_canonical` — sanity check that
  `.execute`, `.read`, `eval`, `exec`, `request.args`, `os.system`,
  `os.environ` are all present in the Python needle set.
- `test_fastpath_needle_set_nonempty_all_langs` — every supported
  language (18 entries) must have a non-empty needle set; an empty
  set would skip every function and produce silent
  false-negatives.

### Validation

- `cargo test -p tldr-cli --test vuln_migration_v1_red`:
  168 passed / 0 failed (the PRIMARY correctness guarantee — every
  positive RED test, every `*_string_literal_fp` regression-guard
  across all 17 surfaces, all GREEN).
- `cargo test -p tldr-cli --test vuln_migration_v1_composite_red`:
  1 passed / 0 failed.
- `cargo test -p tldr-core --lib security::`: 130 passed / 0 failed
  / 1 ignored (vuln + taint + sanitizer + secrets unit suites).

### Carry-forward / deferred

- The prefilter caches needle sets per-language via `OnceLock` —
  one-time initialization, no global mutex contention. Adding a new
  language source/sink pattern to `*_AST_SOURCES` / `*_AST_SINKS`
  automatically extends the corresponding needle set; no per-language
  prefilter wiring is needed beyond the existing `get_ast_patterns`
  match arm.
- `function_body_has_taint_pattern` uses a simple O(N · M) loop
  over needles. M ≤ ~80 per language; for typical bodies the inner
  loop short-circuits on the first hit. If a future profile shows
  the prefilter itself dominating on extreme corpora (e.g.
  millions of tiny functions), upgrading to Aho-Corasick is a
  drop-in replacement at the same call site — the public API
  (`fastpath_pattern_strings` returning `&'static [&'static str]`,
  `function_body_has_taint_pattern` taking `&str`) is stable.

## luau-utf8-tolerance-v1 — internal milestone

NOT a published release. Fixes a MED-severity walker bug where a single
file with non-UTF-8 bytes inside a scanned directory aborted the entire
scan with `stream did not contain valid UTF-8`.

### Bug fixed

- **MEDIUM — `tldr surface --lang luau /tmp/repos/luau-luau` (and the
  Lua surface scanner) aborted on the first non-UTF-8 file.** The
  upstream Luau parser-test corpus intentionally embeds raw `0xFF`
  bytes in `tests/conformance/literals.luau`, `pm.luau`, and
  `sort.luau` to exercise the lexer; `std::fs::read_to_string` rejects
  these and the surface extractor's `?` propagated the parse error
  out, killing the whole-repo scan. Repro on luau-luau before the
  fix: `tldr surface /tmp/repos/luau-luau --lang luau 2>&1 | tail -1`
  emitted `Error: Parse error in
  /tmp/repos/luau-luau/tests/conformance/literals.luau: Cannot read:
  stream did not contain valid UTF-8`. Fix: introduced
  `tldr_core::fs::read_to_string_tolerant`, which classifies a
  non-UTF-8 file as a skippable condition (`ReadOutcome::NonUtf8 {
  byte_offset }`) rather than a hard error, and wired the Lua + Luau
  per-file extractors to skip those files, accumulating an entry in
  the new `ApiSurface.warnings: Vec<String>` field plus an increment
  on `ApiSurface.files_skipped: usize`. Genuine I/O failures still
  propagate. After the fix, the same scan exits 0, surfaces the
  valid `.lua`/`.luau` files, and reports the three skipped files in
  `warnings`:
  `["Skipped …/literals.luau: invalid UTF-8 at byte 2112", …]`. We
  deliberately did not use `from_utf8_lossy` because U+FFFD
  replacement bytes confuse the tree-sitter grammar and yield
  garbage symbols. The new fields are `#[serde(default)]` so older
  consumers that round-trip the JSON keep working. Test coverage:
  `surface::luau::tests::test_walker_skips_non_utf8_files`,
  `surface::luau::tests::test_walker_continues_when_all_files_are_non_utf8`,
  and three unit tests on the helper
  (`fs::tests::read_to_string_tolerant_*`).

### Carry-forward / deferred

- The same per-file UTF-8 tolerance pattern is wired through Lua and
  Luau — the only languages where the bug actually manifested in the
  field. The remaining 16 surface modules now expose
  `files_skipped`/`warnings` fields (defaulted to `0`/`[]` for serde
  back-compat) but still use the original strict `read_to_string`.
  They have not been observed to fail this way; if a future scan
  turns up non-UTF-8 source in those languages, the helper is one
  drop-in away.

## schema-completeness-v1 — internal milestone

NOT a published release. Closes the "schema completeness" anti-product
surface — three independent bugs where tldr commands either lied about
their output schema or used inconsistent exit-code semantics. Fixed
together because each fix is small and the failure mode is the same:
"the JSON schema/exit code does not match the documented contract".

### Bugs fixed

- **MEDIUM — `tldr debt` reported `summary.by_severity = null`.**
  The `DebtSummary` struct had populated `by_category` and `by_rule`
  fields but no `by_severity` field at all. Repro:
  `tldr debt /tmp/repos/flask | jq '.summary.by_severity'` returned
  `null`. Fix: added `by_severity: BTreeMap<String, u32>` to
  `DebtSummary`, populated it by classifying each issue's
  `debt_minutes` into one of `low` (`<15`), `medium` (`15..30`),
  `high` (`30..60`), or `critical` (`>=60`) — buckets aligned 1:1
  with `DebtRule::minutes()` so every rule lands deterministically in
  exactly one bucket. Each bucket value is a finding count (sum
  equals `findings.length`). After fix on flask:
  `{"high": 12, "low": 540, "medium": 20}`.

- **MEDIUM — `tldr temporal` exited 2 when no constraints were
  mined.** The legacy "exit 2 = no constraints found" contract was
  inconsistent with every other tldr command (which use `0` for any
  successful analysis, including empty results). It also broke shell
  pipelines that treat non-zero as failure, and made cross-language
  sweeps spuriously red on small fixtures. Fix: `temporal` now exits
  `0` whenever it produces valid JSON output, regardless of whether
  any constraints/trigrams were mined. Non-zero exits are reserved
  for parse failures and IO errors. Verified across 5 languages
  (Python/JS/Rust/Swift/Kotlin) — all exit 0.

- **MEDIUM — `tldr verify` listed `bounds`, `dead_stores`, and
  `invariants` as `Skipped — not yet integrated` sub-results.** The
  command was effectively lying about running these analyses. Per
  the milestone (option b: drop the unwired sub_results), these keys
  are no longer emitted at all — the verify report now only
  aggregates `contracts` and `specs`. The `sweep_bounds` and
  `sweep_dead_stores` helpers are retained (under `#[allow(dead_code)]`)
  so wiring them up in a future `verify-full-integration-v1`
  milestone is a one-line change. Bounds and invariants integration
  is **deferred** to that milestone.

### Tests added

- `quality::debt::summary_tests::test_debt_summary_by_severity_populated`
  — fixture with mixed-severity findings, asserts `by_severity` is
  populated and bucket counts sum to `findings.len()`.
- `quality::debt::summary_tests::test_severity_for_minutes_buckets`
  — boundary tests for the severity classifier (every
  `DebtRule::minutes()` value lands in exactly one documented bucket).
- `temporal_command::test_temporal_no_sequences_exit_zero` (renamed
  from `test_temporal_no_sequences_exit_2`) — exit 0 + valid JSON
  schema (`.constraints`, `.trigrams`, `.metadata`) on empty result.
- `commands::contracts::verify::tests::test_verify_no_skipped_subresults`
  — runs verify in both quick and non-quick modes; asserts no
  sub_result has status `Skipped`.
- `commands::contracts::verify::tests::test_verify_drops_unwired_keys`
  — hard regression guard: `bounds`/`dead_stores`/`invariants`
  must not appear in the report; `contracts`/`specs` must.

### Files changed

- `crates/tldr-core/src/quality/debt.rs` — `DebtSummary.by_severity`
  field, `severity_for_minutes()` helper, populate in
  `analyze_debt()`.
- `crates/tldr-core/src/quality/debt_tests.rs` — 5 existing
  construction sites updated, 2 new tests.
- `crates/tldr-cli/src/commands/patterns/temporal.rs` — removed
  `process::exit(2)` on empty result; emit JSON and return `Ok(())`.
- `crates/tldr-cli/src/commands/contracts/verify.rs` — dropped
  `bounds`/`dead_stores`/`invariants` insertions; renamed `quick` to
  `_quick`; `#[allow(dead_code)]` on retained sweep helpers.
- `crates/tldr-cli/tests/patterns_test.rs`,
  `crates/tldr-cli/tests/cli_patterns_contracts_tests.rs`,
  `crates/tldr-cli/tests/exhaustive_matrix.rs` — updated exit-code
  assertions and comments to match new contract.

### Carry-forwards

- `verify-full-integration-v1` — wire `bounds`, `dead_stores`, and
  `invariants` into the verify report for real (the helpers are kept
  in place for this).
- `quick` flag is now a no-op since the only sub-analyses it gated
  (`bounds`, `invariants`) are gone. Will regain meaning once the
  full integration milestone lands.

## js-extract-function-expressions-v1 — internal milestone

NOT a published release. Fixes a HIGH-severity gap in the JS/TS
function extractor: function-expression assignments — a major coding
pattern in many JS codebases (express, koa, jQuery, …) — were silently
dropped by `tldr extract` and every downstream command that looks up
functions by name (`complexity`, `explain`, `taint`, `slice`).

### Bug fixed

- **HIGH — `tldr extract` missed JS/TS function-expression
  assignments.** On `/tmp/repos/express/lib/application.js`,
  `tldr extract … | jq '.functions | length'` returned **2** (only the
  two top-level `function`-declaration helpers) even though the file
  defines 17 public methods on the `app` object via
  `app.use = function use() {}`, `app.engine = function() {}`,
  `Foo.prototype.bar = function() {}`, etc. Cascade impact: every
  call-site lookup (`tldr complexity <file> use`,
  `tldr explain <file> use`, `tldr taint`, `tldr slice`) failed with
  `Function not found` for any function declared via the
  function-expression-assignment pattern.

### Patterns now recognized

- `name = function() {}` / `name = () => {}` (simple identifier LHS)
- `obj.method = function() {}` (member-expression LHS — uses trailing
  property as the function name)
- `Foo.prototype.bar = function() {}` (prototype assignment — uses the
  trailing property)
- `{ foo: function() {} }` and `{ foo: () => {} }` (object literal
  pair with function-like value)
- `{ foo() {} }` (object literal method-shorthand — emitted as a
  top-level function so name lookup works)

The same patterns are recognized in TypeScript via the shared
JS/TS code paths.

### Files changed

- `crates/tldr-core/src/ast/extract.rs` — extend
  `extract_ts_functions_detailed` with `assignment_expression` and
  `pair` arms; allow `method_definition` outside class bodies (object
  shorthand). Adds two helpers
  (`extract_ts_assignment_function`, `extract_ts_pair_function`).
- `crates/tldr-core/src/ast/function_finder.rs` — extend
  `find_function_node` for JS/TS so cascade commands (`complexity`,
  `slice`, `taint`, …) can locate `app.use = function() {}` style
  functions by name.
- `crates/tldr-cli/src/commands/remaining/explain.rs` — extend the
  explain-local `find_function_recursive` with the same
  `assignment_expression` / `pair` patterns.

### Validation

- `tldr extract /tmp/repos/express/lib/application.js | jq
  '.functions | length'` → **19** (was 2). The 19 names include
  `use`, `engine`, `param`, `set`, `init`, `enable`, `disable`,
  `defaultConfiguration`, `render`, `listen`, `route`, `get`, `all`,
  `path`, `handle`, `enabled`, `disabled`, `logerror`, `tryRender`.
- `tldr complexity /tmp/repos/express/lib/application.js use` →
  succeeds (cyclomatic=12, cognitive=10). Was: `Function not found`.
- `tldr explain /tmp/repos/express/lib/application.js use` →
  succeeds. Was: `symbol 'use' not found`.
- `tldr taint /tmp/repos/express/lib/application.js use` and
  `tldr slice /tmp/repos/express/lib/application.js engine 100` →
  both succeed.
- 5 new unit tests in `extract.rs` covering: function-expression
  assignment, arrow-function assignment, prototype methods, object
  method shorthand, and TypeScript variants of all of the above.
- `vuln_migration_v1_red`: 168/168 GREEN.
- `tldr-core` lib tests: 4662/4662 GREEN. `tldr-cli` lib tests:
  1392/1392 GREEN.

### Carry-forwards (intentionally not covered in v1)

- **Dynamic property names**: `app[fnName] = function() {}` —
  cannot be statically resolved without symbol propagation; skipped.
- **Computed property keys** in object literals (`{ [k]: () => {} }`)
  — same reason; skipped.
- **Class fields with arrow values** (`class C { foo = () => {} }`) —
  not in scope for this milestone; tracked separately.

## autodetect-correctness-v1 — internal milestone

NOT a published release. Closes the "language autodetect anti-product
surface" by fixing two HIGH-severity correctness bugs in the directory-
level language detector.

### Bugs fixed

- **HIGH — `tldr structure` mis-detected Swift projects as C** when a
  shared build-system manifest (CMakeLists.txt / meson.build /
  configure.ac / Makefile.am) was present alongside dominant `.swift`
  sources.
  Repro: `tldr structure /tmp/repos/swift-collections/Sources` returned
  `language: c, files: 0` even though `Sources/` contains 689 `.swift`
  files. The Swift-Collections repo (and many other Apple projects)
  ships a top-level `CMakeLists.txt` for embedded-build targets next
  to its `Package.swift`. The manifest-priority detector blindly
  forced the C/C++ tie-break and returned C with zero files.

- **HIGH — `tldr deps` failed autodetect for java / scala** when
  source files lived more than one directory deep.
  Repro: `tldr deps /tmp/repos/spring-petclinic/src` and
  `tldr deps /tmp/repos/scala-cats-effect/core` both exited with
  `Error: Unsupported language: unknown`. Passing `--lang java` /
  `--lang scala` worked, but every other subcommand (`structure`,
  `calls`, `extract`) autodetected these projects correctly.

### Root cause

1. **Shared build-system manifest tie-break.** `c_vs_cpp_tie_break`
   in `crates/tldr-core/src/types.rs` only counted `.c`/`.cpp`-family
   extensions. When CMake/Meson/Autotools/Makefile.am were the
   manifest winners but the project was actually Swift or Rust, the
   tie-break still returned C (the default on empty counts) — a
   silent mis-detection with zero downstream files.

2. **Shallow deps autodetect.** `detect_dominant_language` in
   `crates/tldr-core/src/analysis/deps.rs` walked only the root and
   its immediate child directories (depth ≤ 1). Java sources under
   `src/main/java/com/example/...` and Scala sources under
   `core/.../src/main/scala/...` are 4–7 levels deep, so the counter
   saw zero recognised files and returned `UnsupportedLanguage`.

### Fix

- `c_vs_cpp_tie_break` now also counts `.swift` and `.rs` files
  during the project walk. If a non-C/C++ language family strictly
  exceeds the combined C+C++ count, the function returns that
  language instead of falling back to C. This fixes Bug 1 without
  perturbing legitimate C/C++ projects (where `.c` / `.cpp` counts
  always dominate). The classic C-vs-C++ tie-break logic is preserved
  on the C/C++ path.

- `analyze_dependencies` now delegates language detection to
  `Language::from_directory` — the canonical detector used by every
  other subcommand. This unifies autodetect behaviour across the CLI
  and gives `deps` access to the same manifest-priority +
  recursive-extension-majority logic, fixing Bug 2 for java, scala,
  and any future language whose typical source layout is deeper than
  one directory.

### Files modified

- `crates/tldr-core/src/types.rs` — extend `c_vs_cpp_tie_break` with
  Swift and Rust extension-majority overrides.
- `crates/tldr-core/src/analysis/deps.rs` — replace shallow
  `detect_dominant_language` with delegation to
  `Language::from_directory`.
- `crates/tldr-cli/tests/language_autodetect_tests.rs` — add
  `test_swift_autodetect_with_cmakelists_at_root` and
  `test_deps_autodetect_java_scala`.

### Validation

- `language_autodetect_tests`: 20/20 pass (18 pre-existing + 2 new).
- `tldr-core` `types::tests`: 298/298 pass — all manifest-priority
  unit tests stay green (Cargo.toml, tsconfig.json, pyproject.toml,
  go.mod, pom.xml, etc.).
- `tldr-core` `analysis::deps`: 79/79 pass (20 ignored as before).
- `vuln_migration_v1_red`: 168/168 GREEN — no regression.
- Binary verify (post-fix):
  - `tldr structure /tmp/repos/swift-collections/Sources` →
    `language=swift`, `files_count=543` (was `c`, `0`).
  - `tldr deps /tmp/repos/spring-petclinic/src` → exits 0, JSON
    `language=java` (was `Error: Unsupported language: unknown`).
  - `tldr deps /tmp/repos/scala-cats-effect/core` → exits 0, JSON
    `language=scala` (was `Error: Unsupported language: unknown`).
  - Regression check on synthetic fixtures: python, rust, typescript,
    javascript still autodetect correctly.

### Out of scope

- No version bump. No publish. Bug-fix-only milestone.

## references-clap-conflict-v1 — internal milestone

NOT a published release. Fixes a CRITICAL unhandled Rust panic in the
`tldr references` subcommand whenever `--lang` (or `-l`) was supplied.

### Bug fixed

- **CRITICAL — `tldr references SYMBOL PATH --lang LANG` panicked at
  exit code 101 with:**
  ```
  thread 'main' panicked at clap_builder/src/parser/error.rs:32:9:
  Mismatch between definition and access of `lang`. Could not downcast
  to TypeId(...), need to downcast to TypeId(...).
  ```
  Reproduced on every one of the 17 supported languages. The command
  worked without `-l/--lang`, but any user who tried to pin the language
  hit the panic. Also reproduced when the global flag came before the
  subcommand (`tldr -l rust references ...`) since the global flag is
  declared with `global = true`.

### Root cause

`crates/tldr-cli/src/main.rs` declares the global `--lang/-l` argument
as `Option<Language>` (typed enum). `crates/tldr-cli/src/commands/references.rs`
re-declared its own local `--lang/-l` field as `Option<String>`. clap
4.5 detects the type mismatch at runtime when the same argument id is
accessed with two different `TypeId`s and panics with a downcast error.

This was the **only** subcommand with a type mismatch — every other
subcommand that exposes a local `--lang/-l` (calls, dead, structure,
smells, loc, search, deps, diagnostics, hubs, extract, inheritance,
halstead, imports, impact, importers, reaching_defs, slice, taint,
whatbreaks, change_impact, complexity, available, context, cognitive,
detect_patterns) declares it as `Option<Language>`, matching the global.

### Fix

`crates/tldr-cli/src/commands/references.rs`:
- Removed the local `lang: Option<String>` field from `ReferencesArgs`.
- Updated `ReferencesArgs::run` to accept `cli_lang: Option<Language>`
  (passed from the global flag).
- Convert the `Language` enum to the canonical lowercase string via
  `Language::as_str()` for `ReferencesOptions::language` (which
  remains `Option<String>` in `tldr-core`).

`crates/tldr-cli/src/main.rs`:
- Updated the `Command::References` dispatch to forward `cli.lang`
  through to `args.run`.

### Tests added

`crates/tldr-cli/tests/cli_remaining_tests.rs`:
- `test_references_with_lang_no_panic` — `tldr references helper PATH
  --lang python -q` exits non-101 and stderr contains no clap downcast
  text.
- `test_references_with_short_lang_flag_no_panic` — same with `-l python`.
- `test_no_other_subcommand_panics_on_lang` — sanity matrix that
  `calls`, `dead`, `structure`, `smells`, `loc`, `search` with `-l python`
  all exit non-101.

### Validation

- All 17 languages × `tldr references SomeName /tmp/repos/<repo> --lang $LANG`:
  exit 0 for every language (was panic 101 on every language pre-fix).
- `cargo test -p tldr-cli --lib`: 1392/1392 pass.
- `cargo test -p tldr-cli --test cli_remaining_tests`: 80/80 pass
  (was 77 before; +3 regression tests added).
- `cargo test -p tldr-cli --test vuln_migration_v1_red`: 168/168 stays GREEN.

### Carry-forwards

None. This was a localized type-mismatch bug specific to `references`.
The fix verified that no other subcommand has the same issue (audit
in the "Root cause" section). The new
`test_no_other_subcommand_panics_on_lang` test guards against future
regressions if a contributor re-introduces a per-subcommand `lang` with
a non-matching type.

## structure-method-infos-all-langs-v1 — internal milestone

NOT a published release. Closes the medium-severity follow-up gap left
by `schema-unification-v1` (commit `8d71463`): the BUG-21 fix added
`FileStructure::method_infos: Vec<MethodInfo>` to distinguish overloaded
methods, but the field was serialized with
`#[serde(skip_serializing_if = "Vec::is_empty")]`. Languages whose file
fixture had no class scope (so `definitions` filtered to `kind="method"`
yielded zero entries) silently dropped the key from JSON output.
Surfaced by the v0.2.x 17-language sweep — only 3 of 17 languages
actually emitted the field on the canonical `vuln_migration_v1`
fixtures.

### Bug fixed

- **BUG-21 (incomplete) — `tldr structure` JSON drops `method_infos`
  for 14 of 17 languages.** Repro on HEAD before this milestone:
  ```
  for lang in c cpp csharp elixir go java javascript kotlin lua luau \
              ocaml php python ruby rust scala swift typescript; do
    D=crates/tldr-cli/tests/fixtures/vuln_migration_v1/$lang
    has_mi=$(tldr structure --lang $lang "$D" \
      | jq '.files[0] | has("method_infos")')
    printf "  %-12s method_infos=%s\n" "$lang" "$has_mi"
  done
  ```
  Output:
  - HAD field: `csharp`, `java`, `ruby` (3) — fixtures had class scope
  - MISSED field: `c`, `cpp`, `elixir`, `go`, `javascript`, `kotlin`,
    `lua`, `luau`, `ocaml`, `php`, `python`, `rust`, `scala`, `swift`,
    `typescript` (14) — fixtures had no class scope, so the empty
    `method_infos: []` was suppressed at serialization time.

  Languages with method overloading (`cpp`, `kotlin`, `scala`)
  particularly suffered downstream — overloaded methods always collapse
  to identical strings in the legacy `methods: [String]` array, leaving
  consumers no way to disambiguate them when feeding the structure
  output back to a planner / refactor / coverage tool.

  Root cause in `crates/tldr-core/src/types.rs::FileStructure`:
  ```rust
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub method_infos: Vec<MethodInfo>,
  ```
  The population path in `crates/tldr-core/src/ast/extractor.rs::
  extract_file_structure` was already language-agnostic — it derives
  `method_infos` from `definitions` filtered by `kind == "method"`,
  which works for every grammar that classifies class-scope functions
  via the existing `is_inside_class_or_impl` helper. The bug was
  purely in serialization: an empty vector was correct for languages
  without class-scope methods, but suppressing the empty array meant
  consumer code that does `obj.method_infos` (without `has(...)`
  guards) would error on 14 of 17 languages.

  Fix: drop `skip_serializing_if = "Vec::is_empty"` on
  `FileStructure::method_infos` so the field is ALWAYS emitted as `[]`
  when the file contains no class-scope methods. The population logic
  (already present, already correct) is unchanged. Overload
  distinction (BUG-21 original contract) keeps working — verified on
  C++ / Kotlin / Scala overload fixtures: three same-name methods
  produce three distinct `method_infos` entries with different `line`
  AND different `signature` values, while the legacy `methods:
  [String]` array retains all three duplicate name entries (additive,
  no breakage).

  BEFORE / AFTER (binary verify across the 17-language fixture sweep):
  ```
  Language     BEFORE method_infos  AFTER method_infos  ENTRIES
  ---------    -------------------  ------------------  -------
  c            absent               present (=[])       0
  cpp          absent               present (=[])       0  *
  csharp       present              present             1
  elixir       absent               present (=[])       0
  go           absent               present (=[])       0
  java         present              present             1
  javascript   absent               present (=[])       0
  kotlin       absent               present (=[])       0  *
  lua          absent               present (=[])       0
  luau         absent               present (=[])       0
  ocaml        absent               present (=[])       0
  php          absent               present (=[])       0
  python       absent               present (=[])       0
  ruby         present              present             1
  rust         absent               present (=[])       0
  scala        absent               present (=[])       0  *
  swift        absent               present (=[])       0
  typescript   absent               present (=[])       0
  ```
  (* The fixture corpus does not include class-scope code for these
  languages — empty `[]` is the correct output. Overload distinction
  is verified separately in
  `test_structure_method_infos_distinguishes_overloads_cpp_kotlin_scala`
  using inline source: 3 overloaded `bar` methods → 3 distinct
  `method_infos` entries with distinct lines AND distinct signatures.)

  Kotlin overload BEFORE / AFTER on inline source:
  ```
  class Foo {
    fun bar(x: Int) {}
    fun bar(x: Int, y: Int) {}
    fun bar(x: Double) {}
  }

  BEFORE: files[0] | has("method_infos") = false  ← BUG (field absent)
  AFTER:  files[0].method_infos = [
            { name: "bar", signature: "fun bar(x: Int) {}",       line: 2 },
            { name: "bar", signature: "fun bar(x: Int, y: Int) {}", line: 3 },
            { name: "bar", signature: "fun bar(x: Double) {}",     line: 4 },
          ]
  ```

### Tests

- New `crates/tldr-cli/tests/structure_method_infos_all_langs_v1.rs`
  with 4 integration tests (covering the 17-language matrix in two
  passes — inline-source per-language fixtures + project-fixture
  sweep — plus C++ / Kotlin / Scala overload distinction and a Java
  regression guard pinning the prior schema-unification-v1 BUG-21 test
  invariants).
- `vuln_migration_v1_red`: 168/168 GREEN.
- `tldr-core` lib tests: 4657/4657 GREEN.
- `tldr-cli` lib tests: 1392/1392 GREEN.
- `schema_unification_v1`: 6/6 GREEN (no regression on the original
  Java overload test).

### Files modified

```
CHANGELOG.md                                                       (this entry, prepended)
crates/tldr-core/src/types.rs                                      (drop skip_serializing_if on method_infos)
crates/tldr-cli/tests/structure_method_infos_all_langs_v1.rs       (new test file, 4 tests)
```

### Carry-forwards

- The population logic in `extract_file_structure` derives
  `method_infos` from `definitions` filtered by `kind == "method"`,
  which depends on `is_inside_class_or_impl` correctly identifying
  class-scope nodes. The current helper covers Python / TS / JS /
  Rust / Java / C# / C++ / Ruby / Kotlin (companion_object,
  object_declaration, class_body) and treats `module` as
  class-scope for non-Python grammars. Languages that lack class
  semantics (C, OCaml top-level, Lua, Go, Elixir defmodule) emit
  `method_infos: []` — correct under the spec contract. If a future
  fixture introduces (say) a Lua `:` method-call shorthand or a Go
  receiver method that should be classified as `method`, the helper
  may need targeted extension; that is independent of this milestone.

- Consumers that special-cased the historical `has("method_infos")`
  guard can now drop the guard. The field is unconditionally an
  array. Old consumers continue to work (a present empty array
  serializes the same way the absent field would deserialize via
  `#[serde(default)]`).

## rust-secure-taint-aggregator-v2 — internal milestone

NOT a published release. Closes the high-severity Rust regression
where `tldr secure --lang rust <file>` returned `summary.taint_count: 0`
on files that `tldr vuln --lang rust <file>` reported N>0 findings on.
Surfaced by the v0.2.x 17-language sweep — Rust was the only language
failing `secure.taint_count == vuln.findings.length` parity (16/17
passed). Closes follow-up gap left by `secure-taint-aggregator-v1`,
which routed the canonical pipeline ONLY for non-Rust files.

### Bug fixed

- **BUG-17 (rust-secure regression)** — `tldr secure` on a Rust file
  with a real CommandInjection / PathTraversal / Deserialization /
  SQLInjection / SSRF taint flow reported `taint_count: 0` while
  `tldr vuln` on the SAME path reported N>0 findings. Repro on a
  fixture that the canonical Rust pipeline already detects:
  ```
  F=crates/tldr-cli/tests/fixtures/vuln_migration_v1/rust/command_injection_positive.rs
  tldr vuln   --lang rust "$F" | jq '.findings | length'      → 2
  tldr secure --lang rust "$F" | jq '.summary.taint_count'    → 0   ← BUG
  ```
  Root cause in `crates/tldr-cli/src/commands/remaining/secure.rs`:
  `analyze_taint` short-circuited on `.rs` files to ONLY the
  unsafe-block line scanner (which produces `category="unsafe_block"`
  findings counted under `summary.unsafe_blocks`, NOT under
  `summary.taint_count`). The canonical
  `tldr_core::security::vuln::scan_vulnerabilities` pipeline — the
  same one `tldr vuln` uses — was never invoked for Rust paths. The
  prior `secure-taint-aggregator-v1` milestone had wired this routing
  for Python / JS / TS / 14 other languages but explicitly excluded
  `.rs` ("For Rust files, taint is deliberately interpreted as
  'unsafe blocks'"), missing that `tldr vuln` had since adopted dual
  dispatch for `.rs` (canonical + line scanner with overlap dedup,
  per `rust-vuln-taint-pipeline-v1`).

  Fix: secure now mirrors `tldr vuln`'s Rust dual dispatch.
  `analyze_taint` for `.rs` files emits (a) canonical taint findings
  with `category="taint"`, (b) line-scanner SqlInjection /
  CommandInjection findings (deduped against canonical on
  `(line, vuln_type)` — same predicate as `vuln.rs::dedupe_overlap`)
  also with `category="taint"`, and (c) unsafe-block line-scanner
  findings unchanged with `category="unsafe_block"`. The line
  scanner's UnsafeCode / MemorySafety / Panic emissions are
  intentionally NOT included in the taint stream — they are
  smell-class and surfaced by `analyze_rust_unsafe_blocks` /
  `analyze_rust_raw_pointers` / `analyze_rust_bounds` under their own
  categories (`unsafe_block`, `raw_pointer`, `unwrap`,
  `todo_marker`).

  `crates/tldr-cli/src/commands/remaining/vuln.rs::analyze_rust_file`
  visibility lifted from private to `pub(super)` so secure can call
  it directly — single source of truth for the line-scanner logic.
  No duplication.

  BEFORE / AFTER (binary verify):
  ```
  Rust file (command_injection_positive.rs):
    BEFORE: vuln=2  secure.taint_count=0   ← MISMATCH
    AFTER:  vuln=2  secure.taint_count=2   ← parity

  Rust dir (vuln_migration_v1/rust/, 5 files):
    BEFORE: vuln=10 secure.taint_count=0   ← MISMATCH
    AFTER:  vuln=10 secure.taint_count=10  ← parity

  Python file (regression guard, command_injection_positive.py):
    AFTER:  vuln=1  secure.taint_count=1   ← unchanged

  JS file (regression guard, command_injection_positive.js):
    AFTER:  vuln=2  secure.taint_count=2   ← unchanged
  ```

### Tests

- New `test_secure_taint_count_matches_vuln_rust` in
  `crates/tldr-cli/src/commands/remaining/secure.rs` — Rust-specific
  secure↔vuln aggregation parity guard mirroring the existing
  Python guard `test_secure_taint_count_matches_vuln_findings`.
  Asserts `secure.findings|filter(category="taint")|len ==
  vuln.findings|len` on a Rust source-to-sink command-injection
  fixture.
- `vuln_migration_v1_red`: 168/168 GREEN.
- `tldr-cli` lib tests: 1392/1392 GREEN.
- Existing `test_secure_taint_count_matches_vuln_findings`,
  `test_secure_taint_count_matches_findings_array`, and
  `test_rust_secure_metrics_detected` remain GREEN.

### Carry-forwards

- Two `remaining_test.rs` integration tests
  (`secure_command::test_secure_detects_taint`,
  `vuln_command::test_vuln_detects_xss`) were already failing on
  HEAD before this milestone — verified by running `tldr vuln` /
  `tldr secure` on the test fixtures (`PYTHON_SECURE_SAMPLE`,
  `PYTHON_VULN_XSS`). The Python secure path was unchanged by this
  milestone (only the Rust short-circuit was lifted), so these are
  pre-existing failures unrelated to the v2 fix. They surface a
  separate gap in the canonical Python pipeline's coverage of
  `pickle.loads` on function-arg sources and a Python XSS detection
  gap — out of scope for the rust-secure parity fix.

## schema-unification-v1 — internal milestone

NOT a published release. Closes the "JSON schema inconsistency
anti-product surface" by unifying naming conventions, line-field
aliases, top-level shapes, and missing-key emission across `tldr vuln`,
`tldr extract`, `tldr explain`, `tldr imports`, `tldr inheritance`,
and `tldr structure`. The five bugs ship atomically since they all
live on the JSON-output / schema-derivation path. The strongly
preferred shape is **additive** — every change either adds a new
field or stabilizes an existing one; only one bug (BUG-18) is a
true default-shape change and it carries a `--legacy-array`
backward-compatibility flag.

`vuln_migration_v1_red` remains 168/168 GREEN; all 4657 `tldr-core`
library tests + 1391 `tldr-cli` library tests remain GREEN. Three
imports tests in `cli_basic_tests.rs` and one in `cli_p1_tests.rs`
were updated to expect the canonical envelope shape (BUG-18); these
were over-fitted to the historical bare-array shape and the schema
fix is the correct change. The 54 pre-existing
`test_embed_*` / `test_semantic_*` / `test_similar_*` feature-gated
failures in `exhaustive_matrix.rs` (require `--features semantic`,
missing embedding model in env) persist unchanged — verified
present at HEAD before this milestone.

### Bugs fixed

- **BUG-02** — `tldr vuln` emitted `summary.by_type` keys in
  lowercase-no-separator form (`"commandinjection"`) while the
  per-finding `.vuln_type` field used canonical snake_case
  (`"command_injection"`). Pre-fix repro on flask:
  ```
  tldr vuln /tmp/repos/flask | jq '.findings[0].vuln_type'   → "command_injection"
  tldr vuln /tmp/repos/flask | jq '.summary.by_type'         → {"commandinjection": 3}
  ```
  Two views of the same enum disagreed on naming. Root cause in
  `crates/tldr-cli/src/commands/remaining/vuln.rs::build_summary`
  used `format!("{:?}", finding.vuln_type).to_lowercase()` which
  produced the collapsed form. Fixed by routing the key through
  `serde_json::to_value(vuln_type)` which honors the existing
  `#[serde(rename_all = "snake_case")]` on `VulnType`. `.title`
  remains PascalCase-prose ("Command Injection") because that's
  human-readable display, not a schema key. Post-fix on flask:
  ```
  tldr vuln /tmp/repos/flask | jq '.summary.by_type | keys'
    → ["command_injection","path_traversal"]
  ```
- **BUG-17** — `tldr extract`, `tldr explain` used `line_number` and
  `line_start` respectively while `tldr vuln`, `tldr dead`,
  `tldr health` used a unified `line` field. Three different names
  for the same semantic ("the line where this thing is"). **ADDITIVE**
  fix: every return type now emits `line` ALONGSIDE the historical
  field. No field renamed, no field removed. `FunctionInfo`,
  `ClassInfo`, `FieldInfo` (in `crates/tldr-core/src/types.rs`) and
  `ExplainReport` (in `crates/tldr-cli/src/commands/remaining/types.rs`)
  switched from `#[derive(Serialize)]` to a manual `Serialize` impl
  that emits both `line_number` and `line` (or `line_start` and
  `line`). `Deserialize` remains derived (existing field names
  continue to parse; the new `line` output field is ignored on
  roundtrip because serde's default unknown-field policy permits it).
  Post-fix on flask:
  ```
  tldr extract <file> | jq '.functions[0] | {line_number, line}'  → {"line_number":41,"line":41}
  tldr explain <file> <fn> | jq '{line_start, line, line_end}'    → {"line_start":1061,"line":1061,"line_end":1107}
  ```
  Consumer migration: callers may switch to the unified `line` key
  to write language-agnostic queries. The legacy keys remain valid
  indefinitely.
- **BUG-18** — `tldr imports` returned a top-level JSON array while
  every other top-level command (`structure`, `vuln`, `dead`,
  `inheritance`, `health`, …) returned an object. **DEFAULT-SHAPE
  CHANGE** with explicit backward-compat opt-in:
  ```
  tldr imports <file>                  → {"file":"…","language":"…","imports":[…]}   (NEW DEFAULT)
  tldr imports <file> --legacy-array   → [ImportInfo, …]                              (LEGACY)
  ```
  New `ImportsEnvelope { file, language, imports }` struct and
  `--legacy-array` flag in `crates/tldr-cli/src/commands/imports.rs`.
  Three over-fitted tests (`test_imports_returns_json_array`,
  `test_imports_json_format`, `test_imports_schema` in
  `cli_basic_tests.rs`, plus `test_imports_returns_array` in
  `cli_p1_tests.rs`) updated to assert the new envelope AND
  exercise `--legacy-array`. Consumer migration: pipelines using
  `jq '.[]'` should switch to `jq '.imports[]'`, OR pass
  `--legacy-array` to keep the old behavior with no other change.
- **BUG-23** — `tldr inheritance` edges with `external: true` (stdlib
  or unresolved bases) DROPPED the `parent_file` and `parent_line`
  keys instead of emitting them as `null`. Consumers had to use
  `has("parent_file")` to safely descend. Pre-fix on flask:
  ```
  tldr inheritance /tmp/repos/flask | jq '[.edges[] | has("parent_file")] | unique'
    → [false, true]
  ```
  Removed `#[serde(skip_serializing_if = "Option::is_none")]` from
  `InheritanceEdge::parent_file` and `parent_line` in
  `crates/tldr-core/src/types/inheritance.rs`. Stable schema:
  every edge now has `parent_file` and `parent_line` keys (`null`
  when external). Post-fix on flask:
  ```
  tldr inheritance /tmp/repos/flask | jq '[.edges[] | has("parent_file")] | unique'
    → [true]
  ```
- **BUG-21** — `tldr structure` emitted `methods: [String]` (a flat
  list of names) which collapsed overloaded methods. Pre-fix on
  spring-petclinic's `Owner.java` (which has three `getPet`
  overloads):
  ```
  tldr structure /tmp/repos/spring-petclinic | jq '.files[] | select(.path | endswith("Owner.java")) | .methods'
    → [..., "getPet", "getPet", "getPet", "toString", ...]   # 3 indistinguishable strings
  ```
  **ADDITIVE** fix: kept `methods: Vec<String>` and added a parallel
  `method_infos: Vec<MethodInfo>` field where each entry carries
  `(name, signature, line)`. New `MethodInfo` struct in
  `crates/tldr-core/src/types.rs`, populated in
  `crates/tldr-core/src/ast/extractor.rs::extract_file_structure`
  by filtering the existing `definitions` field for `kind=="method"`.
  Empty `method_infos` is skipped in JSON
  (`#[serde(skip_serializing_if = "Vec::is_empty")]`) so the change
  is invisible for files without methods. Post-fix:
  ```
  tldr structure /tmp/repos/spring-petclinic | jq '.files[] | select(.path | endswith("Owner.java")) | .method_infos | map(select(.name=="getPet"))'
    → [
        {"name":"getPet","signature":"public Pet getPet(String name) {","line":108},
        {"name":"getPet","signature":"public Pet getPet(Integer id) {","line":117},
        {"name":"getPet","signature":"public Pet getPet(String name, boolean ignoreNew) {","line":135}
      ]
  ```
  Consumer migration: callers needing overload distinguishability
  should consume `method_infos` (or the existing `definitions` array,
  which already carried the same info but was less discoverable).
  `methods` remains as the legacy flat-name view.

### Carry-forwards

None for this milestone — all 5 bugs implemented in this commit.

### Files modified

- `crates/tldr-core/src/types.rs` — manual `Serialize` for
  `FunctionInfo` / `ClassInfo` / `FieldInfo` (line alias);
  added `MethodInfo` struct; added `FileStructure.method_infos`.
- `crates/tldr-core/src/types/inheritance.rs` — dropped
  `skip_serializing_if` on `InheritanceEdge.parent_file` /
  `parent_line`.
- `crates/tldr-core/src/ast/extractor.rs` — populate
  `FileStructure.method_infos` from `definitions`.
- `crates/tldr-cli/src/commands/imports.rs` — `ImportsEnvelope`
  + `--legacy-array` flag.
- `crates/tldr-cli/src/commands/remaining/types.rs` — manual
  `Serialize` for `ExplainReport` (line alias).
- `crates/tldr-cli/src/commands/remaining/vuln.rs` — derive
  `summary.by_type` keys via `serde_json::to_value` (snake_case).
- `crates/tldr-cli/tests/schema_unification_v1.rs` — NEW
  integration tests (6 tests, all 5 bugs covered).
- `crates/tldr-cli/tests/cli_basic_tests.rs`,
  `crates/tldr-cli/tests/cli_p1_tests.rs` — update the four
  over-fitted imports tests for the envelope default; assert
  `--legacy-array` preserves the historical shape.
- `crates/tldr-core/tests/{definition_info_test,types_base_tests}.rs`
  — add `method_infos: vec![]` to `FileStructure { … }` literals.

## wrapper-cross-consistency-v1 — internal milestone

NOT a published release. Closes the "wrapper consistency anti-product
surface" by aligning summary↔findings invariants and inter-wrapper
threshold parity across `tldr secure`, `tldr health`, `tldr todo`. All
four bugs ship atomically since they all live on the wrapper
aggregation/serialization path. `vuln_migration_v1_red` remains 168/168
GREEN; all 4657 `tldr-core` library tests + 1391 `tldr-cli` library
tests remain GREEN. The four pre-existing failures called out in
`error-handling-and-data-v1` (`test_vuln_detects_xss`,
`test_secure_detects_taint`,
`nextjs_response_json_reflected_xss_via_compute_taint`, plus the
`test_embed_*` / `test_semantic_*` / `test_similar_*` env-dependent
tests in `exhaustive_matrix.rs`) persist unchanged — verified to be
present at HEAD before this milestone and NOT regressions.

### Bugs fixed

- **BUG-04** — `tldr health` and `tldr todo` reported divergent
  `hotspot_count` and `low_cohesion_count` on the same path because
  they ran two different cohesion/complexity analyzers with two
  different thresholds. Pre-fix repro on flask:
  ```
  tldr health /tmp/repos/flask | jq '.summary | {hotspot_count, low_cohesion_count}'
    → { "hotspot_count": 11, "low_cohesion_count": 26 }
  tldr todo  /tmp/repos/flask | jq '.summary | {hotspot_count, low_cohesion_count}'
    → { "hotspot_count": 6,  "low_cohesion_count": 20 }
  ```
  `health` aggregated `tldr_core::quality::complexity::analyze_complexity`
  (threshold 10) and `tldr_core::quality::cohesion::analyze_cohesion`
  (threshold 2). `todo` (`crates/tldr-cli/src/commands/remaining/todo.rs`)
  re-implemented complexity hotspot detection per-function via
  `tldr_core::calculate_complexity` (threshold 10 — coincident) and
  routed cohesion through `crate::commands::patterns::cohesion::run`
  (a different impl, threshold `> 1`). Three differences for the same
  metric. Now both wrappers delegate to the canonical
  `tldr_core::quality::{complexity, cohesion}` analyzers with the
  same default thresholds (10 and 2), so the counts match by
  construction. Post-fix on flask: both report `hotspot_count=11,
  low_cohesion_count=26`.
- **BUG-15** — `tldr secure` summary was missing a `behavioral_count`
  field even though `behavioral` was a category emitted into
  `findings[]` (e.g. bare `except:` clauses). Pre-fix repro on flask:
  ```
  tldr secure /tmp/repos/flask | jq '.findings | length'              → 16
  tldr secure /tmp/repos/flask | jq '[.summary | values | add]'        → 15
  tldr secure /tmp/repos/flask | jq '[.findings[].category] | group_by(.) | map({key:.[0],count:length})'
    → [{"key":"behavioral","count":1}, {"key":"resource_leak","count":11}, {"key":"taint","count":4}]
  ```
  The summary's typed counters summed to 15 while the findings array
  had 16 entries — exactly the 1 behavioral finding was unaccounted
  for. Added `behavioral_count: u32` to `SecureSummary` and the
  text-output formatter; the schema invariant
  `taint_count + leak_count + bounds_warnings + behavioral_count +
   missing_contracts + mutable_params + unsafe_blocks +
   raw_pointer_ops + unwrap_calls + todo_markers == findings.len()`
  now holds (verified post-fix on flask: 4+11+0+1+0+0+0+0+0+0 = 16).
  `taint_critical` is excluded as a severity refinement subset of
  `taint_count`.
- **BUG-19** — `tldr secure`, `tldr todo` (and previously expected of
  `tldr health` — see clarification below) emitted `sub_results: {}`
  on every default invocation, cargo-culting `tldr verify`'s schema
  even though they don't populate it without `--detail`. Pre-fix
  repro:
  ```
  tldr secure /tmp/repos/flask | jq '.sub_results'  → {}
  tldr todo   /tmp/repos/flask | jq '.sub_results'  → {}
  tldr verify /tmp/repos/flask --quick | jq '.sub_results | keys'
    → ["contracts","dead_stores","specs"]
  ```
  Now `SecureReport.sub_results` and `TodoReport.sub_results` carry
  `#[serde(skip_serializing_if = "HashMap::is_empty")]`, so the field
  is omitted from JSON unless `--detail` populated it. `tldr verify`
  is unaffected (different report type, populates the field by
  default and remains 5 keys). Clarification: `tldr health` already
  uses a renamed `details` field (not `sub_results`) and was never
  affected by this bug — verified post-fix that `tldr health` still
  emits `details` populated.
- **BUG-16** — `tldr secure` summary `taint_count` ghosted on Rust
  paths because `update_summary` set it to `findings.len()` from the
  per-analysis `analyze_taint` return value, but on Rust files
  `analyze_taint` returns `category="unsafe_block"` findings (not
  `category="taint"`). Pre-fix repro on ripgrep:
  ```
  tldr secure /tmp/repos/ripgrep | jq '.summary.taint_count'                    → 4
  tldr secure /tmp/repos/ripgrep | jq '[.findings[] | select(.category=="taint")] | length'
                                                                                 → 0
  ```
  Summary claimed 4 taint findings; findings array had zero. The
  prior `secure-taint-aggregator-v1` milestone wired `analyze_taint`
  to canonical `scan_vulnerabilities` for non-Rust paths, but the
  summary writer still consulted a separate (per-analysis) count
  enumeration. Now every `*_count` field in `SecureSummary` is
  computed in a single `compute_summary_from_findings(&findings)`
  pass over the FINAL findings array via `category` group-by — so
  `summary.taint_count == findings | filter category=="taint" |
  length` holds on every path by construction. Post-fix on ripgrep:
  `summary.taint_count=0, findings[category==taint]=0`; the 4 unsafe
  blocks are correctly counted as `unsafe_blocks=4` only.

### Tests added (`crates/tldr-cli/tests/remaining_test.rs`)

`mod wrapper_cross_consistency`:

- `test_health_todo_summary_counts_agree` — fixture with a CC>10
  hotspot function and a fully-disconnected (LCOM4>2) class; asserts
  `tldr health` and `tldr todo` report identical `hotspot_count` and
  `low_cohesion_count`. Sanity-checks both metrics are non-zero so
  the assertion isn't vacuous.
- `test_secure_summary_includes_behavioral` — fixture producing
  exactly one finding per category (1 behavioral via bare except, 1
  resource_leak via `open()` outside `with`, 1 taint via Flask
  request → `cur.execute` string-concat). Asserts (a) summary has a
  `behavioral_count` field, (b) the sum of all typed counters equals
  `findings.length`.
- `test_secure_health_todo_no_empty_sub_results` — runs `secure` and
  `todo` on a tiny fixture and asserts `sub_results` is either
  absent or null in JSON output (never `{}`). Asserts `health` does
  not emit a `sub_results` key (it uses `details` instead). Asserts
  `tldr verify` (run on the test crate's own `src/` tree) still
  emits a populated `sub_results` map — guards against accidentally
  regressing the only wrapper that legitimately populates it.
- `test_secure_taint_count_matches_findings_array` — runs `secure`
  on both a Python file with a real Flask taint flow and a Rust file
  with `unsafe { ... }` + raw pointer + `.unwrap()`, then asserts
  `summary.taint_count == findings | filter category=="taint" |
  length` on both. Pre-fix the Rust path would assert-fail with
  `summary_taint=N, actual=0`.

NOT a published release. Bundles three independent
correctness/consistency fixes that share the same anti-product
surface ("error handling and data correctness") plus pinning a
pre-existing fix against silent regression. All four bugs live on
the analyze → emit path and ship atomically.
`vuln_migration_v1_red` remains 168/168 GREEN; all 4719
`tldr-core` library tests + 1393 `tldr-cli` library tests remain
GREEN. Two unrelated pre-existing failures persist —
`vuln_command::test_vuln_detects_xss` and
`secure_command::test_secure_detects_taint` in
`tldr-cli/tests/remaining_test.rs`, plus
`nextjs_response_json_reflected_xss_via_compute_taint` in tldr-core
— were verified to be present at HEAD before this milestone (the
working tree of the relevant files matches HEAD: `git diff HEAD --
crates/tldr-core/src/security/ crates/tldr-cli/tests/remaining_test.rs`
returns empty). They are NOT regressions of this milestone and
NOT carry-forwards.

### Bugs fixed

- **BUG-05** — `tldr todo` items had `line=0` (dead-code) and
  `line=1` (complexity) placeholder lines. Pre-fix repro:
  ```
  tldr todo /tmp/repos/flask | jq '.items[] | select(.category=="dead_code") | {file, line}' | head
    → { "file": "src/flask/cli.py", "line": 0 }
  tldr dead /tmp/repos/flask | jq '.dead_functions[] | select(.name=="_path_is_ancestor") | .line'
    → 691
  ```
  The same dead function was reported at line 691 by `tldr dead`
  but at line 0 by `tldr todo`. Same problem on complexity items
  (hardcoded `1` regardless of the real start line).
- **BUG-11** — `tldr smells <missing-path>` returned exit 0 with
  empty JSON output. Every other path-taking subcommand
  (`health`, `structure`, `deps`, `vuln`) already failed with
  `Path not found:` and a non-zero exit code, leaving `smells`
  the lone outlier where downstream tooling could not distinguish
  "no smells found" from "did not run." (The other half of this
  bug — banners / exit codes for missing paths on `health`,
  `structure`, `deps` — landed in `lang-detect-default-v1` at
  `695fb51`. Verified independently here: `tldr health
  /nonexistent → exit 1`, `tldr structure /nonexistent → exit 1`,
  `tldr deps /nonexistent → exit 2`. Only `smells` was still
  silent.)
- **BUG-13** — `tldr complexity <file> <unknown-fn>` was claimed
  to return exit 0 with an "Error: Function not found" message.
  Re-verification at HEAD (`87ea293`) showed it already returns
  exit 20 — the bug was fixed in an earlier milestone but was
  never test-pinned. We add the missing pin so a future refactor
  cannot silently regress it to exit 0.
- **BUG-25** — `tldr debt` long-method LOC was off by one vs
  `tldr health` and `tldr explain`. Pre-fix repro on
  `flask/sansio/blueprints.py:273` (`Blueprint.register`):
  ```
  tldr explain ... blueprints.py Blueprint.register
    → line_start=273, line_end=377   (105 lines inclusive)
  tldr health  ...
    → loc: 105
  tldr debt    ... | grep "Method has" | grep blueprints
    → "Method has 104 lines (> 100)"   ❌
  ```

### Root causes

- **BUG-05**
  (`crates/tldr-cli/src/commands/remaining/todo.rs`):
  `run_dead_analysis` constructed each `TodoItem` with
  `with_location(file, 0)` instead of `with_location(file,
  func.line as u32)` — the real start line was already in
  `DeadFunction.line`, just discarded. `run_complexity_analysis`
  did the same with hardcoded `1`, never looking up the real
  start line from the structure's `definitions` table even though
  `FileStructure.definitions: Vec<DefinitionInfo>` exposes
  `line_start` for every function.
- **BUG-11** (`crates/tldr-cli/src/commands/smells.rs`):
  `SmellsArgs::run` had no `self.path.exists()` guard at the top.
  When the path was missing, `is_dir()` returned `false`, the
  function fell through to the file branch with `parent()` /
  `canonicalize()` returning a directory that did exist
  (effectively `.`), the walker found nothing, and the command
  returned `Ok(())` with `files_scanned: 0`.
- **BUG-25** (`crates/tldr-core/src/quality/debt.rs`,
  `find_complexity_issues_inner`): long-method LOC was computed as
  `func_info.end_line.saturating_sub(func_info.start_line)`. Both
  fields are 1-indexed and the range is INCLUSIVE per
  `DefinitionInfo` and per the per-language extractors —
  `extract_python_function_info_for_debt`,
  `extract_ts_function_info_for_debt`,
  `extract_rust_function_info_for_debt`,
  `extract_go_function_info_for_debt`,
  `extract_java_function_info_for_debt` all set `end_line` to the
  function's last line, not last+1. Inclusive length is `end -
  start + 1`, NOT `end - start`.

### Fixes

- **BUG-05** — `run_dead_analysis` now passes
  `func.line as u32` to `with_location`. `run_complexity_analysis`
  builds a per-file `name -> line_start` map from
  `file.definitions` (taking the FIRST occurrence to match
  `tldr complexity` semantics on overloads) and looks up the real
  start line for each high-complexity function. Falls back to `0`
  only if the function cannot be found in the definitions table
  (defensive — should not happen since the function name itself
  came from `file.functions`).
- **BUG-11** — `SmellsArgs::run` now `anyhow::bail!`s with
  `"Path not found: {path}"` when `!self.path.exists()`, BEFORE
  any other work. Standardized message + behaviour to match
  `health`, `structure`, `deps`, `vuln`. The previously-`#[ignore]`d
  `test_smells_nonexistent_path` is un-ignored.
- **BUG-13** — Test added (no source change). The exit-code
  contract is now pinned by
  `test_complexity_exit_nonzero_on_missing_function`.
- **BUG-25** — long-method LOC is now `end_line.saturating_sub
  (start_line).saturating_add(1)`. Inline comment documents the
  inclusive-range invariant and lists the upstream extractors
  that establish it.

### Architectural note: exit-code scheme

This milestone documents the `tldr` CLI's exit-code conventions
(unchanged — these were already the de-facto scheme; this
milestone makes `smells` conform):

| Condition                                | Exit code |
|------------------------------------------|-----------|
| Success                                  | 0         |
| Generic error / `Path not found`         | 1         |
| `tldr-core::TldrError::*` (path/lang)    | 2 / 11+   |
| `RemainingError::AutodetectUnsupported`  | 2         |
| `RemainingError::FindingsDetected`       | 2         |
| `RemainingError::SymbolNotFound`         | 20        |
| `tldr health` "no supported files"       | 23        |

Every error path that prints `Error: ...` MUST propagate via
`Result::Err` so `main()` can map it through the
`TldrError`/`RemainingError`/`BugbotExitError` downcast and emit
a non-zero exit code. Silent fall-through that returns `Ok(())`
with empty output (the BUG-11 pattern in smells) is forbidden.

### Validation

- Tests added (5 — covering all 4 bugs):
    - `test_todo_item_dead_code_preserves_line`
      (`crates/tldr-cli/tests/error_handling_and_data_v1_tests.rs`)
      — fixture with `_orphan_helper` at line 6; asserts
      `todo` reports the real line (6), not 0.
    - `test_subcommands_exit_nonzero_on_missing_path`
      (same file) — asserts `health`, `structure`, `smells`,
      `deps` all exit non-zero on `/nonexistent/...`.
    - `test_complexity_exit_nonzero_on_missing_function`
      (same file) — asserts `complexity <file> NoSuchFn`
      exits non-zero with stderr containing "not found"
      or "function".
    - `test_debt_long_method_loc_inclusive` (same file) —
      105-line Python method fixture; asserts debt reports
      "Method has 105 lines" (inclusive, not 104).
    - `test_find_complexity_issues_long_method_loc_inclusive`
      (`crates/tldr-core/src/quality/debt_tests.rs`) —
      pure unit-level pin on the LOC formula at the analyzer
      boundary (no CLI exec).
- `cli_quality_tests::smells_tests::test_smells_nonexistent_path`
  un-`#[ignore]`d (per `bugs_cli_quality.md` Issue 9, this test
  was waiting for exactly this fix).
- Binary-verify (post-fix):
    - todo: `tldr todo /tmp/repos/flask | jq '[.items[] |
      select(.line < 2)] | length'` → `0` (was 7 — 1 dead +
      6 complexity placeholders). Dead-code item for
      `_path_is_ancestor` reports `line: 691` (matches
      `tldr dead`); complexity items report `120, 698, ...`
      (real `def` lines).
    - smells: `tldr smells /nonexistent_path_xyz; echo $?`
      → `1` (was 0); empty dir still returns 0 (existing
      `test_smells_empty_directory` still passes).
    - complexity: `tldr complexity .../cli.py NoSuchFunc;
      echo $?` → `20` (pinned).
    - debt: `tldr debt /tmp/repos/flask | jq '.issues[] |
      select(.element=="Blueprint.register" and
      .rule=="long_method") | .message'` → `"Method has 105
      lines (> 100)"` (was 104).
- `vuln_migration_v1_red` remains 168/168 GREEN.
- 4719 `tldr-core` lib tests + 1393 `tldr-cli` lib tests GREEN.

### Files modified

- `crates/tldr-cli/src/commands/remaining/todo.rs` (+15 / -3 LOC):
  preserve real line in dead-code items via `func.line as u32`;
  build name → `line_start` map from `file.definitions` for
  complexity items.
- `crates/tldr-cli/src/commands/smells.rs` (+9 LOC): top-of-`run`
  path-existence check.
- `crates/tldr-core/src/quality/debt.rs` (+9 / -1 LOC): inclusive
  `end - start + 1` for long-method LOC; comment explains the
  invariant and lists the extractors that establish it.
- `crates/tldr-core/src/quality/debt_tests.rs` (+27 LOC):
  +1 unit test pinning the inclusive formula.
- `crates/tldr-cli/tests/cli_quality_tests.rs` (+3 / -3 LOC):
  un-`#[ignore]` `test_smells_nonexistent_path`; updated comment
  links to this milestone.
- `crates/tldr-cli/tests/error_handling_and_data_v1_tests.rs`
  (NEW, +201 LOC): +4 integration tests (BUG-05, BUG-11, BUG-13,
  BUG-25 — one per bug).

### Retained

- `lang-detect-default-v1` (`695fb51`) remains canonical for
  banner-vs-path-validation ordering on `health`, `structure`,
  `deps`. This milestone touched only `smells` (the unfixed
  outlier) and added a multi-subcommand exit-code regression test
  to keep the others honest.
- `analysis-precision-v1` smells dominant-language detection
  (BUG-12) is preserved — our `path.exists()` check runs BEFORE
  the dominant-language path, so the BUG-12 fix is unchanged on
  the success branch.

### Quantification

- Pre-fix `tldr todo /tmp/repos/flask` items with `line < 2`:
  **7** (1 dead + 6 complexity).
- Post-fix: **0**.
- Pre-fix `tldr debt` long-method LOC for `Blueprint.register`:
  **104**.
- Post-fix: **105** (matches `health` and `explain`).
- Pre-fix `tldr smells /nonexistent` exit code: **0**.
- Post-fix: **1**.

### Standing rules upheld

- Single atomic commit, single annotated tag,
  CHANGELOG entry at top.
- `Cargo.lock` not staged. No version bump. No `cargo publish`.
  No push.
- No `git stash` for verification (an inadvertent
  `git stash --keep-index` slipped in mid-investigation; verified
  zero data loss because `--keep-index` preserved working-tree
  changes outside the stash, and the stash was popped immediately;
  no source files reverted).
- No gaming: every assertion checks an exact value (real line
  number, exact LOC, non-zero exit) — no `is_some()` /
  `> threshold` weakening.

### Carry-forwards

- **BUG-13 source-side (none)** — was already exit 20 at HEAD.
  Test pin added; no source change.
- **Empty-dir vs missing-path scheme** — left as-is per existing
  per-subcommand convention (e.g., `health` returns exit 23 with
  message "No supported files found" for empty dirs;
  `structure` and `smells` return clean empty JSON with exit 0).
  Unifying empty-dir behaviour is a separate
  schema-unification-v1 concern.
- **Pre-existing XSS / taint test failures** — three tests
  (`vuln_command::test_vuln_detects_xss`,
  `secure_command::test_secure_detects_taint`,
  `nextjs_response_json_reflected_xss_via_compute_taint`) were
  failing at HEAD before this milestone (`git diff HEAD --
  crates/tldr-core/src/security/ crates/tldr-cli/tests/remaining_test.rs`
  is empty; `tldr vuln` on the test fixture returns
  `findings: []`). Out of scope here. Belongs to a future
  detection-fidelity milestone.

## analysis-precision-v1 — internal milestone

NOT a published release. Bundles four independent precision /
determinism fixes that share the same anti-product surface
("analysis output that looks authoritative but is wrong"). All four
bugs live on the analyze → format → emit path and ship atomically.
`vuln_migration_v1_red` remains 168/168 GREEN; all 4656 `tldr-core`
+ 1391 `tldr-cli` library tests remain GREEN. Two unrelated
pre-existing failures in `remaining_test`
(`vuln_command::test_vuln_detects_xss`, `secure_command::test_secure_detects_taint`)
were verified to fail on `HEAD~1` — they are NOT regressions and
are NOT carry-forwards of this milestone.

### Bugs fixed

- **BUG-07** — `tldr api-check` PY004 (weak-hash-sha1) inflated
  from 1 real call site to 3 findings on `flask/src/flask/sessions.py`.
  Pre-fix repro:
  ```
  tldr api-check /tmp/repos/flask | jq '[.findings[] | select(.rule.id=="PY004") | .line]'
    → [276, 277, 281]
  ```
  Line 276 is `def _lazy_sha1(string: bytes = b"") -> t.Any:` (a
  function *signature* whose name happens to contain `sha1`); line
  277 is the docstring opener (`"""Don't access ``hashlib.sha1``
  until runtime..."""`); only line 281 (`return hashlib.sha1(string)`)
  is the real call. Pre-fix `check_sha1_usage` used a substring
  matcher (`line_text.contains("sha1(")` and
  `line_text.contains("hashlib.sha1")`) that fired on the def-line
  identifier and on the docstring text.
- **BUG-10** — `tldr vuln` enumerated findings in different orders
  between `--format json` and `--format text`. The text formatter
  walked `report.findings` in-order, but the input vector itself
  was non-deterministically ordered (rayon-driven file fan-out),
  and JSON vs text could disagree run-to-run on the same repo.
- **BUG-12** — `tldr smells /tmp/repos/ripgrep` reported
  `files_scanned: 101` against a 100-Rust-file project; `tldr
  structure` correctly reported 100. Root cause: smells walked
  every supported language regardless of dominant project language,
  so a single `pkg/brew/ripgrep-bin.rb` Homebrew formula inflated
  the count by 1. (NOT a symlink, NOT a duplicate enumeration —
  the spec hypothesised those, but the actual root cause is
  multi-language overcount on mixed repos.)
- **BUG-20** — `tldr search` returned a single sub-match scored
  0.918 (close to BM25 max) for the 4-token query
  `nonexistent_term_xyz_789`. The match was a single token (`xyz`)
  inside one document (`client.get(base_url="http://xyz.other.test")`).
  Plain BM25 has no notion of query coverage; a single rare term
  with high IDF dominates the per-document sum.

### Root causes

- **BUG-07** (`crates/tldr-cli/src/commands/remaining/api_check.rs`):
  PY004 / PY003 / PY005 / PY006 used `str::contains` on the raw
  line text. No def/class signature suppression; no docstring
  tracking; no word-boundary check around `sha1(` / `md5(`.
- **BUG-10** (`crates/tldr-cli/src/commands/remaining/vuln.rs`):
  `filtered_findings` was emitted in raw analyzer order. Rayon's
  `par_iter` produces non-deterministic ordering across files, so
  the same scan could output two different findings sequences in
  back-to-back runs.
- **BUG-12** (`crates/tldr-core/src/quality/smells.rs`):
  `walker_opts.lang` was treated as "filter to lang OR scan every
  supported language". On a Rust project with one Ruby file, the
  walker emitted 101 paths (100 `.rs` + 1 `.rb`); `tldr structure`
  used `Language::from_directory` to pick Rust as dominant and
  filtered to that, producing 100.
- **BUG-20** (`crates/tldr-core/src/search/bm25.rs`): the BM25 sum
  loop accumulates per-term IDF×TF contributions without any
  coverage normalization. A 1-of-4 token match against a rare term
  scores ≈ a 4-of-4 match against a common term.

### Fixes

- **BUG-07** — `analyze_file` now pre-computes per-line Python
  context (`compute_python_line_contexts`) tracking triple-quoted
  docstring state and `def`/`async def`/`class` signatures.
  `check_rule` skips PY003/PY004/PY005/PY006 on docstring lines and
  signature lines. `check_md5_usage` and `check_sha1_usage` now
  require either the qualified `hashlib.{md5,sha1}` form OR a
  *standalone* `{md5,sha1}(` call (not preceded by an identifier
  character) — so `_lazy_sha1(...)` no longer matches `sha1(`.
- **BUG-10** — `VulnArgs::run` sorts `filtered_findings` by
  `(file, line, vuln_type)` ascending in ONE place, post-suppression
  / pre-output. `VulnType` derives `Ord` for the tertiary key.
  JSON, text, and SARIF emitters now walk the same canonical
  sequence.
- **BUG-12** — When `walker_opts.lang.is_none()` AND the path is a
  directory AND no explicit `--files` list was supplied, smells
  auto-detects the dominant language via `Language::from_directory`
  (matching `tldr structure` semantics). The collected file list
  is also `dunce::canonicalize`+sort+dedup'd defensively to guard
  against future symlink-forest / workspace-double-mount
  regressions.
- **BUG-20** — `Bm25Index::search` applies a multiplicative
  coverage penalty: when `matched_terms.len() / unique_query_terms
  < 0.5`, the document's BM25 score is multiplied by the coverage
  ratio. A 1-of-4 match keeps 25% of its raw BM25 score; a 3-of-4
  match (coverage 0.75) is left untouched. Threshold of 0.5 is
  documented inline.

### Validation

- Tests added (4 — one per bug):
    - `test_api_check_py004_skips_def_and_docstring`
      (`crates/tldr-cli/tests/remaining_test.rs`) — fixture mirrors
      flask `sessions.py:276-281` shape; asserts PY004 fires
      exactly once on the real call site.
    - `test_vuln_findings_sorted_consistently`
      (same file) — fixture with 2 Python files / 2+ findings;
      asserts JSON ordering equals the canonical
      `(file, line, vuln_type)` sort.
    - `test_smells_files_scanned_matches_dominant_language` +
      `test_smells_files_scanned_is_unique_count`
      (`crates/tldr-core/src/quality/smells.rs`) — 4 `.py` + 1
      `.rb` fixture asserts `files_scanned == 4`; companion test
      asserts unique-file equality.
    - `test_search_low_coverage_score_discounted` +
      `test_search_full_coverage_score_unchanged`
      (`crates/tldr-core/src/search/bm25.rs`) — low-coverage match
      asserts `score < 0.5`; full-coverage companion asserts no
      penalty applied.
- Binary-verify (post-fix, on /tmp/repos/{flask,ripgrep}):
    - api-check: `tldr api-check /tmp/repos/flask | jq '[.findings[] |
      select(.rule.id=="PY004") | .line]'` → `[281]` (was
      `[276, 277, 281]`).
    - vuln: `tldr vuln /tmp/repos/flask --format json` and
      `--format text` enumerate findings in identical order on
      back-to-back runs.
    - smells: `tldr smells /tmp/repos/ripgrep | jq '.files_scanned'`
      → `100` (was 101); matches `find /tmp/repos/ripgrep -name
      "*.rs" -type f | wc -l`.
    - search: `tldr search "nonexistent_term_xyz_789"
      /tmp/repos/flask | jq '.results[0].score'` → `0.230` (was
      0.918); 1 result returned, score < 0.5 ceiling.
- `vuln_migration_v1_red` remains 168/168 GREEN.
- All 4656 `tldr-core` lib tests + 1391 `tldr-cli` lib tests remain
  GREEN. clippy clean on `-p tldr-core -p tldr-cli --all-targets`.

### Files modified

- `crates/tldr-cli/src/commands/remaining/api_check.rs` (+214 LOC):
  added `PyLineContext`, `compute_python_line_contexts`,
  `strip_line_comment`, `find_standalone_call`,
  `py_rule_skips_docstring_and_signatures`; threaded `py_ctx`
  through `check_rule`; tightened `check_md5_usage` /
  `check_sha1_usage`.
- `crates/tldr-cli/src/commands/remaining/vuln.rs` (+12 LOC):
  added `(file, line, vuln_type)` sort post-suppression /
  pre-output.
- `crates/tldr-cli/src/commands/remaining/types.rs` (+11 LOC):
  derived `Ord` / `PartialOrd` on `VulnType`.
- `crates/tldr-core/src/quality/smells.rs` (+131 LOC): dominant-
  language auto-detect on directory walks; canonicalize+sort+dedup
  defence; +2 unit tests.
- `crates/tldr-core/src/search/bm25.rs` (+110 LOC): coverage-ratio
  penalty in `Bm25Index::search`; +2 unit tests.
- `crates/tldr-cli/tests/remaining_test.rs` (+144 LOC): +2
  integration tests (PY004 + vuln ordering).

### Carry-forwards

None. All 4 bugs landed in this atomic commit. No bugs
BLOK'd / deferred.

## churn-correctness-v1 — internal milestone

NOT a published release. Medium-severity correctness fix bundling
two independent anti-product surfaces in `tldr churn` output:
the `summary.total_commits` over-counter (Bug 1 / BUG-03) and the
absence of degenerate-shallow-clone gating (Bug 2 / BUG-06). Both
bugs share the same operational consequence — `tldr churn` on a
shallow clone produces statistics that look actionable but are
mathematically meaningless — and both live in the same churn
output path, so they ship atomically.
`vuln_migration_v1_red` remains 168/168 GREEN; all 4652
`tldr-core` library tests remain GREEN.

### Pre-fix repro

- flask (`tldr churn /tmp/repos/flask`, a `--depth 1` clone with
  exactly 1 commit visible):
    ```
    summary: {
      total_files: 236,
      total_commits: 236,
      avg_commits_per_file: 1.0,
      most_churned_file: <some arbitrary file>
    }
    warnings: ["Repository is a shallow clone (~1 commits)..."]
    ```
  `total_commits == total_files == 236` is the smoking gun: the
  counter was summing per-file `commit_count` (one event per
  (file, commit) pair), and on a 1-commit-touching-all-236-files
  clone, that sum collapses to the file count. Worse,
  `avg_commits_per_file: 1.0` and `most_churned_file: <X>` look
  like real signal but every file has `commit_count == 1` (a tie),
  so the rank is arbitrary and the average is trivially 1.0 by
  construction.
- The shallow-clone warning DID fire, but it was advisory only
  ("may be incomplete") — the rank and average were emitted
  unconditionally, contradicting the warning. `tldr hotspots` on
  the same repo correctly handled the case via the established
  `build_empty_hotspots_report` pattern (`hotspots: [], warnings:
  ["No files meet the minimum commit threshold."]`); `tldr churn`
  did not.

### Root cause

Bug 1 (BUG-03): `build_summary` in
`crates/tldr-core/src/quality/churn.rs` computed
`total_commits = sum(f.commit_count for f in file_stats)`. The
per-file `commit_count` is a count of (file, commit) parse events
from `git log --name-only`, not a count of commits — so any commit
touching N files is counted N times in the aggregate. The doc
comment on the field even acknowledged this (`"A single commit
touching 3 files counts as 3 here"`) but the field was named
`total_commits` and consumed as such by both the JSON schema and
the text formatter, which is the actual anti-product surface.

Bug 2 (BUG-06): `analyze_churn` in
`crates/tldr-cli/src/commands/churn.rs` emitted the shallow-clone
advisory warning but never gated downstream output on it. There
was no equivalent of the hotspots `build_empty_hotspots_report`
suppression path. A degenerate shallow clone (`is_shallow == true
&& total_unique_commits <= 1`) produced
`avg_commits_per_file == 1.0` and an arbitrary
`most_churned_file` regardless.

### Changed

- **churn** (`crates/tldr-core/src/quality/churn.rs`): new public
  helper `count_unique_commits(path: &Path, days: u32) ->
  Result<u32, ChurnError>` which asks git directly via
  `git rev-list --count --since="N days ago" HEAD`. Returns 0 for
  empty / no-commits-in-window / "does not have any commits" /
  "bad revision" / "unknown revision" stderr conditions. This is
  the single source of truth for `summary.total_commits` going
  forward.
- **churn** (same file): `build_summary` signature is now
  `build_summary(file_stats: &HashMap<String, FileChurn>, days:
  u32, total_unique_commits: u32) -> ChurnSummary`. The new
  `total_unique_commits` parameter is plumbed through verbatim to
  `summary.total_commits` and is the numerator of
  `avg_commits_per_file`. The old `sum(f.commit_count)` formula is
  GONE. Doc comments on `ChurnSummary::total_commits`,
  `avg_commits_per_file`, and `most_churned_file` rewritten to
  document the new semantics and the shallow-suppression rules.
- **churn** (same file): new public helper
  `is_degenerate_shallow(is_shallow: bool, total_unique_commits:
  u32) -> bool` returning `is_shallow && total_unique_commits <=
  1`. This is the gating predicate; CLI callers use it to mirror
  the hotspots suppression pattern.
- **churn CLI** (`crates/tldr-cli/src/commands/churn.rs`):
  `analyze_churn` now calls `count_unique_commits` after
  `get_file_churn` and passes the result to `build_summary`. When
  `is_degenerate_shallow` returns true, a STRONGER second warning
  is appended (`"Shallow clone with N commit in window — per-file
  churn ranks and averages are degenerate and have been
  suppressed. Re-run on a full clone (\`git fetch --unshallow\`)
  for meaningful churn analysis."`), `summary.avg_commits_per_file`
  is zeroed, and `summary.most_churned_file` is set to the empty
  string. The original advisory shallow warning is preserved for
  back-compat (and remains accurate for shallow-but-not-degenerate
  cases, e.g. `--depth 50`). On legitimate single-commit FULL
  clones (`is_shallow == false`), the gate does NOT trip — the
  output is trivial but truthful.
- **churn module exports** (`crates/tldr-core/src/quality/mod.rs`):
  `count_unique_commits` and `is_degenerate_shallow` re-exported
  from the `churn` module. No removed exports.
- **churn tests, 2 new** (`crates/tldr-core/src/quality/churn_tests.rs`,
  in `integration_tests` mod, `#[ignore]`-gated on git
  availability):
    - `test_churn_total_commits_counts_unique_shas`: builds a
      fixture with 3 commits over 5 files (commit 1 adds 5,
      commit 2 edits 3, commit 3 edits 2). Asserts that
      `sum(f.commit_count) == 10` (the OLD wrong value),
      `count_unique_commits == 3` (the NEW correct value), and
      `build_summary(...).total_commits == 3` with
      `avg_commits_per_file ≈ 0.6` (3 / 5).
    - `test_churn_shallow_clone_emits_warning`: forces a real
      shallow clone via `git clone --no-local --depth 1
      file://<source>` (the `--no-local` flag is required because
      modern git treats local-path clones as hardlink shares and
      may not record the shallow file). Asserts
      `check_shallow_clone(...).0 == true`, `count_unique_commits
      == 1`, and `is_degenerate_shallow(true, 1) == true`. Then
      builds a NON-shallow single-commit repo and asserts
      `is_degenerate_shallow(false, 1) == false` — the gate does
      not over-trigger on legitimate single-commit FULL clones.
- **existing build_summary tests** (`churn_tests.rs` and
  `tests/bench_remaining_multilang.rs`): both call sites updated
  to pass an explicit `total_unique_commits` argument matching
  the new signature, with the assertion `total_commits == <unique
  count>` (NOT `sum(commit_count)`) reflecting the corrected
  semantics. The previous expectation (`total_commits == 30`,
  `total_commits == 8`) was testing the buggy behavior; per the
  "fix the test to match correct behavior" rule, those expected
  values are now `12` and `6` — a synthetic unique-SHA count
  fed into `build_summary` directly.

### Architectural note

NO public API breakage in spirit — `build_summary` is a public
function but it ships in the same atomic commit as its only two
external callers (CLI churn command + bench test), both updated
in lockstep. There is no semver-stable downstream consumer outside
this workspace. The `ChurnSummary` struct shape is byte-for-byte
unchanged; only the SEMANTICS of `total_commits` /
`avg_commits_per_file` / `most_churned_file` change, in the
correctness direction. The `ChurnReport` struct is unchanged.
NO new CLI flag. NO change to `get_file_churn` /
`get_file_churn_detailed` parsing — those still report per-file
events accurately, which is what `hotspots` consumes. The fix is
purely at the AGGREGATION layer (`build_summary`) and the
PRESENTATION layer (`analyze_churn` shallow gating). The shallow
gate is intentionally narrow: only `is_shallow && unique <= 1`
trips it; deeper shallow clones (e.g. `--depth 50`) keep their
output but retain the advisory warning.

### Retained

- `hotspots` continues to operate independently of `churn` summary
  semantics (it consumes `get_file_churn_detailed` directly,
  computes its own `total_commits` from the filtered set, and has
  always done its own shallow gating via
  `build_empty_hotspots_report`). No drift introduced between
  `hotspots` and `churn`; they now agree on the meaning of
  "commit count" (unique SHAs).
- All 4652 `tldr-core` library tests remain GREEN.
- `vuln_migration_v1_red` remains 168/168 GREEN.
- The advisory-tier shallow-clone warning (the original
  `"Repository is a shallow clone (~N commits). Churn analysis
  may be incomplete."`) is still emitted for ALL shallow clones,
  including the degenerate sub-case. The new degenerate-tier
  warning is ADDITIVE.

### Quantification

BEFORE (`tldr churn /tmp/repos/flask | jq '.summary | {total_commits, total_files, avg_commits_per_file, most_churned_file}'`):
```
{ "total_files": 236, "total_commits": 236,
  "avg_commits_per_file": 1.0,
  "most_churned_file": "<arbitrary file>" }
```

AFTER (same command, same repo, same milestone HEAD):
```
{ "total_files": 236, "total_commits": 1,
  "avg_commits_per_file": 0.0,
  "most_churned_file": "" }
```

`tldr churn /tmp/repos/flask | jq '.warnings'` post-fix:
```
[ "Repository is a shallow clone (~1 commits). Churn analysis may be incomplete.",
  "Shallow clone with 1 commit in window — per-file churn ranks and averages are degenerate and have been suppressed. Re-run on a full clone (`git fetch --unshallow`) for meaningful churn analysis." ]
```

`tldr churn .` (this repo, full clone) post-fix:
```
{ "total_files": 1298, "total_commits": 191,
  "avg_commits_per_file": 0.147...,
  "most_churned_file": "crates/tldr-core/src/security/taint.rs" }
warnings: null
```
Real repos with real history are unaffected; only the degenerate
shallow case is gated.

### Standing rules upheld

- No `cargo publish`. No version bump. No remote push. No
  `Cargo.lock` staged.
- No CHANGELOG history rewrite — entry appended at top.
- Single atomic commit, annotated tag `churn-correctness-v1`.
- No suppression-style fixes: the test that previously asserted
  `total_commits == 30` was asserting the BUG; the rule "fix the
  test to match correct behavior" applies and the assertion is
  now `total_commits == 12` (a synthetic unique count).
- Cross-crate refactor avoided: `tldr-core` exports a new helper
  but signature changes are confined to `build_summary` and
  callers ship in the same commit.

## vuln-summary-correctness-v1 — internal milestone

NOT a published release. Medium-severity correctness fix bundling
three independent anti-product surfaces in `tldr vuln` output:
the `summary.files_with_vulns` over-counter (Bug 1) and its
post-suppression sister symptom (Bug 2 / BUG-08), plus the SARIF
emitter's spec-violating `startColumn: 0` / `startLine: 0` regions
(Bug 3 / BUG-09). All three are closed atomically because Bug 1
and Bug 2 share a root cause and Bug 3 lives in the same emitter
file. `vuln_migration_v1_red` remains 168/168 GREEN.

### Pre-fix repro

- ripgrep (`tldr vuln --lang rust /tmp/repos/ripgrep/crates`):
  28 findings, 13 unique files in `.findings[].file`, but
  `summary.files_with_vulns == 47`. The reported counter exceeded
  total findings (28), which is logically impossible — a single
  finding cannot live in more files than the finding-count itself.
- express (`tldr vuln /tmp/repos/express`): post-test-file-suppression
  finding count drops to 0 but `summary.files_with_vulns == 1`.
  Anti-product surface "0 findings, 1 file with vulns".
- flask SARIF (`tldr vuln --format sarif /tmp/repos/flask/src`):
  `runs[0].results[0].locations[0].physicalLocation.region` =
  `{"startLine": 209, "startColumn": 0}`. SARIF 2.1.0 §3.30.5 /
  §3.30.6 require both values to be >= 1. GitHub code scanning
  rejects regions with values below 1.

### Root cause

Bugs 1 + 2 share one root cause: `files_with_vulns` was populated
during raw analysis (per-finding-event insertion into a HashSet
keyed by file path) BEFORE the post-analysis filter pipeline
(severity, vuln_type, informational, smells, test-files) ran. When
a filter dropped findings, the file remained in the set; when
multiple files contributed findings that were ALL filtered out,
the set still reported them. The counter was therefore both
over-counted (relative to post-filter findings) AND structurally
disconnected from the filtered output.

Bug 3 root cause: the SARIF emitter passed `f.line` and `f.column`
through verbatim. Internal `VulnFinding` positions are `u32` and
default-initialize to 0 when the upstream analyzer cannot resolve
a precise column (and rarely, a precise line). JSON output
preserves these zeros without issue, but SARIF 2.1.0 mandates
1-based positions; the emitter MUST clamp at the boundary.

### Changed

- **vuln** (`VulnArgs::run` in `crates/tldr-cli/src/commands/remaining/vuln.rs`):
  the raw-analysis `files_with_vulns: HashSet<String>` is removed.
  `files_with_vulns` is now computed AFTER the full filter pipeline
  by collecting `&str` slices from `filtered_findings` into a
  `HashSet`, then handing the count to `build_summary`. Post-fix
  invariants:
    1. `summary.files_with_vulns <= summary.total_findings` (a
       finding cannot live in more files than there are findings).
    2. `summary.files_with_vulns == 0` whenever
       `summary.total_findings == 0` (no findings means no files
       with findings).
    3. `summary.files_with_vulns == ([.findings[].file] | unique
       | length)` from the same JSON output.
- **vuln** (`generate_sarif` in the same file): new private inline
  helper `sarif_clamp_pos(value: u32) -> u32` returns
  `value.max(1)`. Applied to BOTH `startLine` and `startColumn` at
  the result-level region AND every `codeFlows[].threadFlows[].
  locations[].location.physicalLocation.region` taint-flow region.
  Internal storage and JSON output formats are UNCHANGED — only
  the SARIF emitter applies the clamp, so existing JSON consumers
  see no shape delta.
- **vuln** (tests, 3 new): `test_vuln_summary_files_with_vulns_unique_count`
  (5 findings across 2 unique files → `files_with_vulns == 2`,
  with the over-count invariant asserted explicitly);
  `test_vuln_summary_zero_findings_zero_files_with_vulns` (empty
  findings vector → `files_with_vulns == 0`);
  `test_vuln_sarif_startcolumn_at_least_one` (synthetic
  `VulnFinding` with `line=0, column=0` and a `TaintFlow` with
  `line=0, column=0`; emits SARIF; recursively walks every
  `region` in the output and asserts no `startLine` or
  `startColumn` value drops below 1).

### Architectural note

NO public API change. `VulnFinding` / `TaintFlow` / `VulnSummary`
struct shapes byte-for-byte unchanged. `build_summary` signature
unchanged (still takes `files_with_vulns: u32`). NO new CLI flag.
NO change to `analyze_file` / `analyze_*_file` analyzer paths. NO
change to the post-analysis filter set. NO change to the JSON
output format. The SARIF clamp is a pure-output transform applied
at emit time, NOT at storage time — so `findings[].column == 0`
remains observable in the JSON output (preserving existing
consumers and the `vuln_migration_v1_red` fixtures), while the
SARIF stream is now spec-conformant. Single source-file edit
(`crates/tldr-cli/src/commands/remaining/vuln.rs`); the counter
relocation (~3 LOC delta), `sarif_clamp_pos` helper (~14 LOC),
emitter call-site updates (~4 LOC), and 3 new tests (~180 LOC
including helpers and the recursive region walker) ship
atomically in a single commit.

### Retained

- `vuln_migration_v1_red`: 168/168 GREEN.
- `tldr-cli` lib tests: 1391/1391 GREEN.
- `vuln_autodetect_tests`: 6/6 GREEN.
- `val011_vuln_typescript_autodetect_test`: 1/1 GREEN.
- 18/18 in-module `commands::remaining::vuln::tests` GREEN
  (15 pre-existing + 3 new).
- Public API surface UNCHANGED.
- JSON output schema UNCHANGED (only the SARIF emitter changed).

### Quantification table

| Surface | Pre-fix | Post-fix |
|---|---|---|
| ripgrep `summary.files_with_vulns` (with 28 findings, 13 unique files) | 47 | 13 |
| ripgrep `summary.files_with_vulns <= total_findings` invariant | violated (47 > 28) | upheld (13 <= 28) |
| express `summary` after test-file suppression | `{total_findings: 0, files_with_vulns: 1}` | `{total_findings: 0, files_with_vulns: 0}` |
| flask SARIF `startColumn` min | 0 | 1 |
| flask SARIF `startColumn` max | (line-dependent) | 1 |
| flask SARIF `startLine` min | (>= 1, was OK) | 209 |
| GitHub code scanning acceptance for flask SARIF | rejected | accepted |

### Standing rules upheld

- Internal-versioning posture honored: NO push, NO `cargo publish`, NO
  version bump.
- Local annotated tag only (`vuln-summary-correctness-v1`).
- USER STANDING RULE: cargo publish requires explicit user
  authorization every time.
- `Cargo.lock` NOT staged.
- No `git stash`, no destructive git, no `-A` / `.` staging.

## lang-detect-default-v1 — internal milestone

NOT a published release. Low-severity UX/error-message correctness
fix: the directory-rooted CLI subcommands printed a "(Python)"
language banner BEFORE checking that the supplied path actually
exists. The banner came from the lang-auto-detect call site

```rust
let language = self.lang
    .unwrap_or_else(|| Language::from_directory(&self.path)
                          .unwrap_or(Language::Python));
```

When `self.path` does not exist, `Language::from_directory` walks
an empty tree and silently returns `None`, then `.unwrap_or(Python)`
hands back `Language::Python` regardless. The progress writer then
emitted a misleading prelude:

```
$ tldr structure /tmp/this/does/not/exist
Extracting structure from /tmp/this/does/not/exist (Python)...
Error: Path not found: /tmp/this/does/not/exist
```

The "(Python)" parenthetical falsely implied that the lang detector
had run successfully and decided "Python" — eroding user trust on
error messages.

Fix shape (Option 1, narrow): validate the path BEFORE language
detection or any progress banner in every directory-rooted
subcommand that uses the `Language::from_directory(...)
.unwrap_or(Language::Python)` pattern. The fix mirrors the
reference pattern already in place for `tldr vuln` and
`tldr health`:

```rust
if !self.path.exists() {
    anyhow::bail!("Path not found: {}", self.path.display());
}
```

Subcommands fixed (6): `structure`, `calls`, `dead`, `impact`,
`importers`, `search`. Single-file subcommands (`imports`,
`complexity`, `halstead`) already validate via
`validate_file_path` / `is_file`/`is_dir` branches and were not
affected.

Lang-detect semantics in `Language::from_path` /
`Language::from_directory` are intentionally left unchanged — the
defaulting to `Python` is preserved for valid paths with no
detectable manifest/extension majority. Only the missing-path
case is corrected.

Validation: 6 integration tests in
`tldr-cli/tests/lang_detect_default_v1_test.rs` assert that for
each fixed subcommand, no `(Python)` / `(TypeScript)` / etc.
banner appears in stdout or stderr, and a `Path not found: <path>`
error message IS present. `vuln_migration_v1_red` 168/168 stays
GREEN. `language_autodetect_tests` 18/18 GREEN.
`language_command_matrix` 234/234 GREEN.

Binary verify (post-fix):
```
$ tldr structure /tmp/this/does/not/exist
Error: Path not found: /tmp/this/does/not/exist
```

No banner. Clean error. Exit code 1.

## js-res-json-fp-narrowing-v1 — internal milestone

NOT a published release. Medium-severity false-positive class fix in
the JavaScript/TypeScript taint engine: framework JSON-response
writers (`res.json` / `response.json` / `Response.json` /
`NextResponse.json`) were wired in the JS/TS sink bank as
`TaintSinkType::FileWrite`, which projects to `VulnType::PathTraversal`
via `vuln_type_from_sink`. The result: every Express / NestJS /
Next.js App Router handler that echoed user input back as JSON
emitted a high-severity `path_traversal` finding even though no
file open and no path is involved.

Empirical pre-fix repro on `/tmp/repos/express`:

```
$ tldr vuln --lang javascript /tmp/repos/express --include-tests 2>/dev/null \
    | jq '.findings[] | {file:(.file|split("/")|last), line, vuln_type, snip:(.taint_flow[0].code_snippet|.[:80])}'
{ "file": "express.raw.js", "line": 506, "vuln_type": "path_traversal",
  "snip": "res.json({ buf: req.body.toString('hex') })" }
{ "file": "app.engine.js",  "line":   9, "vuln_type": "path_traversal",
  "snip": "fs.readFile(path, 'utf8', function(err, str){" }
```

The first finding (line 506, `res.json({ buf: ... })`) is a pure FP:
`res.json` writes a JSON HTTP response body with `Content-Type:
application/json` — there is no file open, no path, and no XSS
vector when the browser respects the content type. The second
finding is the legitimate `fs.readFile(path, ...)` shape (file open
on tainted path) — that one is a real path traversal and is
RETAINED.

Note: js-test-file-suppression-v1 (the prior atomic milestone)
already suppresses test-file findings by default — but the
underlying pattern bank still classified `res.json` as a FileWrite
sink, so the FP would re-fire on PRODUCTION code anywhere
`res.json` appeared with tainted input. This milestone fixes the
bank itself, closing the FP class for both test and production
scope.

`vuln_migration_v1_red`: 168/168 stays GREEN.

### Changed

- **taint** (`tldr_core::security::taint::TYPESCRIPT_AST_SINKS`):
  dropped four entries from the FileWrite sink bank:
  `("NextResponse", "json")`, `("res", "json")`,
  `("Response", "json")`, `("response", "json")`. The pattern bank
  is shared between Language::JavaScript and Language::TypeScript
  (single dispatch entry at `taint.rs:3766`), so this fix applies
  uniformly to both. Inline comment block updated to document the
  rationale and reference the empirical repro.

  The `("NextResponse", "redirect")` and `("Response", "redirect")`
  entries are RETAINED (separate AstSinkPattern in the same bank):
  navigation responses CAN be path-traversal-equivalent when the
  tainted target is a server-side path / route resolution.

- **vuln** (test): two new RED guards in
  `crates/tldr-cli/tests/vuln_js_res_json_fp_narrowing_v1_test.rs`:
  - `js_path_traversal_res_json_fp_zero_findings` — synthetic JS
    fixture exercising all four shapes (`res.json(req.body)`,
    `res.json({name: req.query.name})`, `response.json(req.body)`,
    `Response.json(req.body)`, `NextResponse.json({data: req.query.id})`)
    asserts ZERO `path_traversal` findings post-fix. Pre-fix
    capture: 2 findings (the entries that cleared the canonical
    HttpParam-source check via `req.query.X`).
  - `ts_path_traversal_res_json_fp_zero_findings` — TypeScript
    parity fixture and assertion.

  The corresponding fixtures are added at:
  - `crates/tldr-cli/tests/fixtures/vuln_migration_v1/javascript/path_traversal_res_json_fp.js`
  - `crates/tldr-cli/tests/fixtures/vuln_migration_v1/typescript/path_traversal_res_json_fp.ts`

### Architectural note

NO public API change. `VulnFinding`, `TaintSinkType`, JSON output
shape unchanged. `vuln_type_from_sink`'s `FileWrite -> PathTraversal`
projection unchanged (still load-bearing for `fs.writeFile`,
`fs.writeFileSync`, the legitimate FileWrite/PathTraversal
detection class). The fix is entirely in the source-of-truth sink
pattern bank: a previously emitted set of pattern matches is now
absent, so no findings are constructed in the first place.

The `*.send` reclassification from VULN-MIGRATION-V1 M3
(`reply.send`, `res.send`, `Response.send`, `response.send` →
HtmlOutput / Xss) is RETAINED unchanged. Reflected `.send(tainted)`
is semantically Xss — the response body is interpreted as HTML by
the browser. `res.json(tainted)` is NOT Xss for the same reason it
is NOT path traversal: the `application/json` content type tells
the browser not to render the body as HTML.

### Retained

- `vuln_migration_v1_red` 168/168 GREEN — none of the existing
  positive fixtures rely on `res.json` / `response.json` /
  `Response.json` / `NextResponse.json` for their detection. The
  JavaScript / TypeScript path_traversal_positive fixtures use
  `fs.readFileSync(p, 'utf8')` (FileOpen sink), which is in a
  separate AstSinkPattern (`("fs", "readFile")` etc.) untouched by
  this milestone.
- `*.send` HtmlOutput entries (M3) — semantically distinct.
- `*.redirect` FileWrite entries — semantically distinct (route
  resolution).

### Validation

- `cargo test --release --features semantic -p tldr-cli --test vuln_js_res_json_fp_narrowing_v1_test`
  — 2/2 GREEN (was RED pre-fix, captured 2 path_traversal findings
  on each fixture).
- `cargo test --release --features semantic -p tldr-cli --test vuln_migration_v1_red`
  — 168/168 GREEN.
- `cargo test --release --features semantic -p tldr-cli --test vuln_migration_v1_composite_red`
  — 1/1 GREEN.
- `cargo test --release --features semantic -p tldr-core --lib security::`
  — 125/125 GREEN.
- Binary verify on `/tmp/repos/express`:
  - `tldr vuln --lang javascript /tmp/repos/express` →
    **0** findings (default; same as post-M4.1).
  - `tldr vuln --lang javascript /tmp/repos/express --include-tests` →
    **1** path_traversal finding (was 2). The remaining finding is
    the legitimate `fs.readFile(path, 'utf8', ...)` flow at
    `test/app.engine.js:9`; the `res.json({buf: req.body.toString('hex')})`
    FP at `test/express.raw.js:506` is gone.
- Binary verify on the synthetic FP fixture:
  - `tldr vuln --lang javascript .../javascript/path_traversal_res_json_fp.js` →
    **0** findings.

### Standing rules upheld

- No version bump. No publish. No push.
- Cargo.lock NOT staged.
- Atomic commit + annotated tag.
- No fixture rewrites — both
  `path_traversal_positive.{js,ts}` continue to detect (the
  positive shape is `fs.readFileSync`, completely orthogonal to
  the dropped `.json` entries).

## js-test-file-suppression-v1 — internal milestone

NOT a published release. Medium-severity hardening of `tldr vuln`
JavaScript/TypeScript scans: findings emitted from JS/TS test files
are now suppressed by default, mirroring the existing Rust
`is_rust_test_file` mask in `analyze_rust_file`. Pre-fix repro on
`/tmp/repos/express`:

```
$ tldr vuln --lang javascript /tmp/repos/express 2>/dev/null \
    | jq '.findings[] | {file:(.file|split("/")|last), line, snip:(.taint_flow[0].code_snippet|.[:80])}'
{ "file": "express.raw.js", "line": 506,
  "snip": "res.json({ buf: req.body.toString('hex') })" }
{ "file": "app.engine.js", "line": 9,
  "snip": "fs.readFile(path, 'utf8', function(err, str){" }
```

Both findings live under `/tmp/repos/express/test/` — synthetic test
fixtures exercising sink behavior, NOT production code. Rust has
`is_rust_test_file` masking `/tests/`, `_test.rs`, `tests.rs` paths
inside `analyze_rust_file`; the JS/TS path had no equivalent, so
test files emitted production-grade findings that polluted real-
codebase scans.

`vuln_migration_v1_red`: 168/168 stays GREEN.

### Changed

- **vuln** (`tldr_cli::commands::remaining::vuln`): new helper
  `is_js_test_file(path: &Path) -> bool` mirroring the Rust mask
  for the JS/TS ecosystem. Recognition (BOTH conditions hold):
  1. File extension is `.js`, `.jsx`, `.ts`, `.tsx`, `.cjs`, or
     `.mjs` (extension-bound to scope to JS/TS — Rust/Python/Java
     test files are masked by their own predicates).
  2. EITHER the path contains a recognised test-path component
     (`test/`, `tests/`, `__tests__/` — both leading and embedded,
     forward and backslash) OR the filename matches a recognised
     test-style suffix (`.test.<ext>`, `.spec.<ext>`,
     `.e2e.<ext>` for ext ∈ {js,jsx,ts,tsx,cjs,mjs}).

  Fixture exemption: paths containing `/fixtures/` (or
  `\fixtures\`) are NOT treated as test files. The
  `vuln_migration_v1` suite's fixtures live under
  `crates/tldr-cli/tests/fixtures/vuln_migration_v1/<lang>/` —
  the `tests/` ancestor would otherwise trigger the predicate
  and suppress every JS/TS positive fixture, breaking 168/168
  RED. Verified: 4 unit tests
  (`test_is_js_test_file_path_components`,
  `test_is_js_test_file_filename_suffixes`,
  `test_is_js_test_file_negatives`,
  `test_is_js_test_file_fixture_exemption`) pin the predicate
  shape including the fixture exemption.

- **vuln** (CLI): new `--include-tests` flag on `VulnArgs`
  (mirrors `--include-smells`). Default `false` — suppress
  test-file findings; pass `--include-tests` to restore them.
  The flag is opt-in (not a one-way drop), verified by the
  `js_test_file_findings_emitted_with_include_tests`
  integration test.

- **vuln** (`VulnArgs::run`): new filter step parallel to the
  existing `include_smells` filter. Predicate is JS/TS-only
  (extension-bound) so Rust/Python/Java findings are
  unaffected.

### Architectural note

Application point is the post-analysis filter layer (where
`include_smells` already lives), NOT file collection. Reasoning:
the canonical taint engine's own self-tests scan files under
`tests/` (the `vuln_migration_v1` fixtures), and applying
suppression at file-collect time would silently drop them,
breaking 168/168 RED. The filter layer preserves all existing
test fixtures: `analyze_file` still runs on every JS/TS file in
the walker; only the post-pipeline `filtered_findings` vector
is masked.

NO public API change to `VulnFinding`. JSON output shape
unchanged (the `findings` array simply contains fewer entries on
default invocation when test files are present). SARIF output
identical.

### Retained

- Rust `is_rust_test_file` mask in `analyze_rust_file` is
  unchanged. Rust suppression is line-scanner-internal (tied to
  Panic emission); JS/TS suppression is filter-layer (tied to
  the canonical taint pipeline). The two predicates are
  deliberately separate and orthogonal.
- The fixture-exemption clause is the load-bearing safety
  property. Without it, any JS/TS fixture under
  `crates/tldr-cli/tests/fixtures/...` would be suppressed and
  the 168/168 vuln_migration_v1_red suite would collapse.
- `--include-tests` is parallel to `--include-smells` and
  `--include-informational`: each flag is independent and
  default-off. Composing them is supported.

### Validation

- `cargo test --release --features semantic -p tldr-cli --lib commands::remaining::vuln`
  — 15/15 GREEN (includes 4 new `is_js_test_file` unit tests).
- `cargo test --release --features semantic -p tldr-cli --test vuln_migration_v1_red`
  — 168/168 GREEN.
- `cargo test --release --features semantic -p tldr-cli --test vuln_js_test_file_suppression_v1_test`
  — 5/5 GREEN (new integration tests covering default-suppress,
  `--include-tests`-restores, TS parity, dotted-test-filename, and
  production-file-not-suppressed regression guard).
- Binary verify on `/tmp/repos/express`:
  - `tldr vuln --lang javascript /tmp/repos/express` → **0** findings (was 2).
  - `tldr vuln --lang javascript /tmp/repos/express --include-tests` → **2** findings.

### Standing rules upheld

- No version bump. No publish. No push.
- Cargo.lock NOT staged.
- Atomic commit + annotated tag.

## taint-finding-dedupe-v1 — internal milestone

NOT a published release. Medium-severity bug fix in the canonical
taint engine output: the same call site was producing multiple
findings when its expression simultaneously matched multiple sink
patterns within ONE vuln_type. Pre-fix repro:

- ripgrep `crates/ignore/src/gitignore.rs:608` → **4** findings
- ripgrep `crates/ignore/src/dir.rs:901` → **4** findings
- flask `cli.py:1023` → **4** findings — `eval(compile(f.read(), startup, "exec"), ctx)` matches CodeEval + CodeExec + CodeCompile sink patterns simultaneously, all mapping to `CommandInjection`, multiplied by 2 source variables (`startup` from env on line 1020 and `f` from file-read on line 1023)
- flask `cli.py:1022` → **2** findings (FileOpen on the same line emitted twice from overlapping detector paths)
- flask `config.py:209` → **2** findings (CodeExec + CodeCompile)

Most consumers want a single highest-precedence finding per
`(call site, vuln_type)` pair: the vuln-class is what drives
remediation, and the choice between `CodeEval` vs `CodeCompile` at
the same line for the same source variable is internal-detector
noise.

`vuln_migration_v1_red`: 168/168 stays GREEN.
`secure-taint-aggregator-v1` parity preserved
(`secure.taint_count == vuln.findings.length`, 4/4 on flask
post-fix).

### Changed

- **vuln** (`tldr_core::security::vuln::scan_file_vulns`): the
  per-function-merged dedupe key is replaced. Pre-fix key:
  `(VulnType, source.line, sink.line, sink.function)` (set-based,
  first-wins). Post-fix key:
  `(file, sink.line, source.line, source.variable, vuln_type)`
  (HashMap with rank-based keep-best). When multiple findings
  collide on this tuple, the entry with the highest
  `sink_type_precedence` rank is retained.
- **vuln** (new helper `tldr_core::security::vuln::sink_type_precedence`):
  ranks `TaintSinkType` variants by specificity for collision
  resolution. Ordering (highest rank first):
  1. `SqlQuery` (110) — only SQL sink, isolated rank for clarity.
  2. `ShellExec` (100) — direct shell invocation.
  3. `CodeEval` (95) — `eval` family, return value exfiltratable.
  4. `CodeExec` (90) — `exec` family, no return value.
  5. `CodeCompile` (85) — produces a code object only later
     executed; least-specific of the Code* triple.
  6. `Deserialize` (80) — gadget-chain-dependent RCE.
  7. `HtmlOutput` (70) — XSS sink.
  8. `FileOpen` (60) — read-side path-traversal, dominant in
     real corpora; preferred when both file sinks match.
  9. `FileWrite` (50).
  10. `HttpRequest` (40) — SSRF.

  The CodeEval > CodeExec > CodeCompile sub-ordering is the
  load-bearing one: it determines which of the three CommandInjection
  findings survives the flask `cli.py:1023` collapse.
- **vuln** (test): two new regression-guard tests in
  `crates/tldr-core/src/security/vuln.rs` —
  `test_taint_finding_dedupe_eval_compile_collapses_to_one`
  (synthetic `eval(compile(f.read(), ...))` triple-sink pattern
  asserts exactly 1 CommandInjection finding remains, with
  `sink.sink_type == "CodeEval"`) and
  `test_taint_finding_dedupe_distinct_source_vars_kept` (boundary:
  two distinct source variables flowing into the same `os.system`
  sink line must remain as 2 separate findings — the dedupe key
  includes `source.variable`).

### Why `vuln_type` is part of the dedupe key

A single sink expression can simultaneously be detected as TWO
different `vuln_type`s. The canonical example is PHP
`file_get_contents($url)` — both a `PathTraversal` (`FileOpen`
sink) and an `Ssrf` (`HttpRequest` sink) for the same source
variable on the same line. These are ORTHOGONAL findings (different
remediation, different CWE, different risk class), and the
`vuln_migration_v1_red` suite asserts ≥1 finding of EACH type.
Collapsing across `vuln_type` would corrupt this signal.
Within-`vuln_type` collapse still solves the
CodeEval/CodeExec/CodeCompile case (all `CommandInjection`) and
the FileOpen/FileWrite case (both `PathTraversal`), which was the
originally reported bug.

### Architectural note

NO public API change. `VulnFinding` shape unchanged. CLI flags,
output keys, JSON shape unchanged. The fix is entirely internal to
`scan_file_vulns`'s merge phase. A previously emitted set of
duplicate findings is now collapsed to a single representative; no
new finding shapes are introduced.

### Validation

- `cargo test -p tldr-core --lib security::` — 125/125 GREEN
  (includes the two new dedupe tests + the M3.1 causal-ordering
  test).
- `cargo test -p tldr-cli --test vuln_migration_v1_red` —
  168/168 GREEN. (Note: an earlier draft of this fix that
  EXCLUDED `vuln_type` from the dedupe key broke
  `php_ssrf_positive` because PHP `file_get_contents` collapsed
  the SSRF and PathTraversal findings together. Restoring
  `vuln_type` to the key fixes it; spec's pre-warning was correct.)
- `cargo test -p tldr-cli --test vuln_migration_v1_composite_red` —
  1/1 GREEN.
- Binary verify on `/tmp/repos/flask/src`:
  - cli.py:1023: 4 → 2 findings; config.py:209: 2 → 1; cli.py:1022: 2 → 1.
  - Total: 7 → 4 (post-M3.1 baseline → post-M3.2).
- Binary verify on `/tmp/repos/ripgrep/crates`:
  - gitignore.rs:608: 4 → 2; dir.rs:933: 4 → 2; dir.rs:919: 4 → 2.
  - Total: 37 → 28 (post-M3.1 baseline → post-M3.2).
- `secure-taint-aggregator-v1` parity preserved on flask
  (`secure.taint_count == vuln.findings.length == 4`).

## taint-flow-causal-ordering-v1 — internal milestone

NOT a published release. Medium-severity bug fix in the canonical
taint engine: `compute_taint_with_tree` was emitting
causally-impossible flows where `source.line > sink.line`. In
dataflow analysis the source must execute BEFORE the sink that
consumes its value; a flow with the source line strictly greater
than the sink line cannot actually have flowed. Pre-fix repro:

- `tldr vuln /tmp/repos/flask/src 2>/dev/null | jq '[.findings[] | select(.taint_flow[1].line < .taint_flow[0].line)] | length'` → **2** (of 9 total)
- `tldr vuln --lang rust /tmp/repos/ripgrep/crates 2>/dev/null | jq '[.findings[] | select(.taint_flow[1].line < .taint_flow[0].line)] | length'` → **2** (of 43 total)

Concrete flask example: `config.py:208` has `with open(filename, mode="rb") as config_file:` (a `FileOpen` sink); `config.py:209` has `exec(compile(config_file.read(), ...))` where `config_file.read()` is correctly classified as an `UntrustedFileRead` source. The engine then paired source-line=209 with sink-line=208 — but on the call timeline the open precedes the read, so the read CANNOT have tainted the earlier open. Pairing was happening at flow construction time without checking causal ordering.

`vuln_migration_v1_red`: 168/168 stays GREEN. `secure-taint-aggregator-v1` parity preserved (`secure.taint_count == vuln.findings.length`, 7/7 on flask post-fix).

### Changed

- **taint engine** (`tldr_core::security::taint::compute_taint_with_tree`):
  at the flow-emission site, after the `direct || indirect` reachability
  check and the sanitizer check, an additional `causally_ordered =
  source.line <= sink_line` guard is required for the flow to be pushed
  to `result.flows`. Drop strategy chosen over swap-and-relabel because
  the source/sink type classifications are correct in isolation — only
  the pairing is spurious. The dropped class is narrow (2 of 9 on flask;
  2 of 43 on ripgrep) and consists exclusively of "FileOpen + later
  FileRead-as-source" call-chain inversions where the engine has
  already correctly identified BOTH endpoints, just paired them in the
  wrong direction. Swap-and-relabel would corrupt the
  source/sink-type metadata; drop preserves it.
- **vuln** (test): one new regression-guard test in
  `crates/tldr-core/src/security/vuln.rs` —
  `test_taint_flow_causal_ordering_open_then_read_no_inversion`
  reproduces the exact flask `config.py` shape (`with open(f) as cf:
  exec(compile(cf.read(), ...))`) and asserts every emitted flow
  satisfies `source.line <= sink.line`.

### Architectural note

NO public API change. `TaintFlow` shape unchanged. CLI flags,
output keys, JSON shape unchanged. The fix is entirely internal to
`compute_taint_with_tree`. Drop is conservative: a legitimate
"source defined inside a closure that runs after the sink" pattern
(rare in practice, e.g. lazy/deferred evaluation crossing
call-chain boundaries) would also be suppressed; this is acceptable
because the substring/AST-based engine cannot reliably distinguish
those from spurious label collisions, and the FALSE POSITIVE class
suppressed dominates in real corpora.

### Validation

- `cargo test -p tldr-core --lib security::` — 123/123 GREEN
  (includes the new test).
- `cargo test -p tldr-cli --test vuln_migration_v1_red` —
  168/168 GREEN.
- `cargo test -p tldr-cli --test vuln_migration_v1_composite_red` —
  1/1 GREEN.
- Binary verify on `/tmp/repos/flask/src`: inversions 2 → 0;
  total findings 9 → 7.
- Binary verify on `/tmp/repos/ripgrep/crates`: inversions 2 → 0.
- `secure-taint-aggregator-v1` parity preserved on flask
  (`secure.taint_count == vuln.findings.length == 7`).

## health-files-analyzed-counter-v1 — internal milestone

NOT a published release. Medium-severity bug fix in the `tldr health`
dashboard: `summary.files_analyzed` was always reported as `0` even
when `summary.functions_analyzed` and `summary.classes_analyzed`
were correctly populated (e.g., on `/tmp/repos/flask/src/flask`,
pre-fix output was `files_analyzed: 0, functions_analyzed: 311,
classes_analyzed: 53`). Root cause: `aggregate_summary` only reads
metrics from each sub-analyzer's `details` payload, and none of the
sub-analyzers (complexity, cohesion, dead code, martin, coupling,
clones) emit a `files_analyzed` field — so the counter was simply
never set. `vuln_migration_v1_red`: 168/168 stays GREEN.

### Changed

- **health** (`tldr_core::quality::health::run_health`): after
  `aggregate_summary`, `summary.files_analyzed` is populated by a
  new `count_source_files(path, detected_language)` helper which
  walks the input path with the canonical `ProjectWalker` and
  counts files whose extensions match `Language::extensions()` —
  the same source-of-truth used by `collect_module_infos` for dead
  code analysis and by `vuln`'s `files_scanned` counter. A file
  that fails to extract still counts as analyzed (the pipeline
  visited it), matching `vuln`'s semantics.
- **health** (test): two new tests in
  `crates/tldr-core/src/quality/health.rs` —
  `test_count_source_files_directory` (helper-level: 3 .py files
  among .txt/.cfg distractors must yield `count == 3`) and
  `test_run_health_files_analyzed_populated` (end-to-end:
  `run_health` on a 3-file Python directory must report
  `files_analyzed == 3`, guarding the regression).

### Architectural note

NO public API change. `HealthSummary` field shape unchanged
(`files_analyzed: usize` already existed; only its value
population is fixed). `aggregate_summary` is unchanged — the new
file-count source is layered atop it, not threaded through the
sub-analyzer details payloads (which would have required adding a
new field to multiple sub-analyzer outputs). CLI flags / output
keys / JSON shape unchanged.

### Validation

- `cargo test -p tldr-core --lib quality::health::tests` —
  16/16 GREEN (includes the two new tests).
- `cargo test -p tldr-cli --test vuln_migration_v1_red` —
  168/168 GREEN.
- Binary verify: `tldr health /tmp/repos/flask/src/flask | jq
  .summary.files_analyzed` returns `24` (matches the prior
  `tldr vuln` `files_scanned: 24` baseline).

## secure-taint-aggregator-v1 — internal milestone

NOT a published release. High-severity bug fix in the `tldr secure`
dashboard: `summary.taint_count` was reported as `0` while `tldr vuln`
on the SAME path reported N>0 findings (e.g., 9 on
`/tmp/repos/flask/src/flask`). Root cause: `tldr secure` ran a
legacy in-file substring matcher (`TAINT_SINKS` array of
`("cursor.execute", ...)` tuples + an f-string heuristic) that had
not been migrated to the canonical taint pipeline used by
`tldr vuln`. The substring matcher could not see source-to-sink
relationships and produced no findings on real Flask request →
sink flows. `vuln_migration_v1_red`: 168/168 stays GREEN.

### Changed

- **secure** (`commands/remaining/secure.rs`, `analyze_taint`):
  for non-Rust files, taint analysis now routes through
  `tldr_core::security::vuln::scan_vulnerabilities` — the same
  canonical pipeline `tldr vuln` uses — and projects each
  `VulnFinding` to a `SecureFinding` with `category = "taint"`,
  preserving severity, file, and line. The legacy `TAINT_SINKS`
  table, `analyze_fstring_injection`, `traverse_for_fstrings`, and
  `analyze_string_concat_in_sinks` helpers are removed (dead after
  the rewrite). Rust files retain the existing `unsafe { ... }`
  block scanner under the Taint sub-analysis (Rust-specific risk
  surface, semantically distinct from Python/JS taint flow).
- **secure** (test): the legacy
  `test_taint_analysis_finds_sql_injection` fixture
  (`cursor.execute(f"SELECT...{user_input}...")` with no taint
  source) was a false positive of the substring matcher. It is
  replaced with a Flask `request.args.get` → string-concat →
  `cursor.execute` flow that the canonical pipeline reports
  legitimately. A new parity test
  (`test_secure_taint_count_matches_vuln_findings`) asserts that
  `analyze_taint` returns exactly the same count as
  `scan_vulnerabilities` on the same fixture — guarding the
  aggregation contract.

### Architectural note

NO public API change. `SecureFinding` / `SecureSummary` /
`SecureReport` shapes unchanged. The Resources, Bounds, Contracts,
Behavioral, and Mutability sub-analyses are unchanged — only the
Taint dispatch is rewired. `summary.unsafe_blocks` (set under the
Taint analysis for Rust) and `summary.taint_critical` continue to
be derived from the Taint findings list. CLI flags unchanged.

### Validation

- `cargo test -p tldr-cli --lib commands::remaining::secure` —
  8/8 GREEN (includes the new parity test).
- `cargo test -p tldr-cli --test vuln_migration_v1_red` —
  168/168 GREEN.
- Binary verify: `tldr secure /tmp/repos/flask/src/flask` reports
  `summary.taint_count: 9`, exactly matching
  `tldr vuln /tmp/repos/flask/src/flask` `summary.total_findings: 9`.

## rust-format-sql-fp-narrowing-v1 — internal milestone

NOT a published release. Hardening of the `tldr vuln` Rust line-scanner
SqlInjection trigger. Closes a high-severity false-positive class
empirically reproed on `tldr vuln --lang rust /tmp/repos/ripgrep/crates`:
4 critical-severity (CWE-89) `SQL String Interpolation` findings on plain
`format!()` macros containing ZERO SQL keywords anywhere in the file
(bash/fish/powershell flag formatting via `char::from(...)` plus an
`err!` macro `Box::<...>::from(format!(...))`). Root cause: the legacy
`contains_sql_keyword` predicate uppercased the WHOLE line and
substring-matched against {SELECT, INSERT, UPDATE, DELETE, FROM, WHERE},
causing the substring `from(` (uppercased to `FROM(`) to spuriously
match the keyword `FROM`. `vuln_migration_v1_red` remains 168/168 GREEN.
The 6 pre-existing `test_analyze_rust_*` unit tests STAY GREEN
unchanged. Two new tests
(`vuln_format_sql_fp_narrowing_v1_test::rust_format_sql_no_keyword_fp`
and `vuln_format_sql_fp_narrowing_v1_test::rust_format_sql_keyword_positive`)
ship in this commit as RED guards (FP regression-guard + TP guard).

### Changed

- **vuln** (Rust, `analyze_rust_file` line scanner): the `format!(...)`
  SqlInjection trigger predicate is narrowed from "line contains a SQL
  keyword as substring" to "format-string literal contains a SQL
  keyword as a word". The new `format_string_contains_sql_keyword`
  helper (1) extracts the first `"..."` argument to the `format!(`
  call via a small character-walking parser that honors `\` escapes,
  and (2) applies an uppercase-substring check with word-boundary
  enforcement (adjacent bytes must be non-alphanumeric/non-underscore
  or string boundary) on the extracted literal. Lines without a
  string-literal first arg (e.g., the `err!` macro pass-through
  `format!($($tt)*)` in `crates/ignore/src/lib.rs`) yield `None` from
  the literal extractor and short-circuit to no-finding. The legacy
  six-keyword set is preserved verbatim ({SELECT, INSERT, UPDATE,
  DELETE, FROM, WHERE}); no keyword was added or removed. The
  `format!()` macro detection guard (the `trimmed.contains("format!(")`
  outer condition + the `{}` / `{` / `+` interpolation-shape
  conjunction) is unchanged. The CLI `format_string_contains_sql_keyword`
  call site is the only line-scanner edit.

### Architectural note

NO public API change. `VulnFinding` struct shape unchanged. The set of
emitted `VulnType` variants from `analyze_rust_file` is unchanged.
`is_rust_test_file` body unchanged. NO new `VulnType` /
`TaintSinkType` / `TaintSourceType` enum variants. NO new fields on
emitted findings. NO new CLI flag. The narrowing operates entirely
within the existing predicate path; `analyze_rust_file`'s body is
byte-for-byte unchanged except for the predicate-name swap on the
single guarded line. Two helper functions (`is_word_byte`,
`extract_first_format_string_literal`) and the rewritten
`format_string_contains_sql_keyword` predicate (~110 LOC including
docs) are added to the existing helper-functions block in `vuln.rs`.

### Trade-off explicitly accepted

This is a syntactic line-scanner predicate. A determined attacker can
still bypass it (e.g., `format!("{}{}", "SEL", "ECT * FROM ...")` —
keyword split across format args; or string concatenation that
assembles the SQL outside the `format!` literal). The canonical taint
pipeline (`crates/tldr-core/src/security/...`) handles those evasive
shapes via the `taint_flow` graph; the line-scanner predicate exists
only to gate the best-effort `format!`-shaped emission. The narrower
predicate is the right trade-off here: pre-fix the FP floor was
producing 4 critical-severity findings on a single popular open-source
crate (ripgrep) with ZERO SQL anywhere; the residual evasion shapes
are vanishingly rare in real-world Rust code and ARE caught by the
canonical pipeline when present.

### Retained

- `vuln_migration_v1_red`: 168/168 GREEN.
- 6 `test_analyze_rust_*` unit tests in
  `crates/tldr-cli/src/commands/remaining/vuln.rs::tests` GREEN
  (including `test_analyze_rust_detects_command_and_sql_patterns`
  which covers the TP `format!("SELECT * FROM users WHERE name =
  '{}'", name)` shape).
- `vuln_autodetect_tests`: 6/6 GREEN.
- `val011_vuln_typescript_autodetect_test`: 1/1 GREEN.
- Public API surface UNCHANGED.

### Quantification

| Metric                                                       | Pre-fix | Post-fix |
|--------------------------------------------------------------|---------|----------|
| `tldr vuln --lang rust /tmp/repos/ripgrep/crates` SQL findings | 4       | 0        |
| `vuln_migration_v1_red` test count                            | 168     | 168      |
| `vuln_migration_v1_red` GREEN                                 | 168     | 168      |
| `test_analyze_rust_*` unit tests GREEN                        | 6       | 6        |
| New RED guards (FP + TP)                                      | 0       | 2        |

### Standing rules upheld

- Internal-versioning posture honored: NO push, NO `cargo publish`, NO
  version bump.
- Local annotated tag only (`rust-format-sql-fp-narrowing-v1`).
- USER STANDING RULE: cargo publish requires explicit user
  authorization every time.
- NO `git stash` used; HEAD comparisons via
  `git show HEAD:path > /tmp/x && diff -u /tmp/x path` per the
  no-git-stash standing rule.
- NO destructive git operations.
- NO gaming: predicate is honestly narrower, not bypassed via
  `#[cfg(test)]` / `#[allow(...)]` / weakened assertion.

## vuln-autodetect-message-v1 — internal milestone

NOT a published release. UX hardening of the `tldr vuln` autodetect-
unsupported error message. Closes ZERO RED tests; this is a UX-clarity
hardening milestone that closes a misleading-message FP surfaced during
binary-verification of the prior 14 milestones. `vuln_migration_v1_red`
remains 168/168 GREEN.

### Changed

- **vuln** (autodetect error message): when the autodetected language
  lies outside the autodetect-by-extension set
  (Python/Rust/TypeScript/JavaScript), the error now points the user at
  `--lang <detected>` directly — the canonical taint pipeline DOES
  support all 17 languages via an explicit `--lang` flag (Go, Java,
  Cpp, C, CSharp, Ruby, Php, Kotlin, Swift, Scala, Elixir, Lua, Luau,
  Ocaml — every language with `LanguagePatterns` AST banks). Pre-M1
  message read "use --lang python, --lang rust, --lang typescript, or
  --lang javascript", implying ONLY those four were supported and
  steering Java/Ruby/Cpp/etc. users toward an unhelpful workaround.
  Post-M1 message includes the actionable `--lang <detected>` form
  AND retains the four-lang autodetect-routing list (which the
  `vuln_autodetect_tests` regression-guards assert on at L191-198).

### Architectural note

NO public API change. NO new error-type variant. NO new CLI flag. NO
change to `is_natively_analyzed` semantics. NO change to autodetect
extension routing in `is_supported_source_file`. Single source-file
edit (`crates/tldr-cli/src/commands/remaining/vuln.rs`); the message
literal is the only edit. The phrase "is not yet supported by
autodetect" is preserved verbatim per the
`test_vuln_errors_on_unsupported_autodetected_lang` regression-guard
at `vuln_autodetect_tests.rs:186-189`. The four-lang substring
(`--lang python` / `--lang rust` / `--lang typescript` /
`--lang javascript`) is retained per the same test's L191-198
assertion (any-of). The new actionable `--lang {detected}` guidance
is additive; existing tests pass unchanged.

### Retained

- All 6 `vuln_autodetect_tests` GREEN
  (`test_vuln_errors_on_unsupported_autodetected_lang`,
  `test_vuln_autodetects_python`, `test_vuln_autodetects_rust`,
  `test_vuln_no_detectable_lang_empty_dir`,
  `test_vuln_honors_explicit_lang_typescript`,
  `test_vuln_no_cap_on_large_repos`).
- `vuln_migration_v1_red`: 168/168 GREEN.
- `val011_vuln_typescript_autodetect_test`: 1/1 GREEN.
- Public API surface UNCHANGED.

### Standing rules upheld

- Internal-versioning posture honored: NO push, NO `cargo publish`, NO
  version bump.
- Local tag only (`vuln-autodetect-message-v1`).
- USER STANDING RULE: cargo publish requires explicit user
  authorization every time.

## rust-panic-suppression-v1 — internal milestone

NOT a published release. UX hardening of `tldr vuln` JSON output on
production Rust codebases. Closes ZERO RED tests; this is a HARDENING
milestone that closes the `rust-vuln-taint-pipeline-v1` R2 sub-elephant
(per-`.unwrap()` Panic flood on production Rust trees). The existing
`is_rust_test_file` mask only covered `/tests/`, `_test.rs`, and
`tests.rs` paths — every other `.unwrap()` in the codebase produced a
Medium-severity Panic finding regardless of context.
`vuln_migration_v1_red` remains 168/168 GREEN. The 6 pre-existing
`test_analyze_rust_*` unit tests STAY GREEN unchanged (they call
`analyze_rust_file` directly; the new gate is at the `VulnArgs::run`
filter-pipeline layer).

### Changed

- **vuln** (Rust, behavior on default invocation): per-`.unwrap()`
  Panic findings emitted by `analyze_rust_file`'s line scanner are now
  SUPPRESSED by default. The new `--include-smells` CLI flag (default
  `false`) restores the legacy emission set. Predicate is tight —
  `f.vuln_type == VulnType::Panic && f.title.starts_with("Potential
  Panic")` — bound to both the canonical `VulnType::Panic` enum
  variant AND the line scanner's emission title prefix
  (`"Potential Panic From unwrap()"`), so it cannot accidentally
  over-match a hypothetical future canonical-pipeline Panic finding
  with a different title. The 6 non-Panic triggers in
  `analyze_rust_file` (T1 UnsafeCode, T2/T3/T6 MemorySafety, T5
  SqlInjection, T7 CommandInjection) emit unconditionally regardless
  of `--include-smells`. Downstream consumers of the JSON output
  observe a finding-count drop on Rust trees with `.unwrap()`
  callsites outside `/tests/`-style paths; the per-finding JSON shape
  is unchanged (no schema delta).

### Architectural note

NO public API change. `VulnFinding` struct shape unchanged.
`analyze_rust_file` body and signature byte-for-byte unchanged.
`is_rust_test_file` body unchanged. NO new `VulnType` /
`TaintSinkType` / `TaintSourceType` enum variants. NO new fields on
emitted findings. The gate is a runtime-filtered CLI flag mirroring
the existing `include_informational` precedent (`VulnArgs::run` post-
analysis pipeline at the same filter layer), NOT a `#[cfg(test)]` /
`#[allow(...)]` suppression. `--include-smells=true` round-trips
through the filter and restores the legacy Panic emission count
verbatim. Single source-file edit
(`crates/tldr-cli/src/commands/remaining/vuln.rs`); the field
addition (~10 LOC), filter step (~12 LOC), `is_smell_finding` helper
(~16 LOC), and 2 new round-trip tests
(`test_vulnargs_run_default_suppresses_panic`,
`test_vulnargs_run_include_smells_emits_panic`, ~125 LOC including
helpers) ship atomically in a single commit.

### Known residual gaps (out of scope; documented carry-forward)

- The `analyze_rust_file` line scanner remains the sole `Panic`
  emitter; no taint-state cross-reference is performed. The
  long-term fix (Option D from `plan.md` §3 —
  `panic-taint-cross-ref-v1`) would emit Panic only when the
  unwrapped value originates from a tainted source; that requires a
  new `TaintSinkType::Panic` variant and threading taint state into
  `analyze_rust_file`. Out of scope for this milestone.
- The flag is a coarse single bool. If/when ≥3 smell-class triggers
  exist, migrate to a tier enum (Option B from `plan.md` §3 —
  `smells-level-tier-v1`).

These residual gaps are accepted in exchange for eliminating the
high-volume default-invocation Panic flood that cluttered downstream
JSON consumers.

## rust-wildcard-get-narrowing-v1 — internal milestone

NOT a published release. Precision narrowing of the over-broad
`RUST_AST_SINKS` HttpRequest member-access wildcards. Closes ZERO RED
tests; this is a HARDENING milestone that closes premortem `dab0766`
R8 (T2/E1 wildcard-get FP elephant) carried forward from
`rust-vuln-taint-pipeline-v1` M5. `vuln_migration_v1_red` remains
168/168 GREEN. Eliminates a 100% false-positive rate on synthetic
`HashMap::get` / `Vec::get` / `BTreeMap::get` callers passing tainted
arguments.

### Changed

- **vuln** (Rust): `RUST_AST_SINKS` HttpRequest `member_patterns` narrowed
  — the wildcard entries `("*", "get")` and `("*", "post")` (which fired
  on ANY `<receiver>.get(<tainted>)` / `.post(<tainted>)` member-access
  shape, including HashMap/Vec/BTreeMap/Option) are replaced with an
  explicit allowlist of HTTP-client receiver names: `client`, `agent`,
  `http`, `request_builder`, `req` — paired with `get`/`post` fields (10
  entries). `member_patterns_match` matches receiver NAME-text (not type),
  so this allowlist eliminates the 100% FP rate on collection-`.get(...)`
  callers measured at `rust-vuln-taint-pipeline-v1` M3 binary smoke (3/3
  synthetic FPs → 0/3 post-narrowing). Real-world idioms like
  `let client = reqwest::Client::new(); client.get(&url)` and
  `let agent = ureq::agent(); agent.post(&url)` continue to be detected
  via the new allowlist entries. Scoped-identifier raw-fallback paths
  (`reqwest::get`, `reqwest::Client`, `reqwest::blocking::get`,
  `reqwest::blocking::Client`, `ureq::get`, `ureq::post`, `hyper::Client`,
  `Url::parse`) are UNCHANGED. `rust_ssrf_positive`'s closure path
  (`reqwest::blocking::get(&u)`) uses the scoped-identifier raw-fallback
  and is untouched by this narrowing — STAYS GREEN.

### Architectural note

NO public API change. `AstSinkPattern` struct shape unchanged.
`VulnFinding` shape unchanged. `tldr_core::security::vuln::scan_vulnerabilities`
signature unchanged. NO new `VulnType` / `TaintSinkType` /
`TaintSourceType` variants. NO test modifications. The post-M2 match
universe is a STRICT SUBSET of the pre-M2 wildcard universe (additive
AND narrowing — no loosening). Single source-file edit
(`crates/tldr-core/src/security/taint.rs`); the 2-line wildcard removal,
10-line allowlist addition, and doc-comment update ship atomically in a
single commit.

### Known residual gaps (out of scope; documented carry-forward)

- HTTP clients bound to short variable names (e.g.,
  `let c = reqwest::Client::new(); c.get(&url)`) no longer trigger Ssrf
  detection on the member-access shape — receiver `"c"` is not in the
  allowlist.
- Composed-access HTTP calls (e.g., `self.client.get(&url)` inside
  methods) may not match — the receiver text is composed, not a single
  identifier in the allowlist.
- Custom-named HTTP clients (e.g., `let github = reqwest::Client::new();
  github.get(&url)`) require additional allowlist entries OR future
  type-aware receiver filtering.

These residual gaps are accepted in exchange for eliminating the
universal `.get(<tainted>)` false-positive class. A future
`rust-wildcard-receiver-type-aware-v1` milestone (not yet planned) can
layer tree-sitter type-walk inference atop the allowlist without
conflict.

### Quantification (synthetic binary smoke)

| Fixture                                          | Pre-M2 Ssrf | Post-M2 Ssrf | Verdict |
|--------------------------------------------------|-------------|--------------|---------|
| `m: HashMap; m.get(&tainted)`                    | 3           | 0            | FP eliminated |
| `v: Vec; v.get(tainted_idx)`                     | 2           | 0            | FP eliminated |
| `m: BTreeMap; m.get(&tainted)`                   | 3           | 0            | FP eliminated |
| `let client=reqwest::Client::new(); client.get(&u)` | 3        | 3            | TP preserved |
| `let agent=ureq::agent(); agent.post(&u)`        | 3           | 3            | TP preserved |
| `reqwest::blocking::get(&u)` (scoped-id)         | 3           | 3            | TP preserved (raw-fallback unchanged) |

FP rate: 100% → 0%. TP rate: 100% → 100%. Net +100 percentage-point
precision improvement on the `.get(<tainted>)` member-access FP class.

## cpp-deser-declaration-v1 — internal milestone

NOT a published release. Closes the LAST remaining carry-forward from
vuln-source-parity-v1 M5 Bucket B — Cpp subset
(`cpp_deserialization_positive`). `vuln_migration_v1_red` now 168/168
GREEN. Single source-file edit (`crates/tldr-core/src/security/taint.rs`)
extending `extract_first_identifier_arg_ast` with a Cpp `declaration`
entry arm and a forward-coverage Cpp branch in the per-language
`descend_kinds` match.

### Changed

- **taint var-extraction**: `extract_first_identifier_arg_ast` gains a
  Cpp arm placed before the generic args-list lookup. When
  `language == Language::Cpp && descendant.kind() == "declaration"`, the
  helper walks the descendant's named children for an `init_declarator`,
  resolves its `value` field to the `argument_list` node, and delegates
  to `extract_first_identifier_arg_ast_descent` (depth=0). tree-sitter-cpp
  0.23.4 parses
  `boost::archive::text_iarchive ia(std::stringstream(d) >> obj);` as
  `declaration → init_declarator { value: argument_list { binary_expression
  { left: call_expression(std::stringstream → identifier(d)) } } }`; the
  `declaration` node has no `arguments` field and `init_declarator` does
  not match `kind.contains("argument")`, so pre-M2 the generic args-list
  lookup returned `None` and the source/sink pair was silently dropped.
  The descent helper's per-level `string_node_kinds(language)` filter at
  every recursion step preserves the closes-#24 string-literal
  regression-guard by construction.

### Added

- **forward coverage**: Cpp branch added to the per-language
  `descend_kinds` match arm with
  `["binary_expression", "call_expression", "parenthesized_expression",
  "argument_list"]`. This is COSMETIC for `cpp_deserialization_positive`
  (whose flow short-circuits via the new entry arm before reaching the
  args-list lookup) but PROVIDES PROTECTION for future Cpp
  `call_expression` sinks whose first arg is a nested constructor /
  parenthesised / binary expression.

### Architectural note

NO public API change. NO new `TaintSinkType` / `TaintSourceType` /
`VulnType` variants. NO bank modifications (`CPP_AST_SINKS` already had
the `boost::archive::text_iarchive` Deserialize entry). NO test
modifications. The descent helper
(`extract_first_identifier_arg_ast_descent`) body is unchanged — still
unconditional BFS over named descendants with depth bound `MAX_DEPTH=5`
and per-level string-kind filter. The new Cpp arm in the OUTER helper
mirrors the BFS-style language-specific arms already present for PHP
echo / Ruby subshell / OCaml application_expression. Predecessor
milestone `var-extract-nested-constructor-v1` deferred Cpp scope per its
premortem amendment A1; this milestone closes that deferral. Pre-dispatch
discriminative premortem (commit `1c78826`) issued amendments A1
(documentation: descend_kinds match arm lives in OUTER helper) and A2
(fixture count correction: 13 deserialization-specific, 84 broader
`*_string_literal_fp` glob); both applied.

### Retained

- `extract_first_identifier_arg_ast_descent` body unchanged.
- `CPP_AST_SINKS` Deserialize entry unchanged.
- `member_patterns_match` / `field_access_info` / `extract_call_name_*`
  unchanged.
- All 167 currently-GREEN `vuln_migration_v1_red` tests at HEAD remain
  GREEN; the 1 RED transitions to GREEN (168/168 GREEN).
- All 13 `*/deserialization_string_literal_fp.*` fixtures yield 0
  findings post-merge (closes-#24 regression-guard preserved).
- All 80 scanned `*_string_literal_fp.*` fixtures across the broader 84
  glob yield 0 findings (4 luau fixtures skipped — luau ext not in
  `tldr vuln --lang` autodetect map).

## rust-vuln-taint-pipeline-v1 — internal milestone

NOT a published release. Closes 4 of the remaining carry-forwards from
vuln-source-parity-v1 M5 Bucket A — Rust subset
(`rust_command_injection_positive`, `rust_deserialization_positive`,
`rust_path_traversal_positive`, `rust_ssrf_positive`). Reframe C from
vuln-migration-v1 §0 closure. Atomic dispatch flip + dedupe helper +
SSRF bank patch (commit `8560ab9`).

### Changed

- **vuln**: Rust file analysis now runs the canonical taint pipeline
  alongside the legacy line scanner. `tldr vuln file.rs` emits canonical
  taint findings (`SqlInjection`, `Xss`, `CommandInjection`,
  `PathTraversal`, `Ssrf`, `Deserialization`) AND line-scanner smell
  findings (`UnsafeCode`, `MemorySafety`, `Panic`). Findings on the same
  `(line, vuln_type)` tuple are domain-deduped to a single entry;
  line-scanner-only smells (`UnsafeCode`, `MemorySafety`, `Panic`) are
  always preserved. Pre-M2, `analyze_file` at
  `crates/tldr-cli/src/commands/remaining/vuln.rs:368-370` short-circuited
  `.rs` files into `analyze_rust_file` exclusively, blocking the canonical
  `tldr_core::security::vuln::scan_vulnerabilities` pipeline. Post-M2,
  the dispatch is dual: canonical runs for ALL extensions (.rs included);
  the line scanner additionally runs on .rs and its overlapping
  `SqlInjection`/`CommandInjection` emissions are deduped by
  `dedupe_overlap` against canonical findings on the same `(line,
  vuln_type)`. The "Rust files emit smell findings only" implicit
  contract (Reframe C in vuln-migration-v1 §0) is retired.

### Added

- **taint banks**: `RUST_AST_SINKS` HttpRequest patterns extended with
  `("", "reqwest::blocking::get")` and `("", "reqwest::blocking::Client")`
  in `crates/tldr-core/src/security/taint.rs:2464-2491`. Required to close
  `rust_ssrf_positive` whose handler calls `reqwest::blocking::get(&u)`.
  `extract_call_name_rust` returns the full `scoped_identifier` text
  (`"reqwest::blocking::get"`) — same shape as the existing
  `("", "reqwest::get")` entries; matched via the raw-fallback path in
  `member_patterns_match` (empty-receiver → `descendant_text.contains`).

### Architectural note

Atomic-commit boundary: dispatch flip + `dedupe_overlap` helper + SSRF
bank patch + doc-comment retirement of the Reframe C carry-forward note
ship in a SINGLE commit. Splitting creates intermediate states with
regressions: (a) dispatch flip without bank patch leaves
`rust_ssrf_positive` RED; (b) bank patch without dispatch flip is dead
code unreachable for `.rs`; (c) dispatch flip without dedupe produces 2x
`CommandInjection` findings on overlapping lines.

### Carry-forwards (acknowledged, out of M2 scope)

- `rust-wildcard-get-narrowing-v1` (recommended follow-on): the
  `RUST_AST_SINKS` HttpRequest pattern `("*", "get")` becomes LIVE on
  `.rs` files post-dispatch-flip. M3-binary-smoke quantifies a 100% FP
  rate on synthetic non-HTTP-client `.get(<tainted>)` callers
  (`HashMap::get`, `Vec::get`, `BTreeMap::get`). Real-world impact on
  user Rust codebases is unmeasured but expected HIGH. Receiver-type-aware
  narrowing (only fire when receiver resolves to `reqwest::Client` /
  `reqwest::blocking::Client` / `ureq::Agent` / `ureq::Request`) is the
  recommended fix; deferred to preserve M2 atomic boundary.
- `rust-panic-suppression-v1` (recommended follow-on): `is_rust_test_file`
  at `vuln.rs:679-685` suppresses `Panic` findings on `/tests/` paths
  (which masks them on the 4 RED fixtures during verification) but
  production-code paths (`src/main.rs`, `src/lib.rs`, etc.) get every
  `.unwrap()` flagged. UX noise on real-world Rust codebases. A
  `--include-smells` flag or default-suppress-on-Info severity is the
  recommended fix.

### Retained

- `VulnFinding` struct shape unchanged.
- `map_core_vuln_type` exhaustive-match contract preserved (no `_` arm).
- `tldr_core::security::vuln::scan_vulnerabilities` signature unchanged.
- `analyze_rust_file` body unchanged (M2 modifies dispatch only).
- All 9 `#[test]` fns in `commands::remaining::vuln::tests` GREEN
  post-merge.
- All 4 `rust_*_string_literal_fp` regression-guards GREEN post-merge.
- `("*", "get")` / `("*", "post")` wildcard patterns retained as-is —
  narrowing deferred to follow-on (carry-forward documented above).

### Closes carry-forwards

- vuln-source-parity-v1 M5 Bucket A Rust subset (4 tests):
  `rust_command_injection_positive`, `rust_deserialization_positive`,
  `rust_path_traversal_positive`, `rust_ssrf_positive`. RED → GREEN.
  `vuln_migration_v1_red` count: 163/168 → 167/168 (+4 closures).

## ruby-backtick-extraction-v1 — internal milestone

NOT a published release. Closes 1 of the 6 remaining carry-forwards from
vuln-source-parity-v1 M5 Bucket A — Ruby subset
(`ruby_command_injection_positive`). Builds on
`var-extract-nested-constructor-v1` (commit `b577796`).

### Added

- AST dispatch arm in `detect_sinks_ast`
  (`crates/tldr-core/src/security/taint.rs`) for Ruby `subshell` nodes.
  tree-sitter-ruby 0.23.1 collapses both backtick `` `cmd` `` and
  `%x{cmd}` / `%x[cmd]` / `%x(cmd)` lexical forms onto the single
  `subshell` named-node kind (children: `interpolation` /
  `string_content` / `escape_sequence`). subshell is NOT call-shaped —
  `extract_call_name_ruby` returns `None` and the existing
  `for pattern in patterns.sinks` loop cannot match it. The new arm
  treats any `subshell` descendant in Ruby code as a `ShellExec` sink;
  var-extraction reuses
  `extract_first_identifier_arg_ast` (extended in this milestone — see
  Changed below) with a 3-fallback chain (extract_first_identifier_arg_ast
  → extract_assignment_rhs_ident → extract_source_var_from_statement).
  `TaintSink` is constructed with all 5 fields per the canonical site
  at `taint.rs:4456-4462` (var, line, sink_type: ShellExec,
  tainted: false, statement).
- Two new fixture pairs covering the `%x{...}` shape:
  `crates/tldr-cli/tests/fixtures/vuln_migration_v1/ruby/command_injection_percent_x_positive.rb`
  (asserts ≥1 command_injection finding) and
  `command_injection_percent_x_string_literal_fp.rb` (FP regression
  guard — asserts zero findings on a `%x{cmd}` mention inside a
  string literal). Locks both lexical forms into the test suite.

### Changed

- `extract_first_identifier_arg_ast`
  (`crates/tldr-core/src/security/taint.rs`) gained a Ruby-specific
  arm gated on `descendant.kind() == "subshell"`. The generic
  args-list path requires either `child_by_field_name("arguments")`
  OR a child whose kind contains `"argument"` or equals
  `"call_suffix"` — `subshell` has NEITHER. Without the extension the
  helper returns `None` and the new dispatch arm above would emit
  zero sinks. Implementation is BFS-over-named-descendants seeking
  the first non-self `identifier`'s text via `node_text` + 
  `is_valid_identifier`; skips `string_node_kinds(language)` subtrees
  defensively. Mirrors the PHP `echo_statement` BFS at
  `taint.rs:3954-3982` stylistically (NOT the OCaml
  `application_expression` flat 1-level scan).

### Architectural note

The dispatch arm is keyed on the tree-sitter-ruby `subshell` node-kind
directly, NOT via `call_node_kinds(Ruby)` extension. This isolates the
change to ShellExec sink detection and avoids polluting
`call_node_kinds` / `extract_call_name_ruby` consumers (sources,
sanitizers, `references.rs` is_call gate, `rr_baseline_per_language_test`).
Predecessor pattern reference: `field_access_info-extension-v1`
retained `\bgets\b` for the bare-call AST shape gap — same shape of
carry-forward (raw-substring/AST node-kind mismatch), different
localized resolution.

### Retained

- `call_node_kinds(Ruby)` unchanged (still `["call", "method_call"]`).
- `extract_call_name_ruby` unchanged (still matches
  `"call" | "method_call"`).
- `RUBY_AST_SINKS` unchanged (no new `AstSinkPattern` entry — the
  dispatch arm IS the entire matcher for subshell shapes; an entry
  would be silently dead).
- Public API unchanged.

### Closes carry-forwards

- vuln-source-parity-v1 M5 Bucket A Ruby subset:
  `ruby_command_injection_positive` — `\`#{cmd}\`` with
  `cmd = params[:cmd]` source. RED → GREEN.
  `vuln_migration_v1_red` count: 160/166 → 163/168 (closes 1
  carry-forward; +2 NEW tests, both GREEN).

### Deferred

- 5 remaining carry-forwards: 4 Rust (deserialization, command
  injection, path traversal, SSRF) and 1 Cpp (deserialization,
  deferred to `cpp-deser-declaration-v1` per
  `var-extract-nested-constructor-v1` premortem A1).

## var-extract-nested-constructor-v1 — internal milestone

NOT a published release. Closes 2 of the 3 carry-forwards from
vuln-source-parity-v1 M5 Bucket B (Java + Scala
`{java,scala}_deserialization_positive`); cpp DEFERRED to follow-on
milestone `cpp-deser-declaration-v1` per premortem amendment A1
(commit `88f5620`).

### Changed

- `extract_first_identifier_arg_ast`
  (`crates/tldr-core/src/security/taint.rs:3934`) now descends through
  nested constructor / call / instance-shaped first-argument nodes
  when the direct-identifier path fails. Per-language descend-through
  set:
  - Java: `{ object_creation_expression, method_invocation,
    parenthesized_expression }`
  - Scala: `{ call_expression, instance_expression, infix_expression }`
  - Cpp: NONE (deferred)
  Implementation is BFS-over-named-descendants with bounded recursion
  (depth 5) and `string_node_kinds(language)` filter applied at every
  level — closes-#24 string-literal regression-guard preserved at
  every recursion step. New private sub-helper
  `extract_first_identifier_arg_ast_descent` mirrors the BFS pattern
  previously used for PHP `echo_statement`
  (`taint.rs:3954-3982`); not OCaml `application_expression`
  (`taint.rs:3989-4016`) — that is a flat 1-level scan, not a BFS.

### Closes carry-forwards

- vuln-source-parity-v1 M5 Bucket B Java + Scala subset:
  - `java_deserialization_positive`
    (`new java.io.ObjectInputStream(new java.io.ByteArrayInputStream(d.getBytes()))`):
    BFS reaches inner `method_invocation` `d.getBytes()`,
    `split('.').next() = "d"` → identifier valid.
  - `scala_deserialization_positive`
    (`new java.io.ObjectInputStream(new java.io.ByteArrayInputStream(d.getBytes))`):
    sink fires on inner `instance_expression` via raw-substring
    fallback; BFS descends through nested `instance_expression` to
    reach `d.getBytes` → `"d"`.
  `vuln_migration_v1_red` red count drops from 8 to 6 (-2 delta).

### Deferred

- `cpp_deserialization_positive` deferred to follow-on milestone
  `cpp-deser-declaration-v1`. Premortem (commit `88f5620`) directly
  parsed `boost::archive::text_iarchive ia(std::stringstream(d) >> obj);`
  with tree-sitter-cpp v0.23.4 and REFUTED the `function_declarator`
  articulation. Actual shape:
  `declaration → init_declarator { declarator: identifier(ia), value:
  argument_list { binary_expression { left:
  call_expression(std::stringstream → identifier(d)), right:
  identifier(obj) } } }`. The helper invoked on `declaration` cannot
  navigate into the `init_declarator`'s `argument_list` because
  (a) `declaration` has no `arguments` field and
  (b) positional fallback's `kind.contains("argument") || kind == "call_suffix"`
  does not match `init_declarator`. A different fix-shape is required
  at the sink-detection level — out of M2 scope.

### Standing rules upheld

- NO public API change — `extract_first_identifier_arg_ast` signature
  unchanged; new sub-helper is private.
- NO new `TaintSourceType` / `TaintSinkType` / `VulnType` variants.
- NO new bank entries.
- NO modification of `call_node_kinds()`, `extract_call_name_*`,
  `member_patterns_match`, or `field_access_info`.
- Closes-#24 string-literal regression-guard preserved at every
  recursion level — verified via `*_string_literal_fp` test sweep
  (all GREEN, including `java_deserialization_string_literal_fp` and
  `scala_deserialization_string_literal_fp`).
- Bounded recursion (depth 5) prevents pathological deep-template /
  generic recursion.
- Local tag only (`var-extract-nested-constructor-v1`); no push, no
  publish, no version bump.

## vuln-source-parity-v1 — internal milestone

NOT a published release. Closes vuln-migration-v1 M3-CF-01 + M4-CF-01
carry-forward (32 of 33 RED positive tests across 15 languages).
Companion to workspace-test-infrastructure-v1 (parallel pre-publish
hygiene milestone). Both must land before single coherent external
cargo publish ships.

### Added

- 42 additive `AstSinkPattern` + `AstSourcePattern` entries across 16
  `LanguagePatterns` AST banks (M2, commit `f838387`):
  - C/Cpp: `SqlQuery` banks + Cpp `std::getenv` source qualifier +
    `std::fopen` sink qualifier
  - CSharp: `Response.Write` `HtmlOutput` + `Process.Start` FQN +
    `System.IO.File.Open` FQN + `JavaScriptSerializer` /
    `XmlSerializer` / `SoapFormatter` `Deserialize`
  - Elixir: bang-convention `SqlQuery` / `FileOpen` + `:os.cmd` /
    `System.shell` / `Port.open` `ShellExec`
  - Java: `new java.io.File` / `new java.io.ObjectInputStream` FQN
  - Lua/Luau: `:query(` colon-method `SqlQuery`
  - OCaml: `Mariadb.Stmt.execute` / `Postgresql.exec` / `Mysql.exec` /
    `Sqlite3.prepare` `SqlQuery`
  - Python: `response.write` / `Response.set_data` `HtmlOutput`
  - Ruby: `SqlQuery` NEW BANK
    (`ActiveRecord::Base.connection.execute`, `raw_sql`)
  - Scala: `scala.io.Source.fromFile` /
    `new java.io.ObjectInputStream` FQN
  - Swift: Vapor `request.query[` `HttpParam` + `executeQuery` /
    `prepareStatement` `SqlQuery` + `Process.launchedProcess` /
    `Process.run` `ShellExec` + `FileHandle(forReadingAtPath:`
    `FileOpen`
- 1 new `Deserialize` bank entry in `TYPESCRIPT_AST_SINKS` for
  `node-serialize.unserialize` (M4, commit `c9d75ab`)
- 0 new `TaintSourceType` / `TaintSinkType` / `VulnType` enum variants
  (purely bank-additive)

### Changed

- 4 entries in `TYPESCRIPT_AST_SINKS` reclassified from
  `TaintSinkType::FileWrite` to `TaintSinkType::HtmlOutput` (M3, commit
  `669b0f5`):
  - `(reply, send)` (Fastify)
  - `(res, send)` (NestJS Express-style)
  - `(response, send)` (NestJS Response-builder lowercase)
  - `(Response, send)` (NestJS Response-builder capitalized)
- 3 atomic test assertion updates at
  `crates/tldr-core/tests/rr_framework_integ_test.rs` shipped in same
  commit (premortem E1 BLOCKER + M1 pre-flight grep finding):
  - L168 `fastify_reply_send_reflected_via_compute_taint`:
    `TaintSinkType::FileWrite` → `TaintSinkType::HtmlOutput`
  - L248 `nestjs_res_send_reflected_via_compute_taint`: same
  - L301 `nestjs_response_builder_send_via_compute_taint`: same
    (lowercase `response.send` builder; M1 pre-flight grep surfaced
    this 3rd case the premortem missed)
- 2 fixture rewrites at
  `crates/tldr-cli/tests/fixtures/vuln_migration_v1/{javascript,typescript}/deserialization_positive.{js,ts}`:
  replaced `eval(d)` (CodeEval, not Deserialize) with
  `serialize.unserialize(d)` from `node-serialize` package (M4, commit
  `c9d75ab`)
- `(response, redirect)`, `(response, json)`, `(Response, redirect)`,
  `(Response, json)`, `(NextResponse, redirect)`, `(NextResponse, json)`
  PRESERVED as `FileWrite` (semantically navigation/JSON-emit, not Xss)

### Fixed

- `vuln_migration_v1_red` pass rate: 133/166 (80.1%) → **158/166
  (95.2%)** — +25 RED tests transitioned to GREEN
- Closes M3-CF-01 (32 source-bank-gap tests across 6 langs from
  vuln-migration-v1) AND M4-CF-01 (Python `res.send` XSS)
- Reclassification fixes `javascript_xss_positive` +
  `typescript_xss_positive` transitions to GREEN

### Carry-forwards documented (8 across 3 technical buckets)

- **Bucket A — M1-classified (5)**: 1 Ruby backtick + 4 Rust dispatch
  bypass
  - `ruby_command_injection_positive`: tree-sitter-ruby parses
    `` `#{cmd}` `` as `subshell` node, not `call_expression`. AST shape
    inexpressible without FP risk. Future
    `ruby-backtick-extraction-v1` follow-on adds `subshell` to
    `call_node_kinds(Ruby)`. Mirrors FAI-v1 `\bgets\b` carry-forward
    precedent.
  - `rust_{command_injection,deserialization,path_traversal,ssrf}_positive`:
    M1 empirical investigation falsified the planning hypothesis
    (`.unwrap()` chain extraction). Real root cause:
    `crates/tldr-cli/src/commands/remaining/vuln.rs:368-370`
    (`analyze_file`) routes `.rs` extension exclusively to
    `analyze_rust_file` (UnsafeCode/MemorySafety/Panic line scanner),
    bypassing `tldr_core::security::vuln::scan_vulnerabilities`.
    Reframe C from vuln-migration-v1 plan §0 confirmed. Future
    `rust-vuln-taint-pipeline-v1` follow-on designs how line-scanner
    findings interact with canonical taint findings.
- **Bucket B — M2-surfaced (3, NEW technical class)**:
  nested-constructor var-extraction
  - `cpp_deserialization_positive`: tree-sitter-cpp parsing variance on
    `boost::archive::text_iarchive` constructor declaration shape; bank
    entry exists but doesn't fire empirically.
  - `java_deserialization_positive`:
    `extract_first_identifier_arg_ast` returns `var=None` because first
    arg is `new java.io.ByteArrayInputStream(d.getBytes())` (nested
    constructor, not identifier). Var-extraction logic doesn't descend
    through `object_creation` / `new_expression` nodes.
  - `scala_deserialization_positive`: same root cause.
  - Future `var-extract-nested-constructor-v1` follow-on extends
    `extract_first_identifier_arg_ast` to descend through constructor
    argument nodes.
- **Aggregate count 8 exceeds plan's cap of 5** — documented per
  `validator_mandate.carry_forward_max_5` non-additive-resolution
  clause. Bucket B is technically distinct from Bucket A:
  var-extraction limitation, not bank parity or dispatch bypass.

### Retained

- All 83 string-literal regression-guard tests GREEN (closes-#24 root
  pattern preserved)
- All 36 `test_e2e_*` in `tldr-core/security/vuln.rs` GREEN (primary
  regression guard)
- All CLI integration tests (`vuln_autodetect` 6/6, `vuln_ssrf_test`
  3/3, `vuln_sarif_deserialization_test` 2/2, `composite_red` 1/1)
- Public API surface UNCHANGED: no new
  `TaintSourceType` / `TaintSinkType` / `VulnType` variants, no
  signature changes, JSON / SARIF output schema unchanged
- All M2 additive bank entries are PURELY ADDITIVE (no removal of
  existing entries — audit-verified at `M2-report.json`)

### Architectural notes

- This is a HYGIENE-class follow-on milestone (companion to
  workspace-test-infrastructure-v1) closing the source/sink-bank
  coverage gap that vuln-migration-v1 deferred to M3-CF-01 / M4-CF-01.
- Premortem caught E1 BLOCKER (`res.send` sink_type assertion mismatch
  — premortem found 2; M1 pre-flight grep added the 3rd at L301), E2
  (M2/M3/M4 must serialize on `taint.rs`), E3 (sink-addition undercount
  ~14→~22), RM-4 (BSD grep PCRE incompatibility). All 4 amended pre-/
  autonomous.
- M2 worker disclosed honest protocol slip: used `git stash` / pop once
  for diagnostic comparison (violated standing rule). No work lost.
  Same kind of slip the sanitizer-v1 M2 worker made earlier in the
  session. Documented for future reinforcement; cleaner approach is
  `git show HEAD:path > /tmp/x.rs` + diff.
- M2 surfaced 3 NEW carry-forwards (Bucket B) raising aggregate from 5
  to 8 — empirical reality outranking plan estimate. Documented
  honestly with non-additive-resolution rationale rather than gamed
  away.

### Standing rules upheld

- Internal-versioning posture honored: NO push, NO `cargo publish`, NO
  version bumps.
- The 8 internal tags + this one + workspace-test-infrastructure-v1
  sibling are local-only.
- Single coherent external `cargo publish` ships AFTER
  publish-operator's explicit authorization (USER STANDING RULE:
  `cargo publish` requires explicit authorization every time,
  regardless of any milestone PASS verdict or
  `pre-publish-binary-verification.json` artifact recommending
  publish).

### Future follow-on milestones queued

- `var-extract-nested-constructor-v1` — extend
  `extract_first_identifier_arg_ast` to descend through
  `object_creation` / `new_expression` nodes. Closes 3 carry-forwards
  (Bucket B). LOC estimate: +30-60.
- `rust-vuln-taint-pipeline-v1` — design how `analyze_rust_file`
  line-scanner interacts with `scan_vulnerabilities` taint pipeline
  for Rust. Closes 4 carry-forwards (Bucket A Rust subset). LOC
  estimate: TBD; design milestone first.
- `ruby-backtick-extraction-v1` — add Ruby tree-sitter `subshell` node
  kind to `call_node_kinds(Ruby)` OR add a new dispatch path that
  handles backtick `subshell` nodes as `ShellExec` sinks. Closes 1
  carry-forward (Bucket A Ruby subset). LOC estimate: +10-20.

## workspace-test-infrastructure-v1 — internal milestone

NOT a published release. Hygiene milestone — restores
`cargo test --workspace --features semantic` baseline (modulo 35
documented Cat-B carry-forwards owned by vuln-source-parity-v1 sibling
milestone). Penultimate milestone before external publish.

### Removed

- 162 obsolete CLI integration tests for subcommands archived in prior
  internal milestones (cfg, dfg, ssa, gvn, alias, dominators, live_vars,
  abstract_interp, arch, behavioral, bounds, diff_impact, equivalence,
  maintainability, mutability, purity, secrets — all moved to
  `crates/tldr-cli/src/commands/archived/` in earlier work; CLI test
  invocations had been left dangling). Whole-file deletions:
  `ssa_cli_tests.rs` (26 tests) + `gvn_cli_tests.rs` (9 tests). Surgical
  per-test deletions: 127 tests across 8 mixed files
  (`cli_graph_tests.rs`, `cli_patterns_contracts_tests.rs`,
  `cli_remaining_tests.rs`, `cli_tests.rs`, `contracts_test.rs`,
  `p2_multilang_tests.rs`, `patterns_test.rs`, `remaining_test.rs`).
  Modern equivalents (`taint`, `slice`, `whatbreaks`, `references`,
  `dead`, `hubs`, etc.) retain full active test coverage. M3 commit
  `cf0b2be`.
- 8 obsolete DELETE-on-stale Cat-C tests in M4: 2
  `test_*_returns_unsupported` for Kotlin/Swift in
  `language_parity_test.rs` (both languages now SUPPORTED via
  `tree_sitter_kotlin_ng` + `tree_sitter_swift`); replacement
  parse-success tests already exist (`parser.rs:420-432`). Plus 6 other
  DELETE-on-stale entries documented in
  `reports/M4-fix-by-fix-capture.json`.

### Fixed

- 4 doctest failures in `tldr-core` (M2, commit `d17a24c`):
  - `callgraph::cross_file_types::FuncIndexProxy` doctest rewritten to
    use `FuncIndexProxyMut` (working impl) instead of `FuncIndexProxy`
    (`unimplemented!()` stub at L1109)
  - `callgraph::languages::kotlin::KotlinHandler::parse_import_node` and
    `callgraph::languages::luau::LuauHandler::extract_aliased_require`:
    bare ` ``` ` → ` ```text ` fence (rustdoc renders pseudo-grammar
    block as preformatted text, not Rust source)
  - `surface::triggers::extract_name_triggers`: stale
    `tldr_core::contracts::triggers::...` import path →
    `tldr_core::surface::triggers::...` (function lives in `surface/`,
    not `contracts/`)
- 38 Cat-C orthogonal-real test failures across `tldr-core` (M4, commit
  `68058a5`):
  - Empty-directory tree fixture gap
    (`crates/tldr-core/tests/fixtures/empty-dir/.gitkeep` created)
  - Stale Ruby-unsupported assertion in
    `test_surface_unsupported_language_errors` (Ruby IS supported per
    `surface/mod.rs:90-118`; changed to genuinely-unsupported language)
  - `git_log` no-commits-yet handling: returns `Ok(String::new())` on
    `does not have any commits yet` stderr (was bubbling as `Err`)
  - Cognitive-complexity else-clause SonarQube-spec alignment:
    `if x: return 1; else: return -1` cognitive == 1 (only `if` adds;
    else does NOT)
  - Empty-input handling: `analyze_dead_code`,
    `compute_martin_metrics`, `parse_coverage` return
    `Ok(<empty Report>)` instead of `Err`
  - Cobertura/lcov coverage parser regression: parsers no longer filter
    on filename suffix when format hint is explicit
  - Similarity-threshold fixture distinctness for
    `test_find_similar_no_clones` (rewrote fixture functions to fall
    below 0.8 threshold; assertion preserved)
  - Change-impact `NoBaseline` error reason includes `origin/<branch>`
    substring as UX hint when only-remote-tracking-ref-exists
  - Change-impact CLI test fixture git-init helper added
  - Plus 22 test-fixture corrections across various tests (numeric
    drift in expected values, schema field updates, etc.)

### Retained

- ALL active-subcommand CLI integration tests (every test invoking
  variants in `main.rs` `Subcommand` enum: `Tree`, `Structure`,
  `Calls`, `Impact`, `Dead`, `Hubs`, `Whatbreaks`, `Slice`, `Chop`,
  `Taint`, `Resources`, `Vuln`, `ApiCheck`, `Patterns`, `Inheritance`,
  `Deps`, `Cohesion`, `Coupling`, `Contracts`, `Specs`, `Invariants`,
  `Verify`, `Interface`, `Diagnostics`, `Doctor`, `ChangeImpact`,
  `Coverage`, `Search`, `Semantic`, `Similar`, `Context`, `Definition`,
  `References`, `Explain`, `Todo`, `Diff`, `Embed`, `Daemon*`, `Warm`,
  `Cache*`, `Loc`, `Complexity`, `Cognitive`, `Halstead`, `Churn`,
  `Debt`, `Health`, `Hotspots`, `Clones`, `Dice`, `Smells`, `Imports`,
  `Importers`, `Extract`, `Temporal`, `ReachingDefs`, `Available`,
  `DeadStores`).
- ALL `tldr search ...` invocations — `search` is the ACTIVE
  SmartSearch CLI alias per `#[command(name = "search")]` at
  `main.rs:141-142`, NOT archived.
- 3 false-positives from M1 enumeration explicitly preserved (M3 commit
  body documents): `test_debt_category_maintainability` (uses
  `--category maintainability` as VALUE for active `debt`),
  `test_explain_json_schema` (`purity` is JSON schema FIELD in active
  `explain` response), `test_api_check_no_findings_clean_code` (body
  invokes only active `api-check`).
- ALL `test_e2e_*` vuln tests at
  `crates/tldr-core/src/security/vuln.rs:1568-2100` (regression guard).
- ALL daemon tests, semantic / fastembed / embedding tests,
  non-archived `tldr-core` library tests.
- Public API surface UNCHANGED: `Subcommand` enum preserved,
  JSON / SARIF / text output schemas unchanged, exit codes unchanged,
  help text unchanged.

### Carry-forwards documented

- 35 Cat-B failures owned by `vuln-source-parity-v1` sibling milestone:
  - 33 originals from vuln-migration-v1 M3-CF-01 + M4-CF-01
    (source-bank gaps across Go/Java/CSharp/Scala/Lua/Elixir × multiple
    vuln types)
  - +1 reclassified by Option A: `test_vuln_detects_xss` (Python Flask
    f-string return → `HtmlOutput` sink coverage gap; vuln-migration-v1
    M2/M3 didn't cover f-string-return-from-view-function shape;
    absorbed into vuln-source-parity-v1 as Python scope expansion)
  - +1 reclassified by Option A:
    `ruby_io_popen_with_user_input_via_compute_taint` (documented FAI-v1
    M5 bare-`gets` carry-forward; tree-sitter-ruby parses bare `gets` as
    identifier (not call); regex `\bgets\b` retained in
    `RUBY_PATTERNS.sources` as Option A; `analyze_ast_only` test
    harness short-circuits regex bank, so test fails by design; future
    `ruby-bare-call-extraction-v1` follow-on can close it)

### Test infrastructure baseline restored

- `cargo test --workspace --features semantic --no-fail-fast --release`:
  35 failures EXACTLY (all Cat-B vuln-source-parity-v1 carry-forwards)
- `cargo test --workspace --features semantic --doc --no-fail-fast`: 0
  failures
- `cargo build --workspace --tests --features semantic`: exit 0
- `cargo clippy --workspace --tests --features semantic -- -D warnings`:
  exit 0
- After this milestone + vuln-source-parity-v1 (sibling) both land, the
  pre-publish baseline is fully restored.

### Architectural notes

- This is a HYGIENE milestone — no new features, no new test coverage,
  no public API changes.
- The single coherent external `cargo publish` (closing #7, #23, #24,
  #27, #28 + `tldr vuln` FP class + sanitizer correctness) is gated on
  this milestone + vuln-source-parity-v1 both landing.
- Premortem caught 1 critical blocker (search-vs-SmartSearch
  disambiguation) + 2 strengthening conditions (enumeration authority,
  mixed-file per-test delete). All 3 amended pre-/autonomous.
- M3 worker recovered honestly from a script bug mid-flow: first
  deletion-script attempt mishandled raw-string state across lines
  (`r#"..."#` containing brace chars). Working tree was restored by
  user (orchestrator authorization). Corrected script properly
  tokenizes raw-string state with N-hash matching across line
  boundaries.

### Standing rules upheld

- Internal-versioning posture honored: NO push, NO `cargo publish`, NO
  version bumps.
- The 5 internal tags + this one (workspace-test-infrastructure-v1) +
  vuln-source-parity-v1 (sibling, in progress) are local-only.
- Single coherent external publish ships AFTER both pre-publish
  milestones land + publish-operator confirms
  `pre-publish-binary-verification.json` (vuln-v1 M6 artifact) verdict.

## vuln-migration-v1 — internal milestone

NOT a published release. Internal-versioning posture: external `cargo publish`
deferred until pre-publish binary verification confirms no regressions.
This is the FINAL internal milestone — after publish-operator confirms
the pre-publish-binary-verification.json artifact, single coherent
external `cargo publish` ships.

### Changed

- `tldr vuln` command now routes through canonical `compute_taint_with_tree`
  for all 16 supported languages (was: per-language substring scanner in
  `tldr-core/security/vuln.rs` for 14 languages + CLI-local tree-sitter
  `TaintTracker` for Python). Per-function dispatch via
  `extract_functions_detailed`. Mirrors the proven pattern at
  `tldr-cli/commands/taint.rs:128`.
- M3 collapsed core `vuln.rs::scan_file_vulns` from substring 2-pass scanner
  to per-function `compute_taint_with_tree` loop. ~1000 LOC deleted.
- M4 collapsed CLI `remaining/vuln.rs::analyze_python_file` (~700 LOC
  TaintTracker + 9 recursive helpers) onto canonical. Python now routes
  through canonical AST path uniformly with all 15 other languages.

### Added

- 4 ADDITIVE `TaintSinkType` variants at `taint.rs:153`: `HtmlOutput` (Xss),
  `FileOpen` (PathTraversal — distinct from existing `FileWrite`),
  `HttpRequest` (Ssrf), `Deserialize` (untrusted-data deserialization).
  Existing 6 variants preserved verbatim.
- ~163 `AstSinkPattern` entries (41 entries' worth of distinct patterns)
  across all 16 `LanguagePatterns` banks for the 4 new VulnTypes (M2).
  Source-of-truth: `vuln.rs`'s per-language sink tables.
- M3 added `vuln_type_from_sink(TaintSinkType) -> VulnType` projection
  helper (canonical → user-facing VulnType ontology),
  `severity_for(VulnType) -> &'static str`,
  `descriptions_for(TaintSourceType, Language) -> &'static str` (R6
  mitigation: preserves descriptive `"Flask request.args (GET parameters)"`-
  style strings).
- M3 added `From<canonical::TaintSource> for vuln::TaintSource` and
  `From<canonical::TaintSink> for vuln::TaintSink` impls. The vuln-output
  adapter structs are populated from canonical engine output via these
  projections.
- M3 extended `extract_first_identifier_arg_ast` to handle PHP
  `echo_statement` / `print_intrinsic` node kinds — closes M2 carry-forward
  (PHP echo sink-emission var-extraction).
- M3 added SSA-active-path indirect-match fallback in
  `compute_taint_with_tree` Phase 5, gated by `!sink_var_is_ssa_tracked`
  to handle free-variable receivers (e.g.,
  `cursor.execute(f"...{tainted}")`) without breaking val001b
  sanitizer-reassignment correctness.
- M3 extended `tldr-core/src/ast/extract.rs::extract_functions_detailed`
  and `extract_classes_detailed` from `fn` to `pub(crate)` so
  `scan_file_vulns` can call them.
  `tldr-core/src/cfg/extractor::extract_cfg_from_tree` and
  `tldr-core/src/dfg/extractor::extract_dfg_from_tree` similarly extended.
  New `extract_dfg_from_tree_with_cfg` perf helper avoids redundant CFG
  re-parse.
- M3 added AST source-bank entries for `argv[`, `CommandLine.arguments`,
  `Request.Query[`, `queryParameters[`, `request.getQueryString`,
  `ngx.req.get_uri_args`, `conn.params[` across 8 languages — partial
  closure of M3-CF-01 source-bank-gap class.

### Removed

- Core `tldr-core/security/vuln.rs`: `get_sources` (per-language source
  tables, L140-L286), `get_sinks` (per-language sink tables, L290-L780),
  8 inline-propagation/sanitization helpers (`extract_assigned_variable`,
  `extract_propagation`, `is_type_coerced`, `is_sanitized_sink`,
  `is_sanitized_sql`, `is_sanitized_command`, `has_named_param`,
  `get_line_at`), ~22 obsolete unit tests at L1322-L2077. ~1000 LOC total.
- CLI `tldr-cli/src/commands/remaining/vuln.rs`: `TaintSource`
  const-pattern struct + `PYTHON_SOURCES` (~30 entries), `TaintSink`
  const-pattern struct + `PYTHON_SINKS` (~25 entries), `TaintTracker`
  struct + impl, `TaintInfo` CLI-local struct, `analyze_python_file` + 9
  recursive helpers (~700 LOC), 5 is/find helpers (`is_taint_source`,
  `is_taint_sink`, `is_parameterized_query`,
  `is_string_interpolation_tainted`, `find_taint_in_string`,
  `get_python_parser`, `node_text`), 4 obsolete unit tests,
  `tree_sitter::{Node, Parser}` import, `MAX_TAINT_DEPTH` const. ~984 LOC
  total.

### Retained

- **Public API preserved at canonical signatures:** `compute_taint`,
  `compute_taint_with_tree`, `detect_sanitizer_ast`, `scan_vulnerabilities`,
  `tldr vuln` CLI clap args, JSON/SARIF output schema, exit-code-2-on-
  findings behavior.
- **`tldr_core::security::vuln::TaintSource`** (`vuln.rs:68`) and
  **`tldr_core::security::vuln::TaintSink`** (`vuln.rs:81`) — RETAINED as
  output adapter structs with their existing String-typed fields. CLI
  consumer at `remaining/vuln.rs:679-688` reads
  `f.source.line/expression/source_type` and
  `f.sink.line/expression/sink_type` unchanged. `From<canonical>` impls
  project enum-typed canonical → string-typed adapter.
- `VulnType` enum, `VulnFinding`/`VulnSummary`/`VulnReport` output records
  (user-facing ontology preserved exactly).
- `get_remediation`, `get_cwe_id`, `vuln_type_name` (used by SARIF
  `generate_sarif` for `rules.name` + `shortDescription.text` —
  M4-DEVIATION-01 honored).
- **`analyze_rust_file` Rust line-scanner + 7 `rust_finding` helpers** —
  distinct concern (UnsafeCode/MemorySafety/Panic), not taint flow. Per
  Reframe C, permanently out of scope for taint-flow migration.
- **All 30 `test_e2e_*` tests at `vuln.rs:1568-2100`** — primary regression
  guard, ALL preserved + GREEN throughout M3+M4+M5.
- **All CLI integration tests:** `vuln_autodetect_tests.rs` (6/6),
  `vuln_ssrf_test.rs` (3/3), `vuln_sarif_deserialization_test.rs` (2/2).
- Output formatting: `build_summary`, `format_vuln_text`, `generate_sarif`.

### Issues closed (binary-verified)

- **closes-#24 string-literal substring FP class CLOSED end-to-end** at the
  `tldr vuln` command path — the half left open by regex-removal-v1,
  field_access_info-extension-v1, and sanitizer-removal-v1, all of which
  only reached the `tldr taint` command path.
- **83/83 string-literal regression-guard fixture corpus → 0 findings**
  (closes-#24 root mandate met across 16 langs × ~6 vuln categories).
- Original FP repros from Phase-1 investigation:
  - `tldr vuln /tmp/vuln-mig-repro/string_literal_fp.go --lang go` → 0
    findings (was 3 FP CommandInjections at HEAD)
  - `tldr vuln /tmp/vuln-mig-repro/fp2.ts --lang typescript` → 0 findings
    (was 1 FP citing comment line as sink)
  - `tldr vuln /tmp/vuln-mig-repro/string_literal_fp.py --lang python` → 0
    findings (Python FP-clean property preserved post-canonical-collapse)
- Composite multi-pattern FP fixture (all 6 source-pattern strings inside
  string literals + all 6 sink-pattern strings inside comments) → 0
  findings.

### Architectural notes

- **This is the FINAL internal milestone before external publish.**
  Together with regex-removal-v1, field_access_info-extension-v1, and
  sanitizer-removal-v1, the canonical `tldr-core/security/taint.rs` is now
  the **SINGLE SOURCE OF TRUTH** for taint flow detection across both
  `tldr taint` and `tldr vuln`.
- Regex-driven dispatch is fully eliminated for sources, sinks, AND
  sanitizers across the canonical pipeline. The remaining regex (Ruby
  `\bgets\b` from FAI-v1 carry-forward) is a single AST-shape carry-
  forward exception.
- Per Reframe C: `analyze_rust_file` Rust line-scanner remains distinct
  from taint flow detection. It detects Rust-IDIOMATIC smells
  (UnsafeCode/MemorySafety/Panic), not source-to-sink propagation. A
  future `rust-smell-detector-canonical-v1` follow-on would migrate it if
  a canonical smell-detector framework is built; not part of
  vuln-migration-v1.
- **Premortem caught 3 hard blockers pre-/autonomous:** T1
  (`test_taint_sink_type_variants` assertion update), T2 (vuln structs
  DELETE-vs-READ contradiction), T3 (fictional `build_codemap()`
  reference). All 3 amended; pattern continued to add value.

### Carry-forwards documented

- **M3-CF-01 (32 source-bank-gap positive RED tests):** 32 of 166 M1 RED
  positive fixtures STILL RED post-M5 across 6 languages
  (Go/Java/CSharp/Scala/Lua/Elixir × multiple vuln types). M2 audited
  sinks only; canonical AST source banks lack patterns `vuln.rs`'s
  `get_sources` had per-vuln-type. M3 added partial coverage
  (argv/`CommandLine.arguments`/etc. across 8 langs); full parity deferred
  to **`vuln-source-parity-v1`** future internal milestone. Does NOT
  affect closes-#24 (string-literal FP) closure — that's a separate class
  fully addressed.
- **M3-CF-02 (perf two-axis gate):** Avg 17.18× M1 baseline; p99-file
  5.24×. Per-file and per-function rayon parallelization applied (7×
  inner speedup). The M1 baseline (36.67ms avg / 34ms p99) was
  binary-startup-dominated; absolute scanning work is ~33ms/file on the
  20-file Go corpus. Pragmatically acceptable; M1 perf-baseline
  methodology should be revisited in future milestones.
- **M4-CF-01 (`python_xss_positive` still RED):** Fixture uses
  `response.write('<h1>'+name+'</h1>')`; canonical Xss sink bank lacks
  `response.write` (pre-M4 `PYTHON_SINKS` also lacked it). Same
  disposition as M3-CF-01.
- **M4-DEVIATION-01 (`vuln_type_name` retained):** M1 enumeration listed
  it for deletion but `generate_sarif` uses it for SARIF `rules.name` +
  `shortDescription.text`. Output-shape preservation precedence;
  documented.

### Standing rules upheld

- **Internal-versioning posture honored:** NO push, NO `cargo publish`, NO
  version bumps. Pre-publish binary verification artifact (4 checks)
  emitted as operator-handoff for the eventual external publish gate.
- After publish-operator confirms `pre-publish-binary-verification.json`
  verdict, single coherent external `cargo publish` closes #7 (callgraph),
  #23 (Rust trait FuncDef), #24 (string-literal substring FP, ALL paths),
  #27 (cache cross-contamination), #28 (daemon language threading) +
  `tldr vuln` FP class + sanitizer correctness in one release.

## sanitizer-removal-v1 — internal milestone

NOT a published release. Internal-versioning posture: external `cargo publish`
deferred until ALL anti-product surfaces close end-to-end.

This milestone closes the "tainted-forever tiger" carry-forward from
regex-removal-v1 W3 T1: sanitizer dispatch is now regex-free across all 16
supported languages. The W2-pre `detect_sanitizer_ast` per-line helper (dead
code at HEAD `db8f2bd`'s parent) is now wired through the worklist via a new
`build_sanitizer_ast_index` WALK-ONCE-INDEX-BY-LINE helper consumed by both
`process_block` and `ssa_propagate`. The 30 regex sanitizer `Vec` entries
across 16 `*_PATTERNS` banks are deleted, dispatch is flipped from
AST-FIRST-WITH-REGEX-FALLBACK to AST-only at both worklist sites, and the #24
string-literal substring FP closure (originally delivered for sources/sinks
in regex-removal-v1) is generalized to sanitizers.

### Changed

- **Sanitizer detection is now AST-only** via the new
  `build_sanitizer_ast_index` (M2-added WALK-ONCE-INDEX-BY-LINE helper)
  consumed by both `process_block` and `ssa_propagate`.
  `detect_sanitizer_ast` (was dead code at `taint.rs:3490`) is preserved
  as the per-line public API; the worklist consumes the index instead.
- **M2 extended `process_block` signature + `SsaPropagateCtx` struct** to
  thread the index through (private API only).
- **M4 flipped dispatch** from AST-FIRST-WITH-REGEX-FALLBACK to AST-only at
  both `process_block` (~L4109) and `ssa_propagate` (~L4358).
- **M4 added `mask_string_literal_descendants`** helper inside
  `build_sanitizer_ast_index` to address M3-FIND-01 — masks string-literal
  descendant byte ranges with ASCII spaces in a copy of the descendant text
  before passing to `member_patterns_match`'s raw-substring fallback. Closes
  a latent collision class for 13 langs that use raw-substring sanitizer
  entries (Rust, Ruby, Elixir, etc.).
- **M1 extended `AST_ONLY_TEST_MODE` thread-local check** by 3 LOC at
  `taint.rs:1096` to also short-circuit `detect_sanitizer`. The
  `AstOnlyTestModeGuard` (added in field_access_info-extension-v1 M1,
  commit `49ed30c`) now uniformly short-circuits sources, sinks, AND
  sanitizers.

### Added

- **M3 added 2 raw-fallback parity entries:**
  - `TYPESCRIPT_AST_SANITIZERS`: `("*", "parse")` + `("*", "safeParse")`
    `Numeric` (Zod-style schema validation; was regex-only).
  - `CPP_AST_SANITIZERS`: moved `std::stoi` and `static_cast<int>` to
    `call_names` (verified `extract_call_name_c` returns the exact
    strings). Restricts to `call_expression` descendants only — string
    literals are structurally excluded; resolves M2-FIND-01 string-literal
    regression introduced when wiring activated.

### Removed

- **30 regex sanitizer Vec entries** across 16 `*_PATTERNS` `lazy_static`
  banks (Python ×3, TS ×3, Go ×2, Java ×2, Rust ×1, C ×2, Cpp ×2, Ruby ×2,
  Kotlin ×1, Swift ×2, CSharp ×2, Scala ×2, PHP ×3, Lua ×1, Elixir ×2,
  OCaml ×1).
- **24 obsolete unit tests** across 2 files:
  - `crates/tldr-core/src/security/taint_tests.rs`: 17
    `test_<lang>_detect_sanitizers` (typescript, javascript, go, java,
    rust, c, cpp, ruby, kotlin, swift, csharp, scala, php, lua, luau,
    elixir, ocaml) + 3 Python-named-shape sanitizer tests
    (`test_int_sanitizes_sql_injection`,
    `test_shlex_quote_sanitizes_command_injection`,
    `test_html_escape_sanitizes_xss`).
  - `crates/tldr-core/tests/security_tests.rs`: 4
    `test_detect_sanitizer_*` tests (`python_int`, `python_shlex`,
    `python_html_escape`, `typescript`).
- **M4 removed unused params** (`statements`, `language`) from
  `process_block` and `SsaPropagateCtx` post-dispatch-flip — genuinely no
  longer needed.

### Retained

- **Public API preserved as no-ops:** `detect_sanitizer` (regex),
  `is_sanitizer`, `find_sanitizers_in_statement` — all iterate the now-empty
  `patterns.sanitizers` Vec; behavior change is `None`/`false`/empty Vec but
  signatures unchanged. Signature preservation maintains backward
  compatibility for any external caller; deletion deferred to a future
  `patterns-shell-cleanup-v1` milestone.
- **All 16 `LanguagePatterns` struct shells** (`sources`/`sinks`/
  `sanitizers` all empty Vecs) — preserves rollback margin; cleanup
  deferred.
- **`detect_sanitizer_ast` per-line public API** at `taint.rs:3490` — kept
  alongside the new walk-once index helper for external callers.
- **Compute-taint level sanitizer tests in both files** (e.g.,
  `test_sanitizer_removes_taint`, `test_no_vulnerability_when_sanitized`,
  `test_compute_taint_sanitizer_removes_taint`,
  `test_sanitizer_type_serialization`).

### Issues closed (binary-verified)

- **"Tainted-forever tiger" carry-forward from regex-removal-v1 W3 T1:**
  closed. Sanitizer dispatch is now regex-free across all 16 languages.
- **Generalized #24 string-literal substring FP closure to sanitizers:**
  14 `*_in_string_literal_does_not_sanitize` regression-guards transitioned
  RED→GREEN. Binary-verified ZERO findings on string-literal fixtures
  across Python/TS/Ruby/Rust at `/tmp/v041-verify/`.
- **Positive control verified:** real sanitizer call (e.g., Python
  `safe = int(raw)`) breaks flow correctly (UserInput source + CodeEval
  sink detected, ZERO vulnerabilities).

### Architectural notes

- **NO source change** to `field_access_info`, `extract_call_name_*`
  helpers, or `member_patterns_match` (validator mandates honored). M2's
  wiring lives entirely in new private helpers + private struct extensions.
- **The `mask_string_literal_descendants` helper** is a localized fix to
  the AST raw-substring fallback collision class — operates on a copied
  byte buffer, doesn't change the `member_patterns_match` matcher itself,
  and is contained inside `build_sanitizer_ast_index`.
- **Premortem caught 3 hard blockers pre-/autonomous:** M3 reframed
  parity-fill→parity-audit, M4 obsolete-test enumeration expanded
  13-16→24, M1 RED harness API reference fixed. Discipline pattern:
  discriminative premortem-by-static-inspection complements
  integration-test RED gates.

### Standing rules upheld

- **Internal-versioning posture.** External `cargo publish` deferred. One
  future internal milestone queued before the next external publish:
  `vuln-migration-v1`.
- No push, no `cargo publish`, no `Cargo.toml` version bump in this
  milestone. `Cargo.lock` not staged. Explicit-add staging only.

## field_access_info-extension-v1 — internal milestone

NOT a published release. Internal-versioning posture: external `cargo publish`
deferred until ALL anti-product surfaces close end-to-end.

This milestone reframes the original "extend `field_access_info`" framing into
a mechanical entry-shape migration. The Wave-2-pre `member_patterns_match`
call-shape path (added during regex-removal-v1) is now load-bearing for the
three HOLD languages — Ruby, Elixir, OCaml — whose `Module.function` call
shapes were not yet routed through structured AST entries when regex-removal-v1
landed. With this milestone, those 19 entries are migrated to structured
`(Module, function)` tuples and the corresponding regex source+sink banks for
those three languages are deleted (sanitizer banks retained).

### Changed

- **Structured `(Module, function)` AST entries shipped for 3 HOLD languages**
  across 19 entries:
  - **Ruby** (6): `STDIN.read`, `STDIN.gets`, `STDIN.readline` (sources);
    `File.read`, `File.open`, `IO.popen` (sinks).
  - **Elixir** (7): `IO.gets`, `System.get_env`, `File.read`, `File.read!`
    (sources); `System.cmd`, `Code.eval_string`, `Ecto.Adapters.SQL.query`
    (sinks).
  - **OCaml** (6): `Sys.getenv`, `In_channel.read_all`, `In_channel.input_all`
    (sources); `Sys.command`, `Unix.execvp`, `Sqlite3.exec` (sinks).
- **W2-pre call-shape path in `member_patterns_match` is now load-bearing for
  these 3 languages.** The path splits dotted call names from
  `extract_call_name_*` on `rfind('.')` and matches structured
  `(receiver, field)` tuples — added during regex-removal-v1 as a baseline-
  language enabler, now extended in scope to cover Ruby/Elixir/OCaml.
- **OCaml AST var-extraction extended (M5)** to handle `application_expression`
  shape. Added an OCaml-specific branch to `extract_first_identifier_arg_ast`:
  unlike `call_expression` (which has a named `arguments` field), OCaml's
  `application_expression` exposes `child(0)` as the function and
  `child(1..)` as the args, so the existing field lookup did not fire.
- **Ruby AST pattern dispatch order corrected (M5).** The structured
  `('STDIN', 'gets')` Stdin member pattern was moved BEFORE the UserInput
  `call_names: ['gets']` entry in `RUBY_AST_SOURCES` so that the more-specific
  member-shape fires first; otherwise the `ends_with('.gets')` heuristic in
  the UserInput path would shadow it on `STDIN.gets` lines.
- **String-literal regression-guard auto-fix (M5).** `detect_sources_ast` and
  `detect_sinks_ast` now apply two fallbacks when an AST hit's argument list
  contains only string literals (no identifier arg):
  (1) text-fallback via `extract_source_var_from_statement`, and
  (2) synthetic-var-from-call-name fallback. Without these, AST hits whose
  args are all string literals (common for `File.read("/path")`-shaped sinks)
  would silently drop their source/sink emission after the regex banks are
  removed.

### Retained

- **All 16 sanitizer regex banks** across all languages — same posture as
  regex-removal-v1; sanitizer AST dispatch is deferred to the
  `sanitizer-removal-v1` future internal milestone.
- **Subscript-shape AST entries in `RUBY_AST_SOURCES`:** `("", "params[")` and
  `("", "ENV[")`. Subscripts are not `Module.function`-shaped; tree-sitter
  parses them as `element_reference`, not `call`, so the W2-pre call-shape
  path does not apply. These entries continue to use the existing subscript
  matcher.
- **`\bgets\b` regex entry in `RUBY_PATTERNS.sources`.** Bare Ruby `gets` is
  parsed by tree-sitter-ruby as `identifier` (not `call`), so AST
  `call_names: ['gets']` does NOT cover it. Documented carry-forward
  exception (Option A from M1 finding #2). A future milestone may extend
  `extract_call_name_ruby` to recognize bare `gets` and retire this regex.
- **Bare OCaml `read_line` / `input_line` `call_names` entries** — already
  structured-correct under the existing `call_names` path.

### Removed

- Ruby/Elixir/OCaml **regex source+sink Vec entries** in `RUBY_PATTERNS` /
  `ELIXIR_PATTERNS` / `OCAML_PATTERNS` (sanitizer Vecs retained).
- **14 raw-substring `("", "Module.fn")` AST raw-fallback duplicates**
  superseded by the Wave-2 structured shape (M2 b48ba89, M3 6b6a093,
  M4 f4e1b16).
- **6 obsolete unit tests** in `crates/tldr-core/src/security/taint_tests.rs`:
  `test_ruby_detect_sources`, `test_ruby_detect_sinks`,
  `test_elixir_detect_sources`, `test_elixir_detect_sinks`,
  `test_ocaml_detect_sources`, `test_ocaml_detect_sinks`. Sanitizer-touching
  tests retained.

### Issues closed (binary-verified)

- **String-literal substring false-positive class GENERALIZED to 3 HOLD
  languages.** Verified zero sources / zero sinks at the `tldr taint` binary
  for Ruby `"use IO.popen for shell exec"`, Elixir `"use System.cmd"`, and
  OCaml `"use Sys.command"` string-literal fixtures at `/tmp/v040-verify/`.
  This generalizes regex-removal-v1's #24 closure (TypeScript) to Ruby /
  Elixir / OCaml.
- **Real-flow detection preserved.** Ruby `STDIN.gets → IO.popen(cmd)`,
  Elixir `System.get_env → System.cmd`, and OCaml `Sys.getenv → Sys.command`
  all correctly TAINTED in the binary smoke set.

### Architectural notes

- **No source change to `field_access_info` or `extract_call_name_*` helpers.**
  The milestone reframed the original "extend `field_access_info`" framing
  into a mechanical entry-shape migration. The W2-pre `member_patterns_match`
  call-shape path (added during regex-removal-v1) was already the
  architectural enabler — the work in this milestone is the corresponding
  data migration plus three small targeted fixes (OCaml
  `application_expression` var-extraction, Ruby dispatch order, string-
  literal fallback).
- **M1 added a test-only `analyze_ast_only(src, lang, fn_name)` harness** via
  a thread-local `AST_ONLY_TEST_MODE` `Cell` and an RAII
  `AstOnlyTestModeGuard`. While the guard is alive the flag short-circuits
  `detect_sources` / `detect_sinks` to an empty `Vec`, mirroring W2-pre's
  AST-only simulation. Production code never sets the flag.

### Standing rules upheld

- **Internal-versioning posture.** External `cargo publish` deferred. Two
  future internal milestones still queued before the next external publish:
  `sanitizer-removal-v1` and `vuln-migration-v1`.
- No push, no `cargo publish`, no `Cargo.toml` version bump in this
  milestone. `Cargo.lock` not staged. Explicit-add staging only.

## regex-removal-v1 (internal milestone) — 2026-04-29

**INTERNAL milestone — NOT a published release.** Closes #24 (string-literal
substring false positive) end-to-end at the `tldr taint` binary path by
deleting the regex source+sink banks for 13 of 16 supported languages.
Tagged locally as `regex-removal-v1`. No `cargo publish`, no `git push`.
External publish remains deferred until the three follow-on internal
milestones land — `field_access_info-extension-v1`, `sanitizer-removal-v1`,
and `vuln-migration-v1`.

### Changed

- **AST-only source+sink matching** across 13 languages (Python, TypeScript,
  JavaScript, Go, Rust, Java, C, C++, Kotlin, Swift, C#, Scala, PHP, plus
  Lua/Luau which share a single bank). The `sources` and `sinks` Vecs in
  the corresponding `lazy_static` `LanguagePatterns` banks are now empty;
  detection runs entirely through the AST path established in engine-v1
  (M2) and reinforced by Wave-2-pre's AST-native var-extraction fallbacks.
- **`tldr taint` finding-count delta:** substantial reduction in false
  positives. String-literal substring matches that previously fired via
  `text.contains("req.body")` and friends are eliminated. Issue #24 is
  binary-verified closed end-to-end — `tldr taint
  /tmp/v030-verify/issue24_string_literal_fp.ts showDocs --format text`
  reports zero sources, zero sinks, zero vulnerabilities on the
  string-literal lines that previously produced spurious findings.
- **`compute_taint` refactored to internal-parse-and-delegate.** The public
  signature is unchanged; the body now reconstructs source text from the
  line-keyed `statements` HashMap, calls
  `crate::ast::parser::parse(&src, language)`, and on `Ok(tree)` delegates
  to `compute_taint_with_tree(...)`. On parser error it returns
  `Ok(TaintInfo::default())` for graceful degradation. This eliminates the
  legacy regex-only branch that would have become a dead path after the
  bank deletion.
- **`compute_taint_with_tree` dispatch unchanged.** The additive-merge loop
  (AST detection ∪ regex detection) naturally degrades to AST-only behavior
  when the regex banks return empty Vecs for the 13 emptied languages.
  Wave-2-pre's `extract_first_identifier_arg_ast` and
  `extract_assignment_rhs_ident` helpers (added at HEAD `256d709`) take
  over the var-extraction step that previously coupled the AST hit path to
  the regex bank.

### Retained

- **Ruby, Elixir, OCaml regex source+sink banks.** These three languages
  use `Module.function` call shapes (`IO.popen`, `System.cmd`,
  `Sys.command`) that are not yet covered by `field_access_info` for the
  AST member-access path. Banks remain populated; deferred to the
  `field_access_info-extension-v1` future internal milestone.
- **All 16 sanitizer regex banks** across all languages.
  `detect_sanitizer_ast` is currently unwired (zero call sites at HEAD);
  removing the regex sanitizer banks would silently drop sanitizer
  detection. Deferred to the `sanitizer-removal-v1` future internal
  milestone, which will wire the AST sanitizer path before deleting the
  regex banks.

### Removed

- `merge_patterns` helper (TS framework bank consolidation no longer
  needed).
- 4 TypeScript framework sub-banks: `TYPESCRIPT_EXPRESS_PATTERNS`,
  `NEXTJS_PATTERNS`, `FASTIFY_PATTERNS`, `NESTJS_PATTERNS`. Sanitizer
  entries from these sub-banks were consolidated into the surviving
  `TYPESCRIPT_PATTERNS` bank (`parseInt`/`Number`/`parseFloat`,
  `encodeURIComponent`/`DOMPurify.sanitize`, `.parse`/`.safeParse`).
- `find_sinks_in_statement` and `find_sources_in_statement` crate-internal
  aliases (zero remaining callers after the obsolete-test deletion below).
- 23 obsolete regex-bank unit tests (one `detect_sources_*` / `detect_sinks_*`
  per emptied language) in `crates/tldr-core/src/security/taint_tests.rs`,
  plus 10 obsolete `test_detect_*` integration tests in
  `crates/tldr-core/tests/security_tests.rs` (Python sources/sinks +
  TypeScript source/sink + Go sources).
- `test_ast_patterns_defined_for_all_languages` invariant — obsolete by
  design after the bank emptying (the 13 emptied languages now have
  empty regex source/sink Vecs).
- `test_compute_taint_with_tree_no_tree` — its purpose (regex-only
  fallback verification) is invalidated by the Python regex bank deletion.

### Issues closed (binary-verified)

- **#24** — string-literal substring false positive at the `tldr taint`
  path. Verified zero sources / zero sinks / zero vulnerabilities for
  `req.body` and `req.params.id` substrings inside string literals on
  `/tmp/v030-verify/issue24_string_literal_fp.ts`. The regex fallback that
  caused engine-v1 to leave this issue OPEN end-to-end is now gone.

### Standing rules upheld

- **Internal-versioning posture.** External `cargo publish` deferred until
  all four future internal milestones land: `regex-removal-v1` (this one),
  `field_access_info-extension-v1`, `sanitizer-removal-v1`, and
  `vuln-migration-v1`. The next external publish will bundle the four.
- No push, no `cargo publish`, no `Cargo.toml` version bump in this
  milestone. `Cargo.lock` not staged. Explicit-add staging only.

### Wave-2-pre note

This milestone built on the Wave-2-pre architectural fixes (commit
`256d709`), which closed two load-bearing couplings between the AST
detection path and the regex banks before the atomic deletion: (1) call-shape
member_pattern matching for tree-sitter languages where `request.getParameter`
is a single `method_invocation` node rather than a `field_access`, and
(2) regex-free var-extraction helpers that supply the tainted variable
name when the regex bank returns empty. Without those, the bank deletion
in this milestone would have silently dropped 5 baseline-language taint
flows (C `fgets`, Java `request.getParameter`, Kotlin `Runtime.exec`,
Swift `Process.run`, NextJS `dangerouslySetInnerHTML`).

## engine-v1 (internal milestone) — 2026-04-29

**INTERNAL milestone — NOT a published release.** Engine restructure work
that will be bundled into the next external publish once the deferred
regex-fallback work and `tldr vuln` migration also land. Tagged locally
as `engine-v1`. No `cargo publish`, no `git push`.

### Engine internals (unit-test verified)

- **process_block taint propagation** rewired from substring matching to
  VarRef-based per-line use lookup (M1a). Eliminates the variable-shadowing
  false-positive class for the `tldr taint` code path — short variable
  names like `x`, `i`, `db` no longer match unrelated tokens via substring.
  Substring predicate at taint.rs:3761 (Definition arm) and :3780 (Update arm)
  replaced with `rhs_uses_tainted` helper. **Binary-verified:** the prior
  FP on `bar.x()` shadowing `x = input()` no longer fires via `tldr taint`.
- **SSA-versioned taint key** layered on top (M1b). `compute_taint_with_tree`
  accepts an optional `&SsaFunction`; reassignment-through-sanitizer correctly
  clears taint on the post-sanitizer SSA version. Falls back to VarRef-keyed
  mode for languages where SSA construction is partial — never panics.
- **AST member-access matching** is now structural across all 16 language
  families (M2). Replaces `text.contains(member_pattern)` with
  `extract_member_access_receiver_and_field` via the existing
  `field_access_info(language)` schema. 217 member_patterns strings migrated
  from `&[&str]` to `&[(&str, &str)]` across 43 of 48 AST pattern banks.
  **Caveat:** Ruby, Elixir, and OCaml have partial `field_access_info`
  coverage; `Module.function` call patterns retain `call_names` / substring
  fallback.

### Known gaps NOT closed by this milestone (binary-verified open)

These are the reasons engine-v1 is internal-only — the next external
publish ships when all four code paths produce honest results end-to-end:

- **Issue #24 (string-literal substring FP) PERSISTS end-to-end** despite
  M2's unit-test PASS. Source dispatch is AST-preferring with regex
  fallback; when the AST returns empty for a line, the regex bank still
  substring-matches `req.body` against raw line text. Closure requires
  the deferred sink-dispatch flip + parity work (next internal milestone,
  was v0.4.0 §7).
- **`tldr vuln` retains all v0.2.x FPs** including the M1a substring
  shadow. `vuln.rs` has duplicate `TaintSource`/`TaintSink` types and
  inline taint propagation independent of `compute_taint_with_tree`.
  M1a/M1b/M2 do not reach this code path. Closure requires the
  vuln-migration milestone (was v0.5.0).
- **AST sanitizer detection** wired only via regex `detect_sanitizer`;
  AST-based sanitizer dispatch deferred.

### Infrastructure (also internal)

- **Multi-daemon registry** (M3) replaces v0.2.2 single-slot
  `daemon-active.json`. New commands: `tldr daemon list`,
  `tldr daemon stop --all`, `tldr daemon stop --project <abs-path>`.
  Concurrency: bounded compare-and-swap retry (3 attempts, no new
  dependency). One-shot migration shim auto-converts v0.2.x
  `daemon-active.json` on first registry access.
- **Fastembed cache fix** (M4 — closes v0.2.2 M9 deferred finding).
  `embedder.rs` honors `TLDR_FASTEMBED_CACHE` env override and defaults
  to `dirs::cache_dir().join("tldr/fastembed")`. Default parallelism now
  works for the test matrix; `--test-threads=1` workaround retired.
  54 race-prone test cells annotated with `#[serial(embedding_cache)]`.
  Two leaked `.fastembed_cache/` directories (~832 MB total) at workspace
  root and `crates/tldr-cli/` may be deleted:
  `rm -rf .fastembed_cache crates/tldr-cli/.fastembed_cache`

### Documentation

- v0.4.0 cross-procedural design queued at
  `thoughts/shared/plans/v0.4.0-cross-procedural-design.md` (M5).
  7 sections covering DtoTypeIndex, TaintSummary, sink dispatch flip
  + parity work, dependency graph, testing strategy, milestone proposal.

### Test Matrix

730/730 (`exhaustive_matrix`) + 234/234 (`language_command_matrix`) =
**964/964 at DEFAULT parallelism.** `--test-threads=1` no longer required.

### Issues touched (NONE closed by engine-v1)

- **#24** AST path fixed structurally; regex fallback FP persists
  end-to-end. **Issue stays OPEN** until the regex-fallback flip lands.
- **#7, #23, #27, #28** untouched — queued for the next internal
  milestone (quality bundle).

## v0.2.4 — 2026-04-28

### Fixed
- **#17 + #25** — IPC message-size enforcement before allocation. `IpcStream::recv_raw` now uses `tokio::io::AsyncReadExt::take` to bound the read at `MAX_MESSAGE_SIZE + 1` BEFORE allocating the destination String. Both Unix and Windows arms delegate to a shared `recv_raw_from<R: AsyncRead + Unpin>` helper. A 100MB no-newline payload no longer OOMs the daemon. Removed redundant post-allocation check at `read_command()`. ([commit 61e3055](https://github.com/parcadei/tldr-code/commit/61e3055))
- **#26** — `tldr surface` emits C# and Java interface methods regardless of `--include-private`. Interface methods omit `public` per language spec (implicit); the prior visibility predicate required an explicit modifier and silently dropped them. Fix mirrors the Rust trait short-circuit pattern. ([commit bc2fa83](https://github.com/parcadei/tldr-code/commit/bc2fa83))
- **#29** — `tldr imports <file> --lang <LANG>` now honored in both daemon-routed and direct-compute paths. Daemon path: new `params_with_file_lang` helper emits JSON key `"language"` to match `ImportsRequest.language` field name (was silently dropping `--lang` in the daemon hint payload). Direct-compute path: new `parse_file_with_lang(path, Option<TldrLanguage>)` sibling to `parse_file` honors caller-supplied language hint over path-extension detection; `get_imports` forwards `Some(language)`. End-to-end binary verification: `tldr imports myscript --lang python` (extensionless file, no daemon) now correctly detects imports. ([commit a3dfbc3](https://github.com/parcadei/tldr-code/commit/a3dfbc3) + [commit c034b68](https://github.com/parcadei/tldr-code/commit/c034b68))
- **#20 + #21** — Issue paperwork. Both code-fixed in v0.2.2 (M14 closed #20; M13 closed #21) and verified live in v0.2.3. Reopened pending artile confirmation; no artile activity since 2026-04-26. Closed with standard shipped-and-please-reopen-if-broken comments. ZERO source-code changes.

### Test matrix
- `cargo test -p tldr-cli --test exhaustive_matrix --features semantic --release -- --test-threads=1`: **730/730**
- `cargo test -p tldr-cli --test language_command_matrix --features semantic --release`: **234/234**
- Combined: **964/964** + `cargo clippy --workspace --all-features --tests -- -D warnings` clean.
- New tests added: 8 (M1: 3 IPC; M2: 2 surface interface; M3: 2 unit + 1 integration).
- Pre-existing: `exhaustive_matrix` produces 676/730 under default parallelism due to fastembed-cache filesystem race (per v0.2.2 M9 investigation). Use `--test-threads=1` for canonical baseline. Real fix queued for v0.3.0.

### Issue close-outs
- **#20** (daemon status wrong project path) — confirmed shipped in v0.2.3, closed with audit comment.
- **#21** (cargo build duplicate output collisions) — confirmed shipped in v0.2.3, closed with audit comment.
- **#6, #8, #16, #22** — closed earlier this session (already-fixed-in-v0.2.x housekeeping).

## v0.2.3 — 2026-04-27

### Fixed
- **#1.D** — `tldr smells` PR-focused signal filter. New `--files <FILE>...` (repeatable, exact-path-only) for caller-supplied scoping; default behavior excludes test-file findings via existing path-only `is_test_file` helper; new `--include-tests` opts back in. New `excluded_test_smells: usize` counter on `SmellsReport`. Daemon parity (`detect_smells_with_walker_opts`). `--files` entries validated via `tldr_core::validation::validate_file_path` (errors on system dirs). ([commit 4e0b312](https://github.com/parcadei/tldr-code/commit/4e0b312))
- **#1.E** — `tldr whatbreaks` `affected_test_count` populated for Function-target queries. Bug: the function-target branch in `whatbreaks_analysis` extracted `direct_callers` and `transitive_callers` from impact JSON but never set `affected_test_count` (it stayed at default = 0 even when test modules clearly appeared in the caller tree). Fix: `run_impact_analysis` now walks the `ImpactReport`'s caller trees during JSON serialization and emits `affected_test_count` as a new JSON field; the function-target branch reads it into the summary. ([commit b3d80c9](https://github.com/parcadei/tldr-code/commit/b3d80c9))
- **#1.F** — `tldr taint` TypeScript pattern expansion: Next.js, Fastify, NestJS support added in addition to the pre-existing Express coverage. Renamed existing `TYPESCRIPT_PATTERNS` → `TYPESCRIPT_EXPRESS_PATTERNS`; added `NEXTJS_PATTERNS` (6 sources / 4 sinks / 1 sanitizer), `FASTIFY_PATTERNS` (3 sources / 3 sinks), `NESTJS_PATTERNS` (5 sources / 2 sinks; sanitizers intentionally empty). Unified `TYPESCRIPT_PATTERNS` is now the merge of all 4 banks (20 sources / 16 sinks / 3 sanitizers total). Engine semantics already supported indirect-flow propagation (CFG worklist) — patterns alone fix the bug. ([commit 191da3b](https://github.com/parcadei/tldr-code/commit/191da3b))

### Known limitations (Next.js / Fastify / NestJS taint)
- NestJS decorator-injected parameters (`(@Body() body: T)`, `@Query()`, `@Param()`) are invisible to the regex-based source matcher. Coverage focused on `@Req() request: Request` and direct `request.body` access patterns. Future engine-level work could parse decorators properly.
- NestJS pattern bank intentionally has no sanitizers — `class-validator` decorators (`@IsEmail()`, `@IsUrl()`) validate format but do not escape, so calling them sanitizers would mislead on security. Expect higher flow counts on NestJS controllers than on Express.
- `reply.send` (Fastify) and `Response.send` (NestJS) sink patterns may produce false positives on unrelated types that happen to expose a `send` method. Acceptable for v0.2.3; could be refined in a future release.

### Test matrix
- `cargo test -p tldr-cli --test exhaustive_matrix --features semantic --release -- --test-threads=1`: **730/730**
- `cargo test -p tldr-cli --test language_command_matrix --features semantic --release`: **234/234**
- Combined: **964/964** + `cargo clippy --workspace --all-features --tests -- -D warnings` clean.
- Pre-existing: `exhaustive_matrix` produces 676/730 under default parallelism due to fastembed-cache filesystem race (per v0.2.2 M9 investigation). Use `--test-threads=1` for canonical baseline. Real fix queued for v0.3.0.

## v0.2.2 — 2026-04-25

Quality release closing 9 GitHub issues filed against v0.2.0/v0.2.1, plus implementing the SSRF detection rule that was flagged as latent during v0.2.1 (the `VulnType::Ssrf` arm at `crates/tldr-core/src/security/vuln.rs:609-628` returned `vec![]` for every language, so the rule never fired despite v0.2.1's correct CWE-918 wire labelling). Seven fixes shipped across six fix commits + one feature commit; matrix held at 964/964 (730 exhaustive + 234 language-command, run with `--test-threads=1` per the test-harness embedding-mutex contention noted below); `cargo clippy --workspace --all-features --tests -- -D warnings` clean across all eight commits.

### Fixed

- **#9 + #16** — Unicode truncation sweep. Surface modules and CLI output formatters now use char-boundary-aware truncation instead of unsafe byte slicing on potentially non-ASCII text (CJK, emoji, combining marks). Triage named 15 sites; re-verification surfaced 5 additional CLI sites of the same root cause (clones tail @1641+1646, module/class/function docstring previews @2206+2261+2394 in `crates/tldr-cli/src/output.rs`) — 20 sites total fixed via shared helpers `tldr_core::util::truncate_at_char_boundary` and `truncate_at_char_boundary_from_end`. Pre-fix repro: `&s[..N]` panic with `byte index N is not a char boundary; it is inside '世'`. ([commit 88ddac6](https://github.com/parcadei/tldr-code/commit/88ddac6))
- **#18 + #6** — CFG/SSA pipeline correctness. (a) `break` statements no longer create back-edges to loop headers in the CFG (`process_break_statement` now records into `loop_exit_blocks` and the back-edge guards at the while/loop sites short-circuit on exit-block membership). (b) SSA construction no longer drops orphaned function parameters (`collect_variable_definitions` falls back to the entry block when `get_block_for_line` returns `None`, mirroring the `dfg/reaching.rs:131-134` "Orphaned definition" pattern; `fill_phi_sources` now inserts undefined-version sources rather than omitting `PhiSource` entries). ([commit 7ca7b54](https://github.com/parcadei/tldr-code/commit/7ca7b54))
- **#15 + #8** — (a) `tldr tree` no longer false-flags hardlinks as symlink cycles. The `seen_inodes` HashSet at `crates/tldr-core/src/fs/tree.rs:177-188` was unnecessary (WalkDir is configured `follow_links(false)` so symlink cycles can't occur via this code path) AND wrong (it incorrectly flagged hardlinks). Removed the entire `#[cfg(unix)]` inode block. (b) BM25 tokenizer correctly handles single-letter PascalCase prefixes like `IService` and `XRequest`. The PascalCase split rule fired on `is_upper && next_is_lower` with no length guard, splitting `IService` to `['I', 'Service']` and then dropping `'I'` via the `len >= min_length=2` filter. Added `&& current.len() > 1` guard. `HTTPRequest`-style splits preserved. ([commit 48b03f9](https://github.com/parcadei/tldr-code/commit/48b03f9))
- **#10** — Daemon callgraph + BM25 caches now actually populate and serve cached results on subsequent requests. The pre-fix shape `entry.or_insert_with(OnceCell::new).clone()` returned an INDEPENDENT uninitialized clone, so `get_or_init` initialized the clone (which got discarded), not the HashMap entry — every request rebuilt from scratch. Fix: changed HashMap value type to `Arc<OnceCell<T>>` so `.clone()` shares the cell instead of producing an independent uninitialized clone. Preserved the existing "drop write lock before await" pattern. Repro test asserts an internal rebuild counter == 1 across 2 sequential requests (was 2 pre-fix). ([commit 62ae258](https://github.com/parcadei/tldr-code/commit/62ae258))
- **#13** — Alias analysis correctly propagates points-to updates through field stores when the source variable gains new info. The `reverse_copy` index was seeded for source-propagation per inline comment, but `propagate_variable`'s third branch (re-run `propagate_field_store` when source variable changes) was unimplemented. Added `reverse_field_stores: HashMap<String, Vec<(String, String)>>` index + the missing third branch. Restores Andersen's points-to soundness for `pts(loc.field) ⊇ pts(source)` inclusion. ([commit c82e004](https://github.com/parcadei/tldr-code/commit/c82e004))
- **#14** — Daemon startup race fixed. (a) `start.rs` no longer calls `cleanup_stale_pid` before `try_acquire_lock` — the flock-based `try_acquire_lock` already handles stale PIDs safely, and the pre-lock cleanup created a TOCTOU window where two concurrent starts could both proceed. (b) `bind_unix` no longer silently unlinks an existing socket — returns `AddressInUse` instead, so a second daemon-start cannot clobber a live socket from another daemon. Verified via `std::sync::Barrier`-synchronized concurrent test (zero sleeps; 5/5 flakiness runs GREEN). ([commit d87b7f3](https://github.com/parcadei/tldr-code/commit/d87b7f3))
- **SSRF detection rule** (follow-up from v0.2.1 #11 fix) — `tldr vuln` now emits SSRF findings (CWE-918) for 8 languages: Python, TypeScript, JavaScript, Go, Java, Rust, Ruby, PHP. The empty `VulnType::Ssrf => match language` block at `crates/tldr-core/src/security/vuln.rs:609-628` (which returned `vec![]` for every language) was populated with `(pattern, description)` sink-pattern tuples mirroring the `Deserialization` arm's shape — plumbed into the existing taint-engine flow with no engine changes. `VulnType::Ssrf` was also added to the default `vuln_types` list at `vuln.rs:838-845` so the rule actually fires on the default CLI invocation path (`scan_vulnerabilities` with `vuln_filter=None`). 10 remaining languages (C, C++, Kotlin, Swift, C#, Scala, Lua, Luau, Elixir, OCaml) are explicit empty arms — deferred to v0.2.3, no behavior change vs pre-M7 for those languages. Wire format: `vuln_type` JSON field == `"ssrf"`; `cwe_id` == `"CWE-918"`. 18 tests added (15 core unit + 3 CLI integration). ([commit 372b206](https://github.com/parcadei/tldr-code/commit/372b206))
- **#1.B** — `tldr change-impact` now finds the git binary even when `/opt/homebrew/bin` (or other Homebrew/non-default paths) is not on the cargo-built binary's runtime PATH. Resolution order: `GIT_BINARY` env var → `which::which("git")` → common paths fallback (`/opt/homebrew/bin/git`, `/usr/local/bin/git`, `/usr/bin/git`). Result cached in `OnceLock<PathBuf>`. Also: when `--base <branch>` fails because only `origin/<branch>` exists locally (not the bare `<branch>`), the NoBaseline error now appends `(hint: try --base origin/<branch>)`. Reproduced via env-stripped CLI invocation; pre-fix returned NoBaseline, post-fix returns Completed with 3 real working-tree files. ([commit da377c6](https://github.com/parcadei/tldr-code/commit/da377c6))
- **#1.C** — `tldr vuln <ts-file>` now autodetects TypeScript and JavaScript without requiring `--lang`. Pre-fix exited 2 with "taint analysis for typescript is not yet supported by autodetect" even though the underlying taint engine already routes TS/JS through `TYPESCRIPT_PATTERNS` (6 sources, 7 sinks, 2 sanitizers at `taint.rs:450-487`). The fix adds `Language::TypeScript | Language::JavaScript` to `is_natively_analyzed`. Test fixture emits 10 SSRF findings (CWE-918) through the now-enabled autodetect path. ([commit c665c77](https://github.com/parcadei/tldr-code/commit/c665c77))
- **#21** — `cargo build --workspace` no longer emits "output filename collision" warnings. The standalone `tldr-daemon` and `tldr-mcp` crates declared `[[bin]]` targets that collided with the shim `[[bin]]` declarations in `tldr-cli` (which build `target/release/tldr-daemon` and `target/release/tldr-mcp` for cargo-dist's single-package distribution pattern). Removed the duplicate `[[bin]]` declarations and added `autobins = false` to suppress Cargo's auto-bin discovery. `[lib]` sections retained so the shims continue to call `tldr_daemon::run()` / `tldr_mcp::run()`. Pre-fix: 4 warnings; post-fix: 0. All 3 binaries (`tldr`, `tldr-daemon`, `tldr-mcp`) still produced. ([commit 867139c](https://github.com/parcadei/tldr-code/commit/867139c))
- **#20** — `tldr daemon status` now correctly reports a running daemon's status from any cwd (pre-fix: from a different cwd, the command computed a different socket-hash and reported `not_running` even when the daemon was alive). On `daemon start` after successful bind, an active-daemon record is written atomically to `~/Library/Caches/tldr/daemon-active.json` with `{project, pid, socket}`. `daemon status` reads this file as a fallback when `--project` is not explicitly provided, verifies the PID is alive via `kill(0)`, and uses the recorded project path to compute the socket hash. `daemon stop` removes the file. The `--project` workaround still works as a regression guard. ([commit 1a96285](https://github.com/parcadei/tldr-code/commit/1a96285))

### Notes

- The `exhaustive_matrix` test harness has a known **filesystem race on the cold fastembed model cache** under default parallel test execution. `crates/tldr-core/src/semantic/embedder.rs:122` calls `TextEmbedding::try_new(InitOptions::new(fast_model))` with no explicit `with_cache_dir(...)` override, so fastembed defaults to `<CWD>/.fastembed_cache/`. When parallel test processes spawn from a cold cache, they race on creating/extracting the ~110MB Snowflake Arctic-M model files. The first child starts a download; siblings see partially-written files and fail their integrity checks. Result: 676/730 cells under default parallelism, 730/730 with `cargo test ... -- --test-threads=1`. Pre-existing — not introduced by any v0.2.2 fix. Use single-threaded execution for the canonical matrix baseline. Recommended fix (v0.3.0): add `.with_cache_dir(dirs::cache_dir().join("tldr/fastembed"))` to move the model cache to a global location (~/Library/Caches/tldr/fastembed on macOS), eliminating the per-CWD duplication, plus `#[serial(embedding_cache)]` on the affected tests for deterministic single-flight downloads.
- `cargo install tldr-cli` and `cargo install tldr-cli --features semantic` continue to work as in v0.2.0/v0.2.1 — no new install-time requirements.
- The 4 binary targets (aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu) are built automatically by cargo-dist via `.github/workflows/release.yml` on the `v0.2.2` tag.

## v0.2.1 — 2026-04-25

Hotfix release closing 4 GitHub issues filed against v0.2.0, with scope expanded mid-flight to incorporate 2 audit-driven fixes (M6: 7 additional unguarded daemon handlers under #5; M7: request-side camelCase mismatch under #19). All 6 fixes were confirmed reproducible at their respective starting commits and fixed at root cause with new in-process integration tests pinning the bug shape. No regressions: full 964/964 matrix (730 exhaustive + 234 language-command) green across every fix commit; `cargo clippy --workspace --all-features --tests -- -D warnings` clean.

### Fixed

- **#5 (security, Unix-side path traversal)**: `tldr-daemon` IPC handlers (`secrets`, `vuln`) now route every caller-supplied absolute path through `tldr_core::validate_file_path` before any filesystem read, refusing requests for paths outside the active project root with `BAD_REQUEST`. Pre-fix, the handlers accepted any `is_absolute()` path verbatim, which on a daemon already running could be exploited to extract `/Users/<other>/.aws/credentials`-shaped secrets. The Windows TCP unauthenticated listener portion of #5 remains an open design question (multi-user daemon sharing semantics) and is deferred to v0.3.0. ([commit 00ee2dc](https://github.com/parcadei/tldr-code/commit/00ee2dc))
- **#11**: `tldr vuln --format sarif` and `--format json` now correctly label `Deserialization` findings as deserialization (CWE-502) — pre-fix, the wildcard match arm `_ => VulnType::SqlInjection` at `crates/tldr-cli/src/commands/remaining/vuln.rs:645-651` silently mislabeled them as SQL injection (CWE-89). `Ssrf` was affected by the same wildcard and is now correctly mapped to CWE-918. The match is exhaustive — future `tldr_core::security::vuln::VulnType` variants will fail to compile until they are mapped, preventing the same bug pattern from recurring. ([commit 181f929](https://github.com/parcadei/tldr-code/commit/181f929))
- **#12**: `tldr-mcp` now speaks JSON-RPC 2.0 + MCP 2024-11-05 lifecycle correctly. Three sub-bugs fixed in one commit: (a) `JsonRpcRequest.id` is now `Option<Value>` with `#[serde(default)]` so notification frames (no `id`) deserialize cleanly; (b) the dispatcher now suppresses all response emission when `id` is `None`, per JSON-RPC 2.0 §4.1 ("a server MUST NOT reply to a notification"); (c) the canonical method `notifications/initialized` is routed (the legacy bare `initialized` typo was a v0.1.x scaffold mistake — never spec-correct in any MCP draft — and was removed rather than kept as an alias to avoid masking client bugs in the wider ecosystem). ([commit 1620b6d](https://github.com/parcadei/tldr-code/commit/1620b6d))
- **#19** (filed by @etal37): `tldr-mcp`'s `initialize` response now emits `protocolVersion` and `serverInfo` in camelCase per the MCP 2024-11-05 wire spec. Pre-fix, `InitializeResult` serialized snake_case (`protocol_version`, `server_info`) which Claude Code and other spec-compliant clients reject during the lifecycle handshake — the user-facing failure was "Claude Code cannot connect to tldr-mcp". A recursive scan of the day-one handshake responses (`initialize` + `tools/list`) now returns zero snake_case keys outside JSON Schema property declarations under `inputSchema.properties` (which are user-defined argument names extracted by tool handlers, not MCP-defined wire fields). ([commit 2726358](https://github.com/parcadei/tldr-code/commit/2726358))
- **#5 (security, broader handler audit)**: Audit of `crates/tldr-daemon/src/handlers/{ast,flow,quality}.rs` found 7 additional unguarded path arguments using the same `is_absolute → accept` pattern as the original #5 fix. Each was wired through `tldr_core::validate_file_path` with `BAD_REQUEST` mapping. Affected handlers: `imports`, `cfg`, `dfg`, `slice`, `complexity`, `smells`, `maintainability`. Reproduction tests in `crates/tldr-daemon/tests/handler_path_traversal_audit_test.rs` confirm canary file content no longer leaks (canary substring `canary_xyz_42` previously appeared in `ImportInfo.module`, `CfgInfo.function`, `DfgInfo.function`, `ComplexityMetrics.function` response fields). ([commit b988c42](https://github.com/parcadei/tldr-code/commit/b988c42))
- **#19 (broader request-side audit)**: Beyond the response-side `InitializeResult` rename, audit of `crates/tldr-mcp/src/protocol.rs` found `InitializeParams` was silently dropping `protocolVersion` and `clientInfo` from spec-compliant client requests because the request-side struct lacked `#[serde(rename_all = "camelCase")]`. With `#[serde(default)]` on every field, missing-key errors degraded to `None` — Claude Code's day-1 handshake completed but the server's announced-protocol-version and client-info diagnostics became dead code. Now applied via struct-level `rename_all` attribute (future-proof against new fields silently regressing). ([commit 4204616](https://github.com/parcadei/tldr-code/commit/4204616))

### Notes

- `cargo install tldr-cli` and `cargo install tldr-cli --features semantic` continue to work as in v0.2.0 — no new install-time requirements.
- The 4 binary targets (aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu) are built automatically by cargo-dist via `.github/workflows/release.yml` on the `v0.2.1` tag.
- The original triage produced 4 fix milestones (M1–M4); the broader audit milestones M6 + M7 were dispatched after M5's release prep when latent flags surfaced during M1's and M4's work were promoted into the v0.2.1 scope rather than deferred to v0.2.2. Total v0.2.1 fixes shipped: 6 (4 originally-triaged + 2 audit-discovered).

## v0.2.0 — 2026-04-25

Major hardening release. Closes parcadei/tldr-code#1 + extends per-language coverage to all 18 supported languages across the full command surface.

### Fixes (from issue #1)

- **Walker hardening** (dc896a7): single `ProjectWalker` on `ignore::WalkBuilder` with default excludes for `node_modules`, `target`, `dist`, `build`, `.next`, `vendor`, `.git`, `__pycache__`. Replaces ~30 raw `walkdir` call sites. `tldr smells`/`secure`/`vuln` no longer descend into vendored code.
- **Language detector consolidation** (c492f49): single `Language::from_directory` with manifest-priority detection. TS projects no longer report as Python.
- **TSX parser dispatch** (9697d21): `ParserPool` selects `LANGUAGE_TSX` for `.tsx`/`.jsx` files. Resolves exponential blowup in `tldr smells` on JSX files.
- **`change-impact` honesty** (8a89f60): new `ChangeImpactStatus` enum {Completed, NoChanges, NoBaseline, DetectionFailed}. Empty results no longer return cheerful exit-0 success.
- **`vuln` autodetect + cap removed** (b1ceffa): `tldr vuln` autodetects language; emits clear error when taint backend (Python+Rust only) doesn't support detected language. Removed silent 1000-file cap.
- **Workspace discovery** (94cc6f0): call graph auto-discovers pnpm/npm/Cargo/go.work workspace roots. Multi-root tsconfig path resolution. `impact` and `whatbreaks` no longer return spurious 0-callers in monorepos.

### Coverage

- **18-language manifest detection** (d3a7e9f): added 7 missing languages (C, C++, C#, Scala, Lua, Luau, OCaml) with proper tie-breaking.
- **Cross-file call resolution** (2577737): closed gap for C, C++, Ruby, Kotlin, Swift, PHP, Luau, OCaml. All 18 languages now resolve cross-file calls.
- **Ruby bareword calls** (e3d9916): `helper` (no parens) now recognized as method call per Ruby semantics.
- **Elixir contracts** (4afae82): `def name do ... end` form now parses correctly.
- **`surface` for Luau + OCaml** (c6fe8a1): API surface extraction for the last 2 languages, including OCaml's `.mli` interface boundary.
- **`definition` for all 18 languages** (a868cbe): go-to-definition no longer Python-only.
- **`temporal` for all 18 languages** (cd81e05): method-call sequence mining no longer Python-only.

### Test infrastructure

- **234-cell command×language matrix** (2d8500c, 2577737): 13 representative commands × 18 languages, strong assertions including cross-file edge counts.
- **730-cell exhaustive matrix** (91ea0fb, c6fe8a1, a868cbe, cd81e05, e0c5e97): 38 language-applicable commands × 18 languages + orchestrator sanity.
- **Tightened weak assertions** (2cacc37, 51eb4e7, 0d35f1b, e0d2dfc): every PASS now verifies command output, not just clean exit. Surfaced and fixed 5 latent bugs (OCaml diff double-counting, OCaml `_`-pattern in structure/callgraph, bm25 hidden-root, context.rs intra-file-only, C# dead-code over-rescue).

### Known limitations

- The `semantic` feature is opt-in (`cargo install tldr-cli --features semantic`). Builds reliably on Mac; unverified on other platforms. PRs to make it portable are welcome.
- `tldr specs` is pytest-specific by design; generalizing requires per-framework parsers (Jest, RSpec, JUnit, etc.) — separate scope.
- `tldr coverage`, `tldr fix`, `tldr bugbot` operate on non-fixture inputs (XML/JSON/error-output/multi-stage) so they aren't in the per-language matrix.

### Notes

- `semantic` shipped as default in M9 was reverted to opt-in for v0.2.0 because ONNX Runtime linking is fragile on Linux aarch64 and we don't want broken `cargo install` on any platform.

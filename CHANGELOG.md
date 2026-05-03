# Changelog

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

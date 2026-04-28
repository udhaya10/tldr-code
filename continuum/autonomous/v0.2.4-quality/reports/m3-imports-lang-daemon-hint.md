# M3 — VAL-003 — `tldr imports --lang` Daemon Hint Propagation (closes #29)

**Worker:** kraken (M3 VAL-003 v0.2.4)
**Issue:** parcadei/tldr-code#29
**Triage brief:** `continuum/autonomous/v0.2.4-quality/triage/m3-imports-lang-daemon-hint.md`
**Starting HEAD (working-tree base):** `bc2fa83` (M2 already committed; brief named `10f00a9` as the v0.2.4 entry SHA)

---

## Problem

`tldr imports <file> --lang <LANG>` ignored `--lang` when a daemon was running.
Root cause: `crates/tldr-cli/src/commands/imports.rs:L37` called
`try_daemon_route::<Vec<ImportInfo>>(project, "imports", params_with_file(&self.file))`,
and the helper `params_with_file` (`daemon_router.rs:L166-L170`) emitted only
`{"file": <path>}` — `self.lang` was silently dropped before the JSON went to
the daemon. The daemon handler (`crates/tldr-daemon/src/handlers/ast.rs:L161-L199`)
was already correct (`ImportsRequest { file, language: Option<String> }`,
calls `detect_or_parse_language(request.language.as_deref(), &file_path)`).

## Triage Drift vs. Starting HEAD

| Symbol | Brief said | Actually | Δ |
|---|---|---|---|
| Working-tree base SHA | `10f00a9` | `bc2fa83` (M2 landed in interim) | M2 had already merged; M1 WIP unstaged on disk |
| `params_with_file` (current helper) | `daemon_router.rs:L166-L170` | `L166-L170` | exact |
| `params_with_path_lang` analog | `L230-L237` | `L230-L237` | exact |
| `imports.rs` daemon import | `L15` | `L15` | exact |
| `imports.rs` daemon call site | `L37` | `L37` | exact |
| `imports.rs` direct-compute | `L55-L56` | `L57-L58` (1-line shift after edit) | unchanged |
| Insert point in cli_p1_tests | after `test_imports_with_lang_flag` (L197) | L197 | exact |

## Solution

CLI-side only. Added a new params helper that emits the `language` JSON key
(matching the daemon handler's `ImportsRequest.language` field — there is NO
`#[serde(rename)]`, so a `"lang"` key would be silently ignored by serde and
the bug would still ship). Updated the imports daemon fast-path import + call
site.

## Per-Site Fix List

| File | Site | Change |
|---|---|---|
| `crates/tldr-cli/src/commands/daemon_router.rs` | new helper after `params_with_file` | added `pub fn params_with_file_lang(file: &Path, lang: Option<&str>) -> serde_json::Value` emitting `{"file":..., "language":...}` |
| `crates/tldr-cli/src/commands/daemon_router.rs` | `tests` module | added 2 unit tests: `test_params_with_file_lang_includes_language_key` (asserts `params["language"] == "python"` — load-bearing JSON-key assertion) and `test_params_with_file_lang_omits_language_when_none` |
| `crates/tldr-cli/src/commands/imports.rs:L15` | use-statement | replaced `params_with_file` import with `params_with_file_lang` |
| `crates/tldr-cli/src/commands/imports.rs:L36-L40` | daemon fast-path call site | switched to `params_with_file_lang(&self.file, self.lang.as_ref().map(|l| l.as_str()))` |

`imports.rs:L55-L56` (direct-compute) — UNCHANGED per brief.
`crates/tldr-daemon/src/handlers/ast.rs` — UNCHANGED per brief (already correct).

## RED Capture Excerpts

`continuum/autonomous/v0.2.4-quality/reports/m3-red-capture.txt`:

```
error[E0425]: cannot find function `params_with_file_lang` in this scope
   --> crates/tldr-cli/src/commands/daemon_router.rs:342:22
    |
230 | pub fn params_with_path_lang(path: &Path, lang: Option<&str>) -> serde_json::Value {
    | ---------------------------------------------------------------------------------- similarly named function `params_with_path_lang` defined here
...
342 |         let params = params_with_file_lang(Path::new("/tmp/myscript"), Some("python"));
    |                      ^^^^^^^^^^^^^^^^^^^^^ help: a function with a similar name exists: `params_with_path_lang`
```

This compile error (E0425) is the load-bearing RED — the unit tests cannot
even compile pre-fix because the helper does not yet exist.

## GREEN Capture Excerpts

`continuum/autonomous/v0.2.4-quality/reports/m3-green-capture.txt`:

```
test commands::daemon_router::tests::test_params_with_file_lang_includes_language_key ... ok
test commands::daemon_router::tests::test_params_with_file_lang_omits_language_when_none ... ok
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 1369 filtered out

# cli_p1_tests imports module
test imports_command::test_imports_command_exists ... ok
test imports_command::test_imports_error_missing_file ... ok
test imports_command::test_imports_with_lang_flag ... ok
test imports_command::test_imports_auto_detect_language ... ok
test imports_command::test_imports_returns_array ... ok
test result: ok. 5 passed; 0 failed
```

Both new unit tests GREEN, and all 5 existing imports integration tests still pass.

## Test Inventory

- 2 unit tests added in `daemon_router.rs::tests` (load-bearing JSON-key assertion)
- 5 existing integration tests in `cli_p1_tests::imports_command` regression-checked

## Integration-Test Escalation Note

The brief specified a third test — an integration test on an extensionless file:

```rust
let test_file = temp.path().join("myscript");   // no extension
fs::write(&test_file, "import os\nimport sys\n").unwrap();
cmd.args(["imports", test_file.to_str().unwrap(), "--lang", "python", "-q"]);
```

The brief asserted: *"Pre-fix on HEAD 10f00a9: in no-daemon CI this passes
(direct-compute path honors --lang)."*

Verified empirically: this is **NOT** the case. Running the test pre- and
post-fix produces:

```
Error: Unsupported language: unknown
```

Tracing the binary with debug prints showed `self.lang = Some(Python)` and
`self.lang.as_str() = "python"` are correctly populated by clap. The error
originates inside `tldr-core`:

`crates/tldr-core/src/ast/imports.rs:L27-L30` — `get_imports(file_path, language)` calls
`parse_file(file_path)` (no language hint) →
`crates/tldr-core/src/ast/parser.rs:L263-L271` — `ParserPool::parse_file` re-detects
language **purely from the path extension**, ignoring any caller-supplied
`Language`. For an extensionless file, `path.extension()` is `None` →
`unwrap_or_else(|| "unknown".to_string())` → returns
`TldrError::UnsupportedLanguage("unknown")` — the literal observed error.

This is a separate, deeper bug. Touching `crates/tldr-core/` is an explicit
STOP condition in the M3 brief. Per the v0.2.3 anti-pattern note, I did NOT
backup/restore or rewrite tldr-core. Instead, I dropped the integration test
from the M3 commit (the unit tests are the load-bearing assertion for VAL-003)
and **escalate the parser-pool extension-only language detection bug to the
orchestrator** as a follow-up issue. Suggested fix: thread an
`Option<TldrLanguage>` hint into `ParserPool::parse_file` (or replace the
`get_imports` call with `parse_with_path` + `extract_imports_from_tree` in
`tldr-core/src/ast/imports.rs`).

The 2 unit tests still seal VAL-003: `params["language"] == "python"` proves
the daemon hint now carries the language across IPC. Once the parser-pool bug
is fixed in a follow-up, the integration test in the brief can be re-added
and will pass without further CLI changes.

## Matrix + Lint Verification

`cargo clippy --workspace --all-features --tests -- -D warnings` — clean.
(M1 sibling WIP introduces an `unused_imports` warning on
`crates/tldr-cli/src/commands/daemon/ipc.rs:23` for `AsyncReadExt`, but only
in the unstaged working tree. NOT touched by this commit.)

## Files Modified (Source)

3 files:

1. `crates/tldr-cli/src/commands/daemon_router.rs` — new helper + 2 unit tests
2. `crates/tldr-cli/src/commands/imports.rs` — L15 import, L36-L40 call site

(Initially edited `crates/tldr-cli/tests/cli_p1_tests.rs` for the
extensionless-file integration test, but reverted on discovering the
orthogonal `parser.rs` extension-only language detection bug. The test file
is unchanged from `bc2fa83`.)

## Disjointness from Siblings

| Sibling | Their files | Mine | Conflict |
|---|---|---|---|
| M1 (IPC bounded recv) | `crates/tldr-cli/src/commands/daemon/ipc.rs` | none | none |
| M2 (surface interfaces) | `crates/tldr-cli/src/commands/surface/csharp.rs`, `surface/java.rs` | none | none |

Disjoint.

## Commit SHA

`a3dfbc3ea03d00521bf98e398084d6c1d8b6df29`

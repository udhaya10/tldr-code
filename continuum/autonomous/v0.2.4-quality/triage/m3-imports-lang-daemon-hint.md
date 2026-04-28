# M3 — VAL-003 — `tldr imports --lang` Daemon Hint Propagation (closes #29)

**Worker:** kraken (M3 VAL-003 v0.2.4)
**Issue:** parcadei/tldr-code#29
**Starting HEAD:** `10f00a9`
**Validation gate:** `cargo test -p tldr-cli --test cli_p1_tests` GREEN + `cargo test -p tldr-cli --test exhaustive_matrix --features semantic --release -- --test-threads=1` (730/730) + `cargo test -p tldr-cli --test language_command_matrix --features semantic --release` (234/234) + `cargo clippy --workspace --all-features --tests -- -D warnings` (clean).

**BLOKS:** `bloks_context: null`, `bloks_cards: []`. NO BLOKS calls. PREPARE step bypassed.

---

## Context

`tldr imports <file> --lang <LANG>` ignores `--lang` when a daemon is running. ROOT CAUSE: at `crates/tldr-cli/src/commands/imports.rs:L37` the daemon fast-path calls `try_daemon_route::<Vec<ImportInfo>>(project, "imports", params_with_file(&self.file))`, and `params_with_file` (in `daemon_router.rs:L166-L170`) emits `{"file": <path>}` only — `self.lang` is silently dropped. The daemon then falls back to extension-based detection in `detect_or_parse_language(None, &file_path)` and fails on extensionless files.

The DIRECT-COMPUTE path at `imports.rs:L55-L56` is ALREADY CORRECT (passes `self.lang` through). The bug ONLY fires when a daemon is active.

The DAEMON HANDLER at `crates/tldr-daemon/src/handlers/ast.rs:L161-L199` is ALREADY CORRECT: `ImportsRequest { file, language: Option<String> }` with `serde(default)`; L184 calls `detect_or_parse_language(request.language.as_deref(), &file_path)`.

**Fix is CLI-side only.** Add a `params_with_file_lang(file, lang)` helper to `daemon_router.rs` that emits `{"file": <path>, "language": <lang>}`. Update imports.rs:L15 import and L37 call site. **CRITICAL: JSON key MUST be `"language"` (NOT `"lang"`)** to match `ImportsRequest.language` field name — there is NO `#[serde(rename)]` so a `"lang"` key would be silently ignored and the bug would still ship.

---

## Assignment

1. Add `pub fn params_with_file_lang(file: &Path, lang: Option<&str>) -> serde_json::Value` to `crates/tldr-cli/src/commands/daemon_router.rs` IMMEDIATELY AFTER the existing `params_with_file` (after L170). Mirrors the existing `params_with_path_lang` at L230-L237.
2. Update `crates/tldr-cli/src/commands/imports.rs:L15`: replace the `params_with_file` import with `params_with_file_lang`.
3. Update `crates/tldr-cli/src/commands/imports.rs:L37`: replace the call site to use the new helper, passing `self.lang.as_ref().map(|l| l.as_str())`.
4. NO change to `imports.rs:L55-L56` (direct-compute already correct).
5. NO change to daemon handler. NO core changes. NO clap struct changes.
6. Add 2 unit RED tests in `daemon_router.rs::tests` AND 1 integration RED test in `cli_p1_tests.rs::imports_command`.

---

## Step 0 — Verify Starting State

```bash
cd /Users/cosimo/Desktop/PatchWork/tldr-code
git status   # must be clean
git rev-parse HEAD   # must be 10f00a9
```

Confirm these line numbers — re-locate by symbol if drifted:

| Symbol | Expected file:line |
|---|---|
| `pub struct ImportsArgs` | `crates/tldr-cli/src/commands/imports.rs:20-27` (clap; `lang: Option<Language>`) |
| `imports.rs` import line (current `params_with_file`) | `crates/tldr-cli/src/commands/imports.rs:15` |
| Daemon fast-path call site | `crates/tldr-cli/src/commands/imports.rs:37` |
| Direct-compute path (already correct) | `crates/tldr-cli/src/commands/imports.rs:55-56` |
| `pub fn params_with_file` (CURRENT helper, no lang) | `crates/tldr-cli/src/commands/daemon_router.rs:166-170` |
| `pub fn params_with_path_lang` (analogous good pattern) | `crates/tldr-cli/src/commands/daemon_router.rs:230-237` |
| `daemon_router.rs::tests` module | (search for `#[cfg(test)] mod tests`) |
| `pub struct ImportsRequest { pub language: Option<String> }` (do NOT touch) | `crates/tldr-daemon/src/handlers/ast.rs:161-165` |
| Daemon handler call to `detect_or_parse_language` (do NOT touch) | `crates/tldr-daemon/src/handlers/ast.rs:184-185` |
| `mod imports_command` in cli_p1_tests | `crates/tldr-cli/tests/cli_p1_tests.rs:131` (approx) |
| Insert point for new integration test | after `test_imports_with_lang_flag` at `cli_p1_tests.rs:197` (end-of-test) |

If drift > 5 lines, log the actual line and proceed. Do not refactor unrelated symbols.

---

## TDD — RED First

Write the RED tests FIRST, on starting HEAD `10f00a9`. Capture stdout to `continuum/autonomous/v0.2.4-quality/reports/m3-red-capture.txt`.

### RED Test (a) — UNIT tests in `daemon_router.rs::tests` (load-bearing, always-RED pre-fix)

These FAIL TO COMPILE pre-fix because the helper does not exist. The compile-error is the load-bearing RED.

Add INSIDE the existing `#[cfg(test)] mod tests` in `daemon_router.rs` (find the test module; if no test module exists in the file, create one at the end):

```rust
#[test]
fn test_params_with_file_lang_includes_language_key() {
    let params = params_with_file_lang(Path::new("/tmp/myscript"), Some("python"));
    assert_eq!(params["language"], "python");
    assert_eq!(params["file"], "/tmp/myscript");
}

#[test]
fn test_params_with_file_lang_omits_language_when_none() {
    let params = params_with_file_lang(Path::new("/tmp/myscript"), None);
    assert!(params.get("language").is_none());
    assert_eq!(params["file"], "/tmp/myscript");
}
```

If `Path` is not imported in the test module, add `use std::path::Path;` at the top of the test module.

### RED Test (b) — INTEGRATION test in `cli_p1_tests.rs::imports_command`

This passes pre-fix in no-daemon CI (direct-compute path already works) — it is a regression seal post-fix, NOT the load-bearing RED.

Add inside `mod imports_command` after `test_imports_with_lang_flag` (which ends at L197):

```rust
/// Regression #29: --lang flag on an extensionless file must work.
///
/// Pre-fix on HEAD 10f00a9: in no-daemon CI this passes (direct-compute path
/// honors --lang). When daemon is active, params_with_file() drops lang and
/// the daemon falls back to extension detection → UnsupportedLanguage error.
/// This integration test acts as a regression seal post-fix.
#[test]
fn test_imports_lang_flag_on_extensionless_file() {
    let temp = TempDir::new().unwrap();
    // Intentionally no file extension — language MUST come from --lang flag.
    let test_file = temp.path().join("myscript");
    fs::write(&test_file, "import os\nimport sys\n").unwrap();

    let mut cmd = tldr_cmd();
    cmd.args([
        "imports",
        test_file.to_str().unwrap(),
        "--lang",
        "python",
        "-q",
    ]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("os"))
        .stdout(predicate::str::contains("sys"));
}
```

If `tldr_cmd`, `TempDir`, `fs`, `predicate` are not in scope at the insertion point, mirror imports from the existing `test_imports_with_lang_flag` test (which uses the same helpers).

**Capture RED:**

```bash
cargo test -p tldr-cli --lib commands::daemon_router::tests::test_params_with_file_lang_includes_language_key 2>&1 | tee continuum/autonomous/v0.2.4-quality/reports/m3-red-capture.txt
# Expect: COMPILE FAILURE — `params_with_file_lang` is not defined. This is the load-bearing RED.

cargo test -p tldr-cli --test cli_p1_tests imports_command::test_imports_lang_flag_on_extensionless_file 2>&1 | tee -a continuum/autonomous/v0.2.4-quality/reports/m3-red-capture.txt
# Expect (no-daemon CI): PASS pre-fix (direct-compute already works). Acceptable.
# Document this in the report: "RED via missing-helper compile-error in daemon_router; integration test acts as regression seal post-fix."
```

---

## GREEN — Fix

### Step 1 — Add `params_with_file_lang` to `daemon_router.rs`

IMMEDIATELY AFTER the existing `params_with_file` (which ends around L170), add:

```rust
/// Build JSON params with file path and optional language hint.
///
/// Used by commands (e.g. `imports`) that accept `--lang` and route through the daemon.
/// JSON key is `"language"` to match the daemon handler's `ImportsRequest.language` field
/// (handlers/ast.rs:L164) — there is no `#[serde(rename)]` on that field, so a `"lang"`
/// key would be silently ignored.
pub fn params_with_file_lang(file: &Path, lang: Option<&str>) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("file".to_string(), serde_json::json!(file));
    if let Some(l) = lang {
        obj.insert("language".to_string(), serde_json::json!(l));
    }
    serde_json::Value::Object(obj)
}
```

`Path` and `serde_json` should already be in scope at this location (the file uses both). Do not add new top-of-file imports unless `cargo check` complains.

### Step 2 — Update `imports.rs:L15` import

```rust
// Before
use crate::commands::daemon_router::{params_with_file, try_daemon_route};
// After
use crate::commands::daemon_router::{params_with_file_lang, try_daemon_route};
```

If the import line at L15 is structured differently (e.g. multi-line use group), make the equivalent substitution: replace `params_with_file` with `params_with_file_lang`.

### Step 3 — Update `imports.rs:L37` call site

```rust
// Before
if let Some(result) = try_daemon_route::<Vec<ImportInfo>>(project, "imports", params_with_file(&self.file)) {
    // ...
}
// After
if let Some(result) = try_daemon_route::<Vec<ImportInfo>>(
    project,
    "imports",
    params_with_file_lang(&self.file, self.lang.as_ref().map(|l| l.as_str())),
) {
    // ...
}
```

### Step 4 — DO NOT change `imports.rs:L55-L56`

The direct-compute path is already correct:

```rust
let language =
    detect_or_parse_language(self.lang.as_ref().map(|l| l.as_str()), &self.file)?;
```

Leave this UNCHANGED.

### Step 5 — DO NOT change daemon handler

`crates/tldr-daemon/src/handlers/ast.rs:L161-L199` is ALREADY CORRECT. Do NOT modify. Do NOT add any `#[serde(rename)]`.

### Step 6 — `cargo check`

```bash
cargo check -p tldr-cli
```

Expected: clean.

---

## Capture GREEN

```bash
cargo test -p tldr-cli --lib commands::daemon_router::tests 2>&1 | tee continuum/autonomous/v0.2.4-quality/reports/m3-green-capture.txt
# Expect: 2 new tests PASS.

cargo test -p tldr-cli --test cli_p1_tests imports_command 2>&1 | tee -a continuum/autonomous/v0.2.4-quality/reports/m3-green-capture.txt
# Expect: 4 existing + 1 new pass.

# Validation gate
cargo test -p tldr-cli --test exhaustive_matrix --features semantic --release -- --test-threads=1
cargo test -p tldr-cli --test language_command_matrix --features semantic --release
cargo clippy --workspace --all-features --tests -- -D warnings
```

All must be green. Append outputs to the green-capture file.

---

## Constraints

- **No gaming:** if the matrix regresses, fix the root cause.
- **Build complete:** the JSON key MUST be `"language"`. If you ship `"lang"`, the bug ships too (silently ignored by serde). Verify with the unit test asserting `params["language"] == "python"`.
- **No prior-orchestrator artefacts:** only `continuum/autonomous/v0.2.4-quality/` is writable.
- **Disjoint files:** M3 ONLY touches `daemon_router.rs`, `imports.rs`, and `cli_p1_tests.rs`. Do NOT modify `daemon/ipc.rs` (M1) or `surface/csharp.rs` / `surface/java.rs` (M2). Do NOT modify `crates/tldr-daemon/` or `crates/tldr-core/`.
- **No `cargo fmt --all`:** edit-only.
- **No `git push`, no `cargo publish`:** local commit only.
- **`staging_method`:** EXPLICIT `git add <listed-files>` only. FORBIDDEN: `-A`, `.`, `-a`, directory adds. After commit, `git show HEAD --stat | grep -E "continuum/autonomous/(?!v0\.2\.4-quality/)"` must return empty.
- **NO BLOKS:** `bloks_context: null`, `bloks_cards: []` — pass through verbatim.

---

## STOP Conditions (escalate to orchestrator)

| # | Condition | Why |
|---|---|---|
| 1 | Must touch `crates/tldr-daemon/src/handlers/ast.rs` | Already correct; do NOT change |
| 2 | Must touch `crates/tldr-core/src/ast/imports.rs` | Core entry already takes resolved Language |
| 3 | Must touch `imports.rs:L55-L56` (direct-compute) | Already correct |
| 4 | JSON key as `"lang"` (not `"language"`) | Bug would still ship — silently ignored by serde |
| 5 | Must change `ImportsArgs` clap struct | Already `Option<Language>` |
| 6 | More than 3 source files modified | Scope creep |

---

## Disjoint-from-Siblings

| Sibling | Their files | Yours | Conflict? |
|---|---|---|---|
| M1 (IPC) | `daemon/ipc.rs` + new test | `daemon_router.rs`, `imports.rs`, `cli_p1_tests.rs` | None |
| M2 (surface interfaces) | `surface/csharp.rs`, `surface/java.rs` | n/a | None |
| M4 (paperwork) | gh ops only | n/a | None |
| M5 (release prep) | manifests + CHANGELOG | n/a | None |

**Truly independent — runs in parallel with M1 and M2.**

---

## Final Report Shape

Write `continuum/autonomous/v0.2.4-quality/reports/m3-imports-lang-daemon-hint.md`:

- Problem
- Triage drift (line numbers vs starting HEAD)
- Solution (helper added, import + call site updated)
- Per-site fix list (daemon_router.rs new helper + imports.rs L15 + L37)
- RED capture excerpts (compile-error + integration-passes-pre-fix-noted)
- GREEN capture excerpts
- Test inventory (3 new = 2 unit + 1 integration; 4 existing imports tests regression-checked)
- Matrix + lint verification
- Files modified (3: daemon_router.rs + imports.rs + cli_p1_tests.rs)
- Disjointness from M1 / M2
- Commit SHA

Validation JSON `continuum/autonomous/v0.2.4-quality/validation/m3-imports-lang-daemon-hint.json`:

```json
{
  "milestone": "imports-lang-daemon-hint",
  "assertion": "VAL-003",
  "issue": "parcadei/tldr-code#29",
  "starting_head": "10f00a9",
  "commit": "<filled in after commit>",
  "status": "passed",
  "red_capture": "continuum/autonomous/v0.2.4-quality/reports/m3-red-capture.txt",
  "green_capture": "continuum/autonomous/v0.2.4-quality/reports/m3-green-capture.txt",
  "matrix": "964/964",
  "clippy": "clean",
  "tests_added": 3,
  "files_modified": [
    "crates/tldr-cli/src/commands/daemon_router.rs",
    "crates/tldr-cli/src/commands/imports.rs",
    "crates/tldr-cli/tests/cli_p1_tests.rs"
  ],
  "json_key_verified": "language (NOT lang)"
}
```

---

## Final Commit Message Shape

```
fix(M3 VAL-003 v0.2.4): close #29 — imports --lang daemon hint

`tldr imports <file> --lang <LANG>` ignored --lang when a daemon was
running. The daemon fast-path at imports.rs:L37 called
`params_with_file(&self.file)` which emits `{"file": <path>}` only;
self.lang was silently dropped. The daemon then fell back to
extension-based detection and failed on extensionless files.

Direct-compute path (imports.rs:L55-L56) was already correct.
Daemon handler (handlers/ast.rs:L161-L199) was already correct
(ImportsRequest.language deserialised, passed to
detect_or_parse_language). The fix is CLI-side only.

Added params_with_file_lang(file, lang) helper to daemon_router.rs
mirroring the existing params_with_path_lang shape; updated
imports.rs L15 import + L37 call site to use it.

CRITICAL: JSON key is "language" (not "lang") — ImportsRequest.language
has no #[serde(rename)] so a "lang" key would be silently ignored
by serde and the bug would still ship.

Tests added: 3 (2 unit asserting the JSON key + 1 integration
exercising tldr imports myscript --lang python on an extensionless
file). Matrix: 964/964. Clippy: clean.

Closes parcadei/tldr-code#29.
```


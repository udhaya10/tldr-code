# M3b (VAL-003b) — Imports `--lang` direct-compute path

**Closes:** parcadei/tldr-code#29 (second half; M3 closed daemon-routed half)
**Worker:** kraken
**Date:** 2026-04-28
**Starting HEAD:** a3dfbc3 (M3 commit)

## Problem

After M3 closed the daemon-routed path for `tldr imports myscript --lang python`,
the **direct-compute** path (no daemon running) was still broken:

- `crates/tldr-cli/src/commands/imports.rs::run` correctly resolved
  `--lang python` and called `get_imports(file, language)` with the right
  `Language` enum value.
- `crates/tldr-core/src/ast/imports.rs::get_imports` then called
  `parse_file(path)` which **discarded** the caller-supplied language and
  re-detected purely from path extension.
- For an extensionless file like `myscript`, `ParserPool::parse_file`
  fell through to `TldrError::UnsupportedLanguage("unknown")`.

## RED Reproduction (HEAD a3dfbc3)

```
$ printf 'import os\nimport sys\n' > /tmp/myscript-m3b
$ ~/.cargo/bin/tldr imports /tmp/myscript-m3b --lang python --format json --quiet
Error: Unsupported language: unknown
```

Test capture: `m3b-red-capture.txt`

```
test imports_command::test_imports_lang_flag_extensionless_file ... FAILED
stderr=```"Error: Unsupported language: unknown\n"```
```

## Decision: option (b) — analogous sibling, NOT signature change

`ParserPool` already exposed `parse_with_path` as a sibling to `parse`
(threading a path hint for TS/TSX dialect). M3b adds an analogous sibling
`parse_file_with_lang(path, lang_hint: Option<TldrLanguage>)` to
`parse_file`. The hint, when supplied, takes precedence over path-extension
detection.

Rationale:
- `parse_file` has 15+ callers across `crates/tldr-core` and
  `crates/tldr-cli` (metrics, references, impact, dead, clones, search,
  bugbot, dead_code). Changing its signature would cascade through every
  caller and trigger the "more than 4 source files modified" stop
  condition.
- `get_imports` already takes `Language` non-optionally — it just
  threw it away. No signature change to `get_imports` is needed; only
  the body is updated to forward `Some(language)` as the hint.
- New free function `parse_file_with_lang` mirrors `parse_with_path`
  pattern and is a non-breaking, additive API.

Net behavior change is contained to one call site (`get_imports`).
Existing `parse_file` callers retain their existing semantics
(extension-based detection) because they delegate to
`parse_file_with_lang(path, None)` underneath.

## Files Changed

| File | Lines | Change |
|------|-------|--------|
| `crates/tldr-core/src/ast/parser.rs` | +56 / −6 | New method `ParserPool::parse_file_with_lang(path, Option<TldrLanguage>)`; existing `parse_file` delegates with `None`; new free function `parse_file_with_lang` mirrors. |
| `crates/tldr-core/src/ast/imports.rs` | +9 / −3 | Switch `get_imports` from `parse_file` to `parse_file_with_lang(path, Some(language))`; doc updated. |
| `crates/tldr-cli/tests/cli_p1_tests.rs` | +29 / −0 | New RED→GREEN test `test_imports_lang_flag_extensionless_file`. |

Total: **3 source files modified** (under the 4-file stop threshold).
Zero callers of `parse_file` other than `imports.rs` needed updating —
they continue to use extension-based detection by design.

## GREEN Verification

### Test
```
test imports_command::test_imports_lang_flag_extensionless_file ... ok
test result: ok. 1 passed; 0 failed
```

### Binary verification
```
$ printf 'import os\nimport sys\n' > /tmp/myscript-m3b-verify
$ ./target/release/tldr imports /tmp/myscript-m3b-verify --lang python --format json --quiet
[
  {
    "module": "os",
    "is_from": false
  },
  {
    "module": "sys",
    "is_from": false
  }
]
exit: 0
```

2 imports detected — bug closed end-to-end with the actual binary.

## Validation

- `cargo clippy --workspace --all-features --tests -- -D warnings`: clean
- `cargo test -p tldr-core --lib ast`: 264 passed, 0 failed
- `cargo test -p tldr-cli imports`: 10 + 12 + 6 = 28 imports-matching tests pass (1 ignored, 0 failed)

## Stop Conditions

None hit. Three source files modified (under 4-file threshold). No daemon /
ipc / surface code touched. Pre-existing dirty `continuum/autonomous/*`
files left untouched.

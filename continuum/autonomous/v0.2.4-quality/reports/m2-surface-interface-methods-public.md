# M2 — VAL-002 — Surface Interface Methods Always Public (closes #26)

**Worker:** kraken (M2 VAL-002 v0.2.4)
**Issue:** parcadei/tldr-code#26
**Starting HEAD:** `10f00a9`
**Status:** GREEN

---

## Problem

`tldr surface` dropped C# and Java interface methods unless `--include-private`
was set. Both languages allow interface methods to OMIT the `public` modifier
(it's implicitly public per language spec); the existing per-method visibility
predicates required an EXPLICIT `public` (Java) or `public`/`protected` (C#)
keyword on the method's source line. The predicate returned `false`, the method
loop's visibility guard `continue`d, and the methods were silently filtered out.

The Rust extractor already handled this correctly via a trait short-circuit at
`crates/tldr-core/src/surface/rust_lang.rs:L174-L180`. The fix mirrors that
pattern in C# and Java.

## Triage Drift vs Starting HEAD

Verified on HEAD `10f00a9`:

| Symbol | Brief | Actual |
|---|---|---|
| C# method-loop visibility guard | `csharp.rs:127` | `csharp.rs:127` ✓ |
| `is_csharp_member_visible` | `csharp.rs:190-192` | `csharp.rs:190-192` ✓ |
| `determine_csharp_kind` | `csharp.rs:233-247` | `csharp.rs:233` ✓ |
| C# inline tests module | `csharp.rs:297-411` | `csharp.rs:297-411` ✓ |
| Java method-loop visibility guard | `java.rs:126` | `java.rs:126` ✓ |
| `is_java_public_at_line` | `java.rs:213-215` | `java.rs:213-215` ✓ |
| `determine_java_kind` | `java.rs:228-241` | `java.rs:228` ✓ |
| Java inline tests module | `java.rs:294-419` | `java.rs:294-419` ✓ |
| Reference: Rust trait short-circuit | `rust_lang.rs:174-180` | `rust_lang.rs:174-180` ✓ |

Zero drift.

## Solution

Mirror the Rust trait short-circuit pattern. Bind `is_interface = kind ==
ApiKind::Interface;` immediately after `let kind = determine_*_kind(...)`,
then extend the per-method visibility guard with `&& !is_interface` so
interface methods bypass the public-keyword check.

`ApiKind::Interface` is already imported in both files via
`use super::types::{..., ApiKind, ...}`. No new helpers, no new AST traversal,
no struct-shape changes.

## Per-Site Fix List

### `crates/tldr-core/src/surface/csharp.rs`

- L107-L108: added `let is_interface = kind == ApiKind::Interface;` after
  `let kind = determine_csharp_kind(class, &source);`.
- L126-L135: extended visibility guard with `&& !is_interface`. Block now reads:
  ```rust
  if !include_private
      && !is_interface
      && !is_csharp_member_visible(&source, method.line_number as usize)
  {
      continue;
  }
  ```

### `crates/tldr-core/src/surface/java.rs`

- L106-L107: added `let is_interface = kind == ApiKind::Interface;` after
  `let kind = determine_java_kind(class, &source);`.
- L125-L134: extended visibility guard with `&& !is_interface`. Same shape.

**Source delta:** 4 LOC across 2 files (2 lines added per file: binding +
condition extension; comments don't count toward the LOC budget).

## RED Capture (on HEAD 10f00a9, BEFORE fix applied)

```
test surface::csharp::tests::test_extract_csharp_surface_interface_methods_are_public_by_default ... FAILED
panicked at crates/tldr-core/src/surface/csharp.rs:451:9:
Execute should be in surface: ["example.IService"]

test surface::java::tests::test_extract_java_surface_interface_methods_are_public_by_default ... FAILED
panicked at crates/tldr-core/src/surface/java.rs:455:9:
execute should be in surface: ["example.com.example.IService"]
```

Both surfaces emitted ONLY the interface type itself (`IService`) and ZERO
methods. The fix's expected behavior (3 methods present in surface) was
violated, confirming the bug.

Full RED stdout: `continuum/autonomous/v0.2.4-quality/reports/m2-red-capture.txt`.

## GREEN Capture (after fix)

```
running 6 tests
test surface::csharp::tests::test_extract_csharp_surface_interface_methods_are_public_by_default ... ok
test surface::csharp::tests::test_extract_csharp_surface_filters_non_public_members ... ok
test surface::csharp::tests::test_extract_csharp_surface_includes_private_when_requested ... ok
test surface::csharp::tests::test_truncate_docstring_handles_unicode_char_boundaries ... ok
test surface::csharp::tests::test_find_csharp_files_recurses ... ok
test surface::csharp::tests::test_compute_csharp_module_path_strips_nested_src_segment ... ok
test result: ok. 6 passed; 0 failed.
```

```
test surface::java::tests::test_extract_java_surface_interface_methods_are_public_by_default ... ok
test surface::java::tests::test_extract_java_surface_filters_package_private_members ... ok
test surface::java::tests::test_extract_java_surface_includes_non_public_when_requested ... ok
test surface::java::tests::test_compute_java_module_path_strips_nested_src_main_java_prefix ... ok
test surface::java::tests::test_truncate_docstring_handles_unicode_char_boundaries ... ok
test surface::java::tests::test_find_java_files_recurses ... ok
test result: ok. 42 passed; 0 failed.   (java + javascript prefix-matched)
```

Whole-surface result: **351 passed; 0 failed**.

Full GREEN stdout: `continuum/autonomous/v0.2.4-quality/reports/m2-green-capture.txt`.

## Test Inventory

| Test | File | Status |
|---|---|---|
| **NEW** `test_extract_csharp_surface_interface_methods_are_public_by_default` | `csharp.rs:419` | RED→GREEN |
| **NEW** `test_extract_java_surface_interface_methods_are_public_by_default` | `java.rs:421` | RED→GREEN |
| `test_truncate_docstring_handles_unicode_char_boundaries` (cs) | `csharp.rs:311` | green (regression) |
| `test_find_csharp_files_recurses` | `csharp.rs:323` | green (regression) |
| `test_extract_csharp_surface_filters_non_public_members` | `csharp.rs:333` | green (regression) |
| `test_extract_csharp_surface_includes_private_when_requested` | `csharp.rs:374` | green (regression) |
| `test_compute_csharp_module_path_strips_nested_src_segment` | `csharp.rs:402` | green (regression) |
| `test_truncate_docstring_handles_unicode_char_boundaries` (java) | `java.rs:308` | green (regression) |
| `test_find_java_files_recurses` | `java.rs:320` | green (regression) |
| `test_extract_java_surface_filters_package_private_members` | `java.rs:338` | green (regression) |
| `test_extract_java_surface_includes_non_public_when_requested` | `java.rs:381` | green (regression) |
| `test_compute_java_module_path_strips_nested_src_main_java_prefix` | `java.rs:410` | green (regression) |

**Net new tests:** 2. Existing tests at `csharp.rs:297-411` / `java.rs:294-419`:
zero regressions.

## Verification

| Check | Result |
|---|---|
| `cargo test -p tldr-core --lib surface::csharp` | 6/6 pass |
| `cargo test -p tldr-core --lib surface::java` | 6/6 pass (matched 42 incl. prefix) |
| `cargo test -p tldr-core --lib surface` | 351/351 pass |
| `cargo clippy --workspace --all-features --tests -- -D warnings` | clean |

## Files Modified

| File | Source LOC delta | Test LOC delta |
|---|---|---|
| `crates/tldr-core/src/surface/csharp.rs` | +2 (+ comment) | +63 (1 new test) |
| `crates/tldr-core/src/surface/java.rs` | +2 (+ comment) | +59 (1 new test) |

**Total:** 2 source files, 0 new test files (inline only).

## Disjointness from Sibling Milestones

| Sibling | Their files | M2 files | Conflict? |
|---|---|---|---|
| M1 (IPC) | `crates/tldr-cli/src/commands/daemon/ipc.rs` + new test | `crates/tldr-core/src/surface/{csharp,java}.rs` | None |
| M3 (imports --lang) | `daemon_router.rs`, `imports.rs`, `cli_p1_tests.rs` | n/a | None |
| M4 (paperwork) | gh ops only | n/a | None |
| M5 (release prep) | manifests + CHANGELOG | n/a | None |

Truly independent — runs in parallel with M1 and M3.

## STOP Conditions

None triggered. Specifically:
- Did not touch any file other than `csharp.rs` and `java.rs`.
- Did not change `ApiKind` or `ApiEntry` shape.
- Did not touch `determine_*_kind` logic.
- Did not add a Java 9 private-in-interface refinement (per orchestrator: KEEP IT SIMPLE).
- Existing surface tests in both modules still pass.

## Commit SHA

`bc2fa83` (fix(M2 VAL-002): close #26 — surface emits interface methods without --include-private)

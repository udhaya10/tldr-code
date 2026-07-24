# Canonical Traversal Policy Refactor

> Status: **Plan of record.** Execute against this document.
> Scope: consolidate tldr-core's duplicated project-traversal logic onto one
> canonical walker (`walker.rs` / `ProjectWalker`).
> Out of scope: callgraph skip-list divergence (see [Follow-up beads](#out-of-scope-follow-up-beads)).

## Problem

The codebase walks projects through **two independent walkers** that have
**drifted apart** in both behavior and policy:

| Walker | Engine | Honors `.gitignore` | Honors `.tldrignore` | Used by |
|---|---|---|---|---|
| `walker.rs` — `ProjectWalker` / `walk_project` (`walker.rs:240`, `:517`) | `ignore::WalkBuilder` | **Yes** | Yes (gitignore semantics) | quality/metrics/contracts/semantic cluster (~20 sites) |
| `fs/tree.rs` — `get_file_tree` (`fs/tree.rs:119`) | raw `walkdir::WalkDir` | **No** | Yes (root-level only) | structure/analysis cluster (~13 sites) |

`walker.rs:1-5` already declares itself the single walk point ("Every
project-wide filesystem walk in tldr should go through this module instead of
using `walkdir::WalkDir` directly") — yet `fs/tree.rs` reimplements walking on
raw `walkdir` with its own ignore loader, exclude list, and sentinel detector.

### Concrete duplication today

- **Exclude lists — split four ways**, each hand-maintained, each different:
  - `fs/tree.rs:32` `DEFAULT_SKIP_DIRS`
  - `walker.rs:163` `DEFAULT_EXCLUDE_DIRS`
  - `callgraph/scanner.rs:158` `SKIP_DIRECTORIES` + `scanner.rs:453` `should_skip_directory`
  - `callgraph/module_index.rs:1761` `should_skip_directory` *(diverges from scanner.rs)*
- **Generated-dir detection — byte-for-byte duplicated**: `GENERATED_DIR_SENTINELS`
  + `dir_has_generated_sentinel` at both `fs/tree.rs:80`/`:84` and
  `walker.rs:210`/`:443`.
- **`.gitignore` semantic gap**: `walker.rs` honors it; `fs/tree.rs` does not.
  Routing `get_file_tree` through `ProjectWalker` closes this gap for free
  across all its callers.
- **`MAX_FILE_SIZE`** declared at `fs/tree.rs:22` but never referenced there
  (dead vestige). Real oversize enforcement lives in `fs/oversize.rs`.
- **Standalone re-walk** inside `is_corpus_file_impl` (`semantic/chunker.rs:463`)
  rebuilds its own `WalkBuilder`; the accept logic is still live (it is what
  `CorpusPolicy::accepts_path` calls into), but its walker setup is duplicate.
- **Third independent walker**: `metrics/file_utils.rs:67` `walk_source_files`
  (own `WalkOptions`), used by `commands/cognitive.rs` and `commands/halstead.rs`.

### `get_file_tree` callers (verified by grep — these are the refactor's blast radius)

`ast/extractor.rs:119` (structure extraction), `analysis/importers.rs:39`,
`analysis/deps.rs:445`, `analysis/arch_rules.rs:118`,
`analysis/change_impact.rs:960`, `analysis/impact.rs:654`,
`context/builder.rs:513` (context builder), `patterns/mod.rs:322`,
`cli/commands/tree.rs:82`, `cli/commands/daemon/daemon.rs:300` + `:892`,
`mcp/tools/ast.rs:41`, `daemon/handlers/ast.rs:61`.

> Note: **"Text search" is NOT a `get_file_tree` caller** — it uses `walk_project`.
> Removed from the original Phase 2 list on that basis.

## Target state

One canonical walker (`walker.rs` / `ProjectWalker`) exposing a uniform
classification surface, with `fs/tree.rs` becoming a thin adapter that produces
`FileTree` output from a `ProjectWalker` walk.

### `PathClass` (new — Phase 1)

```rust
pub enum PathClass {
    Eligible,      // supported ext, within size limit, not binary
    Ignored,       // matched .gitignore / .tldrignore / .git/info/exclude
    Hidden,        // dotfile/dotdir (when exclude_hidden is on)
    Unsupported,   // extension not in supported set / no language
    Generated,     // under a default-skip dir or sentinel-marked generated dir
    Binary,        // content sniff: not UTF-8 / not text
    Oversized,     // stat().len() > MAX_FILE_SIZE
}
```

**Cost-asymmetry decision (pinned here):** `Ignored`/`Hidden`/`Unsupported`/
`Generated` are **pure-path** (no I/O); `Binary`/`Oversized` require `stat()`
(byte sniff for binary). Implementation splits cleanly:

- `classify_path(path, is_dir) -> PathClass` — cheap, no I/O; resolves the four
  path classes. Applied **during** the walk (this is what `ProjectWalker::iter`
  already does internally via `filter_entry`).
- `classify_content(path)` — expensive; resolves the remaining
  `Eligible`/`Binary`/`Oversized`. Applied **only on the post-filter file set**
  (files the walker already yielded), preserving today's performance — we never
  stat/sniff files that cheap path filters already dropped.

Public `PathClass` is the single classification surface callers reason about;
the cheap/expensive split is an internal perf concern.

## Phases

### Phase 1 — Add `PathClass` + `classify()` on `ProjectWalker`

- Define `PathClass` in `walker.rs`.
- Implement `classify_path` (pure-path) by extracting the filtering logic
  already inline in `ProjectWalker::iter` (`walker.rs:369-409`).
- Implement `classify_content` by consolidating the existing homes:
  `Oversized` ← `fs/oversize.rs`; `Binary` ← `fs/mod.rs`
  `read_to_string_tolerant` `NonUtf8` outcome.
- Keep `ProjectWalker::iter`'s output identical (cheap filters still applied
  in-walk); `classify()` is the new uniform query surface, not a behavior change.
- **Acceptance**: `walker.rs` unit tests still pass; new unit tests cover each
  `PathClass` variant.

### Phase 2 — Route `fs/tree.rs::get_file_tree` through `ProjectWalker`

- Replace the raw `walkdir` body (`fs/tree.rs:213-303` `build_tree_children`)
  with a `ProjectWalker` walk, projected into `FileTree`.
- Delete `build_gitignore` (`fs/tree.rs:181`) — `.tldrignore`/`.gitignore` are
  now handled by `ProjectWalker`.
- Keep `get_file_tree(...)` signature and `IgnoreSpec` param for API
  compatibility; `IgnoreSpec` converts into `ProjectWalker` options internally.
- **No caller changes required.** All ~13 callers above keep working.
- **Free win**: the `.gitignore` semantic gap closes for every `get_file_tree`
  caller.
- **Acceptance**: `fs/tree.rs` tests pass unchanged; `cli tree` output on a
  fixture with `.gitignore` now excludes gitignored paths.

### Phase 3 — Remove duplicate policy logic

**Already done (verify only, do not re-do):** semantic indexing
(`semantic/chunker.rs:576` `enumerate_corpus_files`), enriched search
(`:598` `corpus_stats_for_language`), and freshness (`semantic/store_search.rs:139`)
already route through `ProjectWalker` + `CorpusPolicy`.

**Do in this phase:**
- Collapse the two `DEFAULT_*` lists onto `walker.rs:163` `DEFAULT_EXCLUDE_DIRS`;
  delete `fs/tree.rs:32` `DEFAULT_SKIP_DIRS`.
- Delete the duplicate `dir_has_generated_sentinel` + `GENERATED_DIR_SENTINELS`
  in `fs/tree.rs:80`/`:84`; keep the `walker.rs:210`/`:443` copies.
- Delete dead `MAX_FILE_SIZE` at `fs/tree.rs:22`.
- Extract the duplicate `WalkBuilder` setup out of `is_corpus_file_impl`
  (`semantic/chunker.rs:463`) into the shared walker helper. **The accept
  logic stays** (it is what `CorpusPolicy::accepts_path` calls into); only the
  standalone re-walk goes away.
- Fold `metrics/file_utils.rs:67` `walk_source_files` into `ProjectWalker`;
  migrate `commands/cognitive.rs` + `commands/halstead.rs`.
- Keep `IgnoreSpec` only as a compatibility input that converts into policy
  options (full retirement is Phase 4).

### Phase 4 — Migrate callers, retire `IgnoreSpec`

- Audit every `IgnoreSpec` call site. Most pass `IgnoreSpec::default()` or
  `None` (verified: `context/builder.rs:513`, `analysis/importers.rs:39`,
  `deps.rs:445`, `arch_rules.rs:118`, `change_impact.rs` — all `default()`).
- Where `default()`/`None`: drop the parameter.
- Where a real pattern is supplied (`patterns/mod.rs:322` passes a populated
  `ignore_spec`; `ast/extractor.rs:119` threads one through): convert to
  explicit `ProjectWalker` options.
- Then remove `IgnoreSpec` (or reduce it to a thin options converter).
- **Acceptance**: `cargo build` across all four crates; no `IgnoreSpec`
  references remain outside the (possibly retained) converter.

### Phase 5 — Regression fixture

One fixture containing: valid C++ source; CSV + log files (expected
`Unsupported`); `.gitignore` exclusions; `.tldrignore` exclusions; nested
ignore rules; generated directories; oversized source; negation attempts.

Verify these agree on the same corpus:
**tree, semantic indexing, freshness, watcher, enriched search, counters.**

Preserve the existing invariant at `walker.rs:623` — `.tldrignore` negation
cannot re-include a path already denied.

### Phase 6 — Validate and clean up

1. `cargo test -p tldr-core` (focused) first.
2. `cargo test` (full suite).
3. Keep unrelated C++ language/formatting failures **separate** unless the
   refactor exposes a real regression.

## Out-of-scope follow-up beads

- **Callgraph skip-list divergence.** `callgraph/scanner.rs:158`/`:453` and
  `callgraph/module_index.rs:1761` each maintain their own `should_skip_directory`
  and they diverge (e.g. `module_index.rs` has `.svn`/`.hg`/`env`/`.env` that
  `scanner.rs` lacks; `scanner.rs` has `.idea`/`.vscode`/`Pods`/`.bundle`/
  `__pypackages__`/`.eggs`/`htmlcov` that `module_index.rs` lacks). **Out of
  scope here** — callgraph scanning is not gated by `CorpusPolicy`'s language
  allow-list and carries ecosystem-specific entries. File as a separate bead;
  do not fold into Phase 3.

## Key references

- Canonical walker to extend: `crates/tldr-core/src/walker.rs`
  (`ProjectWalker:240`, `walk_project:517`, `DEFAULT_EXCLUDE_DIRS:163`)
- Walker to fold in: `crates/tldr-core/src/fs/tree.rs`
  (`get_file_tree:119`, `build_gitignore:181`, `DEFAULT_SKIP_DIRS:32`)
- Semantic corpus policy (model for shared-policy pattern):
  `crates/tldr-core/src/semantic/chunker.rs`
  (`CorpusPolicy:188`, `is_corpus_file:459`, `enumerate_corpus_files:576`)
- Oversize: `crates/tldr-core/src/fs/oversize.rs`
- Binary sniff: `crates/tldr-core/src/fs/mod.rs` `read_to_string_tolerant`
- Tests to keep green: `crates/tldr-core/tests/fs_tests.rs`,
  `crates/tldr-core/tests/ast_tests.rs`,
  `semantic/chunker.rs` corpus-contract tests,
  `daemon/watcher.rs` watcher tests.

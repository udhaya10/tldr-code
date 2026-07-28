# Additive Ignore Policy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Git ignore sources an immutable deny floor while allowing `.tldrignore` to exclude additional paths, with identical full-walk, single-file, call-graph, and watcher decisions.

**Architecture:** Keep `ignore::WalkBuilder` as the recursive traversal engine and upgrade `ignore` to 0.4.31. Replace the root-only hand-built matcher with two `IncrementalIgnore` matchers built from canonical configurations: one for Git/standard sources and one for `.tldrignore`. A path is ignored when either independent matcher returns an ignore decision, so `.tldrignore` negation may reopen its own earlier rule but can never reopen a Git-ignored path. Full traversal keeps the efficient native walker and applies this additive matcher as a final policy filter; delta and deleted-path checks reuse the same matcher directly.

**Tech Stack:** Rust, `ignore` 0.4.31, `tempfile`, Cargo tests, Beads.

**Implementation status (2026-07-28):** Completed. Git-aware matching keeps
`ignore`'s repository-boundary behavior; non-repository projects can use the
standard `.ignore` source. Focused core and watcher tests, all 16 certification
contracts, formatting, and `cargo check --workspace --all-targets` pass.

---

### Task 1: Freeze the additive policy with failing integration tests

**Files:**
- Create: `crates/tldr-core/tests/additive_ignore_policy.rs`

- [ ] **Step 1: Add a shared fixture helper**

```rust
use std::fs;
use std::path::{Path, PathBuf};

use tldr_core::semantic::is_corpus_file;
use tldr_core::walker::{build_path_ignore_matcher, ProjectWalker};

fn write(root: &Path, relative: &str, contents: &str) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture parent");
    fs::write(&path, contents).expect("write fixture");
    path
}

fn walked_files(root: &Path) -> Vec<PathBuf> {
    ProjectWalker::new(root)
        .extensions(&["py"])
        .iter()
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(|entry| entry.path().to_path_buf())
        .collect()
}
```

- [ ] **Step 2: Add a Git hard-floor regression**

```rust
#[test]
fn tldrignore_cannot_reinclude_gitignored_file() {
    let project = tempfile::tempdir().expect("temp project");
    let secret = write(project.path(), "nested/secret.py", "SECRET = 1\n");
    write(project.path(), "nested/.gitignore", "secret.py\n");
    write(project.path(), "nested/.tldrignore", "!secret.py\n");

    assert!(!walked_files(project.path()).contains(&secret));
    assert!(!is_corpus_file(project.path(), &secret));
}
```

- [ ] **Step 3: Add `.tldrignore`-internal negation and nested parity**

```rust
#[test]
fn tldrignore_may_reopen_only_its_own_exclusion() {
    let project = tempfile::tempdir().expect("temp project");
    let dropped = write(project.path(), "nested/drop.py", "DROP = 1\n");
    let kept = write(project.path(), "nested/keep.py", "KEEP = 1\n");
    write(project.path(), "nested/.tldrignore", "*.py\n!keep.py\n");

    let walked = walked_files(project.path());
    assert!(!walked.contains(&dropped));
    assert!(walked.contains(&kept));
    assert!(!is_corpus_file(project.path(), &dropped));
    assert!(is_corpus_file(project.path(), &kept));
}
```

- [ ] **Step 4: Add a vanished-path matcher regression**

```rust
#[test]
fn deleted_nested_ignored_path_is_still_rejected() {
    let project = tempfile::tempdir().expect("temp project");
    write(project.path(), "nested/.tldrignore", "generated/\n");
    let vanished = project.path().join("nested/generated/gone.py");

    let matcher = build_path_ignore_matcher(project.path(), true).expect("matcher");
    assert!(matcher.is_ignored(&vanished, false));
}
```

- [ ] **Step 5: Run the tests and verify the old split policy fails**

Run:

```bash
cargo test -p tldr-core --test additive_ignore_policy -- --nocapture
```

Expected: at least the `.tldrignore`-internal negation/parity case fails before implementation.

### Task 2: Upgrade the ignore engine

**Files:**
- Modify: `Cargo.toml:130`
- Modify: `crates/tldr-core/Cargo.toml:66`
- Modify: `Cargo.lock`

- [ ] **Step 1: Pin the approved current version**

```toml
ignore = "=0.4.31"
```

- [ ] **Step 2: Refresh only this dependency**

Run:

```bash
cargo update -p ignore --precise 0.4.31
cargo tree -i ignore --depth 1
```

Expected: `ignore v0.4.31` with `tldr-core` as its direct consumer.

### Task 3: Replace root-only parsing with canonical incremental matchers

**Files:**
- Modify: `crates/tldr-core/src/walker.rs:42-147`
- Modify: `crates/tldr-core/src/walker.rs:364-505`
- Modify: `crates/tldr-core/src/walker.rs:545-618`

- [ ] **Step 1: Introduce independently cached Git and TLDR matchers**

Use `WalkBuilder::build_matchers()` to create one matcher with standard/Git sources and a second matcher with Git sources disabled plus `.tldrignore` registered as the custom filename. Store each `IncrementalIgnore` behind `Arc<Mutex<_>>` so the existing cloneable, thread-safe `PathIgnoreMatcher` API remains intact.

- [ ] **Step 2: Implement deny-wins matching**

For absolute or relative input paths, normalize them against each matcher root and return ignored when either matcher returns `is_ignore()`. Do not concatenate ignore files and do not strip `!`; each source retains its own Git-compatible last-match semantics while the two source families compose with boolean OR.

- [ ] **Step 3: Keep recursive walking efficient**

Keep the existing combined `WalkBuilder` for native directory pruning, then apply `PathIgnoreMatcher` in `filter_entry` as the additive correctness floor. This prevents a high-precedence `.tldrignore` whitelist from reopening a Git-ignored entry.

- [ ] **Step 4: Make `classify_path` authoritative for nested ignore files**

Update its documentation and implementation to use the incremental matcher rather than the root-only approximation. Preserve hidden, generated-directory, extension, and JS/TS-preservation policies as distinct layers.

- [ ] **Step 5: Run focused walker tests**

Run:

```bash
cargo test -p tldr-core --test additive_ignore_policy -- --nocapture
```

Expected: all additive policy cases pass.

### Task 4: Remove the shallow single-file walk

**Files:**
- Modify: `crates/tldr-core/src/semantic/chunker.rs:470-579`

- [ ] **Step 1: Replace ancestor-pruned traversal**

Build one canonical `PathIgnoreMatcher`, reject the target when it is ignored, and check each root-to-parent directory against `DEFAULT_EXCLUDE_DIRS` and generated-directory sentinels. Preserve canonical-root containment, source-language, hidden/binary, and JS/TS-preserved-directory checks.

- [ ] **Step 2: Assert full/single-file parity**

Extend the integration test fixture so every candidate path has identical `ProjectWalker` membership and `is_corpus_file` membership.

- [ ] **Step 3: Run semantic and walker tests**

Run:

```bash
cargo test -p tldr-core additive_ignore -- --nocapture
cargo test -p tldr-core corpus_file -- --nocapture
```

Expected: all relevant unit and integration tests pass.

### Task 5: Reload nested policy changes in the live watcher

**Files:**
- Modify: `crates/tldr-cli/src/commands/daemon/watcher.rs:155-180`
- Modify: `crates/tldr-cli/src/commands/daemon/watcher.rs:496-535`

- [ ] **Step 1: Detect policy files at any depth**

Reload the matcher when an event path’s filename is `.gitignore` or `.tldrignore`, not only when it equals the project-root policy path.

- [ ] **Step 2: Add nested reload tests**

Create `nested/.gitignore` and `nested/.tldrignore`, mutate each during a live matcher session, call `reload_for_paths`, and assert the new decision is observed.

- [ ] **Step 3: Run watcher tests**

Run:

```bash
cargo test -p tldr-cli commands::daemon::watcher::tests -- --nocapture
```

Expected: root and nested policy reload tests pass.

### Task 6: Extend the cross-surface contract and validate

**Files:**
- Modify: `crates/tldr-contract-tests/src/main.rs:696-720`

- [ ] **Step 1: Extend `ignore_matcher_unification`**

Cover nested `.gitignore`, nested `.tldrignore`, `.tldrignore` internal negation, Git hard-floor behaviour, and a vanished ignored path through the public matcher.

- [ ] **Step 2: Run all focused gates**

Run:

```bash
cargo fmt --all -- --check
cargo test -p tldr-core --test additive_ignore_policy
cargo test -p tldr-cli commands::daemon::watcher::tests
cargo run -p tldr-contract-tests -- ignore_matcher_unification
cargo check --workspace --all-targets
```

Expected: formatting, focused tests, contract scenario, and workspace construction all pass.

- [ ] **Step 3: Update Beads and synchronize**

Record the final semantics, dependency version, tests, and any deliberately retained limitations on `TLDR-boa.8`; close it only after every gate passes. Relate the result to `TLDR-1hld.2`, then run `bd dolt push`.

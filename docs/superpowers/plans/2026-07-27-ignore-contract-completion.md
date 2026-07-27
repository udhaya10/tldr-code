# Ignore Contract Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close TLDR-bpf by routing callgraph filtering through the shared tldr-core ignore matcher and reloading root ignore policy during a live daemon session.

**Architecture:** `tldr_core::walker::build_path_ignore_matcher` remains the sole matcher loader. The callgraph scanner consumes its opaque `PathIgnoreMatcher`, while the daemon watcher owns a small lock-protected live matcher that atomically replaces its snapshot when `.tldrignore` or `.gitignore` changes.

**Tech Stack:** Rust 2021, `ignore`, `parking_lot::RwLock`, notify, tempfile, Cargo contract and workspace gates.

---

## File Structure

- Modify `crates/tldr-core/src/callgraph/scanner.rs`: remove the private ignore loader and delegate filtering to the shared matcher.
- Modify `crates/tldr-cli/src/commands/daemon/watcher.rs`: add reloadable matcher state and focused unit tests.
- Modify `crates/tldr-contract-tests/src/main.rs`: certify root-anchored shared matcher behavior through the callgraph consumer.

### Task 1: Add the matcher-unification certification case

**Files:**
- Modify: `crates/tldr-contract-tests/src/main.rs`

- [x] **Step 1: Register the certification scenario**

Add:

```rust
Scenario {
    name: "ignore_matcher_unification",
    smoke: false,
    run: ignore_matcher_unification,
},
```

- [x] **Step 2: Add a root-anchored behavior proof**

Create root and nested `corpus` files, then assert callgraph filtering excludes only the root-anchored path:

```rust
fn ignore_matcher_unification() -> Result<(), String> {
    use tldr_core::callgraph::scanner::filter_tldrignored;

    let project = tempfile::tempdir().map_err(display)?;
    let root_ignored = project.path().join("corpus/root.py");
    let nested_kept = project.path().join("nested/corpus/kept.py");
    fs::create_dir_all(root_ignored.parent().ok_or("root parent missing")?).map_err(display)?;
    fs::create_dir_all(nested_kept.parent().ok_or("nested parent missing")?).map_err(display)?;
    fs::write(&root_ignored, "def root(): pass\n").map_err(display)?;
    fs::write(&nested_kept, "def nested(): pass\n").map_err(display)?;
    fs::write(project.path().join(".tldrignore"), "/corpus/\n").map_err(display)?;

    let filtered =
        filter_tldrignored(project.path(), vec![root_ignored.clone(), nested_kept.clone()]);
    ensure(!filtered.contains(&root_ignored), "root-anchored ignore was not honored")?;
    ensure(filtered.contains(&nested_kept), "root-anchored ignore matched a nested directory")
}
```

- [x] **Step 3: Run certification before implementation**

Run:

```bash
cargo tldr-certification
```

Expected: the new case passes against the existing scanner semantics, establishing a behavior-preserving refactor baseline.

### Task 2: Remove the private callgraph matcher loader

**Files:**
- Modify: `crates/tldr-core/src/callgraph/scanner.rs`

- [x] **Step 1: Replace private ignore imports**

Remove `ignore::gitignore::{Gitignore, GitignoreBuilder}` and import:

```rust
use crate::walker::build_path_ignore_matcher;
```

- [x] **Step 2: Delete `load_tldrignore` and route filtering through shared core**

Use:

```rust
pub fn filter_tldrignored(root: &Path, paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let Some(ignore) = build_path_ignore_matcher(root, false) else {
        return paths;
    };
    paths
        .into_iter()
        .filter(|path| !ignore.is_ignored(path, path.is_dir()))
        .collect()
}
```

- [x] **Step 3: Run the certification case**

Run:

```bash
cargo tldr-certification
```

Expected: all certification cases pass with the shared matcher as the callgraph consumer.

### Task 3: Add failing live-reload unit coverage

**Files:**
- Modify: `crates/tldr-cli/src/commands/daemon/watcher.rs`

- [x] **Step 1: Add a reload lifecycle test**

Add a unit test that constructs the live matcher with no policy, writes `.tldrignore`, reloads, verifies an ignored path, removes the policy, reloads again, and verifies the path is admitted:

```rust
#[test]
fn ignore_policy_reloads_during_a_live_session() {
    let project = tempfile::tempdir().expect("project");
    let ignored = project.path().join("generated/file.rs");
    let live = LiveIgnoreMatcher::new(project.path());
    assert!(!live.is_ignored(&ignored, false));

    std::fs::write(project.path().join(".tldrignore"), "generated/\n").expect("write");
    assert!(live.reload_for_paths(&[project.path().join(".tldrignore")]));
    assert!(live.is_ignored(&ignored, false));

    std::fs::remove_file(project.path().join(".tldrignore")).expect("remove");
    assert!(live.reload_for_paths(&[project.path().join(".tldrignore")]));
    assert!(!live.is_ignored(&ignored, false));
}
```

- [x] **Step 2: Add a non-policy event test**

Prove ordinary source events do not rebuild matcher state:

```rust
#[test]
fn ordinary_source_event_does_not_reload_ignore_policy() {
    let project = tempfile::tempdir().expect("project");
    let live = LiveIgnoreMatcher::new(project.path());
    std::fs::write(project.path().join(".tldrignore"), "generated/\n").expect("write");
    assert!(!live.reload_for_paths(&[project.path().join("src/lib.rs")]));
    assert!(!live.is_ignored(&project.path().join("generated/file.rs"), false));
}
```

- [x] **Step 3: Run the focused tests and verify they fail to compile**

Run:

```bash
cargo test -p tldr-cli ignore_policy_ --all-features
```

Expected: compilation fails because `LiveIgnoreMatcher` is not yet defined.

### Task 4: Implement atomic watcher matcher reload

**Files:**
- Modify: `crates/tldr-cli/src/commands/daemon/watcher.rs`

- [x] **Step 1: Add lock-protected live matcher state**

Define:

```rust
struct LiveIgnoreMatcher {
    project: PathBuf,
    matcher: parking_lot::RwLock<Option<PathIgnoreMatcher>>,
}

impl LiveIgnoreMatcher {
    fn new(project: &Path) -> Self {
        Self {
            project: project.to_path_buf(),
            matcher: parking_lot::RwLock::new(build_path_ignore_matcher(project, true)),
        }
    }

    fn reload_for_paths(&self, paths: &[PathBuf]) -> bool {
        let reload = paths.iter().any(|path| {
            path == &self.project.join(tldr_core::walker::TLDRIGNORE_FILE)
                || path == &self.project.join(".gitignore")
        });
        if reload {
            *self.matcher.write() = build_path_ignore_matcher(&self.project, true);
        }
        reload
    }

    fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        self.matcher
            .read()
            .as_ref()
            .is_some_and(|matcher| matcher.is_ignored(path, is_dir))
    }
}
```

- [x] **Step 2: Reload once before processing event paths**

Change `WatchHandler.ignore` to `LiveIgnoreMatcher`, call `reload_for_paths(&event.paths)` before the event loop, and pass the current decision through `LiveIgnoreMatcher::is_ignored`. Change `watch_decision` to accept `Option<&LiveIgnoreMatcher>` so existing focused tests may continue passing `None`.

- [x] **Step 3: Initialize live state at watcher startup**

Replace the one-time `build_path_ignore_matcher` snapshot with `LiveIgnoreMatcher::new(&project)` and update the startup comment to state that root policies reload in-session.

- [x] **Step 4: Run focused watcher tests**

Run:

```bash
cargo test -p tldr-cli commands::daemon::watcher::tests --all-features
```

Expected: all watcher tests, including the two reload cases, pass.

### Task 5: Validate and deliver the epic

**Files:**
- Modify: Beads records and this plan after gates pass.

- [x] **Step 1: Run formatting and diff hygiene**

Run:

```bash
cargo fmt --all --check
git diff --check
```

Expected: both exit zero.

- [x] **Step 2: Run workspace lint and tests**

Run:

```bash
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --all-features -- -D warnings
CARGO_INCREMENTAL=0 cargo test --workspace --all-targets --all-features
```

Expected: both exit zero.

- [x] **Step 3: Run contract profiles**

Run:

```bash
cargo tldr-smoke
cargo tldr-certification
```

Expected: smoke and certification report zero failures.

- [x] **Step 4: Close Beads work**

Record gate evidence and close `TLDR-9w8`, `TLDR-1m4`, then `TLDR-bpf`.

- [ ] **Step 5: Commit and push**

Commit the matcher unification and reload implementation, pull/rebase from `fork/main`, run `bd dolt push`, push `main` to `fork`, and verify a clean worktree synchronized with `fork/main`.

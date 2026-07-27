# Notify Debounce and Burst Coalescing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Execute this plan inline task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the in-daemon watcher coalesce save storms into bounded batch deltas while escalating tree-wide churn to one full rebuild.

**Architecture:** Replace the library-level watcher debounce with raw `notify` events feeding a bounded Tokio channel. A single worker owns a deterministic batch state machine: it deduplicates paths, flushes after quiet or the first-event max-wait deadline, tracks accepted events in a rolling window, and clears queued deltas before scheduling the daemon's existing single-flight warm rebuild. Batch delta work crosses the async/blocking boundary once.

**Tech Stack:** Rust, Tokio `mpsc`/timers, `notify` via `notify-debouncer-full`, existing `ArtifactManager`, `IndexManager`, and `TldrConfig`.

---

### Task 1: Add project-resolved watcher configuration

**Files:**
- Modify: `crates/tldr-core/src/config.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/types.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/start.rs`

- [x] **Step 1: Write failing configuration tests**

Add tests that parse and merge:

```json
{
  "watcher": {
    "debounce_ms": 125,
    "max_wait_ms": 900,
    "burst_file_cap": 7,
    "burst_event_cap": 11,
    "burst_window_ms": 250
  }
}
```

Assert every value survives `TldrConfig::from_str`, and that an overlay only replaces fields it explicitly supplies.

- [x] **Step 2: Run the focused tests and verify failure**

Run:

```bash
cargo test -p tldr-core config::tests --lib
```

Expected: compilation fails because `WatcherConfig` and `TldrConfig::watcher` do not exist.

- [x] **Step 3: Implement the configuration surface**

Add a serde-defaulted `WatcherConfig` whose five knobs are `Option<u64>`/`Option<usize>`, add it to `TldrConfig`, and deep-merge only `Some` values. Add corresponding concrete fields to `DaemonConfig` with defaults:

```rust
watcher_debounce_ms: 750,
watcher_max_wait_ms: 5_000,
watcher_burst_file_cap: 200,
watcher_burst_event_cap: 1_000,
watcher_burst_window_ms: 2_000,
```

Add `DaemonConfig::resolve(project)` to overlay `TldrConfig::resolve(Some(project))`, and use it in daemon startup instead of `DaemonConfig::default()`.

- [x] **Step 4: Run the focused tests**

Run:

```bash
cargo test -p tldr-core config::tests --lib
```

Expected: all configuration tests pass.

### Task 2: Implement the deterministic debounce state machine

**Files:**
- Modify: `crates/tldr-cli/src/commands/daemon/watcher.rs`

- [x] **Step 1: Write failing state-machine tests**

Use synthetic `Instant` values to prove:

```text
same path at 0/100/200ms -> one delta flush at 950ms
continuous accepted events -> flush no later than first + max_wait
201 pending paths -> FullRebuild and empty pending state
1001 accepted events inside 2s -> FullRebuild, including across quiet flushes
```

- [x] **Step 2: Run the focused tests and verify failure**

Run:

```bash
cargo test -p tldr-cli watcher::tests --lib
```

Expected: compilation fails because `WatchPipeline` and `Flush` do not exist.

- [x] **Step 3: Implement `WatchPipeline`**

Create a worker-owned state object containing the pending path set, first/last batch timestamps, and a `VecDeque<Instant>` for the rolling event window. `accept` rearms quiet time, prunes expired rolling events, and returns `FullRebuild` when either strict cap is exceeded. `flush_due` returns one deduplicated `Delta(Vec<PathBuf>)` after quiet or max wait. Full rebuild resets pending and rolling state.

- [x] **Step 4: Run the focused tests**

Run:

```bash
cargo test -p tldr-cli watcher::tests --lib
```

Expected: all watcher state-machine tests pass.

### Task 3: Batch daemon deltas through one blocking job

**Files:**
- Modify: `crates/tldr-cli/src/commands/daemon/daemon.rs`

- [x] **Step 1: Add the batch entry point**

Refactor `process_dirty_file(file)` into a singleton wrapper over:

```rust
pub(crate) async fn process_dirty_files(
    &self,
    files: Vec<PathBuf>,
) -> ReindexOutcome
```

Deduplicate paths, update dirty bookkeeping, invalidate per-file hot-cache keys and the project key, then enter exactly one `spawn_blocking` closure. Inside it, apply authoritative artifact deltas serially, then apply semantic deltas serially against the resulting artifact generation and attach the vector generation once.

- [x] **Step 2: Add the full-rebuild scheduler**

Expose an async watcher-only helper that clears dirty accounting, invalidates the resident semantic store, and calls the existing `start_warm_build` single-flight path. A second burst while a rebuild is active must report/log `already_building`, never start another build.

- [x] **Step 3: Compile the daemon**

Run:

```bash
cargo check -p tldr-cli --all-features
```

Expected: successful compilation.

### Task 4: Wire the raw watcher to the bounded pipeline

**Files:**
- Modify: `crates/tldr-cli/src/commands/daemon/watcher.rs`

- [x] **Step 1: Replace library debounce with raw events**

Construct `notify::recommended_watcher`, keep `watch_decision` and the pre-corpus liveness tap, and `try_send` each accepted path to a bounded channel. Size the channel to observe the configured event cap. On overflow, set a shared atomic rebuild flag; never block the notify thread.

- [x] **Step 2: Implement the serialized worker loop**

The worker owns `WatchPipeline` and selects between the next batch deadline and channel input. A delta flush calls `process_dirty_files` once. A cap/overflow flush drains queued events, clears pending state, and calls the single-flight rebuild scheduler instead of applying per-file deltas.

- [x] **Step 3: Re-run watcher and daemon checks**

Run:

```bash
cargo test -p tldr-cli watcher::tests --lib
cargo check -p tldr-cli --all-features
```

Expected: tests and compilation pass.

### Task 5: Validate, close, commit, and publish

**Files:**
- Verify all modified files

- [x] **Step 1: Run formatting and focused tests**

```bash
cargo fmt --all -- --check
cargo test -p tldr-core config::tests --lib
cargo test -p tldr-cli watcher::tests --lib
```

- [x] **Step 2: Run repository quality gates**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo check --workspace --all-targets --all-features
```

Expected: all gates pass.

- [x] **Step 3: Close and publish**

```bash
bd close TLDR-ac0.7
git add <exact modified files>
git commit -m "feat(watcher): debounce and cap delta bursts"
git pull --rebase
bd dolt push
git push
git status
```

Expected: `main` is clean and up to date with `fork/main`.

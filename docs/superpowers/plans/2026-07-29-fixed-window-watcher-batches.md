# Fixed-Window Watcher Batches Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Collect filesystem changes in fixed five-second windows while executing completed batches serially through a bounded queue.

**Architecture:** Keep the synchronous OS callback limited to non-blocking raw-event enqueue. Split the existing async worker into a collector, which owns timing and coalescing state, and an executor, which drains completed batches one at a time. A batch deadline is always five seconds after its first event; indexing never blocks collection of the next window.

**Tech Stack:** Rust, Tokio bounded MPSC channels, notify, existing daemon artifact and semantic delta pipelines.

---

### Task 1: Replace quiet debounce with a fixed collection window

**Files:**
- Modify: `crates/tldr-cli/src/commands/daemon/watcher.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/types.rs`
- Test: `crates/tldr-cli/src/commands/daemon/watcher.rs`

- [ ] **Step 1: Write failing timing tests**

Add tests proving that events at 0 ms and 4,900 ms flush together at 5,000 ms,
and that events after the deadline form a new batch:

```rust
let start = Instant::now();
let mut pipeline = WatchPipeline::new(config());
pipeline.accept(PathBuf::from("a.rs"), start);
pipeline.accept(PathBuf::from("b.rs"), start + Duration::from_millis(4_900));
assert!(pipeline.flush_due(start + Duration::from_millis(4_999)).is_none());
assert_eq!(
    pipeline.flush_due(start + Duration::from_millis(5_000)),
    Some(Flush::Delta(vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")]))
);
```

- [ ] **Step 2: Verify the current quiet-debounce behavior fails**

Run:

```bash
cargo test -p tldr-cli fixed_window --lib
```

Expected: the current pipeline flushes before the five-second boundary.

- [ ] **Step 3: Make the first-event deadline authoritative**

Remove per-file quiet-time deadline calculation. Return only
`first_event + max_wait` from `WatchPipeline::deadline`. Retain the pending
path map for deduplication and the rolling event history for burst escalation.

- [ ] **Step 4: Change the default collection duration**

Set both legacy-compatible watcher timing defaults to 5,000 ms so old config
surfaces resolve consistently. Document `watcher_max_wait_ms` as the fixed
collection window and `watcher_debounce_ms` as a compatibility field.

- [ ] **Step 5: Run watcher timing tests**

Run:

```bash
cargo test -p tldr-cli commands::daemon::watcher --lib
```

Expected: fixed-window, duplicate-save, burst-cap, and ignore-policy tests pass.

### Task 2: Collect subsequent windows while indexing

**Files:**
- Modify: `crates/tldr-cli/src/commands/daemon/watcher.rs`
- Test: `crates/tldr-cli/src/commands/daemon/watcher.rs`

- [ ] **Step 1: Write a failing queue/serialization test**

Use a test executor whose first batch waits on a gate. Submit a second completed
batch while the gate is closed and assert it is queued but not started. Release
the gate and assert execution order is exactly Batch 1 then Batch 2.

- [ ] **Step 2: Verify the current worker cannot collect concurrently**

Run:

```bash
cargo test -p tldr-cli watcher_batches_queue_while_indexing --lib
```

Expected: failure because the collector currently awaits
`process_dirty_files`.

- [ ] **Step 3: Add a bounded completed-batch queue**

Create a second Tokio MPSC channel in `spawn_watcher`. The collector sends
`Flush` values to it without running indexing. A dedicated executor owns the
receiver and calls `dispatch_flush` sequentially:

```rust
async fn run_batch_executor(
    daemon: Arc<TLDRDaemon>,
    mut rx: mpsc::Receiver<Flush>,
) {
    while let Some(flush) = rx.recv().await {
        dispatch_flush(&daemon, flush).await;
    }
}
```

Size the completed-batch queue from the configured burst bounds. If enqueue
cannot proceed because the queue is full, clear pending raw deltas and enqueue
or schedule one full rebuild rather than dropping freshness.

- [ ] **Step 4: Preserve ordering and fallback behavior**

Keep one executor only. Full rebuild signals reset collector state, and burst
file/event caps or raw-channel overflow continue escalating rather than
creating an unbounded backlog. ArtifactManager's writer remains the final
generation-order guard.

- [ ] **Step 5: Run focused and full quality gates**

Run:

```bash
cargo fmt --check
cargo test -p tldr-cli commands::daemon::watcher --lib
cargo test -p tldr-cli dirty_file_batch_publishes_each_final_revision --lib
cargo clippy -p tldr-cli --all-targets --all-features -- -D warnings
```

Expected: all commands pass.

### Task 3: Install and validate live behavior

**Files:**
- Modify: Beads issue `TLDR-735f`

- [ ] **Step 1: Install and restart release binaries**

Run the repository release install, restart the project daemon, and verify the
watcher is active.

- [ ] **Step 2: Exercise two adjacent windows**

Create changes in the first five-second window, keep its executor busy, then
create changes in the next window. Confirm two ordered artifact generations and
no concurrent semantic writer.

- [ ] **Step 3: Record results and close the issue**

Append configuration, observed timing, queue ordering, tests, and any remaining
limitations to `TLDR-735f`, then close it only if the acceptance criteria pass.

- [ ] **Step 4: Commit and push**

Commit the implementation, tests, plan, and intended Beads export. Push Beads
and `main` to the configured fork, then verify `main...fork/main` is synchronized.

# Ignore Policy Rebuild Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every ignore-policy change replace structural and semantic indexes without adding work to query execution.

**Architecture:** The watcher sends a typed full-rebuild signal when `.tldrignore`, `.gitignore`, `.ignore`, or `.git/info/exclude` changes. The daemon invalidates the resident semantic generation and runs the existing full warm path, which re-discovers the corpus and atomically publishes replacement artifact and vector generations; an atomic pending bit coalesces policy changes that arrive during an active warm into one follow-up build.

**Tech Stack:** Rust, Tokio, notify, existing artifact store and usearch generation managers, Beads.

---

### Task 1: Route policy changes to the full-rebuild pipeline

**Files:**
- Modify: `crates/tldr-cli/src/commands/daemon/watcher.rs`
- Test: `crates/tldr-cli/src/commands/daemon/watcher.rs`

- [x] Introduce a `WatchSignal` enum with `Path(PathBuf)` and `FullRebuild`.
- [x] Make `WatchHandler::handle_event` enqueue `FullRebuild` immediately after atomically reloading an ignore policy.
- [x] Preserve the existing overflow fallback and ordinary `Path` delta behavior.
- [x] Add focused tests proving policy changes become full rebuilds while ordinary source changes remain deltas.

### Task 2: Coalesce rebuild requests across an active warm

**Files:**
- Modify: `crates/tldr-cli/src/commands/daemon/daemon.rs`
- Test: `crates/tldr-cli/src/commands/daemon/daemon.rs`

- [x] Add one synchronized pending-full-rebuild state to `TLDRDaemon`.
- [x] Claim a pending request when a new warm starts.
- [x] After a warm completes, consume a pending request, invalidate any generation published by the superseded build, and run one replacement warm.
- [x] Add focused tests for request claiming, coalescing, and follow-up consumption.

### Task 3: Validate and close tracking

**Files:**
- Modify: `.beads/interactions.jsonl` through `bd`

- [x] Run watcher and daemon focused tests.
- [x] Run formatting, relevant crate tests, and compilation checks.
- [x] Record the verified behavior in `TLDR-1hld.11` and the parent epic.
- [x] Commit, synchronize Beads, rebase, and push the completed change.

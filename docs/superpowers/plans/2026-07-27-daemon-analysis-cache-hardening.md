# Daemon Analysis Cache Hardening Implementation Plan

> **For maintainers:** This plan records provisional review findings. Reproduce and review each finding before treating it as a permanent architectural conclusion.

**Goal:** Validate and harden the daemon-backed cache and routing used by structural analysis commands while preserving local/daemon result parity.

**Architecture:** Keep the existing per-project daemon, Salsa-backed structural state, and bounded query cache. Strengthen the boundaries around path normalization, cache dependency bookkeeping, request/key construction, routing policy, and concurrent computation. Treat serialization and invalidation changes as measured optimizations, not assumptions.

**Tech Stack:** Rust, Tokio, DashMap, Salsa, Serde, Beads, Criterion-style timing tests.

---

### Task 1: Reproduce relative-path cache misses

**Files:**
- Modify: `crates/tldr-cli/src/commands/daemon/daemon.rs`
- Modify: relevant daemon integration tests under `crates/tldr-cli/tests/`

**Steps:**
1. Add an integration test that invokes a cacheable structural command twice with a relative project path.
2. Assert that the second request is a daemon cache hit and matches the absolute-path response.
3. Normalize the request root at the daemon boundary before project-membership checks and cache-key construction.
4. Run the focused daemon cache tests and representative `calls` and `structure` commands.

### Task 2: Correct cache dependency bookkeeping

**Files:**
- Modify: `crates/tldr-cli/src/commands/daemon/salsa.rs`

**Steps:**
1. Add a unit test that replaces an existing key with a different dependency set.
2. Assert that invalidating an old dependency does not remove the replacement entry.
3. Unregister the key from the old entry's reverse-dependency sets during replacement.
4. Add a corruption test asserting that removing an unreadable entry also corrects byte and dependency accounting.
5. Remove the exact corrupt entry atomically and apply the complete byte, dependency, and statistics cleanup.

### Task 3: Audit daemon/local request and response parity

**Files:**
- Inspect: command routers under `crates/tldr-cli/src/commands/`
- Inspect: daemon request handlers under `crates/tldr-cli/src/commands/daemon/`

**Steps:**
1. Inventory cacheable commands, request fields, cache-key fields, local result types, and daemon result types.
2. Cross-check findings with `TLDR-7vb` and `TLDR-7pp.1.4`.
3. Design a typed registration boundary that derives routing, cache identity, and response conversion from one definition.
4. Add parity tests before migrating commands.

### Task 4: Unify daemon routing contracts

**Files:**
- Modify: `crates/tldr-cli/src/commands/daemon/daemon_router.rs`
- Modify: legacy command routers identified by the parity audit

**Steps:**
1. Document the current strict and legacy routing APIs accurately.
2. Identify commands that silently fall back after a daemon routing failure.
3. Align the implementation with the accepted routing ADR and existing issues `TLDR-14i` and `TLDR-npl`.
4. Add tests for daemon unavailable, protocol mismatch, and explicit local execution.

### Task 5: Prevent duplicate concurrent computation

**Files:**
- Modify: `crates/tldr-cli/src/commands/daemon/salsa.rs`
- Modify: daemon concurrency tests

**Steps:**
1. Add a test that sends concurrent identical cold requests and counts computations.
2. Introduce per-key single-flight coordination without holding the cache map across analysis work.
3. Verify one computation serves all waiters and errors do not poison the key.
4. Benchmark cold contention and uncontended warm hits.

### Task 6: Measure invalidation, eviction, and transport costs

**Files:**
- Modify: daemon benchmarks under `crates/tldr-cli/tests/`
- Inspect: serialization and transport work tracked by `TLDR-kd6` and `TLDR-n74`

**Steps:**
1. Measure project-wide invalidation after representative single-file edits.
2. Measure eviction latency at the entry and byte limits.
3. Separate analysis time from JSON serialization, IPC transfer, and client deserialization.
4. Propose narrower invalidation, constant-time LRU, or binary transport only where measurements justify the complexity.

### Task 7: Review and decide

**Files:**
- Modify: this plan and the corresponding Beads epic

**Steps:**
1. Attach reproduction evidence and benchmark results to each child issue.
2. Obtain maintainer review of correctness, compatibility, and performance conclusions.
3. Mark disproved findings explicitly rather than silently deleting them.
4. Replace the provisional label only after the review records a rollout decision.

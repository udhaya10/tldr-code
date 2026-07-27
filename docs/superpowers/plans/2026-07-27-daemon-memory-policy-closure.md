# Daemon Memory Policy Closure Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close TLDR-cxa with an evidence-backed decision on the remaining memory-pressure escape-hatch requirement.

**Architecture:** Treat process isolation and bounded fixed-shape inference as the escape hatch already shipped after TLDR-yll was written. Preserve presence-based liveness and avoid adding a parent-daemon max-memory kill switch that would make daemon-required commands unavailable; retain status observability and track macOS physical-footprint accuracy separately in TLDR-d3s.

**Tech Stack:** Beads decision records, existing redb resumable worker checkpoints, fixed-shape ONNX benchmark evidence.

---

### Task 1: Reconcile the historical risk with current evidence

**Files:**
- Modify: Beads records only.

- [x] **Step 1: Verify the historical failure**

Confirm TLDR-30f reproduced the old unbounded FastEmbed path at a 25.75 GiB peak and routed the allocator defect to TLDR-vbw0.

- [x] **Step 2: Verify the replacement architecture**

Confirm TLDR-9bxa.10 runs bulk inference in a disposable, checkpointed child process and TLDR-9bxa.11 measured the fixed-shape candidate at 3,029,172,224 bytes (2.82 GiB), below the declared 4 GiB ceiling.

- [x] **Step 3: Record the policy decision**

Record that no parent-daemon max-memory kill switch will be added: the bounded child worker is the explicit memory-pressure escape hatch, its exit returns allocator memory to the OS, and redb checkpoints make recycling resumable. Keep TLDR-d3s open as an observability correction rather than a blocker for liveness semantics.

### Task 2: Close the remaining child and epic

**Files:**
- Modify: Beads records and this plan.

- [x] **Step 1: Update TLDR-yll**

Replace the stale pre-fixed-shape description with the resolved design and measured evidence, then close it.

- [x] **Step 2: Update and close TLDR-cxa**

Record that all six children are complete and close the epic.

- [x] **Step 3: Commit and sync**

Commit the plan and Beads export, pull/rebase from `fork/main`, push Beads, push code to `fork/main`, and verify a clean synchronized worktree.

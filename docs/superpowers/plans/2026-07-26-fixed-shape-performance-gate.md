# Fixed-Shape Performance Gate Implementation Plan

> **For Codex:** Execute this plan inline and keep `TLDR-9bxa.5` as the authoritative Beads issue.

**Goal:** Measure the fixed-shape ONNX path across every Arctic model and supported shape, then enforce the 4 GiB RSS and 10% throughput-regression rollout gates with reproducible JSON evidence.

**Architecture:** Add a reusable performance-report module with deterministic statistics and gate evaluation. Add an example benchmark whose parent process launches one fresh worker per model, preventing allocator/session retention from contaminating cross-model RSS. Each worker builds exact 128/256/384/512-token inputs, measures the FastEmbed oracle, drops it, then measures the fixed-shape backend over heterogeneous cycles while sampling RSS.

**Tech Stack:** Rust, `fastembed`, `ort`, `serde`, existing fixed-shape planner/backend, existing cross-platform RSS helpers.

---

### Task 1: Define performance evidence and rollout gates

**Files:**
- Create: `crates/tldr-core/src/semantic/fixed_shape_benchmark.rs`
- Modify: `crates/tldr-core/src/semantic/mod.rs`
- Test: `crates/tldr-core/src/semantic/fixed_shape_benchmark.rs`

- [x] Define serializable latency, throughput, RSS plateau, per-shape, per-model, and matrix reports.
- [x] Compute mean/p50/p95/max latency and requests per second deterministically.
- [x] Evaluate peak RSS <= 4 GiB, final-window plateau growth, and fixed throughput no worse than 10% below the oracle.
- [x] Add unit tests for percentile selection, regression boundaries, plateau boundaries, and aggregate pass/fail behavior.

### Task 2: Build the isolated empirical benchmark

**Files:**
- Create: `crates/tldr-core/examples/fixed_shape_bench.rs`

- [x] Add a parent mode that launches one clean worker process for each supported Arctic model.
- [x] Generate raw texts whose tokenized lengths land exactly in the 128/256/384/512 buckets.
- [x] Measure equal full batches through FastEmbed and the direct fixed-shape ORT backend.
- [x] Drop the oracle before constructing the candidate backend and sample candidate RSS throughout heterogeneous shape cycles.
- [x] Emit one machine-readable JSON matrix and return failure when any rollout gate fails.

### Task 3: Validate and record the checkpoint

**Files:**
- Modify: Beads issue `TLDR-9bxa.5`

- [x] Run focused unit tests and formatting/lint checks.
- [x] Run the complete all-model/all-shape benchmark and preserve its measured summary in Beads.
- [x] Review the resulting diff for correctness and maintainability.
- [x] Commit, rebase, push Beads, push Git, and verify the branch is clean and synchronized.

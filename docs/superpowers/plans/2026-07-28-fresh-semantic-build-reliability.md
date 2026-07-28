# Fresh Semantic Build Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make an empty-state semantic build correct, bounded, observable,
resumable, cleanly cancellable, and fast enough to publish within 30 minutes on
the documented 558-file M2 Max benchmark.

**Architecture:** Replace the corpus-sized `0/1` worker transaction with a
versioned stable recipe, an up-front deterministic plan, bounded durable batches,
staged generation records, and live progress events. Treat every existing
one-batch job as incompatible disposable state; correctness of a clean rebuild is
more important than recovering it. Publish only a fully verified generation, but
retain every completed bounded batch across crashes.

**Tech Stack:** Rust, redb, rkyv, usearch, Tokio, JSONL worker IPC, existing
`tldr-contract-tests` scenario runner, macOS launchd lifecycle tests.

---

## File responsibility map

- `crates/tldr-core/src/semantic/worker_protocol.rs`: stable recipe and versioned
  progress/event wire types.
- `crates/tldr-core/src/semantic/redb_store.rs`: durable job plan, batch
  checkpoints, staged vector identities, invalidation reason, and schema checks.
- `crates/tldr-core/src/semantic/vector_store.rs`: deterministic planning,
  bounded batch execution, cache reconciliation, and phase telemetry.
- `crates/tldr-core/src/semantic/generation.rs`: reconstruct and publish a
  verified generation from committed staged batches.
- `crates/tldr-core/src/semantic/build_metrics.rs`: stage timing and throughput
  fields shared by benchmarks and status.
- `crates/tldr-cli/src/bin/tldr_embed_worker.rs`: real batch loop and durable
  event acknowledgement.
- `crates/tldr-cli/src/commands/daemon/bulk_worker.rs`: concurrent child event
  reader, cancellation, process reaping, and final report.
- `crates/tldr-cli/src/commands/daemon/index_manager.rs`: shared live progress
  snapshot and resident publication.
- `crates/tldr-cli/src/commands/daemon/status.rs`: JSON/compact operator
  progress surface.
- `crates/tldr-cli/src/commands/daemon/daemon.rs`: cancellation ownership and
  generation join.
- `crates/tldr-cli/src/commands/init.rs`: launchd stop/unload contract.
- `crates/tldr-contract-tests/src/main.rs`: shared smoke/certification lifecycle
  scenarios.
- `crates/tldr-contract-tests/fixtures/semantic-build/`: small deterministic
  semantic corpus and expected identities.
- `docs/FRESH_INSTALL_BENCHMARK_2026-07-28.md`: before/after benchmark evidence.

## Task 1: Specify the state machine and compatibility policy (`TLDR-bjux.9`)

**Files:**

- Create: `docs/SEMANTIC_BUILD_STATE_MACHINE.md`
- Modify: `docs/DAEMON_SEMANTIC_ARCHITECTURE.md`

- [ ] **Step 1: Document the stable recipe**

Define this exact logical shape and specify canonical encoding:

```rust
pub struct BuildRecipe {
    pub schema_version: u32,
    pub artifact_generation: u64,
    pub artifact_digest: [u8; 32],
    pub model_id: String,
    pub model_revision: String,
    pub tokenizer_revision: String,
    pub pipeline_version: String,
    pub chunking_version: String,
    pub token_budget_version: String,
    pub enrichment_version: String,
}
```

State explicitly that temp paths, retry limits, PID, timestamps, IPC paths, and
launch configuration are not recipe fields.

- [ ] **Step 2: Document state transitions**

```text
Unplanned -> Planned -> Running -> Verifying -> Published
                    \-> Cancelled
                    \-> RetryableFailure -> Running
Incompatible(any non-Published state) -> Invalidated -> Unplanned
```

Specify the atomic boundary: vector/cache identities and `next_batch` become
durable in one transaction or neither does.

- [ ] **Step 3: Freeze the legacy policy**

Document that worker protocol/job schema versions preceding this epic are
invalidated and rebuilt from zero. They do not consume retry budget and are not
migrated.

- [ ] **Step 4: Review the document against all eight problem issues**

Run:

```bash
bd show TLDR-bjux
bd dep tree TLDR-bjux
```

Expected: every problem issue maps to a named state, invariant, or status field.

- [ ] **Step 5: Commit**

```bash
git add docs/SEMANTIC_BUILD_STATE_MACHINE.md docs/DAEMON_SEMANTIC_ARCHITECTURE.md
git commit -m "docs(semantic): define durable build state machine"
```

## Task 2: Add the profiling and fault harness (`TLDR-bjux.10`)

**Files:**

- Modify: `crates/tldr-contract-tests/src/main.rs`
- Create: `crates/tldr-contract-tests/fixtures/semantic-build/lib.rs`
- Create: `crates/tldr-contract-tests/fixtures/semantic-build/query.toml`
- Modify: `Cargo.toml`

- [ ] **Step 1: Add a failing current-defect scenario**

Add a scenario whose assertion includes:

```rust
ensure(report.planned_batches > 1, "corpus must use bounded batches")?;
ensure(report.completed_vectors > 0, "first checkpoint must retain vectors")?;
ensure(report.progress_events > 1, "progress must be observable before exit")?;
```

- [ ] **Step 2: Add deterministic termination controls**

Extend the shared runner with:

```rust
enum SemanticFault {
    KillBeforeBatch(u64),
    KillAfterBatch(u64),
    CancelDuringBatch(u64),
    FailPublication(PublicationBoundary),
}
```

Use the existing scenario runner and isolated temporary state; do not create a
second test harness.

- [ ] **Step 3: Emit one machine-readable benchmark record**

The record must contain recipe, corpus identity, phase timings, planned and
completed counts, cache hits/misses, CPU/RSS samples, retries, checkpoint age,
publication generation, and query result.

- [ ] **Step 4: Prove the test initially fails**

Run:

```bash
cargo tldr-certification -- semantic-build
```

Expected: failure showing `total_batches=1` or no live progress event.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/tldr-contract-tests
git commit -m "test(semantic): add cold-build fault harness"
```

## Task 3: Implement stable recipe identity (`TLDR-bjux.11`)

**Files:**

- Modify: `crates/tldr-core/src/semantic/worker_protocol.rs`
- Modify: `crates/tldr-core/src/semantic/lineage.rs`
- Modify: `crates/tldr-cli/src/bin/tldr_embed_worker.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/bulk_worker.rs`

- [ ] **Step 1: Write recipe identity tests**

```rust
#[test]
fn recipe_ignores_transport_and_retry_fields() {
    let left = request("/tmp/export-a", 1);
    let right = request("/tmp/export-b", 9);
    assert_eq!(left.build_recipe().fingerprint(), right.build_recipe().fingerprint());
}

#[test]
fn recipe_changes_when_output_input_changes() {
    assert_ne!(recipe_for_model(ArcticM), recipe_for_model(ArcticL));
}
```

- [ ] **Step 2: Add `BuildRecipe` and canonical fingerprinting**

Use length-delimited BLAKE3 fields, matching `EmbeddingRecipeId`; do not hash
serialized `WorkerBuildRequest`.

- [ ] **Step 3: Invalidate before model load**

When protocol or recipe is incompatible, persist `Invalidated { reason }`,
remove incomplete staged state for that job, create a new job, and leave retries
unchanged.

- [ ] **Step 4: Run focused tests**

```bash
cargo test -p tldr-core worker_protocol
cargo test -p tldr-cli recipe
```

Expected: deterministic identity and incompatibility tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/tldr-core/src/semantic/worker_protocol.rs \
  crates/tldr-core/src/semantic/lineage.rs \
  crates/tldr-cli/src/bin/tldr_embed_worker.rs \
  crates/tldr-cli/src/commands/daemon/bulk_worker.rs
git commit -m "fix(semantic): stabilize worker build identity"
```

## Task 4: Implement bounded durable batches (`TLDR-bjux.12`)

**Files:**

- Modify: `crates/tldr-core/src/semantic/redb_store.rs`
- Modify: `crates/tldr-core/src/semantic/vector_store.rs`
- Modify: `crates/tldr-core/src/semantic/generation.rs`
- Modify: `crates/tldr-cli/src/bin/tldr_embed_worker.rs`

- [ ] **Step 1: Add failing checkpoint tests**

```rust
assert_eq!(job.total_batches, 3);
assert_eq!(job.next_batch, 1);
assert_eq!(store.committed_vector_count(job.id)?, first_batch_vectors);
```

Test duplicate, gap, corrupt count, incompatible recipe, and kill before/after
commit.

- [ ] **Step 2: Persist the complete plan**

Store ordered batch descriptors with stable document/vector identities before
inference. Bound a batch at 128 vectors or 60 seconds of accumulated work.

- [ ] **Step 3: Commit batch effects atomically**

Replace empty calls such as:

```rust
ledger.commit_job_batch(&running, &[])?;
```

with a transaction containing the next checkpoint and exact staged
embedding/vector identities.

- [ ] **Step 4: Resume and reconstruct**

Load committed staged vectors, begin at `next_batch`, reject gaps/duplicates,
verify the final count/checksum, build usearch deterministically, then activate
the generation.

- [ ] **Step 5: Run kill-boundary certification**

```bash
cargo tldr-certification -- semantic-build,resume
```

Expected: every interruption converges to the uninterrupted checksum and query.

- [ ] **Step 6: Commit**

```bash
git add crates/tldr-core/src/semantic/{redb_store.rs,vector_store.rs,generation.rs} \
  crates/tldr-cli/src/bin/tldr_embed_worker.rs
git commit -m "fix(semantic): checkpoint bounded vector batches"
```

## Task 5: Stream and expose progress (`TLDR-bjux.13`)

**Files:**

- Modify: `crates/tldr-core/src/semantic/worker_protocol.rs`
- Modify: `crates/tldr-core/src/semantic/inference_runners.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/bulk_worker.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/index_manager.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/status.rs`

- [ ] **Step 1: Define the versioned progress payload**

```rust
pub struct BuildProgress {
    pub phase: BuildPhase,
    pub planned_files: u64,
    pub completed_files: u64,
    pub planned_chunks: u64,
    pub completed_chunks: u64,
    pub total_batches: u64,
    pub next_batch: u64,
    pub completed_vectors: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub elapsed_ms: u64,
    pub checkpoint_age_ms: u64,
    pub retries: u32,
}
```

- [ ] **Step 2: Read events while the worker lives**

Move stdout reading to a dedicated bounded reader thread/channel before entering
the `try_wait` loop. Update shared progress for every valid event.

- [ ] **Step 3: Render honest status**

Calculate percentage only when a nonzero denominator is known. Otherwise emit a
phase plus `"percentage": null` and an explanatory reason.

- [ ] **Step 4: Test slow and malformed workers**

Run:

```bash
cargo test -p tldr-cli bulk_worker
cargo test -p tldr-cli daemon_status
```

Expected: progress is visible before exit; malformed/oversized frames fail
without deadlock.

- [ ] **Step 5: Commit**

```bash
git add crates/tldr-core/src/semantic/{worker_protocol.rs,inference_runners.rs} \
  crates/tldr-cli/src/commands/daemon/{bulk_worker.rs,index_manager.rs,status.rs}
git commit -m "feat(semantic): stream live build progress"
```

## Task 6: Meet the cold-build SLO (`TLDR-bjux.14`)

**Files:**

- Modify: `crates/tldr-core/src/semantic/structural_planner.rs`
- Modify: `crates/tldr-core/src/semantic/vector_store.rs`
- Modify: `crates/tldr-core/src/semantic/fixed_shape.rs`
- Modify: `crates/tldr-core/src/semantic/fixed_shape_embedder.rs`
- Modify: `crates/tldr-core/src/semantic/build_metrics.rs`

- [ ] **Step 1: Capture the phase baseline**

Run the Task 2 harness from empty cache and save the machine-readable record.
Do not optimize until the record identifies planning, inference, or publication
as the dominant stage.

- [ ] **Step 2: Remove repeated token-fit work**

Memoize tokenizer results by `(recipe, document revision)` during planning and
reuse composed candidates rather than retokenizing the same source prefix.

- [ ] **Step 3: Parallelize independent file plans**

Use indexed parallel planning, then sort/flatten by original corpus ordinal so
planned documents and chunk identities remain byte-identical.

- [ ] **Step 4: Tune fixed-shape inference from measurements**

Change batch/thread settings only when the harness shows better throughput
without exceeding 4 GiB RSS or changing vectors beyond the frozen tolerance.

- [ ] **Step 5: Enforce the gate**

```bash
cargo tldr-certification -- semantic-performance
```

Expected on the documented M2 Max: fresh build and publication ≤30 minutes,
peak RSS <4 GiB, successful semantic query, identical retrieval oracle.

- [ ] **Step 6: Commit**

```bash
git add crates/tldr-core/src/semantic
git commit -m "perf(semantic): bound fresh build latency"
```

## Task 7: Correct cancellation and stop (`TLDR-bjux.15`)

**Files:**

- Modify: `crates/tldr-cli/src/commands/daemon/bulk_worker.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/daemon.rs`
- Modify: `crates/tldr-cli/src/commands/init.rs`
- Modify: `crates/tldr-core/src/semantic/worker_protocol.rs`

- [ ] **Step 1: Add a busy-worker shutdown test**

Assert the command does not report success until worker and daemon PIDs are gone
and launchd no longer lists the service.

- [ ] **Step 2: Implement cancellation acknowledgement**

Persist `Cancelled`, send/observe cancellation, TERM the process group, wait a
bounded grace period, KILL only the remaining exact child group, and reap it.

- [ ] **Step 3: Align launchd state**

An explicit managed stop must boot out the exact service label. Explicit
start/init may bootstrap it again. Status must report registry, process, and
launchd disagreement as an error.

- [ ] **Step 4: Run lifecycle tests**

```bash
cargo test -p tldr-cli daemon_stop
cargo tldr-certification -- semantic-cancel
```

Expected: no orphan or automatic respawn; completion within 5 seconds.

- [ ] **Step 5: Commit**

```bash
git add crates/tldr-cli/src/commands/{init.rs,daemon} \
  crates/tldr-core/src/semantic/worker_protocol.rs
git commit -m "fix(daemon): stop semantic worker tree reliably"
```

## Task 8: Add the certification matrix (`TLDR-bjux.16`)

**Files:**

- Modify: `crates/tldr-contract-tests/src/main.rs`
- Modify: `crates/tldr-contract-tests/fixtures/semantic-build/query.toml`

- [ ] **Step 1: Add table rows for every owned boundary**

Cover kill before/after batch commit, cache/checkpoint divergence, incompatible
recipe, retryable crash, cancellation, stage/verify/activate publication faults,
and daemon restart.

- [ ] **Step 2: Keep one implementation**

Generate cold/warm, uninterrupted/resumed, and CLI/MCP projections from the same
scenario and typed expectation. Do not create new test modules or fixture trees.

- [ ] **Step 3: Run smoke and certification**

```bash
cargo tldr-smoke
cargo tldr-certification
```

Expected: smoke contains one fast resume case; certification covers all
boundaries and leaves no process or temporary state.

- [ ] **Step 4: Run repository quality gates**

```bash
cargo fmt --all --check
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets -- -D warnings
CARGO_INCREMENTAL=0 cargo test --workspace
```

Expected: all commands pass.

- [ ] **Step 5: Commit**

```bash
git add crates/tldr-contract-tests
git commit -m "test(semantic): certify crash-safe fresh builds"
```

## Task 9: Run clean-install release certification (`TLDR-bjux.17`)

**Files:**

- Modify: `docs/FRESH_INSTALL_BENCHMARK_2026-07-28.md`
- Modify: relevant Beads epic/task records

- [ ] **Step 1: Remove only verified tldr-owned state**

Inventory exact paths and hashes first. Do not attempt to resume the old
`bulk-b7e8addadd3a5083d50936b3e9e1646a` job.

- [ ] **Step 2: Build and install current HEAD**

```bash
cargo build --release --locked -p tldr-cli --bins
cargo install --path crates/tldr-cli --locked --force
```

Record source commit and installed/release binary hashes.

- [ ] **Step 3: Run one fresh init/warm**

Capture progress snapshots, phase timings, checkpoint cadence, CPU/RSS and disk
until the index reaches warm. Do not issue a second init/warm concurrently.

- [ ] **Step 4: Validate installed surfaces**

Run structural queries, semantic query, hook injection, session stats, MCP
initialize/tools-list, cancellation, stop, and explicit restart.

- [ ] **Step 5: Update evidence and close only proven work**

Update the benchmark and Beads records with exact measurements. Close the epic
only when every acceptance gate passes from empty state.

- [ ] **Step 6: Commit and push**

```bash
git pull --rebase fork main
bd dolt push
git push fork main
git status
```

Expected: Git and Beads are pushed; branch is clean and synchronized.

## Self-review

- Spec coverage: all eight problem issues map to Tasks 3–7; harness,
  certification, and clean rerun are Tasks 2, 8, and 9.
- Compatibility: the plan explicitly discards old one-batch jobs; no task is
  dedicated to salvaging the current benchmark state.
- Correctness order: state machine and harness precede implementation;
  certification precedes clean release rerun.
- No duplicate persistence owner: redb remains authoritative; usearch remains a
  rebuildable published artifact.
- No duplicate test architecture: all scenarios extend the existing shared
  `tldr-contract-tests` runner.

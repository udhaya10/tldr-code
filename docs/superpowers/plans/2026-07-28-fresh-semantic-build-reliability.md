# Fresh Semantic Build Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a fresh semantic build finish predictably, explain where its time
was spent, expose useful live progress, and restart without repeating completed
inference.

**Architecture:** Keep the existing embedding cache as the durable record of
completed inference. Walk the corpus in deterministic fixed-size windows, reuse
cache hits, embed and immediately cache only misses, and rebuild the usearch
generation from those cached vectors. Job counters are advisory progress, not a
second source of vector truth; the new generation becomes visible only through
the existing verified atomic publication path. Extend the existing build metrics
with correlated structural, semantic, process, and atomic-unit timings.

**Tech Stack:** Rust, existing redb embedding cache/job ledger, usearch, Tokio,
JSONL worker IPC, existing `tldr-contract-tests` runner.

---

## Design constraints

This plan intentionally does not add:

- a second `BuildRecipe` model alongside `ManifestId` and
  `EmbeddingRecipeId`;
- a transaction spanning the job ledger, embedding cache, staged vectors, and
  generation publication;
- a persisted whole-corpus plan solely to calculate an exact percentage;
- a time-based checkpoint rule in addition to a fixed window size;
- preselected tokenizer memoization, parallel planning, or inference tuning
  before profiling identifies the bottleneck;
- a second tracing framework alongside the existing `BuildMetrics` and
  `PhaseRecord` types;
- unbounded per-file or per-batch timing records in default operation;
- launchd lifecycle changes on the semantic-build critical path.

The simple recovery rule is:

```text
deterministic window
  -> look up every document in the embedding cache
  -> embed and immediately cache only misses
  -> report reconciled counters
  -> continue
  -> verify the complete vector set
  -> atomically publish the usearch generation
```

After a crash, planning and cache lookup may repeat. Successful model inference
must not repeat when its cache record is compatible and intact.

## File responsibility map

- `crates/tldr-core/src/semantic/lineage.rs`: existing
  `EmbeddingRecipeId`, which identifies compatible vector values.
- `crates/tldr-core/src/semantic/build_metrics.rs`: shared run, process, phase,
  and bounded atomic-unit timing report.
- `crates/tldr-core/src/semantic/cache.rs`: durable vector cache and cache
  lookup/write behavior.
- `crates/tldr-core/src/semantic/store_search.rs`: existing `ManifestId`
  construction from source and build inputs.
- `crates/tldr-core/src/semantic/vector_store.rs`: deterministic streaming
  windows, cache reconciliation, and final vector-store construction.
- `crates/tldr-core/src/semantic/generation.rs`: verified atomic generation
  publication.
- `crates/tldr-core/src/callgraph/builder_v2.rs`: existing AST
  scan/parse/compose timers, adapted into the shared report.
- `crates/tldr-core/src/artifact_store/ingestion.rs`: artifact-ingestion phase
  and per-file parse timing source.
- `crates/tldr-core/src/semantic/worker_protocol.rs`: stable request identity
  projection and small progress event schema.
- `crates/tldr-cli/src/bin/tldr_embed_worker.rs`: window loop and progress
  emission.
- `crates/tldr-cli/src/commands/daemon/bulk_worker.rs`: live stdout consumption
  and advisory job snapshot updates.
- `crates/tldr-cli/src/commands/daemon/index_manager.rs`: current build
  progress snapshot.
- `crates/tldr-cli/src/commands/daemon/status.rs`: operator-facing progress.
- `crates/tldr-cli/src/commands/daemon/warm.rs`: documented metrics output and
  optional atomic-unit detail controls.
- `crates/tldr-contract-tests/src/main.rs`: baseline, crash, restart, and
  installed-build scenarios using the shared runner.
- `docs/FRESH_INSTALL_BENCHMARK_2026-07-28.md`: before/after measurements.

## Task 1: Baseline and freeze the simple contract (`TLDR-bjux.9`)

**Files:**

- Create: `docs/SEMANTIC_BUILD_RECOVERY.md`
- Modify: `docs/DAEMON_SEMANTIC_ARCHITECTURE.md`
- Modify: `crates/tldr-contract-tests/src/main.rs`

- [ ] **Step 1: Add a failing baseline scenario**

Add one isolated small-corpus scenario to the existing contract runner. It must
record the first run's cache misses and progress events, kill the worker after a
completed window, restart it with the same source and recipe, and assert:

```rust
ensure(first.cache_misses > 0, "baseline must perform inference")?;
ensure(first.progress_events > 0, "progress must arrive before worker exit")?;
ensure(
    resumed.cache_hits >= first.completed_vectors,
    "restart must recover completed inference from cache",
)?;
ensure(
    resumed.new_inference < uninterrupted.new_inference,
    "restart must not repeat compatible cached inference",
)?;
```

- [ ] **Step 2: Capture the current cold-build baseline**

Run the existing fresh-state benchmark protocol once and write one
machine-readable record containing:

```text
commit, machine, corpus_digest, model, phase, files_seen, cache_hits,
cache_misses, new_vectors, elapsed_ms, peak_rss_bytes, publication_generation,
query_result, process_timings, component_timings, unit_summaries
```

Do not require an up-front exact chunk count. Do not add a second harness.

- [ ] **Step 3: Document the recovery contract**

`docs/SEMANTIC_BUILD_RECOVERY.md` must state:

```text
Vector truth: compatible records in the embedding cache.
Progress truth: current scan counters reconciled from cache hits and new writes.
Publication truth: the verified active usearch generation.
Job ledger: advisory status/retry metadata only.
Restart: repeat scan/lookup; do not repeat compatible cached inference.
Incompatibility: discard the job record and start a new scan without spending
retry budget; normal cache-key compatibility decides which vectors are reusable.
Timing: one run_id correlates process roles; phases report wall time; atomic-unit
summaries are bounded by default and raw unit records require explicit opt-in.
```

- [ ] **Step 4: Prove the baseline exposes the current defects**

Run:

```bash
cargo tldr-certification -- semantic-build
```

Expected before implementation: failure because progress is buffered until exit,
the job reports one corpus-sized batch, or restart cannot account for retained
cache vectors.

- [ ] **Step 5: Commit**

```bash
git add docs/SEMANTIC_BUILD_RECOVERY.md \
  docs/DAEMON_SEMANTIC_ARCHITECTURE.md \
  crates/tldr-contract-tests/src/main.rs
git commit -m "test(semantic): freeze cache-backed recovery contract"
```

## Task 2: Reuse stable semantic identities (`TLDR-bjux.11`)

**Files:**

- Modify: `crates/tldr-core/src/semantic/worker_protocol.rs`
- Modify: `crates/tldr-core/src/semantic/lineage.rs`
- Modify: `crates/tldr-core/src/semantic/store_search.rs`
- Modify: `crates/tldr-cli/src/bin/tldr_embed_worker.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/bulk_worker.rs`

- [ ] **Step 1: Write identity regression tests**

Test that two requests which differ only in their temporary artifact export
path, PID, retry count, timestamp, or transport details select the same
`ManifestId`/`EmbeddingRecipeId` pair. Also test that changing source digest,
model, embedding schema, or output-affecting chunk configuration changes the
appropriate existing identity.

```rust
assert_eq!(
    semantic_identity(request("/tmp/export-a", 1)),
    semantic_identity(request("/tmp/export-b", 9)),
);
assert_ne!(
    semantic_identity(request_for_model(ArcticM)),
    semantic_identity(request_for_model(ArcticL)),
);
```

- [ ] **Step 2: Replace full-request hashing with an identity projection**

Derive worker compatibility from the source/artifact digest and the existing
semantic identity types. Do not serialize or hash `WorkerBuildRequest` as the
recipe, and do not introduce another public recipe structure.

- [ ] **Step 3: Invalidate incompatible job metadata before model load**

When the persisted worker protocol or projected identity does not match, record
the reason, replace the advisory job record, and start scanning. Do not consume
retry budget. Leave compatible cache entries available; their existing keys
decide reuse safely.

- [ ] **Step 4: Run focused tests**

```bash
cargo test -p tldr-core lineage
cargo test -p tldr-core store_search
cargo test -p tldr-cli bulk_worker
```

Expected: transport-only changes preserve identity; output-affecting changes do
not; incompatible job metadata restarts without a retry.

- [ ] **Step 5: Commit**

```bash
git add crates/tldr-core/src/semantic/{lineage.rs,store_search.rs,worker_protocol.rs} \
  crates/tldr-cli/src/bin/tldr_embed_worker.rs \
  crates/tldr-cli/src/commands/daemon/bulk_worker.rs
git commit -m "fix(semantic): stabilize worker compatibility identity"
```

## Task 3: Stream cache-backed windows and live progress (`TLDR-bjux.12`)

**Files:**

- Modify: `crates/tldr-core/src/semantic/cache.rs`
- Modify: `crates/tldr-core/src/semantic/vector_store.rs`
- Modify: `crates/tldr-core/src/semantic/generation.rs`
- Modify: `crates/tldr-core/src/semantic/worker_protocol.rs`
- Modify: `crates/tldr-cli/src/bin/tldr_embed_worker.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/bulk_worker.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/index_manager.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/status.rs`

- [ ] **Step 1: Add fixed-window restart tests**

Use one constant maximum, initially:

```rust
const EMBEDDING_WINDOW_VECTORS: usize = 128;
```

Test a corpus larger than one window. Kill after the first window event and
assert that restart observes those vectors as cache hits. Test a final partial
window and a one-window corpus.

- [ ] **Step 2: Make cache writes the durable work boundary**

Keep deterministic source order. For each window, read compatible cache entries,
embed only misses, and use the existing immediate cache writes in
`flush_streaming_window`. A completed window event may be emitted only after all
new records in that window are durable.

Do not write a second staged-vector table. Do not require the job checkpoint and
cache writes to share a cross-store transaction.

- [ ] **Step 3: Reconcile advisory counters**

At start and after each window, derive:

```rust
pub struct BuildProgress {
    pub phase: BuildPhase,
    pub files_seen: u64,
    pub files_total: Option<u64>,
    pub cache_hits: u64,
    pub new_vectors: u64,
    pub elapsed_ms: u64,
    pub retries: u32,
}
```

If a cheap exact file denominator already exists, report it. Do not preplan the
entire corpus for an exact chunk/vector percentage. Render percentage only when
a truthful denominator exists.

- [ ] **Step 4: Consume worker events while the child runs**

Start the bounded stdout reader before the `try_wait` loop in
`bulk_worker.rs`. Validate the small versioned JSONL frames and update the shared
snapshot on every event. Preserve a final completion/error event after process
exit.

- [ ] **Step 5: Verify before atomic publication**

Build the final `VectorStore` from the complete current scan, verify its expected
manifest/count integrity with the existing generation code, and publish it
atomically. A crash before publication leaves the old generation visible; the
next run reuses compatible cache vectors.

- [ ] **Step 6: Run focused tests**

```bash
cargo test -p tldr-core semantic::cache
cargo test -p tldr-core semantic::vector_store
cargo test -p tldr-cli bulk_worker
cargo test -p tldr-cli daemon_status
cargo tldr-certification -- semantic-build,resume
```

Expected: live progress appears before child exit, every completed first-run
vector becomes a restart cache hit, and only a fully verified generation becomes
active.

- [ ] **Step 7: Commit**

```bash
git add crates/tldr-core/src/semantic/{cache.rs,generation.rs,vector_store.rs,worker_protocol.rs} \
  crates/tldr-cli/src/bin/tldr_embed_worker.rs \
  crates/tldr-cli/src/commands/daemon/{bulk_worker.rs,index_manager.rs,status.rs}
git commit -m "fix(semantic): resume builds from durable embedding cache"
```

## Task 4: Unify component and atomic-unit timing (`TLDR-bjux.19`)

**Files:**

- Modify: `crates/tldr-core/src/semantic/build_metrics.rs`
- Modify: `crates/tldr-core/src/callgraph/builder_v2.rs`
- Modify: `crates/tldr-core/src/artifact_store/ingestion.rs`
- Modify: `crates/tldr-core/src/semantic/vector_store.rs`
- Modify: `crates/tldr-core/src/semantic/worker_protocol.rs`
- Modify: `crates/tldr-cli/src/bin/tldr_embed_worker.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/bulk_worker.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/warm.rs`

- [ ] **Step 1: Add timing-schema tests**

Extend the existing metrics schema with these logical levels:

```rust
pub struct ProcessTiming {
    pub run_id: String,
    pub parent_run_id: Option<String>,
    pub role: ProcessRole,
    pub wall_duration_ms: u64,
    pub phases: Vec<PhaseRecord>,
    pub units: Vec<UnitSummary>,
}

pub struct UnitSummary {
    pub kind: UnitKind,
    pub count: u64,
    pub total_duration_ms: u64,
    pub min_duration_ms: u64,
    pub p50_duration_ms: u64,
    pub p95_duration_ms: u64,
    pub max_duration_ms: u64,
    pub slowest: Vec<SlowUnit>,
}
```

Use a bounded reservoir/histogram suitable for the existing metrics module and a
fixed slowest-N list. Tests must cover empty, one-unit, repeated-unit and
concurrent-process reports. Summed unit work may exceed wall time when work is
parallel; document and test that distinction.

- [ ] **Step 2: Fold the existing AST timer into the report**

Replace the isolated `TLDR_PHASE_TIMING` stderr-only ownership with shared phase
records for `scan`, `ast_parse`, and `compose`. Preserve the environment flag
temporarily for compatibility, but make the structured metrics path canonical.
Record AST atomic units by language and source-relative file identity.

Default output keeps:

```text
language, file_count, total/min/p50/p95/max parse_ms, slowest-N files
```

It must not retain every file identity unless detailed output was requested.

- [ ] **Step 3: Instrument the remaining build boundaries**

Record process roles and wall-time phases for:

```text
daemon orchestration
artifact scan -> AST parse -> artifact write
semantic plan/chunk -> cache lookup -> model load -> inference
vector assembly -> verify -> publish
```

Record atomic units for file parse, semantic window, and inference batch. Reuse
the existing batch latency and pipeline telemetry rather than measuring the same
operation twice.

- [ ] **Step 4: Add a documented user surface**

Extend `tldr warm` with:

```text
tldr warm <path> --metrics <report.json>
tldr warm <path> --metrics <report.json> --metrics-detail units
```

Aggregate mode writes one bounded JSON report. Detailed mode additionally
streams raw unit records as JSONL beside the report and references that file
from the report. Both daemon and foreground warm paths must use the same schema.

- [ ] **Step 5: Verify AST timing answers**

On a mixed-language fixture, assert the report can answer:

```text
How much wall time did AST parsing take?
How many files were parsed for each language?
What were p50/p95/max parse times?
Which bounded set of files were slowest?
How much time belonged to each process and semantic phase?
How long did each window/batch take in detailed mode?
```

- [ ] **Step 6: Measure observer overhead**

Compare timings disabled, aggregate, and detailed on the fixed corpus. Aggregate
mode must add less than 1% wall time or record a reviewed alternative bound.
Memory is bounded by phase count, unit kinds, histogram state, and slowest-N.
Detailed mode is explicitly opt-in and streams rather than accumulating records.

- [ ] **Step 7: Commit**

```bash
git add crates/tldr-core/src/{artifact_store/ingestion.rs,callgraph/builder_v2.rs} \
  crates/tldr-core/src/semantic/{build_metrics.rs,vector_store.rs,worker_protocol.rs} \
  crates/tldr-cli/src/bin/tldr_embed_worker.rs \
  crates/tldr-cli/src/commands/daemon/{bulk_worker.rs,warm.rs}
git commit -m "feat(metrics): correlate structural and semantic build timing"
```

## Task 5: Profile and fix only the dominant bottleneck (`TLDR-bjux.14`)

**Files:**

- Modify only the measured bottleneck files under:
  `crates/tldr-core/src/semantic/`
- Modify: `crates/tldr-contract-tests/src/main.rs`
- Modify: `docs/FRESH_INSTALL_BENCHMARK_2026-07-28.md`

- [ ] **Step 1: Measure the corrected pipeline**

Run the Task 1 benchmark from empty state after Tasks 2–4. Record process,
component, and atomic-unit timing plus throughput, CPU utilization, and peak RSS.
Identify one dominant phase before selecting a change.

- [ ] **Step 2: Set the first performance change from evidence**

Choose the smallest change matching the measurement:

```text
planning dominant  -> remove the measured repeated planning/tokenization work
inference dominant -> tune the measured fixed-shape batch/thread setting
publication dominant -> remove the measured duplicate materialization/write
```

Parallel planning or a memoization layer is permitted only when the profile
demonstrates enough benefit to justify its complexity.

- [ ] **Step 3: Freeze equivalence before optimization**

Record document identities, vector count, manifest identity, and retrieval
results. The optimization must preserve them; numeric vector tolerance may be
used only for an explicitly changed inference backend.

- [ ] **Step 4: Implement and compare one change at a time**

For each change, run the same benchmark command and retain it only when it
materially improves the dominant phase without raising peak RSS above the
baseline safety limit.

- [ ] **Step 5: Evaluate the 30-minute target honestly**

The 30-minute M2 Max result remains the target. First record a reproducible
passing measurement; only then turn it into a mandatory platform gate. If the
first simple fix does not meet the target, the benchmark record must identify
the remaining dominant phase before another design is proposed.

- [ ] **Step 6: Commit**

```bash
git add crates/tldr-core/src/semantic \
  crates/tldr-contract-tests/src/main.rs \
  docs/FRESH_INSTALL_BENCHMARK_2026-07-28.md
git commit -m "perf(semantic): remove measured cold-build bottleneck"
```

## Task 6: Certify recovery and a fresh installed build (`TLDR-bjux.16`)

**Files:**

- Modify: `crates/tldr-contract-tests/src/main.rs`
- Modify: `docs/FRESH_INSTALL_BENCHMARK_2026-07-28.md`
- Update: relevant Beads records

- [ ] **Step 1: Add four recovery scenarios**

Use the shared runner and cover exactly these semantic-owned boundaries:

```text
1. kill before the first durable cache write;
2. kill after a completed window but before final publication;
3. restart with a compatible identity and retained cache;
4. restart with an incompatible identity.
```

Expected in every case: the old generation remains visible until a complete new
one publishes; compatible cached inference is reused; incompatible job metadata
does not spend retry budget.

- [ ] **Step 2: Run repository quality gates**

```bash
cargo fmt --all --check
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets -- -D warnings
CARGO_INCREMENTAL=0 cargo test --workspace
cargo tldr-smoke
cargo tldr-certification -- semantic-build,resume
```

Expected: all new semantic scenarios pass. Pre-existing unrelated failures must
be linked to their existing Beads issue rather than hidden.

- [ ] **Step 3: Remove only inventoried tldr-owned state**

Follow the benchmark cleanup inventory. Do not resume the historical
`bulk-b7e8addadd3a5083d50936b3e9e1646a` job.

- [ ] **Step 4: Build and install current HEAD**

```bash
cargo build --release --locked -p tldr-cli --bins
cargo install --path crates/tldr-cli --locked --force
```

Record source commit and installed/release binary hashes.

- [ ] **Step 5: Run one fresh build and installed query**

Capture live progress snapshots, cache hits/new vectors, phase timings, CPU/RSS,
disk, correlated process/component timings, bounded atomic-unit summaries, final
generation, and query result. Do not start a second concurrent semantic build.

- [ ] **Step 6: Update evidence and close only proven work**

Update the benchmark and all linked Beads problems with exact results. The epic
closes only when the installed fresh build publishes, queries successfully,
recovery certification passes, and the measured completion target is satisfied.
Daemon/launchd stop behavior remains tracked by `TLDR-cxa.3` and does not block
this semantic-build epic.

- [ ] **Step 7: Commit and push**

```bash
git pull --rebase fork main
bd dolt push
git push fork main
git status
```

Expected: Git and Beads are pushed; branch is clean and synchronized.

## Self-review

- Spec coverage: stable identity is Task 2; bounded cache-backed restart and live
  progress are Task 3; correlated timing is Task 4; measured performance is Task
  5; crash and clean-install evidence are Task 6.
- Simplicity: no duplicate recipe, vector ledger, staged-vector store, global
  transaction, whole-corpus preplan, or dual count/time checkpoint policy.
- Correctness: durable compatible cache records prevent repeated inference;
  existing generation verification/publication prevents partial visibility.
- Observability: phase, files, cache hits, new vectors, elapsed time, and retries
  answer whether work is advancing without pretending an unknown denominator is
  known.
- Timing: a normal bounded report answers component and AST costs; opt-in JSONL
  answers exact per-file/window/batch questions without burdening normal builds.
- Scope: launchd cancellation remains in the lifecycle epic and is not a
  semantic publication dependency.
- Complexity gate: optimization follows a measured bottleneck; it is not chosen
  in advance.

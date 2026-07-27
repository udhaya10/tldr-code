# Structural Embedding Program Implementation Plan

> **For agentic workers:** Execute inline in dependency order. Beads issues `TLDR-9bxa.5` through `TLDR-9bxa.11` are the authoritative acceptance contracts; this document records the cross-epic integration sequence.

**Goal:** Complete all eleven `TLDR-9bxa` child epics and switch the structural, fixed-shape, bounded, resumable embedding pipeline to the staged default with a complete rollback generation.

**Architecture:** Preserve the existing tree-sitter planner, Arctic semantics, and usearch query surface. Finish the fixed-shape core first, split inference by workload, stream bounded build records into redb, publish usearch as a crash-safe derived generation, move bulk inference into a checkpointed child process, and finish with predeclared side-by-side quality and operational gates.

**Tech Stack:** Rust, tree-sitter, tokenizers, ONNX Runtime, fastembed oracle, redb, usearch, serde/rkyv compatibility reader, daemon Unix IPC, Beads.

---

### Task 1: Finish `TLDR-9bxa.5` fixed-shape production path

**Files:**
- Create: `crates/tldr-core/src/semantic/fixed_shape_embedder.rs`
- Modify: `crates/tldr-core/src/semantic/mod.rs`
- Modify: `crates/tldr-core/src/semantic/vector_store.rs`
- Modify: `crates/tldr-core/src/semantic/build_metrics.rs`
- Test: `crates/tldr-core/src/semantic/fixed_shape_embedder.rs`

- [x] Add an explicit default-off backend selector and a document embedder that tokenizes once, plans finite batches, executes direct ORT, and restores caller order.
- [x] Route cold-build cache misses through the selector while retaining FastEmbed as the oracle/rollback.
- [x] Record backend, exact shapes, per-batch latency, padding, throughput, and build-window RSS in the existing metrics report.
- [x] Run the end-of-epic Rust and full cold-build gates. Plateau and 4 GiB RSS pass; the measured 16.835% throughput exception is recorded for the default-off candidate, with FastEmbed retained as default/rollback and rollout policy deferred to `.11`.

### Task 2: Complete `TLDR-9bxa.6` workload-specific sessions

**Files:**
- Create: `crates/tldr-core/src/semantic/inference_runners.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/index_manager.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/status.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/types.rs`
- Test: `crates/tldr-cli/src/commands/daemon/index_manager.rs`

- [x] Replace the shared embedder mutex with query, delta, and bulk runner state.
- [x] Preserve Arctic query-prefix numerical parity and keep tokenization/inference outside usearch locks.
- [x] Expose each runner state in daemon status and prove concurrent query/delta operation.
- [x] Run the epic gate once, including query p95 under simultaneous bulk work and varied-query RSS plateau.

### Task 3: Complete `TLDR-9bxa.7` bounded streaming build

**Files:**
- Create: `crates/tldr-core/src/semantic/build_pipeline.rs`
- Modify: `crates/tldr-core/src/semantic/vector_store.rs`
- Modify: `crates/tldr-core/src/semantic/build_metrics.rs`
- Test: `crates/tldr-core/src/semantic/build_pipeline.rs`

- [x] Define explicit memory-budget-derived queue capacities and typed stage errors carrying file/chunk identity.
- [x] Stream enumerate -> plan -> compose -> cache -> tokenize -> bucket -> inference -> sink without retaining whole-corpus payloads.
- [x] Preserve deterministic output by stable `ChunkId`, implement cancellation, and prevent partial store publication.
- [x] Run slow-consumer/backpressure, cancellation, parity, and non-ONNX memory gates once at epic completion.

### Task 4: Complete `TLDR-9bxa.8` redb cache and job ledger

**Files:**
- Create: `crates/tldr-core/src/semantic/redb_store.rs`
- Create: `crates/tldr-core/src/semantic/redb_migration.rs`
- Modify: `crates/tldr-core/src/semantic/cache.rs`
- Modify: `crates/tldr-core/src/semantic/mod.rs`
- Modify: `crates/tldr-core/Cargo.toml`
- Modify: `Cargo.toml`
- Test: `crates/tldr-core/src/semantic/redb_store.rs`

- [x] Add versioned metadata, file, chunk, file-chunk, embedding, and job tables with an explicitly bounded redb cache.
- [x] Validate little-endian f32 encoding, dimensions, model/pipeline identity, and corruption/incompatibility rebuild paths.
- [x] Make related record/job transitions atomic and support per-record updates without whole-map rewrites.
- [x] Retain the old rkyv cache as a tested one-time migration source only, then run crash/cache-hit/memory gates once.

### Task 5: Complete `TLDR-9bxa.9` generation publication

**Files:**
- Create: `crates/tldr-core/src/semantic/generation.rs`
- Modify: `crates/tldr-core/src/semantic/redb_store.rs`
- Modify: `crates/tldr-core/src/semantic/vector_store.rs`
- Modify: `crates/tldr-core/src/semantic/store_search.rs`
- Test: `crates/tldr-core/src/semantic/generation.rs`

- [x] Stage redb records, build and fsync immutable usearch generation files, then atomically switch `active_generation`.
- [x] Verify checksums, counts, dimensions, model, metric, and pipeline version on load.
- [x] Rebuild derived usearch deterministically from redb and retain the prior complete generation for rollback.
- [x] Inject failure after every publication boundary and run the recovery matrix once before closing `.9`.

### Task 6: Complete `TLDR-9bxa.10` resumable bulk worker

**Files:**
- Create: `crates/tldr-cli/src/bin/tldr_embed_worker.rs`
- Create: `crates/tldr-core/src/semantic/worker_protocol.rs`
- Create: `crates/tldr-cli/src/commands/daemon/bulk_worker.rs`
- Modify: `crates/tldr-cli/Cargo.toml`
- Modify: `crates/tldr-cli/src/commands/daemon/mod.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/index_manager.rs`
- Test: `crates/tldr-cli/src/commands/daemon/bulk_worker.rs`

- [x] Define a bounded versioned local protocol with model/pipeline compatibility negotiation.
- [x] Persist each completed batch in redb before acknowledging it; resume pending jobs without gaps or duplicates.
- [x] Add cancellation, finite retry accounting, crash detection, and RSS-watermark recycling.
- [x] Kill at every batch boundary and run responsiveness, memory-reclamation, IPC-bound, and startup-overhead gates once.

### Task 7: Complete `TLDR-9bxa.11` quality gates and rollout

**Files:**
- Create: `crates/tldr-core/src/semantic/rollout.rs`
- Modify: `crates/tldr-core/examples/semantic_eval.rs`
- Modify: `crates/tldr-cli/src/commands/embed.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/index_manager.rs`
- Modify: `docs/STRUCTURAL_EMBEDDING_PIPELINE.md`
- Test: `crates/tldr-core/src/semantic/rollout.rs`

- [x] Persist thresholds before comparison and expand evaluation across oversized, nested, NL-to-code, code-to-code, and cross-language cases.
- [x] Compare old/new complete generations for Recall, MRR, nDCG, latency, wall time, index size, cache hits, delta scope, and RSS.
- [x] Add explicit generation selection, default the passing structural generation, and preserve rollback for one stable release.
- [x] Run the final quality, cold-memory, wall-time, delta, recovery, and workspace gates; close `.11` and parent `TLDR-9bxa`.

### Task 8: Program hygiene and delivery

**Files:**
- Modify: Beads issues `TLDR-9bxa`, `TLDR-9bxa.5` through `TLDR-9bxa.11`

- [x] Remove only proven generated junk (`tldr-smoke-report/`) recoverably; retain caches, research evidence, IDE state, and benchmark inputs.
- [x] Update and close each Beads epic only after its acceptance evidence exists.
- [x] Commit and push at epic boundaries, synchronize Dolt, and keep the branch clean.
- [x] Run the full workspace build/test/lint gate once after all child epics integrate.

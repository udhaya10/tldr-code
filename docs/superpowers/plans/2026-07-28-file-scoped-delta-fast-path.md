# File-Scoped Delta Fast Path Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make watcher deltas scale with the changed-file set, avoid loading an ONNX session for zero-inference edits, and retain atomic artifact/vector generations.

**Architecture:** Keep full project ingestion unchanged as the reconciliation authority. Add a file-scoped ingestion branch that derives revisions only for explicit paths, retain whole-graph composition initially because it is correctness-sensitive and measured at roughly 100 ms, and project semantic inputs directly from the pinned snapshot by path. Give the delta runner a tokenizer-only planning cache so ONNX is constructed only when changed documents actually need embeddings.

**Tech Stack:** Rust, Tokio daemon, redb artifact generations, ArcSwap snapshots, tokenizers, ONNX Runtime, usearch.

---

### Task 1: Project semantic inputs only for changed files

**Files:**
- Modify: `crates/tldr-core/src/artifact_store/projections.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/daemon.rs`
- Test: `crates/tldr-core/src/artifact_store/projections.rs`
- Test: `crates/tldr-cli/src/commands/daemon/daemon.rs`

- [ ] **Step 1: Write a failing projection test**

Add a snapshot fixture with two `FileFacts` records and assert:

```rust
let selected = snapshot.semantic_source_chunks_for(
    project.path(),
    [Path::new("src/changed.rs")],
);
assert_eq!(selected.len(), 1);
assert_eq!(selected[0].file_path, project.path().join("src/changed.rs"));
```

- [ ] **Step 2: Run the projection test and verify it fails**

Run:

```bash
cargo test -p tldr-core semantic_source_chunks_for_selects_only_requested_files --lib
```

Expected: compilation failure because `semantic_source_chunks_for` does not exist.

- [ ] **Step 3: Implement the generation-pinned batch projection**

Add a method that normalizes requested paths, performs `self.files.get()` lookups, converts every stored semantic fact for each selected file, and preserves deterministic requested-path/chunk order:

```rust
pub fn semantic_source_chunks_for<'a>(
    &self,
    project: &Path,
    paths: impl IntoIterator<Item = &'a Path>,
) -> Vec<crate::semantic::CodeChunk>
```

Do not call `self.files.values()`, sort the entire corpus, or collapse multiple chunks for one path.

- [ ] **Step 4: Replace the daemon’s full-corpus map**

In `apply_semantic_delta_batch`, request chunks for `applied`, group only the returned changed-file chunks, and pass the complete file source expected by `IndexManager::apply_delta`. Deletions must continue passing `None`.

- [ ] **Step 5: Run focused tests**

Run:

```bash
cargo test -p tldr-core semantic_source_chunks --lib
cargo test -p tldr-cli dirty_file_batch_publishes_each_final_revision --lib
```

Expected: all selected tests pass.

### Task 2: Plan deltas without constructing ONNX

**Files:**
- Modify: `crates/tldr-core/src/semantic/token_budget.rs`
- Modify: `crates/tldr-core/src/semantic/model_artifacts.rs`
- Modify: `crates/tldr-core/src/semantic/inference_runners.rs`
- Test: `crates/tldr-core/src/semantic/inference_runners.rs`

- [ ] **Step 1: Write a failing runner test**

Plan with the delta runner and assert the planning tokenizer becomes ready while `sessions_built == 0` and `requests == 0`.

- [ ] **Step 2: Run the test and verify it fails**

Run:

```bash
cargo test -p tldr-core token_budget_planning_does_not_build_onnx_session --lib
```

Expected: the current runner reports one constructed session.

- [ ] **Step 3: Add a tokenizer-only model loader**

Resolve the commit-pinned `tokenizer.json` through `ResolvedModelArtifacts`, load it with `tokenizers::Tokenizer`, and construct `TokenBudget` using `EmbeddingModel::max_context()` and the tokenizer’s pad token. This path must not construct `TextEmbedding`, run the integrity probe, or initialize ORT.

- [ ] **Step 4: Cache planning tokenizers separately**

Add a model-keyed planning-tokenizer slot to `FixedShapeInferenceRunner`. Change `with_token_budget` to use that slot. Leave `ensure_session` exclusively on query/document inference paths.

- [ ] **Step 5: Run semantic runner and delta tests**

Run:

```bash
cargo test -p tldr-core inference_runners --lib
cargo test -p tldr-cli index_manager --lib
```

Expected: all tests pass and planning does not increment session or request counters.

### Task 3: Avoid whole-corpus reads for `IngestionScope::Files`

**Files:**
- Modify: `crates/tldr-core/src/artifact_store/ingestion.rs`
- Test: `crates/tldr-core/src/artifact_store/ingestion.rs`

- [ ] **Step 1: Add a file-scope read-count regression test**

Create two source files, publish a project generation, change one file, make the other unreadable through the test reader seam, and assert file-scoped ingestion succeeds by reading only the requested path.

- [ ] **Step 2: Run the test and verify it fails**

Run:

```bash
cargo test -p tldr-core file_scope_does_not_read_unchanged_sources --lib
```

Expected: failure because `source_manifest` currently reads both files.

- [ ] **Step 3: Split project and file discovery**

For `IngestionScope::Project`, preserve `discover + source_manifest`. For `IngestionScope::Files`, normalize the explicit paths, read/hash only existing requested corpus files, mark absent requested files as removals, and derive a deterministic lineage revision from the previous generation revision plus sorted changed `(path, revision-or-delete)` entries.

- [ ] **Step 4: Preserve reconciliation semantics**

Keep full project ingestion as the authoritative missed-event and ignore-policy reconciliation path. Ensure an explicit deletion removes the previous file artifacts and that a later full project ingestion converges to the same visible corpus.

- [ ] **Step 5: Run artifact lifecycle tests**

Run:

```bash
cargo test -p tldr-core artifact_store::ingestion --lib
cargo test -p tldr-cli full_artifact_warm_removes_newly_ignored_file --lib
```

Expected: all tests pass.

### Task 4: Validate end-to-end watcher performance

**Files:**
- Modify: `docs/benchmarks/2026-07-28-delta-fast-path/SUMMARY.md`
- Modify: Beads issues `TLDR-8cw9`, `TLDR-1hld.14`, and `TLDR-1hld.8`

- [ ] **Step 1: Run quality gates**

Run:

```bash
cargo fmt --check
cargo test -p tldr-core artifact_store:: --lib
cargo test -p tldr-core inference_runners --lib
cargo test -p tldr-cli commands::daemon::watcher --lib
cargo test -p tldr-cli dirty_file_batch_publishes_each_final_revision --lib
```

Expected: all commands succeed.

- [ ] **Step 2: Install and restart the current binaries**

Build/install the release CLI using the repository’s established install command, gracefully restart the project daemon, and verify status is warm before probing.

- [ ] **Step 3: Run reversible live probes**

Measure an mtime-only event, one small comment edit, and restoration. Record artifact generation, delta requests, vectors, publication latency, RSS, and git cleanliness.

- [ ] **Step 4: Record results and update Beads**

The no-content event must perform zero inference without constructing a delta ONNX session. Changed edits must project only the changed file and embed only output-changing documents. Record any remaining global graph cost explicitly.

- [ ] **Step 5: Commit and push**

Commit only the implementation, tests, benchmark summary, and passive Beads export intended for source control. Push Beads and `main` to the configured fork, then verify `main...fork/main` is synchronized.

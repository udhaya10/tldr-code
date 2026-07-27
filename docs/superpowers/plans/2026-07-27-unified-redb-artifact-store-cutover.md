# Unified Redb Artifact Store One-Shot Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace TLDR's command-specific structural caches and separate semantic persistence with one redb-backed, generation-aware artifact store and one resumable bulk/incremental ingestion pipeline in a single release cutover.

**Architecture:** `redb` becomes the durable source of truth for project revisions, normalized file facts, analyzer artifacts, dependencies, ingestion jobs, and generation manifests. Parallel workers read and analyze files, a bounded single-writer service commits artifact batches, and queries serve only an atomically published generation. The released binary has no legacy-cache read path or silent fallback; existing caches are ignored and the new store is rebuilt resumably from source.

**Tech Stack:** Rust, redb, Serde, Tokio, Rayon, Tree-sitter, bounded hot caches, existing usearch vector index, and tempfile for isolated construction checks.

---

## Execution scope correction (user-confirmed 2026-07-27)

This plan implements the complete production cutover, but it does **not** build
the permanent replacement test architecture. `TLDR-dpbc` follows this epic and
owns that work.

- Tests named in the tasks below are acceptance examples, not a requirement to
  create many Rust integration-test crates.
- Reuse existing tests or add the smallest disposable construction check that
  proves the invariant currently being implemented.
- Do not add proptest, fuzzing, mutation, simulation, formal verification, or a
  general-purpose scenario framework during this epic.
- Prefer inline module checks or one compact cutover acceptance target over
  repeated cold/warm, bulk/delta, CLI/MCP, command, or language test bodies.
- The production requirements, deletion of legacy persistent-cache paths,
  measured rollout gates, and release validation remain mandatory.
- When `TLDR-dpbc` begins, it deletes these construction checks together with
  the legacy suite and builds the permanent one-harness/two-filter suite once.

## One-shot cutover contract

The implementation may land as small, testable commits on the feature branch, but the released runtime switches as one unit:

```text
old release                         new release
-----------                         -----------
QueryCache + JSON files      ─X─>   ArtifactStore
per-command parsing          ─X─>   unified FileFacts ingestion
semantic-only generations    ─X─>   project-wide generations
project-wide invalidation    ─X─>   artifact dependency deltas
legacy daemon routing        ─X─>   typed artifact queries
```

There is no production mode that reads both stores. On first start, the new release creates `project.redb`, builds a resumable generation from source, and publishes it. Until publication, commands report a typed `Building` or `NotReady` state. The previous executable and its cache files remain untouched until the new generation passes validation, providing operational rollback without a dual-backend runtime.

## Target request and ingestion flow

```text
filesystem watcher / bulk scan
              |
              v
canonical project + file revision
              |
              v
read bytes -> hash -> language -> parse once
              |
              v
          FileFacts
      /       |        \
symbols   call edges   semantic chunks
   |          |             |
   |       call graph      embeddings
   |          |
   +---- function bodies ----+
                |
           CFG -> DFG -> PDG
                |
                v
       bounded ArtifactBatch queue
                |
                v
     one redb transaction writer
                |
                v
     validate + atomically publish
                |
                v
      typed query projections -> CLI rendering
```

### Task 1: Freeze the cutover contracts and performance baseline

**Files:**
- Create: `crates/tldr-cli/tests/artifact_store_baseline.rs`
- Modify: `crates/tldr-cli/tests/l2_daemon_cache_bench_test.rs`
- Modify: `docs/STRUCTURAL_EMBEDDING_PIPELINE.md`

- [ ] **Step 1: Add a baseline metrics record**

```rust
#[derive(Debug, Clone, serde::Serialize)]
struct CutoverBaseline {
    command: String,
    elapsed_us: u64,
    files_read: u64,
    parser_invocations: u64,
    ipc_bytes: u64,
    peak_rss_bytes: u64,
    persistent_bytes: u64,
}
```

- [ ] **Step 2: Add fixture commands for every migrated surface**

Cover `tree`, `structure`, `extract`, `imports`, `importers`, `references`, `calls`, `impact`, `dead`, `hubs`, `coupling`, `cfg`, `reaching-defs`, `available`, `dead-stores`, `slice`, `chop`, `semantic`, and `similar`.

- [ ] **Step 3: Capture cold, warm, restart, and single-file-edit baselines**

Run:

```bash
cargo test -p tldr-cli --test artifact_store_baseline -- --ignored --nocapture
cargo test -p tldr-cli --test l2_daemon_cache_bench_test -- --ignored --nocapture
```

Expected: both commands pass and emit machine-readable baseline records.

- [ ] **Step 4: Record non-negotiable correctness invariants**

Add these invariants to `docs/STRUCTURAL_EMBEDDING_PIPELINE.md`:

```text
one unchanged file revision -> at most one parse
one published generation -> no mixed artifact revisions
one interrupted job -> resumes its last committed batch
one file edit -> no unrelated file reparse
one command result -> equivalent to the pre-cutover result schema
```

- [ ] **Step 5: Commit**

```bash
git add crates/tldr-cli/tests/artifact_store_baseline.rs crates/tldr-cli/tests/l2_daemon_cache_bench_test.rs docs/STRUCTURAL_EMBEDDING_PIPELINE.md
git commit -m "test(artifact-store): capture pre-cutover baseline"
```

### Task 2: Define stable artifact identities and envelopes

**Files:**
- Create: `crates/tldr-core/src/artifact_store/mod.rs`
- Create: `crates/tldr-core/src/artifact_store/types.rs`
- Modify: `crates/tldr-core/src/lib.rs`
- Test: `crates/tldr-core/tests/artifact_identity_tests.rs`

- [ ] **Step 1: Write identity round-trip and collision tests**

```rust
#[test]
fn artifact_identity_changes_with_revision_kind_or_producer() {
    let base = ArtifactKey::for_file(project(), revision([1; 32]), ArtifactKind::FileFacts);
    assert_ne!(base, base.with_revision(revision([2; 32])));
    assert_ne!(base, base.with_kind(ArtifactKind::CallEdges));
    assert_ne!(base, base.with_producer(ProducerId::new("callgraph", 2)));
}
```

- [ ] **Step 2: Define the durable identity types**

```rust
#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ArtifactKey {
    pub project: ProjectId,
    pub revision: RevisionId,
    pub subject: ArtifactSubject,
    pub kind: ArtifactKind,
    pub producer: ProducerId,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ArtifactSubject {
    Project,
    File(String),
    Symbol(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ArtifactKind {
    FileFacts,
    Symbols,
    References,
    CallEdges,
    CallGraph,
    Cfg,
    Dfg,
    Pdg,
    SemanticChunks,
    Embeddings,
}
```

- [ ] **Step 3: Define immutable stored envelopes**

```rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ArtifactEnvelope {
    pub key: ArtifactKey,
    pub generation: u64,
    pub dependencies: Vec<ArtifactKey>,
    pub payload_checksum: [u8; 32],
    pub payload: Vec<u8>,
}
```

- [ ] **Step 4: Export the module and run tests**

Run:

```bash
cargo test -p tldr-core --test artifact_identity_tests
```

Expected: all identity, serialization, and checksum tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/tldr-core/src/artifact_store crates/tldr-core/src/lib.rs crates/tldr-core/tests/artifact_identity_tests.rs
git commit -m "feat(artifact-store): define durable artifact identities"
```

### Task 3: Generalize semantic redb persistence

**Files:**
- Create: `crates/tldr-core/src/artifact_store/redb.rs`
- Create: `crates/tldr-core/src/artifact_store/schema.rs`
- Modify: `crates/tldr-core/src/semantic/redb_store.rs`
- Modify: `crates/tldr-core/src/semantic/cache.rs`
- Modify: `crates/tldr-core/src/semantic/generation.rs`
- Test: `crates/tldr-core/tests/artifact_store_transaction_tests.rs`

- [ ] **Step 1: Write atomic artifact-and-checkpoint tests**

```rust
#[test]
fn artifact_batch_and_checkpoint_commit_atomically() {
    let store = test_store();
    store.commit_batch(&batch(7), &checkpoint(7)).unwrap();
    assert_eq!(store.artifact(&key()).unwrap().unwrap().generation, 7);
    assert_eq!(store.job("bulk").unwrap().unwrap().next_batch, 8);
}
```

- [ ] **Step 2: Define namespaced redb tables**

```rust
pub const METADATA: redb::TableDefinition<&str, &[u8]> =
    redb::TableDefinition::new("artifact.metadata");
pub const GENERATIONS: redb::TableDefinition<u64, &[u8]> =
    redb::TableDefinition::new("artifact.generations");
pub const ARTIFACTS: redb::TableDefinition<&[u8], &[u8]> =
    redb::TableDefinition::new("artifact.records");
pub const ARTIFACT_DEPS: redb::MultimapTableDefinition<&[u8], &[u8]> =
    redb::MultimapTableDefinition::new("artifact.dependencies");
pub const GENERATION_ARTIFACTS: redb::MultimapTableDefinition<u64, &[u8]> =
    redb::MultimapTableDefinition::new("artifact.generation_records");
pub const JOBS: redb::TableDefinition<&str, &[u8]> =
    redb::TableDefinition::new("artifact.jobs");
```

- [ ] **Step 3: Move generic transaction, schema, job, and generation behavior**

Create `RedbArtifactStore` with:

```rust
pub trait ArtifactStore: Send + Sync {
    fn active_generation(&self) -> Result<Option<u64>, StoreError>;
    fn artifact(&self, key: &ArtifactKey) -> Result<Option<ArtifactEnvelope>, StoreError>;
    fn commit_batch(&self, batch: &ArtifactBatch, job: &IngestionJob) -> Result<(), StoreError>;
    fn publish(&self, manifest: &GenerationManifest) -> Result<(), StoreError>;
    fn reverse_dependencies(&self, key: &ArtifactKey) -> Result<Vec<ArtifactKey>, StoreError>;
}
```

- [ ] **Step 4: Retain semantic tables as a domain adapter**

Make semantic cache and generation code depend on `ArtifactStore` transactions while keeping vector encoding and semantic recipe validation inside `semantic`.

- [ ] **Step 5: Verify crash and reopen behavior**

Run:

```bash
cargo test -p tldr-core --test artifact_store_transaction_tests
cargo test -p tldr-core semantic::redb_store
cargo test -p tldr-core semantic::generation
```

Expected: committed batches survive reopen, aborted writes remain invisible, and semantic generation tests remain green.

- [ ] **Step 6: Commit**

```bash
git add crates/tldr-core/src/artifact_store crates/tldr-core/src/semantic
git commit -m "refactor(storage): generalize redb artifact transactions"
```

### Task 4: Introduce normalized FileFacts and parse-once instrumentation

**Files:**
- Create: `crates/tldr-core/src/artifact_store/file_facts.rs`
- Create: `crates/tldr-core/src/artifact_store/parser.rs`
- Modify: `crates/tldr-core/src/callgraph/cross_file_types.rs`
- Modify: `crates/tldr-core/src/semantic/chunker.rs`
- Test: `crates/tldr-core/tests/file_facts_tests.rs`

- [ ] **Step 1: Write a parse-once reuse test**

```rust
#[test]
fn one_revision_is_parsed_once_for_all_consumers() {
    let parser = CountingParser::default();
    let facts = parser.parse(&fixture_file()).unwrap();
    let _ = facts.structure();
    let _ = facts.call_edges();
    let _ = facts.semantic_chunks();
    assert_eq!(parser.invocations(), 1);
}
```

- [ ] **Step 2: Define the normalized file representation**

```rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FileFacts {
    pub path: String,
    pub revision: RevisionId,
    pub language: Language,
    pub definitions: Vec<DefinitionFact>,
    pub references: Vec<ReferenceFact>,
    pub imports: Vec<ImportFact>,
    pub calls: Vec<CallFact>,
    pub functions: Vec<FunctionFact>,
    pub diagnostics: Vec<ParseDiagnostic>,
}
```

- [ ] **Step 3: Make `FileIR` a projection of FileFacts**

Replace independent source parsing in the call-graph path with:

```rust
impl TryFrom<&FileFacts> for FileIR {
    type Error = AnalysisError;

    fn try_from(facts: &FileFacts) -> Result<Self, Self::Error> {
        FileIR::from_normalized(facts)
    }
}
```

- [ ] **Step 4: Make semantic chunking consume FileFacts**

Semantic chunks must use the same function boundaries, stable anchors, language, and relative path stored in `FileFacts`.

- [ ] **Step 5: Run parser and language-adapter tests**

Run:

```bash
cargo test -p tldr-core --test file_facts_tests
cargo test -p tldr-cli --test language_adapters_completeness_v1
```

Expected: all supported fixtures retain their definitions, calls, and chunk boundaries; the invocation counter is one.

- [ ] **Step 6: Commit**

```bash
git add crates/tldr-core/src/artifact_store crates/tldr-core/src/callgraph/cross_file_types.rs crates/tldr-core/src/semantic/chunker.rs crates/tldr-core/tests/file_facts_tests.rs
git commit -m "feat(ingestion): parse file revisions into shared facts"
```

### Task 5: Build one resumable bulk and incremental ingestion engine

**Files:**
- Create: `crates/tldr-core/src/artifact_store/ingestion.rs`
- Create: `crates/tldr-core/src/artifact_store/planner.rs`
- Create: `crates/tldr-core/src/artifact_store/writer.rs`
- Modify: `crates/tldr-core/src/semantic/build_pipeline.rs`
- Test: `crates/tldr-core/tests/resumable_ingestion_tests.rs`

- [ ] **Step 1: Write crash-at-every-stage resume tests**

```rust
for stage in IngestionStage::ALL {
    let store = test_store();
    run_with_crash(&store, fixture_project(), stage);
    let resumed = IngestionEngine::resume(store).unwrap();
    assert_eq!(resumed.manifest(), clean_build_manifest());
    assert_eq!(resumed.parse_count("unchanged.rs"), 1);
}
```

- [ ] **Step 2: Define one job model for bulk and delta work**

```rust
pub enum IngestionScope {
    Project,
    Files(Vec<String>),
}

pub struct IngestionJob {
    pub id: String,
    pub target_generation: u64,
    pub scope: IngestionScope,
    pub stage: IngestionStage,
    pub next_batch: u64,
    pub source_manifest: SourceManifest,
}
```

- [ ] **Step 3: Implement bounded parallel producers and one writer**

Workers may read, parse, and derive artifacts concurrently. Only `ArtifactWriter` owns redb write transactions:

```rust
pub struct ArtifactWriter {
    receiver: tokio::sync::mpsc::Receiver<ArtifactBatch>,
    store: std::sync::Arc<RedbArtifactStore>,
}
```

- [ ] **Step 4: Coalesce incremental revisions**

For multiple queued changes to one canonical path, retain only the newest content revision. Before commit, compare the planned revision with the latest source manifest and discard stale work.

- [ ] **Step 5: Publish only validated generations**

Validate artifact checksums, dependency closure, file manifest coverage, call-edge symbol identities, and semantic vector dimensions before changing `active_generation`.

- [ ] **Step 6: Run ingestion tests**

Run:

```bash
cargo test -p tldr-core --test resumable_ingestion_tests
```

Expected: every injected interruption resumes; stale deltas never publish; readers observe only complete generations.

- [ ] **Step 7: Commit**

```bash
git add crates/tldr-core/src/artifact_store crates/tldr-core/src/semantic/build_pipeline.rs crates/tldr-core/tests/resumable_ingestion_tests.rs
git commit -m "feat(ingestion): unify resumable bulk and delta jobs"
```

### Task 6: Move exact structural indexes onto artifacts

**Files:**
- Create: `crates/tldr-core/src/artifact_store/projections.rs`
- Modify: `crates/tldr-core/src/callgraph/mod.rs`
- Modify: `crates/tldr-core/src/analysis/mod.rs`
- Modify: `crates/tldr-core/src/context/builder.rs`
- Test: `crates/tldr-core/tests/artifact_projection_tests.rs`

- [ ] **Step 1: Write projection equivalence tests**

For each fixture, compare the existing analyzer result with the artifact projection after removing timing and ordering-only fields.

- [ ] **Step 2: Compose the call graph from stored per-file edges**

```rust
pub fn project_call_graph(
    store: &dyn ArtifactStore,
    generation: u64,
) -> Result<ProjectCallGraph, AnalysisError> {
    let edges = store.artifacts(generation, ArtifactKind::CallEdges)?;
    ProjectCallGraph::compose(edges)
}
```

- [ ] **Step 3: Route reference, definition, impact, dead, hubs, and coupling queries**

These commands must reuse stored definitions, references, and call edges rather than walking or parsing source.

- [ ] **Step 4: Keep result rendering outside the store**

Persist typed artifacts only. Do not persist `serde_json::Value` or final CLI response objects as authoritative records.

- [ ] **Step 5: Run exact-analysis parity tests**

Run:

```bash
cargo test -p tldr-core --test artifact_projection_tests
cargo test -p tldr-cli --test cross_command_consistency_v3
```

Expected: projections are structurally equivalent to the existing command results.

- [ ] **Step 6: Commit**

```bash
git add crates/tldr-core/src/artifact_store/projections.rs crates/tldr-core/src/callgraph crates/tldr-core/src/analysis crates/tldr-core/src/context crates/tldr-core/tests/artifact_projection_tests.rs
git commit -m "refactor(analysis): serve exact queries from artifacts"
```

### Task 7: Persist demand-driven CFG, DFG, and PDG artifacts

**Files:**
- Create: `crates/tldr-core/src/artifact_store/function_artifacts.rs`
- Modify: `crates/tldr-core/src/cfg/mod.rs`
- Modify: `crates/tldr-core/src/dfg/mod.rs`
- Modify: `crates/tldr-core/src/pdg/mod.rs`
- Test: `crates/tldr-core/tests/function_artifact_tests.rs`

- [ ] **Step 1: Write function revision and dependency tests**

```rust
#[test]
fn dfg_depends_on_the_exact_cfg_revision() {
    let cfg = build_cfg(&facts("fn f() { let x = 1; }"));
    let dfg = build_dfg(&cfg);
    assert_eq!(dfg.dependencies(), &[cfg.key().clone()]);
}
```

- [ ] **Step 2: Store CFG by stable function anchor and file revision**

CFG production consumes `FunctionFact`, not source text. The key includes language and CFG producer version.

- [ ] **Step 3: Make DFG and PDG dependency-explicit**

Persist `DFG -> CFG` and `PDG -> CFG + DFG` edges in `ARTIFACT_DEPS`.

- [ ] **Step 4: Add per-key single-flight construction**

An absent function artifact may be computed synchronously through the coordinator, but concurrent identical requests share one computation and one commit.

- [ ] **Step 5: Rebuild only previously materialized optional artifacts after edits**

Bulk ingestion eagerly builds shared file facts and call edges. CFG/DFG/PDG remain demand-driven; a delta rebuilds an optional artifact only when the previous generation contained that artifact.

- [ ] **Step 6: Run function analysis tests**

Run:

```bash
cargo test -p tldr-core --test function_artifact_tests
cargo test -p tldr-cli commands::daemon --lib
```

Expected: unchanged functions reuse artifacts; changed functions receive new keys; dependent artifacts never cross revisions.

- [ ] **Step 7: Commit**

```bash
git add crates/tldr-core/src/artifact_store/function_artifacts.rs crates/tldr-core/src/cfg crates/tldr-core/src/dfg crates/tldr-core/src/pdg crates/tldr-core/tests/function_artifact_tests.rs
git commit -m "feat(analysis): persist function-level graph artifacts"
```

### Task 8: Put semantic indexing on the shared generation

**Files:**
- Modify: `crates/tldr-core/src/semantic/vector_store.rs`
- Modify: `crates/tldr-core/src/semantic/generation.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/index_manager.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/bulk_worker.rs`
- Test: `crates/tldr-cli/tests/shared_generation_semantic_tests.rs`

- [ ] **Step 1: Write same-generation hybrid consistency tests**

```rust
#[test]
fn semantic_hit_resolves_to_symbol_in_same_generation() {
    let generation = build_fixture_generation();
    let hit = semantic_query(generation, "authentication").first().unwrap();
    assert!(symbol_store(generation).contains(&hit.symbol_anchor));
}
```

- [ ] **Step 2: Remove semantic-owned file walking and parsing**

The semantic builder consumes stored `SemanticChunks` artifacts from the unified ingestion stream.

- [ ] **Step 3: Make vector publication part of the project manifest**

The manifest records vector recipe, dimensions, usearch generation identity, and the exact `SemanticChunks` artifact set.

- [ ] **Step 4: Preserve workload-specific inference runners**

Keep separate query, delta, and bulk inference sessions; generalizing storage must not merge their locks or execution queues.

- [ ] **Step 5: Run semantic and hybrid tests**

Run:

```bash
cargo test -p tldr-cli --test shared_generation_semantic_tests
cargo test -p tldr-core semantic
```

Expected: semantic results resolve to current structural identities and existing semantic equivalence tests remain green.

- [ ] **Step 6: Commit**

```bash
git add crates/tldr-core/src/semantic crates/tldr-cli/src/commands/daemon/index_manager.rs crates/tldr-cli/src/commands/daemon/bulk_worker.rs crates/tldr-cli/tests/shared_generation_semantic_tests.rs
git commit -m "refactor(semantic): join the shared artifact generation"
```

### Task 9: Replace daemon cache orchestration with ArtifactManager

**Files:**
- Create: `crates/tldr-cli/src/commands/daemon/artifact_manager.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/mod.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/daemon.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/watcher.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/types.rs`
- Test: `crates/tldr-cli/tests/artifact_daemon_tests.rs`

- [ ] **Step 1: Write daemon readiness and snapshot tests**

Test `Cold`, `Building`, `Ready { generation }`, `Degraded`, and `Failed` states. Verify a request pins one generation for its entire lifetime.

- [ ] **Step 2: Define the daemon-facing manager**

```rust
pub struct ArtifactManager {
    store: std::sync::Arc<RedbArtifactStore>,
    ingestion: IngestionCoordinator,
    hot: HotGenerationCache,
}

impl ArtifactManager {
    pub fn query(&self, request: ArtifactQuery) -> Result<ArtifactResponse, QueryError>;
    pub fn ingest(&self, change: SourceChange) -> Result<JobId, IngestionError>;
    pub fn state(&self) -> ArtifactState;
}
```

- [ ] **Step 3: Route watcher changes into the unified planner**

Replace direct `QueryCache::invalidate_by_input` and separate semantic `apply_delta` calls with one `SourceChange` submission.

- [ ] **Step 4: Keep queries off redb write locks**

Read and decode required artifacts in bounded read transactions. Build in-memory graph snapshots outside transactions and cache them by generation.

- [ ] **Step 5: Expose generation and ingestion status**

Daemon status reports active generation, target generation, completed/total batches, pending files, last error, redb bytes, and hot-cache bytes.

- [ ] **Step 6: Run daemon tests**

Run:

```bash
cargo test -p tldr-cli --test artifact_daemon_tests
cargo test -p tldr-cli commands::daemon --lib
```

Expected: watcher updates publish atomically, concurrent readers remain on one generation, and status stays responsive during builds.

- [ ] **Step 7: Commit**

```bash
git add crates/tldr-cli/src/commands/daemon
git commit -m "refactor(daemon): coordinate artifacts and ingestion"
```

### Task 10: Cut every CLI surface over and delete legacy runtime paths

**Files:**
- Delete: `crates/tldr-cli/src/commands/daemon/salsa.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/warm.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/cache_clear.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/cache_stats.rs`
- Modify: command modules under `crates/tldr-cli/src/commands/`
- Test: `crates/tldr-cli/tests/one_shot_cutover_tests.rs`

- [ ] **Step 1: Write a forbidden-legacy-path test**

```rust
#[test]
fn released_commands_have_no_legacy_cache_fallback() {
    for command in all_daemon_capable_commands() {
        assert_eq!(command.backend(), Backend::ArtifactStore);
    }
}
```

- [ ] **Step 2: Route all supported commands through typed requests**

Every request carries canonical project identity, pinned generation selection, language, typed options, and output-independent query fields.

- [ ] **Step 3: Replace `warm` behavior**

`tldr warm` starts or resumes the unified core-artifact generation. It does not maintain a separate call-graph JSON cache.

- [ ] **Step 4: Replace cache administration**

`tldr cache stats` reports redb, generation, artifact-kind, and hot-cache statistics. `tldr cache clear` retires generations transactionally and removes derived usearch files after no reader pins them.

- [ ] **Step 5: Delete legacy cache code and fallback branches**

Remove `QueryCache`, legacy JSON persistence, command-specific project answer blobs, duplicate semantic file walking, and silent local fallback.

- [ ] **Step 6: Run the full CLI cutover matrix**

Run:

```bash
cargo test -p tldr-cli --test one_shot_cutover_tests
cargo test -p tldr-cli --tests
```

Expected: all commands use `ArtifactStore`; searching the compiled command registry finds no legacy backend variant.

- [ ] **Step 7: Commit**

```bash
git add crates/tldr-cli
git commit -m "refactor(cli)!: complete artifact-store cutover"
```

### Task 11: Prove recovery, concurrency, storage, and performance

**Files:**
- Create: `crates/tldr-cli/tests/artifact_cutover_acceptance.rs`
- Modify: `crates/tldr-cli/tests/artifact_store_baseline.rs`
- Modify: `docs/STRUCTURAL_EMBEDDING_PIPELINE.md`

- [ ] **Step 1: Run crash-recovery acceptance**

Kill ingestion after discovery, parsing, artifact derivation, batch commit, validation, and immediately before publication. Restart the daemon and verify the final manifest equals a clean build.

- [ ] **Step 2: Run concurrent read/write acceptance**

Issue structural and semantic queries while a delta generation builds. Assert every response reports one generation and no response combines old symbols with new edges or vectors.

- [ ] **Step 3: Run result-equivalence acceptance**

Compare every command fixture with the frozen baseline, allowing only documented ordering and telemetry differences.

- [ ] **Step 4: Run performance acceptance**

```bash
cargo test -p tldr-cli --test artifact_cutover_acceptance -- --ignored --nocapture
cargo test -p tldr-cli --test artifact_store_baseline -- --ignored --nocapture
```

Reject the cutover if unchanged files are reparsed, restart triggers a project parse, a one-file edit rebuilds unrelated file artifacts, or repeated mixed commands are slower without an explained measurement.

- [ ] **Step 5: Record measured outcomes**

Update `docs/STRUCTURAL_EMBEDDING_PIPELINE.md` with cold build time, resumed build time, mixed-command p50/p95, edit-to-ready latency, peak RSS, redb bytes by artifact kind, and comparison with the frozen baseline.

- [ ] **Step 6: Commit**

```bash
git add crates/tldr-cli/tests/artifact_cutover_acceptance.rs crates/tldr-cli/tests/artifact_store_baseline.rs docs/STRUCTURAL_EMBEDDING_PIPELINE.md
git commit -m "test(artifact-store): prove one-shot cutover gates"
```

### Task 12: Release, rollback, and cleanup

**Files:**
- Modify: `crates/tldr-core/src/artifact_store/schema.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/start.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/cache_clear.rs`
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Give the new store an incompatible identity**

Use a new file name and explicit schema identifier:

```rust
pub const STORE_FILE: &str = "project.redb";
pub const STORE_SCHEMA: &str = "tldr-artifact-store-v1";
```

The new release never opens legacy cache files as the authoritative store.

- [ ] **Step 2: Implement first-start behavior**

Start the daemon, open or create `project.redb`, inspect resumable jobs, resume when compatible, otherwise start generation 1 from source, and report progress through daemon status.

- [ ] **Step 3: Preserve executable rollback**

Do not delete legacy cache files during first activation. If the new release is rolled back, the previous executable can use its previous caches. After the new release is accepted, an explicit cache-clean operation removes legacy files.

- [ ] **Step 4: Add generation rollback**

If activation validation fails, retain the previous active generation. If generation 1 has never published, remain `NotReady` with the durable failure reason and resumable job record.

- [ ] **Step 5: Run release gates**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: formatting, linting, unit, integration, recovery, and acceptance suites all pass.

- [ ] **Step 6: Document the breaking storage cutover**

State that the first vNext run builds `project.redb`, the build is resumable, old caches are ignored but initially preserved for executable rollback, and all command surfaces use the shared artifact generation.

- [ ] **Step 7: Commit**

```bash
git add crates/tldr-core/src/artifact_store/schema.rs crates/tldr-cli/src/commands/daemon/start.rs crates/tldr-cli/src/commands/daemon/cache_clear.rs CHANGELOG.md
git commit -m "chore(release): finalize artifact-store cutover"
```

## Final acceptance decision

Ship the one-shot cutover only when all conditions are true:

```text
[ ] every supported command uses ArtifactStore
[ ] no legacy QueryCache or JSON answer-cache runtime remains
[ ] bulk and incremental ingestion share one resumable engine
[ ] one file revision is parsed at most once
[ ] semantic and structural results pin the same generation
[ ] crash recovery passes at every durable stage
[ ] result equivalence passes across the CLI matrix
[ ] measured warm/restart/edit workloads improve or have an accepted explanation
[ ] rollback to the previous executable remains possible
```

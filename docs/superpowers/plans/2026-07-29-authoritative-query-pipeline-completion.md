# Authoritative Query Pipeline Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish the redb ArtifactStore cutover so every supported query has one authoritative execution path, resident retrieval indexes never rebuild the corpus on a warm query, and obsolete daemon/cache APIs cannot be mistaken for supported infrastructure.

**Architecture:** Keep the Unix-socket/TCP IPC daemon implemented in `tldr-cli` as the only runtime. Treat `GenerationSnapshot` as the immutable source for structural facts and lexical documents, attach resident derived indexes to its generation, and expose typed daemon commands consumed by CLI and MCP. Preserve intentionally local cursor parsing and external-tool orchestration, but make those classifications executable and remove unused protocol variants and fallback APIs.

**Tech Stack:** Rust 2021, redb ArtifactStore, `GenerationSnapshot`, usearch, BM25, serde IPC, Tokio, Clap, Cargo tests, Beads.

---

### Task 1: Establish the executable command-route contract

**Files:**
- Create: `crates/tldr-cli/src/commands/route_contract.rs`
- Modify: `crates/tldr-cli/src/commands/mod.rs`
- Modify: `crates/tldr-cli/src/main.rs`
- Test: `crates/tldr-cli/tests/command_route_contract.rs`

- [ ] **Step 1: Write a failing inventory test**

  Add a test that enumerates every `Command` discriminant through a stable `CommandCapability` registry and asserts that each entry declares one owner: `ArtifactProjection`, `ResidentIndex`, `IntentionalLocal`, `ExternalTool`, `Lifecycle`, `Mutation`, or `Parked`.

- [ ] **Step 2: Run the focused test and verify the missing registry fails**

  Run `cargo test -p tldr-cli --test command_route_contract`.
  Expected: compilation fails because `route_contract` and `CommandCapability` do not exist.

- [ ] **Step 3: Implement the registry and orphan-protocol checks**

  Define:

  ```rust
  pub enum ExecutionOwner {
      ArtifactProjection,
      ResidentIndex,
      IntentionalLocal,
      ExternalTool,
      Lifecycle,
      Mutation,
      Parked,
  }

  pub struct CommandCapability {
      pub command: &'static str,
      pub owner: ExecutionOwner,
      pub daemon_command: Option<&'static str>,
      pub rationale: &'static str,
  }
  ```

  Expose `COMMAND_CAPABILITIES`, require unique command names, and add a daemon-protocol inventory so a protocol variant cannot exist without a supported client or an explicit compatibility classification.

- [ ] **Step 4: Run the route-contract test**

  Run `cargo test -p tldr-cli --test command_route_contract`.
  Expected: pass with all current commands explicitly classified.

### Task 2: Make the CLI IPC daemon authoritative

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/tldr-cli/Cargo.toml`
- Delete: `crates/tldr-cli/src/bin/tldr_daemon.rs`
- Delete: `crates/tldr-daemon/Cargo.toml`
- Delete: `crates/tldr-daemon/src/lib.rs`
- Delete: `crates/tldr-daemon/src/server.rs`
- Delete: `crates/tldr-daemon/src/state.rs`
- Delete: remaining files under `crates/tldr-daemon/src/`
- Modify: daemon documentation that still advertises the standalone HTTP runtime

- [ ] **Step 1: Add a packaging test**

  Extend the route-contract test to assert that the workspace contains one daemon implementation and that the installed `tldr-daemon` compatibility binary is absent unless backed by the authoritative IPC implementation.

- [ ] **Step 2: Verify the packaging test fails**

  Run `cargo test -p tldr-cli --test command_route_contract`.
  Expected: fail because the standalone `tldr-daemon` workspace member and wrapper binary still exist.

- [ ] **Step 3: Remove the duplicate daemon**

  Remove the standalone crate from workspace membership, remove the `tldr-daemon` dependency and wrapper binary from `tldr-cli`, and regenerate `Cargo.lock`. Keep `tldr daemon start|status|stop|query` as the sole lifecycle surface.

- [ ] **Step 4: Remove or classify stale protocol variants**

  Delete `Cfg`, `Dfg`, `Arch`, and `Diagnostics` daemon variants when no supported client exists. Keep `Slice` and `ChangeImpact` local until full flag parity exists, and represent that decision in `COMMAND_CAPABILITIES`. Replace the old regex `Search` command when Task 4 lands rather than retaining two meanings for `"search"`.

- [ ] **Step 5: Validate daemon lifecycle**

  Run `cargo test -p tldr-cli daemon`.
  Run `cargo check --workspace --all-targets`.
  Expected: pass with only the CLI IPC daemon compiled.

### Task 3: Build generation-pinned BM25 from ArtifactStore facts

**Files:**
- Modify: `crates/tldr-core/src/search/bm25.rs`
- Modify: `crates/tldr-core/src/artifact_store/projections.rs`
- Create: `crates/tldr-cli/src/commands/daemon/search_index_manager.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/mod.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/daemon.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/watcher.rs`
- Test: `crates/tldr-core/tests/artifact_bm25.rs`
- Test: daemon manager unit tests

- [ ] **Step 1: Write failing snapshot-index tests**

  Cover initial construction, edit replacement, delete, rename, language filtering, path scoping, and ignore-policy generation replacement. Instrument construction so a query test can assert zero `from_project` calls.

- [ ] **Step 2: Verify the tests fail**

  Run `cargo test -p tldr-core --test artifact_bm25`.
  Expected: fail because no snapshot-backed BM25 constructor exists.

- [ ] **Step 3: Add a source-independent BM25 constructor**

  Add `Bm25Index::from_documents` and a `GenerationSnapshot::bm25_documents` projection. Documents carry normalized relative path and stored source text/chunks; query-time code must not call `ProjectWalker` or `fs::read_to_string`.

- [ ] **Step 4: Add the resident manager**

  Implement a lock-protected manager containing:

  ```rust
  struct SearchIndexState {
      generation: u64,
      by_language: HashMap<Language, Arc<Bm25Index>>,
  }
  ```

  Rebuild or replace it when the published artifact generation changes. Queries return `not_ready` on a missing or mismatched generation; they never build under the query lock.

- [ ] **Step 5: Validate construction and freshness**

  Run the core snapshot tests and daemon manager tests.
  Expected: pass, including zero source-walk assertions on repeated queries.

### Task 4: Route CLI and MCP lexical/hybrid search through the resident index

**Files:**
- Modify: `crates/tldr-cli/src/commands/daemon/types.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/daemon.rs`
- Modify: `crates/tldr-cli/src/commands/search.rs`
- Modify: `crates/tldr-cli/src/commands/daemon_router.rs`
- Modify: `crates/tldr-core/src/search/enriched.rs`
- Modify: `crates/tldr-core/src/search/hybrid.rs`
- Modify: `crates/tldr-mcp/src/tools/search.rs`
- Add or modify MCP daemon client files
- Test: CLI daemon parity tests
- Test: MCP search tests

- [ ] **Step 1: Write failing typed-search parity tests**

  Exercise query, path, language, top-k, regex mode, test inclusion, name boost, call-graph enrichment, output data, and explicit not-ready behavior. Add a test proving CLI and MCP send the same typed request.

- [ ] **Step 2: Verify the tests fail**

  Run focused CLI and MCP search tests.
  Expected: fail because CLI and MCP still build locally and the daemon `Search` protocol lacks current semantics.

- [ ] **Step 3: Replace the stale daemon Search command**

  Define a complete request envelope and execute `enriched_search_with_index` against the resident BM25 index plus the pinned snapshot call graph/definitions. Do not keep the former regex-only daemon meaning.

- [ ] **Step 4: Route CLI and MCP**

  Make `tldr search` strict-daemon by default with `--oneshot` as the explicit local build path. Make MCP use the authoritative daemon request and return `not_ready` rather than silently rebuilding.

- [ ] **Step 5: Wire hybrid retrieval**

  Use resident BM25 and vector ranks from the same published generation, fuse with RRF, and fail explicitly when either required resident index is unavailable. Preserve lexical-only `tldr search`.

- [ ] **Step 6: Run search parity and repeated-query tests**

  Expected: identical supported outputs and zero query-path corpus rebuild.

### Task 5: Migrate remaining project-wide structural consumers

**Files:**
- Modify: `crates/tldr-cli/src/commands/daemon/types.rs`
- Modify: `crates/tldr-cli/src/commands/daemon/daemon.rs`
- Modify: `crates/tldr-cli/src/commands/references.rs`
- Modify: `crates/tldr-cli/src/commands/remaining/definition.rs`
- Modify: `crates/tldr-cli/src/commands/patterns/coupling.rs`
- Modify: `crates/tldr-cli/src/commands/deps.rs`
- Modify: `crates/tldr-core/src/artifact_store/projections.rs`
- Add focused parity tests for each command

- [ ] **Step 1: Add failing daemon-versus-oneshot tests**

  Freeze representative Rust, Python, TypeScript, and Lua fixtures. Compare references, global definition lookup, project coupling, and dependency reports while leaving cursor-local definition resolution and pairwise coupling intentionally local.

- [ ] **Step 2: Add missing projections**

  Build reference occurrences, global definition lookup, dependency reports, and project coupling from stored `DefinitionFact`, `ImportFact`, and call edges. Scope every projection to the requested path and language.

- [ ] **Step 3: Add typed daemon commands**

  Add complete request envelopes and snapshot handlers for project-wide phases. Do not route unsupported flags; require `--oneshot` with a precise error when canonical stored-policy semantics cannot represent a request.

- [ ] **Step 4: Split mixed commands at the correct boundary**

  Keep local cursor parsing/local-variable lookup inside `definition`; route only workspace/global lookup. Keep pair-mode `coupling` local; route project mode. Route `deps` and project-wide `references` fully.

- [ ] **Step 5: Run differential and freshness tests**

  Expected: parity passes and repeated daemon queries perform no project parse/walk.

### Task 6: Retire obsolete cache, cold-search, and fallback APIs

**Files:**
- Modify: `crates/tldr-cli/src/commands/daemon_router.rs`
- Modify: `crates/tldr-cli/src/commands/mod.rs`
- Modify: `crates/tldr-core/src/semantic/store_search.rs`
- Modify: `crates/tldr-core/src/semantic/mod.rs`
- Modify: `crates/tldr-core/src/search/enriched.rs`
- Modify: `crates/tldr-core/src/search/hybrid.rs`
- Modify: `crates/tldr-core/src/search/mod.rs`
- Modify: public re-export modules and tests

- [ ] **Step 1: Classify every zero-production-caller API**

  Retain `enriched_search_with_index` as the resident lexical seam. Remove silent `try_daemon_route`, cold semantic `search_with_store`/`query_store`, JSON structure/callgraph cache variants, and duplicate hybrid report/type surfaces unless a supported compatibility test proves a consumer.

- [ ] **Step 2: Add compile-time public-surface tests**

  Ensure supported entry points remain exported and deleted legacy APIs are absent from runtime modules.

- [ ] **Step 3: Remove implementations and re-exports**

  Delete obsolete JSON persistence types and helpers, update module documentation to name ArtifactStore and resident indexes as authoritative, and remove stale Salsa/cold-cache guidance.

- [ ] **Step 4: Run crate API tests**

  Run `cargo test -p tldr-core` and `cargo test -p tldr-cli`.
  Expected: pass without compatibility shims that restore silent fallback.

### Task 7: Prove closure on Dhan and the full workspace

**Files:**
- Modify: `crates/tldr-cli/tests/command_route_contract.rs`
- Create: `docs/benchmarks/2026-07-29-authoritative-query-pipeline/SUMMARY.md`
- Modify: relevant architecture/runtime documentation

- [ ] **Step 1: Run formatting and static gates**

  Run `cargo fmt --all -- --check`.
  Run `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
  Run `cargo check --workspace --all-targets`.

- [ ] **Step 2: Run the complete test suite**

  Run `cargo test --workspace --all-features`.
  Expected: pass.

- [ ] **Step 3: Run CodeRabbit over the implementation diff**

  Confirm changed files contain no credentials, then run `coderabbit review --agent -t uncommitted`. Fix critical and warning findings and repeat until clean or only informational findings remain.

- [ ] **Step 4: Benchmark Dhan repeated queries**

  With the Dhan daemon warm, run repeated lexical search, semantic search, references, definition, coupling, and deps probes. Record warm latency, active artifact/vector/BM25 generation, source-walk/build counters, and edit freshness.

- [ ] **Step 5: Close Beads only from evidence**

  Close implementation issues in dependency order, close `TLDR-eda5.1` only after its executable matrix passes, and close `TLDR-eda5` only after every success criterion is evidenced. Do not commit, sync, or push without authority from the active user/profile.

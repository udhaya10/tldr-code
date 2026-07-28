# Fully Cold Runtime Benchmark Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Measure the current installed tool from a genuinely empty project index and document-embedding cache through full rebuild, warm daemon serving, CLI/search queries, and delta indexing.

**Architecture:** Preserve downloaded model weights so network latency is excluded, but recoverably remove the current project’s structural/vector stores and the global content-addressed document cache. Run exactly one foreground `warm --oneshot` build with correlated phase/unit metrics, then start the daemon from the published store and execute a fixed repeated-query matrix. Store raw evidence and a concise comparative summary in a dated benchmark directory.

**Tech Stack:** Installed release `tldr` binaries, redb embedding/artifact stores, usearch vector generations, `/usr/bin/time`, shell/JSON metrics, daemon status telemetry.

---

### Task 1: Inventory and isolate all rebuildable state

**Files:**
- Create: `docs/benchmarks/2026-07-29-fully-cold-runtime/RUN_CONTEXT.md`
- Modify: Beads issue `TLDR-by82`

- [x] **Step 1: Record source and binary identity**

Capture:

```bash
git rev-parse HEAD
shasum -a 256 ~/.cargo/bin/tldr ~/.cargo/bin/tldr-daemon \
  ~/.cargo/bin/tldr-embed-worker ~/.cargo/bin/tldr-mcp
tldr --version
```

- [x] **Step 2: Resolve exact runtime paths**

Record and size:

```text
<project>/.tldr
~/Library/Caches/tldr/embeddings
~/Library/Caches/tldr/stores/<project-hash>
~/Library/Caches/tldr/fastembed
~/Library/Logs/tldr
<TMPDIR>/tldr-11277ce0.{pid,sock,poke}
```

The fastembed directory contains downloaded model weights and must be retained.
The embedding cache is global; remove it with the installed
`tldr embeddings clear` command because a true first-index benchmark requires
zero document-vector hits.

- [x] **Step 3: Stop all current-project writers**

Stop the project daemon, verify its PID exited, and verify no `tldr embed`,
`tldr warm`, or `tldr-embed-worker` process targets this project.

- [x] **Step 4: Clear benchmark state**

Use `tldr cache clear --project <exact-path>` for project state and
`tldr embeddings clear` for the global reusable-vector cache. Move only
remaining project logs/runtime metadata to a uniquely named directory under
`~/.Trash`. Do not use unresolved globs or remove
`~/Library/Caches/tldr/fastembed`.

- [x] **Step 5: Prove the state is cold**

Verify project generated artifacts, the global embedding cache, project vector
store, logs, and project socket artifacts are absent. Record the recoverable
Trash path for the remaining runtime metadata; the global embedding-cache
deletion itself is intentionally irreversible.

### Task 2: Run one fully cold correlated build

**Files:**
- Create: `docs/benchmarks/2026-07-29-fully-cold-runtime/build-report.json`
- Create: `docs/benchmarks/2026-07-29-fully-cold-runtime/build-report.units.jsonl`
- Create: `docs/benchmarks/2026-07-29-fully-cold-runtime/build-console.log`

- [x] **Step 1: Install the exact source state**

Run:

```bash
cargo install --path crates/tldr-cli --force --locked
```

Record installed binary hashes after installation.

- [x] **Step 2: Start one foreground build**

Run:

```bash
/usr/bin/time -lp tldr warm . --oneshot \
  --metrics docs/benchmarks/2026-07-29-fully-cold-runtime/build-report.json \
  --metrics-detail units
```

Redirect combined output to `build-console.log`. Do not start the daemon,
semantic searches, or another embed process during this command.

- [x] **Step 3: Monitor without competing reads**

Use process/RSS inspection only. Report progress at phase boundaries or at most
once per minute. Never retry while the original build is alive.

- [x] **Step 4: Validate cold-build accounting**

The report must show 511 source files, zero pre-existing embedding-cache hits
apart from legitimate intra-run duplicate-content hits, nonzero inference,
one successful publication, no retries/failures, and bounded peak RSS.

### Task 3: Benchmark daemon startup and CLI/query serving

**Files:**
- Create: `docs/benchmarks/2026-07-29-fully-cold-runtime/query-metrics.jsonl`
- Create: `docs/benchmarks/2026-07-29-fully-cold-runtime/query-results/`

- [x] **Step 1: Initialize and start serving**

Time `tldr init`, daemon start, artifact-ready, and semantic-warm milestones.
Capture daemon RSS, vector count, runner session/request counters, Salsa cache
counters, generation, and store bytes.

- [x] **Step 2: Run a repeated command matrix**

Run each command once for first-hit timing and four more times for warm timing:

```bash
tldr tree . -f compact
tldr structure . -f compact
tldr extract crates/tldr-cli/src/commands/daemon/watcher.rs -f compact
tldr dead . -f compact
tldr search "fixed five second watcher batch queue" . --no-callgraph -f compact
tldr search "semantic delta source chunks" . -f compact
tldr semantic "where watcher events are collected into fixed batches" . -f compact
tldr semantic "avoid full corpus work when one source file changes" . -f compact
```

Capture process wall time, exit status, output bytes, and command-reported
latency where present. Retain the first result for correctness inspection.

- [x] **Step 3: Measure daemon cache effects**

Record Salsa counters before the matrix, after first-hit commands, and after
repeat commands. Report median, minimum, maximum, first-hit, and warm median
for each command.

- [x] **Step 4: Measure delta indexing**

Measure one mtime-only event and one reversible one-line source edit. Confirm
the fixed five-second batching boundary, artifact generation increments,
documents embedded, model requests, total publication latency, and RSS.

### Task 4: Summarize, compare, and publish evidence

**Files:**
- Create: `docs/benchmarks/2026-07-29-fully-cold-runtime/SUMMARY.md`
- Modify: Beads issue `TLDR-by82`

- [x] **Step 1: Compare with prior baselines**

Compare fully cold wall/inference/planning/cache-write/RSS metrics with the
57m40s previous clean run and 30m00.68s context-root-coalesced run. Keep the
4m15.8s reusable-cache rebuild in a separate category.

- [x] **Step 2: Document serving results**

Report daemon startup, first and repeated CLI/query timings, lexical versus
semantic latency, delta latency, memory, disk footprint, and any commands that
still bypass daemon caches.

- [x] **Step 3: Preserve raw evidence**

Record SHA-256 hashes for the report, unit JSONL, console log, and query metrics.
State exactly which caches were deleted and which model weights were retained.

- [x] **Step 4: Close and synchronize**

Close `TLDR-by82` only after the evidence is internally consistent. Run final
git checks, commit benchmark artifacts, push Beads, and push `main` to `fork`.

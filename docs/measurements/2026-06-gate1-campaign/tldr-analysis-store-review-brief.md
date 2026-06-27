# CODEX REVIEW BRIEF — tldr analysis store redesign (TLDR-zde)
# Design review ONLY. No code changes. 2026-06-04.

## What you are reviewing
A finalized-pending-review architecture for replacing tldr's in-RAM, whole-project,
JSON-blob analysis cache with a disk-backed, zero-copy, chunk-invalidated graph store.
Two documents (read both):
1. /tmp/tldr-salsa-chunk-design.md — original three-layer design + Q1-Q10
2. /tmp/tldr-2026-research.md — research annex (salsa PR#967, Turbopack/turbo-persistence,
   rust-analyzer durabilities, Cloudflare mmap-sync, rkyv) with R1-R6 requirements rubric

## Decisions ALREADY MADE since those docs (review against these — they supersede):

D1. GRAPH-NATIVE LAYOUT: composed artifact = CSR (forward callees + reverse callers,
    flat offset/edge arrays) + u32 string interning + FST (fst crate) symbol→node
    index + file→node-range map in the manifest. rkyv for BOTH per-file chunks AND
    the composed artifact; everything mmap'd, accessed in place. JSON survives only
    as a per-response output projection at the CLI edge.

D2. TWO-FLOW GOVERNING CONSTRAINT (user mandate): the ENTIRE system has at most two
    data flows — Flow 1 = this analysis store, Flow 2 = the existing semantic vector
    store. Identical lifecycle (enumerate via shared ignore-oracle → build →
    generation-commit → mmap → per-file delta). ALL ~19 graph CLI commands become
    READ-ONLY PROJECTIONS over Flow 1; no command owns storage/caching/invalidation.
    The per-command QueryKey response cache is DELETED (Q5 = answered). Chunk schema
    carries ALL per-file facts including parse-time metrics so complexity/smells
    project from the same substrate. Single-function queries (slice/CFG/DFG) are
    stateless on-demand compute — not cached at all.

D3. REJECTED (ADR, do not relitigate unless you find a fatal flaw): Redis-class
    network KV; Apache Arrow for the hot path (columnar/analytical mismatch with
    point-lookup+traversal; legit only as future Parquet sidecar for temporal/churn
    analytics); graph databases; heap petgraph; adjacency matrices; bincode
    (demoted to spike-format only); adopting the salsa crate for v1 (its persistence
    is serde-based = parse-on-load, violating R1; we steal ValidateInput/additive
    persistence/durability concepts without the crate).

D4. REQUIREMENTS RUBRIC (user's): R1 near-zero ser/de CPU on reads; R2 disk size
    free; R3 RAM = index only (FST + offsets; graph bulk in OS page cache);
    R4 O(n)/O(n log n) disk AND RAM scaling, no O(n^2) structures anywhere;
    R5 ignore-first (.gitignore/.tldrignore filter at enumeration, shared oracle,
    ignore files themselves are durable inputs); R6 = your review merges into the
    research base.

D5. VERIFIED ALREADY: Gate 0 — FileIR is pure parse output; resolution phase takes
    &file_ir immutably (builder_v2.rs:924,953) and writes resolved edges to separate
    structures (:956). Chunks cannot cache stale resolution.

## CONTEXT FILES (read directly, repo root /Users/udhayakumar/Workspace/03-Parcadei-Ecosystem/tldr-code):
- crates/tldr-cli/src/commands/daemon/salsa.rs            (cache being replaced)
- crates/tldr-cli/src/commands/daemon/daemon.rs:125-220   (warm steps, triple-copy), :1359 (process_dirty_file)
- crates/tldr-core/src/callgraph/builder_v2.rs:610-700    (parse/compose split target)
- crates/tldr-core/src/callgraph/cross_file_types.rs:847  (FileIR = chunk unit)
- crates/tldr-core/src/semantic/vector_store.rs:600-640   (generation pattern to clone)
- crates/tldr-core/src/semantic/types.rs:464              (store_dir_for)
- crates/tldr-cli/src/commands/daemon/watcher.rs          (delta worker model)
- crates/tldr-cli/src/commands/daemon/index_manager.rs    (Flow-2 resident pattern to mirror)
- crates/tldr-cli/src/commands/daemon_router.rs           (command routing to be simplified)

## EMPIRICAL BASELINE (measured this session, on this machine):
- Callgraph warm phase: ~22.0GB phys_footprint transient (MALLOC_LARGE 21.8GB/317
  regions), ~3.5min, on 24MB source / 1570 files / ~28,200 semantic vectors.
- Warm-cache semantic rebuild: ~1min at 3.4GB. Cold embed phase: ~11.5GB resident.
- Daemon restart loses everything (cache is RAM-only).
- A watcher delta blocked on the store lock holds a busy token for the entire build.

## YOUR DELIVERABLES
1. VERDICT: approve / approve-with-amendments (named, concrete) / reject (with the
   specific requirement R1-R6 or invariant it fails).
2. Answers to the REMAINING open gates:
   - Gate 1 (THE blocker): is full-recompose-per-delta viable? Estimate compose cost
     from the code (steps 5-12 of builder_v2: ModuleIndex build, ImportResolver with
     LRU, ReExportTracer, per-call resolution over ~28k functions). Specify the exact
     bench harness to settle it (what to construct, what to measure, pass/fail line).
   - ID stability: file→node-range map means ranges shift on recompose. Find the
     failure modes (stale CLI readers holding old generation? cross-generation
     references?) and the minimal contract that prevents them.
   - rkyv risks: derive churn across the FileIR type tree (PathBuf/String/HashMap
     fields), schema evolution policy, bytecheck-on-mmap cost, alignment pitfalls.
     Is rkyv 0.8.x mature enough or do you recommend a specific version/pattern?
   - Q6 multi-language: chunks are per-file/per-language; does compose run per
     language or unified? What does the manifest need?
   - Q8: what from current salsa.rs semantics (revision counter ordering, dependents
     map, byte-cap eviction) must survive and where does it land in the new design?
3. ADVERSARIAL pass on D2: find a CLI command among the ~19 whose semantics CANNOT
   be served as a pure projection over chunks+CSR (i.e., would secretly need a third
   flow or per-command state). If none, say so explicitly.
4. WEB VERIFICATION (firecrawl CLI is installed and authenticated — use it):
   - `firecrawl search "<query>" --limit 5 -o /tmp/codex-fc-<topic>.json --json`
   - `firecrawl scrape "<url>" -o /tmp/codex-fc-<topic>.md`
   Use it to verify at minimum: (a) rkyv 0.8 maturity/known-issues for mmap+bytecheck
   workloads; (b) fst crate fitness for symbol indexes at ~30k-1M keys; (c) whether
   salsa's persistence followups (post-PR#967) changed anything material since Aug 2025;
   (d) any prior art on CSR-on-mmap code graphs (e.g., Glean/Meta, Kythe, stack-graphs)
   worth stealing from. Cite URLs in your findings.
5. Anything the design silently drops that production systems consider table stakes
   (crash-consistency edge cases, Windows mmap semantics, concurrent CLI readers
   during generation GC — Flow 2 solved reader-grace as TLDR-pdb; does Flow 1 need it?).

## HARD CONSTRAINTS
- Read-only: no file modifications, no cargo build/test, no daemon start/stop.
- A daemon + long warm build may be running (PID 63176) — do not disturb, do not
  touch ~/Library/Caches/tldr.
- firecrawl usage is allowed and encouraged (it writes only to /tmp and .firecrawl/).
- Budget your time: code reading + 3-6 firecrawl lookups, then write the verdict.

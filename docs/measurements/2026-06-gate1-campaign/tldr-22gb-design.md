# Warm-build 22GB memory blow-up: root cause, debug plan, and persistent chunked salsa cache

## 1. WHAT IS THE ISSUE

`tldr warm` on tldr-code (~24MB source, 1570 files) drives the daemon to ~22GB
physical footprint — a ~900x input blow-up — during the CALLGRAPH/STRUCTURE phase
(first ~3.5min), NOT the semantic/embedding phase as originally assumed.

Evidence (2026-06-04, branch build, instrumented via status Memory + ps + vmmap):
- Warm-cache run: dead-flat 11.4GB RSS plateau for ~3.5min, then an 8GB cliff
  (11.3→3.4GB between consecutive 15s samples) at the phase boundary.
- Cold run: vmmap at ~3min (embedding NOT started): Physical footprint 22.0GB,
  of which MALLOC_LARGE = 21.8GB across only 317-375 regions (avg ~70MB/region =
  giant Vec/map doubling-reallocs). status Memory said 13.7GB (peak 16.0) at the
  same moment — the mach resident-size probe underreports because macOS compresses
  idle pages; phys_footprint is the honest metric.
- Flat-then-cliff = one big retained working set, dropped wholesale when the warm
  step's scope ends. Not a leak; a transient retention problem.

## 2. WHY 22GB — four compounding causes (code-verified)

(a) TRIPLE-COPY cache insert (crates/tldr-cli/src/commands/daemon/daemon.rs:141-144,
    repeated for structure at 160-163):
      result = build_project_call_graph(...)      // copy 1: native structs, whole project
      val = serde_json::to_value(&result)         // copy 2: JSON DOM, 5-15x JSON text size
                                                  //   (every key a heap String, every node
                                                  //    a tagged enum in a Map)
      cache.insert(key, &val, vec![])             // copy 3: serde_json::to_vec(&val)
                                                  //   inside insert (salsa.rs:232)
    ALL THREE coexist at the insert call. The Value DOM is PURE WASTE:
    insert<T: Serialize> can take &result directly. Cheapest big win.

(b) Parallel parse retains everything (tldr-core callgraph/builder_v2.rs:638):
    build_indices_parallel holds source text + tree-sitter AST (10-50x source)
    + FileIR per file concurrently; then ALL FileIRs are retained in CallGraphIR
    through the entire resolution phase.

(c) Admitted index duplication (builder_v2.rs:682-685): "We need our own copies
    because the IR's indices use a different format" — FuncIndex/ClassIndex built
    twice in different shapes, both alive simultaneously.

(d) No string interning: owned String file paths / names / signatures duplicated
    across tens of thousands of nodes, edges, indices — then duplicated AGAIN in
    the JSON DOM.

Blow-up arithmetic: 24MB source → ~2.5GB ASTs+FileIRs → x2 duplicated indices →
+ JSON DOM (5-15x) stacked on the native copy → + serialized bytes → ~22GB transient.

## 3. HOW TO DEBUG — STEP-BY-STEP (run later; ~20min total)

PRECONDITION: current cold build finished; embedding cache repopulated on disk, so
the embed phase will be cache-hits (~1min) and only the callgraph phase (~3.5min,
the phase under investigation) recomputes. Salsa cache is in-RAM only → daemon
restart guarantees the callgraph phase re-runs.

Step A1 — instrumented daemon:
    TLDR=~/Workspace/03-Parcadei-Ecosystem/tldr-code/target/release/tldr
    cd ~/Workspace/03-Parcadei-Ecosystem/tldr-code
    $TLDR daemon stop -p .
    MallocStackLogging=1 $TLDR daemon start -p .
    $TLDR daemon list          # → PID
    $TLDR warm .

Step A2 — sample mid-plateau (~60-180s after warm ack, while status shows
    busy: warm-build and Memory > 8GB):
    malloc_history <PID> -allBySize | head -60     # TOP ALLOCATION SITES W/ STACKS
    vmmap --summary <PID> | grep -E 'Physical footprint|MALLOC'
    footprint <PID>                                # categorized snapshot
    Record the top-10 sites by total bytes; map each to causes (a)-(d).

Step A3 — secondary single-number harness (callgraph phase ISOLATED, no daemon):
    $TLDR daemon stop -p .
    /usr/bin/time -l $TLDR warm .       # foreground warm = callgraph only,
                                        # no semantic step; prints max RSS
    NOTE: foreground path skips the daemon's to_value/insert copies, so
    (A3 max-RSS) vs (A2 footprint) DIFFERENCE ≈ cost of cause (a) alone.

Step A4 — after fixes land, re-run A1-A3; acceptance = peak phys_footprint
    during warm. Targets: <8GB after fix (a); <4GB stretch after (a)+(b).

## 4. FIX PLAN (ranked by effort/payoff)

F1 (trivial): delete the to_value intermediate at daemon.rs:141-144 and 160-163 —
    pass &result straight to cache.insert. Expected: minus several GB.
F2: stream/drop AST+source immediately after FileIR extraction inside
    build_indices_parallel; consider bounding rayon width for peak control.
F3: unify the duplicated indices (IR-internal vs step-9 copies).
F4: intern strings (ustr/lasso) for paths+names across nodes/edges/indices.

## 5. SALSA CACHE → DISK + CHUNK-BASED INVALIDATION (user proposal, design sketch)

Current state (salsa.rs — homegrown, not the salsa crate):
- In-RAM DashMap<QueryKey, CacheEntry{serialized JSON bytes}>; only stats are
  persisted (.tldr/cache/salsa_stats.json). Daemon restart loses ALL entries →
  full 22GB recompute every restart.
- Granularity = WHOLE-PROJECT result per query type ("calls" for the entire repo
  is ONE entry). invalidate_by_input(file_hash) dependency tracking exists, but
  since values are project-sized, ONE changed file nukes the whole entry and the
  next query pays full recompute (22GB again). This granularity mismatch is the
  structural root of both the memory spike and the recompute cliff.

Proposal — per-file chunked, disk-backed cache:
- Persist per-FILE chunks (FileIR or equivalent) keyed by (relative path,
  content hash), e.g. .tldr/cache/fileir/<path-hash>.bin — compact binary
  (bincode/rkyv; rkyv enables mmap zero-copy loads), NOT JSON.
- Project-level results (call graph, structure) become a COMPOSE step over
  chunks: load all fresh chunks + run cross-file resolution. The expensive
  per-file parse/extract is memoized; only the resolution pass recomputes.
- Invalidation becomes chunk-based: file change → recompute ONLY that file's
  chunk (mirrors the t8f semantic per-file delta), then re-run compose.
- Daemon restart: load chunks from disk (seconds, mmap) instead of re-parsing
  the world (3.5min + 22GB).

Benefits: kills restart recompute; kills whole-project invalidation cliff;
bounds peak memory (compose can stream chunks instead of retaining all ASTs).

Risks / open questions:
- Cross-file edge resolution genuinely needs a global pass — measure compose
  cost alone (expected: small fraction of parse cost, but verify).
- Format versioning: schema_version field in chunk header; mismatch → recompute.
- Atomic write discipline: reuse the vector_store generation pattern
  (write-new-gen + commit marker) to avoid torn caches.
- Disk size: FileIR binary for 24MB source — estimate during implementation;
  likely tens-to-hundreds of MB, fine for a cache dir.
- Consistency with the semantic store freshness digest (TLDR-kkt) and with
  .tldrignore membership rules (TLDR-1qv) — chunk enumeration must use the same
  corpus walker or the two caches drift.
- The watcher's process_dirty_file already calls invalidate_by_input per file —
  the IPC/watcher plumbing for chunk-level invalidation ALREADY EXISTS; only the
  storage granularity and compose step are new.

## 6. RELATED
- TLDR-yll: RSS observation tooling (status Memory line) + max-RSS policy; the
  probe should report phys_footprint, not just resident (underreport observed).
- TLDR-1j2/TLDR-1qv: .tldrignore gaps; chunk enumeration must share corpus rules.

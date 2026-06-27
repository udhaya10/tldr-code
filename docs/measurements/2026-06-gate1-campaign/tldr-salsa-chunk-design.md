# Design: Disk-backed, chunk-invalidated analysis cache ("salsa v2")
## TLDR-zde — design only, no code yet. For Codex review.

## 0. The user's framing (design north star)

The semantic side already has the right shape:
- DISK:     usearch vector store + metadata, generation-committed, at
            ~/Library/Caches/tldr/stores/<md5-16-of-project>/
- RAM:      daemon-resident store (mmap/view(), near-zero-copy load)
- INVALIDATION: per-file delta (TLDR-t8f) — file change re-embeds ONE file's
            chunks, removes its old vectors, inserts new. No full rebuild.
- FRESHNESS: corpus digest gate (TLDR-kkt) decides "disk store still valid?"

The analysis cache (homegrown "salsa", crates/tldr-cli/src/commands/daemon/salsa.rs)
has NONE of this:
- RAM-only DashMap<QueryKey, CacheEntry{serialized JSON bytes}>; daemon restart
  loses everything → full recompute (measured: ~3.5min, ~22GB phys_footprint
  transient on tldr-code, see TLDR-k8s).
- Value granularity = WHOLE PROJECT per query type ("calls" for the entire repo
  is ONE entry, daemon.rs:133-148). input_hashes dependency tracking exists and
  the watcher already calls invalidate_by_input(file_hash) per changed file
  (daemon.rs:1368-1369) — but because the stored value is project-sized, one
  changed file evicts the whole entry and the next query pays full recompute.
- This granularity mismatch is the structural root of BOTH the 22GB transient
  (whole-project result + JSON DOM + bytes coexist) AND the invalidation cliff.

Goal: give the analysis cache the same three-layer shape the vector store has.

## 1. Architecture: three layers

### L1 — ChunkStore (disk, durable)
- Unit: per-file FileIR (crates/tldr-core/src/callgraph/cross_file_types.rs:847
  — path, funcs, classes, imports, var_types, calls; ALREADY serde-Serialize).
- Location: <store_dir_for(project)>/analysis/<schema_v>/chunks/<xxh64(rel_path)>.fir
  (reuses the existing per-project cache root, semantic/types.rs:464).
- Format: compact binary. Default candidate bincode (serde-native, zero new
  derive churn); rkyv as a stretch goal if mmap zero-copy loads prove necessary
  (rkyv requires its own derives on the whole FileIR type tree — non-trivial).
  NOT JSON (the floats-as-ASCII cache.json lesson, semantic/cache.rs:10).
- Chunk header: {schema_version, language, rel_path, content_hash(xxh64 of file
  bytes), tool_version}. Hash mismatch or schema mismatch → chunk is dead,
  recompute it.
- Manifest (one per generation): corpus membership list + per-file content
  hashes (THE SAME walker rules as semantic, ties into TLDR-1qv/TLDR-9w8 —
  chunk enumeration and corpus enumeration MUST share one membership oracle or
  the caches drift), schema_version, created_at.
- Atomicity: reuse the vector_store generation pattern
  (semantic/vector_store.rs:600-640 — write new generation dir, fsync, flip
  CURRENT marker, GC old generations; recovery walks generations newest-first).

### L2 — ResidentGraph (RAM, daemon)
- The composed CallGraphIR (+ structure view) held by the daemon behind
  ArcSwap<ComposedGraph> — readers grab a snapshot, never block the writer.
- This is the ONLY large retained structure. The QueryKey response cache
  (current salsa entries) shrinks to a thin response-serialization cache over
  the resident graph, or is removed for graph-backed queries entirely
  (open question Q5).

### L3 — ComposeEngine
- Input: all fresh chunks. Output: ResidentGraph.
- This is steps 5-12 of build_project_call_graph_v2 (builder_v2.rs:641 onward):
  add FileIRs → build indices → ModuleIndex → import resolution → cross-file
  edge creation. Today this is fused with parsing; the design SPLITS parse
  (per-file, memoizable) from compose (global, cheap-ish — MUST MEASURE, Q1).

## 2. The four paths

### Full/cold build (warm)
1. Enumerate corpus (shared membership oracle).
2. For each file IN PARALLEL: if ChunkStore has (path, content_hash) → skip;
   else parse → FileIR → write chunk to disk IMMEDIATELY → drop AST + source.
   (This alone fixes TLDR-k8s cause (b): no more all-ASTs-resident plateau.
   Peak becomes max-concurrent-parses × per-file cost, bounded by rayon width.)
3. Compose over all chunks → ResidentGraph (ArcSwap store).
4. Commit generation (manifest + CURRENT flip).

### Daemon restart
1. Load manifest; verify corpus digest against disk state (kkt-style gate;
   stat-based fast path, hash on mtime mismatch).
2. Fresh → load chunks (deserialize, streaming) → compose → resident in
   seconds. Stale subset → recompute only stale/missing chunks, then compose.
3. No manifest / schema mismatch → cold build path.

### Single-file change (watcher delta — mirrors t8f exactly)
1. process_dirty_file (daemon.rs:1359) already keys by file; replace
   "invalidate whole entry" with: re-parse ONE file → new chunk → overwrite on
   disk (atomic rename) → mark compose dirty.
2. Recompose: v1 = full recompose from resident chunks (debounced/coalesced by
   the existing serialized worker, watcher.rs:156-169). Incremental edge
   patching is explicitly OUT OF SCOPE for v1 (Q2 records why).
3. Deletes: drop chunk + manifest entry → recompose. (Vanished-path semantics
   already handled by the watcher's delete-trap patterns, watcher.rs:62-73.)

### Query
1. Daemon: serve from ResidentGraph snapshot; serialize the response per query.
2. CLI-without-daemon: unchanged legacy full-build fallback in v1 (a later
   version can let the CLI read chunks+compose directly).

## 3. What this buys (tied to measured pain)

| Pain (measured 2026-06-04)                    | After                          |
|-----------------------------------------------|--------------------------------|
| Restart → 3.5min + 22GB recompute             | seconds: load chunks + compose |
| 1 file change → whole-entry eviction → full   | 1 chunk re-parse + recompose   |
|   recompute on next query                     |                                |
| 22GB plateau: ASTs all resident + DOM copies  | bounded: streaming parse→write |
|   (TLDR-k8s causes a+b)                       |   + no whole-project JSON blob |
| Cache opacity (RAM-only, dies silently)       | inspectable on-disk artifacts  |

## 4. Open questions (FOR CODEX REVIEW — please rank/answer)

Q1. Compose cost: cross-file resolution (ModuleIndex + ImportResolver +
    ReExportTracer + edge creation over ~28k functions/1570 files) — is
    full-recompose-per-delta viable (target: <2s)? If not, what's the minimal
    incrementalization (e.g., per-file edge lists with lazy global index)?
    How would you measure this BEFORE committing to the design (a bench harness
    that runs steps 5-12 on pre-built FileIRs)?
Q2. Is deferring incremental compose to v2 the right call, or does Q1's answer
    force it into v1?
Q3. bincode vs rkyv for chunks: is rkyv's mmap zero-copy worth the derive churn
    across the FileIR type tree (PathBuf/String-heavy)? Is there a middle
    ground (bincode chunks + one rkyv'd composed-graph artifact for the
    nothing-changed restart fast path)?
Q4. Should the COMPOSED graph also be persisted per generation (keyed by full
    corpus digest) so a no-change restart skips compose entirely? Or is that
    premature given Q1?
Q5. Fate of the QueryKey response cache: keep as thin response cache with
    per-file input_hashes (now actually effective, since values are cheap to
    rebuild from the resident graph), or delete for graph-backed queries?
Q6. Multi-language: today warm builds ONE auto-detected language
    (daemon.rs:133-136 QueryKey carries lang). Chunks are naturally per-file/
    per-language — does the manifest need per-language sections, and does
    compose run per language or unified?
Q7. Concurrency hazards: chunk overwrite during compose; two deltas racing;
    generation GC while a CLI reader loads chunks. The semantic side solved
    reader-grace with TLDR-pdb — does the analysis store need the same
    contract or is daemon-single-writer enough?
Q8. Anything in the existing invalidation semantics (salsa.rs revision counter,
    dependents map, maybe_evict byte caps) that MUST survive into v2 and that
    this design silently drops?
Q9. Failure modes: torn chunk (crash mid-write), hash collision on xxh64 path
    keys, clock-skew on mtime fast path, schema evolution policy.
Q10. Is "salsa" worth keeping as a name, or does this design actually want the
    REAL salsa crate (incremental recomputation framework) as the compose-layer
    engine instead of hand-rolling? Honest pros/cons given the codebase.

## 5. Constraints
- No code yet — design-approval gate first (user decision).
- Must not regress: warm ack latency (29ms, TLDR-utj.7), busy-token liveness
  semantics (TLDR-3w5), watcher serialized-worker model (TLDR-qr9 dissolution).
- Must share corpus membership with TLDR-1qv/9w8's unified matcher.
- Sibling work: F1 (TLDR-9ae) removes the to_value DOM copies independently and
  should land regardless of this design's fate.

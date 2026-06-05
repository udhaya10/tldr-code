# Research annex v2 — second Firecrawl sweep (2026-06-04)
# Focus: library-first opportunities (user philosophy) + production validation of the chunk model.

## F1. GitHub stack-graphs — OUR ARCHITECTURE, VALIDATED AT GITHUB SCALE
Sources: https://github.blog/open-source/introducing-stack-graphs/ ,
         https://arxiv.org/pdf/2211.01224 , https://github.com/github/stack-graphs
- Their index-time rule is verbatim our Gate-0 invariant: "we look at each file
  COMPLETELY IN ISOLATION ... we must ignore [other files] while extracting
  incremental facts". Per-file graphs extracted at push time; merged at query time.
- They go one step FURTHER than our design: resolution is QUERY-TIME PATH-FINDING
  over the merged per-file graphs (stack-based scope resolution), not an eager
  compose. Merge = cheap graph union; the expensive part is lazy and per-query.
- DESIGN IMPACT: this is a SECOND escape hatch for Gate 1, cheaper than Glean's
  ownership sets: if eager compose busts the 2s budget, shift import/call
  resolution to query time over per-file graphs (cacheable per query). Updated
  escalation ladder in F5.
- Their storage: per-file graph blobs (SQLite-backed store in the crate) — the
  chunk model again.

## F2. epserde (Vigna) — candidate to BEAT rkyv for the composed artifact
Sources: https://github.com/vigna/epserde-rs (+ WebGraph-next-gen paper
         https://hal.science/hal-04494627/document , crates.io/crates/webgraph)
- ε-copy deserialization: at load, build a MINUSCULE struct skeleton; all bulk
  (slices/vectors) referenced directly from the mmap backend. Key quote: "the
  performance of the deserialized structure will be IDENTICAL to that of a
  standard, in-memory Rust structure, as references are resolved at
  deserialization time."
- This eliminates rkyv's principal tax: NO Archived* mirror types, no wrapper
  navigation on every access — native &[u32] slices over mmap. For a CSR
  artifact (flat offset/edge/degree arrays + interned string table) this is the
  IDEAL shape. Explicit huge-pages support; MemCase couples instance + backend;
  mmap_rs integration built in.
- Powers webgraph-rs: WebGraph "next generation (is in Rust)" — billions of
  edges, mmap'd, in production research use. CSR-at-scale prior art.
- License: LGPL-2.1 — COMPATIBLE with tldr's AGPL-3.0 (verified Cargo.toml:16).
- DESIGN IMPACT (amendment A6): the format decision is now a TWO-WAY spike, not
  settled: (i) rkyv everywhere, (ii) rkyv for chunks (deep nested FileIR) +
  epserde for the composed CSR artifact (flat arrays, native-speed access).
  Lean (ii): each format used exactly where its strength is; both are
  wiring-only per the library-first philosophy. webgraph's BVGraph compression
  itself is likely overkill (web-graph-specialized gap coding) — epserde + plain
  arrays is the sweet spot.

## F3. ty (Astral) launch data — calibrates the incrementality ceiling
Source: https://astral.sh/blog/ty
- Salsa-based, MIT, shipped. After editing a LOAD-BEARING file in PyTorch:
  diagnostics recompute in 4.7ms (80x faster than Pyright, 500x than Pyrefly).
  10-60x faster than mypy/Pyright WITHOUT caching.
- Validates: fine-grained salsa incrementality delivers millisecond-class
  per-edit updates at huge-repo scale — that is the v2 ceiling if tldr ever
  needs it. ALSO notable: ty runs IN-MEMORY (salsa persistence still maturing) —
  tldr's disk-chunk design is AHEAD of ty on the restart/CI axis, behind it on
  per-edit granularity. Different products, right trade for each.

## F4. oxc-resolver — library-first candidate for part of compose
Sources: https://github.com/oxc-project/oxc-resolver , oxc.rs/docs/guide/usage/resolver
- Rust port of webpack enhanced-resolve, maintained by oxc, used by Rspack/
  Rolldown in production. Handles tsconfig paths/aliases, package.json exports
  maps, monorepo workspace resolution.
- DESIGN IMPACT (candidate only, NOT a v1 decision): tldr's hand-rolled JS/TS
  ModuleIndex/ImportResolver (incl. the VAL-007 workspace-alias logic) could
  potentially be replaced by oxc_resolver — directly per the user's philosophy
  (delete bespoke resolution code, keep wiring). Needs a compatibility spike:
  does oxc_resolver's semantics match builder_v2's resolution contract for the
  languages tldr supports? File as follow-up; do not block v1.

## F5. UPDATED GATE-1 ESCALATION LADDER (supersedes single Glean path)
  1. PASS (compose ≤2s): recompose-always v1 (current design).
  2. MARGINAL: debounce/coalesce recompose (already in design).
  3. FAIL, first resort: stack-graphs model — compose becomes cheap graph union;
     resolution moves to query-time path-finding (cacheable). Production-proven
     at GitHub scale, much simpler than ownership tracking. (F1)
  4. FAIL, heavy artillery: Glean ownership sets / fine-grained salsa (ty-class
     machinery). Only if query-time resolution also proves too slow. (F3 + annex v1)

## F6. AMENDMENT LIST DELTA (extends in-house review's A1-A5)
  A6. Format spike: rkyv-everywhere vs rkyv-chunks + epserde-CSR. Measure load
      cost, access cost (Archived* wrappers vs native slices), derive burden.
  A7. File follow-up issue: oxc_resolver as replacement for hand-rolled JS/TS
      module resolution in compose (library-first; post-v1).

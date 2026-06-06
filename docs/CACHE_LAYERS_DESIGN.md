# Cache Layers — Target Design (Content-Addressed Segment Model)

> **Status: DESIGN — user-ratified, Codex-converged (3 review rounds), pending the
> measurement gate (`TLDR-rfz`) before any implementation begins.**
>
> This documents the **target** architecture. For how caching works *today*, see
> [CACHE_ARCHITECTURE.md](./CACHE_ARCHITECTURE.md) — note that the `QueryCache`
> described there is **retired** by this design (see "Retired machinery" below).
>
> Authoritative spec record: beads issue `TLDR-n74` (spec comments + amendments
> A1–A12), epic `TLDR-17d` (implementation children), review log `TLDR-5qm`.

## Goals

1. **Warm once, reuse everywhere** — cache segments are shared across CLI
   commands, never private to one command.
2. **Minimal invalidation** — a single-file edit invalidates only the segments
   whose input actually changed.

## The one law (the cache contract)

Every cached artifact **declares its exact input slice**; its key **is** the
blake3 hash of that slice plus `(schema, analyzer, grammar)` versions.

- Invalidation is a **key mismatch** — never an explicit flush.
- **No identity inside cached values** — qualified names, file paths, and
  absolute positions live only in the generation node table, joined at query
  time.
- Stale-wrong service is unexpressible: a content key cannot lie.

## The layer rule

**A layer exists iff it has a distinct invalidation trigger and lifetime.**
One trigger per layer; one layer per trigger. Fewer layers merges distinct
lifetimes (the historical "misfiled lifetimes" sin, ADR-9); more layers adds
boundaries with no trigger to bind to.

## The stack

```
                        ┌─────────────── CLI COMMANDS ───────────────┐
                        │     (commands never compute — they look up) │
   calls  impact  dead  hubs  deps        slice  taint  reaching-defs     search  context
   inheritance  cycles  definition        complexity  smells  health      explain  similar
        │                                      │                              │
        ▼                                      ▼                              ▼
┌───────────────────────┐   ┌─────────────────────────┐   ┌─────────────────────────┐
│  L4  DERIVED          │   │   (answered straight    │   │  Flow 2 lane            │
│  pagerank, clones,    │   │    from L2 memos)       │   │  embeddings, vectors    │
│  coupling, cycles     │   │                         │   │  (async, never blocks)  │
│  ── survives edits ── │   │                         │   │                         │
└──────────┬────────────┘   └────────────┬────────────┘   └────────────┬────────────┘
           ▼                             │                             │
┌──────────────────────────────────────┐ │                             │
│  L3  RESIDENT GRAPH (the daemon)     │ │     Flow 2's enriched search│
│  CSR snapshot + indexes + BM25       │◄────────── also reads L3 ─────┘
│  ── rebuilds in ms FROM warm L2 ──   │ │
└──────────────────┬───────────────────┘ │
                   ▼                     ▼
┌─────────────────────────────────────────────────────────┐
│  L2  PER-FUNCTION FACTS  (the shared treasure)          │
│  CFG/DFG bundles · metric rows · fingerprints · chunks  │
│  keyed by content hash — edit fn A, fns B..Z stay WARM  │
└──────────────────────────┬──────────────────────────────┘
                           ▼
┌─────────────────────────────────────────────────────────┐
│  L1  PARSE TREES  — parse ONCE, both flows share        │
└──────────────────────────┬──────────────────────────────┘
                           ▼
┌─────────────────────────────────────────────────────────┐
│  L0  SOURCE FILES (ground truth, not a cache)           │
└─────────────────────────────────────────────────────────┘
        (L5 = on-disk CAS copy of L2 → warm restarts)
```

### Layer reference

| Layer | Contents | Key / lifetime | Invalidation trigger |
|---|---|---|---|
| **L0** | source files | — (ground truth) | user edits |
| **L1** | parse trees | `(file content hash, grammar ver)` | this file's bytes |
| **L2a** | per-function: CFG/DFG/SSA bundle (position-relative), metric row, token fingerprint | `(body hash, file env hash, lang, analyzer ver)` | this function's body, or this file's env |
| **L2b** | per-file compose inputs: signatures, call-sites, imports, receiver/local-type facts | per-fact sub-hashes of file content | this file's compose-relevant facts |
| **L2-async** | embeddings | `(body hash, signature hash, model ver)` — **body-local, A12** | this function's own body/signature; refresh queued asynchronously |
| **L3** | CSR snapshot + name/file/cursor indexes + BM25 postings | compose key (multiset of L2b hashes); generationed | any compose-input change → O(V+E) rebuild **from warm L2** |
| **L4** | pagerank, betweenness, SCC/cycles/layers, coupling, inheritance closure, clones | slice hash (reuse ticket), looked up **through a pinned generation** | its own slice changing |
| **L5** | on-disk CAS backing L2 | content-addressed | GC sweep only; survives restart |
| **side lane** | external truth: churn, diagnostics, coverage, doctor, fix | git HEAD / tool versions; local-only | the outside world (never daemon-resident) |

### Key design points (the amendments that shaped them)

- **Env-hash scope (A9):** `env hash` = direct imports + enclosing-class chain
  **only**, never transitive. Transitive effects flow through compose/L3.
- **Position-relative storage (A1/F2):** bundles carry function-start-relative
  positions; absolute lines + identity live only in the generation node table.
  An edit *above* a function never invalidates it.
- **Epoch safety (F4):** FuncId assignment is deterministic from identity
  ordering (file, qualified name, ordinal) — never positions. L4 artifacts are
  always looked up *through* a pinned generation: generation = consistency
  token, slice hash = reuse ticket. Mixing an old artifact with a new node
  table is unexpressible.
- **Aggregates are folds, not tickets (A3):** whole-project health/debt are
  O(V) monoid folds over warm L2a rows per generation — any edit changes their
  slice, so reuse tickets would never hit.
- **Embeddings are body-local (A12):** resolved-callee context is **never**
  baked into embedding text. Structural context joins at query time from the
  pinned generation and fuses (RRF) with dense + BM25 signals. A callee rename
  re-embeds zero functions. Any future baked-in enrichment must beat
  query-time fusion in the eval harness first.
- **Async embedding consistency (A8):** semantic hits are join-verified against
  the pinned generation — dead keys dropped, positions rebased; responses carry
  an embedding-staleness field. Stale embeddings may mis-rank, never surface
  deleted symbols.
- **Compose is source-blind (F5/A11):** compose consumes only the L2b closed
  fact set. Closability = capturing exactly today's resolver inputs (verified
  per-file-only in `resolution.rs` / `type_resolver.rs`); the frozen-corpus
  byte-hash gates are the mechanical closure proof.

## The invalidation cascade (one file edit)

```
you edit foo() in api.rs (50 functions in the file)
      │
L1: re-parse api.rs only ............................ ~4ms
    (ALL hashes computed in this single walk — no source re-reads)
L2: recompute foo()'s facts only — other 49 warm ..... µs–ms
    foo()'s embedding queued on the async lane
L3: call edges changed? rebuild graph FROM warm L2 ... ~ms (gated target)
    call edges same?   no rebuild at all
L4: pagerank/clones reused unless their slice changed . usually free
      │
      ▼
next `tldr calls` / `impact` / `smells` query: milliseconds
```

- Comment edit **above** a function: zero invalidation.
- Comment edit **inside** a body: that one function's L2a facts (cheap) + an
  async re-embed; no L3 rebuild (compose-input hashes unchanged).

**vs. today:** any file change blunt-clears the per-language cache bucket →
next query repays a full project rebuild (~3s); `tldr search` builds BM25 *and*
the entire call graph from scratch on **every query** (`enriched.rs:925/986/1593`).

## Who benefits (command → layer map)

| Cluster | Served by | Win |
|---|---|---|
| Graph queries (`calls`, `impact`, `dead`, `hubs`, `deps`, `importers`, `inheritance`, `definition`, …) | L3 lookups, O(answer) | seconds → milliseconds; 15+ commands share one snapshot |
| Per-function analyses (`reaching-defs`, `taint`, `slice`, `chop`, `available`, `dead-stores`) | L2a bundle memo | agent sequences on one function hit one warm memo |
| Metric family (`complexity`, `cognitive`, `halstead`, `loc`, `smells`, `debt`, `health`, `hotspots`) | L2a metric rows + O(V) folds | facts computed once; thresholds are instant query-time judgments |
| Expensive derivations (`pagerank`, `betweenness`, `clones`, `coupling`) | L4 reuse tickets | survive every edit that doesn't change their slice |
| Agent flagships (`search`, `context`, `explain`) | L3 + L2 + Flow-2 lane | per-query BM25 + graph rebuilds die |
| Truth-honest (`churn`, `diagnostics`, `doctor`, `coverage`, `fix`) | side lane | stay local; no staleness manufactured |

## Two flows, one foundation

Flow 1 (structural) and Flow 2 (semantic) share L0–L2 and diverge only at L3:

- **Shared (helps both):** file enumeration, tree-sitter parse (today each flow
  parses every file *independently* — `chunker.rs:222` vs callgraph extraction),
  function identity + content hashing, the L2 fact registry (a "chunk" *is* a
  function), watcher + delta pipeline.
- **Flow 1 exclusive:** CSR + graph walks.
- **Flow 2 exclusive:** ONNX inference + vector index.
- **Direction of benefit:** Flow 1's substrate accelerates Flow 2's queries
  (O(deg) enrichment instead of per-query graph builds). Flow 2 contributed the
  *patterns* (content-hash deltas, resident stores, CAS persistence) that
  Flow 1 industrializes. Per-cache-hit value is highest in Flow 2: an avoided
  re-embed saves ~100ms+ of ONNX; an avoided CFG rebuild saves microseconds.

## Retired machinery (explicit fates, not omissions)

| Today | Fate |
|---|---|
| `QueryCache` (`daemon/salsa.rs`) — memoizes materialized answers | **Retired** — the answer-blob class ADR-9 kills; consumers route to L3/L4 |
| Per-language `OnceCell` graph/BM25 buckets + blunt `invalidate_caches()` | **Retired** — replaced by L3 generationed snapshot |
| `Bm25Index::from_project` + `build_project_call_graph` inline in `enriched_search` | **Retired** — search routes to resident L3 |
| Cache-clear coherence bug class (`TLDR-jsi`) | **Unexpressible** — clear = drop CAS; content keys can't serve stale-wrong data |

## Rejected alternatives (recorded with revisit triggers)

1. **Reified cache-key dependency graph** (Salsa/Bazel-style stored edges +
   push invalidation): our derivation DAG is static and shallow (~10 edge types
   known at design time); A12 deleted the only volatile many-to-many edge
   (embedding ← resolved callees); stored edges are mutable state with their
   own staleness — the bug class content-addressing eliminates.
   **Revisit iff** the derivation DAG becomes dynamic (analyses reading
   arbitrary other functions' facts at runtime).
2. **Generation as the L4 cache key** ("first query per generation pays"):
   replaced by slice-hash reuse tickets — body-only edits keep pagerank warm
   across rebuilds. Generation survives as the consistency/pinning token.
3. **Enrichment-slice embedding keys** (body + signature + resolved callees):
   honest but invalidation-heavy; superseded by A12 (body-local key +
   query-time fusion — which is also *fresher* than baked-in context).
4. **Comment-stripped/normalized body hashing**: parked as a future measured
   lever (complicates position-correct rendering; per-function recompute is
   likely cheap enough).
5. **External databases** (Redis/SQLite/Parquet) and **incremental graph
   surgery**: rejected in ADR-9 with numbers (see `TLDR-t0s`).

## Gates before implementation

All implementation children of epic `TLDR-17d` are blocked by the measurement
gate `TLDR-rfz` on the frozen rust corpus (745 files / 20,320 funcs / 26,312
edges):

1. minimal CSR build + query latencies at 1×/10×/100× (budget: single-digit ms
   rebuild at 26k edges, O(V+E) scaling),
2. resident BM25 build/query/single-file-update → rebuild-vs-delta decided on
   numbers,
3. edit-path hash fan-out within the ~4ms re-parse envelope,
4. env-hash blast radius (fraction of real edits touching imports; functions
   churned per such edit).

Verdict semantics: **CONFIRMED** (budgets hold → implement) / **REVISED**
(amend numbers first) / **INVALIDATED** (back to design). Every implementation
step gets Codex review logged on `TLDR-5qm`; output-identical changes ride the
frozen-corpus byte-hash gates.

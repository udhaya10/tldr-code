# Daemon & Semantic Search Architecture

> Scope: this document describes the daemon + semantic work (originally
> developed on the `tldr-cli-daemon-support` branch; now merged). It explains
> how tldr's semantic search and
> background daemon were redesigned — from a cold, JSON-cached, rebuild-the-world
> model into a **persistent vector store served by a long-lived daemon that
> watches the filesystem and incrementally re-indexes only what changed**.
>
> Companion docs: `INCREMENTAL_REINDEX_DESIGN.md` (the t8f delta design) and
> `CACHE_ARCHITECTURE.md` (the daemon query cache).

---

## 1. What changed, in one paragraph

The old semantic path embedded raw code, persisted vectors in a JSON cache, and
rebuilt the entire in-memory index on every file change (~14 s on `tldr-core/src`
after a single save). This branch replaces that with a **usearch-backed vector
store** (`key u64 → f32 vector`) that is persisted on disk, loaded once into a
**resident daemon**, kept fresh by an **in-process filesystem watcher**, and
updated with **surgical per-file deltas** instead of full rebuilds. The legacy
in-memory `SemanticIndex` / JSON cache and the `embedding_client.rs` HTTP client
were removed; there is now a single, no-silent-fallback search path.

Roughly **9,000 lines across 69 files**, built issue-by-issue under beads
(`TLDR-l5d → m01 → zxb → atc → t8f → ac0.5/0.6 → ac0.2 → 82b`).

---

## 2. Layered view

```text
          CLI (tldr search / semantic / similar / embed)
                          │
                          ▼
        ┌────────── tldr-cli/commands/daemon (resident, per project) ─────────┐
        │                                                                      │
        │   filesystem watcher ──► dirty-file channel ──► serialized worker    │
        │   (notify-debouncer)                              │                  │
        │                                                   ▼                  │
        │            IndexManager (RwLock<Option<(EmbeddingModel, VectorStore)>>) │
        │                                   • query  (shared read lock)        │
        │                                   • warm / invalidate (write lock)   │
        │                                   • apply_delta  (write lock, t8f)   │
        └──────────────────────────────────────┬───────────────────────────────┘
                                                │
                                                ▼
                          tldr-core::semantic::VectorStore (usearch)
                          • add / remove / search / apply_file_delta
                          • persisted store dir (manifest + sidecar + index)
```

> **Phase 1 update (TLDR-7xz, two-modes contract):** `tldr semantic` no longer
> cold-serves from the CLI. The daemon is the **only** serve path — a query
> either gets full-quality warm results or an honest error saying exactly what
> to do (see §11). `search_with_store` remains the shared build/load engine
> (used by `warm` inside the daemon and by the legacy `embed` builder), but the
> CLI query path requires a warm daemon.

---

## 3. The vector store (`tldr-core/src/semantic/vector_store.rs`)

The heart of the branch. A `VectorStore` is a `usearch` index mapping a stable
`u64` key to an `f32` vector, plus a metadata sidecar that holds everything
usearch does not (file path, function name, line range, content hash).

### 3.1 Public surface (selected)

| Method | Purpose |
| --- | --- |
| `new(dimensions, capacity)` | Create an empty writable index. |
| `add(key, vector, meta)` | Insert one chunk's vector + `ChunkMeta`. |
| `remove(key)` | Delete a key's vector (the linchpin for incremental). |
| `search(query, k)` | Exact KNN → `Vec<SearchHit>`. |
| `apply_file_delta(...)` | Re-embed only a file's changed/added functions. |
| `apply_file_delete(rel)` | Remove all vectors belonging to a deleted file. |
| `save(dir, id)` / `load(dir, expect)` | Crash-safe persistence (see §3.3). |
| `corpus_digest()` | Build-time digest used by the freshness gate. |
| `build(...)` / `for_build(...)` | Build a store from a chunk set. |

Supporting helpers: `chunk_identity`, `identity_key`, `key_chunks`,
`root_relative`, `stat_signal`.

### 3.2 Keys and identity

Keys are derived from a chunk's **identity** (`chunk_identity` → `identity_key`),
not its array position, so a key is stable across rebuilds and a removed function
can be located and deleted precisely. `FileRecord` tracks which keys belong to a
file so a delete or delta can target just that file's vectors.

### 3.3 Crash-safe persistence

The store directory is content-addressed per project:

```
<cache_dir>/tldr/stores/<md5(canonical_project_root)[..16]>/
  index.<gen>.usearch      # usearch index for generation <gen>
  manifest / sidecar        # ChunkMeta, FileRecords, corpus digest, model
  CURRENT                   # atomic pointer → the committed generation
```

(`store_dir_for` in `semantic/types.rs` computes the path.)

Saves are **generation-numbered** and committed by writing `CURRENT` via
temp-file + rename — the single atomic commit point. `CURRENT` carries a magic
(`"TLDR"` / `0x544C4452`) so a torn or foreign pointer is detectable. `load`
verifies the manifest generation matches the filename, and **recovers from an
older generation** if the newest committed one is unusable (logging a warning;
the next save repairs `CURRENT`). Old generations are garbage-collected, keeping
the last `KEEP_GENS`.

`load()` (copy into RAM, writable) is used rather than `view()` (mmap,
read-only) because the daemon needs `add`/`remove` for incremental deltas. This
is documented as a **scale-bounded** choice — revisit `view()` past ~20 resident
daemons or a 500 MB+ index (see `INCREMENTAL_REINDEX_DESIGN.md §3`).

---

## 4. The single search path (`store_search.rs`)

This module is the **only** production search path. Per `TLDR-lx7`, there is **no
silent degradation** to the legacy in-memory index or JSON cache: if the store
cannot load, build, or search, the error propagates with a detailed message.

Two entry points:

- **`search_with_store(...)`** — one-shot load-or-build + query. Loads or builds
  the store, runs the freshness gate, embeds the query, searches. Post-Phase-1
  this is **not** reachable from `tldr semantic` (no cold CLI serve); it backs
  the daemon's own warm path and the legacy `embed` builder.
- **`query_store(...)` / `query_store_with_vector(...)`** — daemon reuse. Takes
  an already-resident `VectorStore` and embeds + searches only; no load/build/
  freshness cost per query. `query_store_with_vector` is the production daemon
  path (resident embedder, TLDR-ac0.5).

Helpers: `load_or_build_store`, `empty_search_report`.

### Freshness gate (`TLDR-kkt`)

`VectorStore::load` only verifies persisted integrity, not whether the **source**
changed since the store was built. So the cold path adds a coarse
**detect-drift → full-rebuild** gate: after a clean load it compares the store's
build-time `corpus_digest()` against a freshly computed digest over the project
root. The digest is a **stat-only walk** (no parse) over the pre-parse candidate
set, so any file added/removed or any mtime/size change forces a rebuild rather
than serving stale rankings. The daemon manages freshness separately (via the
watcher + deltas), so it does not pay this per query.

---

## 5. The daemon

The daemon (`tldr-cli/src/commands/daemon/`) is a long-lived, per-project server
that keeps the vector store and the query cache resident between commands.

### 5.1 `IndexManager` (`index_manager.rs`, new)

A thin concurrency wrapper around the resident store:

```rust
parking_lot::RwLock<Option<(EmbeddingModel, VectorStore)>>
```

- `query` — shared **read lock** fast path; promotes to an exclusive write lock
  only on a cold miss (to build).
- `warm` — write-lock build.
- `invalidate` — write-lock clear.
- `apply_delta` — write-lock incremental per-file re-index (t8f).
- `is_warm` / `store_len` — observability.

The read/write split (`TLDR-4bf`) lets concurrent queries proceed without
serializing on a mutex, while writes (warm / delta / invalidate) take the
exclusive lock. The daemon and watcher never touch a raw lock — they go through
`IndexManager`.

`apply_delta` returns a `DeltaOutcome` (`Filtered`, cold/other-model no-op,
applied, etc.) so callers can distinguish "outside the corpus" from "store cold"
from "delta applied."

### 5.2 Incremental re-index (`TLDR-t8f`)

`daemon.rs::handle_notify` used to do two coarse things on any change: nuke the
whole index (`*semantic_index = None`) and treat dirty-file accounting as a
no-op. Both are replaced.

`process_dirty_file` now performs a **surgical per-file delta**: re-embed only the
functions whose content changed, **remove** vectors for deleted functions, and
leave everything else untouched. A **source-files-only filter** (`TLDR-ac0.6`,
the §6 "first gate") drops paths outside the source corpus using the same rules
as the build walker, so editor scratch files and `.tldr/` writes never trigger a
delta. Full design: `INCREMENTAL_REINDEX_DESIGN.md`.

### 5.3 Resident embedder reuse (`TLDR-ac0.5`)

The query path reuses one resident embedder instead of constructing a new one
per query. A guard skips building the embedder when the query is blank.

---

## 6. The in-daemon filesystem watcher (`watcher.rs`, new — `TLDR-ac0.2`)

File-change detection now lives **inside** the Rust daemon, co-located with the
in-RAM index it mutates, replacing the old cross-process C++ fsnotifier → IPC
`Notify` hop.

```text
notify-debouncer-full (OS watcher + debounce, own thread)
     │  watch_decision() filter (cheap excludes + corpus membership)
     ▼
bounded mpsc<PathBuf>  ── drop-on-full (never block the watch thread)
     ▼
single serialized worker task
     │  coalesce: drain everything queued into a dedup set
     ▼
TLDRDaemon::process_dirty_file()  (salsa invalidate + in-place delta)
```

Key properties:

- **No shared lock** between the watch thread and the worker — invalidation flows
  over the channel, dissolving the async-thread-mutex hazard (`TLDR-qr9`) by
  construction.
- `watch_decision()` filters events cheaply: excludes in-tree `.tldr/` writes,
  drops access-only events and non-corpus files, but **passes through deletes of
  vanished source files** (you can't stat a file that's gone, so membership is
  judged by path rules). Symlinked roots resolve to canonical corpus membership.
- Honest framing (from the module doc): notify is **not faster** than fsnotifier
  — same OS primitives. The win is **consolidation into one process** and turning
  the t8f delta into an in-process call rather than an IPC contract.

`spawn_watcher(daemon)` returns an `Option<WatcherGuard>` tying the watcher's
lifetime to the daemon. It returns `None` (and logs a warning) when the store
dir is inside the project root or the OS watcher can't be created — the daemon
keeps serving IPC `Notify` in that case. Coverage includes end-to-end
"new file appears → routed → indexed" tests.

---

## 7. Single-instance hardening (`TLDR-82b`)

Several fixes ensure exactly one daemon owns a project:

- **Fail closed on an unresolvable project root** — never silently serve the
  wrong tree.
- **Owner liveness judged by PID, not socket reply** — a stale socket that still
  answers does not count as a live owner; the registry checks the recorded PID.
- Hardening across `pid.rs`, `daemon_registry.rs`, `start.rs`, and `ipc.rs`,
  with tests for dead-PID cleanup and cross-CWD status.

---

## 8. Project configuration (`tldr-core/src/config.rs`, new)

`TldrConfig` is loaded in two layers: a global `~/.tldr/config.json` (in the
user's HOME), then a project `<project_root>/.tldr/config.json` that overrides
it:

- `version` — config schema version (defaults to 1).
- `embedding` — provider/model/endpoint/dimensions.
- `semantic` — enabled flag + language filter.

This makes embedding model and semantic behavior configurable per project rather
than hard-coded.

---

## 9. File map (what to read)

| Area | Path |
| --- | --- |
| Vector store | `tldr-core/src/semantic/vector_store.rs` (+2043) |
| Single search path | `tldr-core/src/semantic/store_search.rs` (+636) |
| Store dir / freshness types | `tldr-core/src/semantic/types.rs` |
| Config | `tldr-core/src/config.rs` (+342) |
| Daemon concurrency wrapper | `tldr-cli/src/commands/daemon/index_manager.rs` (+820) |
| Filesystem watcher | `tldr-cli/src/commands/daemon/watcher.rs` (+391) |
| Daemon dispatch + delta | `tldr-cli/src/commands/daemon/daemon.rs` (`handle_notify`, `process_dirty_file`) |
| Single-instance | `daemon/pid.rs`, `daemon/start.rs`, `daemon/daemon_registry.rs`, `daemon/ipc.rs` |
| Removed legacy client | `tldr-core/src/search/embedding_client.rs` (−294, deleted) |
| Benchmarks / eval | `tldr-core/examples/embed_bench.rs`, `tldr-core/examples/semantic_eval.rs` |

### Design docs
- `docs/INCREMENTAL_REINDEX_DESIGN.md` — the t8f per-file delta design.
- `docs/CACHE_ARCHITECTURE.md` — the daemon query cache (separate from the store).
- `docs/CODEBASE_OVERVIEW.md` — whole-repo overview.

---

## 10. Known boundaries / non-goals

- **No cross-file semantic effects** in the delta path: re-embed is by file
  identity, not semantic dependency. Enrichment is off by default, so this is
  acceptable for now.
- **No sub-function-granularity deltas.**
- **One daemon per project** — no multi-writer coordination across separate
  daemons for the same tree.
- **`load()` over `view()`** is a deliberate scale-bounded choice; revisit for
  very large indexes or many concurrent resident daemons.

---

## 11. Operational lifecycle — serve matrix, warm flow, and what survives a restart (Phase 1, TLDR-7xz)

This section documents the **runtime contract** as shipped by Phase 1
("one warm path — works beautifully or says why") and the persistence
behavior verified live on 2026-06-04.

### 11.1 The two-modes serve matrix

`tldr semantic` never silently degrades. Every state has exactly one honest
answer:

| State | `tldr semantic "query"` answers |
| --- | --- |
| No daemon for this project | `daemon not started — run tldr daemon start` |
| Daemon up, no index | `index not built — run tldr warm` |
| Daemon up, index building | `index build in progress — retry when warm completes` |
| Daemon up, index warm | full-quality ranked results (~3 ms serve) |

Parked surfaces (capability exists but the warm path for it does not yet)
answer with the standardized message
`not available in this version, <reason>`:

| Surface | Status | Un-parked by |
| --- | --- | --- |
| `tldr similar` | parked (seeded similarity needs a warm daemon API) | TLDR-utj.4 |
| `tldr semantic --hybrid` | parked (BM25 fusion moving into the daemon) | TLDR-utj.3 |
| MCP `tldr_semantic` | parked (MCP has no daemon client) | TLDR-utj.5 |

### 11.2 The warm flow (the blessed sequence)

```bash
tldr daemon start     # 1. start the per-project daemon (instant, starts COLD)
tldr warm             # 2. trigger the index build INSIDE the daemon
tldr daemon status    # 3. poll while it builds
tldr semantic "..."   # 4. honest "build in progress" → ranked results when warm
```

Division of labor:

| Actor | Role |
| --- | --- |
| `tldr warm` (CLI) | thin client: sends the build request, exits |
| daemon | owns the long build, in the background; serves only when warm |
| `tldr embed` (CLI) | legacy **foreground** builder; shares the chunk cache but exercises none of the daemon flow (notice task: TLDR-e0b) |

`tldr warm` acks **immediately** with `warm build started — poll 'tldr daemon
status' for progress` (a duplicate `warm` answers `already in progress`); the
build runs as a detached task inside the daemon. `tldr daemon status` shows
the live build (`busy: warm-build` with age, `Index: building`) and flips to
`warm (N vectors)` at completion (TLDR-utj.7 / TLDR-qzc).

### 11.2b Presence-based liveness (epic TLDR-cxa, shipped 2026-06-04)

The daemon self-terminates only when the **project is dormant**, not merely
its socket. Two operational pitfalls this replaced — an unattended build
longer than `idle_timeout_secs` self-terminated mid-build (TLDR-3w5), and the
warm client printed a misleading 30 s timeout error (TLDR-utj.7) — are fixed;
the old keep-alive polling workaround is no longer needed.

`idle_timeout_secs` (default 1800 s) now measures **project-presence idle**
(semantics migration: TLDR-d26 — same key, broadened meaning). The countdown
resets on ANY of:

| Presence source | Mechanism |
| --- | --- |
| `socket` | any accepted client connection (the old, only signal) |
| `cli_poke` | **every** `tldr` CLI invocation and `tldr_mcp` tool call in the project — registry-gated unix-datagram poke at `<socket>.poke` (TLDR-nke / TLDR-axz); opt out with `TLDR_NO_POKE=1` |
| `watcher` | any project file **write** (pre-corpus-filter — a `cargo build` writing `target/` is proof of life), excluding the daemon's own store writes and read events (immortality guards) |
| `internal` | touched when internal work completes, so the countdown restarts at build **completion**, not start |

While internal work (warm build, per-file delta) is in flight, idle shutdown
is suspended outright ("never abandon your own job") — `daemon status` shows
the busy token with its age, so a hung build is visible rather than silently
immortal. `daemon status` also reports per-source presence ages and the
computed `idle stop` deadline. Accepted trade-off: a machine with constant
file activity keeps its daemon alive indefinitely — warm availability over
memory thrift (escape hatch: TLDR-yll).

### 11.3 Persistence: the three layers

| Layer | Where | Survives daemon restart? | Survives reboot? | Invalidated by |
| --- | --- | --- | --- | --- |
| **Vector store** (usearch index + sidecar + manifest) | `<cache_dir>/tldr/stores/<hash>/` | **yes** (reloaded, §11.4) | yes | source-tree change (corpus digest) or model/config change |
| **Per-chunk embedding cache** | `<cache_dir>/tldr/embeddings/` | **yes** | yes | per chunk, on content-hash change — never wholesale |
| **In-memory daemon state** (salsa, call-graph caches, resident embedder, resident store copy) | daemon RAM | no — recomputed lazily | no | every restart, by design |

The per-chunk cache is the expensive part and is the reason interrupted builds
are not catastrophic: every chunk embedded up to the last completed batch is
reused by the next `warm`, which only embeds what is missing. (The ONNX model
itself lives in `<cache_dir>/tldr/fastembed/`, downloaded once.)

### 11.4 Restart over a committed index: the reload path

`tldr warm` on a fresh daemon → `IndexManager::warm()` → `load_or_build_store`
(`store_search.rs`):

1. **Cold start is deliberate.** A new daemon holds no store and no embedder;
   queries before `warm` get the honest cold message (pinned by test —
   cold queries must not trigger a load).
2. **Freshness gate** — compute the expected manifest ID (model + dims + root)
   and a stat-only **corpus digest** over the source tree; both must match the
   persisted store.
3. **Verified load** (`vector_store.rs::load_generation`) — all-or-nothing:
   read `CURRENT` → manifest generation; verify manifest format/config/
   generation; checksum-verify the sidecar and the usearch index file; verify
   keys digest, index size == sidecar count, and that the index contains every
   sidecar key. Any mismatch → `Corrupt` → fall back to an older generation or
   rebuild. A half-written or drifted index is never served.
4. **Resident from then on** — the store lives in the `IndexManager` RwLock;
   queries are pure in-memory (`exact_search`). Note: this is usearch
   **`load()` (full copy into RAM)**, not `view()`/mmap — see §3.3 and §10.
5. **ONNX embedder loads lazily, once** (TLDR-ac0.5) — a few seconds on the
   first query, then resident.

Cost of restart-warm over a committed, fresh index: file reads + checksums +
lazy model load — **seconds, not the build time**.

| | Restart after build **committed** | Restart **mid-build** |
| --- | --- | --- |
| Vector store | reloaded from disk, warm in seconds | last *committed* generation only; in-flight assembly lost |
| Embedding cache | intact | intact up to last completed batch — next `warm` skips those chunks |
| In-memory state | recomputed lazily | recomputed lazily |
| If source changed while down | corpus digest mismatch → logged rebuild (cheap via chunk cache) | same |

### 11.5 While the daemon runs

Disk is not re-read per query. File edits flow through the in-process watcher
(§6) → per-file delta (§5.2): re-chunk one file, re-embed only changed chunks,
apply under a brief write lock. Saves are generation-numbered and committed via
the atomic `CURRENT` rename (§3.3), so a crash at any point leaves the previous
committed generation loadable.

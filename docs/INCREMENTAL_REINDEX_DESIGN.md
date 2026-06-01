# Incremental Re-indexing — Design (TLDR-t8f)

Status: **Design** (not yet implemented). Owner: semantic search.
Depends on: **TLDR-l5d** (usearch vector store) → **TLDR-atc** (daemon warm path, ✅ done).
Parent epic: **TLDR-ac0** (replace JSON embedding cache with usearch).

This document plans incremental re-indexing *before* code, because the current
file-change handling is a coarse placeholder that needs replacing, not patching.

---

## 1. Problem — today's `Notify` is a stub

`tldr-cli/src/commands/daemon/daemon.rs :: handle_notify` does two coarse things
on any file change:

1. **Nukes the whole semantic index** — `*self.semantic_index.lock() = None`.
   The next query then does a **full rebuild** (re-chunk the entire tree, reload
   the cache, rebuild the in-memory index ≈ 14 s on tldr-core/src). One keystroke
   save ⇒ a 14 s penalty on the next search.
2. **Dirty-file accounting is a literal no-op** — when `dirty_files` reaches
   `auto_reindex_threshold` the handler just clears the set, with the comment
   *"In full implementation, would spawn background reindex task / For now, just
   clear the dirty set."* No reindex happens.

There is also **no debounce**: editors emit many save/Notify events in a burst,
each invalidating the index. The net effect is "the daemon feels like it isn't
caching during active editing."

**Goal:** turn a full rebuild into a **surgical per-file delta** — re-embed only
the functions whose content actually changed, remove vectors for deleted
functions, and leave everything else untouched.

---

## 2. Goals / non-goals

**Goals**
- Editing one file re-embeds **only that file's changed functions**, not the corpus.
- Deleted functions' vectors are **removed** from the index.
- Query results reflect a change **within one debounce + save cycle**.
- A **full rebuild remains the fallback** when the delta path detects drift.
- Survives daemon restart without going stale or silently wrong.

**Non-goals (for this iteration)**
- Cross-file semantic effects (a change in A that alters B's enriched text) — we
  re-embed by file identity, not by semantic dependency. Enrichment is off by
  default (`TLDR_ENRICH`), so this is acceptable.
- Sub-function granularity deltas.
- Multi-writer coordination across *separate* daemons (one daemon per project).

---

## 3. Prerequisite — the usearch writable RAM index (TLDR-l5d)

Incremental is **gated** on the usearch migration providing a **writable,
in-RAM** index. Verified against `usearch/rust/lib.rs`:

- `add_f32(key: u64, &[f32])` / `add_i8(...)` — insert a vector under a key.
- `remove(key: u64) -> usize` — delete a key's vector. **This is the linchpin.**
- `exact_search_f32(query, count)` — exact KNN (our chosen search; not HNSW).
- `save(path)` / `load(path)` (`restore`, copies into RAM) vs `view(path)`
  (`restore_view`, mmap). We use **`load()`**: it keeps the index **writable**
  (required for `add`/`remove`), and at our scale (~58 MB f32) the copy is cheap.
  `view()` is documented read-only (usearch issue #97); that is **not enforced by
  the Rust types**, so we treat it as read-only by policy rather than relying on it.
- **`load()` is a scale-bounded decision, not a blanket policy** (Codex review):
  it copies the whole index into RAM, so with ~20 resident project daemons or a
  500 MB+ monorepo the duplication becomes material. Revisit `view()` for the
  read/query path (with a separate writable RAM index for deltas) past that ceiling.
- **f32 first; i8 is a real option, not irrelevant.** usearch has `ScalarKind::I8`,
  `add_i8`, and `exact_search_i8` — but i8 needs a **separately built quantized
  index** and the **query quantized too** (`exact_search_f32` gets no i8 benefit).
  i8 is ~4× smaller, near-lossless for cosine on unit-normalized vectors, and
  matters once dims/models compound (ArcticL = 1024 dims). Deferred to TLDR-ccg;
  measure recall on the n=52 eval before flipping.

The vector store is `key (u64) → vector`. Everything else (file path, function
name, line range, content hash) lives in the **metadata sidecar + manifest** (§4),
which ship in l5d (P0).

---

## 4. Data model

> **Scope note (Codex review, 2026-06-01):** the metadata sidecar AND the manifest
> below are part of **l5d (P0)**, NOT t8f. They are a *correctness* requirement, not
> an incremental optimization: a persisted usearch index is vectors-only, so after a
> **daemon restart** search results (path/function/line range/snippet) cannot be
> reconstructed without the sidecar — and re-chunking the whole tree on every restart
> is exactly the cost we are removing. l5d ships: usearch vector store **+ sidecar +
> manifest**. t8f only *adds* the per-file delta logic on top.

### 4.0 Store manifest (version + identity)

A small `manifest` (one per store, e.g. `index.manifest`) guards against pairing
valid vectors with the wrong metadata after upgrades/model changes/crashes:

```
Manifest {
    format_version: u32,     // bump on any on-disk layout change
    generation:     u64,     // incremented every successful save; index+sidecar
                             //   MUST share the same generation (atomicity, §7)
    embedding_model: String, // e.g. "ArcticL" — a model change invalidates vectors
    dimensions:      u32,    // must equal the loaded usearch index dims
    metric:          String, // "cos"
    scalar_kind:     String, // "f32" (i8 later) — quantization changes vectors
    embed_schema:    String, // raw-v1 / enriched-v1 (same tag as the cache key)
    root:            String, // canonical project root the keys are relative to
    chunk_count:     u64,
    checksum:        u64,    // of the sidecar payload, cross-checked on load
}
```

On `load()`: if `format_version`, `embedding_model`, `dimensions`, `scalar_kind`,
`embed_schema`, or `root` mismatch the running config — or the index's reported
dims/count disagree with the manifest, or `checksum` fails, or the index and
sidecar `generation` differ — **reject and full-rebuild** (logged, never silent).

### 4.1 Stable chunk key (`u64`)

Each chunk needs an identity that is **stable across edits to its body** so a
body change updates the *same* key rather than churning keys.

```
identity = "{file_rel_path}::{class_name}::{function_name}::{ordinal}"
key       = u64 hash(identity)            // 64-bit; collision-safe at <1e5..1e6 keys
```

- `file_rel_path` is **root-relative** (the same normalization the cache key fix
  introduced — TLDR-atc), so identity is CWD/absolute-path independent.
- `ordinal` disambiguates **duplicate names in one file** (overloads, two
  same-named methods) by their order of appearance. Without it, two functions
  named `new` in one file would collide.
- File-level chunks (`function_name = None`): identity = `"{file_rel_path}#file"`.

A 64-bit hash makes accidental collisions negligible below ~10^6 keys (birthday
bound ≈ 2^32). The sidecar stores `identity` per key so a collision (or a hash
change) is *detectable* and falls back to rebuild.

**Ordinal instability (Codex review).** `ordinal` is positional, so inserting a
*new* same-named function *above* an existing one shifts the existing one's
ordinal → its key churns even though its body did not change. That key is then
classified `remove(old) + embed(new)`. The damage is bounded: the re-`embed` is a
**content-hash cache hit** (same body ⇒ no ONNX call), so the real cost is a couple
of index `remove`/`add` ops, not re-embedding. Acceptable for now; if churn proves
noticeable, replace the positional ordinal with a content-anchored disambiguator
(e.g. a short prefix hash of the body) so identity is insertion-order-independent.

**Root-relative path is load-bearing — do not let it fail silently.** The key uses
`file_rel_path = strip_prefix(build_root)`. Today `CacheKey::from_chunk` falls back
to the **raw** path when `strip_prefix` fails (cache.rs) — which silently re-creates
the original absolute-vs-relative key-divergence bug for symlinked roots,
differently-normalized paths, or chunks outside the root. **Requirement:** canonicalize
the root once and derive `file_rel_path` deterministically; on a `strip_prefix` miss,
**log a warning and use a single canonical fallback** (e.g. the canonicalized absolute
path), never the raw as-given path. (Hardening of the existing key path — tracked
separately; it predates usearch but the same rule applies to the usearch keys.)

### 4.2 Metadata sidecar (`key → ChunkMeta`)

Stored next to the usearch index (e.g. `index.usearch` + `index.meta`):

```
ChunkMeta {
    identity:      String,   // for collision detection + rebuild
    file_rel_path: String,
    function_name: Option<String>,
    class_name:    Option<String>,
    line_start:    u32,
    line_end:      u32,
    content_hash:  String,   // detects body changes
    indexed_at:    u64,      // file mtime (or content-hash time) at index — drives
                             //   startup reconcile (§7); was referenced there but
                             //   missing from the schema (Codex review).
}
```

Serialized compactly (bincode preferred; JSON behind a `--dump` for debugging).
Small — no vectors. The sidecar is what lets the daemon serve results and run
deltas **without re-chunking the whole tree** on restart.

### 4.3 Per-file key index (`file_rel_path → {key}`)

Derivable from the sidecar (group keys by `file_rel_path`), kept in RAM for O(1)
"which keys belong to this file" lookups during a delta. Rebuilt on `load()`.

---

## 5. The delta algorithm

On a debounced change to `file` (see §6):

```
1. new_chunks = chunk_file(file)                    // re-chunk ONLY this file
2. new = { key(c) -> (content_hash(c), meta(c)) for c in new_chunks }
3. old = sidecar keys where file_rel_path == file   // via per-file index

4. removed = old.keys - new.keys
   for k in removed: index.remove(k); sidecar.remove(k)

5. for (k, (h, meta)) in new:
      match sidecar.get(k):
        None                       -> EMBED  (new function)
        Some(old) if old.hash != h -> EMBED  (changed body)
        Some(old) if old.hash == h -> META-ONLY (unchanged body, maybe moved lines)
      // EMBED:   index.remove(k) [if present]; v = embed(c); index.add(k, v)
      // META:    (no embed) just refresh line_start/line_end etc. in sidecar
      sidecar.put(k, meta)

6. per-file index updated from new.keys
7. mark index dirty (for the next save, §7)
```

Key properties:
- **Only changed bodies are embedded.** Unchanged functions whose *line numbers*
  shifted get a **metadata-only** update (cheap, no ONNX) — important, because
  editing one function shifts every line below it.
- **Deletes and renames fall out naturally**: a rename is `removed(old name)` +
  `EMBED(new name)`; a function moved to another file is handled when *both*
  files are re-chunked (the `Notify` for each).
- **The embedding cache still applies**: `embed(c)` first checks the content-hash
  cache, so re-adding a body seen before (e.g. revert) is a cache hit.

### File deletion

`Notify` cannot always distinguish "edited" from "deleted." If `file` no longer
exists: `for k in per_file[file]: index.remove(k); sidecar.remove(k)`.

---

## 6. Notify pipeline — debounce & coalesce

The missing piece that made the old behavior feel broken.

- **Source-files-only filter (first gate).** Drop the Notify immediately if the
  path is not an indexable source file (honor the same `ProjectWalker` ignore rules
  — skip `.git/`, `target/`, `node_modules/`, binaries, the `.tldr/` dir, etc.).
  Otherwise editor/tooling churn outside the corpus drives needless deltas.
- Maintain a `pending: HashSet<PathBuf>` of changed files and a debounce timer.
- Each accepted `Notify(file)` inserts into `pending` and (re)arms a **quiet timer**
  (default **750 ms**, configurable).
- **Hard max-wait deadline.** A re-arming quiet timer alone can be starved forever
  under a steady low-rate stream of saves. Also stamp the *first* event in a batch;
  when `now - first >= max_wait` (default **5 s**) flush regardless of the quiet
  timer. So a batch flushes after 750 ms of quiet **or** 5 s elapsed, whichever first.
- On flush: drain `pending` and run the §5 delta **per file** inside one
  `spawn_blocking` job (event loop stays responsive — same pattern as TLDR-atc).
- Coalescing: a burst of saves to one file ⇒ one delta; a burst across N files ⇒
  N deltas in one job.
- **Burst cap (rolling).** If `pending` exceeds a threshold (default **200 files** —
  a branch switch / `git pull`) **or** the rolling accepted-event count over a short
  window spikes, skip deltas and schedule a **single full rebuild** instead (deltas
  stop being cheaper than a rebuild past some fraction of the corpus). The full
  rebuild supersedes any queued deltas (clear `pending`).

---

## 7. Persistence & crash recovery

- **Three files must commit as one unit** (Codex review): the usearch index, the
  sidecar, and the manifest (§4.0). `usearch::save(path)` is a plain wrapper — it is
  **not** an atomic temp+rename — so a crash between writing the index and the
  sidecar/manifest would otherwise leave a split-brain pair (valid vectors, stale or
  missing metadata).
- **Save protocol** (periodic — every ~30 s or after K deltas):
  1. `gen = manifest.generation + 1`.
  2. Write **all three** to unique temp paths: `index.save(index.tmp.<gen>)`,
     sidecar → `meta.tmp.<gen>`, manifest (carrying `gen` + sidecar `checksum`) →
     `manifest.tmp.<gen>`. `fsync` each.
  3. Rename index and sidecar into place, then **rename the manifest LAST** — the
     manifest swap is the single **commit point**. A crash before it leaves the old
     trio intact (temps are reaped on next open).
- On daemon **startup**: read the manifest first; `index.load()` + read sidecar;
  **verify the manifest gates** (§4.0: format/model/dims/scalar/schema/root match,
  index dims+count agree, sidecar checksum + `generation` match). Any mismatch →
  **full rebuild**.
- If the manifest verifies, **reconcile**: compare each source file's on-disk mtime
  to its `indexed_at`; files newer → run a delta; sidecar files that no longer exist
  → remove their keys. This bounds staleness after a crash without a full rebuild.
- If load fails or reconcile detects structural drift → **full rebuild**.

---

## 8. Concurrency

- The writable index + sidecar live behind the daemon's existing
  `Arc<std::sync::Mutex<…>>` (introduced in TLDR-atc). All mutation (delta jobs,
  saves) and search happen while holding it **inside `spawn_blocking`**, so the
  async event loop never blocks and `daemon stop` stays responsive.
- **`handle_notify` MUST NOT take the index mutex on the async thread** (Codex
  review — today it does, `daemon.rs::handle_notify`, which parks a Tokio worker
  for the *whole build* if a Notify lands mid-build). Notify is async and must stay
  O(1) and lock-free w.r.t. the index: it only (a) filters non-source paths, (b)
  inserts into the debounce `pending` set (its own tiny lock or an mpsc channel),
  and (c) arms/stamps the timer. The index mutex is acquired **only** later, inside
  the `spawn_blocking` flush job. Same rule for the legacy "invalidate" path until
  deltas land: signal via an `AtomicBool`/channel, never `index.lock()` inline.
- Search and delta serialize on the mutex (correct; both are short once warm).
- One daemon per project (socket keyed by project hash), so there is no
  cross-daemon writer contention on the same index file in normal use. Saves use
  atomic temp+rename so an external reader never sees a half-written index.

---

## 9. Edge cases

| Case | Handling |
|---|---|
| Edit function body | same key, hash differs → re-embed |
| Edit lines above a function | key+hash unchanged → **metadata-only** line update |
| Rename function | remove old key, embed new key |
| Two functions same name in a file | `ordinal` in identity disambiguates |
| Delete function | key in `old` not in `new` → remove |
| Delete file | file gone → remove all its keys |
| Move function across files | handled via both files' deltas |
| Rename/move a file | old path keys removed, new path keys added |
| Unsupported/binary file Notify | chunker yields nothing → treat as "no chunks" (removes stale keys if any) |
| Hash collision (two identities → same u64) | sidecar `identity` mismatch detected → fall back to full rebuild |
| Burst (branch switch) | `pending` cap → single full rebuild |

---

## 10. Failure handling / fallback

Incremental is an **optimization layered over** a correct full rebuild. Any of
these trigger a full rebuild (logged, not silent):
- sidecar/index load failure or version mismatch,
- identity-collision detected,
- `pending` over the burst cap,
- a delta job error (chunk/embed failure) for a file → rebuild that file or all.

The full-rebuild path is exactly today's `SemanticIndex::build` (now usearch-backed).

---

## 11. Phasing

1. **P0 — usearch store (TLDR-l5d):** `key→vector` add/remove/search/save/load +
   **metadata sidecar + manifest (§4.0)** + per-file index + the paired-atomic save
   protocol (§7) + manifest-gated load. Full-rebuild path only, no deltas yet — but
   the sidecar/manifest are **correctness requirements here**, not deferrable to t8f
   (a restart must serve results without re-chunking the tree).
2. **P1 — delta core (this doc §5):** `apply_file_delta(file)` with embed/meta/remove
   classification. Wire `handle_notify` to call it (still synchronous, no debounce).
3. **P2 — debounce pipeline (§6):** `pending` set + timer + `spawn_blocking` batch +
   burst cap.
4. **P3 — persistence/recovery (§7):** periodic atomic save + startup reconcile.

Each phase is independently testable and shippable.

---

## 12. Testing

- **Unit (delta classifier):** synthetic before/after chunk sets → assert the
  exact `{embed, meta-only, remove}` partition for: body edit, line shift, rename,
  dup-name, delete, file-delete.
- **Index integration:** build → `apply_file_delta` on a modified temp file →
  assert only changed keys' vectors changed, removed keys gone, search reflects it.
- **Debounce:** N rapid Notifies to one file → exactly one delta job.
- **Recovery:** save → mutate files on disk → reload → reconcile → assert deltas,
  not full rebuild.
- **Latency guard:** one-function edit re-embeds 1 chunk, not the corpus (assert
  embed count == changed-function count).

---

## 13. Acceptance (from TLDR-t8f)

- ✅ Editing one file re-embeds only that file's changed functions.
- ✅ Deleted functions' vectors are removed.
- ✅ Query results reflect the change within one debounce + save cycle.
- ✅ Full rebuild remains the fallback.

---

## 14. Open questions / decisions to confirm before P1

1. **Quantization** for `add_*`: `f32` (exact, ~58 MB) vs `i8` (~15 MB, tiny recall
   loss). Recommend **f32 first**, measure recall on the n=52 eval, then try `i8`.
2. **Debounce interval** default (750 ms?) and **burst cap** (200 files?).
3. **Sidecar format**: bincode (small/fast) vs JSON (debuggable). Recommend bincode
   with a version byte; keep a `--dump` for debugging.
4. **`indexed_at` source**: file mtime (cheap, good enough) vs content hash on
   startup (accurate, slower). Recommend mtime + content-hash tiebreak only on
   suspicion.
5. Who sends `Notify` today, and does it fire on **delete**? Verify the hook/watch
   source emits delete events, else add a startup reconcile to catch them.

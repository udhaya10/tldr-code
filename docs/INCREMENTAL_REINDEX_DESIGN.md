# Incremental Re-indexing — Design (TLDR-t8f)

Status: **✅ Implemented** (TLDR-t8f shipped). Owner: semantic search.
Depends on: **TLDR-l5d** (usearch vector store) → **TLDR-atc** (daemon warm path, ✅ done).
Parent epic: **TLDR-ac0** (replace JSON embedding cache with usearch).

> This document was written as a design *before* the code landed. The per-file
> delta is now shipped: `VectorStore::apply_file_delta`
> (`tldr-core/src/semantic/vector_store.rs`), `IndexManager::apply_delta`
> (`tldr-cli/src/commands/daemon/index_manager.rs`), called from
> `process_dirty_file` (`daemon.rs`) under `spawn_blocking`. The "nuke the
> whole index" path below NO LONGER exists. Section 1 is retained as the
> historical problem statement that motivated the design; the only remaining
> stub is the threshold-based *full* reindex (per-save invalidation + delta
> already keep results fresh — see `daemon.rs` `process_dirty_file`).

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

A small `manifest` guards against pairing valid vectors with the wrong metadata
after upgrades/model changes/crashes. It must cover **every input that changes the
vectors OR the chunk boundaries** — not just the model name (Codex review):

```
Manifest {
    format_version:   u32,   // bump on any on-disk layout change
    generation:       u64,   // the ACTIVE store generation. The index/sidecar/
                             //   manifest FILES are generation-suffixed (§7); the
                             //   filename carries the generation, so per-record
                             //   generation is unnecessary — file-level suffices.
    embedding_model:  String,// e.g. "ArcticL"
    model_revision:   String,// fastembed/ONNX weights + TOKENIZER revision. Same
                             //   model NAME with a changed tokenizer ⇒ incompatible
                             //   vectors that would otherwise silently match.
    dimensions:       u32,   // must equal the loaded usearch index dims
    metric:           String,// "cos"
    scalar_kind:      String,// "f32" | "i8" — quantization changes vectors
    search_mode:      String,// "exact" (vs hnsw) — guards a different search build
    embed_schema:     String,// raw-v1 / enriched-v1 (the embed-INPUT recipe tag)
    chunk_params:     String,// granularity + max_tokens + overlap + language filter
                             //   — a chunk-config change moves function boundaries
                             //   (same content_hash, different spans).
    walker_version:   String,// source-selection / ignore-rule version — changes
                             //   WHICH files are in the corpus.
    root:             String,// canonical project root the keys are relative to
    chunk_count:      u64,
    keys_checksum:    u64,   // digest of the SORTED usearch key set — detects KEY-SET
                             //   drift (membership) the sidecar checksum misses.
    index_checksum:   u64,   // digest of the on-disk index FILE bytes — detects a
                             //   corrupted/swapped index where the keys still match
                             //   but the VECTORS are wrong (keys_checksum can't catch
                             //   that; Codex round-3). Computed over index.<gen>.usearch.
    sidecar_checksum: u64,   // digest of the sidecar payload.
}
```

On `load()`, **reject → full rebuild** (logged, never silent) if ANY of
`format_version, embedding_model, model_revision, dimensions, scalar_kind,
search_mode, embed_schema, chunk_params, walker_version, root` mismatch the running
config; OR the loaded index's reported dims/count disagree with the manifest; OR
`sidecar_checksum` / `index_checksum` fails; OR the digest of the loaded usearch key
set ≠ `keys_checksum`; OR the generation embedded in the loaded filenames ≠
`generation`. (`index_checksum` covers vector correctness; `keys_checksum` covers
key membership — both are needed.)

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
classified `remove(old) + embed(new)`. The damage is bounded **only if** the embed
hits a content-addressed cache (Codex review): the re-`embed` must resolve the
**unchanged body** to its existing vector via a `(content_hash, model, embed_schema)
→ vector` lookup, so it is a cache hit (no ONNX), not a real re-embed.
**l5d requirement:** preserve this content-addressed embedding lookup as a layer
*distinct* from the `key (u64) → vector` usearch index — the usearch migration must
NOT drop content-hash dedup, or every ordinal shift / rename becomes a true
re-embed. (It is the current `EmbeddingCache` role; keep it, content-keyed.)
If churn still proves noticeable, replace the positional ordinal with a
content-anchored disambiguator (e.g. a short prefix hash of the body) so identity is
insertion-order-independent.

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
    content_hash:  String,   // detects body changes; also anchors the snippet read
}
```

Serialized compactly (bincode preferred; JSON behind a `--dump` for debugging).
**No vectors and no per-record `generation`** — the sidecar FILE is
generation-suffixed (§7), so the generation is carried at the file level.

**Snippets are NOT stored — read lazily at query time** (Codex review: §4 claimed
the sidecar serves results without re-chunking, but ChunkMeta had no snippet). The
decision: keep the sidecar small by NOT storing chunk text. At query time, produce
the snippet from a **single read of the bounded span** anchored by `(file_rel_path,
line_start, line_end)`, then **hash that exact buffer** and render the snippet **from
the same buffer** — read once, validate, render, never re-open between hash and
render (avoids a TOCTOU where the file changes between validation and display).
Guards: missing file, `line_start/line_end` out of range, an over-large span, or
`hash ≠ content_hash` → return the result **without a snippet** (degraded, never
wrong). This avoids re-chunking (we have the line range), avoids sidecar bloat, and
is self-correcting (a changed file triggers a delta that refreshes vector + lines).
Storing `content` in the sidecar is an opt-in alternative for fully source-
independent serving, at the cost of size — deferred unless a need appears.

`indexed_at` (the per-file mtime+size used by reconcile) lives on the **per-file
record** (§4.3), not per chunk.

### 4.3 Per-file record (`file_rel_path → FileRecord`)

Persisted alongside the sidecar (and rebuilt in RAM on `load()`):

```
FileRecord {
    keys:      Set<u64>, // which chunk keys belong to this file (O(1) delta lookup)
    mtime:     u64,      // file mtime AT INDEX time — reconcile signal (§7)
    size:      u64,      // file size  AT INDEX time — catches same-mtime edits
    file_type: enum,     // Regular | Symlink | Other — detects file↔dir/type swaps
                         //   at reconcile (§7.3), not just content changes
}
```

`keys` drives "which vectors to touch for this file"; `(mtime, size)` is the
startup-reconcile signal. Comparing against the **stored** mtime (not wall-clock)
means clock skew is irrelevant — we only ask "did this file change since we indexed
it." Size catches the same-mtime-different-content case that mtime alone misses.

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
- **Burst cap (rolling).** Schedule a **single full rebuild** instead of deltas when
  EITHER: `pending` size > **200 files** (default; a branch switch / `git pull`), OR
  the rolling count of accepted events over a **2 s window** exceeds **1000** (a
  storm). The full rebuild supersedes any queued deltas (clear `pending`). All three
  numbers (`200`, `2 s`, `1000`) are config knobs; defaults chosen so a normal
  multi-file save stays on the delta path while a tree-wide churn flips to rebuild.

---

## 7. Persistence & crash recovery

### 7.1 Crash-safe save — generation-suffixed files + pointer swap

The naive "rename index+sidecar, then rename manifest last" is **NOT crash-safe**
(Codex review): renaming index/sidecar over the live files *destroys the old
committed pair*, so a crash before the manifest rename corrupts both old and new.
`usearch::save(path)` is also a plain wrapper, not an atomic temp+rename.

The fix is **immutable, generation-suffixed artifacts plus a single atomic pointer**:

- Files are named by generation and **never overwritten in place**:
  `index.<gen>.usearch`, `meta.<gen>`, `manifest.<gen>`.
- One tiny `CURRENT` file holds the active generation number — its atomic rename is
  the **only** commit point.

**Save** (periodic — every ~30 s or after K deltas):
1. `gen = current + 1`.
2. Write the three new-generation files via temp+rename:
   `index.save(index.<gen>.tmp)` → `fsync` → rename to `index.<gen>.usearch`;
   same for `meta.<gen>` and `manifest.<gen>` (the manifest embeds `gen`,
   `keys_checksum`, `sidecar_checksum`). **`fsync` each file AND `fsync` the
   directory** after the renames (a rename isn't durable until the dir is synced).
3. Write `CURRENT.tmp` → `fsync` → **rename `CURRENT.tmp` → `CURRENT`** → `fsync` the
   directory. This rename is the commit point. **`CURRENT` is structured, not a bare
   integer** (Codex round-3): `CURRENT { magic: u32, gen: u64, checksum: u32 }` so a
   torn/partial write is *detectable*. On read, if `magic`/`checksum` is invalid,
   **do not trust it** — fall back (§7.2) to scanning `manifest.<gen>` files newest→
   oldest and loading the newest one that verifies.
4. GC: retain the **last `KEEP_GENS` generations** (default **3**) AND any generation
   younger than a **grace window** (default **60 s**); delete only generations that
   are both older than `KEEP_GENS` back *and* past the grace window. Never delete the
   generation `CURRENT` points at. (Retention > 1 + grace is what makes the
   concurrent-reader race below safe; a bare `gen-1` is not enough.)

A crash at any point leaves `CURRENT` pointing at a fully-written older generation
(or, if `CURRENT` itself is torn, the newest verifying `manifest.<gen>`); the
half-written `<gen>` files are unreferenced and reaped on next open. No in-place
overwrite ever touches a live generation.

### 7.2 Load + verify (startup, and any reader)

1. Read `CURRENT`; if `magic`/`checksum` valid → `gen`. If `CURRENT` is torn/missing
   → **scan** `manifest.<gen>` files newest→oldest and pick the newest that verifies.
2. Load `index.<gen>.usearch`, `meta.<gen>`, `manifest.<gen>`. If any file is missing
   (a concurrent GC removed it after we read `CURRENT`) → **re-read `CURRENT` and
   retry** (bounded, e.g. 3×); the retention+grace (§7.1) makes this near-impossible.
3. **Verify the manifest gates** (§4.0): config fields match, index dims/count agree,
   `sidecar_checksum` AND `index_checksum` ok, usearch key-set digest == `keys_checksum`,
   and the generation embedded in all three filenames == `gen`.
4. On verify failure, try the **previous generation**; if none of the retained
   generations verify → **full rebuild**.

This load path is used by the daemon at startup AND by a **cold CLI reader** that
opens the store directly — see §8 for the reader/writer concurrency contract.

### 7.3 Reconcile (precise, implementable)

After a verified load, catch changes made while the daemon was down — **without**
re-chunking the whole tree:

- The `FileRecord` (§4.3) stores `file_type` (regular / symlink / other) alongside
  `{keys, mtime, size}`, so reconcile can detect *type* changes, not just content.
- For each `FileRecord`: `lstat`/`stat` the path.
  - **Now a regular indexable file**, and **mtime ≠ stored OR size ≠ stored** → §5
    delta. (Compare to *stored* values, so clock skew is irrelevant; size catches
    the rare same-mtime edit.)
  - **Gone**, OR **no longer a regular indexable file** (replaced by a directory,
    socket, etc. — the file↔dir swap) → treat as **deletion**: remove its keys.
- A source file **on disk with no `FileRecord`** → chunk + add (created while down,
  or a dir→file swap).
- **Symlinks:** follow `ProjectWalker`'s policy (it does not follow symlinks by
  default → symlinked files are simply not in the corpus). If a future config
  enables following them, key by the **canonical target path** and store the target's
  identity in the record, so a re-pointed link is seen as a change. Until then,
  symlinks are out of scope and excluded — documented, not silently mishandled.
- **Case-only renames** (`Foo.rs`→`foo.rs`) on case-insensitive filesystems: the
  corpus enumeration normalizes paths per the platform's case rules and detects
  canonical-case drift as a rename (old record removed, new added). Edge case; the
  walker's canonicalization is the single source of truth for path identity.
- **Residual risk:** same mtime AND same size AND same type AND different content.
  Genuinely rare; self-heals on the next real edit; escape hatch = `tldr index
  --rebuild` (or a content-hash full sweep behind an explicit flag). We do **not**
  content-hash every file on every startup — unbounded on large repos; mtime+size+type
  is the bounded default.
- If reconcile itself errors or detects structural drift → **full rebuild**.

---

## 8. Concurrency

- The writable index + sidecar live behind the daemon's `IndexManager`, which
  holds a `parking_lot::RwLock<Option<(EmbeddingModel, VectorStore)>>` (the
  read/write split lets concurrent queries share a read lock while delta/warm
  take the write lock; the embedder is a separate `parking_lot::Mutex`). All
  mutation (delta jobs, saves) and search happen while holding the appropriate
  guard **inside `spawn_blocking`**, so the async event loop never blocks and
  `daemon stop` stays responsive.
- **`handle_notify` MUST NOT take the index mutex on the async thread** (Codex
  review — today it does, `daemon.rs::handle_notify`, which parks a Tokio worker
  for the *whole build* if a Notify lands mid-build). Notify is async and must stay
  O(1) and lock-free w.r.t. the index: it only (a) filters non-source paths, (b)
  inserts into the debounce `pending` set (its own tiny lock or an mpsc channel),
  and (c) arms/stamps the timer. The index mutex is acquired **only** later, inside
  the `spawn_blocking` flush job. Same rule for the legacy "invalidate" path until
  deltas land: signal via an `AtomicBool`/channel, never `index.lock()` inline.
- Search and delta serialize on the mutex (correct; both are short once warm).
- One daemon per project (socket keyed by project hash) → a single writer; no
  cross-daemon writer contention on the store in normal use.
- **Reader/writer contract — the cold CLI can read the store while the daemon writes
  it** (Codex round-3: the cold `tldr semantic` path opens the store directly, so the
  daemon is NOT the only reader). The store is **single-writer (daemon), multi-reader**:
  - Writers only ever *create* new `<gen>` files and atomically swap `CURRENT`
    (§7.1); they never mutate a published generation in place.
  - A reader snapshots `CURRENT` → `gen`, then opens that generation's immutable
    trio. GC **retention (`KEEP_GENS`) + grace window** (§7.1) keep the snapshotted
    generation alive long enough to open; if a reader still races a GC and hits a
    missing file, it **re-reads `CURRENT` and retries** (§7.2).
  - This needs no cross-process lock for *reads*. (A coarse advisory file lock around
    *writes* is optional belt-and-suspenders, but the immutable-generation + pointer
    design already makes readers see a consistent snapshot.)

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

To avoid the earlier P0/P3 overlap (Codex review): the **save/load mechanism**
(§7.1 generation-suffixed atomic save, §7.2 manifest-gated load) is **P0** — any
persisted store needs it. The **delta-specific** parts (periodic *delta*-save
cadence + GC tuning, and the §7.3 *reconcile*) are **P3**.

1. **P0 — usearch store (TLDR-l5d):** `key→vector` add/remove/exact_search + content-
   addressed dedup layer (§4.1) + **sidecar + manifest (§4.0)** + per-file records
   (§4.3) + the **§7.1 crash-safe save / §7.2 manifest-gated load**. Full-rebuild
   path only, no deltas. The sidecar/manifest/atomic-save are **correctness
   requirements here**, not deferrable (a restart must serve results without
   re-chunking, and must never load a split-brain store).
2. **P1 — delta core (§5):** `apply_file_delta(file)` with embed/meta/remove
   classification. Wire `handle_notify` to call it (synchronous, no debounce yet).
3. **P2 — debounce pipeline (§6):** source filter + `pending` set + quiet timer +
   max-wait + `spawn_blocking` batch + burst cap; `handle_notify` stays lock-free (§8).
4. **P3 — incremental persistence/recovery:** periodic **delta**-save cadence + old-
   generation GC (on top of P0's save mechanism) + the **§7.3 startup reconcile**
   (mtime+size deltas instead of full rebuild).

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

## 14. Open questions / decisions

**Resolved (this round):**
- Quantization → **f32 first** (i8 = TLDR-ccg; measure recall on the n=52 eval first).
- Debounce → 750 ms quiet + 5 s max-wait; burst cap 200 files / 1000 events-per-2s (§6).
- Reconcile signal → **mtime + size** vs the per-file record; no all-files content
  hash on startup (§7.3). Content-hash is the per-chunk change detector inside a delta.
- Sidecar format → bincode + version byte; `--dump` for JSON debugging.
- **Migration / one-time re-embed → ACCEPTED.** Moving JSON cache → usearch (and the
  earlier root-relative key change) invalidates old keys once; we pay a single cold
  re-embed rather than building a dual-read legacy shim. The atc re-embed already
  paid most of it for the deployed model.

**Manifest field encodings (resolved — concrete recipes):**
- `model_revision` = the `EmbeddingModel` enum variant **+** the fastembed model id
  **+** a pinned source revision/hash of the ONNX weights & tokenizer (fastembed
  exposes the model descriptor; pin its revision). A tokenizer/weights bump changes
  this string → invalidates.
- `chunk_params` = a stable serialization of `ChunkOptions` (granularity, max_tokens,
  overlap, language filter) — e.g. its bincode/debug digest. Boundary-affecting only.
- `walker_version` = a digest of the effective ignore-rule set + walker config
  (default-excludes, `.gitignore` honored?, lang hint) — bump when corpus selection
  changes.
- `embed_schema` already exists (raw-v1 / enriched-v1). `format_version` is a manual
  bump for any on-disk layout change.

**Still open — confirm before / during P0:**
1. **`Notify` source & delete events.** Confirm what emits `Notify` today and whether
   it fires on file delete/rename. If deletes aren't emitted, §7.3 reconcile is the
   safety net — but verify the watcher so deletes aren't silently missed while live.
2. **`content` in sidecar?** Default = lazy source read (§4.2). Revisit only if a
   consumer needs snippets for files that may be absent/moved at query time.
3. **`KEEP_GENS` / grace window** (§7.1) tuning vs disk usage (each retained
   generation is a full index copy) — start at 3 gens / 60 s, measure.

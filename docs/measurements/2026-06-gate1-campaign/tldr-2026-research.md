# Research: 2026 state-of-the-art for persistent, zero-copy, incremental analysis caches
## TLDR-zde research annex — sources scraped 2026-06-04 via Firecrawl (.firecrawl/)
## Status: research base. Codex review (pending retry) merges into this; user requirements R1-R6 are the rubric.

## A. What each production system does (2026)

### A1. salsa crate — persistent caching MERGED (PR #967, Aug 11 2025, `persistence` feature)
Source: github.com/salsa-rs/salsa/pull/967 + hackmd.io/@ibraheemdev/S17Bxowwxg
- ADDITIVE OPT-IN persistence: only queries/structs marked serializable are persisted;
  unmarked intermediate memos are dropped and their input deps are FLATTENED into the
  serialized query's dep list (so correctness survives partial persistence).
- `ValidateInput` trait: after deserialize, inputs re-sync against the real world
  (read file → if changed, set_field → revision bump → downstream invalidation).
  This is exactly tldr's kkt freshness-gate idea, formalized.
- Compile-time consistency: a serializable struct cannot contain a non-serializable
  one (can't persist an ID without its data).
- Limitations (per PR): transitive deps forced serializable, no query hashing yet,
  accumulators not serializable. Followups active — a Jan 1 2026 issue comment marks
  this as THE live thread (driven by ty/Astral's needs; MichaReiser asked for it
  in Apr 2024 explicitly because "persistent cache is essential for CLI + CI").
- FORMAT: serde-based → parse-on-load. NOT zero-copy. This is the gap vs R1.

### A2. Turbopack — filesystem caching SHIPPED on-by-default (Next.js 16.1)
Source: nextjs.org/blog/turbopack-incremental-computation + turbo-persistence README
- Persists the ENTIRE incremental-computation state: dependency graph, aggregation
  graph, and all intermediate value cells. Restart = resume warm. "Over a year of
  dedicated work" to meet quality bar.
- Aggregation graph: coarsened layers over the fine dep graph (hundreds of thousands
  of nodes) so invalidation traversals don't walk the full graph — their answer to
  graph-walk cost at scale.
- turbo-persistence (custom store, LSM-flavored):
  - Immutable files named by sequence number; single CURRENT file holds committed seq
    (same shape as tldr's vector_store generation pattern — independent convergence!).
  - Static Sorted Tables (.sst) + blob files + tombstone files + meta files with
    hash-range + AMQF (approximate membership filter) for fast key routing.
  - Value tiers: INLINE (≤8B in key block) / SMALL (≤4KB shared blocks) /
    MEDIUM (≤64MB dedicated blocks) / BLOB (>64MB separate file).
  - Single write transaction, concurrently fillable from many threads; uncommitted
    leftovers auto-cleaned on startup. Optimized for bulk-write + fast random read.

### A3. rust-analyzer — durabilities (the validation-cost insight)
Source: rust-analyzer.github.io/blog/2023/07/24/durable-incrementality.html (+ issue #4712)
- No persistent cache in-process (still true; #4712 remains open; their hopes ride on
  the new salsa persistence work).
- KEY IDEA TO STEAL — durability tiers: inputs classified volatile/normal/durable
  (user file vs deps vs stdlib). Validation flooding stops at durability boundaries:
  a change to a volatile input doesn't force re-validating the durable universe.
  Without this, even no-op revalidation walks the whole query graph.
- Maps directly to tldr: per-file chunks = normal; project config (Cargo.toml,
  tsconfig, .tldrignore!) = durable inputs whose change invalidates ModuleIndex
  and the compose layer wholesale; compose output = volatile derived layer.

### A4. Cloudflare mmap-sync — zero-copy reads in production (the R1+R3 existence proof)
Source: crates.io/crates/mmap-sync (v2.x)
- rkyv-archived data in shared mmap; wait-free single-writer / many-reader via
  Synchronizer; readers access archived structs IN PLACE — zero deserialization,
  zero copies, RAM cost = page cache only.
- Production-proven (Cloudflare bot-scoring ML feature store). Proves the
  "disk holds archived data, RAM holds only the map + index" model at scale.

### A5. rkyv — the zero-copy format itself
Source: rkyv.org/zero-copy-deserialization.html, docs.rs/rkyv
- Total zero-copy: archived form IS the in-memory form (relative pointers);
  `access` = pointer cast + optional bytecheck validation. No allocation, no parse.
- Cost side: requires Archive derives across the whole type tree (PathBuf/String/
  HashMap-heavy types need rkyv-compatible mirrors); archived types are read-only
  views; schema evolution = explicit versioning discipline.
- bincode/postcard by contrast: compact but ALWAYS parse+allocate on load.

## B. Synthesis: the 2026 architecture for tldr's analysis cache, vs requirements

R1 (near-zero ser/de CPU):
  Chunk payloads = rkyv archives, mmap'd, accessed in place (mmap-sync model).
  Write cost: one archive serialize per changed file (cold-path only).
  Read cost: pointer cast + bytecheck. NO parse on daemon restart or query.
  v1 pragmatic fallback: bincode chunks (serde-native, zero derive churn) are
  acceptable ONLY if the restart budget tolerates parse — the research says the
  2026 answer is rkyv; bincode is the 2020 answer.

R2 (disk is cheap): rkyv archives are larger than bincode (alignment, relative
  pointers) — explicitly acceptable. Value-tier packing (turbo-persistence INLINE/
  SMALL/MEDIUM/BLOB) available later if small-file overhead bites.

R3 (RAM = index only):
  - RAM holds: key index {rel_path → (generation, offset/len, content_hash)} = O(#files),
    + mmap page cache (OS-managed, evictable under pressure — not process heap).
  - CRITICAL EXTENSION the user's framing demands: the COMPOSED graph must also be
    an rkyv artifact on disk (per generation, keyed by corpus digest), mmap'd and
    queried in place — exactly like usearch view(). Otherwise ResidentGraph in an
    ArcSwap re-creates a heap-resident O(V+E) structure and violates R3.
  - Queries walk the archived graph zero-copy; hot pages stay cached, cold pages
    get evicted. Process heap stays O(#files) + small.

R4 (O(n)/O(n log n) audit):
  - Disk: chunks O(source bytes); edges as per-file OUTGOING adjacency lists →
    total O(E) (real-world E ≈ near-linear; the O(n²) trap is adjacency matrices
    or all-pairs precomputation — design stores none); manifest O(#files);
    interning table O(unique strings). Sorted key index → O(n log n) build,
    O(log n) lookup (SST model).
  - RAM: index O(#files); compose working set O(V+E) TRANSIENT during recompose
    (streamed, then archived + dropped); steady-state = page cache only.
  - Aggregation-graph idea (Turbopack) reserved for when invalidation traversal
    itself becomes the bottleneck — not v1.
  - Watch item: reverse-edge index ("who calls X") doubles E on disk — still O(E).

R5 (ignore-first):
  - ONE membership oracle (gitignore + tldrignore + excludes, TLDR-bpf/9w8) applied
    at ENUMERATION; ignored paths are never stat'd into the manifest, never hashed,
    never parsed, never chunked, never watched-through (TLDR-1j2 pre-channel filter),
    never composed. Salsa's ValidateInput formalizes the restart re-sync: validate =
    diff manifest against oracle output; only members get content-hash checks.
  - .tldrignore/.gitignore THEMSELVES are durable inputs (A3): their change
    invalidates membership → manifest diff → chunk add/drop + recompose.

R6 (Codex as research base): pending retry (thread handle alive). Its Q1-Q10
  answers get merged into this annex; disagreements adjudicated against THIS
  evidence base, not vibes.

## C. Sharpened answers to the design doc's open questions

Q3 (format): rkyv for chunk payloads AND the composed-graph artifact is the 2026
  answer (A4/A5). Middle ground struck differently than originally guessed: it's
  not "bincode chunks + rkyv composed graph" — if we pay rkyv derive churn at all,
  pay it once for both. bincode survives only as a v0 spike format.
Q4 (persist composed graph): YES — upgraded from "maybe" to REQUIRED by R3.
  Per-generation, keyed by corpus digest, mmap'd; this is what makes restart O(seconds)
  and steady-state RAM index-only.
Q10 (real salsa crate?): NOT for v1. Evidence: salsa persistence is serde-based
  (violates R1 on the hot path), still limitation-heavy (no query hashing,
  transitive-serializability constraints), and tldr's dependency structure is
  shallow (file → chunk → compose) — it doesn't need salsa's general recursive
  query machinery. STEAL its concepts (additive persistence, ValidateInput,
  dependency flattening, durability tiers) without the crate. Revisit if compose
  ever needs fine-grained incrementality (deep derived-query chains).
Q1 (compose cost): still THE open gate — none of the researched systems make
  full-recompose cheap by magic; Turbopack's answer (aggregation layers) is what
  we'd reach for IF the bench fails the ~2s budget. Bench remains the blocker.

## D. Source inventory (scraped, in .firecrawl/)
- turbopack-incremental.md — Next.js engineering blog (2025/2026 era, FS cache stable)
- turbo-persistence-readme.md — store format spec
- ra-durable.md — durabilities design
- ra-persist-issue.md — rust-analyzer #4712 thread
- salsa-pr967.md — merged persistence prototype PR
- salsa-persist-design.md — ibraheemdev HackMD design doc
- salsa-issue10.md — 7-year persistence RFC thread incl. ty/Astral demand signal
- mmap-sync.md — Cloudflare zero-copy mmap crate

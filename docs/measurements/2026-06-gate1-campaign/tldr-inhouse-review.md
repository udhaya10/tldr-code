# IN-HOUSE DESIGN REVIEW — analysis store (TLDR-zde)
# Substitute for the Codex review (provider outage); same brief, same deliverables.
# Reviewer: Claude (session) + advisor pass + Firecrawl web verification. 2026-06-04.

## 1. VERDICT: APPROVE WITH 5 NAMED AMENDMENTS (below). No R1-R6 violations found.

## 2. WEB-VERIFIED FINDINGS (deliverable 4)

(a) rkyv 0.8 for mmap workloads — FIT, with one pattern mandated.
    Source: https://rkyv.org/faq.html
    - The FAQ explicitly blesses the exact pattern we need: full bytecheck validation
      is OPTIONAL; "there are many other ways you can verify your data, for example
      with checksums". Even when validating, "validation is still faster than
      deserializing with other high-performance formats".
    - AMENDMENT A2 derived: validate ONCE after write (bytecheck) + store xxh3
      checksum in the artifact header; subsequent opens verify checksum then use
      access_unchecked. Avoids O(artifact-size) bytecheck on every daemon start —
      without it, a multi-GB artifact would pay a full scan per open and quietly
      violate R1.
    - Risks priced: derive churn across FileIR tree is real but bounded (std types
      incl. PathBuf supported); schema evolution is manual → version field in
      header + rebuild-on-mismatch (already in design).

(b) fst crate — OVERQUALIFIED for our scale, adopt without reservation.
    Sources: https://blog.burntsushi.net/transducers/ , https://github.com/burntsushi/fst
    - Canonical demo indexes 1.6 BILLION keys; the crate is designed around mmap
      ("leverages memory maps to make range queries fast"). Our symbol index is
      30k-1M keys — three to five orders of magnitude below its design point.
    - Bonus: prefix/range/fuzzy (Levenshtein) queries built in — future `tldr`
      symbol-completion features come free.

(c) salsa persistence followups since PR#967 — ACTIVE but architecture unchanged.
    Source: https://github.com/salsa-rs/salsa/pulls?q=is%3Apr+persistence
    - Followup PRs #969, #973, #975, #992, #1027, #1088 (8 closed) — persistence is
      maturing fast, BUT remains serde-based opt-in serialization (parse-on-load).
    - Decision unchanged: steal concepts (ValidateInput, additive persistence,
      durability tiers), skip the crate for v1. Re-evaluate at v2 if compose ever
      needs fine-grained derived-query incrementality.

(d) Prior art: Meta Glean incremental indexing — VALIDATES our simpler v1 choice.
    Source: https://glean.software/blog/incremental/
    - Glean's model: per-unit facts; incremental DB "hides" changed units' facts
      (≈ our chunk overwrite); visibility propagated via OWNERSHIP SETS
      (Elias-Fano coded, interval-mapped, ~7% DB overhead); derived facts carry
      boolean ownership expressions that "can get arbitrarily large".
    - Lesson: ownership/visibility machinery is what you must build if you do NOT
      recompose derived state from scratch. It is substantially complex. Our
      recompose-always v1 (given Gate 1 passes) avoids that entire class.
      Glean's design is the documented ESCALATION PATH if incremental compose is
      ever forced — do not invent a third alternative (Amendment A5).

## 3. GATE ANSWERS (deliverable 2)

GATE 1 — compose-cost bench spec (THE precondition; ~20 lines of instrumentation,
no refactor needed):
  - Instrument build_project_call_graph_v2 with env-gated phase timers
    (TLDR_PHASE_TIMING=1): T_parse = build_indices_parallel (step 4);
    T_compose = steps 5-12 (add_file, build_indices, ModuleIndex,
    ImportResolver/ReExportTracer loop, edge creation).
  - Run on (i) tldr-code (1570 files/~28k fns) and (ii) one 5-10x larger corpus
    (big OSS repo) to get the scaling slope, not just a point.
  - PASS: T_compose ≤ 2s on tldr-code AND slope ≤ O(n log n) between sizes.
  - MARGINAL (2-5s): acceptable — deltas are already debounced/coalesced by the
    serialized worker; recompose at most once per quiet window.
  - FAIL (>5s or superlinear): v1 must add incremental compose; adopt the Glean
    escalation path (per-file edge lists + visibility) — STOP and redesign first.

ID-STABILITY CONTRACT:
  - Node IDs are generation-local. NEVER persisted outside the artifact, NEVER
    returned to clients (responses carry symbol names + file:line). All external
    entry via FST lookup. This single rule eliminates cross-generation dangling-ID
    bugs by construction.
  - Readers pin a generation at open; on unix an unlinked-but-mapped file stays
    valid, so GC can delete eagerly. WINDOWS CAVEAT: mapped files cannot be
    deleted → GC must tolerate failure + retry later (Flow 2 has the same issue;
    share the code).
  - GC policy: keep ≥2 generations + age threshold = the reader-grace contract;
    adopt/extend TLDR-pdb's Flow-2 decision rather than inventing a second one.

MULTI-LANGUAGE (Q6): chunks already carry language; compose runs PER LANGUAGE
  (builder_v2 is per-language today — no cross-language edges exist to lose).
  One manifest with per-language sections: file list + node-range map + CSR
  artifact offset per language. Language-agnostic projections (structure, tree,
  extract) read chunks directly and ignore compose entirely.

Q8 — WHAT SURVIVES FROM salsa.rs:
  - revision counter → generation sequence number under CURRENT (monotonicity
    preserved; same role, better durability).
  - dependents map (input_hash → QueryKeys) → DELETED, replaced structurally by
    the file→node-range manifest. Nothing hash-shaped to maintain.
  - maybe_evict byte caps → generation GC + orphan-chunk GC by manifest
    reachability (mirrors TLDR-za0 on Flow 2).
  - Busy-token/liveness semantics (TLDR-3w5) and warm-ack latency (utj.7):
    untouched by design; recompose runs inside the existing serialized worker.

## 4. ADVERSARIAL PROJECTION HUNT (deliverable 3)

Examined all ~19 commands. NO command requires a third flow. Three findings:
  - `temporal` + `coupling` (co-change): need GIT HISTORY as an additional INPUT.
    Git is an input source read at query time, not a store we own — consistent
    with the two-flow rule the same way source text is. (If churn queries ever
    get slow, the v3 Parquet sidecar note in the ADR covers it.)
  - `explain`/`context`: need source text snippets → read from disk on demand,
    stateless. Fine.
  - `dead`: needs root/entry-point determination → derivable from chunk facts
    (main fns, pub visibility, test attrs). Requires the chunk schema to carry
    visibility/attr flags — ADD to chunk schema checklist.
  Confirmed dependency: `complexity`/`smells` require parse-time metrics in chunks
  (already mandated by the two-flow decision D2).

## 5. TABLE-STAKES OMISSIONS FOUND (deliverable 5)

  - Crash mid-generation-write: covered by CURRENT-flip pattern (Flow 2 proven).
    But chunk overwrites OUTSIDE a generation flip (delta path) need atomic
    rename + manifest update ordering: write chunk → fsync → update manifest →
    fsync → THEN recompose. Spell this out in the implementation issue.
  - Windows mmap deletion semantics (above) — share Flow 2's handling.
  - Concurrent CLI reader during GC — adopt TLDR-pdb contract (above).
  - Artifact integrity on partial disk (ENOSPC mid-write): generation pattern
    handles (incomplete generation never becomes CURRENT); chunk delta path must
    write-to-temp + rename, never truncate-in-place.

## 6. AMENDMENTS (the verdict's conditions)

  A1. Gate-1 phase-timer bench is a HARD precondition to any storage code.
  A2. Integrity pattern: bytecheck-once-after-write + header checksum +
      access_unchecked on subsequent opens.
  A3. Reader-grace/GC: adopt TLDR-pdb semantics for Flow 1; Windows
      deletion-failure tolerance shared with Flow 2.
  A4. Manifest gets per-language sections (Q6); chunk schema checklist grows
      visibility/attr flags (for `dead`) + parse-time metrics (for complexity/smells).
  A5. If Gate 1 fails: escalate to the Glean ownership-set path — documented,
      named, no third invention.

## 7. DISAGREEMENT LOG
  None between this review, the advisor pass, and the research annex. The one
  correction this review adds over earlier session statements: per-open bytecheck
  would have silently violated R1 at scale — caught by verification (a).

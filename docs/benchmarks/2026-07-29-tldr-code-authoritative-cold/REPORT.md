# tldr-code authoritative cold benchmark — 2026-07-29

## Purpose

This run checks the current authoritative daemon against the corrected
historical cold `tldr-code` benchmark with a nearly identical semantic
workload. It must not be compared directly with Dhan's smaller corpus by wall
time alone.

## Corpus policy

The historical run at `7ba3ba8` had no `.tldrignore`. The current policy is now
tracked in the repository and intentionally retains code-bearing tests,
fixtures, and examples so they remain part of the benchmark corpus.

Excluded material is limited to documentation, continuum/benchmark material,
the disconnected archived CLI implementation, and defensive local-secret/key
patterns. The current source tree is not byte-identical to the historical
commit, so both raw and per-vector results are reported.

## Binary and cold-state proof

- Source HEAD: `d7f45ab6c7b3816fd6ed96737803d91ff2501a3e`.
- Version: `tldr 0.4.0`.
- Build command: `cargo build --release --workspace --bins`.
- `tldr` SHA-256:
  `ae5b3891528071b5cc55311cdbb2740d12008b2e94943797e60c343a4393735b`.
- `tldr-mcp` SHA-256:
  `c7e39c121dd3f68eb9e582c47a7d2a46a5eed2c489208a568a3e3b9ac31fac5a`.
- `tldr-embed-worker` SHA-256:
  `46281dac4f4173d862787fddcf2845bc06ba93a356f8bb063d0996434f7b2081`.
- Each installed executable matched its freshly built release artifact.
- No daemon was running before cleanup.
- `tldr embeddings clear` removed one file / 30.3 MiB from the shared
  reusable document cache; downloaded model weights were preserved.
- `tldr cache clear --project <tldr-code>` removed eight files / 1.1 GiB.
- The verification `tldr cache stats` reported `No artifact store found`.
- Exactly one authoritative daemon was then started at
  `2026-07-29T07:47:00Z`.

## Cold completion

- Model: `Snowflake/snowflake-arctic-embed-m`.
- Semantic elapsed: 1,830.348s (30m30.348s).
- Files: 517.
- Stored vectors: 15,163.
- Newly embedded vectors: 15,158.
- Legitimate intra-run duplicate cache hits: 5.
- Windows: 119.
- Retries/failures: zero.
- ArtifactStore: generation 1, 517 files, 4 parse-recovery errors,
  269,488,128 redb bytes.
- Build RSS: approximately 482 MiB during inference; 620 MiB when the warm
  vector index was published.

The five cache hits are not evidence of a warm start. The reusable cache was
empty before the run, and the hits appeared only after earlier documents from
this same build had been inserted. The historical corrected run also recorded
five such intra-run duplicates.

## Apples-to-apples comparison

| Run | Files | New vectors | Duplicate hits | Time | Vectors/s | ms/new vector |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Historical `tldr-code` (`7ba3ba8`) | 512 | 15,056 | 5 | 1,670.380s | 9.014 | 110.94 |
| Current authoritative `tldr-code` | 517 | 15,158 | 5 | 1,830.348s | 8.281 | 120.75 |
| Current Dhan | 351 | 5,958 | 0 | 513.424s | 11.604 | 86.17 |

Against the historical corrected `tldr-code` run, current wall time is 9.58%
longer and time per newly embedded vector is 8.84% higher. This benchmark does
not show a semantic-build speedup. It shows performance in the same general
range with a slight regression that deserves profiling if cold-build latency
is a priority.

Dhan's 8m33s result is not a like-for-like wall-time comparison: it embedded
only 39.3% as many new vectors as the current `tldr-code` run. Dhan was also
about 28.6% faster per vector (86.17 vs 120.75 ms), which can vary with content
shape, token lengths, and window occupancy.

## Warm-path validation

After completion, the daemon reported a warm semantic index and a resident BM25
index for 517 documents.

- First dense semantic probe: 5 results in 0.51s, including creation of the
  query inference session.
- Resident BM25 probe: 5 results in 0.03s.
- Warm hybrid probe: 5 results in 0.05s.
- Bulk and query runners each reported one healthy session and zero failures.

The completed index and ArtifactStore were intentionally left warm for normal
development use.

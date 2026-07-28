# Clean mixed-batch semantic build results

The clean build completed successfully and published fresh structural and vector
generations. It used the newly installed CLI and worker recorded in
`RUN_CONTEXT.md`, an empty artifact/vector/document cache, and preserved local
model weights.

## Outcome

| Metric | Previous clean baseline | New clean run | Change |
|---|---:|---:|---:|
| Total wall time | 4,431.280 s | 3,460.211 s | -971.069 s (-21.9%) |
| Semantic worker | — | 3,454.657 s | — |
| Inference | 3,894.830 s | 3,026.470 s | -868.360 s (-22.3%) |
| Files | 559 | 511 | -48 (-8.6%) |
| Real inference rows | 51,171 | 49,212 | -1,959 (-3.8%) |
| Inference calls | 2,217 | 1,781 | -436 (-19.7%) |
| Capacity rows | 74,308 | 63,132 | -11,176 (-15.0%) |
| Dummy rows | 23,137 | 13,920 | -9,217 (-39.8%) |
| Row utilization | 68.9% | 78.0% | +9.1 percentage points |
| Weighted cell padding | 61.5% | 54.6% | -6.9 percentage points |
| Durability windows | 400 | 385 | -15 (-3.8%) |

Total wall time fell from approximately 73m51s to 57m40s, a reduction of
approximately 16m11s. Inference fell from approximately 64m55s to 50m26s.

This comparison combines two changes:

1. `.tldrignore` reduced the corpus from 559 to 511 files and from 51,171 to
   49,212 inferred documents.
2. Mixed-bucket promotion filled otherwise-dummy rows in longer fixed shapes.

It therefore proves the combined production improvement, but it does not isolate
the batching-only contribution under identical document order.

## Post-change shape accounting

| Tokens | Calls | Real rows | Capacity rows | Dummy rows | Row utilization |
|---:|---:|---:|---:|---:|---:|
| 128 | 751 | 34,144 | 48,064 | 13,920 | 71.0% |
| 256 | 232 | 7,424 | 7,424 | 0 | 100.0% |
| 384 | 210 | 2,940 | 2,940 | 0 | 100.0% |
| 512 | 588 | 4,704 | 4,704 | 0 | 100.0% |

Promotion eliminated dummy rows from the 256-, 384-, and 512-token shapes.
All remaining 13,920 dummy rows occur in 128-token batches. Packing is still
bounded by each 128-document durability window, so the implementation does not
reach the theoretical cross-window optimum.

## Component timing

| Component | Time |
|---|---:|
| Source discovery | 0.032 s |
| AST parsing | 2.640 s |
| Artifact write | 0.281 s |
| Call-graph composition | 0.094 s |
| Model load | 0.484 s |
| Semantic planning | 145.647 s |
| Cache lookup | 0.203 s |
| Inference | 3,026.470 s |
| Cache write | 235.595 s |
| Vector assembly | 36.388 s |
| Generation stage and records | 3.125 s |
| Verification | 0.817 s |
| Activation | 0.071 s |
| Semantic publication | 4.014 s |

The run embedded 49,212 documents and reported three cache hits created within
the run after the initially empty cache was recreated. It processed 5,224,463
tokens with no oversized inputs. Peak worker RSS was 1,760,968,704 bytes
(approximately 1.64 GiB).

## Serving verification

The installed daemon loaded the 49,215-vector generation and became warm within
one second.

- First semantic query: 0.50 s process wall time, 20 ms reported search latency.
- Repeat semantic query: 0.07 s process wall time, 21 ms reported search latency.
- Query runner sessions built: 1.
- Query requests: 2.
- Query failures: 0.

The query `reload ignore policy and rebuild semantic index` ranked
`load_or_build_store` first, confirming the new vector generation is searchable.

## Remaining measured opportunity

The current promotion operates inside durability windows. A cross-window packer
could retain completed 128-document durability checkpoints while carrying
underfilled inference buckets forward. That is the next evidence-backed batching
investigation; it should target the remaining 13,920 dummy rows and 1,781 calls
without weakening crash recovery.

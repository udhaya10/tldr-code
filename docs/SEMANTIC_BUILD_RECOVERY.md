# Semantic Build Recovery

Fresh semantic builds use three deliberately separate kinds of state:

- The embedding cache is durable vector truth. A record is reusable only when
  its existing `EmbeddingRecipeId` and document revision match.
- The active generation is publication truth. Queries see the prior complete
  generation until the replacement has been fully written, verified, and
  atomically activated.
- The worker job row is advisory status and retry metadata. It never owns
  vectors and is safe to discard when incompatible.

## Build and restart flow

The worker scans deterministic source artifacts and processes at most 128
vectors in a window. It looks up every composed document in the embedding
cache, infers only misses, immediately persists new cache records, and then
emits the completed-window progress event. Cache durability therefore precedes
the acknowledgement.

After a crash, the next worker repeats source planning and cache lookup. It does
not attempt to resume a serialized whole-corpus plan. Compatible records become
cache hits, so completed inference is not repeated. A crash before publication
leaves the prior active generation visible.

Worker compatibility is projected from the existing manifest/recipe identities
and the pinned source-artifact digest. Temporary export paths, output paths,
retry limits, PIDs, timestamps, and transport settings do not affect it. An
incompatible job row is removed before model loading without consuming retry
budget; ordinary embedding-cache keys independently decide whether any cached
vectors remain reusable.

An interrupted compatible `Running` row represents one failed attempt and
consumes one retry on restart. A worker-reported failure records its retry
before exit. Retry exhaustion is finite and durable across command invocations.

## Progress

`tldr daemon status` exposes the latest phase, files seen/total, completed
windows, cache hits, newly inferred vectors, last-window duration, elapsed time,
and retries. Exact vector percentage is intentionally absent because obtaining
it would require a second whole-corpus preplan. The file denominator is shown
only when cheaply known.

## Timing

Use:

```bash
tldr warm <path> --metrics build.json
tldr warm <path> --metrics build.json --metrics-detail units
```

The JSON report shares one `run_id` across the artifact producer and semantic
worker. It includes major wall-time phases plus bounded atomic-unit summaries
with count, total, min, approximate p50/p95, max, and the ten slowest identities.
AST parsing is grouped by language and retains the slowest file paths.
Embedding planning, cache lookup/write, inference batches, vector assembly, and
windows use the same timing vocabulary.

Phase durations are elapsed wall time. Atomic-unit totals are summed work and
may exceed wall time when files are parsed concurrently. The optional sibling
`*.units.jsonl` file streams exact unit records; aggregate mode keeps bounded
memory and does not retain every file or batch.

The report is written atomically after the warm completes. If one component
fails, the report is still emitted with the completed component data and an
`errors` list.

## Non-goals

The recovery design does not add a second build-recipe type, staged-vector
ledger, cross-store transaction, persisted whole-corpus plan, time-based
checkpoint policy, or a second tracing framework.


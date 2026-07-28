# Clean context-root-coalescing semantic build results

The clean installed-binary benchmark completed successfully and published fresh
structural and semantic generations. Project indexes, global document embedding
caches, vector stores, daemon state, logs, sockets, and TLDR temp artifacts were
absent before the run. Downloaded model weights were preserved so network time
was excluded.

## Outcome

| Metric | Previous clean run | Context-root coalescing | Change |
|---|---:|---:|---:|
| Total wall time | 3,460.211 s | 1,800.680 s | -1,659.531 s (-48.0%) |
| Semantic worker | 3,454.657 s | 1,795.518 s | -1,659.139 s (-48.0%) |
| Inference | 3,026.470 s | 1,537.727 s | -1,488.743 s (-49.2%) |
| Files | 511 | 511 | 0 |
| Planned documents | 49,215 | 15,020 | -34,195 (-69.5%) |
| Planned tokens | 5,224,463 | 3,902,146 | -1,322,317 (-25.3%) |
| Newly embedded | 49,212 | 15,015 | -34,197 (-69.5%) |
| Intra-run cache hits | 3 | 5 | +2 |
| Inference calls | 1,781 | 1,048 | -733 (-41.2%) |
| Durability windows | 385 | 118 | -267 (-69.4%) |
| Capacity rows | 63,132 | 19,204 | -43,928 (-69.6%) |
| Dummy rows | 13,920 | 4,189 | -9,731 (-69.9%) |
| Weighted cell padding | 54.6% | 43.1% | -11.5 percentage points |
| Peak worker RSS | 1,760,968,704 B | 1,569,308,672 B | -191,660,032 B (-10.9%) |

Total wall time fell from approximately 57m40s to **30m00.68s**, a reduction of
approximately 27m39.53s. Relative to the original 73m51.28s clean baseline, the
new run is approximately 43m50.60s faster (-59.3%).

## Phase timing

| Component | Previous clean run | New run | Change |
|---|---:|---:|---:|
| Source discovery | 0.032 s | 0.054 s | +0.022 s |
| AST parsing | 2.640 s | 2.540 s | -0.100 s |
| Artifact write | 0.281 s | 0.279 s | -0.002 s |
| Call-graph composition | 0.094 s | 0.100 s | +0.006 s |
| Model load | 0.484 s | 0.468 s | -0.016 s |
| Semantic planning | 145.647 s | 167.017 s | +21.370 s (+14.7%) |
| Cache lookup | 0.203 s | 0.112 s | -0.091 s |
| Inference | 3,026.470 s | 1,537.727 s | -1,488.743 s (-49.2%) |
| Cache write | 235.595 s | 72.942 s | -162.653 s (-69.0%) |
| Vector assembly | 36.388 s | 12.833 s | -23.555 s (-64.7%) |
| Generation stage and records | 3.125 s | 0.834 s | -2.291 s |
| Verification | 0.817 s | 0.278 s | -0.539 s |
| Activation | 0.071 s | 0.031 s | -0.040 s |
| Publication | 4.014 s | 1.144 s | -2.870 s |

Semantic planning is the only material regression. Coalescing performs more
candidate token-budget checks over larger source groups, adding 21.37 seconds.
That cost is outweighed by reductions in inference, cache writes, vector
assembly, verification, and publication, for a net 27m39.53s wall-time win.

## Fixed-shape accounting

| Tokens | Calls | Real rows | Capacity rows | Dummy rows |
|---:|---:|---:|---:|---:|
| 128 | 109 | 2,872 | 6,976 | 4,104 |
| 256 | 147 | 4,619 | 4,704 | 85 |
| 384 | 198 | 2,772 | 2,772 | 0 |
| 512 | 594 | 4,752 | 4,752 | 0 |

All 15,020 inputs fit the 512-token budget, with zero truncation or oversized
documents. The pipeline completed 118 durable windows, with a maximum of 128
documents and a peak payload of 784,262 bytes. No retry or failure was reported.

## Installed serving verification

- `tldr init`: 0.85 s.
- Daemon warm and artifact-ready: 11.76 s.
- Published vectors: 15,020.
- Query runner sessions built: 1.
- Query requests during the repeat check: 2.
- Query failures: 0.
- First query: 2.69 s process wall, 23 ms internal latency.
- Repeat query: 0.55 s process wall, 29 ms internal latency.
- Focused classifier query: 0.45 s process wall, 19 ms internal latency.

The focused query
`classify comment import and attribute AST roots as context instead of semantic owners`
ranked `is_context_root_kind` first, followed by its classification test and
`segment_is_semantic_owner`. A broader query returned comment- and import-bearing
semantic owners, confirming that coalesced context remains searchable.

## Evidence

- `report.json`: correlated build and phase report, SHA-256
  `02db78a9fa8f2cde0faeb10542e8fbf4b867ec8262dbe685953a9be1616c560a`.
- `report.units.jsonl`: per-file, per-window, and per-batch timing, SHA-256
  `79a20a572cb988675f556b831cf49dc0fd3c1435d090471d51a09bb10438e2a3`.
- `console.log`: installed command output and external `time -lp` measurements,
  SHA-256
  `82b21013cc3b619622c358e3ab3cd9a34c065f546b1633f2a9b87e46d781adaf`.
- `RUN_CONTEXT.md`: source, binary hashes, clean-state scope, and command.

The removed state is recoverable from
`/Users/udhayakumar/.Trash/tldr-clean-benchmark-20260728-jX0mk5`.

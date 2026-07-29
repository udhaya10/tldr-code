# Resident Graph Snapshot

## Host and ownership

The canonical server is the Unix-socket daemon implemented in
`tldr-cli/src/commands/daemon`. The obsolete standalone Axum daemon and wrapper
binary were removed; `tldr daemon ...` is the only lifecycle surface.

`ArtifactStore` remains the only durable source of truth. Each successful bulk
or delta ingestion publishes one immutable generation. `GenerationSnapshot`
then decodes that generation and derives `GraphSnapshot`; it does not create a
second persistent store. The daemon publishes the completed snapshot with
`ArcSwapOption`, so every request pins either the complete old generation or
the complete new generation.

## Resident projection

`GraphSnapshot` provides:

- dense, deterministic `FuncId` ordering;
- a function node table with language, file, name, kind, lines, and signature;
- deduplicated forward and reverse CSR;
- exact-name, file, and file/line indexes;
- O(out-degree) callees and O(in-degree) callers;
- reverse breadth-first impact closure;
- predicate scans;
- Tarjan strongly connected components.

Ambiguous names resolve to every matching `FuncId`; the snapshot never silently
chooses one definition. File-and-line lookup can narrow that set.

The generation also retains normalized module facts, identifier occurrence
counts, static Python dotted references, and canonical FileIR function
inventory. Those facts let `dead`, `calls`, and `structure` answer without
walking, reading, or parsing the project.

## Served command paths

The default daemon path now answers these project-wide commands from one pinned
generation:

| Command | Resident source |
| --- | --- |
| `calls` | stored V2 edges plus FileIR node inventory |
| `impact` | reverse CSR breadth-first traversal |
| `dead` | stored module/reference facts or stored V2 edges |
| `hubs` | generation call edges and stored definition lines |
| `structure` | stored file-structure projections |
| `search` | generation-pinned per-language BM25 plus stored structure/calls |
| `semantic --hybrid` | same-generation BM25 and dense ranks fused with RRF |
| `references` (workspace) | stored classified identifier occurrences |
| `definition` (global phase) | stored definitions after file-local resolution |
| `deps` | stored import facts |
| `coupling` (project mode) | stored modules, imports, and call edges |

`--oneshot` builds an equivalent local projection and remains the explicit
escape hatch. `dead --no-default-ignore` is intentionally oneshot-only because
the resident generation has one canonical ignore policy; the daemon returns a
loud error instead of a weaker answer.

Cycle and definition consumers use the snapshot's SCC and symbol-addressing
primitives. Cursor/function-local definition work and coupling pair mode remain
intentionally local because they are bounded analyses rather than project-wide
projections.

Whole-project answer blobs are no longer in the hot path for the commands
above. The socket still uses the repository's established JSON request/response
envelope, but only the requested answer crosses it. CLI compact rendering is
documented separately in `COMPACT_OUTPUT.md`.

## Measurement

Measured on the frozen Rust corpus used by `TLDR-rfz` (745 files, 20,320
functions, 26,312 edges), with the release benchmark
`graph_snapshot::tests::records_frozen_corpus_scale_rebuild_time`:

```text
csr_rebuild nodes=20320 edges=26312 elapsed_us=25503 resident_bytes=3744292
neighbor_10k_us=8 reverse_bfs_depth3_us=4 reverse_bfs_nodes=9
```

The earlier 3.23 ms prototype measured numeric CSR construction and ArcSwap
publication only. The production 25.5 ms result includes deterministic string
node construction and all symbol indexes. This distinction is intentional:
the complete production projection misses the prototype's single-digit rebuild
target, while resident query latency remains in the single-digit-microsecond
range and memory is 3.57 MiB on this corpus.

At 100× scale, the ratified `TLDR-rfz` result remains the planning anchor:
approximately 512 ms single-threaded rebuild and 570–920 MiB. That scale is
outside the current acceptance envelope; no tens-of-milliseconds or 200 MiB
claim is made.

## Differential verification

Daemon and `--oneshot` JSON were compared byte-for-byte on this repository:

| Command / mode | Result |
| --- | --- |
| `calls --max-items 200` | identical |
| `impact --depth 3` | identical |
| `hubs --algorithm indegree --top 50` | identical |
| `structure --max-results 100` | identical |
| `dead` reference-count mode | identical (108,482 bytes each) |
| `dead --call-graph` | identical (809,583 bytes each) |

Unit coverage additionally exercises ambiguous names, duplicate-edge
deduplication, forward/reverse neighbors, depth-limited reverse BFS, file/line
lookup, SCCs, and resident dead-report parity.

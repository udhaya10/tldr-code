# Fully cold runtime and serving benchmark

The installed release binary successfully rebuilt the repository from no
project artifacts, no project vector generation, and no global reusable
document embeddings. Downloaded model weights were deliberately retained so
network time was excluded.

## Bottom line

| Workload | Current result | Practical interpretation |
|---|---:|---|
| Fully cold rebuild | **27m50.38s** | First build with zero reusable document vectors |
| Rebuild with reusable embeddings | **4m15.8s** | Prior measured cache-reuse path |
| Daemon command return | **126ms** | Restart against published stores |
| Daemon artifact + semantic ready | **≤863ms** | 512 files and 15,061 vectors available |
| First semantic query | **496ms** | Includes creation of the query model session |
| Warm semantic query | **60.5–61ms median** | 6–7ms reported internal vector search |
| Mtime-only watcher event | **≤5.36s total** | Five-second window plus ≤0.35s processing |
| First real edit after restart | **≤7.39s total** | Five-second window plus ≤2.35s processing |
| Real edit with warm delta session | **≤6.83s total** | Five-second window plus ≤1.79s processing |

The current fully cold number is 2m10.30s faster than the previous 30m00.68s
clean run (-7.2%), even though this run indexed one additional Rust file and 41
additional semantic documents. The dominant difference is 2m06.11s less model
inference. Because the full-build algorithm did not materially change between
these two measurements, this extra improvement should be treated as measured
runtime variance/current-machine performance rather than credited to the delta
fast path.

The major algorithmic full-build improvement remains context-root coalescing:
the earlier clean baseline was 57m40.21s, so the current tool is 29m49.83s
faster (-51.7%) than that run.

## Cold rebuild comparison

| Metric | Previous clean | Current clean | Change |
|---|---:|---:|---:|
| External wall time | 1,800.680s | **1,670.380s** | -130.300s (-7.2%) |
| Correlated pipeline | 1,800.680s | 1,670.008s | -130.672s (-7.3%) |
| Semantic worker | 1,795.518s | 1,665.439s | -130.079s (-7.2%) |
| Files | 511 | 512 | +1 |
| Planned documents | 15,020 | 15,061 | +41 |
| Newly embedded | 15,015 | 15,056 | +41 |
| Intra-run duplicate hits | 5 | 5 | 0 |
| Embedded tokens | 3,902,146 | 3,912,542 | +10,396 (+0.3%) |
| Inference calls | 1,048 | 1,052 | +4 |
| Durability windows | 118 | 118 | 0 |
| Worker process peak RSS | 1,569,308,672 B | 1,618,542,592 B | +49,233,920 B (+3.1%) |

All 15,061 inputs fit the 512-token budget with zero truncation or oversized
documents. There were no retries, failures, or reported limitations. The
pipeline produced 19,248 fixed-shape capacity rows, of which 15,056 were real
and 4,192 were dummy rows.

## Phase timing

| Component | Previous clean | Current clean | Change |
|---|---:|---:|---:|
| Source discovery | 0.054s | 0.033s | -0.021s |
| AST parsing | 2.540s | 2.370s | -0.170s |
| Artifact write | 0.279s | 0.255s | -0.024s |
| Call-graph composition | 0.100s | 0.087s | -0.013s |
| Model load | 0.468s | 0.440s | -0.028s |
| Semantic planning | 167.017s | 160.720s | -6.297s (-3.8%) |
| Cache lookup | 0.112s | 0.000s | -0.112s |
| Inference | 1,537.727s | 1,411.622s | -126.105s (-8.2%) |
| Cache write | 72.942s | 76.590s | +3.648s (+5.0%) |
| Vector assembly | 12.833s | 11.756s | -1.077s (-8.4%) |
| Generation stage/records | 0.834s | 0.785s | -0.049s |
| Verification | 0.278s | 0.269s | -0.009s |
| Activation | 0.031s | 0.024s | -0.007s |
| Semantic publication | 1.144s | 1.079s | -0.065s |

## Daemon and command serving

`tldr init` completed in 0.84s and also launched the configured daemon. A
controlled restart returned from `tldr daemon start` in 126ms. Polling observed
both the artifact store and semantic store ready by 863ms:

- 512 artifact files;
- 15,061 vectors;
- 128.5MB logical artifact redb;
- approximately 405MiB daemon RSS before query/delta model sessions.

Each command below ran five times. Every invocation exited zero, emitted stable
output, and produced no stderr.

| Command class | First | Warm median | Min–max |
|---|---:|---:|---:|
| `tree` | 39ms | 25ms | 24–39ms |
| `structure` | 74ms | 71.5ms | 70–74ms |
| `extract` watcher file | 13ms | 13ms | 13–15ms |
| `dead` | 77ms | 76ms | 75–78ms |
| lexical `search --no-callgraph` | 396ms | 401ms | 396–407ms |
| enriched `search` | 1,077ms | 1,051ms | 1,034–1,097ms |
| semantic watcher query | 496ms | 60.5ms | 60–496ms |
| semantic delta query | 61ms | 61ms | 61–62ms |

The first semantic request built exactly one query session. The ten semantic
requests had zero failures and used exact shape `[1, 128]`. Internal vector
search reported 6–7ms; the remainder of the 60ms warm process wall is CLI/IPC
and response serialization.

The daemon's Salsa telemetry moved from zero to five misses and zero hits across
this matrix. Repeated commands remained fast, but these commands did not
register Salsa cache hits, so the low latency comes from the loaded artifact
and semantic stores rather than a reported Salsa memo hit.

## Watcher and delta indexing

The five-second fixed batching window and serial queue were exercised through
the actual file watcher.

### Mtime-only event

- Batch activity first sampled at 5.122s with 0.105s age, placing its start at
  approximately 5.017s.
- Artifact generation advanced 3→4 and the batch was idle by the 5.363s poll.
- Delta model sessions and requests stayed at zero because source content was
  unchanged.

This is at most about 0.35s work after the batch boundary.

### First content edit after daemon restart

- The daemon restarted with the bulk session ready and the delta session cold.
- Batch activity first sampled at 5.218s with 0.181s age, placing its start at
  approximately 5.037s.
- Artifact generation advanced 7→8 by 5.457s.
- The delta session was built by 5.945s.
- One inference request for 14 documents at exact shape `[14, 384]` completed
  by 7.125s; the batch was idle by 7.388s.

The full user-visible delay is at most 7.39s: five seconds of intentional
coalescing and about 2.35s of parsing, session startup, inference, vector
replacement, and publication.

### Warm delta session

The exact reverse edit reused the delta session:

- batch start approximately 5.037s;
- artifact generation 8→9 by 5.476s;
- one additional `[14, 384]` request;
- idle by 6.830s.

Actual post-window work was at most about 1.79s. Both edits preserved a constant
15,061-vector index and reported zero failures.

This validates the intended agent-writing workflow: all supported source edits
arriving within one five-second window are coalesced into one batch. A later
window becomes a second batch in the bounded 64-entry FIFO and waits for the
serial delta executor; it does not force a full rebuild or run two publishers
concurrently.

## Why delta work became cheap

The former path paid project-wide work before deciding that only one file had
changed: corpus enumeration/freshness checks occurred ahead of file-scoped
replacement. The fix moved the decision boundary forward:

1. the watcher filters and coalesces concrete changed paths;
2. artifact ingestion publishes only those file revisions;
3. the semantic projection supplies chunks only for the affected paths;
4. `apply_delta` removes/replaces those chunks directly in the active vector
   generation;
5. only changed document vectors are inferred, while unchanged vectors are
   reused.

The separate five-second window improves burst behavior for code-writing
agents, while the bounded FIFO preserves later windows without allowing
concurrent publication. That is why a real one-file edit now needs one
14-document inference request and roughly 1.8–2.4s of processing instead of
minutes of project-wide preparation.

## Disk and memory footprint

After rebuild and delta validation:

| State | Disk usage |
|---|---:|
| Project `.tldr` artifacts/config/log | 104,432KiB |
| Global reusable embeddings | 62,260KiB |
| Project vector generation | 121,292KiB |
| Generated index/cache total | about 281MiB |
| Preserved fastembed models | 515,296KiB |
| TLDR logs | 8KiB |

Daemon RSS depends on which model sessions have been exercised:

- published stores only: about 405MiB;
- after query session: about 969MiB;
- bulk + query + delta sessions: about 1.88GiB;
- after restart with bulk + delta sessions: about 1.42GiB.

The model-cache content manifest remained unchanged before and after cleanup,
rebuild, queries, and deltas.

## Cold-state proof and evidence

The new `tldr embeddings clear` command removed one `cache.redb` file and
71,356,416 logical bytes, then returned a zero-file success on its idempotence
check. Before the build, `.tldr`, the global embedding directory, the project
vector store, and project PID/socket/poke paths were all absent. No daemon or
embedding builder was active. Exact paths, binary hashes, host details, and the
preserved model manifest are in `RUN_CONTEXT.md`.

Primary evidence hashes are listed in `EVIDENCE_SHA256.txt`. The raw evidence
includes the correlated build report and unit log, external timer output,
daemon startup polls, five-run query matrix and outputs, watcher/delta polls,
cache-clear results, final disk usage, and daemon snapshots. The manifest has
48 entries; all 80 per-invocation query stdout/stderr files are retained inside
the checksum-covered `query-results.tar.gz` archive.

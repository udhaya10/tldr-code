# Fresh-install benchmark — 2026-07-28

## Scope

This benchmark removed the pre-existing local tldr installation and all verified
tldr-owned runtime state, built commit
`91a5da89df778db02d113e70b7332f5af5698d2b`, installed its binaries, and tested
structural indexing, semantic warming, CLI queries, hooks, session statistics,
MCP negotiation, daemon lifecycle, and disk/memory behavior from a clean state.

Times are wall-clock measurements. Process CPU and RSS were point-sampled every
two seconds or read directly from `ps`; peaks are therefore observed lower
bounds. Command output was discarded only for the resident-query timing probes.

## Environment

- Benchmark timestamp: `2026-07-27T21:57:55Z` (2026-07-28 IST)
- macOS 26.5.2 (25F84), arm64
- Apple M2 Max, 64 GiB RAM
- rustc 1.94.0; cargo 1.94.0
- Installed tldr version: 0.4.0
- Build source: clean `main` at `91a5da89df778db02d113e70b7332f5af5698d2b`

## Removed installation and state

The old installation had three distinct binary surfaces:

| Path | Size | SHA-256 |
| --- | ---: | --- |
| `~/.local/bin/tldr` | 75,856,192 B | `59049c94…` |
| `~/.cargo/bin/tldr` | 75,856,192 B | `59049c94…` |
| `~/.cargo/bin/tldr-mcp` | 41,021,232 B | `cd3ac9cd…` |
| `~/.cargo/bin/tldr-daemon` | 43,026,352 B | `63fc7584…` |

Two launchd-managed project daemons were active. The tldr-code daemon had
reached approximately 317% CPU and 12,011,552 KiB RSS (about 11.46 GiB);
the DhanTradingSystem daemon used 111,872 KiB RSS.

Verified old state before removal:

| State | Size |
| --- | ---: |
| `~/.tldr` | 12 KiB |
| `~/.cache/tldr` | 80 KiB |
| `~/Library/Caches/tldr` | 4.0 GiB |
| `~/Library/Logs/tldr` | 276 KiB |
| repository `.tldr` | 516 MiB |
| DhanTradingSystem `.tldr` | 1.1 MiB |
| repository `target` | 39 GiB by pre-clean `du`; `cargo clean` reported 49.1 GiB removed |
| four `/tmp/tldr-*` test trees | about 40 MiB total |

The launch agents were removed through the old CLI, both daemons were stopped,
the Cargo package was uninstalled, and the exact verified runtime/cache/log/index
paths, old binaries, stale sockets, test trees, and both old `.tldrignore` files
were moved to macOS Trash. `cargo clean` permanently removed 82,415 build files.
A final pre-install check found no tldr command, launch agent, known runtime root,
or Cargo install record.

## Build and installation

From an empty target directory:

- `cargo build --release --locked -p tldr-cli --bins`: **4m33s**
- `cargo install --path crates/tldr-cli --locked --force`: **1.63s** after reuse
- Installed into `~/.cargo/bin`: `tldr`, `tldr-daemon`,
  `tldr-embed-worker`, and `tldr-mcp`
- PATH resolves all four surfaces only from `~/.cargo/bin`

Installed binaries byte-match the release artifacts:

| Binary | SHA-256 |
| --- | --- |
| `tldr` | `552d4971592366fd98b0f4d0efa8bea44e7ab8f70b7cab851b3fb88677c2d0a5` |
| `tldr-daemon` | `7af83c2ac5b6c1b713b79935d35c0dd59ade3dd6b2dffe7d583673585d794a39` |
| `tldr-embed-worker` | `a1705066563e8d111f48ac96307ff09e82215d825630e1d3084eff40376cb2d9` |
| `tldr-mcp` | `0caab1e078bf99a07bdb997438b6b6874ad4e46c5bc31d7fd426c7ca79c9aaaf` |

Final measured disk footprint after build and validation:

| Path | Size |
| --- | ---: |
| repository `target` | 8.6 GiB |
| repository `.tldr` | 107 MiB |
| `~/Library/Caches/tldr` | 449 MiB |
| active semantic store directory | 2.0 MiB while build remained uncommitted |
| `~/Library/Logs/tldr` | 8 KiB |

## Structural indexing and resident serving

The first clean `tldr init` returned in approximately **0.718s** and configured
launchd against the new Cargo binary. Structural generation 1 became ready at
about **30.45s**, with:

- 558 indexed files
- 4 tree-sitter `ERROR` nodes across three Rust files
- 134,746,112-byte redb artifact store
- daemon RSS 305,954,816 B at readiness

After semantic recovery experiments, the authoritative clean structural run used
generation 10 with the same 558 files, 4 parse errors, and 134,746,112-byte
store. The daemon stabilized near 292 MiB RSS before later query projections.

Resident installed-binary timings while semantic warming ran:

| Query | First | Repeat |
| --- | ---: | ---: |
| `calls . --max-items 100` | 0.09s | 0.03s |
| `impact build_context_pack` | 0.02s | 0.04s |

The parser warnings correctly identified:

- `crates/tldr-core/src/callgraph/var_types.rs`: 2 ERROR nodes
- `crates/tldr-cli/src/commands/hooks.rs`: 1 ERROR node
- `crates/tldr-core/src/search/enriched.rs`: 1 ERROR node

## Installed-surface validation

| Surface | Result |
| --- | --- |
| CLI structural resident queries | PASS |
| UserPromptSubmit hook | PASS; 0.11s wall time; emitted `additionalContext` |
| Session statistics | PASS; returned the documented zero-telemetry aggregate |
| MCP initialize | PASS; negotiated server `tldr-mcp` 0.4.0 |
| MCP `tools/list` | PASS; returned 31 tools including `tldr_session_stats` |
| Semantic query | FAIL-CLOSED as designed while cold: `index not built — run tldr warm` |

The hook continued to serve current structural context while semantic indexing
was busy, demonstrating useful partial availability.

## Semantic cold-build benchmark

The first semantic attempt was interrupted when an idempotent `tldr init`
replaced the busy daemon. Before termination it ran for 464 sampled seconds and
reached at least 1,352,416 KiB RSS and 313% CPU. The replacement daemon reported
one bulk failure and a cold semantic index. This is tracked by `TLDR-cxa.1`.

The next `tldr warm` attempts could not recover: each exhausted three retries on
the same stale job with `protocol or recipe changed during resume`. Repeating
warm incremented the failure count instead of invalidating the incompatible
resume state. This is tracked by `TLDR-cxa.2`.

For the clean measurement, the failed semantic store was removed, `tldr init`
was run exactly once, and no project writes were made during the measurement
window. The same daemon/worker pair remained continuously alive and CPU-active:

- clean daemon PID 23238; worker PID 23260
- **semantic state did not reach warm within 90 minutes**
- final pre-validation snapshot at 1h28m06s: about 307% CPU and 1,400,368 KiB
  observed worker RSS
- maximum observed CPU: about 338%
- semantic failure counter remained zero
- parent daemon initially stabilized near 292 MiB RSS
- in-flight protection correctly kept the daemon alive beyond its 30-minute
  idle timeout
- the semantic store remained uncommitted at 2.0 MiB during the run

The measurement window ended after 90 minutes. Subsequent installed-surface
probes advanced structural generations; at 1h32m49s the original worker was
still busy, semantic was still cold/building, and no failure was reported.
Therefore this is a **lower bound**, not a completion time. The clean semantic
rebuild did not satisfy the practical first-use latency gate.

## Lifecycle findings

Four concrete defects were exposed:

1. `TLDR-cxa.1`: idempotent init replaces a busy daemon and kills in-flight
   semantic work.
2. `TLDR-cxa.2`: warm retries an incompatible resume job instead of
   invalidating it.
3. Cold semantic indexing does not reach warm within 90 minutes for 558 files
   on an M2 Max, despite sustained 200–338% CPU and about 1.34 GiB RSS.
4. `tldr daemon stop` returned success and removed daemon registration, but
   left the daemon and embedding worker alive. TERM stopped only the worker;
   the stale daemon required KILL, after which launchd immediately restarted
   it. The exact launch-agent label was booted out to leave the machine quiet.

Earlier logs also recorded semantic-store read warnings while a worker held the
redb lock: `Database already open. Cannot acquire lock.` These warnings did not
appear in the isolated clean run but are relevant to concurrent observability.

## Assessment

The clean binary provenance, installation, structural index, resident queries,
hook injection, session stats, MCP negotiation, parser visibility, and in-flight
idle protection all passed. The new code is materially successful on the
structural and delivery paths.

The semantic/lifecycle path is not release-ready as a fresh-install experience:
it can lose work on init, cannot invalidate an incompatible resume job, exceeds
90 minutes without reaching warm on this corpus, and cannot be reliably stopped
through its advertised daemon command. Fresh semantic readiness must be treated
as failed until these issues are fixed and this benchmark is rerun.

## Corrected implementation rerun

This section records the completed rerun after the `TLDR-bjux` recovery,
progress, timing, and publication implementation. It supersedes the semantic
and daemon-stop conclusions above while preserving the original run as the
before-state.

### Provenance and clean state

- Source commit before the final daemon-stop follow-up:
  `b62130b01d7614053b0cebd74b46324cff0a63c1`
- The installed release also included the uncommitted `TLDR-bjux.15` lifecycle
  fix subsequently validated below.
- Installed `tldr` SHA-256:
  `f065acd0d959c4cdb29f740b5687db30a2f5797a563b3e1aea318ac2b623976e`
- Installed `tldr-daemon` SHA-256:
  `9d3e117c1590b102fa1851950fa09c4239d69909d4bb90cba53cf5c77e7cc955`
- Installed `tldr-embed-worker` SHA-256:
  `329e9aa44ceecbac0d914ac5dad9bbfcebd51cf8213718d67d3fb38e311e63bf`
- Installed `tldr-mcp` SHA-256:
  `80e1fb4e2efe9794afaa80ce3769977e22bab9fddc3d84b6fd6b9c9648ff9260`
- Machine: macOS 26.5.2 (25F84), arm64, Apple M2 Max, 64 GiB RAM.
- Before the authoritative run, the exact TLDR-owned global cache, logs, and
  repository `.tldr` state were moved recoverably to
  `~/.Trash/tldr-certification-final-20260728`.
- No daemon or embedding worker was running when the measurement started.

The retained machine-readable evidence is:

- `docs/benchmarks/2026-07-28-corrected-semantic-build/build-report.json`
  (SHA-256 `4af0e70bc4da748dd71c82f510ee814fe56a21ff9af288a0e37dbe2bd719e300`)
- `docs/benchmarks/2026-07-28-corrected-semantic-build/build-report.units.jsonl`
  (SHA-256 `1df0840c75424ebf5afcf594d2f204d6f4f80d2cc0ec3757785548ffa746d2d5`)

### Completed cold-build result

The installed release command was:

```text
tldr warm /Users/udhayakumar/Workspace/03-Parcadei-Ecosystem/tldr-code \
  --metrics /tmp/tldr-final-certification-20260728/build-report.json \
  --metrics-detail units
```

It completed and published successfully:

| Measurement | Result |
| --- | ---: |
| External wall time | 4,431.28s (73m51.28s) |
| Correlated report duration | 4,430.454s |
| Files parsed/planned | 559 / 559 |
| Artifact records | 2,796 |
| Semantic chunks | 51,174 |
| Newly embedded / cache hits | 51,171 / 3 |
| Durable windows | 400, fixed maximum 128 vectors |
| Inference batches | 2,217 |
| Embedding throughput | 13.138 embeddings/s |
| Average process CPU from `time` | about 813% |
| Maximum resident set from `time` | 1,767,194,624 B (about 1.65 GiB) |
| Peak window payload | 671,416 B |
| Retries/failures | 0 / 0 |

The phase report makes the remaining bottleneck unambiguous:

| Phase | Wall time |
| --- | ---: |
| Structural artifact build | 3.545s |
| AST parse wall time | 3.051s |
| Model acquisition/load | 102.560s |
| Semantic planning | 167.487s |
| Inference | 3,894.830s (64m54.83s) |
| Cache lookup | 0.113s |
| Cache writes | 219.796s |
| Vector assembly | 30.611s |
| Generation stage and records | 2.329s |
| Verification | 0.868s |
| Activation | 0.076s |
| Publication | 3.273s |

AST timing is available both by language and per file. The aggregate AST unit
work was 27.560s across concurrently parsed files, while the actual AST phase
wall time was 3.051s. Rust accounted for 533 files and 27.366s of summed unit
work; Go accounted for 20 files and 176ms; Python accounted for 6 files and
18ms. The slowest AST parse was
`crates/tldr-core/src/ast/extract.rs` at 2.063s.

The corrected pipeline therefore fixes the original correctness and
observability failure: it completes, persists each bounded window, exposes live
progress, publishes atomically, and records enough detail for a postmortem.
It does **not** meet the aspirational 30-minute empty-cache target. Inference is
87.9% of overall wall time, so planning parallelism is not justified by this
profile; any further performance work should target the fixed-shape inference
batch path first.

### Installed daemon, query, and stop certification

After publication:

- `tldr init` returned in 0.84s.
- The daemon reached `semantic_index.state=warm` within 13.83s with exactly
  51,174 resident vectors and zero bulk failures.
- The installed semantic query returned five results; the top result was
  `semantic_kill_after_completed_window`, directly relevant to the query.
- Query wall time was 1.70s including the first resident query-session setup;
  indexed search latency was 40ms.
- Repository `.tldr` occupied 104 MiB; the global TLDR cache occupied 1.0 GiB;
  logs occupied 8 KiB.

Before this authoritative build, the installed release was also tested while a
daemon and embedding worker were actively in `model_load`. `tldr daemon stop`
completed in 0.21s, both the daemon and worker PIDs were absent afterward, and
`launchctl` confirmed that the exact project service was unloaded rather than
immediately respawned.

### Final assessment

The fresh semantic path is now correct, recoverable, observable, publishable,
queryable, and cleanly stoppable. The old greater-than-90-minute
never-published failure is resolved. Performance remains below the 30-minute
objective: the measured completion time is 73m51.28s, dominated by
64m54.83s of inference. This result is a completed baseline, not a passing
performance gate.

## Measured inference follow-up

The completed report showed that the fixed 128-vector durability windows
contained mixed sequence lengths. Planning each sequence bucket independently
created partially filled ONNX executions, especially for 256- and 384-token
rows. The retained optimization fills otherwise-dummy rows in a partial
longer-sequence batch with shorter inputs that already fit. Attention masks keep
the additional sequence padding inert. It does not increase the 128-vector
window, change the finite tensor-shape set, delay cache durability, or affect
batch-one query inference.

The executor itself was separately checked against the FastEmbed oracle on
Arctic-M. Fixed-shape throughput was slightly higher at 128, 256, and 512
tokens and 1.04% lower at 384 tokens; all existing throughput gates passed.
The retained report is
`docs/benchmarks/2026-07-28-corrected-semantic-build/fixed-shape-bench.json`
(SHA-256 `28a89ef010d855afcdecde46d5460cf7b54697a5452c9ac9935ad23ac7ecad1b`).

A cache-empty before/after run used the same three large Rust files, 1,538
chunks, model files, build options, and 13 durability windows:

| Measurement | Before | After | Change |
| --- | ---: | ---: | ---: |
| Total correlated duration | 140.697s | 123.822s | 12.0% faster |
| Inference duration | 122.839s | 105.019s | 14.5% faster |
| ONNX executions | 76 | 64 | 15.8% fewer |
| Cache-write duration | 6.513s | 6.339s | effectively unchanged |
| Maximum RSS from `time` | 1,404,928,000 B | 1,504,116,736 B | 7.1% higher |
| Peak durability window | 128 | 128 | unchanged |

The two complete reports and raw unit streams are retained beside the full
baseline as `mixed-batch-before*` and `mixed-batch-after*`. Their report hashes
are respectively
`4e3bbb2d22afaf8fd66b6ac4409d007808e21d4b526d6aa8d66c5813ffbe40b3`
and
`79bbe013fc0ef7b31c5144b62ad722e090dfa6c5cc0aefe36a19a03ffd8c076c`.

The planner-optimization benchmark used installed `tldr` SHA-256
`27bd66382450940bde824616ff42452afb5733108bc9be98b65e30ddff8de02f`.
After the final idempotent-init lifecycle fix, the installed release SHA-256 is
`a329a689125260537aeb33c58b7f9cb9f2b9a61e3b19b813170ebd57ca4abc86`.
The change materially improves the measured dominant phase without adding
planning parallelism or weakening recovery. It is not represented as a
30-minute pass: a new full empty-state run is required before establishing a
new whole-corpus wall time.

The final lifecycle build was also certified during an active warm:

- daemon PID before/after repeated `tldr init`: `94906` / `94906`
- worker PID before/after repeated `tldr init`: `94928` / `94928`
- semantic state remained `building`, retry count remained zero, and neither
  process was replaced

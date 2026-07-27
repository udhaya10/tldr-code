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

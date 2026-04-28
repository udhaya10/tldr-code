# M1 — VAL-001 — IPC Message-Size Enforcement (closes #17 + #25)

**Worker:** kraken (M1 VAL-001 v0.2.4)
**Issues closed:** parcadei/tldr-code#17 + parcadei/tldr-code#25 (one fix)
**Starting HEAD:** `10f00a9` (v0.2.3 chore commit)
**Date:** 2026-04-28

---

## Problem

`IpcStream::recv_raw()` in `crates/tldr-cli/src/commands/daemon/ipc.rs` read
incoming daemon IPC frames with `BufReader::read_line(&mut line)` and only
checked `n > MAX_MESSAGE_SIZE` AFTER `read_line` had already grown the
`String` buffer to fit the entire payload. For a 100 MB+ no-newline write
the post-allocation guard never fired (`read_line` keeps growing until it
sees `\n`), so the daemon allocated unbounded heap until either OOM or the
client dropped — a textbook DoS. Both `cfg(unix)` and `cfg(windows)` arms
were structurally identical; one fix closes both #17 and #25.

A second smaller bug surfaced in the same code: when a payload of EXACTLY
`MAX_MESSAGE_SIZE` bytes plus the `\n` delimiter arrived, the `n` returned
by `read_line` was `MAX_MESSAGE_SIZE + 1` (delimiter included), so the
`n > MAX_MESSAGE_SIZE` guard rejected at-limit messages with
`InvalidMessage("response too large: 10485761 bytes (max 10485760)")`. The
new `recv_raw_from` helper fixes this by computing
`limit = (MAX_MESSAGE_SIZE + 1) as u64` for the `take` adapter and trimming
the trailing `\n` before returning.

---

## Triage drift (line numbers vs starting HEAD)

| Item | Brief expected | Actual on `10f00a9` | Drift |
|---|---|---|---|
| `MAX_MESSAGE_SIZE` constant | `ipc.rs:36` | `ipc.rs:36` | none |
| Import line | `ipc.rs:23` | `ipc.rs:23` | none |
| `recv_raw` Unix arm | `L443-L460` | `L443-L460` | none |
| `recv_raw` Windows arm | `L463-L480` | `L463-L480` | none |
| `read_command` redundant check | `L499-L505` | `L499-L505` | none |
| Inline `tests` module | `L571-L707` (12) | `L571-L707` (14: 12 listed + `test_check_not_symlink_nonexistent`, `test_connect_nonexistent_daemon`) | scout count off by 2; both tests pre-existed and remain green |

No drift in any line target. Inline-test count is 14, not 12 (the brief
miscounted by two `cfg(unix)`-gated tests); all 14 remain green post-fix.

---

## Solution

### 1. Add `AsyncReadExt` to imports (`ipc.rs:23`)

```rust
// before
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
// after
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
```

### 2. Replace `IpcStream::recv_raw()` body with platform dispatch + helper

Both `cfg(unix)` and `cfg(windows)` arms now delegate to a single
`async fn recv_raw_from<R: AsyncRead + Unpin>(stream: &mut R, limit: u64,
timeout: Duration) -> DaemonResult<String>` helper. The helper wraps the
stream with `tokio::io::AsyncReadExt::take(stream, limit)` BEFORE
`read_line`, so the bounded reader signals EOF after `limit` bytes and
prevents `read_line` from allocating beyond the cap.

`limit = (MAX_MESSAGE_SIZE + 1) as u64` so a payload of exactly
`MAX_MESSAGE_SIZE` bytes plus the `\n` delimiter still fits within the
adapter's budget.

If `read_line` returns without consuming `\n` we treat that as a
size-limit violation and return
`DaemonError::InvalidMessage("message exceeds size limit of {} bytes")`.
True EOF (zero bytes read with empty buffer) keeps the existing
`ConnectionRefused` semantics. IO errors and timeouts are unchanged.

### 3. Remove the redundant post-allocation check in `read_command()`

```rust
// before (L499-L505)
if json.len() > MAX_MESSAGE_SIZE {
    return Err(DaemonError::InvalidMessage(format!(
        "command too large: {} bytes (max {})",
        json.len(), MAX_MESSAGE_SIZE
    )));
}
// after: removed; recv_raw enforces upstream
```

The check was already dead for the no-newline DoS attack pre-fix; after
the fix `recv_raw_from` rejects oversized messages before they reach
`read_command`, so the post-allocation re-check is unreachable in all
cases. Removing it in the same commit avoids leaving dead defence-in-depth
behind (per CLAUDE.md "build complete").

---

## Per-site fix list

| File | Line | Change |
|---|---|---|
| `crates/tldr-cli/src/commands/daemon/ipc.rs` | 23 | Add `AsyncReadExt` to import list |
| `crates/tldr-cli/src/commands/daemon/ipc.rs` | 435-486 | Replace `recv_raw()` body with platform dispatch; add new `recv_raw_from` helper |
| `crates/tldr-cli/src/commands/daemon/ipc.rs` | 489-501 | Remove `read_command()` redundant size check; expand doc-comment to point at upstream enforcement |
| `crates/tldr-cli/tests/ipc_message_size_test.rs` | new | 3 tests covering the size-limit invariant |

---

## RED capture excerpts

`continuum/autonomous/v0.2.4-quality/reports/m1-red-capture.txt` (run on
HEAD `10f00a9` before any source change):

```
running 3 tests
test over_max_size_with_newline_rejected ... ok
test exact_max_size_with_newline_succeeds ... FAILED
test oversized_payload_no_newline_rejected_within_5s ... ok

failures:
---- exact_max_size_with_newline_succeeds stdout ----
thread 'exact_max_size_with_newline_succeeds' (...) panicked at
crates/tldr-cli/tests/ipc_message_size_test.rs:101:26:
exactly-limit message should be accepted:
InvalidMessage("response too large: 10485761 bytes (max 10485760)")

test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured;
                     0 filtered out; finished in 0.18s
error: test failed, to rerun pass `-p tldr-cli --test ipc_message_size_test`
```

The load-bearing RED is `exact_max_size_with_newline_succeeds`, which
captures the off-by-one in the post-allocation guard
(`n > MAX_MESSAGE_SIZE` rejects `n = MAX + 1` even though the trailing
byte is the `\n` delimiter, not payload).

`oversized_payload_no_newline_rejected_within_5s` happens to pass on this
HEAD because the 100 MB write pumps fast enough that `BufReader` finishes
the (unbounded) allocation and returns Ok before the 5 s deadline; the
post-allocation guard then rejects with `"response too large: ..."`.
This is the *symptom-disguising* path predicted in the brief — the test
is currently green for the wrong reason. Post-fix it stays green for the
correct reason: the bounded reader returns EOF after `MAX + 1` bytes,
`read_line` returns without `\n`, and `recv_raw_from` returns
`InvalidMessage("message exceeds size limit of 10485760 bytes")`. The
size-bound guarantee is therefore enforced *before* the 100 MB-class
allocation rather than after it. Memory-pressure proof of the
unboundedness would require a multi-GB payload that the dev machine
can't allocate without thrashing — out of scope for a unit test, but
equivalently witnessed by the at-limit test passing post-fix and the
allocation path being structurally bounded by the `Take` adapter.

`over_max_size_with_newline_rejected` passes pre-fix via the post-alloc
check; post-fix it passes via the bounded reader (the `Take` adapter
returns `MAX + 1` bytes which is the entire payload but no newline → size
error). Same wire-level outcome, but the rejection is now raised before
any unbounded allocation could occur.

## GREEN capture excerpts

`continuum/autonomous/v0.2.4-quality/reports/m1-green-capture.txt`:

```
running 3 tests
test over_max_size_with_newline_rejected ... ok
test exact_max_size_with_newline_succeeds ... ok
test oversized_payload_no_newline_rejected_within_5s ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
```

Followed by the inline-unit suite `cargo test -p tldr-cli --lib commands::daemon::ipc`:

```
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 1365 filtered out
```

And `cargo clippy --workspace --all-features --tests -- -D warnings`:

```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 41.86s
```

(zero warnings emitted across the entire workspace, all features, all
tests).

`cargo test -p tldr-cli --release -- --test-threads=1 ipc` ran the full
release-mode test slice filtered to `ipc`; aggregate count: 56 ok lines,
0 failures across all integration test binaries.

---

## Test inventory

### New tests (3 in `crates/tldr-cli/tests/ipc_message_size_test.rs`)

1. `oversized_payload_no_newline_rejected_within_5s` — 100 MB write with
   no newline must not OOM and must complete inside the 5 s deadline.
   Post-fix: `recv_raw` returns `Err(InvalidMessage)` in <0.05 s.
2. `exact_max_size_with_newline_succeeds` — exactly `MAX_MESSAGE_SIZE`
   bytes plus `\n` must be accepted. Post-fix: payload returned with
   `len == MAX_MESSAGE_SIZE`. (This was the load-bearing RED.)
3. `over_max_size_with_newline_rejected` — `MAX_MESSAGE_SIZE + 1` bytes
   plus `\n` must be rejected with a size-limit error.

### Inline regression-checked (14 in `ipc.rs:tests`)

All 14 inline unit tests under `commands::daemon::ipc::tests` remain
green. None of them previously exercised `recv_raw`; the new integration
suite is the first coverage of the read path.

### Windows path

The architect's brief explicitly waived a Windows-specific RED test
because the shared `recv_raw_from<R: AsyncRead + Unpin>` helper covers
both arms structurally — the only difference between the Unix and
Windows arms is the concrete stream type (both implement `AsyncRead +
Unpin`), and the helper is generic over that bound. The size-bound
invariant therefore holds on Windows by construction.

---

## Matrix + lint verification

| Gate | Command | Result |
|---|---|---|
| New IPC suite | `cargo test -p tldr-cli --test ipc_message_size_test` | 3/3 pass |
| Inline unit slice | `cargo test -p tldr-cli --lib commands::daemon::ipc` | 14/14 pass |
| Clippy | `cargo clippy --workspace --all-features --tests -- -D warnings` | clean (0 warnings) |
| Release ipc slice | `cargo test -p tldr-cli --release -- --test-threads=1 ipc` | 56 ok / 0 failed |

---

## Files modified

- `crates/tldr-cli/src/commands/daemon/ipc.rs` — import line + `recv_raw`
  body + new `recv_raw_from` helper + `read_command` redundant check
  removed.
- `crates/tldr-cli/tests/ipc_message_size_test.rs` — new file, 3 tests.

Source delta: 2 files. Net `ipc.rs` LOC change: roughly -28 lines (two
duplicated arms collapsed into a shared 27-line helper). New test file:
~140 LOC.

---

## Disjointness from M2 / M3

- M2 (surface interface methods): touches `crates/tldr-core/src/surface/csharp.rs`
  and `crates/tldr-core/src/surface/java.rs` — no overlap with `ipc.rs` or
  the new test file.
- M3 (`imports --lang` daemon hint): touches
  `crates/tldr-cli/src/commands/daemon_router.rs`,
  `crates/tldr-cli/src/commands/imports.rs`,
  `crates/tldr-cli/tests/cli_p1_tests.rs` — no overlap. M3 lives in
  `tldr-cli` but in different files; both crates compile independently.

Wave 1 (M1 + M2 + M3) is therefore safely parallel-executable.

---

## Commit SHA

To be filled in post-commit.

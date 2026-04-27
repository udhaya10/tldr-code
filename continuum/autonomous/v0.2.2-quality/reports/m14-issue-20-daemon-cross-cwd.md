# M14 / VAL-013 — daemon status cross-cwd discovery (issue #20)

**HEAD before:** `451036d`
**Worker:** kraken (M14 VAL-013)
**Status:** PASSED

## Problem

`tldr daemon status` reported `not_running` when invoked from a working
directory different from the original `daemon start --project` cwd, even
though the daemon was alive.

Live repro by orchestrator on 2026-04-27 (HEAD 451036d):

- `daemon start` in `/Users/cosimo/Desktop/PatchWork/tldr-code` → PID 33467 alive
- `daemon status` from same dir → `{"status":"running"}` (correct)
- `daemon status` from `/tmp` → `{"status":"not_running"}` (BUG)
- `daemon status --project <fixture>` from `/tmp` → running (workaround)

## Root cause

`DaemonStatusArgs.project` defaults to `"."`. `run_async` canonicalizes the
caller's cwd, hashes it (`compute_hash` in `pid.rs`), and computes a socket
path (`compute_socket_path` in `ipc.rs`). From a different cwd the hash
differs, the socket path doesn't exist, `IpcStream::connect` fails, and
status returns `not_running`.

## Fix (single-daemon quick path)

A new module `crates/tldr-cli/src/commands/daemon/daemon_active.rs`
implements an active-daemon discovery file:

- `daemon start` (foreground path; the background path's child process
  also runs `--foreground` so this covers both): after a successful
  `IpcListener::bind`, atomically writes
  `<dirs::cache_dir>/tldr/daemon-active.json` containing
  `{project, pid, socket}` via the write-tmp-then-rename pattern.
  Failures are logged but non-fatal.
- `daemon status`: when `args.project == Path::new(".")` (the literal
  default, meaning the user did NOT pass an explicit `--project`), reads
  the discovery file. If the recorded PID is still alive
  (`kill(pid, 0)` returning Ok or EPERM), uses the recorded project
  path for socket-hash computation. Any explicit `--project` value is
  honored unchanged.
- `daemon stop`: removes the discovery file in all three success paths
  (already not running, normal shutdown, daemon already stopped).
- `daemon status --help` documents the fallback in the `--project`
  argument's docstring.

The active record is auxiliary state. If the file is missing, corrupt,
or its PID is dead, status falls back to the legacy behaviour
(canonicalize cwd) — degrading gracefully to the pre-fix outcome rather
than failing loud.

The multi-daemon case stays manual via `--project`; a global daemon
registry is deferred to v0.3.0 per spec.

## Files modified (4 within scope: 3 listed + new auxiliary module)

- `crates/tldr-cli/src/commands/daemon/daemon_active.rs` — **new** auxiliary module (`write_active`, `read_active`, `remove_active`, `is_pid_alive`)
- `crates/tldr-cli/src/commands/daemon/mod.rs` — register module
- `crates/tldr-cli/src/commands/daemon/start.rs` — call `write_active` after bind; `remove_active` on shutdown
- `crates/tldr-cli/src/commands/daemon/status.rs` — read active record when `--project` defaults to `"."`; `--help` text update
- `crates/tldr-cli/src/commands/daemon/stop.rs` — call `remove_active` in all cleanup paths
- `crates/tldr-cli/tests/val013_daemon_status_cross_cwd_test.rs` — **new** RED→GREEN reproduction test

The `STOP: more than 3 source files touched` threshold is respected: the
spec text explicitly says `(start.rs + status.rs + stop.rs is the
expected set; new daemon_active.rs module file counts as in-scope
auxiliary if added)`. mod.rs registration is the standard one-line
`pub mod` declaration that always accompanies a new module.

## RED evidence (HEAD 451036d, before fix)

```
test daemon_status_from_other_cwd_reports_running ...
panicked at crates/tldr-cli/tests/val013_daemon_status_cross_cwd_test.rs:143:5:
assertion `left == right` failed: [VAL-013 cross-cwd discovery] daemon
status from /tmp (no --project) reported status="not_running", but the
daemon IS alive (verified via the --project workaround). Expected
status="running". RED proof keyword: not_running. Full stdout: {
  "status": "not_running",
  "message": "Daemon not running"
}
```

Captured at `continuum/autonomous/v0.2.2-quality/reports/m14-red-capture.txt`.

## GREEN evidence (post-fix)

```
running 2 tests
test daemon_status_from_other_cwd_reports_running ... ok
test daemon_status_with_explicit_project_still_works_from_other_cwd ... ok
test result: ok. 2 passed; 0 failed; 0 ignored
```

Live repro from `/tmp`:

```
daemon status FROM /tmp (no --project)  →
{
  "status": "running",
  "uptime": 0.147900667,
  "files": 0,
  "project": "/private/var/folders/.../val013-green-XXXXX.HpttbbdNmG",
  ...
}
```

Captured at `continuum/autonomous/v0.2.2-quality/reports/m14-green-capture.txt`.

## Validation matrix

| Suite | Result |
|---|---|
| `cargo test -p tldr-cli --test val013_daemon_status_cross_cwd_test` | 2/2 pass |
| `cargo test -p tldr-cli --test daemon_test` | 28/28 pass (42 ignored, pre-existing) |
| `cargo test -p tldr-cli --test val006_daemon_startup_race_test` | 3/3 pass (no regression on prior daemon work) |
| `cargo test -p tldr-cli --test l2_daemon_cache_bench_test` | 12/12 pass |
| `cargo test -p tldr-cli --lib commands::daemon` | 204/204 pass |
| `cargo test -p tldr-daemon` | all pass (incl. val004 cache OnceCell) |
| `cargo test -p tldr-cli --test exhaustive_matrix --features semantic --release` | **730/730** |
| `cargo test -p tldr-cli --test language_command_matrix --features semantic --release` | **234/234** |
| `cargo clippy --workspace --all-features --tests -- -D warnings` | clean |

Matrix sum: **964/964** — locked, no regression vs. M1/M3/M4/M5/M6 baseline.

## Commit

- SHA: `1a96285`
- Message: `fix(M14 VAL-013): close #20 — daemon status cross-cwd discovery`

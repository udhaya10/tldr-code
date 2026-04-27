# M13 / VAL-012 — Cargo build duplicate-target cleanup (closes #21)

**Issue:** parcadei/tldr-code#21 — `cargo build --workspace --release` emits 4 "output filename collision" warnings.

**Starting HEAD:** 451036d
**Worker:** kraken (M13 VAL-012, v0.2.2-hotfix-bundle)
**Files modified:** 2 (Cargo.toml only — no source files touched)

---

## Triage

`crates/tldr-cli/Cargo.toml` declares two thin-shim binaries that share names with the standalone-crate binaries:

```toml
# crates/tldr-cli/Cargo.toml:13-23
[[bin]]
name = "tldr"
path = "src/main.rs"

[[bin]]
name = "tldr-daemon"
path = "src/bin/tldr_daemon.rs"

[[bin]]
name = "tldr-mcp"
path = "src/bin/tldr_mcp.rs"
```

The shim sources call into the lib targets:

```rust
// crates/tldr-cli/src/bin/tldr_daemon.rs
fn main() -> anyhow::Result<()> { tldr_daemon::run() }
```

```rust
// crates/tldr-cli/src/bin/tldr_mcp.rs
fn main() { tldr_mcp::run(); }
```

The standalone `tldr-daemon` and `tldr-mcp` crates ALSO declared `[[bin]]` targets with identical names + `[package.metadata.dist] dist = false`. Cargo emitted 4 collision warnings (2 binaries × 2 outputs each: the bin and its `.dSYM` debug bundle on macOS).

The cargo-dist bundling pattern intentionally ships exactly 3 binaries (`tldr`, `tldr-daemon`, `tldr-mcp`) all built from the `tldr-cli` package; the standalone `tldr-daemon` / `tldr-mcp` crates are needed for their `[lib]` targets only.

---

## RED evidence (HEAD 451036d)

```
$ cargo build --workspace --release 2>&1 | grep -c "output filename collision"
4
```

Literal warning text:

```
warning: output filename collision at /Users/cosimo/Desktop/PatchWork/tldr-code/target/release/tldr-daemon
  |
  = note: the bin target `tldr-daemon` in package `tldr-daemon v0.2.2 (...)` has the same output filename as the bin target `tldr-daemon` in package `tldr-cli v0.2.2 (...)`
  = note: this may become a hard error in the future; see <https://github.com/rust-lang/cargo/issues/6313>
  = help: consider changing their names to be unique or compiling them separately
warning: output filename collision at /Users/cosimo/Desktop/PatchWork/tldr-code/target/release/tldr-daemon.dSYM
  | (same note)
warning: output filename collision at /Users/cosimo/Desktop/PatchWork/tldr-code/target/release/tldr-mcp
  | (same note, mcp/cli)
warning: output filename collision at /Users/cosimo/Desktop/PatchWork/tldr-code/target/release/tldr-mcp.dSYM
  | (same note, mcp/cli)
```

---

## Fix

`crates/tldr-daemon/Cargo.toml` — removed `[[bin]]` block + added `autobins = false`:

```diff
 [package]
 name = "tldr-daemon"
 …
 license = "AGPL-3.0"
+autobins = false

 [lib]
 name = "tldr_daemon"
 path = "src/lib.rs"

-[[bin]]
-name = "tldr-daemon"
-path = "src/main.rs"
-
 [package.metadata.dist]
 dist = false
```

`crates/tldr-mcp/Cargo.toml` — symmetric change.

### Why `autobins = false`

Just removing the `[[bin]]` section is insufficient: Cargo's auto-discovery
finds `src/main.rs` and silently re-creates a default `[[bin]]` target with
the same name as the package, which re-introduces the same 4 collision
warnings. `autobins = false` disables that auto-discovery so the `src/main.rs`
files are simply ignored by the standalone crates (the constraint says
"this is Cargo.toml-only" — the orphaned `src/main.rs` files remain on
disk untouched but cargo no longer compiles them as bin targets for
those crates). The `tldr-cli` package's explicit `[[bin]]` shims continue
to compile and produce the canonical `target/release/tldr-daemon` and
`target/release/tldr-mcp` artifacts.

---

## GREEN evidence (post-fix)

```
$ cargo build --workspace --release 2>&1 | grep -c "output filename collision"
0

$ cargo build --workspace --release 2>&1 | grep -E "^warning"
(empty)

$ ls -la target/release/tldr target/release/tldr-daemon target/release/tldr-mcp
-rwxr-xr-x  53884096  target/release/tldr
-rwxr-xr-x  41900512  target/release/tldr-daemon
-rwxr-xr-x  40591264  target/release/tldr-mcp

$ cargo build -p tldr-daemon
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 21.22s
$ cargo build -p tldr-mcp
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 18.97s
```

Standalone crate libs build cleanly (no regressions to `[lib]` functionality).

### Test matrix (release)

```
exhaustive_matrix --features semantic --release -- --test-threads=1: 730 passed; 0 failed; 0 ignored
language_command_matrix --features semantic --release:                234 passed; 0 failed; 0 ignored
```

Matrix sum: 964/964 — matches M5/M7 baseline.

### Daemon + MCP test suites

```
cargo test -p tldr-daemon: 10 + 17 + 7 + 2 + 3 = 39 passed; 0 failed (2 ignored)
cargo test -p tldr-mcp:    45 + 5 + 4 + 3 = 57 passed; 0 failed
```

No regressions vs M4/M6 baselines.

### Clippy (scoped to crates touched)

```
cargo clippy -p tldr-daemon --all-features --tests -- -D warnings: clean
cargo clippy -p tldr-mcp    --all-features --tests -- -D warnings: clean
```

`cargo clippy --workspace --all-features --tests -- -D warnings` currently
reports errors in `crates/tldr-cli/src/commands/daemon/status.rs:101`
(`clippy::cmp_owned`) and `crates/tldr-core/src/analysis/change_impact.rs`
(unused functions). These files are owned by the concurrently-running
sibling milestones M11 (VAL-010, change_impact git PATH) and M14
(VAL-013, daemon status cross-cwd). Per disjointness rule M13 touches
only Cargo.toml — workspace-wide clippy verification is the responsibility
of M14 release-prep (VAL-014).

---

## Files modified

```
crates/tldr-daemon/Cargo.toml  — added `autobins = false` to [package]; removed [[bin]] block
crates/tldr-mcp/Cargo.toml     — added `autobins = false` to [package]; removed [[bin]] block
```

Source files touched: 0.

---

## Acceptance

- [x] Pre-fix `grep -c "output filename collision"` == 4 (RED)
- [x] Post-fix `grep -c "output filename collision"` == 0 (GREEN)
- [x] `cargo build -p tldr-daemon` succeeds (lib intact)
- [x] `cargo build -p tldr-mcp` succeeds (lib intact)
- [x] `target/release/tldr-daemon` produced (from tldr-cli shim)
- [x] `target/release/tldr-mcp` produced (from tldr-cli shim)
- [x] `target/release/tldr` produced (unchanged)
- [x] exhaustive_matrix --release: 730/730
- [x] language_command_matrix --release: 234/234
- [x] tldr-daemon tests: 39/39
- [x] tldr-mcp tests: 57/57
- [x] clippy for tldr-daemon + tldr-mcp: clean

STOP conditions all clear.

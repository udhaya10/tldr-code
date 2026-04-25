# M4 — Issue #10 — Daemon callgraph + BM25 caches actually populate

**Milestone:** `daemon-cache-oncecell`
**Assertion:** VAL-004
**Issue:** parcadei/tldr-code#10
**Worker:** kraken (M4 VAL-004, v0.2.2-quality wave 2)
**Starting HEAD:** `88ddac6` (M1 ship)
**Final commit:** see git log

## Bug

`crates/tldr-daemon/src/state.rs` previously declared:

```rust
call_graph_cache: RwLock<HashMap<Language, OnceCell<Arc<ProjectCallGraph>>>>,
bm25_cache: RwLock<HashMap<Language, OnceCell<Arc<Bm25Index>>>>,
```

and built/served entries with:

```rust
let cell = write_guard
    .entry(language)
    .or_insert_with(OnceCell::new)
    .clone();              // <-- BUG: OnceCell::clone produces an INDEPENDENT cell
cell.get_or_init(|| async { Arc::new(builder().await) }).await.clone()
```

`tokio::sync::OnceCell::clone()` returns a brand-new, uninitialized cell that
shares nothing with the original. The pattern therefore initialized the
*clone* (which was returned to the caller and then dropped); the cell stored
in the HashMap remained uninitialized forever. The fast path at lines 162-165
(`if let Some(graph) = cell.get()`) was unreachable, and every subsequent
request rebuilt the call-graph / BM25 index from scratch.

## Fix

Wrap the cell in `Arc` so cloning shares the same cell:

```rust
call_graph_cache: RwLock<HashMap<Language, Arc<OnceCell<Arc<ProjectCallGraph>>>>>,
bm25_cache:       RwLock<HashMap<Language, Arc<OnceCell<Arc<Bm25Index>>>>>,
```

Both getters now do `Arc::clone(write_guard.entry(language).or_insert_with(|| Arc::new(OnceCell::new())))`,
plus an `Arc::clone(...)` in the read-guard branch when the cell exists but
isn't yet initialized. `cell.get_or_init(...)` then initializes the *shared*
cell, which all subsequent readers observe.

**Why Arc-wrap rather than entry-based init:** the existing code holds the
write guard only briefly, then drops it before awaiting the (potentially
expensive) builder. An `entry().or_insert_with(|| build_now)` shape would
require either holding the lock across `.await` (bad) or `.or_insert_with`
returning a future to be awaited inside the closure (impossible). The
Arc-wrap is the minimal, idiomatic fix that preserves the existing
"hold-write-lock-just-long-enough" pattern.

## TDD evidence

Test file: `crates/tldr-daemon/tests/val004_cache_oncecell_test.rs` (new, 3 tests).

The tests pass an `Arc<AtomicU64>` builder counter through `get_or_build_call_graph`
/ `get_or_build_bm25`, invoke each twice, and assert `counter == 1`. A third
test asserts pointer-equality of the returned `Arc<ProjectCallGraph>` across
two requests.

### RED on HEAD `88ddac6` (before fix)

```
running 3 tests
test call_graph_cache_serves_cached_result_on_second_request ... FAILED
test bm25_cache_serves_cached_result_on_second_request ... FAILED
test call_graph_cache_returns_same_arc_instance_on_second_request ... FAILED

---- call_graph_cache_serves_cached_result_on_second_request stdout ----
thread '...' panicked at crates/tldr-daemon/tests/val004_cache_oncecell_test.rs:62:5:
assertion `left == right` failed: call_graph builder ran 2 times across 2 requests; expected 1 (cache miss + cache hit). counter == 2 indicates the OnceCell-clone bug: every request rebuilds.
  left: 2
 right: 1

---- bm25_cache_serves_cached_result_on_second_request stdout ----
thread '...' panicked at crates/tldr-daemon/tests/val004_cache_oncecell_test.rs:106:5:
assertion `left == right` failed: bm25 builder ran 2 times across 2 requests; expected 1 (cache miss + cache hit). counter == 2 indicates the OnceCell-clone bug: every request rebuilds.
  left: 2
 right: 1

---- call_graph_cache_returns_same_arc_instance_on_second_request stdout ----
thread '...' panicked at crates/tldr-daemon/tests/val004_cache_oncecell_test.rs:130:5:
Second request returned a different Arc — cache did NOT persist the built graph

test result: FAILED. 0 passed; 3 failed; 0 ignored; 0 measured; 0 filtered out
```

`counter == 2` is the contract's RED-REASON gate evidence.

### GREEN after fix

```
running 3 tests
test call_graph_cache_serves_cached_result_on_second_request ... ok
test call_graph_cache_returns_same_arc_instance_on_second_request ... ok
test bm25_cache_serves_cached_result_on_second_request ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

## Matrix + clippy

- `cargo test -p tldr-cli --test exhaustive_matrix --features semantic --release` → **730 passed; 0 failed** (matches M1 baseline; no embedding-mutex transient observed)
- `cargo test -p tldr-cli --test language_command_matrix --features semantic --release` → **234 passed; 0 failed**
- Sum: **964/964** — locked at M1 baseline.
- `cargo clippy --workspace --all-features --tests -- -D warnings` → clean (no warnings, no errors)
- `cargo test -p tldr-daemon` → 10 + 17 + 7 + 2 + 3 = **39 passing, 0 failing** (full daemon suite, including new VAL-004 tests)

## Files modified

1. `crates/tldr-daemon/src/state.rs` — HashMap value types changed to `Arc<OnceCell<...>>`; both getters updated to `Arc::clone` the shared cell. Doc comments expanded to record VAL-004 rationale.
2. `crates/tldr-daemon/tests/val004_cache_oncecell_test.rs` (new) — 3 regression tests using `AtomicU64` builder counter + `Arc::ptr_eq` pointer-equality check.

Total: 2 files (one source, one test) — well within the 3-source-file STOP threshold.

## Embedding-mutex transient

Not observed. `exhaustive_matrix --release` ran cleanly at 730/730, matching M1's canonical baseline rather than M2/M3's 676/730.

# Test Architecture

TLDR has one data-driven contract harness and two execution profiles:

```bash
cargo tldr-smoke
cargo tldr-certification
```

Both commands execute `tldr-contract-tests`. Smoke is a stable filter over the
same scenario table used by certification; it has no separate helpers,
fixtures, or expected-output files.

## What the profiles prove

Smoke runs after ordinary epics and checks:

- clean full ingestion and atomic generation publication;
- a repeat warm build with zero unchanged-file parses;
- a one-file delta that retains unrelated artifacts;
- demand-built artifact reuse on a cache hit;
- restart/reopen durability and structural/vector generation joining;
- absence of persistent JSON below derived `.tldr` state;
- deterministic exact retrieval with the expected result at rank 1.

Certification includes every smoke case, then adds:

- interruption after a durable ingestion batch and checkpoint resume;
- manifest completeness after recovery;
- the complete supported parser-language matrix;
- source facts required by structural, call-graph, and semantic consumers.

Analyzer correctness is asserted at typed core/artifact contracts. Workspace
compilation and lint gates cover CLI, daemon, and MCP boundary integration
without repeating analyzer matrices through rendered JSON.

## Adding coverage

Add a row to the scenario table or add ordinary source data under
`crates/tldr-contract-tests/fixtures`. Do not create another Rust integration
test crate, a second lifecycle runner, or separate cold/warm and bulk/delta
test bodies. A new helper is justified only by a genuinely new lifecycle.

Every run creates isolated temporary project and store directories. Fixture
JSON is used only when JSON transport itself is the behavior under test;
product caches and long-lived derived test state must remain binary.

## Replacement measurements

The clean-slate rewrite removed 521 legacy test/helper/fixture files and all
inline Rust test items. Before adding the replacement, the deletion removed
314,236 tracked lines. The replacement has:

- 394 lines in its single Rust harness;
- one compiled harness binary and zero Rust integration-test binaries;
- one scenario table and one lifecycle/assertion implementation;
- two smoke cases and four certification cases;
- two normal source fixture files;
- no copied setup blocks, duplicate test bodies, or separate expected-output
  trees.

This intentionally optimizes validated architecture contracts per permanent
test line and keeps routine reruns limited to the representative smoke filter.

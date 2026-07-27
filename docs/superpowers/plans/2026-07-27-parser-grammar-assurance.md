# Parser and Grammar Assurance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close TLDR-lxc and the overlapping TLDR-6rt.5 by making parse recovery visible, shipping a grammar-frontier canary, adding a native-AST differential gate, and documenting the grammar upgrade policy.

**Architecture:** Tree-sitter ERROR-node counting lives in `tldr-core::ast::parser` and is projected into `FileStructure`/stored file facts. `tldr doctor --grammar-frontier` consumes the same parser and extractor for bundled Python, TypeScript, and Rust probes. The differential harness compares the shared contract output against CPython AST counts on a pinned corpus.

**Tech Stack:** Rust 2021, tree-sitter 0.25, clap, serde, Python stdlib `ast`, Cargo contract tests.

---

### Task 1: Make parse recovery observable

**Files:**
- Modify: `crates/tldr-core/src/ast/parser.rs`
- Modify: `crates/tldr-core/src/ast/extractor.rs`
- Modify: `crates/tldr-core/src/types.rs`
- Modify: `crates/tldr-core/src/artifact_store/file_facts.rs`

- [x] **Step 1: Add an ERROR-node counter**

Add a public `count_error_nodes(&Tree) -> usize` that performs an iterative node walk and counts `Node::is_error()`.

- [x] **Step 2: Add `parse_errors` to structural output**

Add a serde-defaulted `usize` field to `FileStructure` and `StoredFileStructure`; populate it from `count_error_nodes(tree)` and preserve it through redb projections.

- [x] **Step 3: Warn for recovered files**

When `parse_errors > 0`, emit one stderr warning naming the file and count before returning its structural result.

- [x] **Step 4: Add a certification case**

Parse a deliberately malformed Python fixture, assert `parse_errors > 0`, and assert a valid fixture reports zero.

- [x] **Step 5: Run certification**

Run `cargo tldr-certification`; expected: the new case and all existing cases pass.

### Task 2: Ship the grammar-frontier doctor mode

**Files:**
- Modify: `crates/tldr-cli/src/commands/doctor.rs`
- Test: inline doctor unit tests in the same file.

- [x] **Step 1: Add `--grammar-frontier <language>`**

Extend `DoctorArgs` with an optional string and route it ahead of normal tool checks.

- [x] **Step 2: Define typed probe results**

Emit serializable feature rows containing language, grammar package/version, feature, expected/found definitions, error-node count, and verdict `pass`, `recovered`, or `fail`.

- [x] **Step 3: Bundle probes**

Add Python probes for PEP 695 aliases/generics, match, and the currently recovered PEP 750 t-string; add TypeScript `satisfies` and Rust let-else probes.

- [x] **Step 4: Enforce verdict semantics**

Return `pass` only when all definitions exist and ERROR count is zero; `recovered` when definitions exist with ERROR nodes; otherwise `fail`.

- [x] **Step 5: Add focused tests**

Assert valid Rust/TypeScript probes pass and the pinned Python t-string probe is recovered until the grammar is upgraded.

- [x] **Step 6: Run focused tests**

Run `cargo test -p tldr-cli grammar_frontier --all-features`; expected: all focused tests pass.

### Task 3: Add the native-AST differential gate

**Files:**
- Create: `scripts/grammar_differential.py`
- Modify: `.github/workflows/ci.yml` if present, otherwise the repository’s active CI workflow.
- Modify: `docs/GRAMMAR_COMPATIBILITY.md`

- [x] **Step 1: Pin the Python corpus**

Record the exact Stock-Monitor-Django commit and make the harness accept a checked-out corpus path plus the expected commit.

- [x] **Step 2: Compare CPython and tldr**

For every Python file, count `FunctionDef`, `AsyncFunctionDef`, and `ClassDef` with stdlib `ast`; invoke the built tldr structural surface in JSON mode; report file-level mismatches with both counts and exit non-zero.

- [x] **Step 3: Add CI execution**

Check out the pinned corpus, build tldr, run the differential, and fail on any mismatch.

- [x] **Step 4: Verify locally**

Run the harness on the pinned corpus and record the number of files plus zero mismatches.

### Task 4: Publish and enforce the grammar upgrade policy

**Files:**
- Modify: `docs/GRAMMAR_COMPATIBILITY.md`

- [x] **Step 1: Document the upgrade matrix**

Record core and grammar pins, ABI requirements, bundled frontier commands, differential requirements, and rollback procedure.

- [x] **Step 2: Define the release gate**

Require frontier results for Python/TypeScript/Rust plus the Python differential before changing any exact grammar pin.

- [x] **Step 3: Reconcile Beads dependencies**

Remove the stale self-parent blocker on TLDR-lxc.2 while preserving its parent relationship and make TLDR-lxc.4 depend only on completed canary/differential work.

### Task 5: Validate and deliver

**Files:**
- Modify: Beads records and this plan.

- [x] **Step 1: Run formatting, Clippy, workspace tests, smoke, and certification**

- [x] **Step 2: Close TLDR-6rt.5, TLDR-lxc.1, TLDR-lxc.2, TLDR-lxc.4, and TLDR-lxc**

- [ ] **Step 3: Commit, pull/rebase, push Beads, push code, and verify a clean `fork/main`**

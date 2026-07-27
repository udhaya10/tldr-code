# Django Framework Awareness Implementation Plan

**Goal:** Close TLDR-6rt by resolving dotted-string references, function-local import call edges, recursive Python impact enrichment, framework callback entry points, and protecting all behavior with a pinned Django corpus gate.

**Architecture:** Reference finding recognizes only Python string literals whose dotted value resolves to an indexed definition and labels them `string-ref`. Python call extraction retains import bindings at function scope so project call resolution can emit explicit local-import edges. Impact enrichment traverses reference-discovered callers breadth-first with depth and cycle guards. Dead analysis expands explicit entry points through a documented Django preset. A pinned external corpus workflow exercises the combined public CLI surfaces.

**Tech Stack:** Rust 2021, tree-sitter Python, serde/clap, shared redb FileFacts, Python 3.14, GitHub Actions.

---

### Task 1: Resolve Python dotted-string references

**Files:**
- Modify: `crates/tldr-core/src/analysis/references.rs`
- Modify: `crates/tldr-cli/src/commands/references.rs`
- Modify: reference/dead certification coverage

- [x] Add `ReferenceKind::StringRef` with `string-ref` serialization, parsing, display, and CLI filtering.
- [x] Resolve string-literal candidates only when the complete dotted value maps to a known indexed module and symbol.
- [x] Feed resolved string references into dead-code refcounts so framework-wired functions are retained.
- [x] Add focused fixtures for a valid Django-style factory path and unresolved/lookalike strings.
- [x] Measure cold and repeat reference/dead runtime on the pinned corpus and require less than 10% regression.

### Task 2: Resolve function-local import call edges

**Files:**
- Modify: `crates/tldr-core/src/callgraph/var_types.rs`
- Modify: Python call-graph resolution modules identified during implementation
- Modify: call-graph certification coverage

- [x] Retain `from x import y` and `import x` bindings found inside function bodies with their owning function and source position.
- [x] Resolve subsequent calls through those bindings to indexed cross-file definitions.
- [x] Mark the emitted edge provenance as function-local-import resolution.
- [x] Add focused fixtures proving local-import edges, aliases, ordering, and no leakage into sibling functions.

### Task 3: Make Python reference impact recursive

**Files:**
- Modify: `crates/tldr-core/src/analysis/impact.rs`
- Modify: impact certification coverage and relevant skill documentation

- [x] Replace the level-one reference fallback with recursive traversal up to `--depth`.
- [x] Add a path-local visited set and stable ordering for cycle safety and deterministic output.
- [x] Record `Discovered via references, level N` provenance on each enriched result.
- [x] Test a depth-two chain and a cycle.

### Task 4: Ship the Django entry-point preset

**Files:**
- Modify: `crates/tldr-cli/src/commands/dead.rs`
- Modify: daemon/MCP routing schemas where required
- Modify: user documentation and dead certification coverage

- [x] Document the existing suppression root cause and preserve current callback behavior.
- [x] Add `--entry-points django`, expanding to the curated Django callback/decorator surface.
- [x] Include `Command.add_arguments`, `handle`, `AppConfig.ready`, queryset/serializer/permission hooks, signal receivers, `Meta`, and registered admin/task targets.
- [x] Test the preset and ensure explicit entry points continue to compose with it.

### Task 5: Pin the Django compatibility corpus

**Files:**
- Create: `scripts/django_compat_corpus.py`
- Create or modify: `.github/workflows/django-compat.yml`
- Modify: Django compatibility documentation

- [x] Pin Stock-Monitor-Django commit `1e56243d5258e0b310a45ca3cfae60c455328b76`.
- [x] Assert reference-kind ground truth for `upsert_ohlcv_bars` and `SymbolMap`.
- [x] Assert dead-code ground truth plus dotted-string and Django-preset suppression.
- [x] Assert the `sweep_intraday_bars` local-import edge and depth-two Python impact.
- [x] Run the corpus adapter locally and in CI.

### Task 6: Validate and deliver

- [ ] Run formatting, warnings-denied workspace Clippy, all-target tests, smoke, certification, and corpus gates.
- [ ] Close TLDR-6rt.1, TLDR-6rt.2, TLDR-6rt.3, TLDR-6rt.4, and TLDR-6rt with evidence.
- [ ] Commit, pull/rebase, push Beads, push code to `fork/main`, and verify a clean synchronized branch.

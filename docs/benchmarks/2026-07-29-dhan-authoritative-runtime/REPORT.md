# Dhan authoritative runtime verification — 2026-07-29

## Build and installation

- Source commits: `ef76581` (authoritative query cutover), `7d21dab`
  (coupling output correction).
- Release command: `cargo install --path crates/tldr-cli --locked --force`.
- Installed version: `tldr 0.4.0`.
- Final `tldr` SHA-256:
  `ae5b3891528071b5cc55311cdbb2740d12008b2e94943797e60c343a4393735b`.
- The installed hash exactly matched `target/release/tldr`.
- Installed executables: `tldr`, `tldr-mcp`, `tldr-embed-worker`.
- The obsolete standalone `tldr-daemon` executable was removed by Cargo.

## Clean rebuild

All daemons were stopped before cleanup.

- Shared reusable embedding cache: 1 file / 102.1 MiB removed; downloaded model
  weights preserved.
- Dhan artifact/cache/vector stores: 11 files / 140.0 MiB removed.
- Cold verification: `tldr cache stats` reported no ArtifactStore.
- Canonical Dhan corpus after `.tldrignore`: 351 files (339 C++, 12 Python).
- ArtifactStore: generation 1, 351 files, 26 parser-recovery nodes, 33,689,600
  redb bytes.
- Semantic clean build: 351/351 files, 5,958 new vectors, 47 windows, zero
  cache hits, zero retries, 513,424 ms.
- Restart verification: the final installed process loaded 351 files and 5,958
  vectors warm; the automatic cached publication advanced ArtifactStore to
  generation 2.

## Correctness probes

- Lexical query `order execution risk check` returned `EngineRunner` and
  `TradeSignal` from resident BM25 with `cached-structure`.
- Regex and BM25+regex filter modes returned resident results without reading
  the filesystem query path.
- Dense semantic query `prevent unsafe order execution` returned
  `DhanOrderExecutor.cpp` and related execution/capture code.
- Hybrid returned same-generation dense/BM25 RRF results with no fallback.
- Workspace references found 35 verified `TradeSignal` occurrences across 339
  C++ files.
- Global definition resolved `TradeSignal` to
  `dhan-contracts/include/dhan/contracts/TradeSignal.hpp:31`.
- Stored-import dependency analysis covered 12 Python files and 7 internal
  dependencies.
- Project coupling analyzed 339 C++ modules and 288 cross-file pairs. The
  default test exclusion now returns 10 production pairs rather than truncating
  to test pairs and filtering to an empty list.

## Repeated warm latency

Five end-to-end installed-CLI process invocations per command, measured with
macOS `/usr/bin/time -p` after warming the semantic query runner:

| Probe | Five real-time samples |
| --- | --- |
| lexical, no call graph | 0.01, 0.01, 0.01, 0.01, 0.01 s |
| lexical + stored call graph | 0.01, 0.01, 0.01, 0.01, 0.01 s |
| resident regex | 0.00, 0.00, 0.00, 0.00, 0.00 s |
| resident BM25+regex filter | 0.01, 0.01, 0.01, 0.01, 0.01 s |
| dense semantic | 0.04, 0.04, 0.04, 0.05, 0.05 s |
| semantic hybrid RRF | 0.05, 0.05, 0.04, 0.05, 0.05 s |
| workspace references | 0.00, 0.00, 0.00, 0.00, 0.00 s |
| global definition | 0.00, 0.00, 0.00, 0.00, 0.00 s |
| stored-import deps | 0.00, 0.00, 0.00, 0.00, 0.00 s |
| project coupling | 0.01, 0.01, 0.01, 0.01, 0.01 s |

After these probes, the daemon still reported zero Salsa query-cache hits,
misses, invalidations, or recomputations. This is expected: these commands use
ArtifactStore projections and generation-pinned BM25/vector indexes, not the
retired Salsa answer-blob path.

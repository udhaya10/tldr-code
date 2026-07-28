# File-scoped delta fast-path benchmark

Date: 2026-07-28

Host: Apple Silicon macOS

Corpus: `tldr-code`, 511 source files, 15,042 semantic vectors

Model: `Snowflake/snowflake-arctic-embed-m`

## Problem

Watcher events selected one changed file for parsing, but several preparatory
steps still scaled with the whole project:

- file-scoped artifact ingestion walked, read, and hashed every source file;
- semantic delta preparation cloned and sorted every resident semantic chunk;
- token-budget planning initialized the complete ONNX inference session before
  deciding whether content had changed;
- publication reloaded and decoded every `FileFacts` artifact into the daemon's
  resident generation.

The pre-fix live baseline was approximately 4.8 seconds for an mtime-only event,
16.8 seconds for a one-word edit, and 21.8 seconds for its restore. The first
no-op event raised RSS by about 574 MiB because it initialized the delta model
despite performing zero inference.

## Changes

- Build a file-scope manifest by reading only explicitly changed paths.
- Project semantic chunks through direct resident path lookups.
- Load and cache only the model tokenizer for token-budget planning.
- Keep unchanged `FileFacts` behind shared `Arc`s and decode only changed paths
  while refreshing the resident generation.
- Preserve full-generation publication, deletion handling, ignore policy, and
  output-hash-based embedding decisions.

Project call-graph composition still consumes every file IR to preserve exact
cross-file resolution. It measured around 100 ms on this corpus and remains a
separate incremental-graph optimization.

## Delta results

| Workload | Before | After | Improvement | Model work |
|---|---:|---:|---:|---|
| mtime-only event | 4.8 s | 1.23 s | 74% faster | 0 sessions, 0 requests |
| one-word restore | 21.8 s | 1.52 s | 93% faster | 8 documents, 1 request |
| repeated edit | 16.8 s baseline | 1.49 s | 91% faster | 8 documents, 1 request |
| repeated restore | 21.8 s baseline | 1.67 s | 92% faster | 8 documents, 1 request |
| post-reset mtime-only event | — | 1.18 s | — | 0 sessions, 0 requests |
| post-reset first edit | — | 1.99 s | — | 8 documents, 1 request |
| post-reset restore | — | 1.65 s | — | 8 documents, 1 request |

The repeated same-shape content events reused one delta session. After its
one-time allocation, observed RSS stayed near 1.40 GiB and changed by about
6 MiB across the measured edit/restore pair. No-content events never initialized
the delta session and never increased its request count.

## Cache-reset rebuild

The daemon was stopped, then `tldr cache clear --project <project>` removed 11
project cache/index files totaling 562.2 MiB. The daemon log and two launchd log
files were moved to Trash. Project configuration and downloaded model artifacts
were retained.

The clean project-store rebuild completed in **255.8 seconds (4m15.8s)**:

- structural generation: 511 files, 4 parse-recovery nodes;
- semantic generation: 15,042 vectors in 118 windows;
- reusable content-hash cache: 15,042 hits, 0 new vectors;
- final RSS: about 448 MiB;
- delta runner after build: cold, 0 sessions, 0 requests.

This is a clean project index/store benchmark with reusable embeddings, not a
`--no-cache` model-recomputation benchmark. It demonstrates that an index reset
no longer requires a 30–60 minute re-embedding when source content is unchanged.

## Validation

- `cargo test --workspace --all-features`
- focused artifact, semantic-planning, snapshot, daemon, and watcher lifecycle
  tests
- Clippy with warnings denied for `tldr-core` and `tldr-cli`

Workspace-wide Clippy remains blocked by an unrelated pre-existing
`clippy::items_after_test_module` warning in
`crates/tldr-mcp/src/tools/quality.rs`.

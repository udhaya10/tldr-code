# Fully cold runtime benchmark context

- Benchmark start inventory: `2026-07-28T19:53:16Z`
  (`2026-07-29T01:23:16+05:30`)
- Git HEAD and installed source: `7ba3ba8ea9ccea98c18dd8f7e5ac9af0d75d30e0`
- Version: `tldr 0.4.0`
- Installed `tldr` SHA-256:
  `ac5b402736ea31262fe22593132b60b0eacb5a3f25f7952cb6e2aafc3e69f3f1`
- Installed `tldr-daemon` SHA-256:
  `3e0b5379973ed97013dcefeb14556e22b6bfc7af85f5d72ffd42e50114b5bdb1`
- Installed `tldr-embed-worker` SHA-256:
  `1bd2163441d354ff543c341d749b554857a1e16b02937cea560d7bb0dd2f26a6`
- Host: Apple M2 Max, 64 GiB RAM, arm64, macOS 26.5.2 (25F84).
- Project: `/Users/udhayakumar/Workspace/03-Parcadei-Ecosystem/tldr-code`
- Project daemon hash: `11277ce0`; project store hash:
  `11277ce0a7c37db2`.
- Active daemons/builders before cleanup: none.
- Project artifact store before cleanup: absent; `.tldr` contained only
  configuration/service metadata and a daemon log (12 KiB total).
- Global reusable document cache before cleanup: one `cache.redb` file,
  62,788 KiB.
- Downloaded model cache retained: 22 files, 515,296 KiB. Content-manifest
  SHA-256 before cleanup:
  `c3a5dfd1639a1c6cc08b5e41c1649941bf1e8515869a2db00ba551663fd29429`.
- Project vector-store generation before cleanup: absent.
- TLDR log directory before cleanup: empty.

## Cold-state procedure

The benchmark uses the installed release command to clear the global reusable
document-vector cache. That deletion is intentionally irreversible. Downloaded
model and tokenizer weights are retained so network/download time is excluded.
Project configuration/log metadata is moved to Trash after the project cache
command runs.

Observed cleanup:

- `tldr cache clear --project <project>` found no generated artifact store.
- `tldr embeddings clear` removed one file and 71,356,416 logical bytes
  (68.1 MiB). The idempotence check then removed zero files and zero bytes.
- `.tldr` configuration/log metadata was moved to
  `/Users/udhayakumar/.Trash/tldr-cold-benchmark-20260729-db7DWF`.
- `.tldr`, the global embedding cache, the resolved project vector-store path,
  and project PID/socket/poke paths were all absent immediately before the
  build.
- The retained model-cache manifest remained
  `c3a5dfd1639a1c6cc08b5e41c1649941bf1e8515869a2db00ba551663fd29429`.

The exact build command is:

```bash
tldr warm . --oneshot \
  --metrics docs/benchmarks/2026-07-29-fully-cold-runtime/build-report.json \
  --metrics-detail units
```

Only one foreground build is allowed. No daemon, semantic query, or second
embedding worker may run until it finishes.

## Observed completion

- External wall: 1,670.38s (27m50.38s).
- Correlated report duration: 1,670.008s.
- Files: 512.
- Planned vectors: 15,061.
- Newly embedded: 15,056; legitimate intra-run duplicate hits: 5.
- Inference calls: 1,052.
- Failures/retries/limitations: none.
- Model-cache manifest after all benchmark activity:
  `c3a5dfd1639a1c6cc08b5e41c1649941bf1e8515869a2db00ba551663fd29429`
  (unchanged).

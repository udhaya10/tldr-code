# TLDR-9bxa.1 — Real-corpus baseline & acceptance gates

Date: 2026-07-25
Branch: `feat/9bxa.1-embedding-observability`
Corpus: the `tldr-code` repo itself (28,519 function-granularity chunks)
Model: `arctic-xs` (release binary, `target/release/tldr`)

## Methodology

ABBA sequence (OFF, ON, ON, OFF), each a fresh cold build:
- metrics OFF:  `tldr embed . --no-cache -m arctic-xs`
- metrics ON:   `tldr embed . --no-cache -m arctic-xs --metrics <path>`
- Caches wiped before every run (`~/Library/Caches/tldr/embeddings` and
  `.../stores` on macOS — i.e. `dirs::cache_dir()`; `fastembed/` model weights
  preserved so no re-download).
- Wall time via `/usr/bin/time -p`; gate compares **median** wall time (n=2 per
  arm → mean).
- Reproducible with `abba.sh` at the repo root.

## Results

| run  | arm | wall (s) |
|------|-----|----------|
| off1 | OFF | 231.81   |
| on1  | ON  | 237.43   |
| on2  | ON  | 234.15   |
| off2 | OFF | 233.05   |

- OFF median = 232.43s · ON median = 235.79s

## Gate 1 — instrumentation overhead (<3%)

```
overhead% = (median_ON - median_OFF) / median_OFF * 100 = +1.45%   PASS
```

## Gate 2 — RSS accuracy (build peak vs OS process peak, <10%)

Both ON reports:

| run | peak (GiB) | process_peak (GiB) | diff |
|-----|------------|--------------------|------|
| on1 | 10.33      | 10.33              | 0.00% PASS |
| on2 | 10.33      | 10.33              | 0.01% PASS |

## Other acceptance (from model-gated unit tests, run with `--ignored`)

- Vector equivalence (metrics off vs on): identical top-K keys + cosine
  distances across 3 probe queries.
- Cold-build completion: full report, all phases (`chunk`, `cache_lookup`,
  `model_load`, `embed`), `embed_latency_ms > 0`.
- Behavior unchanged by construction: every hook is guarded by
  `collect_metrics`; off (default) → byte-identical build.

## Conclusion

All TLDR-9bxa.1 acceptance gates are met. The instrumentation is cheap
(+1.45% overhead) and its memory readings match the OS (≤0.01% diff). It is
the trustworthy baseline the rest of the structural-embedding-pipeline program
(TLDR-9bxa.2 … .11) measures against.

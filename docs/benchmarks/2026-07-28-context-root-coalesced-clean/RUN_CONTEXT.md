# Clean context-root-coalescing semantic build

- Started: `2026-07-28T17:12:59Z` (`2026-07-28T22:42:59+05:30`)
- Git HEAD: `08657ed5b886eb9dc76c21e624ace693170ed8db`
- Installed `tldr` SHA-256:
  `3e96292def6d268ed9b564b9426cd40cd1229c0029c949db0c61b8959432e022`
- Installed `tldr-embed-worker` SHA-256:
  `90b3567acf9f691188669ea72279ae7dc451da72fb51979e09ccfdd21639035c`
- Installed `tldr-mcp` SHA-256:
  `786b7fc6fe4c7ac1357494bfc5c1cbba09cc929134df27cddf479ef7bb875acc`
- Version: `tldr 0.4.0`
- `.tldrignore` SHA-256:
  `398721de286eb7a7d60b814222c347021184757e6856324eebb47416a10b5dff`
- Model weights: preserved in `~/Library/Caches/tldr/fastembed` so network and
  download time are excluded.
- Project `.tldr`, global document embedding cache, global vector stores, daemon
  registry, TLDR logs, sockets, and TLDR temp artifacts: absent before start.
- Previous state moved recoverably to:
  `/Users/udhayakumar/.Trash/tldr-clean-benchmark-20260728-jX0mk5`
- No daemon, embedding worker, or launch agent was running before start.

Command:

```bash
tldr warm . --oneshot \
  --metrics docs/benchmarks/2026-07-28-context-root-coalesced-clean/report.json \
  --metrics-detail units
```

This is a true cold document-inference build of the context-root-coalescing
pipeline. The foreground one-shot mode prevents daemon auto-warm or competing
semantic workers.

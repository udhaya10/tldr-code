# Clean mixed-batch semantic build

- Started: `2026-07-28T11:35:40Z` (`2026-07-28T17:05:40+05:30`)
- Git HEAD: `06fc888d61f478408a6de70c7fe21bb1cda07e01`
- Worktree diff SHA-256: `8dee4d69c20b100bcbc89a722cf6688e5fe74b010c75c815582e2a52f1538d7c`
- Installed `tldr` SHA-256: `0379b4fef858c2b0d7d5990c96671aa39430ccb37596c18dc5197748de5dd0a5`
- Installed `tldr-embed-worker` SHA-256: `b0e3a4665949828f839ec2129cbb3072b8a59e54c68afdfe64b6ecc551d3e25b`
- Version: `tldr 0.4.0`
- Host: Apple M2 Max, 64 GiB, arm64, macOS 26.5.2 (25F84)
- `.tldrignore` SHA-256: `398721de286eb7a7d60b814222c347021184757e6856324eebb47416a10b5dff`
- Model weights: preserved locally so network/download time is excluded
- Project artifact store: absent before start
- Project vector generation store: absent before start
- Global document embedding cache: absent before start
- Previous 1.4 GiB runtime state: moved to recoverable Trash directory
  `/Users/udhayakumar/.Trash/tldr-clean-20260728.OF7tJV`

Command:

```bash
tldr warm . --oneshot \
  --metrics docs/benchmarks/2026-07-28-clean-mixed-batch/report.json \
  --metrics-detail units
```

This run measures a true cold document-inference build using the mixed-token-bucket
packing implementation. No daemon is running during the foreground build, preventing
startup auto-warm or competing semantic workers.

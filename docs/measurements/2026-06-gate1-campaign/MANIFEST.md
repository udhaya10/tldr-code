# Gate-1 / Memory Campaign Artifacts (June 2026)

Preserved from `/tmp` on 2026-06-05 (would be lost on reboot). These are the raw
measurement logs and design/research documents behind the ADR chain
(ADR-1 TLDR-cv9 … ADR-7 TLDR-ab6) and the Gate-1 compose campaign
(223s → 0.56s) plus the warm-memory investigation (H-mem falsified, TLDR-myd).

## Measurement logs

| File | What it is |
|---|---|
| `tldr-rss-cold.log` | Overnight cold-build RSS timeline (~94 KB). Source of the 27.0 GB getrusage high-water peak attributed to the EMBED phase (ADR-7 / TLDR-ab6). |
| `tldr-rss-timeline.log` | RSS timeline polling log (companion run). |
| `tldr-vmmap-cold.log` | vmmap snapshots during the cold build — resident vs compressed-footprint evidence (pre-F1: 13.7 GB resident vs 22.0 GB footprint; motivates the phys_footprint probe, TLDR-d3s). |

## Design / research documents

| File | What it is |
|---|---|
| `tldr-22gb-design.md` | Original "22 GB blow-up" debug plan + design (epic TLDR-k8s). Note: its memory attribution was later corrected by ADR-7. |
| `tldr-salsa-chunk-design.md` | Disk-backed per-file chunk-invalidated salsa cache design (Path B, TLDR-zde). Status: deferred per ADR-6. |
| `tldr-analysis-store-review-brief.md` | Review brief for the analysis-store / chunk-store proposal. |
| `tldr-inhouse-review.md` | In-house review of the design. |
| `tldr-2026-research.md` | Background research notes. |
| `tldr-research-annex-v2.md` | Research annex v2. |
| `tldr-watcher-ignore-design.md` | Daemon filesystem-watcher ignore-handling design (TLDR-bfc lineage). |

## Frozen corpora (TLDR-olg count-identity gates)

Shallow clones at `/tmp/repos` used for the python/TS resolver measurements
(ADR-6 decision input 2) and frozen as count-gate references for the fix half
of TLDR-olg. **If `/tmp/repos` is lost, re-clone at exactly these SHAs:**

| Repo | HEAD SHA | Origin |
|---|---|---|
| flask | `36e4a824f340fdee7ed50937ba8e7f6bc7d17f81` | https://github.com/pallets/flask |
| django | `8c2d3dca633629effad17ccc98730234f740d03f` | https://github.com/django/django |
| zod | `bbc68f990c7e6a5e3f506c56fb04bd0279b9c9b5` | https://github.com/colinhacks/zod |
| nest | `5e80f308ad5a36e1ec18cc2bde17dd6447e085db` | https://github.com/nestjs/nest |
| caddy | `d730df2a83e83ea3ec2990b213385cc34152c62e` | https://github.com/caddyserver/caddy |
| junit5 | `205df1c998af62bce0ed5c49ca4504aeec94f53a` | https://github.com/junit-team/junit5 |

(`django-sub25` / `django-sub50` are local subset directories derived from the
django clone, not git checkouts — no SHA.)

Measured phase splits on these corpora (round-5 binary, `--respect-ignore`):

- flask: 83 files / 1,460 fn / 1,183 edges — parse 20ms / compose 241ms (91%)
- django: 2,917 files / 32,308 fn / 43,039 edges — parse 465ms / **compose 23.3s (97.6%)**
- zod: 405 files / 1,905 fn / 2,311 edges — parse 115ms / **compose 4.7s (97.5%)**
- nest: 1,724 files / 3,679 fn / 4,582 edges — parse 155ms / compose 1.9s (91%)
- reference rust (tldr-code, fixed): 0.02 ms/edge; broken baseline 8.5 ms/edge

Full context lives in beads: `bd show TLDR-olg TLDR-7mp TLDR-ab6 TLDR-myd`.

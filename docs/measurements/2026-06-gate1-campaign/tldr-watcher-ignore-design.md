# Watcher: honor .tldrignore (+ close .gitignore delete-path gap) in watch_decision — REINDEX-ONLY

## Problem (verified 2026-06-04, explanation session)

The in-daemon watcher (crates/tldr-cli/src/commands/daemon/watcher.rs) never consults
`.tldrignore`. `watch_decision` (watcher.rs:74) filters via: Access drop → `.tldr/` +
store-dir prefix → `is_corpus_file` (tldr-core chunker.rs:312). `is_corpus_file` honors
`.gitignore` / DEFAULT_EXCLUDE_DIRS / generated sentinels but has ZERO `.tldrignore`
references (confirmed: no matches in chunker.rs or walker.rs).

Consequence: an edit to a source file in a tldrignored-but-not-gitignored dir flows
OS event → enqueue → process_dirty_file (daemon.rs:1359) → salsa invalidation →
re-chunk + re-embed + index insert. Southbound work on a path the user excluded.

Secondary gap in the SAME decision fn: a DELETED gitignored file falls through the
vanished-path passthrough branch (is_corpus_file can't judge gone paths) and gets
enqueued; apply_delta's store-side filter drops it, but only after a wasted southbound hop.

Irony: the CLI-foreground warm AUTO-CREATES `.tldrignore` (warm.rs:240-251) — the
product promises the contract, the watcher breaks it.

## Decided scope (user decision 2026-06-04)

- REINDEX-ONLY: ignored paths must never reach the southbound pipeline (no enqueue,
  no salsa invalidation, no chunk/embed/index work).
- presence_decision is UNCHANGED: ignored-dir writes still count as presence
  (preserves the deliberate TLDR-3w5 choice that `cargo build` writing to gitignored
  `target/` keeps the daemon alive).
- OS events cannot be suppressed (FSEvents has no subtree exclusion); the guarantee
  is filtered-before-channel.

## Design

1. Reuse the Gitignore-based matcher from tldr_core::callgraph::scanner::load_tldrignore
   (scanner.rs:529 — proper glob semantics via matched_path_or_any_parents). Do NOT use
   warm.rs's name-stem HashSet load_tldrignore (warm.rs:323) — crude component matching.
   load_tldrignore is currently private; export it (or add a small pub helper
   `is_tldrignored(root, path, is_dir) -> bool`). filter_tldrignored (scanner.rs:554)
   is already pub but batch-shaped, wrong shape for per-event checks.
2. In spawn_watcher (watcher.rs:125): load the `.tldrignore` matcher ONCE, plus a
   root-`.gitignore` matcher (same GitignoreBuilder pattern) to close the deleted-
   gitignored-file gap. Move/clone both into the debounce-handler closure.
3. In watch_decision: check `matched_path_or_any_parents(rel_path, is_dir)` for BOTH
   matchers BEFORE the `path.exists()` / `is_corpus_file` branch. Order matters:
   the check must run before the exists() branch so DELETES inside ignored dirs are
   also dropped — same delete-trap shape as the existing `.tldr/` prefix check
   (is_corpus_file always returns false for vanished paths). For vanished paths use
   is_dir=false; parent-dir patterns like `vendored/` still match via _or_any_parents.
4. presence_decision: NO change. The presence tap (watcher.rs:193) stays pre-filter.
5. Tests (watcher.rs tests module already has the pattern):
   - tldrignored existing file modify → !watch_decision, presence_decision still true
   - tldrignored DELETED file → !watch_decision (delete-trap variant)
   - gitignored DELETED file → !watch_decision (the secondary gap)
   - .tldrignore absent → behavior identical to today
   - glob pattern (`*.gen.py`) and nested dir pattern (`vendor/sub/`) respected

## Documented limitations (v1, do not solve)

- Matchers load once at daemon start; editing .gitignore/.tldrignore mid-session
  requires daemon restart. (Possible follow-up: watcher sees events on those two
  files → rebuild matchers.)
- Presence-side and root-matcher only: nested .gitignore files and git global
  excludes are NOT covered by the new explicit matchers (is_corpus_file still
  covers them for existing files; the new matcher only needs to catch deletes).

## Related but SEPARATE defect (filed independently)

enumerate_corpus_files (chunker.rs:417) — the single source of truth for what gets
embedded — also ignores .tldrignore, so the initial warm build over-indexes
tldrignored dirs (wasted embed minutes + RSS + polluted search results). Fixing the
watcher alone leaves the full-build path inconsistent with the delta path.

## Verification plan (repro already scripted in session)

Scratch project /tmp/tig with `vendored/` in .tldrignore; warm; create new .py under
vendored/ → BEFORE fix: status vector count increases (bug). AFTER fix: count stable,
Watcher presence age still resets (reindex-only semantics confirmed).

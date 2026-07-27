//! In-daemon filesystem watcher (TLDR-ac0.2).
//!
//! Brings file-change detection INTO the Rust daemon, co-located with the
//! in-RAM index it mutates, replacing the cross-process C++ fsnotifier → IPC
//! `Notify` hop. The shape is:
//!
//! ```text
//!   notify-debouncer-full (OS watcher + debounce, own thread)
//!        │  watch_decision() filter (cheap excludes + corpus membership)
//!        ▼
//!   bounded mpsc<PathBuf>   ── drop-on-full (never block the watch thread)
//!        ▼
//!   single serialized worker task
//!        │  coalesce: drain everything queued into a dedup set
//!        ▼
//!   TLDRDaemon::process_dirty_file()  (salsa invalidate + in-place delta)
//! ```
//!
//! The watcher and worker share NO lock — invalidation flows over the channel,
//! dissolving the async-thread-mutex hazard (TLDR-qr9) by construction. Honest
//! framing: notify is NOT faster than fsnotifier (same OS primitives); the win
//! is consolidation into one process and making the t8f delta an in-process
//! call rather than an IPC contract.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify_debouncer_full::notify::{EventKind, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, RecommendedCache};
use tokio::sync::mpsc;

use tldr_core::semantic::{is_corpus_file, store_dir_for};
use tldr_core::walker::{build_path_ignore_matcher, PathIgnoreMatcher};

use super::activity::Source;
use super::daemon::TLDRDaemon;

/// Debounce window: editor save-storms and `git checkout` bursts collapse to a
/// single emission per file within this window. notify auto-selects a tick rate
/// of 1/4 of this when `tick_rate` is `None`.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// Bounded channel depth. On overflow the watch-thread handler DROPS the event
/// (drop-before-persist) rather than blocking — a coarse burst cap. The
/// advanced burst-cap lives in TLDR-ac0.7; here a drop is safe because the next
/// edit (or a manual reindex) re-enqueues, and `apply_delta` re-reads disk
/// state, so a missed intermediate event never corrupts the index.
const CHANNEL_CAP: usize = 1024;

/// The live watcher. Holding this value keeps the OS watcher and worker alive;
/// dropping it stops the watcher, closes the channel, and ends the worker.
pub(crate) type WatcherGuard = Debouncer<RecommendedWatcher, RecommendedCache>;

/// Decide whether a single event path should be enqueued for reindexing.
///
/// Pure and side-effect-free (modulo the filesystem reads it performs) so the
/// trap / corpus / symlink tests can exercise it directly without standing up a
/// live daemon and racing debounce timing.
///
/// Order of checks (cheapest first, and the prefix excludes are load-bearing
/// for DELETES — see below):
/// 1. Pure read events (`Access`) carry no index change → drop.
/// 2. The daemon's OWN writes must never feed back: the in-tree `<root>/.tldr`
///    cache subtree (`persist_stats` writes `salsa_stats.json` there) and the
///    resident store dir. A prefix check is the ONLY self-write defense that
///    works for deletes — `is_corpus_file` canonicalizes the file and so always
///    returns `false` for a vanished path, which would otherwise let a deleted
///    `.tldr/*` file fall through to the passthrough branch.
/// 3. `.tldrignore` / `.gitignore` exclusion (TLDR-1j2): consulted via the
///    root-level `ignore_matcher` BEFORE the `exists()` branch so it ALSO drops
///    DELETES inside ignored dirs (a vanished path can't be walked). For paths
///    that still exist, the deeper per-directory matching is handled by
///    `is_corpus_file` in step 4; this matcher is the only mechanism that can
///    drop a deleted ignored file before a wasted southbound reindex hop.
/// 4. An existing path must be a corpus member (same walker rules as the build,
///    `.tldrignore`-aware via `add_custom_ignore_filename`).
/// 5. A vanished path (delete / rename-away) that survived the ignore matcher is
///    passed through; `apply_delta`'s store-side delete filter cleanly drops it
///    (`removed == 0` → `Filtered`) if it was never indexed.
pub(crate) fn watch_decision(
    project: &Path,
    cache_excl: &Path,
    store_dir: &Path,
    ignore_matcher: Option<&PathIgnoreMatcher>,
    path: &Path,
    kind: &EventKind,
) -> bool {
    if !presence_decision(cache_excl, store_dir, path, kind) {
        return false;
    }
    // `.tldrignore`/`.gitignore` drop (TLDR-1j2), BEFORE exists() so deletes
    // inside ignored dirs are dropped too. `path.is_dir()` is `false` for a
    // vanished path; parent-dir patterns (`vendored/`) still match via
    // `matched_path_or_any_parents`.
    if let Some(ig) = ignore_matcher {
        if ig.is_ignored(path, path.is_dir()) {
            return false;
        }
    }
    if path.exists() {
        is_corpus_file(project, path)
    } else {
        true
    }
}

/// Decide whether a single event counts as project PRESENCE for the daemon's
/// idle timer (TLDR-3w5) — deliberately looser than [`watch_decision`]: a
/// `cargo build` writing to `target/` is filtered from indexing (not corpus)
/// but is still proof someone is alive in this project, so liveness taps the
/// event stream BEFORE the corpus filter.
///
/// Two exclusions, both immortality-safe by design (a daemon must never count
/// its own activity as presence):
/// - Self-writes (`<root>/.tldr` cache subtree + resident store dir): counting
///   our own store/stats writes would be a self-perpetuating liveness loop.
/// - `Access` (read) events — INTENTIONAL, do not "restore" raw-event
///   behavior: the daemon's own corpus READS during build/delta (plus
///   Spotlight/backup/AV scanners) fire `Access` events, which would make an
///   actively-building daemon immortal via its own reads. Writes-only loses
///   nothing the presence philosophy wants — human/agent/build activity
///   manifests as `Modify`/`Create`/`Remove`.
pub(crate) fn presence_decision(
    cache_excl: &Path,
    store_dir: &Path,
    path: &Path,
    kind: &EventKind,
) -> bool {
    !matches!(kind, EventKind::Access(_))
        && !path.starts_with(cache_excl)
        && !path.starts_with(store_dir)
}

/// Spawn the recursive project watcher and its serialized reindex worker.
///
/// Returns the guard to hold for the daemon's lifetime (see [`WatcherGuard`]).
/// Returns `None` — and the daemon keeps serving the IPC `Notify` path — if the
/// hard self-write precondition fails or the OS watcher can't be created; both
/// are logged. Must be called from within a Tokio runtime (it spawns the
/// worker task).
pub(crate) fn spawn_watcher(daemon: Arc<TLDRDaemon>) -> Option<WatcherGuard> {
    let project = daemon.project().clone();

    // HARD PRECONDITION (TLDR-ac0.2): the resident store dir must be OUTSIDE the
    // watched root, else the daemon's own index writes fire events → reindex →
    // write → infinite loop. `store_dir_for` resolves to `~/.cache/tldr/stores/`
    // (external by design); refuse to watch and warn loudly if that invariant
    // ever changes, rather than silently spinning.
    let store_dir = store_dir_for(&project);
    if store_dir.starts_with(&project) {
        eprintln!(
            "[ac0.2] refusing to watch: store dir {} is inside project root {} \
             (would self-write-loop); watcher disabled, IPC Notify still served",
            store_dir.display(),
            project.display()
        );
        return None;
    }
    // The in-tree cache subtree (`<root>/.tldr`) IS inside the watched root.
    let cache_excl = project.join(".tldr");

    // Root-level `.tldrignore` (+ `.gitignore`) matcher for the reindex filter
    // (TLDR-1j2). Loaded ONCE here; editing either file mid-session needs a
    // daemon restart (documented v1 limitation). `presence_decision` is
    // deliberately NOT gated on this — an ignored-dir write still counts as
    // project presence (the TLDR-3w5 `cargo build` → `target/` liveness rule).
    let handler_ignore = build_path_ignore_matcher(&project, true);
    if handler_ignore.is_some() {
        eprintln!("[ac0.2] reindex filter honoring .tldrignore/.gitignore");
    }

    let (tx, mut rx) = mpsc::channel::<PathBuf>(CHANNEL_CAP);

    // Serialized reindex worker: one file at a time (`process_dirty_file` awaits
    // its `spawn_blocking` delta, so deltas never overlap and never contend on
    // the store write lock), with newest-wins coalescing. Draining everything
    // currently queued into a dedup set collapses an editor save-storm on one
    // file to a single reindex. Ordering is intentionally discarded: it doesn't
    // matter because `apply_delta` re-reads current disk state rather than
    // trusting the event kind, so modify-then-delete and delete-then-recreate
    // both resolve to the final on-disk state.
    let worker_daemon = Arc::clone(&daemon);
    tokio::spawn(async move {
        while let Some(first) = rx.recv().await {
            let mut batch: HashSet<PathBuf> = HashSet::new();
            batch.insert(first);
            while let Ok(more) = rx.try_recv() {
                batch.insert(more);
            }
            for path in batch {
                let _ = worker_daemon.process_dirty_file(path).await;
            }
        }
        // Channel closed (guard dropped) → worker exits cleanly.
    });

    // The debouncer handler runs on notify's OWN thread (sync). It must never
    // block — `try_send` drops on a full channel (drop-before-persist), and
    // the presence tap is a relaxed atomic store. The filters are pure, so
    // the handler needs no daemon handle beyond the activity Arc.
    let handler_project = project.clone();
    let handler_store_dir = store_dir.clone();
    let handler_activity = Arc::clone(daemon.activity());
    let result = new_debouncer(DEBOUNCE, None, move |res: DebounceEventResult| {
        let events = match res {
            Ok(events) => events,
            Err(errors) => {
                for e in errors {
                    eprintln!("[ac0.2] watch error: {e:?}");
                }
                return;
            }
        };
        for event in events {
            for path in &event.paths {
                // Presence tap (TLDR-3w5): post-debounce, PRE-corpus-filter —
                // any non-self, non-read project event defers idle shutdown,
                // even if it never reaches the index (e.g. target/ writes).
                if presence_decision(&cache_excl, &handler_store_dir, path, &event.kind) {
                    handler_activity.touch(Source::Watcher);
                }
                if watch_decision(
                    &handler_project,
                    &cache_excl,
                    &handler_store_dir,
                    handler_ignore.as_ref(),
                    path,
                    &event.kind,
                ) {
                    // Drop-on-full: never block the watch thread.
                    let _ = tx.try_send(path.clone());
                }
            }
        }
    });

    let mut debouncer = match result {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[ac0.2] failed to create filesystem watcher: {e}");
            return None;
        }
    };

    if let Err(e) = debouncer.watch(&project, RecursiveMode::Recursive) {
        eprintln!(
            "[ac0.2] failed to watch {}: {e}; watcher disabled, IPC Notify still served",
            project.display()
        );
        return None;
    }

    eprintln!("[ac0.2] watching {} recursively", project.display());
    Some(debouncer)
}

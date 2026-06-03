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
/// 3. An existing path must be a corpus member (same walker rules as the build).
/// 4. A vanished path (delete / rename-away) can't be walker-checked, so it is
///    passed through; `apply_delta`'s store-side delete filter cleanly drops it
///    (`removed == 0` → `Filtered`) if it was never indexed.
pub(crate) fn watch_decision(
    project: &Path,
    cache_excl: &Path,
    store_dir: &Path,
    path: &Path,
    kind: &EventKind,
) -> bool {
    if matches!(kind, EventKind::Access(_)) {
        return false;
    }
    if path.starts_with(cache_excl) || path.starts_with(store_dir) {
        return false;
    }
    if path.exists() {
        is_corpus_file(project, path)
    } else {
        true
    }
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
    // block — `try_send` drops on a full channel (drop-before-persist). The
    // filter is pure, so the handler needs no daemon handle.
    let handler_project = project.clone();
    let handler_store_dir = store_dir.clone();
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
                if watch_decision(
                    &handler_project,
                    &cache_excl,
                    &handler_store_dir,
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

#[cfg(test)]
mod tests {
    use super::*;
    use notify_debouncer_full::notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind};

    fn create_kind() -> EventKind {
        EventKind::Create(CreateKind::File)
    }
    fn modify_kind() -> EventKind {
        EventKind::Modify(ModifyKind::Any)
    }
    fn remove_kind() -> EventKind {
        EventKind::Remove(RemoveKind::File)
    }

    /// A real, on-disk source file under the root is a corpus member → enqueue.
    #[test]
    fn corpus_file_create_is_enqueued() {
        let root = tempfile::tempdir().unwrap();
        let store_dir = store_dir_for(root.path());
        let cache_excl = root.path().join(".tldr");
        let py = root.path().join("m.py");
        std::fs::write(&py, "def f():\n    return 1\n").unwrap();

        assert!(watch_decision(
            root.path(),
            &cache_excl,
            &store_dir,
            &py,
            &create_kind()
        ));
        assert!(watch_decision(
            root.path(),
            &cache_excl,
            &store_dir,
            &py,
            &modify_kind()
        ));
    }

    /// SELF-WRITE TRAP: an in-tree `.tldr/cache` write (what `persist_stats`
    /// does) must never enqueue — for BOTH an existing file and a deleted one.
    /// The delete case is the critical one: `is_corpus_file` returns false for a
    /// gone file, so only the prefix exclusion stops a deleted `.tldr/*` event
    /// from falling through to the passthrough branch and looping.
    #[test]
    fn in_tree_tldr_write_and_delete_are_excluded() {
        let root = tempfile::tempdir().unwrap();
        let store_dir = store_dir_for(root.path());
        let cache_excl = root.path().join(".tldr");
        let stats = cache_excl.join("cache").join("salsa_stats.json");
        std::fs::create_dir_all(stats.parent().unwrap()).unwrap();
        std::fs::write(&stats, "{}").unwrap();

        // Existing stats file write → excluded.
        assert!(!watch_decision(
            root.path(),
            &cache_excl,
            &store_dir,
            &stats,
            &modify_kind()
        ));

        // Deleted stats file → still excluded (prefix check, not is_corpus_file).
        std::fs::remove_file(&stats).unwrap();
        assert!(!watch_decision(
            root.path(),
            &cache_excl,
            &store_dir,
            &stats,
            &remove_kind()
        ));
    }

    /// A deletion of a (now-vanished) source file is passed through — the
    /// store-side delete filter in `apply_delta` decides whether it was indexed.
    #[test]
    fn vanished_source_delete_is_passed_through() {
        let root = tempfile::tempdir().unwrap();
        let store_dir = store_dir_for(root.path());
        let cache_excl = root.path().join(".tldr");
        let gone = root.path().join("deleted.py");
        // Never created on disk → exists() == false, not under .tldr.
        assert!(watch_decision(
            root.path(),
            &cache_excl,
            &store_dir,
            &gone,
            &remove_kind()
        ));
    }

    /// A non-corpus existing file (wrong extension / not a known language) is
    /// dropped even though it lives under the root.
    #[test]
    fn non_corpus_existing_file_is_dropped() {
        let root = tempfile::tempdir().unwrap();
        let store_dir = store_dir_for(root.path());
        let cache_excl = root.path().join(".tldr");
        let blob = root.path().join("data.bin");
        std::fs::write(&blob, [0u8, 159, 146, 150]).unwrap();
        assert!(!watch_decision(
            root.path(),
            &cache_excl,
            &store_dir,
            &blob,
            &create_kind()
        ));
    }

    /// Pure read events carry no index change.
    #[test]
    fn access_events_are_dropped() {
        let root = tempfile::tempdir().unwrap();
        let store_dir = store_dir_for(root.path());
        let cache_excl = root.path().join(".tldr");
        let py = root.path().join("m.py");
        std::fs::write(&py, "def f():\n    return 1\n").unwrap();
        assert!(!watch_decision(
            root.path(),
            &cache_excl,
            &store_dir,
            &py,
            &EventKind::Access(AccessKind::Read)
        ));
    }

    /// END-TO-END WIRING SMOKE TEST: prove the notify → channel → worker path
    /// actually fires (the pure `watch_decision` tests above cover the filter;
    /// this covers the plumbing). Tolerant by construction — skips if the OS
    /// watcher can't start in this environment, and polls within a generous
    /// budget rather than asserting on exact debounce timing. Asserts only that
    /// the new file reached the dirty set (no ONNX needed: `process_dirty_file`
    /// inserts into the dirty set before the semantic delta).
    #[tokio::test(flavor = "multi_thread")]
    async fn watcher_routes_new_file_end_to_end() {
        use super::super::types::DaemonConfig;

        let root = tempfile::tempdir().unwrap();
        let config = DaemonConfig {
            enable_watcher: true,
            ..DaemonConfig::default()
        };
        let daemon = Arc::new(TLDRDaemon::new(root.path().to_path_buf(), config));

        let Some(_guard) = spawn_watcher(Arc::clone(&daemon)) else {
            return; // watcher couldn't start here — nothing to smoke-test.
        };

        // Let the OS watcher register before mutating the tree.
        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(root.path().join("m.py"), "def f():\n    return 1\n").unwrap();

        // Poll up to ~5s (debounce is 500ms) for the worker to process it.
        let mut detected = false;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if daemon.dirty_file_count().await > 0 {
                detected = true;
                break;
            }
        }
        assert!(
            detected,
            "watcher should route a new .py file through process_dirty_file"
        );
    }

    /// SYMLINKED ROOT: when the daemon watches via a symlinked root path, events
    /// arrive prefixed with that symlinked form. `is_corpus_file` canonicalizes
    /// both sides, so a corpus file under the symlinked root is still detected.
    #[cfg(unix)]
    #[test]
    fn symlinked_root_resolves_corpus_membership() {
        let real = tempfile::tempdir().unwrap();
        let py_real = real.path().join("m.py");
        std::fs::write(&py_real, "def f():\n    return 1\n").unwrap();

        let link_base = tempfile::tempdir().unwrap();
        let link_root = link_base.path().join("link");
        std::os::unix::fs::symlink(real.path(), &link_root).unwrap();

        // Watch via the symlinked root; the event path is symlink-prefixed.
        let store_dir = store_dir_for(&link_root);
        let cache_excl = link_root.join(".tldr");
        let py_via_link = link_root.join("m.py");
        assert!(watch_decision(
            &link_root,
            &cache_excl,
            &store_dir,
            &py_via_link,
            &create_kind()
        ));
    }
}

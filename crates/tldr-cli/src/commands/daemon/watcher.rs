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
            None,
            &py,
            &create_kind()
        ));
        assert!(watch_decision(
            root.path(),
            &cache_excl,
            &store_dir,
            None,
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
            None,
            &stats,
            &modify_kind()
        ));

        // Deleted stats file → still excluded (prefix check, not is_corpus_file).
        std::fs::remove_file(&stats).unwrap();
        assert!(!watch_decision(
            root.path(),
            &cache_excl,
            &store_dir,
            None,
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
            None,
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
            None,
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
            None,
            &py,
            &EventKind::Access(AccessKind::Read)
        ));
    }

    // -- .tldrignore / .gitignore reindex filter (TLDR-1j2) --------------------

    /// Helper: build the root-level `.tldrignore` + `.gitignore` matcher the
    /// watcher loads once in `spawn_watcher`.
    fn matcher_for(root: &Path) -> Option<PathIgnoreMatcher> {
        build_path_ignore_matcher(root, true)
    }

    /// TLDRIGNORED EXISTING FILE: an edit to a source file inside a
    /// `.tldrignore`d-but-not-`.gitignore`d dir must NOT reindex — proven via
    /// BOTH mechanisms: the explicit root matcher AND `is_corpus_file` (which is
    /// now `.tldrignore`-aware via `add_custom_ignore_filename`, so even a
    /// `None` matcher drops it). Presence is UNCHANGED (reindex-only scope).
    #[test]
    fn tldrignored_existing_file_is_dropped_but_counts_presence() {
        let root = tempfile::tempdir().unwrap();
        let store_dir = store_dir_for(root.path());
        let cache_excl = root.path().join(".tldr");
        std::fs::write(root.path().join(".tldrignore"), "vendored/\n").unwrap();
        let vfile = root.path().join("vendored").join("v2.py");
        std::fs::create_dir_all(vfile.parent().unwrap()).unwrap();
        std::fs::write(&vfile, "def f():\n    return 1\n").unwrap();

        let matcher = matcher_for(root.path());

        // With the explicit matcher → dropped.
        assert!(!watch_decision(
            root.path(),
            &cache_excl,
            &store_dir,
            matcher.as_ref(),
            &vfile,
            &modify_kind()
        ));
        // Even WITHOUT the matcher → still dropped, proving is_corpus_file now
        // honors .tldrignore (keeps the warm build + delta path consistent).
        assert!(!watch_decision(
            root.path(),
            &cache_excl,
            &store_dir,
            None,
            &vfile,
            &modify_kind()
        ));
        // …but the edit still counts as project presence (TLDR-3w5 unchanged).
        assert!(
            presence_decision(&cache_excl, &store_dir, &vfile, &modify_kind()),
            "tldrignored edit must still defer idle shutdown"
        );
    }

    /// TLDRIGNORED DELETE TRAP: a DELETED file inside a `.tldrignore`d dir must
    /// be dropped. This is the case `is_corpus_file` CANNOT catch (it bails on a
    /// vanished path), so the explicit matcher — checked before the exists()
    /// branch — is the only thing that stops a wasted southbound reindex hop.
    #[test]
    fn tldrignored_deleted_file_is_dropped() {
        let root = tempfile::tempdir().unwrap();
        let store_dir = store_dir_for(root.path());
        let cache_excl = root.path().join(".tldr");
        std::fs::write(root.path().join(".tldrignore"), "vendored/\n").unwrap();
        let gone = root.path().join("vendored").join("gone.py"); // never created

        let matcher = matcher_for(root.path());
        assert!(!watch_decision(
            root.path(),
            &cache_excl,
            &store_dir,
            matcher.as_ref(),
            &gone,
            &remove_kind()
        ));
    }

    /// GITIGNORED DELETE GAP (secondary fix): a DELETED `.gitignore`d file also
    /// falls through the vanished-path branch today (apply_delta drops it, but
    /// only after a wasted hop). The matcher includes root `.gitignore`, so it
    /// is dropped up front.
    #[test]
    fn gitignored_deleted_file_is_dropped() {
        let root = tempfile::tempdir().unwrap();
        let store_dir = store_dir_for(root.path());
        let cache_excl = root.path().join(".tldr");
        std::fs::write(root.path().join(".gitignore"), "secrets/\n").unwrap();
        let gone = root.path().join("secrets").join("key.py"); // never created

        let matcher = matcher_for(root.path());
        assert!(!watch_decision(
            root.path(),
            &cache_excl,
            &store_dir,
            matcher.as_ref(),
            &gone,
            &remove_kind()
        ));
    }

    /// NO `.tldrignore`: behavior is identical to before — the matcher loader
    /// returns `None` and a normal corpus file is still enqueued.
    #[test]
    fn absent_tldrignore_behaves_as_before() {
        let root = tempfile::tempdir().unwrap();
        let store_dir = store_dir_for(root.path());
        let cache_excl = root.path().join(".tldr");
        let py = root.path().join("m.py");
        std::fs::write(&py, "def f():\n    return 1\n").unwrap();

        assert!(matcher_for(root.path()).is_none(), "no ignore files → None");
        assert!(watch_decision(
            root.path(),
            &cache_excl,
            &store_dir,
            None,
            &py,
            &modify_kind()
        ));
    }

    /// GLOB + NESTED-DIR patterns are respected (delete-path, via the matcher):
    /// `*.gen.py` and `gen/sub/` both drop vanished paths that match.
    #[test]
    fn tldrignore_glob_and_nested_dir_patterns_respected() {
        let root = tempfile::tempdir().unwrap();
        let store_dir = store_dir_for(root.path());
        let cache_excl = root.path().join(".tldr");
        std::fs::write(root.path().join(".tldrignore"), "*.gen.py\ngen/sub/\n").unwrap();
        let matcher = matcher_for(root.path());

        let glob_gone = root.path().join("module.gen.py"); // matches *.gen.py
        let nested_gone = root.path().join("gen").join("sub").join("x.py"); // gen/sub/
        let kept_gone = root.path().join("real.py"); // matches nothing

        assert!(!watch_decision(
            root.path(),
            &cache_excl,
            &store_dir,
            matcher.as_ref(),
            &glob_gone,
            &remove_kind()
        ));
        assert!(!watch_decision(
            root.path(),
            &cache_excl,
            &store_dir,
            matcher.as_ref(),
            &nested_gone,
            &remove_kind()
        ));
        // A non-matching vanished path is still passed through.
        assert!(watch_decision(
            root.path(),
            &cache_excl,
            &store_dir,
            matcher.as_ref(),
            &kept_gone,
            &remove_kind()
        ));
    }

    /// PRESENCE TAP (TLDR-3w5): a write to a NON-corpus project path (e.g.
    /// `cargo build` writing into `target/`) is filtered from indexing but IS
    /// proof of life — presence says yes where watch says no.
    #[test]
    fn presence_counts_non_corpus_project_writes() {
        let root = tempfile::tempdir().unwrap();
        let store_dir = store_dir_for(root.path());
        let cache_excl = root.path().join(".tldr");
        let artifact = root.path().join("target").join("debug").join("build.bin");
        std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
        std::fs::write(&artifact, [0u8; 4]).unwrap();

        assert!(
            presence_decision(&cache_excl, &store_dir, &artifact, &modify_kind()),
            "non-corpus project write must count as presence"
        );
        assert!(
            !watch_decision(
                root.path(),
                &cache_excl,
                &store_dir,
                None,
                &artifact,
                &modify_kind()
            ),
            "…while still being excluded from indexing"
        );
    }

    /// PRESENCE IMMORTALITY TRAP (TLDR-3w5): the daemon's own writes
    /// (`.tldr` cache subtree + resident store dir) must NOT count as
    /// presence — counting our own store writes would be a self-perpetuating
    /// liveness loop. Covers writes AND deletes (prefix check, not exists()).
    #[test]
    fn presence_excludes_daemon_self_writes() {
        let root = tempfile::tempdir().unwrap();
        let store_dir = store_dir_for(root.path());
        let cache_excl = root.path().join(".tldr");

        let stats = cache_excl.join("cache").join("salsa_stats.json");
        assert!(!presence_decision(
            &cache_excl,
            &store_dir,
            &stats,
            &modify_kind()
        ));
        assert!(!presence_decision(
            &cache_excl,
            &store_dir,
            &stats,
            &remove_kind()
        ));

        let store_file = store_dir.join("index.usearch");
        assert!(!presence_decision(
            &cache_excl,
            &store_dir,
            &store_file,
            &modify_kind()
        ));
        assert!(!presence_decision(
            &cache_excl,
            &store_dir,
            &store_file,
            &remove_kind()
        ));
    }

    /// PRESENCE READ-EXCLUSION (TLDR-3w5): `Access` events must not count —
    /// the daemon's own corpus reads during build/delta (plus Spotlight,
    /// backups, AV scanners) would otherwise make a building daemon immortal
    /// via its own reads.
    #[test]
    fn presence_excludes_access_events() {
        let root = tempfile::tempdir().unwrap();
        let store_dir = store_dir_for(root.path());
        let cache_excl = root.path().join(".tldr");
        let py = root.path().join("m.py");

        assert!(!presence_decision(
            &cache_excl,
            &store_dir,
            &py,
            &EventKind::Access(AccessKind::Read)
        ));
        // …but a real write to the same path does count.
        assert!(presence_decision(
            &cache_excl,
            &store_dir,
            &py,
            &modify_kind()
        ));
        assert!(presence_decision(
            &cache_excl,
            &store_dir,
            &py,
            &create_kind()
        ));
        assert!(presence_decision(
            &cache_excl,
            &store_dir,
            &py,
            &remove_kind()
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
        // Presence tap wiring (TLDR-3w5): the same event must have refreshed
        // Watcher presence on its way through the debounce handler.
        let watcher_age = daemon.activity().presence_ages()[Source::Watcher as usize];
        assert!(
            watcher_age < Duration::from_secs(10),
            "watcher event should have touched Watcher presence (age: {watcher_age:?})"
        );
    }

    /// END-TO-END `.tldrignore`: a new file created inside a `.tldrignore`d dir
    /// must NOT reach the dirty set, while a normal source file does. Proves the
    /// filter is wired through the live watcher (not just the pure decision fn).
    #[tokio::test(flavor = "multi_thread")]
    async fn watcher_skips_tldrignored_dir_end_to_end() {
        use super::super::types::DaemonConfig;

        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".tldrignore"), "vendored/\n").unwrap();
        std::fs::create_dir_all(root.path().join("vendored")).unwrap();

        let config = DaemonConfig {
            enable_watcher: true,
            ..DaemonConfig::default()
        };
        let daemon = Arc::new(TLDRDaemon::new(root.path().to_path_buf(), config));

        let Some(_guard) = spawn_watcher(Arc::clone(&daemon)) else {
            return; // watcher couldn't start here — nothing to smoke-test.
        };

        tokio::time::sleep(Duration::from_millis(300)).await;
        // Write into the tldrignored dir.
        std::fs::write(
            root.path().join("vendored").join("v.py"),
            "def f():\n    return 1\n",
        )
        .unwrap();

        // Give the debounce window + margin to elapse; the dirty set must stay
        // empty (the ignored write was filtered before the channel).
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(
            daemon.dirty_file_count().await,
            0,
            "a write inside a .tldrignored dir must not reach the reindex worker"
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
            None,
            &py_via_link,
            &create_kind()
        ));
    }
}

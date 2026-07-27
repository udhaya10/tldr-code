//! In-daemon filesystem watcher (TLDR-ac0.2).
//!
//! Brings file-change detection INTO the Rust daemon, co-located with the
//! in-RAM index it mutates, replacing the cross-process C++ fsnotifier → IPC
//! `Notify` hop. The shape is:
//!
//! ```text
//!   notify OS watcher (own thread, raw accepted events)
//!        │  watch_decision() filter (cheap excludes + corpus membership)
//!        ▼
//!   bounded mpsc<PathBuf>   ── overflow flag (never block the watch thread)
//!        ▼
//!   single serialized debounce/coalesce worker
//!        │  quiet timer + max-wait + rolling burst cap
//!        ▼
//!   batch delta OR one single-flight full rebuild
//! ```
//!
//! The watcher and worker share NO lock — invalidation flows over the channel,
//! dissolving the async-thread-mutex hazard (TLDR-qr9) by construction. Honest
//! framing: notify is NOT faster than fsnotifier (same OS primitives); the win
//! is consolidation into one process and making the t8f delta an in-process
//! call rather than an IPC contract.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify_debouncer_full::notify::{
    recommended_watcher, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};
use tokio::sync::mpsc;

use tldr_core::semantic::{is_corpus_file, store_dir_for};
use tldr_core::walker::{build_path_ignore_matcher, PathIgnoreMatcher, TLDRIGNORE_FILE};

use super::activity::{ActivityTracker, Source};
use super::daemon::TLDRDaemon;

/// Safety bound for the callback-to-worker queue and rolling event history.
/// Configured caps above this still behave correctly, but overflow escalates
/// conservatively before an untrusted config can request an enormous channel.
const MAX_EVENT_CAP: usize = 65_535;

/// The live watcher. Holding this value keeps the OS watcher and worker alive;
/// dropping it stops the watcher, closes the channel, and ends the worker.
pub(crate) type WatcherGuard = RecommendedWatcher;

#[derive(Debug, Clone, Copy)]
struct WatchPipelineConfig {
    debounce: Duration,
    max_wait: Duration,
    burst_file_cap: usize,
    burst_event_cap: usize,
    burst_window: Duration,
}

impl WatchPipelineConfig {
    fn from_daemon(daemon: &TLDRDaemon) -> Self {
        let config = daemon.config();
        Self {
            debounce: Duration::from_millis(config.watcher_debounce_ms),
            max_wait: Duration::from_millis(config.watcher_max_wait_ms),
            burst_file_cap: config.watcher_burst_file_cap,
            burst_event_cap: config.watcher_burst_event_cap.min(MAX_EVENT_CAP),
            burst_window: Duration::from_millis(config.watcher_burst_window_ms),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Flush {
    Delta(Vec<PathBuf>),
    FullRebuild,
}

/// Deterministic worker-owned debounce and rolling-burst state.
///
/// The notify thread only enqueues accepted paths. Keeping all timing state in
/// one serialized owner makes quiet/max-wait ordering explicit and prevents a
/// mutex from leaking onto notify's synchronous callback thread.
struct WatchPipeline {
    config: WatchPipelineConfig,
    pending: HashMap<PathBuf, Instant>,
    first_event: Option<Instant>,
    accepted_events: VecDeque<Instant>,
}

impl WatchPipeline {
    fn new(config: WatchPipelineConfig) -> Self {
        Self {
            config,
            pending: HashMap::new(),
            first_event: None,
            accepted_events: VecDeque::new(),
        }
    }

    fn accept(&mut self, path: PathBuf, now: Instant) -> Option<Flush> {
        while self
            .accepted_events
            .front()
            .is_some_and(|first| now.saturating_duration_since(*first) > self.config.burst_window)
        {
            self.accepted_events.pop_front();
        }
        self.accepted_events.push_back(now);
        self.pending.insert(path, now);
        self.first_event.get_or_insert(now);

        if self.pending.len() > self.config.burst_file_cap
            || self.accepted_events.len() > self.config.burst_event_cap
        {
            self.reset_all();
            return Some(Flush::FullRebuild);
        }
        None
    }

    fn deadline(&self) -> Option<Instant> {
        let quiet = self
            .pending
            .values()
            .map(|last_event| {
                last_event
                    .checked_add(self.config.debounce)
                    .unwrap_or(*last_event)
            })
            .min()?;
        let first_event = self.first_event?;
        let max_wait = first_event
            .checked_add(self.config.max_wait)
            .unwrap_or(first_event);
        Some(quiet.min(max_wait))
    }

    fn flush_due(&mut self, now: Instant) -> Option<Flush> {
        if now < self.deadline()? {
            return None;
        }
        let mut files: Vec<_> = self.pending.drain().map(|(path, _)| path).collect();
        files.sort();
        self.first_event = None;
        (!files.is_empty()).then_some(Flush::Delta(files))
    }

    fn reset_all(&mut self) {
        self.pending.clear();
        self.first_event = None;
        self.accepted_events.clear();
    }
}

struct LiveIgnoreMatcher {
    project: PathBuf,
    matcher: parking_lot::RwLock<Option<PathIgnoreMatcher>>,
}

impl LiveIgnoreMatcher {
    fn new(project: &Path) -> Self {
        Self {
            project: project.to_path_buf(),
            matcher: parking_lot::RwLock::new(build_path_ignore_matcher(project, true)),
        }
    }

    fn reload_for_paths(&self, paths: &[PathBuf]) -> bool {
        let tldrignore = self.project.join(TLDRIGNORE_FILE);
        let gitignore = self.project.join(".gitignore");
        let reload = paths
            .iter()
            .any(|path| path == &tldrignore || path == &gitignore);
        if reload {
            *self.matcher.write() = build_path_ignore_matcher(&self.project, true);
        }
        reload
    }

    fn snapshot(&self) -> Option<PathIgnoreMatcher> {
        self.matcher.read().clone()
    }

    #[cfg(test)]
    fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        self.matcher
            .read()
            .as_ref()
            .is_some_and(|matcher| matcher.is_ignored(path, is_dir))
    }
}

struct WatchHandler {
    project: PathBuf,
    cache_excl: PathBuf,
    store_dir: PathBuf,
    ignore: LiveIgnoreMatcher,
    activity: Arc<ActivityTracker>,
    tx: mpsc::Sender<PathBuf>,
    overflowed: Arc<AtomicBool>,
}

impl WatchHandler {
    fn handle(&self, result: notify_debouncer_full::notify::Result<Event>) {
        match result {
            Ok(event) => self.handle_event(event),
            Err(error) => eprintln!("[ac0.7] watch error: {error:?}"),
        }
    }

    fn handle_event(&self, event: Event) {
        if self.ignore.reload_for_paths(&event.paths) {
            eprintln!("[TLDR-1m4] reloaded .tldrignore/.gitignore policy");
        }
        let ignore = self.ignore.snapshot();
        for path in &event.paths {
            if presence_decision(&self.cache_excl, &self.store_dir, path, &event.kind) {
                self.activity.touch(Source::Watcher);
            }
            if !watch_decision(
                &self.project,
                &self.cache_excl,
                &self.store_dir,
                ignore.as_ref(),
                path,
                &event.kind,
            ) {
                continue;
            }
            match self.tx.try_send(path.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.overflowed.store(true, Ordering::SeqCst);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => return,
            }
        }
    }
}

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

    // Root-level `.tldrignore` (+ `.gitignore`) matcher for the reindex filter.
    // Policy-file events atomically replace this snapshot during the daemon
    // session (TLDR-1m4). `presence_decision` is deliberately NOT gated on
    // this — an ignored-dir write still counts as project presence (the
    // TLDR-3w5 `cargo build` → `target/` liveness rule).
    let handler_ignore = LiveIgnoreMatcher::new(&project);
    if handler_ignore.snapshot().is_some() {
        eprintln!("[ac0.2] reindex filter honoring .tldrignore/.gitignore");
    }

    let pipeline_config = WatchPipelineConfig::from_daemon(&daemon);
    // Hold enough events to observe the configured strict `> cap` threshold.
    // If the worker is temporarily inside its blocking batch and this still
    // fills, the callback sets `overflowed`; the next worker turn rebuilds.
    let channel_cap = pipeline_config.burst_event_cap.saturating_add(1).max(1);
    let (tx, rx) = mpsc::channel::<PathBuf>(channel_cap);
    let overflowed = Arc::new(AtomicBool::new(false));

    // Serialized worker: it alone owns all debounce/burst state. A quiet or
    // max-wait flush crosses into blocking work ONCE for the whole batch.
    // A file/event cap (or bounded-channel overflow) clears queued deltas and
    // schedules the daemon's existing single-flight full warm.
    tokio::spawn(run_pipeline_worker(
        Arc::clone(&daemon),
        rx,
        Arc::clone(&overflowed),
        pipeline_config,
    ));

    // The raw notify handler runs on notify's OWN thread (sync). It never
    // blocks: `try_send` either enqueues or flips the overflow escalation flag.
    let handler = WatchHandler {
        project: project.clone(),
        cache_excl,
        store_dir,
        ignore: handler_ignore,
        activity: Arc::clone(daemon.activity()),
        tx,
        overflowed,
    };
    let result = recommended_watcher(move |res: notify_debouncer_full::notify::Result<Event>| {
        handler.handle(res);
    });

    let mut watcher = match result {
        Ok(watcher) => watcher,
        Err(e) => {
            eprintln!("[ac0.2] failed to create filesystem watcher: {e}");
            return None;
        }
    };

    if let Err(e) = watcher.watch(&project, RecursiveMode::Recursive) {
        eprintln!(
            "[ac0.2] failed to watch {}: {e}; watcher disabled, IPC Notify still served",
            project.display()
        );
        return None;
    }

    eprintln!("[ac0.2] watching {} recursively", project.display());
    Some(watcher)
}

async fn run_pipeline_worker(
    daemon: Arc<TLDRDaemon>,
    mut rx: mpsc::Receiver<PathBuf>,
    overflowed: Arc<AtomicBool>,
    config: WatchPipelineConfig,
) {
    let mut pipeline = WatchPipeline::new(config);
    loop {
        if overflowed.swap(false, Ordering::SeqCst) {
            drain_receiver(&mut rx);
            pipeline.reset_all();
            daemon.schedule_full_rebuild().await;
            continue;
        }

        if let Some(deadline) = pipeline.deadline() {
            tokio::select! {
                maybe_path = rx.recv() => {
                    let Some(path) = maybe_path else {
                        break;
                    };
                    accept_path(&daemon, &mut pipeline, &mut rx, path).await;
                }
                () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                    if let Some(flush) = pipeline.flush_due(Instant::now()) {
                        dispatch_flush(&daemon, flush).await;
                    }
                }
            }
        } else {
            let Some(path) = rx.recv().await else {
                break;
            };
            accept_path(&daemon, &mut pipeline, &mut rx, path).await;
        }
    }
}

async fn accept_path(
    daemon: &TLDRDaemon,
    pipeline: &mut WatchPipeline,
    rx: &mut mpsc::Receiver<PathBuf>,
    path: PathBuf,
) {
    if let Some(flush) = pipeline.accept(path, Instant::now()) {
        if matches!(flush, Flush::FullRebuild) {
            drain_receiver(rx);
        }
        dispatch_flush(daemon, flush).await;
    }
}

fn drain_receiver(rx: &mut mpsc::Receiver<PathBuf>) {
    while rx.try_recv().is_ok() {}
}

async fn dispatch_flush(daemon: &TLDRDaemon, flush: Flush) {
    match flush {
        Flush::Delta(files) => {
            let _ = daemon.process_dirty_files(files).await;
        }
        Flush::FullRebuild => daemon.schedule_full_rebuild().await,
    }
}

#[cfg(test)]
mod tests {
    use super::{Flush, LiveIgnoreMatcher, WatchPipeline, WatchPipelineConfig};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn config() -> WatchPipelineConfig {
        WatchPipelineConfig {
            debounce: Duration::from_millis(750),
            max_wait: Duration::from_secs(5),
            burst_file_cap: 200,
            burst_event_cap: 1_000,
            burst_window: Duration::from_secs(2),
        }
    }

    #[test]
    fn ignore_policy_reloads_during_a_live_session() {
        let project = tempfile::tempdir().expect("project");
        let ignored = project.path().join("generated/file.rs");
        let policy = project.path().join(".tldrignore");
        let live = LiveIgnoreMatcher::new(project.path());
        assert!(!live.is_ignored(&ignored, false));

        std::fs::write(&policy, "generated/\n").expect("write");
        assert!(live.reload_for_paths(std::slice::from_ref(&policy)));
        assert!(live.is_ignored(&ignored, false));

        std::fs::remove_file(&policy).expect("remove");
        assert!(live.reload_for_paths(&[policy]));
        assert!(!live.is_ignored(&ignored, false));

        let git_policy = project.path().join(".gitignore");
        std::fs::write(&git_policy, "generated/\n").expect("write gitignore");
        assert!(live.reload_for_paths(std::slice::from_ref(&git_policy)));
        assert!(live.is_ignored(&ignored, false));

        std::fs::remove_file(&git_policy).expect("remove gitignore");
        assert!(live.reload_for_paths(&[git_policy]));
        assert!(!live.is_ignored(&ignored, false));
    }

    #[test]
    fn ignore_policy_ordinary_source_event_does_not_reload() {
        let project = tempfile::tempdir().expect("project");
        let live = LiveIgnoreMatcher::new(project.path());
        std::fs::write(project.path().join(".tldrignore"), "generated/\n").expect("write");

        assert!(!live.reload_for_paths(&[project.path().join("src/lib.rs")]));
        assert!(!live.is_ignored(&project.path().join("generated/file.rs"), false));
    }

    #[test]
    fn repeated_saves_to_one_file_coalesce_to_one_delta() {
        let started = Instant::now();
        let mut pipeline = WatchPipeline::new(config());
        let file = PathBuf::from("/project/src/lib.rs");

        assert_eq!(pipeline.accept(file.clone(), started), None);
        assert_eq!(
            pipeline.accept(file.clone(), started + Duration::from_millis(100)),
            None
        );
        assert_eq!(
            pipeline.accept(file.clone(), started + Duration::from_millis(200)),
            None
        );
        assert_eq!(
            pipeline.flush_due(started + Duration::from_millis(949)),
            None
        );
        assert_eq!(
            pipeline.flush_due(started + Duration::from_millis(950)),
            Some(Flush::Delta(vec![file]))
        );
    }

    #[test]
    fn steady_stream_flushes_at_first_event_max_wait() {
        let started = Instant::now();
        let mut pipeline = WatchPipeline::new(config());
        let file = PathBuf::from("/project/src/lib.rs");

        for half_second in 0..10 {
            assert_eq!(
                pipeline.accept(
                    file.clone(),
                    started + Duration::from_millis(half_second * 500),
                ),
                None
            );
        }
        assert_eq!(
            pipeline.flush_due(started + Duration::from_millis(4_999)),
            None
        );
        assert_eq!(
            pipeline.flush_due(started + Duration::from_secs(5)),
            Some(Flush::Delta(vec![file]))
        );
    }

    #[test]
    fn noisy_file_does_not_rearm_another_files_quiet_timer() {
        let started = Instant::now();
        let mut pipeline = WatchPipeline::new(config());
        let quiet = PathBuf::from("/project/src/quiet.rs");
        let noisy = PathBuf::from("/project/src/noisy.rs");

        assert_eq!(pipeline.accept(quiet.clone(), started), None);
        assert_eq!(pipeline.accept(noisy.clone(), started), None);
        assert_eq!(
            pipeline.accept(noisy.clone(), started + Duration::from_millis(700)),
            None
        );
        assert_eq!(
            pipeline.flush_due(started + Duration::from_millis(750)),
            Some(Flush::Delta(vec![noisy, quiet]))
        );
    }

    #[test]
    fn file_cap_escalates_and_clears_pending_deltas() {
        let started = Instant::now();
        let mut pipeline = WatchPipeline::new(config());

        for index in 0..200 {
            assert_eq!(
                pipeline.accept(
                    PathBuf::from(format!("/project/src/file_{index}.rs")),
                    started
                ),
                None
            );
        }
        assert_eq!(
            pipeline.accept(PathBuf::from("/project/src/overflow.rs"), started),
            Some(Flush::FullRebuild)
        );
        assert_eq!(pipeline.flush_due(started + Duration::from_secs(10)), None);
    }

    #[test]
    fn rolling_event_cap_spans_quiet_flushes() {
        let started = Instant::now();
        let mut cfg = config();
        cfg.debounce = Duration::from_micros(1);
        let mut pipeline = WatchPipeline::new(cfg);
        let file = PathBuf::from("/project/src/lib.rs");

        for index in 0..1_000 {
            let now = started + Duration::from_micros(index * 1_500);
            assert_eq!(pipeline.accept(file.clone(), now), None);
            if index % 100 == 99 {
                assert!(matches!(
                    pipeline.flush_due(now + Duration::from_micros(1)),
                    Some(Flush::Delta(_))
                ));
            }
        }
        assert_eq!(
            pipeline.accept(file, started + Duration::from_millis(1_500)),
            Some(Flush::FullRebuild)
        );
    }
}

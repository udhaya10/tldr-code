//! Presence-based liveness tracking for the daemon (TLDR-3w5, epic TLDR-cxa).
//!
//! The idle question is NOT "is anyone talking to my socket?" — it is "is
//! anyone alive in this project?". The daemon self-terminates only when the
//! project is dormant: no client, no file activity, and no in-flight internal
//! work for a full `idle_timeout`.
//!
//! Two complementary mechanisms:
//!
//! - **Per-source presence timestamps** ([`Source`]): each liveness source
//!   (socket accept, CLI poke, watcher event, internal work completion) gets
//!   its own relaxed `AtomicU64` of millis-since-tracker-epoch. Per-source —
//!   not a single collapsed timestamp — so `daemon status` (TLDR-qzc) can
//!   answer "what kept me alive".
//! - **Busy tokens with age** ([`BusyGuard`]): RAII guards held for the
//!   lifetime of long internal work (index build, per-file delta). While any
//!   token is live the daemon never idles out ("never abandon your own job").
//!   Tokens record a label + start `Instant` so hung work is *visible* as
//!   stale-busy (age keeps growing) rather than silently immortal — RAII
//!   covers panic-unwind, but not hangs; observability covers hangs.
//!
//! Idle predicate: `all per-source presences stale && busy_count == 0`.
//!
//! CRITICAL placement rule for busy guards: move the guard INTO the
//! `spawn_blocking` closure (`move || { let _g = guard; ... }`), never hold it
//! in the awaiting async task. The `tldr warm` client times out at 30s and the
//! connection task may be cancelled with it — a guard owned by the closure
//! lives exactly as long as the blocking work, immune to that cancellation.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A liveness source. Each gets an independent presence timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum Source {
    /// A client connection was accepted on the IPC socket.
    Socket = 0,
    /// A non-daemon-routed CLI invocation poked us (TLDR-nke; reserved,
    /// unwired until that child lands).
    #[allow(dead_code)] // wired by TLDR-nke (CLI-wide poke)
    CliPoke = 1,
    /// The filesystem watcher saw a project write (post-debounce, pre-corpus
    /// -filter, self-writes excluded — see `watcher::presence_decision`).
    Watcher = 2,
    /// Internal work (build / delta) completed. Touched on `BusyGuard` drop so
    /// the idle countdown restarts when a 90-minute build finishes instead of
    /// killing the freshly-warmed daemon immediately (socket presence is long
    /// stale by then).
    Internal = 3,
}

const SOURCE_COUNT: usize = 4;

/// Display names in `Source` order, for `daemon status` (TLDR-qzc).
pub(crate) const SOURCE_NAMES: [&str; SOURCE_COUNT] = ["socket", "cli_poke", "watcher", "internal"];

/// A live unit of internal work, for the busy snapshot (TLDR-qzc).
#[derive(Debug, Clone)]
pub(crate) struct BusyInfo {
    pub label: &'static str,
    pub age: Duration,
}

struct BusyEntry {
    label: &'static str,
    started: Instant,
}

/// Tracks project presence across all liveness sources. Cheap to touch from
/// any thread (relaxed atomic store) — including the sync notify watcher
/// thread, which must never block.
pub(crate) struct ActivityTracker {
    /// Epoch all source timestamps are relative to (tracker creation =
    /// daemon start, which itself counts as presence: all sources init to 0).
    epoch: Instant,
    /// Millis-since-epoch of last presence, indexed by `Source`.
    sources: [AtomicU64; SOURCE_COUNT],
    /// Lock-free busy count for the hot idle-loop read; the mutexed map below
    /// is only for ages/snapshot.
    busy_count: AtomicUsize,
    busy: Mutex<std::collections::HashMap<u64, BusyEntry>>,
    next_token: AtomicU64,
}

impl ActivityTracker {
    pub(crate) fn new() -> Self {
        Self {
            epoch: Instant::now(),
            sources: [const { AtomicU64::new(0) }; SOURCE_COUNT],
            busy_count: AtomicUsize::new(0),
            busy: Mutex::new(std::collections::HashMap::new()),
            next_token: AtomicU64::new(0),
        }
    }

    fn now_millis(&self) -> u64 {
        // u64 millis overflows after ~584M years of uptime; fine.
        self.epoch.elapsed().as_millis() as u64
    }

    /// Record presence from `source`. Relaxed store — safe and non-blocking
    /// from any thread, including notify's sync watcher thread.
    pub(crate) fn touch(&self, source: Source) {
        self.sources[source as usize].store(self.now_millis(), Ordering::Relaxed);
    }

    /// Begin a unit of internal work. Hold the returned guard for the work's
    /// exact lifetime (inside the `spawn_blocking` closure — see module docs).
    pub(crate) fn begin(self: &Arc<Self>, label: &'static str) -> BusyGuard {
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        self.busy.lock().expect("busy lock poisoned").insert(
            token,
            BusyEntry {
                label,
                started: Instant::now(),
            },
        );
        self.busy_count.fetch_add(1, Ordering::Relaxed);
        BusyGuard {
            tracker: Arc::clone(self),
            token,
        }
    }

    /// Number of live busy tokens.
    pub(crate) fn busy_count(&self) -> usize {
        self.busy_count.load(Ordering::Relaxed)
    }

    /// Snapshot of live busy tokens with ages, oldest first (TLDR-qzc).
    pub(crate) fn busy_snapshot(&self) -> Vec<BusyInfo> {
        let mut infos: Vec<BusyInfo> = self
            .busy
            .lock()
            .expect("busy lock poisoned")
            .values()
            .map(|e| BusyInfo {
                label: e.label,
                age: e.started.elapsed(),
            })
            .collect();
        infos.sort_by(|a, b| b.age.cmp(&a.age));
        infos
    }

    /// Age of each source's last presence, in `Source` order (TLDR-qzc).
    pub(crate) fn presence_ages(&self) -> [Duration; SOURCE_COUNT] {
        let now = self.now_millis();
        std::array::from_fn(|i| {
            Duration::from_millis(now.saturating_sub(self.sources[i].load(Ordering::Relaxed)))
        })
    }

    /// Age of the freshest presence across all sources.
    pub(crate) fn freshest_presence_age(&self) -> Duration {
        self.presence_ages().into_iter().min().unwrap_or_default()
    }

    /// The idle predicate: dormant only if EVERY source's presence is older
    /// than `timeout` AND no internal work is in flight.
    pub(crate) fn is_idle(&self, timeout: Duration) -> bool {
        self.busy_count() == 0 && self.freshest_presence_age() > timeout
    }
}

/// RAII token for in-flight internal work. Dropping (including via
/// panic-unwind) ends the busy state and touches [`Source::Internal`] so the
/// idle countdown restarts from work completion.
pub(crate) struct BusyGuard {
    tracker: Arc<ActivityTracker>,
    token: u64,
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.tracker
            .busy
            .lock()
            .expect("busy lock poisoned")
            .remove(&self.token);
        self.tracker.busy_count.fetch_sub(1, Ordering::Relaxed);
        self.tracker.touch(Source::Internal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests use a tiny timeout + a real sleep to make all sources stale —
    /// backdating the epoch `Instant` is not portable (underflow panic on a
    /// freshly-booted machine).
    const TIMEOUT: Duration = Duration::from_millis(20);

    fn stale_tracker() -> Arc<ActivityTracker> {
        let t = Arc::new(ActivityTracker::new());
        std::thread::sleep(2 * TIMEOUT);
        assert!(t.is_idle(TIMEOUT), "precondition: all sources stale");
        t
    }

    #[test]
    fn fresh_tracker_is_not_idle() {
        // Daemon start counts as presence: all sources init to 0 == epoch.
        let t = ActivityTracker::new();
        assert!(!t.is_idle(TIMEOUT));
    }

    #[test]
    fn touch_defeats_idle_per_source() {
        for source in [
            Source::Socket,
            Source::CliPoke,
            Source::Watcher,
            Source::Internal,
        ] {
            let t = stale_tracker();
            t.touch(source);
            assert!(!t.is_idle(TIMEOUT), "touch({source:?}) must defeat idle");
        }
    }

    /// THE original bug (TLDR-3w5), encoded: a daemon whose every presence
    /// timestamp is stale must NOT idle out while internal work (the 90-min
    /// index build) is in flight.
    #[test]
    fn busy_token_defeats_idle_even_with_all_sources_stale() {
        let t = stale_tracker();
        let guard = t.begin("warm-build");
        std::thread::sleep(2 * TIMEOUT); // work outlives the idle timeout
        assert!(!t.is_idle(TIMEOUT), "busy must defeat idle");
        assert_eq!(t.busy_count(), 1);
        drop(guard);
        assert_eq!(t.busy_count(), 0);
    }

    /// Guard drop must restart the idle countdown (touch Internal): when a
    /// 90-min build finishes, socket presence is long stale — without the
    /// drop-touch the daemon would idle-kill the freshly warmed cache
    /// immediately.
    #[test]
    fn guard_drop_restarts_idle_countdown() {
        let t = stale_tracker();
        drop(t.begin("warm-build"));
        assert!(
            !t.is_idle(TIMEOUT),
            "idle countdown must restart at work completion, not work start"
        );
    }

    #[test]
    fn guard_dropped_on_panic_unwind() {
        let t = stale_tracker();
        let t2 = Arc::clone(&t);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _g = t2.begin("delta");
            panic!("simulated worker panic");
        }));
        assert_eq!(t.busy_count(), 0, "RAII must release busy on unwind");
        assert!(!t.is_idle(TIMEOUT), "drop-touch still applies");
    }

    #[test]
    fn busy_snapshot_reports_label_and_age_oldest_first() {
        let t = Arc::new(ActivityTracker::new());
        let _g1 = t.begin("warm-build");
        std::thread::sleep(Duration::from_millis(10));
        let _g2 = t.begin("delta");
        let snap = t.busy_snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].label, "warm-build", "oldest first");
        assert_eq!(snap[1].label, "delta");
        assert!(snap[0].age >= snap[1].age);
    }

    #[test]
    fn presence_ages_are_per_source() {
        let t = stale_tracker();
        t.touch(Source::Watcher);
        let ages = t.presence_ages();
        assert!(ages[Source::Watcher as usize] < TIMEOUT);
        assert!(ages[Source::Socket as usize] > TIMEOUT);
    }
}

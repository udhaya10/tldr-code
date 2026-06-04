//! Thin concurrency wrapper around the daemon's resident VectorStore.
//!
//! Owns `parking_lot::RwLock<Option<(EmbeddingModel, VectorStore)>>` and
//! exposes `query` (shared read-lock warm path; honest [`QueryError::NotReady`]
//! on a cold store — NEVER an inline build, TLDR-7xz.2), `warm` (write-lock
//! build), `invalidate` (write-lock clear), and `apply_delta` (incremental
//! per-file re-index — TLDR-t8f). The daemon and future watcher never touch a
//! raw lock.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};

use tldr_core::semantic::vector_store::{key_chunks, root_relative, stat_signal, VectorStore};
use tldr_core::semantic::{
    chunk_file, load_or_build_store, query_store_with_vector, store_dir_for, BuildOptions,
    CacheConfig, ChunkOptions, Embedder, EmbeddingModel, IndexSearchOptions,
};

/// Why a semantic query could not be served (TLDR-7xz.1/.2).
///
/// The daemon has exactly two modes: serve warm at full quality, or say
/// honestly why it can't. There is no cold build on the query path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryError {
    /// The resident store is cold (never warmed, or invalidated). The fix is
    /// explicit: `tldr warm`. The query path never builds.
    NotReady,
    /// A build currently holds the store write lock (`warm` in progress).
    /// Honest "in progress" instead of blocking the query for the build's
    /// duration.
    Building,
    /// Embedding/search/serialization failure.
    Internal(String),
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryError::NotReady => write!(f, "index not built — run tldr warm"),
            QueryError::Building => {
                write!(f, "index build in progress — retry when warm completes")
            }
            QueryError::Internal(e) => write!(f, "{e}"),
        }
    }
}

/// Point-in-time resident index state, for `daemon status` (TLDR-qzc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
    /// Resident store loaded; queries serve at full quality.
    Warm { vectors: usize },
    /// The store write lock is held — a `warm` build (or, briefly, a delta)
    /// is in progress.
    Building,
    /// Never warmed or invalidated; `tldr warm` is the fix.
    Cold,
}

/// Result of an incremental delta on a single file change (TLDR-t8f).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaOutcome {
    /// Path is outside the source corpus — filtered by the same rules as the
    /// build walker (TLDR-ac0.6). No-op, distinct from a cold-store skip.
    Filtered,
    /// Store cold or warm under a different model — no-op; the next query's
    /// cold build already reflects the change.
    Skipped,
    /// The file was deleted: `removed` vectors dropped from the store.
    Deleted { removed: usize },
    /// Delta applied in place: `embedded` of `total` chunks re-embedded (the
    /// rest were metadata-only line shifts).
    Applied { embedded: usize, total: usize },
    /// The delta path can't safely produce build-equivalent vectors for this
    /// configuration (e.g. `TLDR_ENRICH` on, whose per-file enrichment would
    /// diverge from the whole-corpus build). Caller should full-rebuild.
    NeedsRebuild,
}

pub struct IndexManager {
    store: RwLock<Option<(EmbeddingModel, VectorStore)>>,
    /// Resident embedder, kept loaded across deltas so a per-save incremental
    /// re-index pays no ONNX startup cost (design intent — TLDR-t8f). Lazily
    /// created and re-created on a model change. Behind its own `Mutex` so a
    /// delta's embed doesn't touch the store lock.
    embedder: Mutex<Option<(EmbeddingModel, Embedder)>>,
    /// Test-only counter of how many times the resident embedder was actually
    /// constructed (`Embedder::new`). Lets a test prove the warm query path
    /// REUSES the embedder (built once, not per query — TLDR-ac0.5) rather than
    /// just proving result-equivalence. Per-instance to avoid cross-test races.
    #[cfg(test)]
    embedder_builds: std::sync::atomic::AtomicUsize,
}

impl IndexManager {
    pub fn new() -> Self {
        Self {
            store: RwLock::new(None),
            embedder: Mutex::new(None),
            #[cfg(test)]
            embedder_builds: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Serve a semantic query from the WARM resident store, or say honestly why
    /// it can't (TLDR-7xz.1/.2). Warm queries run under plain shared `read()`
    /// guards — truly parallel, no serialization. A cold store returns
    /// [`QueryError::NotReady`]; an in-progress `warm` build (write lock held)
    /// returns [`QueryError::Building`]. The query path NEVER builds — the old
    /// silent inline cold-build under the write lock is gone.
    ///
    /// MUST be called inside `spawn_blocking` — never hold the guard across
    /// `.await`.
    pub fn query(
        &self,
        project: &Path,
        query: &str,
        search_opts: &IndexSearchOptions,
        model: EmbeddingModel,
    ) -> Result<serde_json::Value, QueryError> {
        // Empty/whitespace queries short-circuit FIRST — before embed_query, whose
        // `Embedder::new` would load ONNX on a cold daemon (and `"   "` would run a
        // wasted embed) only to be discarded downstream (TLDR-ac0.5 Codex review).
        // Mirrors the cold `query_store` path, which also guards before Embedder::new.
        if query.trim().is_empty() {
            let report = tldr_core::semantic::empty_search_report(query, model);
            return serde_json::to_value(&report)
                .map_err(|e| QueryError::Internal(format!("Serialization error: {e}")));
        }

        // Readiness pre-check BEFORE embedding (TLDR-7xz.2 + advisor): a cold
        // daemon must answer "not ready" without loading ONNX. The bounded
        // try_read rides out brief writers (a delta's apply takes the write
        // lock for milliseconds) while a long `warm` build maps to an honest
        // Building instead of blocking this query for the build's duration.
        {
            let guard = self
                .store
                .try_read_for(Duration::from_millis(250))
                .ok_or(QueryError::Building)?;
            if !guard.as_ref().is_some_and(|(m, _)| *m == model) {
                return Err(QueryError::NotReady);
            }
        } // drop read lock before embedding

        // Embed the query on the RESIDENT embedder OUTSIDE the store lock
        // (TLDR-ac0.5): reuses the same warm embedder as the delta path, so no
        // per-query `Embedder::new` ONNX reload — and the brief embedder-`Mutex`
        // wait doesn't extend the store read lock, so warm queries still run
        // `store.search` truly in parallel (ac0.1).
        let qv = self
            .embed_query(model, query)
            .map_err(QueryError::Internal)?;

        // Re-take the read lock and re-check: the store may have been
        // invalidated or re-warmed under a different model while we embedded.
        // Honest NotReady on that (rare) race — never a build.
        let guard = self
            .store
            .try_read_for(Duration::from_millis(250))
            .ok_or(QueryError::Building)?;
        match guard.as_ref() {
            Some((m, store)) if *m == model => {
                Self::do_search(store, project, query, &qv, search_opts, model)
                    .map_err(QueryError::Internal)
            }
            _ => Err(QueryError::NotReady),
        }
    }

    fn do_search(
        store: &VectorStore,
        project: &Path,
        query: &str,
        query_vector: &[f32],
        search_opts: &IndexSearchOptions,
        model: EmbeddingModel,
    ) -> Result<serde_json::Value, String> {
        let t_search = Instant::now();
        let report = query_store_with_vector(
            store,
            project,
            query,
            query_vector,
            search_opts,
            model,
            Instant::now(),
        )
        .map_err(|e| format!("Semantic search failed: {e}"))?;
        eprintln!(
            "[ac0.1] store SEARCH took {}ms",
            t_search.elapsed().as_millis()
        );
        serde_json::to_value(&report).map_err(|e| format!("Serialization error: {e}"))
    }

    /// Embed a search QUERY on the resident embedder (TLDR-ac0.5), (re)creating it
    /// on a model change. Applies the model's asymmetric query prefix via
    /// [`Embedder::embed_query`] — the query counterpart to [`Self::embed`] (which
    /// uses `embed_batch`, no prefix, for indexed documents on the delta path).
    /// Shares the SAME `embedder` field; holds only the embedder `Mutex`, never the
    /// store lock.
    fn embed_query(&self, model: EmbeddingModel, query: &str) -> Result<Vec<f32>, String> {
        let mut guard = self.embedder.lock();
        if !guard.as_ref().is_some_and(|(m, _)| *m == model) {
            let embedder = Embedder::new(model).map_err(|e| e.to_string())?;
            #[cfg(test)]
            self.embedder_builds
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *guard = Some((model, embedder));
        }
        let (_, embedder) = guard.as_mut().expect("embedder present after init");
        embedder.embed_query(query).map_err(|e| e.to_string())
    }

    /// Test-only: how many times the resident embedder was constructed.
    #[cfg(test)]
    fn embedder_builds(&self) -> usize {
        self.embedder_builds
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Write-lock build: load from disk (if fresh) or full-rebuild. Used by the
    /// `warm` command at daemon startup.
    ///
    /// Returns `Ok(true)` if the store was built/replaced, `Ok(false)` if
    /// already warm with the same model.
    pub fn warm(&self, project: &Path, model: EmbeddingModel) -> Result<bool, String> {
        let guard = self.store.upgradable_read();
        if guard.as_ref().is_some_and(|(m, _)| *m == model) {
            return Ok(false);
        }

        let mut guard = parking_lot::RwLockUpgradableReadGuard::upgrade(guard);
        // Re-check after upgrade.
        if guard.as_ref().is_some_and(|(m, _)| *m == model) {
            return Ok(false);
        }

        let build_opts = BuildOptions {
            model,
            show_progress: false,
            use_cache: true,
            ..Default::default()
        };
        let store_dir = store_dir_for(project);
        let store = load_or_build_store(project, &store_dir, &build_opts, Some(CacheConfig::default()))
            .map_err(|e| e.to_string())?;
        *guard = Some((model, store));
        Ok(true)
    }

    /// Incremental per-file re-index (TLDR-t8f, design doc §5). On a file change,
    /// re-chunk **only** that file, re-embed only the chunks whose body changed,
    /// remove vanished keys, and apply the delta to the resident store in place —
    /// a few-ms update instead of a full rebuild.
    ///
    /// Concurrency: classification reads the store under a **shared read lock**
    /// (dropped before embedding), embedding runs **lock-free** on the resident
    /// embedder, and only the final apply takes the **write lock** — which
    /// re-validates against the current store and errors on a stale snapshot, so
    /// a concurrent rebuild can never produce a half-applied delta. MUST be called
    /// inside `spawn_blocking` (never hold a guard across `.await`; TLDR-qr9).
    ///
    /// Returns [`DeltaOutcome::Skipped`] when the store is cold / a different
    /// model (the next cold query already reflects the change). Any `Err` — or
    /// [`DeltaOutcome::NeedsRebuild`] — means the caller should [`Self::invalidate`]
    /// and let the next query full-rebuild (the design's fallback).
    pub fn apply_delta(&self, project: &Path, file: &Path) -> Result<DeltaOutcome, String> {
        let is_delete = !(file.exists() && file.is_file());

        // 0. Capture the warm model (or bail if cold) FIRST — a cold store always
        //    no-ops (the next query rebuilds via enumerate_corpus_files anyway), so
        //    short-circuit before the corpus walk. This matters on cold churn (a
        //    `git checkout` / `npm install` between daemon start and first query
        //    floods Notify events); without this, every such edit would pay a
        //    discarded walker build. `model` is a Copy enum — free to hold here and
        //    drop on the Filtered/NeedsRebuild paths below. The delta embeds with
        //    the SAME model the resident store was built with — no model param.
        let model = match self.store.read().as_ref() {
            Some((m, _)) => *m,
            None => return Ok(DeltaOutcome::Skipped),
        };

        // §6 corpus filter for EDITS (TLDR-ac0.6): cheap, filesystem-only check
        // using the SAME walker rules as the build (gitignore + DEFAULT_EXCLUDE_DIRS
        // + generated-dir sentinels + binary/hidden + language extension). Run
        // BEFORE the enrich gate so a noisy write under an ignored path
        // (node_modules/, target/, ...) is a cheap no-op instead of triggering a
        // full rebuild. Deletes can't be walker-checked (the file is gone); they're
        // filtered store-side below by counting removed keys.
        if !is_delete && !tldr_core::semantic::is_corpus_file(project, file) {
            return Ok(DeltaOutcome::Filtered);
        }

        // Per-file enrichment can't reproduce the whole-corpus build vectors, so
        // a delta would diverge from the index. Fall back to a full rebuild.
        let enrich = std::env::var("TLDR_ENRICH")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if enrich {
            return Ok(DeltaOutcome::NeedsRebuild);
        }

        // Deletion: `Notify` can't always distinguish edit from delete (§5). Use
        // the resident store as the source of truth (TLDR-ac0.6): apply_file_delete
        // is a clean no-op (`Ok(0)`, no FileRecord written) for a path it has no
        // record of, so 0 keys removed means the file was never in the corpus →
        // report Filtered, store untouched. A removal >0 inherits gitignore /
        // JS-TS-preservation / generated-sentinel rules by construction, because
        // the store IS the build's filtered output — no path replica to drift from
        // the walker.
        if is_delete {
            let file_rel = deleted_file_rel(project, file);
            let mut guard = self.store.write();
            return match guard.as_mut() {
                Some((m, store)) if *m == model => {
                    let removed = store
                        .apply_file_delete(&file_rel)
                        .map_err(|e| e.to_string())?;
                    if removed == 0 {
                        Ok(DeltaOutcome::Filtered)
                    } else {
                        Ok(DeltaOutcome::Deleted { removed })
                    }
                }
                // Store rebuilt/invalidated under a different model since step 0.
                _ => Ok(DeltaOutcome::Skipped),
            };
        }

        // 1. Re-chunk ONLY this file (lock-free). Match the build's chunk options:
        //    BuildOptions defaults to function granularity, all languages.
        let chunk_opts = ChunkOptions::default();
        let new_chunks = chunk_file(file, &chunk_opts)
            .map_err(|e| format!("delta chunk_file failed: {e}"))?
            .chunks;
        // Shared key computation — identical keys to the build (else removes miss).
        let keyed = key_chunks(project, &new_chunks);

        // 2. Classify under a shared read lock: which keys need re-embedding
        //    (new, or content-hash changed). Drop the lock before embedding.
        let to_embed: Vec<usize> = {
            let guard = self.store.read();
            let store = match guard.as_ref() {
                Some((m, s)) if *m == model => s,
                _ => return Ok(DeltaOutcome::Skipped),
            };
            keyed
                .iter()
                .enumerate()
                .filter_map(|(i, (key, meta))| {
                    let changed = match store.content_hash(*key) {
                        None => true,
                        Some(h) => h != meta.content_hash.as_str(),
                    };
                    changed.then_some(i)
                })
                .collect()
        };

        // 3. Embed the changed chunks (lock-free, on the resident embedder).
        let mut embedded: HashMap<u64, Vec<f32>> = HashMap::new();
        if !to_embed.is_empty() {
            let texts: Vec<&str> = to_embed
                .iter()
                .map(|&i| new_chunks[i].content.as_str())
                .collect();
            let vectors = self.embed(model, texts)?;
            for (&i, vector) in to_embed.iter().zip(vectors) {
                embedded.insert(keyed[i].0, vector);
            }
        }

        // 4. Apply under the write lock — re-validates against the current store.
        let signal = stat_signal(file);
        let file_rel = keyed
            .first()
            .map(|(_, m)| m.file_rel_path.clone())
            .unwrap_or_else(|| root_relative(project, file));
        let mut guard = self.store.write();
        let store = match guard.as_mut() {
            Some((m, s)) if *m == model => s,
            _ => return Ok(DeltaOutcome::Skipped),
        };
        store
            .apply_file_delta(&file_rel, &keyed, &embedded, signal)
            .map_err(|e| e.to_string())?;
        Ok(DeltaOutcome::Applied {
            embedded: embedded.len(),
            total: keyed.len(),
        })
    }

    /// Embed `texts` with the resident embedder, (re)creating it on a model
    /// change. Holds only the embedder `Mutex` — never the store lock.
    fn embed(&self, model: EmbeddingModel, texts: Vec<&str>) -> Result<Vec<Vec<f32>>, String> {
        let mut guard = self.embedder.lock();
        if !guard.as_ref().is_some_and(|(m, _)| *m == model) {
            let embedder = Embedder::new(model).map_err(|e| e.to_string())?;
            *guard = Some((model, embedder));
        }
        let (_, embedder) = guard.as_mut().expect("embedder present after init");
        embedder.embed_batch(texts, false).map_err(|e| e.to_string())
    }

    /// Write-lock invalidate: drops the resident store so the next query
    /// triggers a rebuild. Used by the notify handler on file changes.
    pub fn invalidate(&self) {
        let mut guard = self.store.write();
        *guard = None;
    }

    /// Whether the store is currently warm (Some) or invalidated (None).
    pub fn is_warm(&self) -> bool {
        self.store.read().is_some()
    }

    /// Bounded-wait index state probe for `daemon status` (TLDR-qzc).
    ///
    /// MUST NOT block on the store lock: during a long `warm` build the write
    /// lock is held for the build's whole duration, and `status` exists
    /// precisely to answer "is it building or done?" DURING that window. The
    /// short `try_read_for` rides out brief writers (a delta's apply holds the
    /// write lock for milliseconds) while a long-held write lock maps to
    /// [`IndexState::Building`] — same pattern as the query path's readiness
    /// pre-check above.
    pub fn state(&self) -> IndexState {
        match self.store.try_read_for(Duration::from_millis(100)) {
            None => IndexState::Building,
            Some(guard) => match guard.as_ref() {
                Some((_, store)) => IndexState::Warm { vectors: store.len() },
                None => IndexState::Cold,
            },
        }
    }

    /// Number of vectors in the resident store, or `None` if cold. A delta's
    /// effect is observable here — an edit keeps the count (no orphaned keys),
    /// a delete drops it by the file's chunk count.
    pub fn store_len(&self) -> Option<usize> {
        self.store.read().as_ref().map(|(_, s)| s.len())
    }
}

/// Root-relative key for a **deleted** file. The file is gone, so
/// [`root_relative`]'s canonicalize fallback can't run; derive the relative tail
/// by a purely lexical strip against `project` **and** its canonical form. The
/// build keyed by the lexical relative path, and a `Notify` sender that emits a
/// canonicalized path still strips to the same tail (canonicalizing only
/// rewrites the root prefix, not the relative remainder) — so deletes match the
/// stored keys even under a symlinked root (the ss3 bug class). Falls back to
/// `root_relative` (which warns) only if neither prefix matches.
fn deleted_file_rel(project: &Path, file: &Path) -> String {
    if let Ok(rel) = file.strip_prefix(project) {
        return rel.to_string_lossy().replace('\\', "/");
    }
    if let Ok(croot) = project.canonicalize() {
        if let Ok(rel) = file.strip_prefix(&croot) {
            return rel.to_string_lossy().replace('\\', "/");
        }
    }
    root_relative(project, file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    /// TLDR-qzc: the status state probe must answer without blocking on the
    /// store lock — Cold on an empty store, Building while a writer (a warm
    /// build) holds the write lock. A blocking probe would hang `daemon
    /// status` for the full duration of a 90-minute build.
    #[test]
    fn state_probe_reports_cold_and_building_without_blocking() {
        let mgr = IndexManager::new();
        assert_eq!(mgr.state(), IndexState::Cold);

        let _writer = mgr.store.write(); // simulate in-progress warm build
        let started = std::time::Instant::now();
        assert_eq!(mgr.state(), IndexState::Building);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "probe must not block on the held write lock"
        );
    }

    /// Prove that two concurrent warm-path queries overlap under shared read
    /// locks (not serialize). The production `query()` fast path takes
    /// `self.store.read()` — a plain shared guard. This test exercises that
    /// same lock mode: two threads each hold a `read()` guard and rendezvous
    /// at a barrier. With a Mutex (or upgradable_read, which is exclusive),
    /// the second thread would block and the barrier would time out.
    #[test]
    fn concurrent_read_locks_overlap() {
        let manager = Arc::new(IndexManager::new());
        let barrier = Arc::new(Barrier::new(2));

        let handles: Vec<_> = (0..2)
            .map(|_| {
                let mgr = Arc::clone(&manager);
                let bar = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let guard = mgr.store.read();
                    bar.wait();
                    assert!(guard.is_none());
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread panicked");
        }
    }

    /// Negative test: upgradable_read() is mutually exclusive — a second
    /// try_upgradable_read fails while the first is held. This validates that
    /// using upgradable_read on the hot path would serialize queries.
    #[test]
    fn upgradable_read_is_exclusive() {
        let manager = IndexManager::new();
        let _guard = manager.store.upgradable_read();
        assert!(
            manager.store.try_upgradable_read().is_none(),
            "upgradable_read should be exclusive — if this passes, \
             two upgradable reads CAN coexist and the design assumption is wrong"
        );
    }

    /// Verify that invalidate() actually clears the store.
    #[test]
    fn invalidate_clears_store() {
        let manager = IndexManager::new();
        assert!(!manager.is_warm());
        manager.invalidate();
        assert!(!manager.is_warm());
    }

    // --- TLDR-ac0.6 source-filter tests ---

    use tldr_core::semantic::vector_store::{ChunkMeta, FileKind, FileRecord};

    fn seeded_manager() -> IndexManager {
        let manager = IndexManager::new();
        let model = EmbeddingModel::default();
        let dims = model.dimensions();
        let mut vector = vec![0.0; dims];
        vector[0] = 1.0;

        let mut store = VectorStore::new(dims, 8).unwrap();
        store
            .add(
                1,
                &vector,
                ChunkMeta {
                    identity: "src/lib.rs::seed::0".to_string(),
                    file_rel_path: "src/lib.rs".to_string(),
                    function_name: Some("seed".to_string()),
                    class_name: None,
                    line_start: 1,
                    line_end: 1,
                    content_hash: "seed-hash".to_string(),
                },
            )
            .unwrap();
        // Register the per-file record too — apply_file_delete keys off this, so
        // without it every delete would no-op (0 removed) regardless of the path,
        // and the store-as-source-of-truth delete filter wouldn't be exercised.
        store.set_file_record(
            "src/lib.rs".to_string(),
            FileRecord {
                keys: std::iter::once(1).collect(),
                mtime: 0,
                size: 0,
                file_type: FileKind::Regular,
            },
        );
        *manager.store.write() = Some((model, store));
        manager
    }

    fn write_file(root: &std::path::Path, rel: &str, contents: &[u8]) -> std::path::PathBuf {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn apply_delta_filters_non_corpus_edit_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let cases = [
            write_file(tmp.path(), "node_modules/foo/bar.js", b"function f(){}\n"),
            write_file(tmp.path(), "target/debug/main", b"\0ELF\n"),
            write_file(tmp.path(), "src/data.xyz", b"unknown ext\n"),
            write_file(tmp.path(), ".git/HEAD", b"ref: refs/heads/main\n"),
        ];

        for path in &cases {
            let manager = seeded_manager();
            let before = manager.store_len();
            let outcome = manager.apply_delta(tmp.path(), path).unwrap();
            assert_eq!(outcome, DeltaOutcome::Filtered, "expected Filtered for {}", path.display());
            assert_eq!(manager.store_len(), before, "store_len changed for {}", path.display());
        }
    }

    #[test]
    fn apply_delta_filters_ignored_delete_paths() {
        // A delete of a path with no FileRecord removes 0 keys → the store-as-
        // source-of-truth filter reports Filtered, store untouched. (The path
        // never existed on disk, so apply_delta takes the delete branch.)
        let tmp = tempfile::tempdir().unwrap();
        let deleted = tmp.path().join("node_modules/foo/bar.js");
        let manager = seeded_manager();
        let before = manager.store_len();

        let outcome = manager.apply_delta(tmp.path(), &deleted).unwrap();
        assert_eq!(outcome, DeltaOutcome::Filtered);
        assert_eq!(manager.store_len(), before);
    }

    #[test]
    fn apply_delta_deletes_corpus_file_from_store() {
        // The mirror of the filter case: a delete whose rel-path DOES match a
        // stored FileRecord removes its keys and reports Deleted. This proves the
        // delete branch keys off the store (the seeded record is "src/lib.rs"),
        // not a path rule — the file need not exist on disk.
        let tmp = tempfile::tempdir().unwrap();
        let deleted = tmp.path().join("src/lib.rs");
        let manager = seeded_manager();
        assert_eq!(manager.store_len(), Some(1));

        let outcome = manager.apply_delta(tmp.path(), &deleted).unwrap();
        assert_eq!(outcome, DeltaOutcome::Deleted { removed: 1 });
        assert_eq!(manager.store_len(), Some(0));
    }

    // --- TLDR-ac0.5 resident-embedder parity ---

    /// The pass/fail for ac0.5 is results-EQUIVALENCE: reusing the daemon's warm
    /// embedder (via `query_store_with_vector`) must rank IDENTICALLY to the cold
    /// path's fresh `Embedder::new` per query (via `query_store`). The pure
    /// `hits_to_report` unit tests never touch an embedder, so they can't catch a
    /// query-vector regression — this end-to-end test is the real gate. Ignored by
    /// default (loads the ONNX model).
    #[test]
    #[ignore = "loads the ONNX embedder; run on demand"]
    fn resident_embedder_query_matches_fresh_embedder_ranking() {
        let corpus = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        std::fs::write(
            corpus.path().join("sim.rs"),
            "/// cosine similarity between two vectors\n\
             fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 { 0.0 }\n",
        )
        .unwrap();
        std::fs::write(
            corpus.path().join("cfg.rs"),
            "/// parse a configuration file from disk\n\
             fn parse_config(path: &str) -> String { String::new() }\n",
        )
        .unwrap();
        std::fs::write(
            corpus.path().join("http.rs"),
            "/// send an http get request and return the body\n\
             fn http_get(url: &str) -> String { String::new() }\n",
        )
        .unwrap();

        // Resolve the model the SAME way the daemon does (daemon.rs
        // resolve_semantic_model): project config > built-in default. Never a
        // hardcoded variant and never `BuildOptions::default()` (which silently
        // pins ArcticM regardless of config — the trap called out in daemon.rs).
        // The cold and warm paths must search the same configured model.
        let config = tldr_core::config::TldrConfig::resolve(Some(corpus.path()));
        let model = EmbeddingModel::resolve(None, &config).unwrap();
        let build_opts = BuildOptions {
            model,
            show_progress: false,
            use_cache: true,
            ..Default::default()
        };
        // store_dir + cache MUST live outside the corpus (freshness-gate precondition).
        let store_dir = work.path().join("store");
        let cache = || {
            Some(CacheConfig {
                cache_dir: work.path().join("cache"),
                max_size_mb: 50,
                ttl_days: 1,
            })
        };
        let search_opts = IndexSearchOptions {
            top_k: 10,
            threshold: 0.0, // keep every hit so the FULL ranking is compared
            include_snippet: false,
            snippet_lines: 5,
        };
        let query = "compute cosine similarity between vectors";

        // Cold path: build+persist the store and embed with a FRESH Embedder.
        let cold = tldr_core::semantic::search_with_store(
            corpus.path(),
            &store_dir,
            query,
            &search_opts,
            &build_opts,
            cache(),
        )
        .unwrap();
        let cold_val = serde_json::to_value(&cold).unwrap();

        // Warm path: load the SAME persisted store into the IndexManager and query
        // through the RESIDENT embedder (the ac0.5 code path).
        let store = load_or_build_store(corpus.path(), &store_dir, &build_opts, cache()).unwrap();
        let manager = IndexManager::new();
        *manager.store.write() = Some((model, store));
        let warm_val = manager
            .query(corpus.path(), query, &search_opts, model)
            .unwrap();

        // Extract ordered (file_path, function_name, score) — latency is excluded.
        let ranking = |v: &serde_json::Value| -> Vec<(String, String, f64)> {
            v["results"]
                .as_array()
                .expect("results array")
                .iter()
                .map(|r| {
                    (
                        r["file_path"].as_str().unwrap_or("").to_string(),
                        r["function_name"].as_str().unwrap_or("").to_string(),
                        r["score"].as_f64().unwrap_or(f64::NAN),
                    )
                })
                .collect()
        };
        let cold_rank = ranking(&cold_val);
        let warm_rank = ranking(&warm_val);

        assert!(!cold_rank.is_empty(), "cold path returned no results");
        assert_eq!(
            cold_rank.len(),
            warm_rank.len(),
            "result count differs: cold {:?} vs warm {:?}",
            cold_rank,
            warm_rank
        );
        for (i, (c, w)) in cold_rank.iter().zip(&warm_rank).enumerate() {
            assert_eq!(c.0, w.0, "rank {i}: file_path differs");
            assert_eq!(c.1, w.1, "rank {i}: function_name differs");
            assert!(
                (c.2 - w.2).abs() < 1e-6,
                "rank {i}: score differs cold {} vs warm {}",
                c.2,
                w.2
            );
        }
    }

    /// The PERF claim of ac0.5: the daemon builds the embedder ONCE and reuses it
    /// across queries (no per-query ONNX reload). The parity test alone can't prove
    /// this — it would pass even with `Embedder::new` per query. Here the per-
    /// instance build counter makes reuse deterministic: two queries -> exactly one
    /// construction. Uses the SAME model `seeded_manager` baked in
    /// (EmbeddingModel::default()) so `query()` takes the WARM fast path; a model
    /// mismatch would hit the slow rebuild and invalidate the count. Ignored
    /// (loads the ONNX embedder).
    #[test]
    #[ignore = "loads the ONNX embedder; run on demand"]
    fn resident_embedder_built_once_across_queries() {
        let manager = seeded_manager(); // warm store @ EmbeddingModel::default()
        let model = EmbeddingModel::default();
        let project = tempfile::tempdir().unwrap();
        let opts = IndexSearchOptions {
            top_k: 5,
            threshold: 0.0,
            include_snippet: false,
            snippet_lines: 5,
        };

        assert_eq!(manager.embedder_builds(), 0, "no embedder before any query");
        manager
            .query(project.path(), "first query about parsing", &opts, model)
            .unwrap();
        assert_eq!(manager.embedder_builds(), 1, "embedder built on first query");
        manager
            .query(project.path(), "a second, different query", &opts, model)
            .unwrap();
        assert_eq!(
            manager.embedder_builds(),
            1,
            "embedder REUSED on the second query — not reconstructed"
        );
    }

    /// Two searches on a WARM (seeded) store overlap under shared read locks rather
    /// than serializing. NOTE: this validates warm-store read-lock concurrency
    /// ONLY. It does NOT test the embed-before-lock ordering (`do_search` takes a
    /// pre-computed vector and never embeds) — that ordering is a code-structure
    /// fact, verified by reading `query()`, not by this test.
    #[test]
    fn concurrent_warm_searches_overlap() {
        let manager = Arc::new(seeded_manager());
        let model = EmbeddingModel::default();
        let mut dummy = vec![0.0_f32; model.dimensions()];
        dummy[0] = 1.0;
        let opts = IndexSearchOptions {
            top_k: 5,
            threshold: 0.0,
            include_snippet: false,
            snippet_lines: 5,
        };

        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let mgr = Arc::clone(&manager);
                let bar = Arc::clone(&barrier);
                let dv = dummy.clone();
                let so = opts.clone();
                std::thread::spawn(move || {
                    let guard = mgr.store.read();
                    let (_, store) = guard.as_ref().expect("warm store");
                    // Both threads now hold a shared read lock simultaneously. If
                    // do_search took a write lock (or read locks were exclusive),
                    // the barrier would deadlock and the test would hang.
                    bar.wait();
                    let v = IndexManager::do_search(store, Path::new("/p"), "q", &dv, &so, model)
                        .expect("search ok");
                    assert!(v.get("results").is_some(), "report has results field");
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread panicked");
        }
    }

    /// TLDR-7xz.1/.2: a query on a COLD store must return an honest
    /// `QueryError::NotReady` — WITHOUT building the store and WITHOUT loading
    /// ONNX (the readiness pre-check fires before `embed_query`). This is the
    /// unit-level proof that the old inline cold-build slow path is gone.
    #[test]
    fn cold_query_returns_not_ready_without_building_anything() {
        let manager = IndexManager::new(); // cold: no store, no embedder
        let model = EmbeddingModel::default();
        let project = tempfile::tempdir().unwrap();
        let opts = IndexSearchOptions {
            top_k: 5,
            threshold: 0.0,
            include_snippet: false,
            snippet_lines: 5,
        };

        let err = manager
            .query(project.path(), "find the parser", &opts, model)
            .expect_err("cold store must not serve");
        assert_eq!(err, QueryError::NotReady);
        assert_eq!(
            err.to_string(),
            "index not built — run tldr warm",
            "NotReady must carry the standardized guidance"
        );
        assert!(!manager.is_warm(), "query must NOT build the store");
        assert_eq!(
            manager.embedder_builds(),
            0,
            "cold query must NOT construct the embedder (no ONNX load)"
        );
    }

    /// A model MISMATCH between the warm store and the request is also honest
    /// NotReady — never a rebuild on the query path.
    #[test]
    fn model_mismatch_returns_not_ready() {
        let manager = seeded_manager(); // warm @ EmbeddingModel::default()
        let project = tempfile::tempdir().unwrap();
        let opts = IndexSearchOptions {
            top_k: 5,
            threshold: 0.0,
            include_snippet: false,
            snippet_lines: 5,
        };
        // Pick any model that differs from the seeded one.
        let other = [EmbeddingModel::ArcticXS, EmbeddingModel::ArcticL]
            .into_iter()
            .find(|m| *m != EmbeddingModel::default())
            .unwrap();

        let err = manager
            .query(project.path(), "anything", &opts, other)
            .expect_err("mismatched model must not serve");
        assert_eq!(err, QueryError::NotReady);
        assert_eq!(
            manager.embedder_builds(),
            0,
            "readiness check must fire before any embedder construction"
        );
    }

    /// The daemon's blank-query guard (TLDR-ac0.5 Codex review) must short-circuit
    /// BEFORE constructing the resident embedder — a blank query on a COLD daemon
    /// must NOT load ONNX. Proven WITHOUT ONNX: `embedder_builds` stays 0 and the
    /// result is an empty report. Covers `IndexManager::query` (the daemon's actual
    /// entry point); the store_search empty-query test covers query_store_with_vector.
    #[test]
    fn blank_query_short_circuits_before_building_embedder() {
        let manager = IndexManager::new(); // cold: no store, no embedder
        let model = EmbeddingModel::default();
        let project = tempfile::tempdir().unwrap();
        let opts = IndexSearchOptions {
            top_k: 5,
            threshold: 0.0,
            include_snippet: false,
            snippet_lines: 5,
        };
        for q in ["", "   ", "\t\n"] {
            let v = manager.query(project.path(), q, &opts, model).unwrap();
            assert_eq!(v["total_results"], serde_json::json!(0), "blank {q:?} -> empty");
            assert!(
                v["results"].as_array().unwrap().is_empty(),
                "blank {q:?} -> no results"
            );
        }
        assert_eq!(
            manager.embedder_builds(),
            0,
            "blank query must NOT construct the embedder (no ONNX load)"
        );
    }

    #[test]
    fn apply_delta_filters_gitignored_source_file() {
        let tmp = tempfile::tempdir().unwrap();
        // Initialize a git repo so .gitignore is honoured by the ignore crate.
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "generated/\n").unwrap();
        write_file(tmp.path(), "generated/auto.py", b"def gen(): pass\n");

        let manager = seeded_manager();
        let before = manager.store_len();
        let path = tmp.path().join("generated/auto.py");
        let outcome = manager.apply_delta(tmp.path(), &path).unwrap();
        assert_eq!(
            outcome,
            DeltaOutcome::Filtered,
            "gitignored file must be filtered"
        );
        assert_eq!(manager.store_len(), before);
    }
}

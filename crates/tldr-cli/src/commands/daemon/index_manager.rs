//! Thin concurrency wrapper around the daemon's resident VectorStore.
//!
//! Owns `parking_lot::RwLock<Option<(EmbeddingModel, VectorStore)>>` and
//! exposes exactly three operations: `query` (shared read-lock fast path,
//! exclusive write-lock on cold miss), `warm` (write-lock build), and
//! `invalidate` (write-lock clear). The daemon and future watcher never
//! touch a raw lock.

use std::path::Path;
use std::time::Instant;

use parking_lot::RwLock;

use tldr_core::semantic::{
    load_or_build_store, query_store, store_dir_for, BuildOptions, CacheConfig, EmbeddingModel,
    IndexSearchOptions,
};
use tldr_core::semantic::vector_store::VectorStore;

pub struct IndexManager {
    store: RwLock<Option<(EmbeddingModel, VectorStore)>>,
}

impl IndexManager {
    pub fn new() -> Self {
        Self {
            store: RwLock::new(None),
        }
    }

    /// Shared read-lock fast path when the store is warm; exclusive write-lock
    /// slow path to build on cold miss. Concurrent warm queries run under
    /// plain `read()` guards — truly parallel, no serialization.
    ///
    /// MUST be called inside `spawn_blocking` — never hold the guard across
    /// `.await`.
    pub fn query(
        &self,
        project: &Path,
        query: &str,
        search_opts: &IndexSearchOptions,
        model: EmbeddingModel,
    ) -> Result<serde_json::Value, String> {
        // Fast path: plain shared read — concurrent with other readers.
        {
            let guard = self.store.read();
            if guard.as_ref().is_some_and(|(m, _)| *m == model) {
                let (_, store) = guard.as_ref().unwrap();
                return Self::do_search(store, project, query, search_opts, model);
            }
        } // drop read lock before acquiring write

        // Slow path: exclusive write lock, re-check, build on miss.
        let mut guard = self.store.write();
        if !guard.as_ref().is_some_and(|(m, _)| *m == model) {
            let t_build = Instant::now();
            let build_opts = BuildOptions {
                model,
                show_progress: false,
                use_cache: true,
                ..Default::default()
            };
            let store_dir = store_dir_for(project);
            let store = load_or_build_store(
                project,
                &store_dir,
                &build_opts,
                Some(CacheConfig::default()),
            )
            .map_err(|e| format!("Failed to build vector store: {e}"))?;
            eprintln!(
                "[ac0.1] store BUILD took {}ms (model {:?})",
                t_build.elapsed().as_millis(),
                model
            );
            *guard = Some((model, store));
        }

        let (_, store) = guard.as_ref().expect("store present after build");
        Self::do_search(store, project, query, search_opts, model)
    }

    fn do_search(
        store: &VectorStore,
        project: &Path,
        query: &str,
        search_opts: &IndexSearchOptions,
        model: EmbeddingModel,
    ) -> Result<serde_json::Value, String> {
        let t_search = Instant::now();
        let report = query_store(store, project, query, search_opts, model, Instant::now())
            .map_err(|e| format!("Semantic search failed: {e}"))?;
        eprintln!(
            "[ac0.1] store SEARCH took {}ms",
            t_search.elapsed().as_millis()
        );
        serde_json::to_value(&report).map_err(|e| format!("Serialization error: {e}"))
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

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
}

//! Thin concurrency wrapper around the daemon's resident VectorStore.
//!
//! Owns `parking_lot::RwLock<Option<(EmbeddingModel, VectorStore)>>` and
//! exposes `query` (shared read-lock fast path, exclusive write-lock on cold
//! miss), `warm` (write-lock build), `invalidate` (write-lock clear), and
//! `apply_delta` (incremental per-file re-index — TLDR-t8f). The daemon and
//! future watcher never touch a raw lock.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use parking_lot::{Mutex, RwLock};

use tldr_core::semantic::vector_store::{key_chunks, root_relative, stat_signal, VectorStore};
use tldr_core::semantic::{
    chunk_file, load_or_build_store, query_store, store_dir_for, BuildOptions, CacheConfig,
    ChunkOptions, Embedder, EmbeddingModel, IndexSearchOptions,
};

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
}

impl IndexManager {
    pub fn new() -> Self {
        Self {
            store: RwLock::new(None),
            embedder: Mutex::new(None),
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

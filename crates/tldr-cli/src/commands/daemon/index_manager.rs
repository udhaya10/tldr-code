//! Thin concurrency wrapper around the daemon's resident VectorStore.
//!
//! Owns the resident store plus independent query, delta, and bulk inference
//! runners. Full builds happen outside the store lock and publish briefly;
//! query and delta inference likewise release the store before ONNX execution.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use tldr_core::semantic::vector_store::{
    chunk_id_key, key_chunks_reconciled, plan_structural_delta_from_artifact, root_relative,
    stat_signal, VectorStore,
};
use tldr_core::semantic::{
    query_store_with_vector, store_dir_for, BuildCancellation, BuildOptions, BulkInferenceRunner,
    CacheConfig, ChunkGranularity, EmbeddingModel, FixedShapeInferenceRunner, GenerationManager,
    GenerationSelection, IndexSearchOptions, InferenceRunnerSnapshot,
};

use super::bulk_worker::BulkWorker;

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
    /// Batch-one query session. Never shared with document workloads.
    query_runner: FixedShapeInferenceRunner,
    /// Small fixed-shape delta session. Never shared with queries or bulk.
    delta_runner: FixedShapeInferenceRunner,
    /// Serialized full-build boundary. Epic 10 moves this into a child process.
    bulk_runner: BulkInferenceRunner,
}

impl Default for IndexManager {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexManager {
    pub fn new() -> Self {
        Self {
            store: RwLock::new(None),
            query_runner: FixedShapeInferenceRunner::query(),
            delta_runner: FixedShapeInferenceRunner::delta(),
            bulk_runner: BulkInferenceRunner::default(),
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

        // Embed on the batch-one QUERY session outside the store lock. Delta
        // and bulk work own different sessions and cannot pollute its arena.
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

    /// Embed a search query on the dedicated batch-one fixed-shape runner.
    fn embed_query(&self, model: EmbeddingModel, query: &str) -> Result<Vec<f32>, String> {
        self.query_runner.embed_query(model, query)
    }

    /// Build a replacement outside the store lock, then publish under a brief
    /// write guard. Used by the `warm` command at daemon startup.
    ///
    /// Returns `Ok(true)` if the store was built/replaced, `Ok(false)` if
    /// already warm with the same model.
    pub fn warm(
        &self,
        project: &Path,
        model: EmbeddingModel,
        source_chunks: Vec<tldr_core::semantic::CodeChunk>,
    ) -> Result<bool, String> {
        if self
            .store
            .read()
            .as_ref()
            .is_some_and(|(resident_model, _)| *resident_model == model)
        {
            return Ok(false);
        }
        self.bulk_runner.run(model, || {
            // Re-check after serializing competing warm calls.
            if self
                .store
                .read()
                .as_ref()
                .is_some_and(|(resident_model, _)| *resident_model == model)
            {
                return Ok(false);
            }
            let build_opts = BuildOptions {
                model,
                show_progress: false,
                use_cache: true,
                ..Default::default()
            };
            let store_dir = store_dir_for(project);
            let requested =
                std::env::var("TLDR_SEMANTIC_GENERATION").unwrap_or_else(|_| "active".into());
            let selection = GenerationSelection::parse(&requested)?;
            let worker = BulkWorker::installed()?;
            worker.build(
                project,
                &store_dir,
                &build_opts,
                Some(CacheConfig::default()),
                &BuildCancellation::default(),
                &source_chunks,
            )?;
            let identity = tldr_core::semantic::store_search::manifest_id_for(project, &build_opts);
            let manager = GenerationManager::open(&store_dir).map_err(|error| error.to_string())?;
            let replacement = match selection {
                GenerationSelection::Active => manager.load(&identity),
                GenerationSelection::Previous => {
                    manager.select_previous(&identity).and_then(|store| {
                        store.ok_or_else(|| {
                            tldr_core::TldrError::Embedding(
                                "no previous complete generation is retained".into(),
                            )
                        })
                    })
                }
                GenerationSelection::Number(generation) => manager.select(generation, &identity),
            }
            .map_err(|error| error.to_string())?;
            // Publication alone takes the write lock; an existing generation
            // continues serving while the replacement is built.
            *self.store.write() = Some((model, replacement));
            Ok(true)
        })
    }

    /// Published semantic generation for joining the project manifest.
    pub fn active_generation(&self, project: &Path) -> Result<Option<u64>, String> {
        GenerationManager::open(&store_dir_for(project))
            .and_then(|manager| manager.active_generation())
            .map_err(|error| error.to_string())
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
    pub fn apply_delta(
        &self,
        project: &Path,
        file: &Path,
        source_chunk: Option<tldr_core::semantic::CodeChunk>,
    ) -> Result<DeltaOutcome, String> {
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

        // 1. Snapshot, then plan + compose ONLY this complete file with the
        // resident model's tokenizer. This is the same raw structural recipe
        // as a whole build.
        let planned_signal = stat_signal(file);
        let source_chunk =
            source_chunk.ok_or_else(|| "shared semantic source artifact is missing".to_string())?;
        let (new_chunks, documents) = self.delta_runner.with_token_budget(model, |budget| {
            plan_structural_delta_from_artifact(
                project,
                source_chunk,
                budget,
                ChunkGranularity::Function,
            )
            .map_err(|error| error.to_string())
        })?;
        let file_rel = root_relative(project, file);
        let prior = {
            let guard = self.store.read();
            match guard.as_ref() {
                Some((m, store)) if *m == model => store.file_chunk_meta(&file_rel),
                _ => return Ok(DeltaOutcome::Skipped),
            }
        };
        let keyed = key_chunks_reconciled(project, &new_chunks, &documents, &prior)
            .map_err(|error| error.to_string())?;
        let expected_old_keys = prior
            .iter()
            .map(|meta| chunk_id_key(meta.chunk_id))
            .collect();

        // 2. Classify under a shared read lock: which keys need re-embedding
        //    (new, or exact composed-document revision changed). Drop the lock
        //    before embedding.
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
                    let changed = match store.revision(*key) {
                        None => true,
                        Some(revision) => revision != meta.revision,
                    };
                    changed.then_some(i)
                })
                .collect()
        };

        // 3. Embed the changed chunks on the delta-only session, without the
        //    store lock.
        let mut embedded: HashMap<u64, Vec<f32>> = HashMap::new();
        if !to_embed.is_empty() {
            // TLDR-vbw0.1 Tier 1: route through embed_batch_indexed (via the
            // embed() helper below) which sorts by text length before
            // batching, collapsing ONNX input shapes so the CPU arena
            // plateaus instead of climbing across the run. The first tuple
            // element is the POSITION in to_embed/keyed/new_chunks (used to
            // key the writeback into `embedded` via keyed[i].0).
            let indexed: Vec<(usize, &str)> = to_embed
                .iter()
                .map(|&i| (i, documents[i].as_str()))
                .collect();
            let vectors = self.embed(model, indexed)?;
            for (i, vector) in vectors {
                embedded.insert(keyed[i].0, vector);
            }
        }

        // 4. Apply under the write lock — re-validates against the current store.
        let signal = stat_signal(file);
        if signal != planned_signal {
            return Ok(DeltaOutcome::NeedsRebuild);
        }
        let mut guard = self.store.write();
        let store = match guard.as_mut() {
            Some((m, s)) if *m == model => s,
            _ => return Ok(DeltaOutcome::Skipped),
        };
        store
            .apply_file_delta_reconciled(&file_rel, &expected_old_keys, &keyed, &embedded, signal)
            .map_err(|e| e.to_string())?;
        Ok(DeltaOutcome::Applied {
            embedded: embedded.len(),
            total: keyed.len(),
        })
    }

    /// Embed `(index, text)` pairs on the dedicated delta runner.
    fn embed(
        &self,
        model: EmbeddingModel,
        indexed: Vec<(usize, &str)>,
    ) -> Result<Vec<(usize, Vec<f32>)>, String> {
        self.delta_runner.embed_documents(model, indexed)
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
                Some((_, store)) => IndexState::Warm {
                    vectors: store.len(),
                },
                None if self.bulk_runner.snapshot().state == "busy" => IndexState::Building,
                None => IndexState::Cold,
            },
        }
    }

    /// Query, delta, and bulk runner state for daemon status.
    pub fn runner_states(&self) -> [InferenceRunnerSnapshot; 3] {
        [
            self.query_runner.snapshot(),
            self.delta_runner.snapshot(),
            self.bulk_runner.snapshot(),
        ]
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

//! Build-time instrumentation for the semantic embedding pipeline (TLDR-9bxa.1).
//!
//! **Observability only.** Recording never alters chunking, the embedding call,
//! cache semantics, or the resulting vectors. All collection is gated behind
//! [`crate::semantic::index::BuildOptions::collect_metrics`]; when it is `false`,
//! [`VectorStore::build`](crate::semantic::vector_store::VectorStore::build)
//! never constructs a [`BuildMetrics`] and is byte-identical to the
//! un-instrumented path. When it is `true`, the recording only *reads* input
//! lengths and timestamps and runs an independent RSS sampler thread — it does
//! not touch the texts, vectors, or cache, so vectors are unchanged either way.
//!
//! ## Scope (fastembed-rs is a black box)
//!
//! fastembed-rs performs the sub-[`EMBED_BATCH_SIZE`](crate::semantic::embedder) batching
//! and tokenization internally and exposes neither. Therefore **exact per-input
//! token lengths, true ONNX tensor shapes, padding ratio, and per-batch latency
//! are NOT observable** from the current backend. What this module captures
//! instead, and what becomes measurable only later:
//!
//! | Want | .1 (FastEmbed) | Later |
//! |---|---|---|
//! | token length | not observable; `input_bytes_*` is a proxy | TLDR-9bxa.5 (fixed-shape tokenizer) |
//! | true tensor shape (`batch × seq_len`) | not observable | TLDR-9bxa.5 |
//! | padding ratio | not observable | TLDR-9bxa.5 |
//! | per-batch latency | aggregate only (`embed_latency_ms`) | TLDR-9bxa.5 / .10 |
//! | per-batch grouping + input-length stats | ✅ derived from sorted inputs | — |
//! | cache hit/miss, throughput, RSS peak+timeline, phases, run metadata | ✅ | — |
//!
//! These limits are recorded verbatim in every emitted report's `limitations`.

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::semantic::embedder::EMBED_BATCH_SIZE;
use crate::semantic::index::BuildOptions;
use crate::util;

/// Metrics report schema version. Bump when the serialized shape of
/// [`MetricsReport`] changes in a way that breaks consumers.
pub const METRICS_SCHEMA_VERSION: u32 = 1;

/// Default RSS sampling interval for the timeline.
const DEFAULT_RSS_SAMPLE_INTERVAL_MS: u64 = 500;

// =============================================================================
// Report types (machine-readable; serialized to JSON by `tldr embed --metrics`).
// =============================================================================

/// A complete, serializable record of one instrumented embedding build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsReport {
    /// [`METRICS_SCHEMA_VERSION`] at emit time.
    pub schema_version: u32,
    /// Run identifier (`<unix-millis start>-<pid>`). Collides only for
    /// same-millisecond starts within one process, i.e. effectively never.
    pub run_id: String,
    /// Resolved embedding model.
    pub model: ModelInfo,
    /// Repository-relative or absolute root as passed to the build.
    pub root: String,
    /// Stat-only corpus digest captured before chunking (freshness-gate input).
    pub corpus_digest: u64,
    /// Snapshot of the build options that produced these vectors.
    pub options: BuildOptionsSummary,
    /// Build start time, unix epoch millis.
    pub started_at_unix_ms: u64,
    /// Total wall time of the instrumented build, millis. Excludes sampler
    /// teardown (captured before the sampler join).
    pub duration_ms: u64,
    /// Timed phase boundaries (`chunk`, `cache_lookup`, `model_load`, `embed`).
    pub phases: Vec<PhaseRecord>,
    /// Total chunks (cached + embedded).
    pub chunks_total: usize,
    /// Chunks served from the content-addressed cache (Phase 1 hits).
    pub chunks_cached: usize,
    /// Whether a dedup cache was actually opened (effective state), distinct
    /// from the requested `options.use_cache`.
    pub cache_opened: bool,
    /// Token-budget outcomes (TLDR-9bxa.2): per-corpus oversized accounting.
    /// `None` only if token checks were not run (tokenizer unavailable).
    pub token_budget: Option<crate::semantic::token_budget::TokenStats>,
    /// Chunks actually embedded via ONNX (Phase 2; == cache misses).
    pub chunks_embedded: usize,
    /// Per-batch shape descriptors, one entry per `EMBED_BATCH_SIZE` group in
    /// the (length-sorted) embed order fastembed feeds the session.
    pub batches: Vec<BatchShape>,
    /// Aggregate wall time of the embed call (`embed_batch_indexed`) covering
    /// all Phase-2 batches, millis. Excludes model load / integrity check (see
    /// the `model_load` phase) so the derived `embeddings_per_second` reflects
    /// inference throughput, not one-time init.
    pub embed_latency_ms: u64,
    /// RSS peak, final sample, and timeline.
    pub rss: RssSummary,
    /// Derived throughput rates.
    pub throughput: Throughput,
    /// Explicit, recorded scope limits (see module docs).
    pub limitations: Vec<String>,
}

/// Resolved embedding model identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Model name (e.g. `Xenova/snowflake-arctic-embed-l`).
    pub name: String,
    /// Embedding dimensionality.
    pub dimensions: usize,
}

/// Snapshot of the [`BuildOptions`] that produced a report's vectors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildOptionsSummary {
    /// Chunking granularity (`File` / `Function`).
    pub granularity: String,
    /// Whether the dedup cache was REQUESTED. The EFFECTIVE state (was one
    /// actually opened?) is [`MetricsReport::cache_opened`].
    pub use_cache: bool,
    /// Whether enriched (vs raw) text was embedded — mirrors `TLDR_ENRICH`.
    pub enrich: bool,
    /// Language-extension filter, if any.
    pub languages: Option<Vec<String>>,
    /// The fixed batch size fastembed forms internally (`EMBED_BATCH_SIZE`).
    pub batch_size: usize,
}

impl BuildOptionsSummary {
    /// Snapshot a [`BuildOptions`] for the report. `enrich` is the value
    /// [`VectorStore::build`] actually used (read once and shared), so the
    /// report cannot disagree with the embedded recipe.
    pub fn from_options(options: &BuildOptions, enrich: bool) -> Self {
        // Stable string label — NOT the `Debug` repr, which would silently
        // change for every consumer if a `ChunkGranularity` variant were
        // renamed (review n1).
        let granularity = match options.granularity {
            crate::semantic::ChunkGranularity::File => "file",
            crate::semantic::ChunkGranularity::Function => "function",
        };
        Self {
            granularity: granularity.to_string(),
            use_cache: options.use_cache,
            enrich,
            languages: options.languages.clone(),
            batch_size: EMBED_BATCH_SIZE,
        }
    }
}

/// One timed phase of the build (`chunk`, `cache_lookup`, `embed`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseRecord {
    /// Phase name.
    pub name: String,
    /// Phase wall duration, millis.
    pub duration_ms: u64,
    /// RSS sampled at the end of the phase, bytes.
    pub rss_bytes_at_end: Option<u64>,
}

/// Observable shape of one embed batch. `token_length_available` is always
/// `false` under the FastEmbed backend (see module docs / `limitations`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchShape {
    /// Zero-based batch index in embed order.
    pub index: usize,
    /// Number of inputs in this batch (last batch may be partial).
    pub size: usize,
    /// Smallest input byte length in the batch.
    pub input_bytes_min: usize,
    /// Mean input byte length in the batch (integer-truncated).
    pub input_bytes_mean: usize,
    /// Largest input byte length in the batch (drives fastembed's padding).
    pub input_bytes_max: usize,
    /// Whether a true token length is available (always `false` under FastEmbed).
    pub token_length_available: bool,
}

/// RSS summary: build-scoped peak, process-lifetime peak, final sample, timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RssSummary {
    /// Peak RSS observed during the build WINDOW (max of timeline + phase-end +
    /// final samples), bytes. Time-scoped to the build (vs
    /// [`RssSummary::process_peak_bytes`]'s process lifetime) — but the value is
    /// still total resident memory at each sample, so it INCLUDES any resident
    /// pages allocated before the build began.
    pub peak_bytes: Option<u64>,
    /// Process-lifetime peak via `getrusage(RUSAGE_SELF).ru_maxrss`, bytes.
    /// Cross-check against `peak_bytes`; on a fresh standalone `tldr embed`
    /// process the two should agree within ~10%.
    pub process_peak_bytes: Option<u64>,
    /// RSS sampled at build end, bytes.
    pub final_bytes: Option<u64>,
    /// Sampler cadence, millis.
    pub sample_interval_ms: u64,
    /// Sampled RSS over the build (offset from build start).
    pub timeline: Vec<RssSample>,
}

/// One RSS timeline sample.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RssSample {
    /// Milliseconds since build start.
    pub t_offset_ms: u64,
    /// Resident set size at this sample, bytes.
    pub rss_bytes: u64,
}

/// Derived throughput rates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Throughput {
    /// Total chunks (cached + embedded) per second over the whole build.
    pub chunks_per_second: f64,
    /// Embedded chunks per second over the embed phase.
    pub embeddings_per_second: f64,
}

// =============================================================================
// In-flight collector.
// =============================================================================

/// Collects instrumentation during a single [`VectorStore::build`]. Owns an
/// RSS sampler thread for the lifetime of the build; `finalize` (or `drop`)
/// stops and joins it.
pub struct BuildMetrics {
    model: ModelInfo,
    root: String,
    corpus_digest: u64,
    options: BuildOptionsSummary,
    start: Instant,
    started_at_unix_ms: u64,
    phases: Vec<PhaseRecord>,
    current_phase: Option<(String, Instant)>,
    cache_hits: usize,
    /// == cache misses == chunks embedded.
    cache_misses: usize,
    /// Whether a dedup cache was actually opened (effective, not requested).
    cache_opened: bool,
    /// Input byte lengths in the SAME (length-sorted ascending) order
    /// `embed_batch_indexed` feeds fastembed, so consecutive `EMBED_BATCH_SIZE`
    /// groups match the batches the session actually sees.
    input_lengths: Vec<usize>,
    embed_latency_ms: u64,
    /// Token-budget outcomes per input (TLDR-9bxa.2), accumulated into the report.
    token_stats: crate::semantic::token_budget::TokenStats,
    rss_samples: Arc<Mutex<Vec<RssSample>>>,
    /// Condvar-based shutdown signal so the sampler wakes and exits promptly
    /// (sub-millisecond) on finalize/drop, instead of polling a flag every
    /// 25ms (TLDR-9bxa.1 review).
    signal: Arc<SamplerSignal>,
    sampler_handle: Option<JoinHandle<()>>,
    sample_interval_ms: u64,
}

impl BuildMetrics {
    /// Begin collecting. Starts the RSS sampler thread immediately.
    pub fn new(
        model_name: impl Into<String>,
        dimensions: usize,
        root: impl Into<String>,
        corpus_digest: u64,
        options: &BuildOptions,
        enrich: bool,
    ) -> Self {
        let start = Instant::now();
        let started_at_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let sample_interval_ms = DEFAULT_RSS_SAMPLE_INTERVAL_MS;
        let rss_samples = Arc::new(Mutex::new(Vec::new()));
        let signal = SamplerSignal::new();
        let sampler_handle = Some(spawn_sampler(
            start,
            sample_interval_ms,
            Arc::clone(&rss_samples),
            Arc::clone(&signal),
        ));
        Self {
            model: ModelInfo {
                name: model_name.into(),
                dimensions,
            },
            root: root.into(),
            corpus_digest,
            options: BuildOptionsSummary::from_options(options, enrich),
            start,
            started_at_unix_ms,
            phases: Vec::new(),
            current_phase: None,
            cache_hits: 0,
            cache_misses: 0,
            cache_opened: false,
            input_lengths: Vec::new(),
            embed_latency_ms: 0,
            token_stats: crate::semantic::token_budget::TokenStats::default(),
            rss_samples,
            signal,
            sampler_handle,
            sample_interval_ms,
        }
    }

    /// Open a phase, closing any currently-open phase first.
    pub fn begin_phase(&mut self, name: &str) {
        self.end_phase();
        self.current_phase = Some((name.to_string(), Instant::now()));
    }

    /// Close the currently-open phase, recording its duration + end RSS.
    pub fn end_phase(&mut self) {
        if let Some((name, t)) = self.current_phase.take() {
            self.phases.push(PhaseRecord {
                name,
                duration_ms: t.elapsed().as_millis() as u64,
                rss_bytes_at_end: util::current_rss_bytes(),
            });
        }
    }

    /// Record cache hit/miss counts (misses == chunks embedded in Phase 2).
    pub fn record_cache(&mut self, hits: usize, misses: usize) {
        self.cache_hits = hits;
        self.cache_misses = misses;
    }

    /// Record whether a dedup cache was actually opened (effective state, not
    /// just the requested option — `use_cache` + `cache_config: None` still
    /// yields no cache).
    pub fn record_cache_opened(&mut self, opened: bool) {
        self.cache_opened = opened;
    }

    /// Record the byte lengths of all Phase-2 embed inputs, in length-sorted
    /// ascending order (matching `embed_batch_indexed`'s internal sort).
    pub fn record_embed_inputs(&mut self, lengths_sorted: Vec<usize>) {
        self.input_lengths = lengths_sorted;
    }

    /// Record the aggregate ONNX embed latency (Phase 2), millis.
    pub fn record_embed_latency_ms(&mut self, ms: u64) {
        self.embed_latency_ms = ms;
    }

    /// Record one input's token-budget check (TLDR-9bxa.2). Accumulated into
    /// the report's `token_budget` stats.
    pub fn record_token_check(
        &mut self,
        check: crate::semantic::token_budget::TokenCheck,
    ) {
        self.token_stats.record(check);
    }

    /// Stop the sampler, compute aggregates, and produce the final report.
    /// `chunks_total` is the full chunk count (cached + embedded).
    ///
    /// Takes `&mut self` (rather than consuming) because the type implements
    /// `Drop` to guarantee the sampler thread is joined — moving fields out of
    /// a `Drop` type is forbidden. The data structs are cloned into the report
    /// (cheap, one-time at build end); `self` is left drained and its `Drop`
    /// then no-ops (the sampler was already joined here).
    pub fn finalize(&mut self, chunks_total: usize) -> MetricsReport {
        // Close any open phase FIRST so its end is included, THEN capture total
        // build duration, THEN tear down the sampler. Ordering matters: the
        // final phase must close before duration is read (review), and the
        // sampler join must NOT be counted in duration.
        self.end_phase();
        let duration_ms = self.start.elapsed().as_millis() as u64;
        self.stop_sampler();
        let mut timeline = self
            .rss_samples
            .lock()
            .map(|t| t.clone())
            .unwrap_or_default();
        // Drop any sample taken AFTER duration_ms was captured: a sample can
        // land in the sub-ms window before the condvar shutdown reaches the
        // sampler, which would otherwise yield a timeline point past the
        // reported build duration (TLDR-9bxa.1 review).
        timeline.retain(|s| s.t_offset_ms <= duration_ms);
        let final_bytes = util::current_rss_bytes();
        // Build-scoped peak = the max RSS observed DURING this build (timeline
        // samples + phase-end samples + the final sample) — NOT the process-
        // lifetime `ru_maxrss`, which also covers everything before the build
        // (review). `ru_maxrss` is still reported as `process_peak_bytes` so the
        // OS figure remains available for cross-check (acceptance: agrees within
        // 10% on a fresh standalone `tldr embed` process).
        let mut peak_bytes = final_bytes;
        for s in &timeline {
            peak_bytes = max_opt(peak_bytes, Some(s.rss_bytes));
        }
        for p in &self.phases {
            peak_bytes = max_opt(peak_bytes, p.rss_bytes_at_end);
        }
        let chunks_embedded = self.cache_misses;
        let batches = compute_batch_shapes(&self.input_lengths, self.options.batch_size);
        let throughput = Throughput {
            chunks_per_second: ms_per_sec(duration_ms, chunks_total),
            embeddings_per_second: ms_per_sec(self.embed_latency_ms, chunks_embedded),
        };
        MetricsReport {
            schema_version: METRICS_SCHEMA_VERSION,
            run_id: format!("{}-{}", self.started_at_unix_ms, std::process::id()),
            model: self.model.clone(),
            root: self.root.clone(),
            corpus_digest: self.corpus_digest,
            options: self.options.clone(),
            started_at_unix_ms: self.started_at_unix_ms,
            duration_ms,
            phases: self.phases.clone(),
            chunks_total,
            chunks_cached: self.cache_hits,
            chunks_embedded,
            cache_opened: self.cache_opened,
            token_budget: Some(self.token_stats.clone()),
            batches,
            embed_latency_ms: self.embed_latency_ms,
            rss: RssSummary {
                peak_bytes,
                process_peak_bytes: util::peak_rss_bytes(),
                final_bytes,
                sample_interval_ms: self.sample_interval_ms,
                timeline,
            },
            throughput,
            limitations: fastembed_limitations(),
        }
    }

    fn stop_sampler(&mut self) {
        // Set the stop flag and notify the condvar so the sampler wakes from
        // its timed wait IMMEDIATELY (no up-to-25ms poll delay) and exits.
        if let Ok(mut g) = self.signal.stop.lock() {
            *g = true;
        }
        self.signal.cv.notify_all();
        if let Some(h) = self.sampler_handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for BuildMetrics {
    fn drop(&mut self) {
        // Never leak the sampler thread, even if finalize wasn't called.
        self.stop_sampler();
    }
}

fn ms_per_sec(duration_ms: u64, count: usize) -> f64 {
    if duration_ms > 0 {
        count as f64 * 1000.0 / duration_ms as f64
    } else {
        0.0
    }
}

/// Max of two optional u64s (`None` treated as absent).
fn max_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

/// Derive per-batch shape descriptors from the length-sorted embed inputs.
///
/// `sorted_lengths` MUST be in the same order `embed_batch_indexed` feeds
/// fastembed (byte length ascending); consecutive chunks of `batch_size` then
/// correspond to the batches the ONNX session actually receives. Pure and
/// side-effect-free so it can be unit-tested without a model.
pub fn compute_batch_shapes(sorted_lengths: &[usize], batch_size: usize) -> Vec<BatchShape> {
    if sorted_lengths.is_empty() || batch_size == 0 {
        return Vec::new();
    }
    sorted_lengths
        .chunks(batch_size)
        .enumerate()
        .map(|(index, chunk)| {
            // `chunk` is guaranteed non-empty: `chunks(batch_size>0)` never
            // yields an empty slice, and empty-input / `batch_size == 0`
            // early-return above. `unwrap()` (not `unwrap_or`) so a future
            // regression to those guards surfaces instead of being masked.
            let min = chunk.iter().copied().min().unwrap();
            let max = chunk.iter().copied().max().unwrap();
            let mean = chunk.iter().sum::<usize>() / chunk.len();
            BatchShape {
                index,
                size: chunk.len(),
                input_bytes_min: min,
                input_bytes_mean: mean,
                input_bytes_max: max,
                token_length_available: false,
            }
        })
        .collect()
}

/// Recorded scope limits (see module docs). Serialized into every report.
fn fastembed_limitations() -> Vec<String> {
    vec![
        "token_length_available=false: fastembed-rs tokenizes internally and does not expose per-input token counts; exact ONNX sequence lengths are unobservable under the current backend (deferred to TLDR-9bxa.5 fixed-shape backend)."
            .to_string(),
        "padding_ratio and true tensor shapes (batch x seq_len) are not observable under fastembed-rs; input_bytes_* are a byte-length proxy (deferred to TLDR-9bxa.5)."
            .to_string(),
        "per-batch latency is not observable without restructuring the embedding call, which TLDR-9bxa.1's vectors-unchanged non-goal forbids; embed_latency_ms is the aggregate over all batches (per-batch timing arrives with TLDR-9bxa.5 / .10)."
            .to_string(),
    ]
}

/// Condvar-based shutdown signal shared with the sampler thread. The sampler
/// waits on `cv` for the sample interval; `stop_sampler` sets `stop` and
/// notifies `cv`, waking the sampler immediately (no poll delay).
struct SamplerSignal {
    stop: Mutex<bool>,
    cv: Condvar,
}

impl SamplerSignal {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            stop: Mutex::new(false),
            cv: Condvar::new(),
        })
    }
}

/// Spawn the RSS sampler thread. Pushes `(t_offset_ms, rss_bytes)` samples at
/// `interval_ms` cadence, and exits within sub-millisecond of `stop_sampler`
/// via the condvar (TLDR-9bxa.1 review: was polling every 25ms).
fn spawn_sampler(
    start: Instant,
    interval_ms: u64,
    samples: Arc<Mutex<Vec<RssSample>>>,
    signal: Arc<SamplerSignal>,
) -> JoinHandle<()> {
    std::thread::spawn(move || loop {
        if let Some(rss) = util::current_rss_bytes() {
            let sample = RssSample {
                t_offset_ms: start.elapsed().as_millis() as u64,
                rss_bytes: rss,
            };
            if let Ok(mut guard) = samples.lock() {
                guard.push(sample);
            }
        }
        // Wait for the interval, OR return promptly when stopped. Poisoned
        // mutexes (a sampler panic — shouldn't happen) are recovered, not fatal.
        let guard = signal.stop.lock().unwrap_or_else(|p| p.into_inner());
        if *guard {
            break;
        }
        match signal
            .cv
            .wait_timeout(guard, Duration::from_millis(interval_ms))
        {
            Ok((g, _)) => {
                if *g {
                    break;
                }
            }
            Err(p) => {
                let (g, _) = p.into_inner();
                if *g {
                    break;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_shapes_full_and_partial_batches() {
        // Sorted ascending, batch_size 2 -> 3 batches, last partial (size 1).
        let lens = vec![10, 20, 30, 40, 50];
        let shapes = compute_batch_shapes(&lens, 2);
        assert_eq!(shapes.len(), 3);

        assert_eq!(shapes[0].index, 0);
        assert_eq!(shapes[0].size, 2);
        assert_eq!(shapes[0].input_bytes_min, 10);
        assert_eq!(shapes[0].input_bytes_mean, 15);
        assert_eq!(shapes[0].input_bytes_max, 20);

        assert_eq!(shapes[1].index, 1);
        assert_eq!(shapes[1].size, 2);
        assert_eq!(shapes[1].input_bytes_min, 30);
        assert_eq!(shapes[1].input_bytes_max, 40);

        // Partial last batch.
        assert_eq!(shapes[2].index, 2);
        assert_eq!(shapes[2].size, 1);
        assert_eq!(shapes[2].input_bytes_min, 50);
        assert_eq!(shapes[2].input_bytes_mean, 50);
        assert_eq!(shapes[2].input_bytes_max, 50);

        // Token length never observable under FastEmbed.
        for s in &shapes {
            assert!(!s.token_length_available);
        }
    }

    #[test]
    fn batch_shapes_empty_or_zero_batch_size() {
        assert!(compute_batch_shapes(&[], 32).is_empty());
        assert!(compute_batch_shapes(&[1, 2, 3], 0).is_empty());
    }

    #[test]
    fn batch_shapes_exact_multiple_no_partial() {
        let lens = vec![1, 2, 3, 4];
        let shapes = compute_batch_shapes(&lens, 2);
        assert_eq!(shapes.len(), 2);
        assert!(shapes.iter().all(|s| s.size == 2));
    }

    #[test]
    fn schema_version_is_one() {
        assert_eq!(METRICS_SCHEMA_VERSION, 1);
    }

    #[test]
    fn report_serde_roundtrip_preserves_batches_and_phases() {
        let report = MetricsReport {
            schema_version: METRICS_SCHEMA_VERSION,
            run_id: "123".to_string(),
            model: ModelInfo {
                name: "X".to_string(),
                dimensions: 768,
            },
            root: "/repo".to_string(),
            corpus_digest: 42,
            options: BuildOptionsSummary {
                granularity: "Function".to_string(),
                use_cache: true,
                enrich: false,
                languages: None,
                batch_size: 32,
            },
            started_at_unix_ms: 123,
            duration_ms: 1000,
            phases: vec![PhaseRecord {
                name: "embed".to_string(),
                duration_ms: 800,
                rss_bytes_at_end: Some(1_000_000),
            }],
            chunks_total: 5,
            chunks_cached: 2,
            cache_opened: false,
            token_budget: None,
            chunks_embedded: 3,
            batches: vec![BatchShape {
                index: 0,
                size: 3,
                input_bytes_min: 1,
                input_bytes_mean: 2,
                input_bytes_max: 3,
                token_length_available: false,
            }],
            embed_latency_ms: 800,
            rss: RssSummary {
                peak_bytes: Some(2_000_000),
                process_peak_bytes: Some(2_500_000),
                final_bytes: Some(1_000_000),
                sample_interval_ms: 500,
                timeline: vec![RssSample {
                    t_offset_ms: 0,
                    rss_bytes: 500_000,
                }],
            },
            throughput: Throughput {
                chunks_per_second: 5.0,
                embeddings_per_second: 3.75,
            },
            limitations: fastembed_limitations(),
        };
        let json = serde_json::to_string(&report).expect("serialize");
        let back: MetricsReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.chunks_embedded, 3);
        assert_eq!(back.batches.len(), 1);
        assert_eq!(back.batches[0].input_bytes_max, 3);
        assert_eq!(back.phases[0].name, "embed");
        assert_eq!(back.rss.peak_bytes, Some(2_000_000));
        assert!(!back.limitations.is_empty());
    }

    #[test]
    fn collector_records_phases_and_stops_sampler() {
        // Constructing a collector spawns a sampler thread; finalize must join it.
        let opts = BuildOptions::default();
        let mut m = BuildMetrics::new("ArcticM", 768, "/repo", 9, &opts, false);
        m.begin_phase("chunk");
        // Phase duration is >= 0; give the sampler a moment to record a sample.
        std::thread::sleep(Duration::from_millis(60));
        m.end_phase();
        m.record_cache(2, 3);
        m.record_embed_inputs(vec![5, 10, 15]);
        m.record_embed_latency_ms(250);
        let report = m.finalize(5);

        assert_eq!(report.chunks_total, 5);
        assert_eq!(report.chunks_cached, 2);
        assert_eq!(report.chunks_embedded, 3);
        assert_eq!(report.batches.len(), 1); // 3 inputs, batch_size 32 -> 1 batch
        assert_eq!(report.batches[0].size, 3);
        assert_eq!(report.embed_latency_ms, 250);
        // At least one phase recorded with a name.
        assert!(report.phases.iter().any(|p| p.name == "chunk"));
        // The sampler ran ~60ms at 500ms interval -> may have 0 or 1 samples;
        // either is fine, but the timeline field must exist and be bounded.
        assert!(report.rss.timeline.len() <= 2);
    }

    /// peak_bytes is the BUILD-SCOPED max (timeline + phase-end + final), not
    /// the process-lifetime ru_maxrss (review finding 3).
    #[test]
    fn peak_is_build_scoped_max_of_observations() {
        let opts = BuildOptions::default();
        let mut m = BuildMetrics::new("ArcticM", 768, "/repo", 0, &opts, false);
        m.begin_phase("chunk");
        // Let the sampler capture at least one sample.
        std::thread::sleep(Duration::from_millis(80));
        m.end_phase();
        let report = m.finalize(0);

        // Recompute the max over exactly the observations finalize used.
        let expected = report
            .rss
            .timeline
            .iter()
            .map(|s| s.rss_bytes)
            .chain(report.phases.iter().filter_map(|p| p.rss_bytes_at_end))
            .chain(report.rss.final_bytes.iter().copied())
            .max();
        assert_eq!(report.rss.peak_bytes, expected);
        assert!(report.rss.peak_bytes.is_some(), "build must observe at least one RSS sample");

        // A build-scoped peak can never exceed the process-lifetime peak.
        if let (Some(peak), Some(proc_peak)) =
            (report.rss.peak_bytes, report.rss.process_peak_bytes)
        {
            assert!(peak <= proc_peak, "build peak {peak} exceeds process peak {proc_peak}");
        }
    }

    #[test]
    fn dropped_unfinalized_collector_does_not_leak_thread() {
        // Just exercise Drop without finalize; if the thread weren't joined this
        // would still pass, but it guards the stop-on-drop contract.
        let opts = BuildOptions::default();
        let m = BuildMetrics::new("ArcticM", 768, "/repo", 0, &opts, false);
        drop(m);
    }
}

//! Reproducible performance evidence for the fixed-shape embedding candidate.
//!
//! The live benchmark is implemented in `examples/fixed_shape_bench.rs`; this
//! module owns its stable report schema, statistics, and rollout-gate logic so
//! the evidence can also be consumed by CI and later cold-build instrumentation.

use serde::{Deserialize, Serialize};

/// Four gibibytes, the hard resident-memory ceiling for fixed-shape rollout.
pub const DEFAULT_MAX_PEAK_RSS_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Sixty-four mebibytes, the allowed spread in the final RSS plateau window.
pub const DEFAULT_MAX_PLATEAU_SPREAD_BYTES: u64 = 64 * 1024 * 1024;

/// Quantitative rollout limits for one benchmark matrix.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct PerformanceGate {
    /// Maximum observed current resident set size during candidate execution.
    pub max_peak_rss_bytes: u64,
    /// Maximum allowed fractional throughput loss relative to FastEmbed.
    pub max_throughput_regression_fraction: f64,
    /// Number of final heterogeneous-shape cycles used to judge RSS stability.
    pub plateau_window: usize,
    /// Maximum RSS spread inside the final plateau window.
    pub max_plateau_spread_bytes: u64,
}

impl Default for PerformanceGate {
    fn default() -> Self {
        Self {
            max_peak_rss_bytes: DEFAULT_MAX_PEAK_RSS_BYTES,
            max_throughput_regression_fraction: 0.10,
            plateau_window: 3,
            max_plateau_spread_bytes: DEFAULT_MAX_PLATEAU_SPREAD_BYTES,
        }
    }
}

impl PerformanceGate {
    /// Validate gate values before accepting benchmark evidence.
    pub fn validate(self) -> Result<Self, String> {
        if self.max_peak_rss_bytes == 0 {
            return Err("maximum peak RSS must be positive".to_string());
        }
        if !self.max_throughput_regression_fraction.is_finite()
            || !(0.0..1.0).contains(&self.max_throughput_regression_fraction)
        {
            return Err("throughput regression fraction must be finite and in [0, 1)".to_string());
        }
        if self.plateau_window < 2 {
            return Err("RSS plateau window must contain at least two cycles".to_string());
        }
        Ok(self)
    }
}

/// Distribution summary for repeated inference durations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct LatencySummary {
    /// Number of measured calls.
    pub samples: usize,
    /// Arithmetic mean duration in milliseconds.
    pub mean_ms: f64,
    /// Nearest-rank median duration in milliseconds.
    pub p50_ms: f64,
    /// Nearest-rank 95th-percentile duration in milliseconds.
    pub p95_ms: f64,
    /// Longest measured duration in milliseconds.
    pub max_ms: f64,
}

impl LatencySummary {
    /// Summarize a non-empty collection of finite, non-negative durations.
    pub fn from_milliseconds(samples: &[f64]) -> Result<Self, String> {
        if samples.is_empty() {
            return Err("latency report requires at least one sample".to_string());
        }
        if samples
            .iter()
            .any(|sample| !sample.is_finite() || *sample < 0.0)
        {
            return Err("latency samples must be finite and non-negative".to_string());
        }
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        let mean_ms = sorted.iter().sum::<f64>() / sorted.len() as f64;
        Ok(Self {
            samples: sorted.len(),
            mean_ms,
            p50_ms: nearest_rank(&sorted, 0.50),
            p95_ms: nearest_rank(&sorted, 0.95),
            max_ms: *sorted.last().expect("non-empty checked above"),
        })
    }
}

/// Oracle-versus-candidate evidence for one exact tensor shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShapePerformanceReport {
    /// Exact ONNX sequence dimension.
    pub sequence: usize,
    /// Exact ONNX batch dimension and number of real benchmark requests.
    pub batch: usize,
    /// FastEmbed latency distribution for equal full batches.
    pub oracle_latency: LatencySummary,
    /// Direct fixed-shape ORT latency distribution for equal full batches.
    pub fixed_latency: LatencySummary,
    /// FastEmbed requests completed per second.
    pub oracle_requests_per_second: f64,
    /// Fixed-shape requests completed per second.
    pub fixed_requests_per_second: f64,
    /// Fractional candidate throughput loss; zero when the candidate is faster.
    pub throughput_regression_fraction: f64,
    /// Whether this shape satisfies the configured throughput gate.
    pub throughput_passed: bool,
}

impl ShapePerformanceReport {
    /// Build one shape report from repeated equal-work latency samples.
    pub fn from_samples(
        sequence: usize,
        batch: usize,
        oracle_ms: &[f64],
        fixed_ms: &[f64],
        gate: PerformanceGate,
    ) -> Result<Self, String> {
        if sequence == 0 || batch == 0 {
            return Err("benchmark tensor dimensions must be positive".to_string());
        }
        let gate = gate.validate()?;
        let oracle_latency = LatencySummary::from_milliseconds(oracle_ms)?;
        let fixed_latency = LatencySummary::from_milliseconds(fixed_ms)?;
        let oracle_requests_per_second = requests_per_second(batch, oracle_latency.mean_ms)?;
        let fixed_requests_per_second = requests_per_second(batch, fixed_latency.mean_ms)?;
        let throughput_regression_fraction =
            throughput_regression(oracle_requests_per_second, fixed_requests_per_second)?;
        Ok(Self {
            sequence,
            batch,
            oracle_latency,
            fixed_latency,
            oracle_requests_per_second,
            fixed_requests_per_second,
            throughput_regression_fraction,
            throughput_passed: throughput_regression_fraction
                <= gate.max_throughput_regression_fraction,
        })
    }
}

/// Candidate RSS behavior over repeated heterogeneous-shape cycles.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RssPlateauReport {
    /// Resident set size immediately before loading the candidate session.
    pub baseline_rss_bytes: u64,
    /// Highest sampled current RSS from candidate load through measured calls.
    pub peak_rss_bytes: u64,
    /// Current RSS after every complete 128/256/384/512 cycle.
    pub cycle_end_rss_bytes: Vec<u64>,
    /// Max-minus-min RSS in the configured final cycle window.
    pub final_window_spread_bytes: u64,
    /// Whether the sampled peak stays below the hard memory ceiling.
    pub peak_passed: bool,
    /// Whether the final cycle window stays within the allowed spread.
    pub plateau_passed: bool,
}

impl RssPlateauReport {
    /// Evaluate sampled RSS evidence against the configured gate.
    pub fn from_samples(
        baseline_rss_bytes: u64,
        sampled_peak_rss_bytes: u64,
        cycle_end_rss_bytes: Vec<u64>,
        gate: PerformanceGate,
    ) -> Result<Self, String> {
        let gate = gate.validate()?;
        if cycle_end_rss_bytes.len() < gate.plateau_window {
            return Err(format!(
                "RSS report needs at least {} complete cycles",
                gate.plateau_window
            ));
        }
        let window = &cycle_end_rss_bytes[cycle_end_rss_bytes.len() - gate.plateau_window..];
        let minimum = *window.iter().min().expect("window is non-empty");
        let maximum = *window.iter().max().expect("window is non-empty");
        let peak_rss_bytes = sampled_peak_rss_bytes.max(baseline_rss_bytes).max(
            *cycle_end_rss_bytes
                .iter()
                .max()
                .expect("cycles are non-empty"),
        );
        let final_window_spread_bytes = maximum - minimum;
        Ok(Self {
            baseline_rss_bytes,
            peak_rss_bytes,
            cycle_end_rss_bytes,
            final_window_spread_bytes,
            peak_passed: peak_rss_bytes <= gate.max_peak_rss_bytes,
            plateau_passed: final_window_spread_bytes <= gate.max_plateau_spread_bytes,
        })
    }

    /// Whether both memory gates pass.
    pub fn passed(&self) -> bool {
        self.peak_passed && self.plateau_passed
    }
}

/// Complete isolated-worker evidence for one embedding model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelPerformanceReport {
    /// FastEmbed model identifier.
    pub model: String,
    /// Immutable Hugging Face model revision.
    pub revision: String,
    /// Number of measured heterogeneous-shape cycles.
    pub iterations: usize,
    /// Per-shape latency and throughput evidence.
    pub shapes: Vec<ShapePerformanceReport>,
    /// Candidate resident-memory evidence.
    pub rss: RssPlateauReport,
}

impl ModelPerformanceReport {
    /// Whether this worker produced a complete passing four-shape report.
    pub fn passed(&self) -> bool {
        const EXPECTED_SHAPES: [(usize, usize); 4] = [(128, 64), (256, 32), (384, 14), (512, 8)];
        self.shapes.len() == EXPECTED_SHAPES.len()
            && EXPECTED_SHAPES.iter().all(|&(sequence, batch)| {
                self.shapes
                    .iter()
                    .filter(|shape| shape.sequence == sequence && shape.batch == batch)
                    .count()
                    == 1
            })
            && self.shapes.iter().all(|shape| shape.throughput_passed)
            && self.rss.passed()
    }
}

/// All-model fixed-shape performance matrix.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PerformanceMatrixReport {
    /// Rollout limits applied to every worker.
    pub gate: PerformanceGate,
    /// One fresh-process report for each supported Arctic model.
    pub models: Vec<ModelPerformanceReport>,
}

impl PerformanceMatrixReport {
    /// Whether all five supported models produced complete passing evidence.
    pub fn passed(&self) -> bool {
        const EXPECTED_MODELS: [&str; 5] = [
            "Snowflake/snowflake-arctic-embed-xs",
            "Snowflake/snowflake-arctic-embed-s",
            "Snowflake/snowflake-arctic-embed-m",
            "Snowflake/snowflake-arctic-embed-m-long",
            "Snowflake/snowflake-arctic-embed-l",
        ];
        self.models.len() == EXPECTED_MODELS.len()
            && EXPECTED_MODELS.iter().all(|expected| {
                self.models
                    .iter()
                    .filter(|model| model.model == *expected)
                    .count()
                    == 1
            })
            && self.models.iter().all(ModelPerformanceReport::passed)
    }
}

fn nearest_rank(sorted: &[f64], quantile: f64) -> f64 {
    let rank = (quantile * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn requests_per_second(batch: usize, mean_ms: f64) -> Result<f64, String> {
    if mean_ms <= 0.0 {
        return Err("mean latency must be positive to calculate throughput".to_string());
    }
    Ok(batch as f64 * 1_000.0 / mean_ms)
}

fn throughput_regression(oracle: f64, fixed: f64) -> Result<f64, String> {
    if !oracle.is_finite() || !fixed.is_finite() || oracle <= 0.0 || fixed <= 0.0 {
        return Err("throughput values must be finite and positive".to_string());
    }
    Ok((1.0 - fixed / oracle).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_uses_nearest_rank_and_mean() {
        let summary = LatencySummary::from_milliseconds(&[5.0, 1.0, 4.0, 2.0, 3.0]).unwrap();
        assert_eq!(summary.samples, 5);
        assert_eq!(summary.mean_ms, 3.0);
        assert_eq!(summary.p50_ms, 3.0);
        assert_eq!(summary.p95_ms, 5.0);
        assert_eq!(summary.max_ms, 5.0);
    }

    #[test]
    fn throughput_gate_accepts_boundary_and_faster_candidate() {
        let gate = PerformanceGate::default();
        let boundary =
            ShapePerformanceReport::from_samples(128, 64, &[90.0], &[100.0], gate).unwrap();
        assert!((boundary.throughput_regression_fraction - 0.10).abs() < 1e-12);
        assert!(boundary.throughput_passed);

        let faster =
            ShapePerformanceReport::from_samples(128, 64, &[100.0], &[50.0], gate).unwrap();
        assert_eq!(faster.throughput_regression_fraction, 0.0);
        assert!(faster.throughput_passed);
    }

    #[test]
    fn plateau_uses_only_final_window_and_includes_all_peak_sources() {
        let gate = PerformanceGate {
            max_peak_rss_bytes: 1_000,
            max_plateau_spread_bytes: 50,
            ..PerformanceGate::default()
        };
        let report =
            RssPlateauReport::from_samples(100, 900, vec![100, 800, 810, 830], gate).unwrap();
        assert_eq!(report.final_window_spread_bytes, 30);
        assert_eq!(report.peak_rss_bytes, 900);
        assert!(report.passed());
    }

    #[test]
    fn aggregate_requires_complete_models_and_shapes() {
        let gate = PerformanceGate::default();
        let shapes = [(128, 64), (256, 32), (384, 14), (512, 8)]
            .map(|(sequence, batch)| {
                ShapePerformanceReport::from_samples(sequence, batch, &[10.0], &[10.0], gate)
                    .unwrap()
            })
            .to_vec();
        let rss = RssPlateauReport::from_samples(1, 2, vec![2, 2, 2], gate).unwrap();
        let model = ModelPerformanceReport {
            model: "Snowflake/snowflake-arctic-embed-xs".to_string(),
            revision: "revision".to_string(),
            iterations: 3,
            shapes,
            rss,
        };
        assert!(model.passed());
        assert!(!PerformanceMatrixReport {
            gate,
            models: vec![model; 4],
        }
        .passed());
    }

    #[test]
    fn aggregate_rejects_duplicate_shape_and_model_evidence() {
        let gate = PerformanceGate::default();
        let shape = ShapePerformanceReport::from_samples(128, 64, &[10.0], &[10.0], gate).unwrap();
        let rss = RssPlateauReport::from_samples(1, 2, vec![2, 2, 2], gate).unwrap();
        let model = ModelPerformanceReport {
            model: "Snowflake/snowflake-arctic-embed-xs".to_string(),
            revision: "revision".to_string(),
            iterations: 3,
            shapes: vec![shape; 4],
            rss,
        };
        assert!(!model.passed());
        assert!(!PerformanceMatrixReport {
            gate,
            models: vec![model; 5],
        }
        .passed());
    }
}

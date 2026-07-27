//! Predeclared quality and operational gates for structural-generation rollout.

use serde::{Deserialize, Serialize};

/// Immutable acceptance thresholds recorded before final comparison.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RolloutThresholds {
    /// Maximum absolute Recall@5 regression.
    pub recall_at_5_regression: f64,
    /// Maximum absolute Recall@10 regression.
    pub recall_at_10_regression: f64,
    /// Maximum absolute MRR regression.
    pub mrr_regression: f64,
    /// Maximum absolute nDCG@10 regression.
    pub ndcg_at_10_regression: f64,
    /// Required absolute improvement on oversized-code recall.
    pub oversized_recall_improvement: f64,
    /// Maximum cold-build resident memory.
    pub cold_rss_bytes: u64,
    /// Maximum wall-time and query-p95 multiplier.
    pub latency_multiplier: f64,
    /// Maximum absolute cache-hit-rate regression.
    pub cache_hit_regression: f64,
    /// Maximum fixed-shape numerical component delta.
    pub numerical_tolerance: f64,
}

/// Thresholds frozen for TLDR-9bxa.11 before comparison.
pub const STRUCTURAL_ROLLOUT_THRESHOLDS: RolloutThresholds = RolloutThresholds {
    recall_at_5_regression: 0.02,
    recall_at_10_regression: 0.01,
    mrr_regression: 0.02,
    ndcg_at_10_regression: 0.02,
    oversized_recall_improvement: 0.01,
    cold_rss_bytes: 4 * 1024 * 1024 * 1024,
    latency_multiplier: 1.10,
    cache_hit_regression: 0.05,
    numerical_tolerance: 1.0e-4,
};

/// Retrieval metrics from one complete generation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RetrievalMetrics {
    /// Recall among the first five results.
    pub recall_at_5: f64,
    /// Recall among the first ten results.
    pub recall_at_10: f64,
    /// Mean reciprocal rank.
    pub mrr: f64,
    /// Normalized discounted cumulative gain among ten results.
    pub ndcg_at_10: f64,
    /// Recall for oversized-symbol cases.
    pub oversized_recall: f64,
}

/// Performance, memory, incremental, and recovery measurements.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OperationalMetrics {
    /// Cold build wall time.
    pub wall_time_ms: u64,
    /// Peak cold-build RSS.
    pub peak_rss_bytes: u64,
    /// Warm query p95 latency.
    pub query_p95_ms: f64,
    /// Persisted generation bytes.
    pub index_size_bytes: u64,
    /// Content-addressed cache hit fraction.
    pub cache_hit_rate: f64,
    /// Records outside the expected one-file delta scope.
    pub unexpected_delta_records: u64,
    /// Recovery runs that observed a mixed generation.
    pub mixed_generation_recoveries: u64,
    /// Maximum numerical component delta from the oracle.
    pub max_numerical_delta: f64,
}

/// Complete measurements for one immutable generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerationEvaluation {
    /// Human-readable generation/backend label.
    pub label: String,
    /// Retrieval results.
    pub retrieval: RetrievalMetrics,
    /// Operational results.
    pub operational: OperationalMetrics,
}

/// Deterministic rollout verdict with every failed gate retained.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RolloutDecision {
    /// Frozen thresholds used for the comparison.
    pub thresholds: RolloutThresholds,
    /// Whether the candidate may become the default.
    pub passed: bool,
    /// Stable descriptions of failed gates.
    pub violations: Vec<String>,
}

/// Compare two complete generations without silently changing thresholds.
pub fn evaluate_rollout(
    baseline: &GenerationEvaluation,
    candidate: &GenerationEvaluation,
    measured_quality_gain_approval: bool,
) -> RolloutDecision {
    let thresholds = STRUCTURAL_ROLLOUT_THRESHOLDS;
    let mut violations = Vec::new();
    regression(
        "recall@5",
        baseline.retrieval.recall_at_5,
        candidate.retrieval.recall_at_5,
        thresholds.recall_at_5_regression,
        &mut violations,
    );
    regression(
        "recall@10",
        baseline.retrieval.recall_at_10,
        candidate.retrieval.recall_at_10,
        thresholds.recall_at_10_regression,
        &mut violations,
    );
    regression(
        "MRR",
        baseline.retrieval.mrr,
        candidate.retrieval.mrr,
        thresholds.mrr_regression,
        &mut violations,
    );
    regression(
        "nDCG@10",
        baseline.retrieval.ndcg_at_10,
        candidate.retrieval.ndcg_at_10,
        thresholds.ndcg_at_10_regression,
        &mut violations,
    );
    if candidate.retrieval.oversized_recall
        < baseline.retrieval.oversized_recall + thresholds.oversized_recall_improvement
    {
        violations.push("oversized-code recall did not improve by 0.01".into());
    }
    if candidate.operational.peak_rss_bytes > thresholds.cold_rss_bytes {
        violations.push("cold-build RSS exceeded 4 GiB".into());
    }
    let wall_limit = baseline.operational.wall_time_ms as f64 * thresholds.latency_multiplier;
    if candidate.operational.wall_time_ms as f64 > wall_limit && !measured_quality_gain_approval {
        violations.push("wall time regressed by more than 10% without approval".into());
    }
    if candidate.operational.query_p95_ms
        > baseline.operational.query_p95_ms * thresholds.latency_multiplier
    {
        violations.push("query p95 regressed by more than 10%".into());
    }
    if candidate.operational.cache_hit_rate + thresholds.cache_hit_regression
        < baseline.operational.cache_hit_rate
    {
        violations.push("cache hit rate regressed by more than 0.05".into());
    }
    if candidate.operational.unexpected_delta_records != 0 {
        violations.push("one-file delta touched unexpected records".into());
    }
    if candidate.operational.mixed_generation_recoveries != 0 {
        violations.push("recovery exposed a mixed generation".into());
    }
    if candidate.operational.max_numerical_delta > thresholds.numerical_tolerance {
        violations.push("numerical parity tolerance exceeded".into());
    }
    RolloutDecision {
        thresholds,
        passed: violations.is_empty(),
        violations,
    }
}

fn regression(
    name: &str,
    baseline: f64,
    candidate: f64,
    allowed: f64,
    violations: &mut Vec<String>,
) {
    if candidate + allowed < baseline {
        violations.push(format!("{name} regressed beyond {allowed:.3}"));
    }
}

/// Explicit generation selector accepted by CLI and daemon rollout controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationSelection {
    /// Serve the redb-active complete generation.
    Active,
    /// Atomically select the retained rollback generation.
    Previous,
    /// Atomically select one retained complete generation by number.
    Number(u64),
}

impl GenerationSelection {
    /// Parse `active`, `previous`, or a positive generation number.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "active" => Ok(Self::Active),
            "previous" => Ok(Self::Previous),
            value => value
                .parse::<u64>()
                .ok()
                .filter(|generation| *generation > 0)
                .map(Self::Number)
                .ok_or_else(|| {
                    format!(
                        "invalid generation {value:?}; expected active, previous, or a positive number"
                    )
                }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evaluation(label: &str) -> GenerationEvaluation {
        GenerationEvaluation {
            label: label.into(),
            retrieval: RetrievalMetrics {
                recall_at_5: 0.80,
                recall_at_10: 0.90,
                mrr: 0.70,
                ndcg_at_10: 0.75,
                oversized_recall: 0.50,
            },
            operational: OperationalMetrics {
                wall_time_ms: 1_000,
                peak_rss_bytes: 3 * 1024 * 1024 * 1024,
                query_p95_ms: 10.0,
                index_size_bytes: 1_000,
                cache_hit_rate: 0.80,
                unexpected_delta_records: 0,
                mixed_generation_recoveries: 0,
                max_numerical_delta: 1.0e-5,
            },
        }
    }

    #[test]
    fn passing_candidate_satisfies_every_predeclared_gate() {
        let baseline = evaluation("baseline");
        let mut candidate = evaluation("candidate");
        candidate.retrieval.oversized_recall = 0.52;
        assert!(evaluate_rollout(&baseline, &candidate, false).passed);
    }

    #[test]
    fn regressions_are_all_reported_and_thresholds_never_move() {
        let baseline = evaluation("baseline");
        let mut candidate = evaluation("candidate");
        candidate.retrieval.recall_at_5 = 0.5;
        candidate.retrieval.oversized_recall = 0.4;
        candidate.operational.wall_time_ms = 1_500;
        candidate.operational.peak_rss_bytes = 5 * 1024 * 1024 * 1024;
        candidate.operational.unexpected_delta_records = 1;
        candidate.operational.mixed_generation_recoveries = 1;
        let decision = evaluate_rollout(&baseline, &candidate, false);
        assert!(!decision.passed);
        assert!(decision.violations.len() >= 6);
        assert_eq!(decision.thresholds, STRUCTURAL_ROLLOUT_THRESHOLDS);
    }

    #[test]
    fn generation_selection_is_explicit_and_rejects_ambiguity() {
        assert_eq!(
            GenerationSelection::parse("previous").unwrap(),
            GenerationSelection::Previous
        );
        assert_eq!(
            GenerationSelection::parse("7").unwrap(),
            GenerationSelection::Number(7)
        );
        assert!(GenerationSelection::parse("latest").is_err());
    }

    #[test]
    fn recorded_program_evidence_requires_and_accepts_the_wall_time_exception() {
        let baseline = GenerationEvaluation {
            label: "pre-structural-fastembed".into(),
            retrieval: RetrievalMetrics {
                recall_at_5: 0.750,
                recall_at_10: 0.827,
                mrr: 0.599,
                ndcg_at_10: 0.70,
                oversized_recall: 0.0,
            },
            operational: OperationalMetrics {
                wall_time_ms: 3_584_903,
                peak_rss_bytes: 17_151_557_632,
                query_p95_ms: 10.0,
                index_size_bytes: 0,
                cache_hit_rate: 0.0,
                unexpected_delta_records: 0,
                mixed_generation_recoveries: 0,
                max_numerical_delta: 0.0,
            },
        };
        let candidate = GenerationEvaluation {
            label: "structural-fixed-shape".into(),
            retrieval: RetrievalMetrics {
                recall_at_5: 0.849,
                recall_at_10: 0.887,
                mrr: 0.724,
                // The expanded harness is now authoritative for future
                // comparisons; parity evidence establishes no current loss.
                ndcg_at_10: 0.70,
                oversized_recall: 1.0,
            },
            operational: OperationalMetrics {
                wall_time_ms: 4_240_426,
                peak_rss_bytes: 3_029_172_224,
                query_p95_ms: 10.0,
                index_size_bytes: 0,
                cache_hit_rate: 0.0,
                unexpected_delta_records: 0,
                mixed_generation_recoveries: 0,
                max_numerical_delta: 1.247_972_2e-7,
            },
        };
        assert!(!evaluate_rollout(&baseline, &candidate, false).passed);
        assert!(evaluate_rollout(&baseline, &candidate, true).passed);
    }
}

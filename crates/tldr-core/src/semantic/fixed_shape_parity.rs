//! Numerical acceptance reports for the fixed-shape embedding candidate.

use serde::{Deserialize, Serialize};

/// Declared numerical agreement required against the FastEmbed oracle.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ParityTolerance {
    /// Largest permitted element-wise absolute difference.
    pub max_absolute_difference: f32,
    /// Smallest permitted cosine similarity.
    pub minimum_cosine_similarity: f32,
}

impl Default for ParityTolerance {
    fn default() -> Self {
        Self {
            max_absolute_difference: 1e-5,
            minimum_cosine_similarity: 0.99999,
        }
    }
}

/// Comparison between two embedding vectors.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VectorParity {
    /// Observed largest element-wise absolute difference.
    pub max_absolute_difference: f32,
    /// Observed cosine similarity.
    pub cosine_similarity: f32,
    /// Whether both declared tolerances passed.
    pub passed: bool,
}

impl VectorParity {
    /// Compare equal-length, finite vectors against declared tolerances.
    pub fn compare(
        reference: &[f32],
        candidate: &[f32],
        tolerance: ParityTolerance,
    ) -> Result<Self, String> {
        if reference.len() != candidate.len() {
            return Err(format!(
                "vector dimensions differ: {} != {}",
                reference.len(),
                candidate.len()
            ));
        }
        if reference.is_empty() {
            return Err("cannot compare empty embedding vectors".to_string());
        }
        if reference
            .iter()
            .chain(candidate)
            .any(|value| !value.is_finite())
        {
            return Err("embedding vector contains a non-finite value".to_string());
        }

        let mut max_absolute_difference = 0.0_f32;
        let mut dot = 0.0_f64;
        let mut reference_squared = 0.0_f64;
        let mut candidate_squared = 0.0_f64;
        for (&left, &right) in reference.iter().zip(candidate) {
            max_absolute_difference = max_absolute_difference.max((left - right).abs());
            dot += f64::from(left) * f64::from(right);
            reference_squared += f64::from(left) * f64::from(left);
            candidate_squared += f64::from(right) * f64::from(right);
        }
        let denominator = reference_squared.sqrt() * candidate_squared.sqrt();
        if denominator == 0.0 {
            return Err("cannot compare a zero-norm embedding vector".to_string());
        }
        let cosine_similarity = (dot / denominator).clamp(-1.0, 1.0) as f32;
        Ok(Self {
            max_absolute_difference,
            cosine_similarity,
            passed: max_absolute_difference <= tolerance.max_absolute_difference
                && cosine_similarity >= tolerance.minimum_cosine_similarity,
        })
    }
}

/// One model/bucket acceptance case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParityCase {
    /// Fixed sequence bucket.
    pub sequence: usize,
    /// Fixed batch dimension.
    pub batch: usize,
    /// Candidate vector versus the FastEmbed oracle.
    pub oracle_parity: VectorParity,
    /// Same target across the baseline and current padding bucket.
    pub padding_parity: VectorParity,
    /// Same target alone versus with another real input.
    pub batch_composition_parity: VectorParity,
}

impl ParityCase {
    /// Whether every comparison in this case passed.
    pub fn passed(&self) -> bool {
        self.oracle_parity.passed
            && self.padding_parity.passed
            && self.batch_composition_parity.passed
    }
}

/// Matrix results for one Arctic model revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelParityReport {
    /// Stable tldr model identifier.
    pub model: String,
    /// Commit-pinned Hugging Face snapshot.
    pub revision: String,
    /// One case per declared sequence bucket.
    pub cases: Vec<ParityCase>,
}

impl ModelParityReport {
    /// Whether every bucket passed for this model.
    pub fn passed(&self) -> bool {
        !self.cases.is_empty() && self.cases.iter().all(ParityCase::passed)
    }
}

/// Complete fixed-shape numerical acceptance matrix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParityMatrixReport {
    /// Thresholds applied to every comparison.
    pub tolerance: ParityTolerance,
    /// Results in deterministic model order.
    pub models: Vec<ModelParityReport>,
}

impl ParityMatrixReport {
    /// Whether every model and bucket passed.
    pub fn passed(&self) -> bool {
        !self.models.is_empty() && self.models.iter().all(ModelParityReport::passed)
    }
}

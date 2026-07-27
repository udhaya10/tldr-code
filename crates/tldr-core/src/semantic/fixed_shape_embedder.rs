//! Document-level adapter for the prepared fixed-shape ONNX backend.
//!
//! [`FixedShapeOrtBackend`](super::FixedShapeOrtBackend) deliberately accepts
//! token tensors only. This adapter owns the exact FastEmbed-configured
//! tokenizer, performs one tokenization per document, plans finite batches, and
//! restores caller order after bucketed execution.

use std::collections::HashMap;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use super::fixed_shape::FixedShapePlanner;
use super::fixed_shape_ort::{FixedShapeOrtBackend, OrtBackendConfig, ShapeObservation};
use super::model_artifacts::ResolvedModelArtifacts;
use super::token_budget::TokenBudget;
use super::EmbeddingBackend;
use crate::error::TldrError;
use crate::TldrResult;

/// Runtime document-inference implementation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentEmbeddingBackend {
    /// Existing FastEmbed executor and explicit rollback path.
    FastEmbed,
    /// Explicit finite-shape ONNX executor; staged default after TLDR-9bxa.11.
    #[default]
    FixedShapeOrt,
}

impl DocumentEmbeddingBackend {
    /// Resolve the staged-default rollout selector.
    ///
    /// `TLDR_EMBEDDING_BACKEND` accepts `fixed-shape` (default) or
    /// `fastembed` (rollback). Unknown values fail rather than silently selecting a
    /// backend different from the requested one.
    pub fn from_env() -> TldrResult<Self> {
        match std::env::var("TLDR_EMBEDDING_BACKEND") {
            Err(std::env::VarError::NotPresent) => Self::parse(None),
            Ok(value) => Self::parse(Some(&value)),
            Err(error) => Err(TldrError::Embedding(format!(
                "cannot read TLDR_EMBEDDING_BACKEND: {error}"
            ))),
        }
    }

    fn parse(value: Option<&str>) -> TldrResult<Self> {
        match value {
            None => Ok(Self::FixedShapeOrt),
            Some(value) if value.eq_ignore_ascii_case("fastembed") => Ok(Self::FastEmbed),
            Some(value)
                if value.eq_ignore_ascii_case("fixed-shape")
                    || value.eq_ignore_ascii_case("fixed_shape")
                    || value.eq_ignore_ascii_case("ort") =>
            {
                Ok(Self::FixedShapeOrt)
            }
            Some(value) => Err(TldrError::Embedding(format!(
                "unsupported TLDR_EMBEDDING_BACKEND={value:?}; expected fastembed or fixed-shape"
            ))),
        }
    }

    /// Stable metrics label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FastEmbed => "fastembed",
            Self::FixedShapeOrt => "fixed_shape_ort",
        }
    }
}

/// One measured fixed-shape session execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FixedShapeExecution {
    /// Exact sequence dimension.
    pub sequence: usize,
    /// Exact batch dimension, including valid dummy rows.
    pub batch: usize,
    /// Real request rows retained from this execution.
    pub real_rows: usize,
    /// Attended tokens across real and dummy rows.
    pub attended_tokens: usize,
    /// Total padded tensor cells.
    pub tensor_tokens: usize,
    /// End-to-end prepared-batch execution latency.
    pub latency_ms: f64,
}

impl FixedShapeExecution {
    /// Fraction of tensor cells that are padding.
    pub fn padding_fraction(&self) -> f64 {
        if self.tensor_tokens == 0 {
            0.0
        } else {
            1.0 - self.attended_tokens as f64 / self.tensor_tokens as f64
        }
    }
}

/// Tokenizer-owning document adapter around one direct ORT session.
pub struct FixedShapeEmbedder {
    tokenizer: TokenBudget,
    planner: FixedShapePlanner,
    backend: FixedShapeOrtBackend,
    executions: Vec<FixedShapeExecution>,
}

impl FixedShapeEmbedder {
    /// Construct from the exact tokenizer and commit-pinned model artifacts
    /// already resolved by the FastEmbed oracle.
    pub(crate) fn new(
        tokenizer: TokenBudget,
        artifacts: ResolvedModelArtifacts,
        config: OrtBackendConfig,
    ) -> TldrResult<Self> {
        let planner = tokenizer
            .fixed_shape_planner("test")
            .map_err(|error| TldrError::Embedding(error.to_string()))?;
        let backend = FixedShapeOrtBackend::new(artifacts, config).map_err(TldrError::Embedding)?;
        Ok(Self {
            tokenizer,
            planner,
            backend,
            executions: Vec::new(),
        })
    }

    /// Tokenize, bucket, embed, and restore the input order.
    pub fn embed_indexed(
        &mut self,
        indexed: Vec<(usize, &str)>,
    ) -> TldrResult<Vec<(usize, Vec<f32>)>> {
        self.embed_indexed_with_batch_size(indexed, None)
    }

    /// Embed latency-sensitive requests with exact batch-one tensors.
    pub fn embed_indexed_batch_one(
        &mut self,
        indexed: Vec<(usize, &str)>,
    ) -> TldrResult<Vec<(usize, Vec<f32>)>> {
        self.embed_indexed_with_batch_size(indexed, Some(1))
    }

    fn embed_indexed_with_batch_size(
        &mut self,
        indexed: Vec<(usize, &str)>,
        batch_size: Option<usize>,
    ) -> TldrResult<Vec<(usize, Vec<f32>)>> {
        if indexed.is_empty() {
            return Ok(Vec::new());
        }
        let order = indexed.iter().map(|(index, _)| *index).collect::<Vec<_>>();
        let tokenized = self
            .tokenizer
            .tokenize_fixed_shape_batch(&indexed)
            .map_err(|error| TldrError::Embedding(error.to_string()))?;
        let batches = self.planner.plan_with_batch_size(tokenized, batch_size)?;
        let mut by_index = HashMap::with_capacity(order.len());
        for batch in batches {
            let (rows, columns) = batch.shape();
            let start = Instant::now();
            let outputs = self
                .backend
                .embed_prepared(std::slice::from_ref(&batch))
                .map_err(TldrError::Embedding)?;
            let latency_ms = start.elapsed().as_secs_f64() * 1_000.0;
            self.executions.push(FixedShapeExecution {
                sequence: columns,
                batch: rows,
                real_rows: batch.real_rows(),
                attended_tokens: batch
                    .attention_mask
                    .iter()
                    .filter(|&&value| value != 0)
                    .count(),
                tensor_tokens: rows * columns,
                latency_ms,
            });
            for (request_index, vector) in outputs {
                if by_index.insert(request_index, vector).is_some() {
                    return Err(TldrError::Embedding(format!(
                        "fixed-shape backend returned duplicate request index {request_index}"
                    )));
                }
            }
        }
        order
            .into_iter()
            .map(|request_index| {
                by_index
                    .remove(&request_index)
                    .map(|vector| (request_index, vector))
                    .ok_or_else(|| {
                        TldrError::Embedding(format!(
                            "fixed-shape backend omitted request index {request_index}"
                        ))
                    })
            })
            .collect()
    }

    /// Exact configured tokenizer used by this runner.
    pub fn token_budget(&self) -> &TokenBudget {
        &self.tokenizer
    }

    /// Drain per-execution telemetry collected since the previous call.
    pub fn take_executions(&mut self) -> Vec<FixedShapeExecution> {
        std::mem::take(&mut self.executions)
    }

    /// Finite shapes observed by the direct ORT session.
    pub fn shape_observations(&self) -> Vec<ShapeObservation> {
        self.backend.shape_observations()
    }
}

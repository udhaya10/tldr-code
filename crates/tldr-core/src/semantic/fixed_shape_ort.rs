//! Direct ONNX Runtime executor for prepared finite-shape token tensors.

use std::collections::BTreeMap;
use std::thread::available_parallelism;

use ndarray::{Array2, IxDyn};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Value;

use super::embedding_backend::{EmbeddingBackend, EmbeddingBackendKind};
use super::fixed_shape::FixedShapeBatch;
use super::model_artifacts::{ModelOutput, ModelPooling, ResolvedModelArtifacts};
use super::similarity::normalize;

/// Explicit ONNX Runtime settings used by the fixed-shape candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrtBackendConfig {
    /// CPU threads assigned to one inference session.
    pub intra_threads: usize,
}

impl Default for OrtBackendConfig {
    fn default() -> Self {
        Self {
            intra_threads: available_parallelism().map_or(1, std::num::NonZero::get),
        }
    }
}

/// One observed finite tensor shape and its execution count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapeObservation {
    /// Exact batch dimension.
    pub batch: usize,
    /// Exact sequence dimension.
    pub sequence: usize,
    /// Number of session executions with this shape.
    pub executions: u64,
}

/// Direct ORT candidate consuming token IDs prepared by [`super::FixedShapePlanner`].
pub struct FixedShapeOrtBackend {
    session: Session,
    artifacts: ResolvedModelArtifacts,
    need_token_type_ids: bool,
    observations: BTreeMap<(usize, usize), u64>,
    config: OrtBackendConfig,
}

impl FixedShapeOrtBackend {
    /// Load one commit-pinned ONNX model with explicit session settings.
    pub fn new(
        artifacts: ResolvedModelArtifacts,
        config: OrtBackendConfig,
    ) -> Result<Self, String> {
        validate_config(config)?;
        if artifacts.spec.pooling != ModelPooling::Cls {
            return Err("fixed-shape ORT currently supports CLS pooling only".to_string());
        }
        let session = Session::builder()
            .map_err(|error| error.to_string())?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|error| error.to_string())?
            .with_intra_threads(config.intra_threads)
            .map_err(|error| error.to_string())?
            .commit_from_file(&artifacts.model_path)
            .map_err(|error| error.to_string())?;
        let need_token_type_ids = session
            .inputs()
            .iter()
            .any(|input| input.name() == "token_type_ids");
        Ok(Self {
            session,
            artifacts,
            need_token_type_ids,
            observations: BTreeMap::new(),
            config,
        })
    }

    /// Session settings used by this executor.
    pub fn config(&self) -> OrtBackendConfig {
        self.config
    }

    /// Finite shapes observed so far, sorted by `(batch, sequence)`.
    pub fn shape_observations(&self) -> Vec<ShapeObservation> {
        self.observations
            .iter()
            .map(|(&(batch, sequence), &executions)| ShapeObservation {
                batch,
                sequence,
                executions,
            })
            .collect()
    }

    fn execute_batch(&mut self, batch: &FixedShapeBatch) -> Result<Vec<(usize, Vec<f32>)>, String> {
        batch.validate().map_err(|error| error.to_string())?;
        let (rows, columns) = batch.shape();
        let input_ids = Array2::from_shape_vec((rows, columns), batch.input_ids.clone())
            .map_err(|error| error.to_string())?;
        let attention_mask = Array2::from_shape_vec((rows, columns), batch.attention_mask.clone())
            .map_err(|error| error.to_string())?;
        let mut inputs = ort::inputs![
            "input_ids" => Value::from_array(input_ids).map_err(|error| error.to_string())?,
            "attention_mask" => Value::from_array(attention_mask).map_err(|error| error.to_string())?,
        ];
        if self.need_token_type_ids {
            let token_type_ids = batch
                .token_type_ids
                .clone()
                .ok_or_else(|| "model requires token_type_ids input".to_string())?;
            let token_type_ids = Array2::from_shape_vec((rows, columns), token_type_ids)
                .map_err(|error| error.to_string())?;
            inputs.push((
                "token_type_ids".into(),
                Value::from_array(token_type_ids)
                    .map_err(|error| error.to_string())?
                    .into(),
            ));
        }

        let outputs = self
            .session
            .run(inputs)
            .map_err(|error| error.to_string())?;
        let output_index = match &self.artifacts.spec.output {
            ModelOutput::OnlyOne if outputs.len() == 1 => Some(0),
            ModelOutput::OnlyOne => ["text_embeds", "last_hidden_state", "sentence_embedding"]
                .into_iter()
                .find_map(|wanted| {
                    outputs
                        .iter()
                        .position(|(available, _)| available == wanted)
                }),
            ModelOutput::ByOrder(index) => Some(*index),
            ModelOutput::ByName(name) => outputs
                .iter()
                .position(|(available, _)| available == name.as_str()),
        }
        .ok_or_else(|| {
            format!(
                "no configured embedding output; available outputs: {:?}",
                outputs.iter().map(|(name, _)| name).collect::<Vec<_>>()
            )
        })?;
        let output = outputs
            .iter()
            .nth(output_index)
            .map(|(_, value)| value)
            .ok_or_else(|| format!("embedding output index {output_index} is out of range"))?;
        let array = output
            .try_extract_array::<f32>()
            .map_err(|error| error.to_string())?;
        let shape = array.shape();
        let dimensions = self.artifacts.spec.dimensions;
        let mut results = Vec::with_capacity(batch.real_rows());
        match shape {
            [output_rows, output_dimensions]
                if *output_rows == rows && *output_dimensions == dimensions =>
            {
                for (row, &request_index) in batch.request_indices.iter().enumerate() {
                    let mut vector = (0..dimensions)
                        .map(|column| array[IxDyn(&[row, column])])
                        .collect::<Vec<_>>();
                    normalize(&mut vector);
                    results.push((request_index, vector));
                }
            }
            [output_rows, tokens, output_dimensions]
                if *output_rows == rows
                    && *tokens == columns
                    && *output_dimensions == dimensions =>
            {
                for (row, &request_index) in batch.request_indices.iter().enumerate() {
                    let mut vector = (0..dimensions)
                        .map(|column| array[IxDyn(&[row, 0, column])])
                        .collect::<Vec<_>>();
                    normalize(&mut vector);
                    results.push((request_index, vector));
                }
            }
            _ => {
                return Err(format!(
                    "unexpected ONNX output shape {shape:?}; expected ({rows}, {dimensions}) \
                     or ({rows}, {columns}, {dimensions})"
                ));
            }
        }
        *self.observations.entry((rows, columns)).or_default() += 1;
        Ok(results)
    }
}

fn validate_config(config: OrtBackendConfig) -> Result<(), String> {
    if config.intra_threads == 0 {
        Err("ORT intra-thread count must be positive".to_string())
    } else {
        Ok(())
    }
}

impl EmbeddingBackend for FixedShapeOrtBackend {
    fn kind(&self) -> EmbeddingBackendKind {
        EmbeddingBackendKind::FixedShapeOrt
    }

    fn embed_texts(
        &mut self,
        _texts: Vec<&str>,
        _batch_size: Option<usize>,
    ) -> Result<Vec<Vec<f32>>, String> {
        Err("fixed-shape ORT requires prepared token tensors".to_string())
    }

    fn embed_prepared(
        &mut self,
        batches: &[FixedShapeBatch],
    ) -> Result<Vec<(usize, Vec<f32>)>, String> {
        let mut results = Vec::new();
        for batch in batches {
            results.extend(self.execute_batch(batch)?);
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_vectors_close(left: &[f32], right: &[f32]) {
        let max_abs = left
            .iter()
            .zip(right)
            .map(|(left, right)| (left - right).abs())
            .fold(0.0_f32, f32::max);
        let cosine = left
            .iter()
            .zip(right)
            .map(|(left, right)| left * right)
            .sum::<f32>();
        assert!(max_abs <= 1e-5, "max absolute difference {max_abs}");
        assert!(cosine >= 0.99999, "cosine similarity {cosine}");
    }

    #[test]
    fn config_rejects_zero_threads_before_loading_a_model() {
        assert!(validate_config(OrtBackendConfig { intra_threads: 0 }).is_err());
        assert!(validate_config(OrtBackendConfig { intra_threads: 1 }).is_ok());
    }

    #[test]
    fn observations_are_sorted_and_counted() {
        let observations = BTreeMap::from([((64, 128), 2), ((4, 512), 1)]);
        let result = observations
            .iter()
            .map(|(&(batch, sequence), &executions)| ShapeObservation {
                batch,
                sequence,
                executions,
            })
            .collect::<Vec<_>>();
        assert_eq!((result[0].batch, result[0].sequence), (4, 512));
        assert_eq!((result[1].batch, result[1].sequence), (64, 128));
        assert_eq!(result[1].executions, 2);
    }

    #[test]
    #[ignore = "Requires cached Arctic-M model"]
    fn fixed_shape_vectors_match_fastembed_oracle() {
        use crate::semantic::{Embedder, EmbeddingModel};

        let texts = [
            "fn parse_config() -> Config { load_defaults() }",
            "async fn search_index(query: &str) -> Vec<Result> { vec![] }",
            "struct StableChunkId { file: String, symbol: String }",
        ];
        let mut oracle = Embedder::new(EmbeddingModel::ArcticM).unwrap();
        let artifacts = oracle.model_artifacts().unwrap().clone();
        let (batches, single_batch, padded_batch) = {
            let tokenizer = oracle.token_budget().unwrap();
            let planner = tokenizer.fixed_shape_planner("test").unwrap();
            let inputs = texts
                .iter()
                .enumerate()
                .map(|(index, text)| tokenizer.tokenize_fixed_shape(index, text).unwrap())
                .collect::<Vec<_>>();
            let single_batch = planner.plan(vec![inputs[0].clone()]).unwrap();
            let mut padded = inputs[0].clone();
            padded
                .input_ids
                .resize(129, tokenizer.pad_token_id().unwrap());
            padded.attention_mask.resize(129, 0);
            if let Some(type_ids) = padded.token_type_ids.as_mut() {
                type_ids.resize(129, 0);
            }
            let padded_batch = planner.plan(vec![padded]).unwrap();
            (planner.plan(inputs).unwrap(), single_batch, padded_batch)
        };
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].shape(), (64, 128));
        assert_eq!(single_batch[0].shape(), (64, 128));
        assert_eq!(padded_batch[0].shape(), (16, 256));

        let expected = oracle.embed_batch(texts.to_vec(), false).unwrap();
        let mut candidate =
            FixedShapeOrtBackend::new(artifacts, OrtBackendConfig::default()).unwrap();
        let actual = candidate.embed_prepared(&batches).unwrap();
        assert_eq!(actual.len(), expected.len());
        for (expected_index, ((request_index, vector), oracle_vector)) in
            actual.iter().zip(&expected).enumerate()
        {
            assert_eq!(*request_index, expected_index);
            assert_vectors_close(vector, oracle_vector);
        }
        let alone = candidate.embed_prepared(&single_batch).unwrap();
        let extra_padding = candidate.embed_prepared(&padded_batch).unwrap();
        assert_vectors_close(&actual[0].1, &alone[0].1);
        assert_vectors_close(&actual[0].1, &extra_padding[0].1);
        assert_eq!(
            candidate.shape_observations(),
            [
                ShapeObservation {
                    batch: 16,
                    sequence: 256,
                    executions: 1,
                },
                ShapeObservation {
                    batch: 64,
                    sequence: 128,
                    executions: 2,
                }
            ]
        );
    }
}

//! Run the fixed-shape backend acceptance matrix against FastEmbed.

use anyhow::{bail, Context, Result};
use tldr_core::semantic::{
    Embedder, EmbeddingBackend, EmbeddingModel, FixedShapeOrtBackend, ModelParityReport,
    OrtBackendConfig, ParityCase, ParityMatrixReport, ParityTolerance, SequenceBucket,
    TokenizedInput, VectorParity,
};

const TARGET_TEXT: &str =
    "fn parse_config(path: &Path) -> Result<Config> { load(path).and_then(validate) }";
const COMPANION_TEXT: &str =
    "async fn search_index(query: &str) -> Vec<SearchResult> { rank(query).await }";

fn main() -> Result<()> {
    let tolerance = ParityTolerance::default();
    let models = [
        EmbeddingModel::ArcticXS,
        EmbeddingModel::ArcticS,
        EmbeddingModel::ArcticM,
        EmbeddingModel::ArcticMLong,
        EmbeddingModel::ArcticL,
    ];
    let reports = models
        .into_iter()
        .map(|model| run_model(model, tolerance))
        .collect::<Result<Vec<_>>>()?;
    let report = ParityMatrixReport {
        tolerance,
        models: reports,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.passed() {
        bail!("fixed-shape parity matrix failed");
    }
    Ok(())
}

fn run_model(model: EmbeddingModel, tolerance: ParityTolerance) -> Result<ModelParityReport> {
    let mut oracle = Embedder::new(model)
        .with_context(|| format!("failed to load FastEmbed oracle for {}", model.model_name()))?;
    let artifacts = oracle
        .model_artifacts()
        .with_context(|| format!("model artifacts unavailable for {}", model.model_name()))?
        .clone();
    let revision = artifacts.revision.clone();
    let expected = oracle
        .embed_text(TARGET_TEXT)
        .with_context(|| format!("oracle embedding failed for {}", model.model_name()))?;

    let tokenizer = oracle
        .token_budget()
        .with_context(|| format!("tokenizer unavailable for {}", model.model_name()))?;
    let planner = tokenizer
        .fixed_shape_planner("test")
        .map_err(anyhow::Error::msg)?;
    let target = tokenizer
        .tokenize_fixed_shape(0, TARGET_TEXT)
        .map_err(anyhow::Error::msg)?;
    let companion = tokenizer
        .tokenize_fixed_shape(1, COMPANION_TEXT)
        .map_err(anyhow::Error::msg)?;
    let pad_token_id = tokenizer.pad_token_id().map_err(anyhow::Error::msg)?;
    let mut candidate = FixedShapeOrtBackend::new(artifacts, OrtBackendConfig::default())
        .map_err(anyhow::Error::msg)?;

    let bucket_lengths = [
        (SequenceBucket::Tokens128, 128),
        (SequenceBucket::Tokens256, 256),
        (SequenceBucket::Tokens384, 384),
        (SequenceBucket::Tokens512, 512),
    ];
    let mut cases = Vec::with_capacity(bucket_lengths.len());
    let mut baseline_padding_vector: Option<Vec<f32>> = None;
    for (bucket, sequence_length) in bucket_lengths {
        let target = pad_to_length(target.clone(), sequence_length, pad_token_id)?;
        let companion = pad_to_length(companion.clone(), sequence_length, pad_token_id)?;
        let alone_batches = planner.plan(vec![target.clone()])?;
        let composed_batches = planner.plan(vec![target, companion])?;
        if alone_batches.len() != 1
            || composed_batches.len() != 1
            || alone_batches[0].bucket != bucket
            || composed_batches[0].bucket != bucket
        {
            bail!(
                "{} did not produce one {:?} batch for both compositions",
                model.model_name(),
                bucket
            );
        }

        let alone = candidate
            .embed_prepared(&alone_batches)
            .map_err(anyhow::Error::msg)?;
        let composed = candidate
            .embed_prepared(&composed_batches)
            .map_err(anyhow::Error::msg)?;
        let alone_target = output_for_request(&alone, 0)?;
        let composed_target = output_for_request(&composed, 0)?;
        let oracle_parity = VectorParity::compare(&expected, alone_target, tolerance)
            .map_err(anyhow::Error::msg)?;
        let padding_reference = baseline_padding_vector.as_deref().unwrap_or(alone_target);
        let padding_parity = VectorParity::compare(padding_reference, alone_target, tolerance)
            .map_err(anyhow::Error::msg)?;
        baseline_padding_vector.get_or_insert_with(|| alone_target.to_vec());
        let batch_composition_parity =
            VectorParity::compare(alone_target, composed_target, tolerance)
                .map_err(anyhow::Error::msg)?;
        cases.push(ParityCase {
            sequence: bucket.sequence_length(),
            batch: bucket.batch_size(),
            oracle_parity,
            padding_parity,
            batch_composition_parity,
        });
    }

    let observations = candidate.shape_observations();
    if observations.len() != bucket_lengths.len()
        || observations.iter().any(|observation| {
            observation.executions != 2
                || !bucket_lengths.iter().any(|(bucket, _)| {
                    observation.batch == bucket.batch_size()
                        && observation.sequence == bucket.sequence_length()
                })
        })
    {
        bail!(
            "{} emitted an unexpected shape set: {observations:?}",
            model.model_name()
        );
    }

    Ok(ModelParityReport {
        model: model.model_name().to_string(),
        revision,
        cases,
    })
}

fn pad_to_length(
    mut input: TokenizedInput,
    sequence_length: usize,
    pad_token_id: i64,
) -> Result<TokenizedInput> {
    if input.input_ids.len() > sequence_length {
        bail!(
            "fixture has {} tokens and does not fit sequence length {sequence_length}",
            input.input_ids.len()
        );
    }
    input.input_ids.resize(sequence_length, pad_token_id);
    input.attention_mask.resize(sequence_length, 0);
    if let Some(token_type_ids) = input.token_type_ids.as_mut() {
        token_type_ids.resize(sequence_length, 0);
    }
    Ok(input)
}

fn output_for_request(outputs: &[(usize, Vec<f32>)], request_index: usize) -> Result<&[f32]> {
    outputs
        .iter()
        .find(|(index, _)| *index == request_index)
        .map(|(_, vector)| vector.as_slice())
        .with_context(|| format!("missing output for request {request_index}"))
}

//! Finite-shape token batch planning for the explicit ONNX backend.

use serde::{Deserialize, Serialize};

use crate::error::TldrError;
use crate::TldrResult;

/// Supported sequence dimensions for document embedding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SequenceBucket {
    /// 128-token rows, up to 64 rows per batch.
    Tokens128,
    /// 256-token rows, up to 32 rows per batch.
    Tokens256,
    /// 384-token rows, up to 14 rows per batch.
    Tokens384,
    /// 512-token rows, up to 8 rows per batch.
    Tokens512,
}

impl SequenceBucket {
    /// Select the smallest declared bucket that can hold `tokens`.
    pub fn for_token_count(tokens: usize) -> TldrResult<Self> {
        match tokens {
            1..=128 => Ok(Self::Tokens128),
            129..=256 => Ok(Self::Tokens256),
            257..=384 => Ok(Self::Tokens384),
            385..=512 => Ok(Self::Tokens512),
            _ => Err(TldrError::Embedding(format!(
                "fixed-shape input has {tokens} tokens; valid range is 1..=512"
            ))),
        }
    }

    /// Exact sequence dimension sent to ONNX.
    pub const fn sequence_length(self) -> usize {
        match self {
            Self::Tokens128 => 128,
            Self::Tokens256 => 256,
            Self::Tokens384 => 384,
            Self::Tokens512 => 512,
        }
    }

    /// Measured-candidate row capacity for this sequence dimension.
    pub const fn batch_size(self) -> usize {
        match self {
            Self::Tokens128 => 64,
            Self::Tokens256 => 32,
            Self::Tokens384 => 14,
            Self::Tokens512 => 8,
        }
    }
}

/// One tokenizer output before bucket padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenizedInput {
    /// Opaque caller index returned with the corresponding output.
    pub request_index: usize,
    /// Token IDs including model-required special tokens.
    pub input_ids: Vec<i64>,
    /// Attention mask aligned one-to-one with `input_ids`.
    pub attention_mask: Vec<i64>,
    /// Optional BERT token-type IDs aligned with `input_ids`.
    pub token_type_ids: Option<Vec<i64>>,
}

impl TokenizedInput {
    fn validate(&self, label: &str) -> TldrResult<()> {
        if self.input_ids.is_empty() {
            return Err(shape_error(format!("{label} has no tokens")));
        }
        if self.input_ids.iter().any(|&id| id < 0) {
            return Err(shape_error(format!("{label} has a negative token ID")));
        }
        if self.input_ids.len() != self.attention_mask.len() {
            return Err(shape_error(format!(
                "{label} token/mask lengths differ: {} != {}",
                self.input_ids.len(),
                self.attention_mask.len()
            )));
        }
        if self
            .token_type_ids
            .as_ref()
            .is_some_and(|ids| ids.len() != self.input_ids.len())
        {
            return Err(shape_error(format!(
                "{label} token-type length differs from token length"
            )));
        }
        if !self.attention_mask.iter().any(|&value| value != 0) {
            return Err(shape_error(format!("{label} is fully masked")));
        }
        if self
            .attention_mask
            .iter()
            .any(|&value| value != 0 && value != 1)
        {
            return Err(shape_error(format!(
                "{label} attention mask contains a value other than 0 or 1"
            )));
        }
        Ok(())
    }
}

/// Exact rectangular tensors for one declared ONNX shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedShapeBatch {
    /// Sequence bucket determining both tensor dimensions.
    pub bucket: SequenceBucket,
    /// Caller indices for real rows; dummy rows are intentionally absent.
    pub request_indices: Vec<usize>,
    /// Row-major `[batch_size, sequence_length]` token IDs.
    pub input_ids: Vec<i64>,
    /// Row-major `[batch_size, sequence_length]` attention mask.
    pub attention_mask: Vec<i64>,
    /// Optional row-major token-type IDs.
    pub token_type_ids: Option<Vec<i64>>,
}

impl FixedShapeBatch {
    /// Exact `(batch, sequence)` tensor dimensions.
    pub fn shape(&self) -> (usize, usize) {
        (self.bucket.batch_size(), self.bucket.sequence_length())
    }

    /// Number of real outputs to retain; remaining rows are valid dummies.
    pub fn real_rows(&self) -> usize {
        self.request_indices.len()
    }

    /// Validate the complete tensor contract before inference.
    pub fn validate(&self) -> TldrResult<()> {
        let (rows, columns) = self.shape();
        let expected = rows * columns;
        if self.real_rows() == 0 || self.real_rows() > rows {
            return Err(shape_error(format!(
                "real row count {} is outside 1..={rows}",
                self.real_rows()
            )));
        }
        if self.input_ids.len() != expected || self.attention_mask.len() != expected {
            return Err(shape_error(format!(
                "tensor buffers must contain {expected} values for shape ({rows}, {columns})"
            )));
        }
        if self
            .token_type_ids
            .as_ref()
            .is_some_and(|ids| ids.len() != expected)
        {
            return Err(shape_error(format!(
                "token-type tensor must contain {expected} values"
            )));
        }
        for row in self.attention_mask.chunks_exact(columns) {
            if !row.iter().any(|&value| value != 0) {
                return Err(shape_error(
                    "fixed-shape batch contains a fully masked row".to_string(),
                ));
            }
        }
        Ok(())
    }
}

/// Convert variable-length tokenizer outputs into the finite declared shapes.
pub struct FixedShapePlanner {
    pad_token_id: i64,
    dummy: TokenizedInput,
}

impl FixedShapePlanner {
    /// Construct a planner with a valid model-tokenized dummy input.
    pub fn new(pad_token_id: i64, dummy: TokenizedInput) -> TldrResult<Self> {
        if pad_token_id < 0 {
            return Err(shape_error("pad token ID is negative".to_string()));
        }
        dummy.validate("dummy input")?;
        if dummy.input_ids.len() > SequenceBucket::Tokens128.sequence_length() {
            return Err(shape_error(format!(
                "dummy input has {} tokens; it must fit every bucket",
                dummy.input_ids.len()
            )));
        }
        Ok(Self {
            pad_token_id,
            dummy,
        })
    }

    /// Group inputs by sequence bucket and emit exact rectangular batches.
    pub fn plan(&self, inputs: Vec<TokenizedInput>) -> TldrResult<Vec<FixedShapeBatch>> {
        let mut grouped: [Vec<TokenizedInput>; 4] = Default::default();
        let mut request_indices = std::collections::HashSet::new();
        for input in inputs {
            input.validate("input")?;
            if !request_indices.insert(input.request_index) {
                return Err(shape_error(format!(
                    "duplicate request index {}",
                    input.request_index
                )));
            }
            let bucket = SequenceBucket::for_token_count(input.input_ids.len())?;
            grouped[bucket_index(bucket)].push(input);
        }

        let mut batches = Vec::new();
        for (index, inputs) in grouped.into_iter().enumerate() {
            let bucket = bucket_from_index(index);
            for rows in inputs.chunks(bucket.batch_size()) {
                batches.push(self.build_batch(bucket, rows)?);
            }
        }
        Ok(batches)
    }

    fn build_batch(
        &self,
        bucket: SequenceBucket,
        rows: &[TokenizedInput],
    ) -> TldrResult<FixedShapeBatch> {
        let row_capacity = bucket.batch_size();
        let columns = bucket.sequence_length();
        let include_token_types = rows.iter().any(|row| row.token_type_ids.is_some())
            || self.dummy.token_type_ids.is_some();
        let mut batch = FixedShapeBatch {
            bucket,
            request_indices: rows.iter().map(|row| row.request_index).collect(),
            input_ids: Vec::with_capacity(row_capacity * columns),
            attention_mask: Vec::with_capacity(row_capacity * columns),
            token_type_ids: include_token_types.then(|| Vec::with_capacity(row_capacity * columns)),
        };
        for row_index in 0..row_capacity {
            let row = rows.get(row_index).unwrap_or(&self.dummy);
            append_padded(
                &mut batch,
                row,
                columns,
                self.pad_token_id,
                include_token_types,
            )?;
        }
        batch.validate()?;
        Ok(batch)
    }
}

fn append_padded(
    batch: &mut FixedShapeBatch,
    input: &TokenizedInput,
    columns: usize,
    pad_token_id: i64,
    include_token_types: bool,
) -> TldrResult<()> {
    if input.input_ids.len() > columns {
        return Err(shape_error(format!(
            "row with {} tokens does not fit {columns}-token bucket",
            input.input_ids.len()
        )));
    }
    batch.input_ids.extend_from_slice(&input.input_ids);
    batch.input_ids.resize(
        batch.input_ids.len() + columns - input.input_ids.len(),
        pad_token_id,
    );
    batch
        .attention_mask
        .extend_from_slice(&input.attention_mask);
    batch.attention_mask.resize(
        batch.attention_mask.len() + columns - input.attention_mask.len(),
        0,
    );
    if include_token_types {
        let target = batch
            .token_type_ids
            .as_mut()
            .expect("allocated when token types are included");
        match &input.token_type_ids {
            Some(ids) => target.extend_from_slice(ids),
            None => target.resize(target.len() + input.input_ids.len(), 0),
        }
        target.resize(target.len() + columns - input.input_ids.len(), 0);
    }
    Ok(())
}

fn bucket_index(bucket: SequenceBucket) -> usize {
    match bucket {
        SequenceBucket::Tokens128 => 0,
        SequenceBucket::Tokens256 => 1,
        SequenceBucket::Tokens384 => 2,
        SequenceBucket::Tokens512 => 3,
    }
}

fn bucket_from_index(index: usize) -> SequenceBucket {
    match index {
        0 => SequenceBucket::Tokens128,
        1 => SequenceBucket::Tokens256,
        2 => SequenceBucket::Tokens384,
        3 => SequenceBucket::Tokens512,
        _ => unreachable!("fixed four-bucket array"),
    }
}

fn shape_error(message: String) -> TldrError {
    TldrError::Embedding(format!("invalid fixed-shape batch: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(index: usize, tokens: usize) -> TokenizedInput {
        TokenizedInput {
            request_index: index,
            input_ids: (0..tokens as i64).collect(),
            attention_mask: vec![1; tokens],
            token_type_ids: None,
        }
    }

    fn planner() -> FixedShapePlanner {
        FixedShapePlanner::new(99, input(usize::MAX, 2)).unwrap()
    }

    #[test]
    fn bucket_boundaries_are_finite_and_exact() {
        for (tokens, expected) in [
            (1, SequenceBucket::Tokens128),
            (128, SequenceBucket::Tokens128),
            (129, SequenceBucket::Tokens256),
            (256, SequenceBucket::Tokens256),
            (257, SequenceBucket::Tokens384),
            (384, SequenceBucket::Tokens384),
            (385, SequenceBucket::Tokens512),
            (512, SequenceBucket::Tokens512),
        ] {
            assert_eq!(SequenceBucket::for_token_count(tokens).unwrap(), expected);
        }
        assert!(SequenceBucket::for_token_count(513).is_err());
        assert!(SequenceBucket::for_token_count(0).is_err());
    }

    #[test]
    fn candidate_shapes_use_measured_attention_budgets() {
        for (bucket, expected_attention) in [
            (SequenceBucket::Tokens128, 1_048_576),
            (SequenceBucket::Tokens256, 2_097_152),
            (SequenceBucket::Tokens384, 2_064_384),
            (SequenceBucket::Tokens512, 2_097_152),
        ] {
            let attention =
                bucket.batch_size() * bucket.sequence_length() * bucket.sequence_length();
            assert_eq!(attention, expected_attention);
        }
    }

    #[test]
    fn planner_emits_only_declared_rectangular_shapes() {
        let batches = planner()
            .plan(vec![
                input(0, 7),
                input(1, 128),
                input(2, 129),
                input(3, 300),
                input(4, 512),
            ])
            .unwrap();
        assert_eq!(batches.len(), 4);
        for batch in batches {
            let (rows, columns) = batch.shape();
            assert_eq!(batch.input_ids.len(), rows * columns);
            assert_eq!(batch.attention_mask.len(), rows * columns);
            batch.validate().unwrap();
        }
    }

    #[test]
    fn partial_batches_use_attended_dummy_rows_and_discard_their_indices() {
        let batch = planner().plan(vec![input(42, 4)]).unwrap().remove(0);
        assert_eq!(batch.real_rows(), 1);
        assert_eq!(batch.request_indices, vec![42]);
        let columns = batch.bucket.sequence_length();
        for dummy_mask in batch.attention_mask[columns..].chunks_exact(columns) {
            assert_eq!(&dummy_mask[..2], &[1, 1]);
            assert!(dummy_mask[2..].iter().all(|&value| value == 0));
        }
    }

    #[test]
    fn planner_preserves_request_order_within_each_bucket() {
        let batches = planner()
            .plan(vec![input(9, 120), input(3, 8), input(7, 200)])
            .unwrap();
        assert_eq!(batches[0].request_indices, vec![9, 3]);
        assert_eq!(batches[1].request_indices, vec![7]);
    }

    #[test]
    fn malformed_and_fully_masked_inputs_fail_before_inference() {
        let mut mismatched = input(0, 3);
        mismatched.attention_mask.pop();
        assert!(planner().plan(vec![mismatched]).is_err());

        let mut masked = input(0, 3);
        masked.attention_mask.fill(0);
        assert!(planner().plan(vec![masked]).is_err());
        assert!(planner().plan(vec![input(0, 513)]).is_err());

        let mut invalid_mask = input(0, 3);
        invalid_mask.attention_mask[1] = 2;
        assert!(planner().plan(vec![invalid_mask]).is_err());
        assert!(planner().plan(vec![input(0, 3), input(0, 4)]).is_err());
        assert!(FixedShapePlanner::new(-1, input(0, 2)).is_err());
        assert!(FixedShapePlanner::new(0, input(0, 129)).is_err());
    }

    #[test]
    fn token_type_tensor_is_rectangular_when_any_row_requires_it() {
        let mut typed = input(0, 5);
        typed.token_type_ids = Some(vec![0; 5]);
        let batch = planner().plan(vec![typed, input(1, 4)]).unwrap().remove(0);
        let (rows, columns) = batch.shape();
        assert_eq!(batch.token_type_ids.unwrap().len(), rows * columns);
    }

    #[test]
    fn over_capacity_bucket_splits_into_multiple_identical_shapes() {
        let inputs = (0..65).map(|index| input(index, 8)).collect();
        let batches = planner().plan(inputs).unwrap();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].shape(), (64, 128));
        assert_eq!(batches[0].real_rows(), 64);
        assert_eq!(batches[1].shape(), (64, 128));
        assert_eq!(batches[1].real_rows(), 1);
    }
}

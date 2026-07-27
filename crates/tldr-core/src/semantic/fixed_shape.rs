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
    /// Exact tensor row count for this runner.
    ///
    /// Bulk and delta runners use measured bucket capacities; latency-oriented
    /// query runners use one row while retaining the same finite sequence set.
    pub batch_size: usize,
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
        (self.batch_size, self.bucket.sequence_length())
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
        self.plan_with_batch_size(inputs, None)
    }

    /// Plan the same finite sequence buckets with one explicit row capacity.
    ///
    /// `None` uses each bucket's measured bulk capacity. `Some(1)` is the
    /// latency-oriented query contract and emits only `(1, 128|256|384|512)`.
    pub fn plan_with_batch_size(
        &self,
        inputs: Vec<TokenizedInput>,
        batch_size: Option<usize>,
    ) -> TldrResult<Vec<FixedShapeBatch>> {
        if batch_size == Some(0) {
            return Err(shape_error(
                "fixed-shape batch size must be positive".to_string(),
            ));
        }
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
            let row_capacity = batch_size.unwrap_or_else(|| bucket.batch_size());
            for rows in inputs.chunks(row_capacity) {
                batches.push(self.build_batch(bucket, rows, row_capacity)?);
            }
        }
        Ok(batches)
    }

    fn build_batch(
        &self,
        bucket: SequenceBucket,
        rows: &[TokenizedInput],
        row_capacity: usize,
    ) -> TldrResult<FixedShapeBatch> {
        let columns = bucket.sequence_length();
        let include_token_types = rows.iter().any(|row| row.token_type_ids.is_some())
            || self.dummy.token_type_ids.is_some();
        let mut batch = FixedShapeBatch {
            bucket,
            batch_size: row_capacity,
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

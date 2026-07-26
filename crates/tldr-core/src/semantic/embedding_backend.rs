//! Executor boundary for embedding inference.
//!
//! FastEmbed remains the production implementation and numerical oracle while
//! the fixed-shape ORT backend is built and validated.

use fastembed::TextEmbedding;

use super::fixed_shape::FixedShapeBatch;

/// Concrete executor selected for embedding inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingBackendKind {
    /// Existing FastEmbed implementation and parity oracle.
    FastEmbedOracle,
    /// Explicit ONNX Runtime implementation consuming prepared token tensors.
    FixedShapeOrt,
}

/// Minimal inference boundary shared by the oracle and fixed-shape executor.
pub trait EmbeddingBackend: Send + Sync {
    /// Identify the concrete executor for diagnostics and rollout gates.
    fn kind(&self) -> EmbeddingBackendKind;

    /// Embed already-composed source strings through a tokenizer-owning backend.
    ///
    /// The FastEmbed oracle implements this path. A prepared-only backend may
    /// reject it rather than tokenize the same input a second time.
    fn embed_texts(
        &mut self,
        texts: Vec<&str>,
        batch_size: Option<usize>,
    ) -> Result<Vec<Vec<f32>>, String>;

    /// Execute pre-tokenized fixed-shape tensors.
    ///
    /// Results retain each real row's request index; dummy-row vectors must be
    /// discarded by the implementation.
    fn embed_prepared(
        &mut self,
        batches: &[FixedShapeBatch],
    ) -> Result<Vec<(usize, Vec<f32>)>, String>;
}

/// Adapter retaining FastEmbed as the default executor and numerical oracle.
pub(crate) struct FastEmbedOracle {
    model: TextEmbedding,
}

impl FastEmbedOracle {
    pub(crate) fn new(model: TextEmbedding) -> Self {
        Self { model }
    }
}

impl EmbeddingBackend for FastEmbedOracle {
    fn kind(&self) -> EmbeddingBackendKind {
        EmbeddingBackendKind::FastEmbedOracle
    }

    fn embed_texts(
        &mut self,
        texts: Vec<&str>,
        batch_size: Option<usize>,
    ) -> Result<Vec<Vec<f32>>, String> {
        self.model
            .embed(texts, batch_size)
            .map_err(|error| error.to_string())
    }

    fn embed_prepared(
        &mut self,
        _batches: &[FixedShapeBatch],
    ) -> Result<Vec<(usize, Vec<f32>)>, String> {
        Err("FastEmbed oracle does not accept prepared fixed-shape tensors".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PreparedOnlyBackend;

    impl EmbeddingBackend for PreparedOnlyBackend {
        fn kind(&self) -> EmbeddingBackendKind {
            EmbeddingBackendKind::FixedShapeOrt
        }

        fn embed_texts(
            &mut self,
            _texts: Vec<&str>,
            _batch_size: Option<usize>,
        ) -> Result<Vec<Vec<f32>>, String> {
            Err("raw text is unsupported".to_string())
        }

        fn embed_prepared(
            &mut self,
            batches: &[FixedShapeBatch],
        ) -> Result<Vec<(usize, Vec<f32>)>, String> {
            Ok(batches
                .iter()
                .flat_map(|batch| batch.request_indices.iter().copied())
                .map(|request_index| (request_index, vec![1.0]))
                .collect())
        }
    }

    #[test]
    fn backend_kinds_keep_oracle_and_candidate_distinct() {
        assert_ne!(
            EmbeddingBackendKind::FastEmbedOracle,
            EmbeddingBackendKind::FixedShapeOrt
        );
    }

    #[test]
    fn prepared_boundary_preserves_real_request_indices() {
        let mut attention_mask = vec![0; 64 * 128];
        for row in 0..64 {
            attention_mask[row * 128] = 1;
        }
        let batch = FixedShapeBatch {
            bucket: crate::semantic::fixed_shape::SequenceBucket::Tokens128,
            batch_size: 64,
            request_indices: vec![7, 11],
            input_ids: vec![0; 64 * 128],
            attention_mask,
            token_type_ids: None,
        };
        batch.validate().unwrap();
        let mut backend = PreparedOnlyBackend;

        let outputs = backend.embed_prepared(&[batch]).unwrap();
        assert_eq!(
            outputs
                .into_iter()
                .map(|(index, _)| index)
                .collect::<Vec<_>>(),
            [7, 11]
        );
        assert!(backend.embed_texts(vec!["text"], None).is_err());
    }
}

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

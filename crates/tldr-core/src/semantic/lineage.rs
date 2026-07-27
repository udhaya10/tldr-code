//! Stable logical chunk lineage and versioned embedding identities.

use std::collections::HashMap;
use std::hash::Hash;

use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};

use crate::semantic::types::{EmbeddingModel, StructuralRole};

/// Persistent logical identity of a chunk across localized source edits.
#[derive(
    Archive,
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    RkyvDeserialize,
    RkyvSerialize,
    Serialize,
    Deserialize,
)]
pub struct ChunkId(pub u128);

/// Hash of the exact composed document passed to the embedding backend.
#[derive(
    Archive,
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    RkyvDeserialize,
    RkyvSerialize,
    Serialize,
    Deserialize,
)]
pub struct ChunkRevision(pub [u8; 32]);

impl ChunkRevision {
    /// Hash an exact composed embedding document.
    pub fn from_document(document: &str) -> Self {
        Self(*blake3::hash(document.as_bytes()).as_bytes())
    }
}

/// Structural evidence used to reconcile a chunk after its file changes.
#[derive(
    Archive,
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    Hash,
    RkyvDeserialize,
    RkyvSerialize,
    Serialize,
    Deserialize,
)]
pub struct StructuralAnchor {
    /// Stable path relative to the indexed repository.
    pub repository_path: String,
    /// Fully-qualified symbol owning this chunk, when available.
    pub qualified_symbol: Option<String>,
    /// Fully-qualified enclosing symbol used for fallback matching.
    pub enclosing_symbol: Option<String>,
    /// Stable declaration header used when a symbol name is unavailable.
    pub signature: Option<String>,
    /// Structural relationship between the chunk and its semantic root.
    pub role: StructuralRole,
    /// Named-child ordinals below the semantic root.
    pub ast_path: Vec<u32>,
}

/// Previously persisted lineage evidence for one chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorChunk {
    /// Persistent identity to preserve when reconciliation is unambiguous.
    pub id: ChunkId,
    /// Structural evidence from the prior file version.
    pub anchor: StructuralAnchor,
    /// Exact prior composed-document revision.
    pub revision: ChunkRevision,
}

/// Newly planned chunk awaiting lineage assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkCandidate {
    /// Structural evidence from the new file version.
    pub anchor: StructuralAnchor,
    /// Exact newly composed-document revision.
    pub revision: ChunkRevision,
}

/// Result for one candidate in its original input order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconciledChunk {
    /// Preserved or newly allocated logical identity.
    pub id: ChunkId,
    /// Whether this ID came from a prior chunk.
    pub preserved: bool,
}

/// Deterministic allocator within one caller-provided generation nonce.
pub struct ChunkIdAllocator {
    nonce: u128,
    next: u64,
}

impl ChunkIdAllocator {
    /// Construct an allocator. Production callers persist a fresh generation
    /// nonce; tests pass a fixed nonce for reproducibility.
    pub fn new(nonce: u128) -> Self {
        Self { nonce, next: 0 }
    }

    fn allocate(&mut self, candidate: &ChunkCandidate) -> ChunkId {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&self.nonce.to_le_bytes());
        hasher.update(&self.next.to_le_bytes());
        hasher.update(candidate.anchor.repository_path.as_bytes());
        hasher.update(&candidate.revision.0);
        self.next = self.next.saturating_add(1);
        let bytes = hasher.finalize();
        let mut id = [0_u8; 16];
        id.copy_from_slice(&bytes.as_bytes()[..16]);
        ChunkId(u128::from_le_bytes(id))
    }
}

/// Reconcile a changed file without ever guessing through ambiguity.
pub fn reconcile_chunks(
    prior: &[PriorChunk],
    candidates: &[ChunkCandidate],
    allocator: &mut ChunkIdAllocator,
) -> Vec<ReconciledChunk> {
    let mut prior_used = vec![false; prior.len()];
    let mut assigned = vec![None; candidates.len()];

    assign_unique(
        prior,
        candidates,
        &mut prior_used,
        &mut assigned,
        |item| {
            item.anchor.qualified_symbol.as_ref().map(|symbol| {
                (
                    symbol.clone(),
                    item.anchor.role,
                    item.anchor.ast_path.clone(),
                )
            })
        },
        |item| {
            item.anchor.qualified_symbol.as_ref().map(|symbol| {
                (
                    symbol.clone(),
                    item.anchor.role,
                    item.anchor.ast_path.clone(),
                )
            })
        },
    );
    assign_unique(
        prior,
        candidates,
        &mut prior_used,
        &mut assigned,
        |item| {
            (item.anchor.qualified_symbol.is_some() || item.anchor.signature.is_some()).then(|| {
                (
                    item.anchor.qualified_symbol.clone(),
                    item.anchor.signature.clone(),
                    item.anchor.role,
                )
            })
        },
        |item| {
            (item.anchor.qualified_symbol.is_some() || item.anchor.signature.is_some()).then(|| {
                (
                    item.anchor.qualified_symbol.clone(),
                    item.anchor.signature.clone(),
                    item.anchor.role,
                )
            })
        },
    );
    assign_unique(
        prior,
        candidates,
        &mut prior_used,
        &mut assigned,
        |item| {
            Some((
                item.anchor.repository_path.clone(),
                item.anchor.enclosing_symbol.clone(),
                item.revision,
            ))
        },
        |item| {
            Some((
                item.anchor.repository_path.clone(),
                item.anchor.enclosing_symbol.clone(),
                item.revision,
            ))
        },
    );

    // Last-resort ordered matching is allowed only for groups whose remaining
    // signatures are unique. Anonymous/duplicate siblings are ambiguous and
    // deliberately receive new lineage.
    let mut old_groups: HashMap<_, Vec<usize>> = HashMap::new();
    let mut new_groups: HashMap<_, Vec<usize>> = HashMap::new();
    for (index, item) in prior.iter().enumerate().filter(|(i, _)| !prior_used[*i]) {
        old_groups
            .entry((
                item.anchor.repository_path.clone(),
                item.anchor.enclosing_symbol.clone(),
                item.anchor.role,
            ))
            .or_default()
            .push(index);
    }
    for (index, item) in candidates
        .iter()
        .enumerate()
        .filter(|(i, _)| assigned[*i].is_none())
    {
        new_groups
            .entry((
                item.anchor.repository_path.clone(),
                item.anchor.enclosing_symbol.clone(),
                item.anchor.role,
            ))
            .or_default()
            .push(index);
    }
    for (key, mut old) in old_groups {
        let Some(mut new) = new_groups.remove(&key) else {
            continue;
        };
        if old.len() != new.len() {
            continue;
        }
        old.sort_by_key(|&index| prior[index].anchor.ast_path.clone());
        new.sort_by_key(|&index| candidates[index].anchor.ast_path.clone());
        let old_signatures = old
            .iter()
            .filter_map(|&index| prior[index].anchor.signature.as_ref())
            .collect::<std::collections::HashSet<_>>();
        let new_signatures = new
            .iter()
            .filter_map(|&index| candidates[index].anchor.signature.as_ref())
            .collect::<std::collections::HashSet<_>>();
        if old_signatures.len() != old.len() || new_signatures.len() != new.len() {
            continue;
        }
        for (old_index, new_index) in old.into_iter().zip(new) {
            prior_used[old_index] = true;
            assigned[new_index] = Some(prior[old_index].id);
        }
    }

    candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| match assigned[index] {
            Some(id) => ReconciledChunk {
                id,
                preserved: true,
            },
            None => ReconciledChunk {
                id: allocator.allocate(candidate),
                preserved: false,
            },
        })
        .collect()
}

fn assign_unique<Key>(
    prior: &[PriorChunk],
    candidates: &[ChunkCandidate],
    prior_used: &mut [bool],
    assigned: &mut [Option<ChunkId>],
    old_key: impl Fn(&PriorChunk) -> Option<Key>,
    new_key: impl Fn(&ChunkCandidate) -> Option<Key>,
) where
    Key: Eq + Hash,
{
    let mut old: HashMap<Key, Vec<usize>> = HashMap::new();
    let mut new: HashMap<Key, Vec<usize>> = HashMap::new();
    for (index, item) in prior.iter().enumerate().filter(|(i, _)| !prior_used[*i]) {
        if let Some(key) = old_key(item) {
            old.entry(key).or_default().push(index);
        }
    }
    for (index, item) in candidates
        .iter()
        .enumerate()
        .filter(|(i, _)| assigned[*i].is_none())
    {
        if let Some(key) = new_key(item) {
            new.entry(key).or_default().push(index);
        }
    }
    for (key, old_indices) in old {
        let Some(new_indices) = new.get(&key) else {
            continue;
        };
        if old_indices.len() == 1 && new_indices.len() == 1 {
            let old_index = old_indices[0];
            let new_index = new_indices[0];
            prior_used[old_index] = true;
            assigned[new_index] = Some(prior[old_index].id);
        }
    }
}

/// Whether an input is embedded as a retrieval document or query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingMode {
    /// Corpus document/passsage input.
    Document,
    /// Retrieval query input, including its asymmetric prefix.
    Query,
}

/// Every recipe component that can change an embedding for identical text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingRecipeId {
    /// Version of the source-to-document composition pipeline.
    pub pipeline_version: String,
    /// Stable embedding model identifier.
    pub model_id: String,
    /// Exact embedding weight revision.
    pub model_revision: String,
    /// Stable tokenizer identifier.
    pub tokenizer_id: String,
    /// Exact tokenizer artifact revision.
    pub tokenizer_revision: String,
    /// Query or document embedding mode.
    pub mode: EmbeddingMode,
    /// Pooling recipe version.
    pub pooling_version: String,
    /// Vector normalization recipe version.
    pub normalization_version: String,
}

impl EmbeddingRecipeId {
    /// Build the document-side recipe used by the current FastEmbed pipeline.
    ///
    /// The revision labels are deliberately explicit even while model and
    /// tokenizer artifacts share the same registry identifier. A future
    /// independent tokenizer or weights pin changes only its corresponding
    /// field and therefore invalidates exactly the affected cache namespace.
    pub fn for_document(model: EmbeddingModel, pipeline_version: impl Into<String>) -> Self {
        let model_id = model.model_name().to_string();
        Self {
            pipeline_version: pipeline_version.into(),
            model_id: model_id.clone(),
            model_revision: format!("{model_id}@fastembed-registry-v1"),
            tokenizer_id: model_id.clone(),
            tokenizer_revision: format!("{model_id}@fastembed-tokenizer-v1"),
            mode: EmbeddingMode::Document,
            pooling_version: "fastembed-default-pooling-v1".to_string(),
            normalization_version: "fastembed-l2-normalization-v1".to_string(),
        }
    }

    /// Hash every length-delimited recipe component into a stable fingerprint.
    pub fn fingerprint(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        for field in [
            self.pipeline_version.as_str(),
            self.model_id.as_str(),
            self.model_revision.as_str(),
            self.tokenizer_id.as_str(),
            self.tokenizer_revision.as_str(),
            match self.mode {
                EmbeddingMode::Document => "document",
                EmbeddingMode::Query => "query",
            },
            self.pooling_version.as_str(),
            self.normalization_version.as_str(),
        ] {
            hasher.update(&(field.len() as u64).to_le_bytes());
            hasher.update(field.as_bytes());
        }
        *hasher.finalize().as_bytes()
    }
}

/// Complete embedding cache identity, independent of source position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EmbeddingCacheIdentity {
    /// Fingerprint of every embedding recipe component.
    pub recipe: [u8; 32],
    /// Hash of the exact composed input.
    pub revision: ChunkRevision,
}

impl EmbeddingCacheIdentity {
    /// Build the complete cache identity for one composed document.
    pub fn new(recipe: &EmbeddingRecipeId, document: &str) -> Self {
        Self {
            recipe: recipe.fingerprint(),
            revision: ChunkRevision::from_document(document),
        }
    }
}

use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use serde_json::json;
use tldr_core::artifact_store::{GenerationSnapshot, RedbArtifactStore};
use tldr_core::ast::parser::parse_with_path;
use tldr_core::semantic::vector_store::plan_structural_delta_from_artifact;
use tldr_core::semantic::{
    ChunkGranularity, EmbeddingModel, FixedShapeInferenceRunner, StructuralRole,
};

fn role_name(role: StructuralRole) -> &'static str {
    match role {
        StructuralRole::WholeRoot => "whole_root",
        StructuralRole::ParentSummary => "parent_summary",
        StructuralRole::AstChild => "ast_child",
        StructuralRole::TokenizerFallback => "tokenizer_fallback",
        StructuralRole::ParseFallback => "parse_fallback",
    }
}

fn ast_owner_kind(tree: &tree_sitter::Tree, path: &[u32]) -> String {
    let mut node = tree.root_node();
    for component in path {
        if component % 2 == 0 {
            return "gap".to_string();
        }
        let ordinal = (component - 1) / 2;
        let Some(child) = node.named_child(ordinal as usize) else {
            return "unresolved".to_string();
        };
        node = child;
    }
    node.kind().to_string()
}

fn main() -> Result<()> {
    let root = std::env::current_dir()?;
    let store = RedbArtifactStore::open(&root.join(".tldr/store/project.redb"))?;
    let snapshot =
        GenerationSnapshot::active(&store)?.ok_or_else(|| anyhow!("no active generation"))?;
    let source_files = snapshot.semantic_source_chunks(&root);
    let runner = FixedShapeInferenceRunner::delta();
    runner
        .with_token_budget(EmbeddingModel::ArcticM, |budget| {
            let mut roles = BTreeMap::<String, usize>::new();
            let mut token_buckets = BTreeMap::from([
                ("1-32".to_string(), 0usize),
                ("33-64".to_string(), 0),
                ("65-128".to_string(), 0),
                ("129-256".to_string(), 0),
                ("257-384".to_string(), 0),
                ("385-512".to_string(), 0),
            ]);
            let mut owner_kinds = BTreeMap::<String, usize>::new();
            let mut per_file = Vec::<(String, usize)>::new();
            let mut chunks_total = 0usize;
            let mut tokens_total = 0usize;
            let mut source_bytes = 0usize;
            let mut planned_bytes = 0usize;
            let mut overlap_bytes = 0usize;
            let mut symbol_chunks = 0usize;
            for source in source_files {
                let tree =
                    parse_with_path(&source.content, source.language, Some(&source.file_path))
                        .map_err(|error| error.to_string())?;
                let path = source
                    .file_path
                    .strip_prefix(&root)
                    .unwrap_or(&source.file_path)
                    .to_string_lossy()
                    .replace('\\', "/");
                source_bytes += source.content.len();
                let (chunks, documents) = plan_structural_delta_from_artifact(
                    &root,
                    source,
                    budget,
                    ChunkGranularity::Function,
                )
                .map_err(|error| error.to_string())?;
                per_file.push((path, chunks.len()));
                chunks_total += chunks.len();
                for (chunk, document) in chunks.iter().zip(documents) {
                    *roles
                        .entry(role_name(chunk.structure.role).to_string())
                        .or_default() += 1;
                    *owner_kinds
                        .entry(ast_owner_kind(&tree, &chunk.structure.ast_path))
                        .or_default() += 1;
                    symbol_chunks += usize::from(chunk.structure.qualified_symbol.is_some());
                    planned_bytes += chunk
                        .structure
                        .source_range
                        .1
                        .saturating_sub(chunk.structure.source_range.0);
                    overlap_bytes += chunk.structure.overlap_bytes;
                    let tokens = budget
                        .token_count(&document)
                        .map_err(|error| error.to_string())?;
                    tokens_total += tokens;
                    *token_buckets
                        .get_mut(match tokens {
                            0..=32 => "1-32",
                            33..=64 => "33-64",
                            65..=128 => "65-128",
                            129..=256 => "129-256",
                            257..=384 => "257-384",
                            _ => "385-512",
                        })
                        .expect("complete buckets") += 1;
                }
            }
            per_file.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
            let report = serde_json::to_string_pretty(&json!({
                "generation": snapshot.generation(),
                "files": per_file.len(),
                "chunks": chunks_total,
                "tokens": tokens_total,
                "roles": roles,
                "owner_kinds": owner_kinds,
                "token_buckets": token_buckets,
                "symbol_chunks": symbol_chunks,
                "source_bytes": source_bytes,
                "planned_bytes": planned_bytes,
                "overlap_bytes": overlap_bytes,
                "top_10_files": per_file.into_iter().take(10).collect::<Vec<_>>(),
            }))
            .map_err(|error| error.to_string())?;
            std::fs::write("/tmp/tldr-context-root-after.json", &report)
                .map_err(|error| error.to_string())?;
            println!("{report}");
            Ok(())
        })
        .map_err(|error| anyhow!(error))
}

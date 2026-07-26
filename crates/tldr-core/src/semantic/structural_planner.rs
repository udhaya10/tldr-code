//! Deterministic, token-budgeted planning of complete source files.

use crate::semantic::enrichment::EmbeddingUnit;
use crate::semantic::token_budget::{TokenBudget, TokenBudgetError};
use crate::semantic::types::{ChunkGranularity, ChunkStructure, CodeChunk, StructuralRole};
use crate::{TldrError, TldrResult};

const FALLBACK_OVERLAP_TOKENS: usize = 16;

#[derive(Clone, Default)]
struct StructuralContext {
    qualified_symbol: Option<String>,
    signature: Option<String>,
}

/// Compose the exact raw model input. Source is mandatory; structural fields
/// are added whole, in priority order, only while the configured tokenizer says
/// the resulting input fits.
pub(crate) fn compose_minimal(
    chunk: &CodeChunk,
    budget: &TokenBudget,
) -> Result<String, TokenBudgetError> {
    let mut document = format!("Code:\n{}", chunk.content);
    let path = chunk
        .structure
        .ast_path
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(".");
    let fields = [
        chunk
            .structure
            .qualified_symbol
            .as_ref()
            .map(|value| format!("Symbol: {value}")),
        chunk
            .structure
            .signature
            .as_ref()
            .map(|value| format!("Signature: {value}")),
        Some(format!("File: {}", chunk.structure.repository_path)),
        Some(format!("Language: {}", chunk.language)),
        Some(format!("Role: {:?}", chunk.structure.role)),
        Some(format!("AST path: {path}")),
    ];
    for field in fields.into_iter().flatten() {
        let candidate = format!("{field}\n{document}");
        if !budget.check(&candidate)?.truncated {
            document = candidate;
        }
    }
    Ok(document)
}

/// Add enrichment one complete field at a time. A field that does not fit is
/// omitted; mandatory source and already-selected higher-priority fields remain.
pub(crate) fn compose_enriched(
    unit: &EmbeddingUnit,
    budget: &TokenBudget,
) -> Result<String, TokenBudgetError> {
    let mut document = compose_minimal(&unit.chunk, budget)?;
    let fields = [
        (!unit.calls.is_empty()).then(|| format!("Calls: {}", unit.calls.join(", "))),
        (!unit.called_by.is_empty()).then(|| format!("Called by: {}", unit.called_by.join(", "))),
        (!unit.cfg_summary.is_empty()).then(|| format!("Control flow: {}", unit.cfg_summary)),
        (!unit.dfg_summary.is_empty()).then(|| format!("Data flow: {}", unit.dfg_summary)),
        (!unit.docstring.is_empty()).then(|| format!("Description: {}", unit.docstring)),
        (!unit.dependencies.is_empty()).then(|| format!("Dependencies: {}", unit.dependencies)),
    ];
    for field in fields.into_iter().flatten() {
        let candidate = format!("{document}\n{field}");
        if !budget.check(&candidate)?.truncated {
            document = candidate;
        }
    }
    Ok(document)
}

/// Plan complete, non-overlapping source files discovered by the corpus gate.
pub(crate) fn plan_chunks(
    files: &[CodeChunk],
    budget: &TokenBudget,
    granularity: ChunkGranularity,
) -> TldrResult<Vec<CodeChunk>> {
    let mut planned = Vec::new();
    for file in files {
        plan_file(file, budget, granularity, &mut planned)?;
    }
    Ok(planned)
}

fn fits(chunk: &CodeChunk, budget: &TokenBudget) -> TldrResult<bool> {
    compose_minimal(chunk, budget)
        .and_then(|text| budget.check(&text))
        .map(|check| !check.truncated)
        .map_err(|error| {
            TldrError::Embedding(format!("structural token accounting failed: {error}"))
        })
}

fn plan_file(
    file: &CodeChunk,
    budget: &TokenBudget,
    granularity: ChunkGranularity,
    out: &mut Vec<CodeChunk>,
) -> TldrResult<()> {
    let context = StructuralContext::default();
    let whole = derived(
        file,
        0,
        file.content.len(),
        Vec::new(),
        StructuralRole::WholeRoot,
        &context,
    );
    if granularity == ChunkGranularity::File && fits(&whole, budget)? {
        out.push(whole);
        return Ok(());
    }

    let tree = match crate::ast::parser::parse_with_path(
        &file.content,
        file.language,
        Some(&file.file_path),
    ) {
        Ok(tree) => tree,
        Err(_) => {
            return fallback(
                file,
                0,
                file.content.len(),
                Vec::new(),
                StructuralRole::ParseFallback,
                &context,
                budget,
                out,
            );
        }
    };
    split_node(
        file,
        tree.root_node(),
        Vec::new(),
        &context,
        granularity == ChunkGranularity::File,
        true,
        budget,
        out,
    )
}

fn split_node(
    file: &CodeChunk,
    node: tree_sitter::Node<'_>,
    path: Vec<u32>,
    parent_context: &StructuralContext,
    allow_whole: bool,
    preserve_named_roots: bool,
    budget: &TokenBudget,
    out: &mut Vec<CodeChunk>,
) -> TldrResult<()> {
    let context = node_context(file, node, parent_context);
    let start = node.start_byte();
    let end = node.end_byte();
    let candidate = derived(
        file,
        start,
        end,
        path.clone(),
        StructuralRole::AstChild,
        &context,
    );
    if allow_whole && start < end && fits(&candidate, budget)? {
        out.push(candidate);
        return Ok(());
    }
    if node.is_error() || node.kind() == "ERROR" {
        return fallback(
            file,
            start,
            end,
            path,
            StructuralRole::ParseFallback,
            &context,
            budget,
            out,
        );
    }

    let mut cursor = node.walk();
    let children: Vec<_> = node.named_children(&mut cursor).collect();
    if children.is_empty() {
        return fallback(
            file,
            start,
            end,
            path,
            StructuralRole::TokenizerFallback,
            &context,
            budget,
            out,
        );
    }

    if let Some((signature_start, signature_end)) = signature_range(node) {
        if signature_end > signature_start && !fits(&candidate, budget)? {
            fallback(
                file,
                signature_start,
                signature_end,
                path.clone(),
                StructuralRole::ParentSummary,
                &context,
                budget,
                out,
            )?;

            // The summary owns the signature bytes. Plan only the body and any
            // trailing source so parent/child context does not duplicate source.
            let body = node
                .child_by_field_name("body")
                .expect("signature range requires body");
            let mut body_path = path.clone();
            let body_ordinal = named_child_ordinal(node, body).unwrap_or(0);
            body_path.push(body_ordinal.saturating_mul(2).saturating_add(1));
            split_node(file, body, body_path, &context, true, false, budget, out)?;
            if body.end_byte() < end {
                let mut tail_path = path;
                tail_path.push(body_ordinal.saturating_mul(2).saturating_add(2));
                fallback(
                    file,
                    body.end_byte(),
                    end,
                    tail_path,
                    StructuralRole::TokenizerFallback,
                    &context,
                    budget,
                    out,
                )?;
            }
            return Ok(());
        }
    }

    let segments = child_segments(file, node, &path, &context);
    let mut index = 0;
    while index < segments.len() {
        let segment = &segments[index];
        if let Some(child) = segment.node {
            if child.is_error() || child.kind() == "ERROR" {
                split_node(
                    file,
                    child,
                    segment.path.clone(),
                    &context,
                    true,
                    false,
                    budget,
                    out,
                )?;
                index += 1;
                continue;
            }
        }
        let mut last = index;
        let mut merged = derived(
            file,
            segment.start,
            segment.end,
            segment.path.clone(),
            StructuralRole::AstChild,
            &segment.context,
        );
        while last + 1 < segments.len() {
            let next = &segments[last + 1];
            if preserve_named_roots && next.node.is_some() {
                break;
            }
            let metadata = segments[index..=last + 1]
                .iter()
                .find(|segment| segment.context.qualified_symbol.is_some())
                .unwrap_or(segment);
            let candidate = derived(
                file,
                segment.start,
                next.end,
                metadata.path.clone(),
                StructuralRole::AstChild,
                &metadata.context,
            );
            if !fits(&candidate, budget)? {
                break;
            }
            merged = candidate;
            last += 1;
        }

        if fits(&merged, budget)? {
            out.push(merged);
        } else if last == index {
            if let Some(child) = segment.node {
                split_node(
                    file,
                    child,
                    segment.path.clone(),
                    &context,
                    true,
                    false,
                    budget,
                    out,
                )?;
            } else {
                fallback(
                    file,
                    segment.start,
                    segment.end,
                    segment.path.clone(),
                    StructuralRole::TokenizerFallback,
                    &segment.context,
                    budget,
                    out,
                )?;
            }
        }
        index = last + 1;
    }
    Ok(())
}

struct Segment<'tree> {
    start: usize,
    end: usize,
    path: Vec<u32>,
    node: Option<tree_sitter::Node<'tree>>,
    context: StructuralContext,
}

fn named_child_ordinal(node: tree_sitter::Node<'_>, child: tree_sitter::Node<'_>) -> Option<u32> {
    let mut cursor = node.walk();
    let ordinal = node
        .named_children(&mut cursor)
        .position(|candidate| candidate.id() == child.id())
        .and_then(|ordinal| u32::try_from(ordinal).ok());
    ordinal
}

fn child_segments<'tree>(
    file: &CodeChunk,
    node: tree_sitter::Node<'tree>,
    path: &[u32],
    context: &StructuralContext,
) -> Vec<Segment<'tree>> {
    let mut cursor = node.walk();
    let children: Vec<_> = node.named_children(&mut cursor).collect();
    let child_count = children.len() as u32;
    let mut segments = Vec::new();
    let mut at = node.start_byte();
    for (ordinal, child) in children.into_iter().enumerate() {
        let base = (ordinal as u32).saturating_mul(2);
        if at < child.start_byte() {
            let mut gap_path = path.to_vec();
            gap_path.push(base);
            segments.push(Segment {
                start: at,
                end: child.start_byte(),
                path: gap_path,
                node: None,
                context: context.clone(),
            });
        }
        let mut child_path = path.to_vec();
        child_path.push(base + 1);
        segments.push(Segment {
            start: child.start_byte(),
            end: child.end_byte(),
            path: child_path,
            node: Some(child),
            context: node_context(file, child, context),
        });
        at = child.end_byte();
    }
    if at < node.end_byte() {
        let mut gap_path = path.to_vec();
        gap_path.push(child_count.saturating_mul(2));
        segments.push(Segment {
            start: at,
            end: node.end_byte(),
            path: gap_path,
            node: None,
            context: context.clone(),
        });
    }
    segments
}

fn node_context(
    file: &CodeChunk,
    node: tree_sitter::Node<'_>,
    parent: &StructuralContext,
) -> StructuralContext {
    let name = crate::ast::extractor::get_definition_node_name(node, &file.content).or_else(|| {
        (node.kind() == "impl_item")
            .then(|| node.child_by_field_name("type"))
            .flatten()
            .and_then(|name| file.content.get(name.byte_range()))
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
    });
    let qualified_symbol = name
        .as_deref()
        .map(|name| match parent.qualified_symbol.as_deref() {
            Some(parent) => format!("{parent}::{name}"),
            None => name.to_string(),
        });
    let signature = signature_range(node)
        .and_then(|range| file.content.get(range.0..range.1))
        .map(str::trim)
        .filter(|signature| !signature.is_empty())
        .map(str::to_string);
    StructuralContext {
        qualified_symbol: qualified_symbol.or_else(|| parent.qualified_symbol.clone()),
        signature: signature.or_else(|| parent.signature.clone()),
    }
}

fn signature_range(node: tree_sitter::Node<'_>) -> Option<(usize, usize)> {
    let body = node.child_by_field_name("body")?;
    (body.start_byte() > node.start_byte()).then_some((node.start_byte(), body.start_byte()))
}

#[allow(clippy::too_many_arguments)]
fn fallback(
    file: &CodeChunk,
    start: usize,
    end: usize,
    path: Vec<u32>,
    role: StructuralRole,
    context: &StructuralContext,
    budget: &TokenBudget,
    out: &mut Vec<CodeChunk>,
) -> TldrResult<()> {
    if start >= end {
        return Ok(());
    }
    let source = &file.content[start..end];
    let mut allowance = budget.budget();
    loop {
        let overlap = FALLBACK_OVERLAP_TOKENS.min(allowance.saturating_sub(1));
        let windows = budget
            .byte_windows(source, allowance, overlap)
            .map_err(|error| TldrError::Embedding(format!("tokenizer fallback failed: {error}")))?;
        let mut planned = Vec::new();
        let mut previous_end = 0;
        let mut all_fit = true;
        for (window_start, window_end) in windows {
            let mut chunk = derived(
                file,
                start + window_start,
                start + window_end,
                path.clone(),
                role,
                context,
            );
            if window_start < previous_end {
                chunk.structure.overlap_range = Some((start + window_start, start + previous_end));
                chunk.structure.overlap_bytes = previous_end - window_start;
            }
            all_fit &= fits(&chunk, budget)?;
            planned.push(chunk);
            previous_end = window_end;
        }
        if all_fit && !planned.is_empty() {
            out.extend(planned);
            return Ok(());
        }
        if allowance <= 1 {
            return Err(TldrError::Embedding(
                "token budget cannot fit source plus structural context".into(),
            ));
        }
        allowance = (allowance * 3 / 4).max(1);
    }
}

fn derived(
    file: &CodeChunk,
    start: usize,
    end: usize,
    path: Vec<u32>,
    role: StructuralRole,
    context: &StructuralContext,
) -> CodeChunk {
    let content = file.content[start..end].to_string();
    let mut chunk = file.clone();
    if let Some(qualified) = context.qualified_symbol.as_deref() {
        let (owner, name) = qualified
            .rsplit_once("::")
            .map_or((None, qualified), |(owner, name)| (Some(owner), name));
        chunk.function_name = Some(name.to_string());
        chunk.class_name = owner.map(str::to_string);
    } else {
        chunk.function_name = None;
        chunk.class_name = None;
    }
    chunk.content_hash = format!("{:x}", md5::compute(content.as_bytes()));
    chunk.content = content;
    chunk.structure = ChunkStructure {
        role,
        ast_path: path,
        source_range: (start, end),
        overlap_range: None,
        overlap_bytes: 0,
        repository_path: file.structure.repository_path.clone(),
        qualified_symbol: context.qualified_symbol.clone(),
        signature: context.signature.clone(),
    };
    chunk.line_start = file.line_start
        + file.content[..start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count() as u32;
    chunk.line_end = chunk.line_start
        + chunk
            .content
            .strip_suffix('\n')
            .unwrap_or(&chunk.content)
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count() as u32;
    chunk
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokenizers::models::wordlevel::WordLevel;
    use tokenizers::pre_tokenizers::whitespace::Whitespace;
    use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

    fn budget(max: usize) -> TokenBudget {
        let vocab = ["[UNK]", "fn", "x", "let", "return", "a", "b"]
            .into_iter()
            .enumerate()
            .map(|(index, token)| (token.into(), index as u32))
            .collect();
        let model = WordLevel::builder()
            .vocab(vocab)
            .unk_token("[UNK]".into())
            .build()
            .unwrap();
        let mut tokenizer = Tokenizer::new(model);
        tokenizer.with_pre_tokenizer(Some(Whitespace));
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: max,
                ..Default::default()
            }))
            .unwrap();
        tokenizer.with_padding(Some(PaddingParams::default()));
        TokenBudget::from_configured_tokenizer(&tokenizer).unwrap()
    }

    fn file_at(source: &str, language: crate::Language, path: &str) -> CodeChunk {
        CodeChunk {
            file_path: PathBuf::from(path),
            function_name: None,
            class_name: None,
            line_start: 1,
            line_end: source.lines().count().max(1) as u32,
            content: source.into(),
            content_hash: format!("{:x}", md5::compute(source)),
            language,
            structure: ChunkStructure {
                repository_path: "src/x.rs".into(),
                ..Default::default()
            },
        }
    }

    fn file(source: &str, language: crate::Language) -> CodeChunk {
        file_at(source, language, "/repo/src/x.rs")
    }

    fn assert_coverage(source: &str, chunks: &[CodeChunk]) {
        let mut covered = vec![0_u16; source.len()];
        let mut declared_overlap = vec![false; source.len()];
        for chunk in chunks {
            if let Some((start, end)) = chunk.structure.overlap_range {
                assert!(start <= end && end <= source.len());
                assert_eq!(chunk.structure.overlap_bytes, end - start);
                for byte in &mut declared_overlap[start..end] {
                    *byte = true;
                }
            } else {
                assert_eq!(chunk.structure.overlap_bytes, 0);
            }
        }
        for chunk in chunks {
            let (start, end) = chunk.structure.source_range;
            assert_eq!(&source[start..end], chunk.content);
            for byte in &mut covered[start..end] {
                *byte += 1;
            }
        }
        assert!(covered.iter().all(|&count| count > 0));
        for (offset, count) in covered.into_iter().enumerate() {
            assert!(
                count <= 1 || declared_overlap[offset],
                "byte {offset} was duplicated without declared overlap"
            );
        }
    }

    #[test]
    fn file_granularity_keeps_an_in_budget_file_whole() {
        let source = "fn x() { return; }";
        let budget = budget(64);
        let plan = plan_chunks(
            &[file(source, crate::Language::Rust)],
            &budget,
            ChunkGranularity::File,
        )
        .unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].structure.role, StructuralRole::WholeRoot);
        assert_coverage(source, &plan);
    }

    #[test]
    fn function_granularity_preserves_adjacent_semantic_roots() {
        let source = "fn alpha() {}\nfn beta() {}\n";
        let budget = budget(64);
        let plan = plan_chunks(
            &[file(source, crate::Language::Rust)],
            &budget,
            ChunkGranularity::Function,
        )
        .unwrap();

        assert_coverage(source, &plan);
        assert_eq!(
            plan.iter()
                .filter_map(|chunk| chunk.structure.qualified_symbol.as_deref())
                .collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
    }

    #[test]
    fn function_granularity_covers_imports_comments_and_nested_blocks() {
        let source = format!(
            "// leading\nuse a::b;\nfn x() {{\n{}\n}}\n// trailing\n",
            "    if a { let b = a; }".repeat(40)
        );
        let budget = budget(32);
        let plan = plan_chunks(
            &[file(&source, crate::Language::Rust)],
            &budget,
            ChunkGranularity::Function,
        )
        .unwrap();
        assert_coverage(&source, &plan);
        assert!(plan
            .iter()
            .any(|chunk| !chunk.structure.ast_path.is_empty()));
        assert!(plan.iter().all(|chunk| !budget
            .check(&compose_minimal(chunk, &budget).unwrap())
            .unwrap()
            .truncated));
    }

    #[test]
    fn every_planned_region_survives_store_keying() {
        let source = format!(
            "// {}\nuse a::b;\nfn x() {{ return; }}\n// trailing\n",
            "comment ".repeat(100)
        );
        let budget = budget(24);
        let plan = plan_chunks(
            &[file(&source, crate::Language::Rust)],
            &budget,
            ChunkGranularity::Function,
        )
        .unwrap();
        assert_coverage(&source, &plan);

        let keyed = crate::semantic::vector_store::key_chunks(std::path::Path::new("/repo"), &plan);
        let unique: std::collections::HashSet<_> = keyed.iter().map(|(key, _)| *key).collect();
        assert_eq!(unique.len(), plan.len());
        let vectors = vec![vec![1.0, 0.0, 0.0]; plan.len()];
        let store = crate::semantic::vector_store::VectorStore::from_embedded(
            &plan,
            &vectors,
            std::path::Path::new("/repo"),
        )
        .unwrap();
        assert_eq!(store.len(), plan.len());
    }

    #[test]
    fn enriched_composition_never_sacrifices_source_or_exceeds_budget() {
        let source = "fn x() { return; }";
        let budget = budget(24);
        let chunk = plan_chunks(
            &[file(source, crate::Language::Rust)],
            &budget,
            ChunkGranularity::Function,
        )
        .unwrap()
        .remove(0);
        let unit = EmbeddingUnit {
            chunk,
            signature: "fn x()".into(),
            docstring: "documentation ".repeat(100),
            calls: vec!["callee".repeat(100)],
            called_by: vec!["caller".repeat(100)],
            cfg_summary: "branches=100 ".repeat(20),
            dfg_summary: "variables=100 ".repeat(20),
            dependencies: "dependency ".repeat(100),
        };

        let document = compose_enriched(&unit, &budget).unwrap();
        assert!(document.contains(&format!("Code:\n{source}")));
        assert!(budget.token_count(&document).unwrap() <= budget.budget());
    }

    #[test]
    fn oversized_symbol_emits_explicit_parent_summary_and_fallback_overlap() {
        let source = format!("fn x() {{ let a = \"{}\"; }}", "a ".repeat(180));
        let budget = budget(28);
        let first = plan_chunks(
            &[file(&source, crate::Language::Rust)],
            &budget,
            ChunkGranularity::Function,
        )
        .unwrap();
        let second = plan_chunks(
            &[file(&source, crate::Language::Rust)],
            &budget,
            ChunkGranularity::Function,
        )
        .unwrap();
        assert_eq!(first, second);
        assert_coverage(&source, &first);
        assert!(first
            .iter()
            .any(|chunk| chunk.structure.role == StructuralRole::ParentSummary));
        assert!(first.iter().any(|chunk| {
            chunk.structure.role == StructuralRole::TokenizerFallback
                && chunk.structure.overlap_bytes > 0
        }));
        assert!(first.iter().all(|chunk| {
            chunk
                .structure
                .qualified_symbol
                .as_deref()
                .is_none_or(|symbol| !symbol.contains("x::x"))
        }));
    }

    #[test]
    fn unrelated_leading_lines_do_not_change_symbol_boundaries() {
        let body = format!("fn x() {{ {} }}", "let a = 1; ".repeat(80));
        let budget = budget(32);
        let base = plan_chunks(
            &[file(&body, crate::Language::Rust)],
            &budget,
            ChunkGranularity::Function,
        )
        .unwrap();
        let shifted_source = format!("\n\n{body}");
        let shifted = plan_chunks(
            &[file(&shifted_source, crate::Language::Rust)],
            &budget,
            ChunkGranularity::Function,
        )
        .unwrap();

        let base_symbol: Vec<_> = base
            .iter()
            .filter(|chunk| chunk.structure.qualified_symbol.as_deref() == Some("x"))
            .map(|chunk| {
                (
                    &chunk.content,
                    &chunk.structure.ast_path,
                    chunk.structure.role,
                )
            })
            .collect();
        let shifted_symbol: Vec<_> = shifted
            .iter()
            .filter(|chunk| chunk.structure.qualified_symbol.as_deref() == Some("x"))
            .map(|chunk| {
                (
                    &chunk.content,
                    &chunk.structure.ast_path,
                    chunk.structure.role,
                )
            })
            .collect();
        assert_eq!(base_symbol, shifted_symbol);
    }

    #[test]
    fn malformed_unicode_uses_parse_fallback_without_source_loss() {
        let source = "fn x( { 世界 🌍 ".repeat(40);
        let budget = budget(32);
        let plan = plan_chunks(
            &[file(&source, crate::Language::Rust)],
            &budget,
            ChunkGranularity::Function,
        )
        .unwrap();
        assert!(plan
            .iter()
            .any(|chunk| chunk.structure.role == StructuralRole::ParseFallback));
        assert_coverage(&source, &plan);
    }

    #[test]
    fn language_aware_names_cover_declarators_bindings_and_rust_impls() {
        let generous_budget = budget(64);
        for (language, source, expected) in [
            (
                crate::Language::C,
                "int calculate(){return 0;}",
                "calculate",
            ),
            (
                crate::Language::Cpp,
                "int calculate(){return 0;}",
                "calculate",
            ),
            (
                crate::Language::Ocaml,
                "let calculate x = x + 1",
                "calculate",
            ),
        ] {
            let plan = plan_chunks(
                &[file(source, language)],
                &generous_budget,
                ChunkGranularity::Function,
            )
            .unwrap();
            assert!(plan
                .iter()
                .any(|chunk| { chunk.structure.qualified_symbol.as_deref() == Some(expected) }));
        }

        let source = format!(
            "struct Thing; impl Thing {{ fn run(&self) {{ {} }} }}",
            "let a = 1; ".repeat(80)
        );
        let plan = plan_chunks(
            &[file(&source, crate::Language::Rust)],
            &budget(32),
            ChunkGranularity::Function,
        )
        .unwrap();
        assert!(plan
            .iter()
            .any(|chunk| { chunk.structure.qualified_symbol.as_deref() == Some("Thing::run") }));
    }

    #[test]
    fn all_parser_languages_cover_complete_files() {
        let fixtures = [
            (crate::Language::Python, "def x(): pass"),
            (crate::Language::TypeScript, "export const x: number = 1;"),
            (crate::Language::JavaScript, "export const x = 1;"),
            (crate::Language::Go, "package main\nfunc main() {}"),
            (crate::Language::Rust, "pub fn x() {}"),
            (
                crate::Language::Java,
                "class X { public static void main(String[] a){} }",
            ),
            (crate::Language::C, "int main(){return 0;}"),
            (crate::Language::Cpp, "int main(){return 0;}"),
            (crate::Language::Ruby, "def x; end"),
            (crate::Language::Kotlin, "fun x(){}"),
            (crate::Language::Swift, "func x(){}"),
            (crate::Language::CSharp, "class X { static void Main(){} }"),
            (
                crate::Language::Scala,
                "object X { def main(args: Array[String]): Unit = {} }",
            ),
            (crate::Language::Php, "<?php function x(){}"),
            (crate::Language::Lua, "function x() end"),
            (crate::Language::Luau, "function x() end"),
            (
                crate::Language::Elixir,
                "defmodule X do\ndef y(), do: :ok\nend",
            ),
            (crate::Language::Ocaml, "let x () = ()"),
        ];
        assert_eq!(fixtures.len(), crate::Language::all().len());
        let budget = budget(64);
        for (language, source) in fixtures {
            let plan = plan_chunks(
                &[file(source, language)],
                &budget,
                ChunkGranularity::Function,
            )
            .unwrap();
            assert_coverage(source, &plan);
        }

        for (language, source, path) in [
            (
                crate::Language::TypeScript,
                "export const X = () => <section>hello</section>;",
                "/repo/src/x.tsx",
            ),
            (
                crate::Language::JavaScript,
                "export const X = () => <section>hello</section>;",
                "/repo/src/x.jsx",
            ),
        ] {
            let plan = plan_chunks(
                &[file_at(source, language, path)],
                &budget,
                ChunkGranularity::Function,
            )
            .unwrap();
            assert_coverage(source, &plan);
            assert!(plan
                .iter()
                .all(|chunk| chunk.structure.role != StructuralRole::ParseFallback));
        }
    }
}

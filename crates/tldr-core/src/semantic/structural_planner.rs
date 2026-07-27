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

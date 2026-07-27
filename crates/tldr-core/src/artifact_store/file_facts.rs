//! Parse-once normalized facts shared by structural and semantic consumers.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};

use crate::ast::extract::extract_from_tree;
use crate::ast::parser::parse_file_with_lang;
use crate::{Language, TldrResult};

use super::RevisionId;

/// One normalized definition.
#[derive(
    Archive, Clone, Debug, Deserialize, PartialEq, RkyvDeserialize, RkyvSerialize, Serialize,
)]
pub struct DefinitionFact {
    /// Symbol name.
    pub name: String,
    /// Function, method, class, struct, or constant.
    pub kind: String,
    /// Stable display signature.
    pub signature: String,
    /// First source line, one-indexed.
    pub line_start: u32,
    /// Last source line, one-indexed.
    pub line_end: u32,
}

/// One normalized import.
#[derive(
    Archive, Clone, Debug, Deserialize, PartialEq, RkyvDeserialize, RkyvSerialize, Serialize,
)]
pub struct ImportFact {
    /// Imported module.
    pub module: String,
    /// Selected names.
    pub names: Vec<String>,
    /// Optional alias.
    pub alias: Option<String>,
}

/// One normalized intra-file call edge.
#[derive(
    Archive, Clone, Debug, Deserialize, PartialEq, RkyvDeserialize, RkyvSerialize, Serialize,
)]
pub struct CallFact {
    /// Calling symbol.
    pub caller: String,
    /// Called symbol.
    pub callee: String,
}

/// Semantic text unit derived from the same syntax tree as structural facts.
#[derive(
    Archive, Clone, Debug, Deserialize, PartialEq, RkyvDeserialize, RkyvSerialize, Serialize,
)]
pub struct SemanticChunkFact {
    /// Stable optional function anchor.
    pub function_name: Option<String>,
    /// Optional containing class.
    pub class_name: Option<String>,
    /// Source range.
    pub line_start: u32,
    /// Source range.
    pub line_end: u32,
    /// Exact text submitted to semantic composition.
    pub content: String,
    /// Existing content-addressed hash.
    pub content_hash: String,
}

/// Complete normalized representation of one source-file revision.
#[derive(
    Archive, Clone, Debug, Deserialize, PartialEq, RkyvDeserialize, RkyvSerialize, Serialize,
)]
pub struct FileFacts {
    /// Root-relative source path.
    pub path: String,
    /// Exact source revision.
    pub revision: RevisionId,
    /// Stable lowercase language label.
    pub language: String,
    /// Definitions, including class methods.
    pub definitions: Vec<DefinitionFact>,
    /// Imports.
    pub imports: Vec<ImportFact>,
    /// Intra-file call edges.
    pub calls: Vec<CallFact>,
    /// Semantic chunks derived without another parse.
    pub semantic_chunks: Vec<SemanticChunkFact>,
    /// Non-fatal extraction diagnostics.
    pub diagnostics: Vec<String>,
}

/// Instrumented parser that produces all shared facts from one syntax tree.
#[derive(Default)]
pub struct FileFactsParser {
    invocations: AtomicU64,
}

impl FileFactsParser {
    /// Parse one file exactly once and project all normalized facts.
    pub fn parse(&self, root: &Path, path: &Path) -> TldrResult<FileFacts> {
        let language = Language::from_path_with_siblings(path)
            .or_else(|| Language::from_path(path))
            .ok_or_else(|| crate::TldrError::UnsupportedLanguage(path.display().to_string()))?;
        let (tree, source, language) = parse_file_with_lang(path, Some(language))?;
        self.invocations.fetch_add(1, Ordering::Relaxed);
        let module = extract_from_tree(&tree, &source, language, path, Some(root))?;

        let mut definitions = Vec::new();
        for function in &module.functions {
            definitions.push(function_fact(function, "function"));
        }
        for class in &module.classes {
            definitions.push(DefinitionFact {
                name: class.name.clone(),
                kind: "class".into(),
                signature: class.name.clone(),
                line_start: class.line_number,
                line_end: class.line_end,
            });
            for method in &class.methods {
                definitions.push(function_fact(method, "method"));
            }
        }
        for constant in &module.constants {
            definitions.push(DefinitionFact {
                name: constant.name.clone(),
                kind: "constant".into(),
                signature: constant.field_type.as_ref().map_or_else(
                    || constant.name.clone(),
                    |ty| format!("{}: {ty}", constant.name),
                ),
                line_start: constant.line_number,
                line_end: constant.line_end,
            });
        }

        let calls = module
            .call_graph
            .calls
            .iter()
            .flat_map(|(caller, callees)| {
                callees.iter().map(|callee| CallFact {
                    caller: caller.clone(),
                    callee: callee.clone(),
                })
            })
            .collect();

        #[cfg(feature = "semantic")]
        let semantic_chunks = crate::semantic::chunker::chunks_from_parsed(
            path,
            &source,
            &tree,
            language,
            &crate::semantic::ChunkOptions::default(),
        )
        .into_iter()
        .map(|chunk| SemanticChunkFact {
            function_name: chunk.function_name,
            class_name: chunk.class_name,
            line_start: chunk.line_start,
            line_end: chunk.line_end,
            content: chunk.content,
            content_hash: chunk.content_hash,
        })
        .collect();

        #[cfg(not(feature = "semantic"))]
        let semantic_chunks = Vec::new();

        Ok(FileFacts {
            path: root_relative(root, path),
            revision: RevisionId::for_bytes(source.as_bytes()),
            language: language.to_string(),
            definitions,
            imports: module
                .imports
                .into_iter()
                .map(|import| ImportFact {
                    module: import.module,
                    names: import.names,
                    alias: import.alias,
                })
                .collect(),
            calls,
            semantic_chunks,
            diagnostics: Vec::new(),
        })
    }

    /// Number of source parses performed by this parser.
    pub fn invocations(&self) -> u64 {
        self.invocations.load(Ordering::Relaxed)
    }
}

fn function_fact(function: &crate::FunctionInfo, kind: &str) -> DefinitionFact {
    let mut signature = format!("{}({})", function.name, function.params.join(", "));
    if let Some(return_type) = &function.return_type {
        signature.push_str(" -> ");
        signature.push_str(return_type);
    }
    DefinitionFact {
        name: function.name.clone(),
        kind: kind.into(),
        signature,
        line_start: function.line_number,
        line_end: function.line_end,
    }
}

fn root_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[allow(dead_code)]
fn _path(value: &str) -> PathBuf {
    PathBuf::from(value)
}

//! Definition command - Go-to-definition functionality
//!
//! Finds where a symbol is defined in the codebase.
//! Supports both position-based and name-based lookup.
//!
//! # Example
//!
//! ```bash
//! # Position-based: find definition of symbol at line 10, column 5
//! tldr definition src/main.py 10 5
//!
//! # Name-based: find definition by symbol name
//! tldr definition --symbol MyClass --file src/main.py
//!
//! # Cross-file resolution with project context
//! tldr definition --symbol helper --file src/main.py --project .
//! ```

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;
use tree_sitter::Node;

use super::error::{RemainingError, RemainingResult};
use super::types::{DefinitionResult, Location, SymbolInfo, SymbolKind};
use crate::output::OutputWriter;

use tldr_core::ast::parser::PARSER_POOL;
use tldr_core::callgraph::cross_file_types::{ClassDef, FuncDef};
use tldr_core::callgraph::languages::LanguageRegistry;
use tldr_core::Language;

// =============================================================================
// Constants
// =============================================================================

/// Maximum depth for import resolution to prevent cycles
const MAX_IMPORT_DEPTH: usize = 10;

/// Python built-in functions
const PYTHON_BUILTINS: &[&str] = &[
    "abs",
    "aiter",
    "all",
    "any",
    "anext",
    "ascii",
    "bin",
    "bool",
    "breakpoint",
    "bytearray",
    "bytes",
    "callable",
    "chr",
    "classmethod",
    "compile",
    "complex",
    "delattr",
    "dict",
    "dir",
    "divmod",
    "enumerate",
    "eval",
    "exec",
    "filter",
    "float",
    "format",
    "frozenset",
    "getattr",
    "globals",
    "hasattr",
    "hash",
    "help",
    "hex",
    "id",
    "input",
    "int",
    "isinstance",
    "issubclass",
    "iter",
    "len",
    "list",
    "locals",
    "map",
    "max",
    "memoryview",
    "min",
    "next",
    "object",
    "oct",
    "open",
    "ord",
    "pow",
    "print",
    "property",
    "range",
    "repr",
    "reversed",
    "round",
    "set",
    "setattr",
    "slice",
    "sorted",
    "staticmethod",
    "str",
    "sum",
    "super",
    "tuple",
    "type",
    "vars",
    "zip",
    "__import__",
];

// =============================================================================
// Graph Utils (TIGER-02 Mitigation)
// =============================================================================

/// Tracks visited nodes to detect cycles during import resolution
pub struct DefinitionCycleDetector {
    visited: HashSet<(PathBuf, String)>,
}

impl DefinitionCycleDetector {
    /// Create a new cycle detector
    pub fn new() -> Self {
        Self {
            visited: HashSet::new(),
        }
    }

    /// Visit a (file, symbol) pair. Returns true if already visited (cycle detected).
    pub fn visit(&mut self, file: &Path, symbol: &str) -> bool {
        let key = (file.to_path_buf(), symbol.to_string());
        !self.visited.insert(key)
    }
}

impl Default for DefinitionCycleDetector {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// CLI Arguments
// =============================================================================

/// Find symbol definition (go-to-definition)
///
/// Supports two modes:
/// 1. Position-based: Find symbol at file:line:column and jump to its definition
/// 2. Name-based: Find definition of a named symbol using --symbol and --file
///
/// # Example
///
/// ```bash
/// # Position mode
/// tldr definition src/main.py 10 5
///
/// # Name mode
/// tldr definition --symbol MyClass --file src/main.py
/// ```
#[derive(Debug, Args)]
pub struct DefinitionArgs {
    /// Source file (positional, for position-based lookup)
    pub file: Option<PathBuf>,

    /// line number (1-indexed, for position-based lookup)
    pub line: Option<u32>,

    /// column number (0-indexed, for position-based lookup)
    pub column: Option<u32>,

    /// Find symbol by name instead of position
    #[arg(long)]
    pub symbol: Option<String>,

    /// File to search in (used with --symbol)
    #[arg(long = "file", name = "target_file")]
    pub target_file: Option<PathBuf>,

    /// Project root for cross-file resolution
    #[arg(long)]
    pub project: Option<PathBuf>,

    /// Output file (optional, stdout if not specified)
    #[arg(long, short = 'O')]
    pub output: Option<PathBuf>,
}

impl DefinitionArgs {
    /// Run the definition command
    pub fn run(
        &self,
        format: crate::output::OutputFormat,
        quiet: bool,
        lang: Option<Language>,
    ) -> Result<()> {
        let writer = OutputWriter::new(format, quiet);

        // Convert language option to string hint
        let lang_hint = match lang {
            Some(l) => format!("{:?}", l).to_lowercase(),
            None => "auto".to_string(),
        };

        // Determine which mode we're in
        let result = if let Some(ref symbol_name) = self.symbol {
            // Name-based mode - require --file
            let file = self.target_file.as_ref().ok_or_else(|| {
                RemainingError::invalid_argument("--file is required with --symbol")
            })?;

            writer.progress(&format!(
                "Finding definition of '{}' in {}...",
                symbol_name,
                file.display()
            ));

            find_definition_by_name(symbol_name, file, self.project.as_deref(), &lang_hint)?
        } else {
            // Position-based mode
            let file = self
                .file
                .as_ref()
                .ok_or_else(|| RemainingError::invalid_argument("file argument is required"))?;
            let line = self
                .line
                .ok_or_else(|| RemainingError::invalid_argument("line argument is required"))?;
            let column = self
                .column
                .ok_or_else(|| RemainingError::invalid_argument("column argument is required"))?;

            writer.progress(&format!(
                "Finding definition at {}:{}:{}...",
                file.display(),
                line,
                column
            ));

            match find_definition_by_position(
                file,
                line,
                column,
                self.project.as_deref(),
                &lang_hint,
            ) {
                Ok(result) => result,
                Err(e) => {
                    // Return a graceful "not found" result instead of failing.
                    // The new resolver (definition-name-resolution-v1) emits
                    // an `unresolved at FILE:LINE:COL — symbol 'X' not found
                    // in scope` message via `RemainingError::InvalidArgument`
                    // — surface it verbatim in `symbol.name` so callers see
                    // a useful message, not the legacy opaque
                    // `<unknown at ...>` payload.
                    let msg = e.to_string();
                    let display_name = if msg.contains("unresolved at") {
                        format!("<{}>", msg)
                    } else {
                        format!("<unknown at {}:{}:{}>", file.display(), line, column)
                    };
                    DefinitionResult {
                        symbol: SymbolInfo {
                            name: display_name,
                            kind: SymbolKind::Variable,
                            location: Some(Location::with_column(
                                file.display().to_string(),
                                line,
                                column,
                            )),
                            type_annotation: None,
                            docstring: None,
                            is_builtin: false,
                            module: None,
                        },
                        definition: None,
                        type_definition: None,
                    }
                }
            }
        };

        // Determine output format
        let use_text = format == crate::output::OutputFormat::Text;

        // Write output
        if let Some(ref output_path) = self.output {
            if use_text {
                let text = format_definition_text(&result);
                fs::write(output_path, text)?;
            } else {
                let json = serde_json::to_string_pretty(&result)?;
                fs::write(output_path, json)?;
            }
        } else if use_text {
            let text = format_definition_text(&result);
            writer.write_text(&text)?;
        } else {
            writer.write(&result)?;
        }

        Ok(())
    }
}

// =============================================================================
// Core Functions
// =============================================================================

/// Find definition by symbol name
pub fn find_definition_by_name(
    symbol: &str,
    file: &Path,
    project: Option<&Path>,
    lang_hint: &str,
) -> RemainingResult<DefinitionResult> {
    // Validate file exists
    if !file.exists() {
        return Err(RemainingError::file_not_found(file));
    }

    // Detect language. Returns UnsupportedLanguage for genuinely unknown
    // extensions; the supported set covers all 18 TLDR languages (VAL-015).
    let language = detect_language(file, lang_hint)?;

    // Python builtins still surface as a builtin definition with no
    // location — every other language goes straight to source resolution.
    if is_builtin(symbol, &language) {
        return Ok(DefinitionResult {
            symbol: SymbolInfo {
                name: symbol.to_string(),
                kind: SymbolKind::Function,
                location: None,
                type_annotation: None,
                docstring: None,
                is_builtin: true,
                module: Some("builtins".to_string()),
            },
            definition: None,
            type_definition: None,
        });
    }

    // Read and parse file
    let source = fs::read_to_string(file).map_err(RemainingError::Io)?;

    // Try to find the symbol in this file first
    if let Some(result) = find_symbol_in_file(symbol, file, &source, language)? {
        return Ok(result);
    }

    // If not found and we have a project context, try cross-file resolution.
    if let Some(project_root) = project {
        let mut detector = DefinitionCycleDetector::new();
        if let Some(result) =
            resolve_cross_file(symbol, file, project_root, language, &mut detector, 0)?
        {
            return Ok(result);
        }
    }

    Err(RemainingError::symbol_not_found(symbol, file))
}

/// Find definition by position (line, column)
///
/// Implements a three-pass resolver (`definition-name-resolution-v1`) so
/// that cursors on USAGE sites resolve, not just on declaration sites:
///
/// 1. **Local scope**: walk up tree-sitter ancestors from the cursor and
///    look at parameter lists / let-bindings / var declarations of each
///    enclosing function/method/block. If a binding name matches the
///    symbol text, return that binding's location.
/// 2. **File scope**: scan the file for top-level definitions
///    (functions, classes, Python module-level assignments). Reuses the
///    existing [`find_symbol_in_file`] helper.
/// 3. **Import scope**: if the symbol matches an `import` / `use`
///    alias, return the import line (so `click` in `click.echo(...)`
///    resolves to `import click`).
///
/// If none match the result is a clear `<unresolved at FILE:LINE:COL —
/// symbol 'X' not found in scope>` payload, not the legacy
/// `<unknown ...>` opaque.
pub fn find_definition_by_position(
    file: &Path,
    line: u32,
    column: u32,
    project: Option<&Path>,
    lang_hint: &str,
) -> RemainingResult<DefinitionResult> {
    // Validate file exists
    if !file.exists() {
        return Err(RemainingError::file_not_found(file));
    }

    // Detect language. Supports all 18 TLDR languages (VAL-015).
    let language = detect_language(file, lang_hint)?;

    // Read and parse file
    let source = fs::read_to_string(file).map_err(RemainingError::Io)?;

    // Find symbol at position
    let symbol_name = find_symbol_at_position(&source, line, column, language, file)?;

    // Pass 1: try local-scope resolution from the cursor position.
    // This catches usages of parameters and locally-declared variables
    // before we fall through to the file/import scopes.
    if let Some(result) =
        resolve_local_scope(&source, line, column, &symbol_name, language, file)?
    {
        return Ok(result);
    }

    // Pass 2 (+ optional cross-file): existing name-based search. This
    // covers top-level functions, classes, and Python module-level
    // assignments.
    match find_definition_by_name(&symbol_name, file, project, lang_hint) {
        Ok(result) => Ok(result),
        Err(RemainingError::SymbolNotFound { .. }) => {
            // Pass 3: import-scope resolution. If the cursor sits on an
            // imported alias (`click` in `click.echo(...)`), resolve to
            // the `import` line.
            if let Some(result) = resolve_import_scope(&source, &symbol_name, language, file)? {
                return Ok(result);
            }
            // Total miss — surface a clearer message than the legacy
            // `<unknown>` shape.
            Err(RemainingError::invalid_argument(format!(
                "unresolved at {}:{}:{} — symbol '{}' not found in scope",
                file.display(),
                line,
                column,
                symbol_name
            )))
        }
        Err(e) => Err(e),
    }
}

/// Pass 1: local-scope resolution.
///
/// Walks up tree-sitter ancestors from the cursor node. For each
/// function/method/closure/block ancestor, scans its parameters and
/// variable bindings. The first matching binding wins (innermost
/// scope).
///
/// Currently covers Python (parameters, simple `=` assignments), the
/// JS/TS family (parameters, `let`/`const`/`var`), and Rust
/// (parameters, `let` bindings). Other languages fall through to the
/// next pass without resolving locally — this is the documented
/// carry-forward.
fn resolve_local_scope(
    source: &str,
    line: u32,
    column: u32,
    symbol: &str,
    language: Language,
    file: &Path,
) -> RemainingResult<Option<DefinitionResult>> {
    // Only the languages with implemented binding scrapers participate.
    // Other languages return None, falling through to file/import passes.
    if !matches!(
        language,
        Language::Python
            | Language::JavaScript
            | Language::TypeScript
            | Language::Rust
            | Language::Go
    ) {
        return Ok(None);
    }

    let tree = PARSER_POOL
        .parse_with_path(source, language, Some(file))
        .map_err(|e| RemainingError::parse_error(file.to_path_buf(), e.to_string()))?;
    let root = tree.root_node();
    let target_line = line.saturating_sub(1) as usize;
    let target_col = column as usize;
    let point = tree_sitter::Point::new(target_line, target_col);
    let Some(start_node) = root.descendant_for_point_range(point, point) else {
        return Ok(None);
    };

    // Walk up ancestors, scanning each scope-introducing ancestor for
    // bindings.
    let mut current = Some(start_node);
    while let Some(node) = current {
        if is_scope_node(node.kind(), language) {
            if let Some(loc) = scan_scope_for_binding(node, source, symbol, language, file) {
                return Ok(Some(DefinitionResult {
                    symbol: SymbolInfo {
                        name: symbol.to_string(),
                        kind: loc.0,
                        location: Some(loc.1.clone()),
                        type_annotation: None,
                        docstring: None,
                        is_builtin: false,
                        module: None,
                    },
                    definition: Some(loc.1),
                    type_definition: None,
                }));
            }
        }
        current = node.parent();
    }

    Ok(None)
}

/// Returns true for tree-sitter node kinds that introduce a new
/// lexical scope in the given language. Used by
/// [`resolve_local_scope`] to bound the per-scope binding scan.
fn is_scope_node(kind: &str, language: Language) -> bool {
    match language {
        Language::Python => matches!(
            kind,
            "function_definition" | "lambda" | "module"
        ),
        Language::JavaScript | Language::TypeScript => matches!(
            kind,
            "function_declaration"
                | "function"
                | "function_expression"
                | "arrow_function"
                | "method_definition"
                | "method_signature"
                | "statement_block"
                | "program"
        ),
        Language::Rust => matches!(
            kind,
            "function_item"
                | "closure_expression"
                | "block"
                | "source_file"
        ),
        Language::Go => matches!(
            kind,
            "function_declaration" | "method_declaration" | "block" | "source_file"
        ),
        _ => false,
    }
}

/// Scan the given scope `node` for a binding with name `symbol`.
/// Returns the kind + location of the first match found (intra-scope
/// order, recursive into bindings only).
fn scan_scope_for_binding(
    node: Node,
    source: &str,
    symbol: &str,
    language: Language,
    file: &Path,
) -> Option<(SymbolKind, Location)> {
    // Search only the scope's immediate body, but recurse into binding
    // forms. We delegate to a language-specific recursive helper.
    let bytes = source.as_bytes();
    match language {
        Language::Python => scan_python_scope(node, bytes, symbol, file),
        Language::JavaScript | Language::TypeScript => scan_jslike_scope(node, bytes, symbol, file),
        Language::Rust => scan_rust_scope(node, bytes, symbol, file),
        Language::Go => scan_go_scope(node, bytes, symbol, file),
        _ => None,
    }
}

/// Python-specific scope binding scanner.
///
/// Looks at the scope node's parameter list (when it is a function or
/// lambda) and recursively at `assignment` and `for` statements within
/// the body. Stops at nested function/class/lambda boundaries to
/// preserve lexical scoping.
fn scan_python_scope(
    node: Node,
    src: &[u8],
    symbol: &str,
    file: &Path,
) -> Option<(SymbolKind, Location)> {
    // Parameters first (only meaningful on function_definition / lambda).
    if matches!(node.kind(), "function_definition" | "lambda") {
        if let Some(params) = node.child_by_field_name("parameters") {
            if let Some(loc) = python_scan_params(params, src, symbol, file) {
                return Some(loc);
            }
        }
    }
    // Walk body looking for assignments / for-targets, but don't descend
    // into nested function/class/lambda scopes.
    let body = node
        .child_by_field_name("body")
        .or_else(|| Some(node));
    if let Some(body) = body {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if let Some(loc) = python_walk_for_binding(child, src, symbol, file) {
                return Some(loc);
            }
        }
    }
    None
}

fn python_scan_params(
    params: Node,
    src: &[u8],
    symbol: &str,
    file: &Path,
) -> Option<(SymbolKind, Location)> {
    let mut cursor = params.walk();
    for child in params.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                if child.utf8_text(src).ok()? == symbol {
                    return Some(make_param_location(child, file));
                }
            }
            // Default-argument and typed-parameter shapes wrap the name.
            "default_parameter"
            | "typed_parameter"
            | "typed_default_parameter"
            | "list_splat_pattern"
            | "dictionary_splat_pattern" => {
                let name_node = match child.child_by_field_name("name") {
                    Some(n) => Some(n),
                    None => {
                        // Fallback: first identifier child.
                        let mut c = child.walk();
                        let found = child
                            .children(&mut c)
                            .find(|n| n.kind() == "identifier");
                        found
                    }
                };
                if let Some(name) = name_node {
                    if name.utf8_text(src).ok()? == symbol {
                        return Some(make_param_location(name, file));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn python_walk_for_binding(
    node: Node,
    src: &[u8],
    symbol: &str,
    file: &Path,
) -> Option<(SymbolKind, Location)> {
    match node.kind() {
        // Don't descend into nested scopes — they have their own bindings.
        "function_definition" | "class_definition" | "lambda" => None,
        "assignment" => {
            if let Some(left) = node.child_by_field_name("left") {
                if let Some(loc) = python_match_target(left, src, symbol, file) {
                    return Some(loc);
                }
            }
            None
        }
        "for_statement" => {
            if let Some(left) = node.child_by_field_name("left") {
                if let Some(loc) = python_match_target(left, src, symbol, file) {
                    return Some(loc);
                }
            }
            // Continue into body.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(loc) = python_walk_for_binding(child, src, symbol, file) {
                    return Some(loc);
                }
            }
            None
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(loc) = python_walk_for_binding(child, src, symbol, file) {
                    return Some(loc);
                }
            }
            None
        }
    }
}

fn python_match_target(
    target: Node,
    src: &[u8],
    symbol: &str,
    file: &Path,
) -> Option<(SymbolKind, Location)> {
    match target.kind() {
        "identifier" => {
            if target.utf8_text(src).ok()? == symbol {
                Some((
                    SymbolKind::Variable,
                    Location::with_column(
                        file.display().to_string(),
                        target.start_position().row as u32 + 1,
                        target.start_position().column as u32,
                    ),
                ))
            } else {
                None
            }
        }
        // Tuple / list patterns: `a, b = ...`
        "pattern_list" | "tuple_pattern" | "list_pattern" => {
            let mut cursor = target.walk();
            for child in target.children(&mut cursor) {
                if let Some(loc) = python_match_target(child, src, symbol, file) {
                    return Some(loc);
                }
            }
            None
        }
        _ => None,
    }
}

fn make_param_location(name: Node, file: &Path) -> (SymbolKind, Location) {
    (
        SymbolKind::Parameter,
        Location::with_column(
            file.display().to_string(),
            name.start_position().row as u32 + 1,
            name.start_position().column as u32,
        ),
    )
}

/// JS/TS scope binding scanner. Handles formal parameters and
/// `let`/`const`/`var` declarations.
fn scan_jslike_scope(
    node: Node,
    src: &[u8],
    symbol: &str,
    file: &Path,
) -> Option<(SymbolKind, Location)> {
    if let Some(params) = node.child_by_field_name("parameters") {
        if let Some(loc) = jslike_scan_params(params, src, symbol, file) {
            return Some(loc);
        }
    }
    // Walk body for variable_declarations.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(loc) = jslike_walk_for_binding(child, src, symbol, file) {
            return Some(loc);
        }
    }
    None
}

fn jslike_scan_params(
    params: Node,
    src: &[u8],
    symbol: &str,
    file: &Path,
) -> Option<(SymbolKind, Location)> {
    let mut cursor = params.walk();
    for child in params.children(&mut cursor) {
        match child.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => {
                if child.utf8_text(src).ok()? == symbol {
                    return Some(make_param_location(child, file));
                }
            }
            "required_parameter" | "optional_parameter" | "rest_pattern"
            | "assignment_pattern" => {
                let pat_node = match child.child_by_field_name("pattern") {
                    Some(n) => Some(n),
                    None => {
                        let mut c = child.walk();
                        let found = child
                            .children(&mut c)
                            .find(|n| n.kind() == "identifier");
                        found
                    }
                };
                if let Some(pat) = pat_node {
                    if pat.kind() == "identifier" && pat.utf8_text(src).ok()? == symbol {
                        return Some(make_param_location(pat, file));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn jslike_walk_for_binding(
    node: Node,
    src: &[u8],
    symbol: &str,
    file: &Path,
) -> Option<(SymbolKind, Location)> {
    match node.kind() {
        // Don't descend into nested scopes.
        "function_declaration" | "function" | "function_expression" | "arrow_function"
        | "method_definition" | "method_signature" | "class_declaration" => None,
        "lexical_declaration" | "variable_declaration" => {
            // children are variable_declarators
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "variable_declarator" {
                    if let Some(name) = child.child_by_field_name("name") {
                        if name.kind() == "identifier"
                            && name.utf8_text(src).ok()? == symbol
                        {
                            return Some((
                                SymbolKind::Variable,
                                Location::with_column(
                                    file.display().to_string(),
                                    name.start_position().row as u32 + 1,
                                    name.start_position().column as u32,
                                ),
                            ));
                        }
                    }
                }
            }
            None
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(loc) = jslike_walk_for_binding(child, src, symbol, file) {
                    return Some(loc);
                }
            }
            None
        }
    }
}

/// Rust scope binding scanner. Handles function parameters and
/// `let` bindings.
fn scan_rust_scope(
    node: Node,
    src: &[u8],
    symbol: &str,
    file: &Path,
) -> Option<(SymbolKind, Location)> {
    if matches!(node.kind(), "function_item" | "closure_expression") {
        if let Some(params) = node.child_by_field_name("parameters") {
            if let Some(loc) = rust_scan_params(params, src, symbol, file) {
                return Some(loc);
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(loc) = rust_walk_for_binding(child, src, symbol, file) {
            return Some(loc);
        }
    }
    None
}

fn rust_scan_params(
    params: Node,
    src: &[u8],
    symbol: &str,
    file: &Path,
) -> Option<(SymbolKind, Location)> {
    let mut cursor = params.walk();
    for child in params.children(&mut cursor) {
        if child.kind() == "parameter" {
            if let Some(pat) = child.child_by_field_name("pattern") {
                if pat.kind() == "identifier" && pat.utf8_text(src).ok()? == symbol {
                    return Some(make_param_location(pat, file));
                }
            }
        }
    }
    None
}

fn rust_walk_for_binding(
    node: Node,
    src: &[u8],
    symbol: &str,
    file: &Path,
) -> Option<(SymbolKind, Location)> {
    match node.kind() {
        // Don't descend into nested scopes.
        "function_item" | "closure_expression" | "impl_item" => None,
        "let_declaration" => {
            if let Some(pat) = node.child_by_field_name("pattern") {
                if pat.kind() == "identifier" && pat.utf8_text(src).ok()? == symbol {
                    return Some((
                        SymbolKind::Variable,
                        Location::with_column(
                            file.display().to_string(),
                            pat.start_position().row as u32 + 1,
                            pat.start_position().column as u32,
                        ),
                    ));
                }
            }
            None
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(loc) = rust_walk_for_binding(child, src, symbol, file) {
                    return Some(loc);
                }
            }
            None
        }
    }
}

/// Go scope binding scanner. Handles function parameters and
/// short variable declarations (`x := ...`).
fn scan_go_scope(
    node: Node,
    src: &[u8],
    symbol: &str,
    file: &Path,
) -> Option<(SymbolKind, Location)> {
    if matches!(node.kind(), "function_declaration" | "method_declaration") {
        if let Some(params) = node.child_by_field_name("parameters") {
            let mut cursor = params.walk();
            for child in params.children(&mut cursor) {
                if child.kind() == "parameter_declaration" {
                    let mut c = child.walk();
                    for n in child.children(&mut c) {
                        if n.kind() == "identifier" && n.utf8_text(src).ok()? == symbol {
                            return Some(make_param_location(n, file));
                        }
                    }
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(loc) = go_walk_for_binding(child, src, symbol, file) {
            return Some(loc);
        }
    }
    None
}

fn go_walk_for_binding(
    node: Node,
    src: &[u8],
    symbol: &str,
    file: &Path,
) -> Option<(SymbolKind, Location)> {
    match node.kind() {
        "function_declaration" | "method_declaration" | "func_literal" => None,
        "short_var_declaration" | "var_declaration" => {
            if let Some(left) = node.child_by_field_name("left") {
                let mut c = left.walk();
                for n in left.children(&mut c) {
                    if n.kind() == "identifier" && n.utf8_text(src).ok()? == symbol {
                        return Some((
                            SymbolKind::Variable,
                            Location::with_column(
                                file.display().to_string(),
                                n.start_position().row as u32 + 1,
                                n.start_position().column as u32,
                            ),
                        ));
                    }
                }
            }
            None
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(loc) = go_walk_for_binding(child, src, symbol, file) {
                    return Some(loc);
                }
            }
            None
        }
    }
}

/// Pass 3: import-scope resolution.
///
/// Scans the source for `import` / `from ... import` (Python),
/// `import { ... } from ...` / `import X from ...` (JS/TS), and
/// `use ::path::X;` (Rust) statements, returning the line of the
/// matching alias. This handles the canonical case from the bug
/// report (`click` in `click.echo(...)` resolves to `import click`
/// on line 1).
fn resolve_import_scope(
    source: &str,
    symbol: &str,
    language: Language,
    file: &Path,
) -> RemainingResult<Option<DefinitionResult>> {
    let line_idx = match language {
        Language::Python => python_import_line(source, symbol),
        Language::JavaScript | Language::TypeScript => jslike_import_line(source, symbol),
        Language::Rust => rust_use_line(source, symbol),
        _ => None,
    };

    let Some((line_no, col)) = line_idx else {
        return Ok(None);
    };

    let location = Location::with_column(file.display().to_string(), line_no, col);
    Ok(Some(DefinitionResult {
        symbol: SymbolInfo {
            name: symbol.to_string(),
            kind: SymbolKind::Module,
            location: Some(location.clone()),
            type_annotation: None,
            docstring: None,
            is_builtin: false,
            module: None,
        },
        definition: Some(location),
        type_definition: None,
    }))
}

/// Find the (1-indexed line, column) of a Python import that exposes
/// `symbol`. Supports `import X`, `import X as Y`, `import a.b.c`,
/// `from M import X, Y`, `from M import X as Z`.
fn python_import_line(source: &str, symbol: &str) -> Option<(u32, u32)> {
    for (idx, raw) in source.lines().enumerate() {
        let line = raw.trim_start();
        let leading = raw.len() - line.len();
        if let Some(rest) = line.strip_prefix("import ") {
            for piece in rest.split(',') {
                let piece = piece.trim();
                if piece.is_empty() {
                    continue;
                }
                // `X as Y` — alias is what's bound.
                let bound = if let Some((_, alias)) = piece.split_once(" as ") {
                    alias.trim()
                } else {
                    // `a.b.c` binds top-level `a`.
                    piece.split('.').next().unwrap_or(piece).trim()
                };
                if bound == symbol {
                    return Some((idx as u32 + 1, leading as u32));
                }
            }
        } else if line.starts_with("from ") {
            if let Some(import_idx) = line.find(" import ") {
                let names_str = &line[import_idx + 8..];
                for piece in names_str.split(',') {
                    let piece = piece.trim().trim_start_matches('(').trim_end_matches(')');
                    if piece.is_empty() || piece == "*" {
                        continue;
                    }
                    let bound = if let Some((_, alias)) = piece.split_once(" as ") {
                        alias.trim()
                    } else {
                        piece.trim()
                    };
                    if bound == symbol {
                        return Some((idx as u32 + 1, leading as u32));
                    }
                }
            }
        }
    }
    None
}

/// Find the (1-indexed line, column) of a JS/TS `import` that
/// exposes `symbol`. Handles default, namespace, and named imports
/// (with `as` aliases). Best-effort, line-based — does not handle
/// multi-line `import { ... }` blocks.
fn jslike_import_line(source: &str, symbol: &str) -> Option<(u32, u32)> {
    for (idx, raw) in source.lines().enumerate() {
        let line = raw.trim_start();
        let leading = raw.len() - line.len();
        let body = match line.strip_prefix("import ") {
            Some(b) => b,
            None => continue,
        };
        // Strip trailing `from "..."` clause (and quoted source).
        let body = body
            .split(" from ")
            .next()
            .unwrap_or(body)
            .trim()
            .trim_end_matches(';')
            .trim();

        // Cases:
        //   X                    — default import
        //   X, { a, b as c }     — default + named
        //   { a, b as c }        — named
        //   * as X               — namespace
        //   "side-effect"        — no bindings
        if body.starts_with('"') || body.starts_with('\'') {
            continue;
        }

        // Split off namespace `* as X`.
        if let Some(rest) = body.strip_prefix("* as ") {
            let bound = rest.trim().trim_end_matches(',').trim();
            if bound == symbol {
                return Some((idx as u32 + 1, leading as u32));
            }
            continue;
        }

        // Default import: first token before `,` or `{`.
        let mut remainder = body;
        let mut pieces: Vec<&str> = Vec::new();
        if !remainder.starts_with('{') {
            // there's a default before `,` or `{`
            if let Some(idx_brace) = remainder.find('{') {
                let (default_part, rest) = remainder.split_at(idx_brace);
                let default_name = default_part.trim().trim_end_matches(',').trim();
                if !default_name.is_empty() {
                    pieces.push(default_name);
                }
                remainder = rest;
            } else {
                let default_name = remainder.trim();
                if !default_name.is_empty() {
                    pieces.push(default_name);
                }
                remainder = "";
            }
        }
        // Named import block.
        if remainder.starts_with('{') {
            let inside = remainder
                .trim_start_matches('{')
                .trim_end_matches('}')
                .trim();
            for p in inside.split(',') {
                let p = p.trim();
                if !p.is_empty() {
                    pieces.push(p);
                }
            }
        }
        for piece in pieces {
            let bound = if let Some((_, alias)) = piece.split_once(" as ") {
                alias.trim()
            } else {
                piece.trim()
            };
            if bound == symbol {
                return Some((idx as u32 + 1, leading as u32));
            }
        }
    }
    None
}

/// Find the (1-indexed line, column) of a Rust `use` that exposes
/// `symbol`. Best-effort: handles `use a::b::Symbol;` and
/// `use a::b::Symbol as Alias;` but not nested grouped (`use a::{b, c}`)
/// — those are documented as carry-forwards.
fn rust_use_line(source: &str, symbol: &str) -> Option<(u32, u32)> {
    for (idx, raw) in source.lines().enumerate() {
        let line = raw.trim_start();
        let leading = raw.len() - line.len();
        let Some(rest) = line.strip_prefix("use ") else {
            continue;
        };
        let rest = rest.trim_end_matches(';').trim();
        // Skip grouped imports (e.g. `use a::{b, c}`).
        if rest.contains('{') {
            continue;
        }
        // Last segment is the bound name (modulo `as`).
        let bound = if let Some((_, alias)) = rest.split_once(" as ") {
            alias.trim()
        } else {
            rest.rsplit("::").next().unwrap_or(rest).trim()
        };
        if bound == symbol {
            return Some((idx as u32 + 1, leading as u32));
        }
    }
    None
}

/// Find symbol name at a given position.
///
/// Parses with the given language via `ParserPool` (route TS/JS through the
/// right grammar dialect using the file path), then walks up the AST from
/// the deepest node at `(line, column)` looking for an identifier-like node.
/// Identifier kinds vary across languages — we accept any kind whose name
/// ends in `"identifier"` to cover language-specific variants
/// (`identifier`, `property_identifier`, `field_identifier`,
/// `type_identifier`, `shorthand_property_identifier`, etc.).
fn find_symbol_at_position(
    source: &str,
    line: u32,
    column: u32,
    language: Language,
    file: &Path,
) -> RemainingResult<String> {
    let tree = PARSER_POOL
        .parse_with_path(source, language, Some(file))
        .map_err(|e| RemainingError::parse_error(file.to_path_buf(), e.to_string()))?;

    // Convert 1-indexed line to 0-indexed
    let target_line = line.saturating_sub(1) as usize;
    let target_col = column as usize;

    // Find the node at the position
    let root = tree.root_node();
    let point = tree_sitter::Point::new(target_line, target_col);

    let node = root
        .descendant_for_point_range(point, point)
        .ok_or_else(|| {
            RemainingError::invalid_argument(format!(
                "No symbol found at line {}, column {}",
                line, column
            ))
        })?;

    let text = node.utf8_text(source.as_bytes()).map_err(|_| {
        RemainingError::parse_error(file.to_path_buf(), "Invalid UTF-8".to_string())
    })?;

    if is_identifier_kind(node.kind()) {
        return Ok(text.to_string());
    }

    // Walk up looking for an identifier-like node (covers cases where the
    // tree-sitter cursor lands on a wrapper node such as `call_expression`).
    let mut current = node.parent();
    while let Some(n) = current {
        if is_identifier_kind(n.kind()) {
            let text = n.utf8_text(source.as_bytes()).map_err(|_| {
                RemainingError::parse_error(file.to_path_buf(), "Invalid UTF-8".to_string())
            })?;
            return Ok(text.to_string());
        }
        current = n.parent();
    }

    // Fall back to the original token text — better than nothing.
    Ok(text.to_string())
}

/// Returns true for any tree-sitter node kind that represents an identifier
/// in one of the supported languages.
fn is_identifier_kind(kind: &str) -> bool {
    // Most languages use "identifier"; OO languages add "property_identifier",
    // "field_identifier", "type_identifier"; Ruby uses "constant" for class
    // names; Elixir/Erlang use "atom" sometimes; Lua uses "name".
    kind == "identifier"
        || kind == "property_identifier"
        || kind == "field_identifier"
        || kind == "type_identifier"
        || kind == "shorthand_property_identifier"
        || kind == "constant"
        || kind == "name"
        || kind.ends_with("_identifier")
}

/// Find a symbol definition within a single file.
///
/// Dispatches based on language:
/// - Python keeps its bespoke recursive walker so module-level
///   `assignment` definitions (variables, constants) are still found —
///   that detail is missing from the shared `extract_definitions` API.
/// - Every other language uses
///   `CallGraphLanguageSupport::extract_definitions`, which already knows
///   the per-language tree-sitter kinds for functions, methods, and
///   classes.
fn find_symbol_in_file(
    symbol: &str,
    file: &Path,
    source: &str,
    language: Language,
) -> RemainingResult<Option<DefinitionResult>> {
    if language == Language::Python {
        return find_symbol_in_file_python(symbol, file, source);
    }
    find_symbol_in_file_generic(symbol, file, source, language)
}

/// Python-specific in-file search (legacy path: handles module-level
/// `assignment` definitions in addition to functions and classes).
fn find_symbol_in_file_python(
    symbol: &str,
    file: &Path,
    source: &str,
) -> RemainingResult<Option<DefinitionResult>> {
    let tree = PARSER_POOL
        .parse_with_path(source, Language::Python, Some(file))
        .map_err(|e| RemainingError::parse_error(file.to_path_buf(), e.to_string()))?;

    let root = tree.root_node();

    if let Some((kind, location)) = find_definition_recursive(root, source, symbol, file) {
        return Ok(Some(DefinitionResult {
            symbol: SymbolInfo {
                name: symbol.to_string(),
                kind,
                location: Some(location.clone()),
                type_annotation: None,
                docstring: None,
                is_builtin: false,
                module: None,
            },
            definition: Some(location),
            type_definition: None,
        }));
    }

    Ok(None)
}

/// Generic in-file search backed by `CallGraphLanguageSupport::extract_definitions`.
///
/// The handler returns `(Vec<FuncDef>, Vec<ClassDef>)`. We match the
/// requested symbol against both vectors and translate the result into
/// the CLI's `DefinitionResult` shape.
fn find_symbol_in_file_generic(
    symbol: &str,
    file: &Path,
    source: &str,
    language: Language,
) -> RemainingResult<Option<DefinitionResult>> {
    let tree = PARSER_POOL
        .parse_with_path(source, language, Some(file))
        .map_err(|e| RemainingError::parse_error(file.to_path_buf(), e.to_string()))?;

    let registry = LanguageRegistry::with_defaults();
    let handler = registry
        .get(language.as_str())
        .ok_or_else(|| RemainingError::unsupported_language(format!("{:?}", language)))?;

    let (funcs, classes) = handler
        .extract_definitions(source, file, &tree)
        .map_err(|e| RemainingError::parse_error(file.to_path_buf(), e.to_string()))?;

    if let Some((kind, location)) = match_definition(symbol, &funcs, &classes, file) {
        return Ok(Some(DefinitionResult {
            symbol: SymbolInfo {
                name: symbol.to_string(),
                kind,
                location: Some(location.clone()),
                type_annotation: None,
                docstring: None,
                is_builtin: false,
                module: None,
            },
            definition: Some(location),
            type_definition: None,
        }));
    }

    Ok(None)
}

/// Match `symbol` against extracted FuncDefs / ClassDefs and produce a
/// `(SymbolKind, Location)` pair for the first match. Functions inside a
/// class become `Method`; standalone functions are `Function`; classes
/// (including Rust struct/enum/trait, which the handlers report as
/// classes) become `Class`.
fn match_definition(
    symbol: &str,
    funcs: &[FuncDef],
    classes: &[ClassDef],
    file: &Path,
) -> Option<(SymbolKind, Location)> {
    for f in funcs {
        if f.name == symbol {
            let kind = if f.is_method {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            };
            let loc = Location::new(file.display().to_string(), f.line);
            return Some((kind, loc));
        }
    }
    for c in classes {
        if c.name == symbol {
            let loc = Location::new(file.display().to_string(), c.line);
            return Some((SymbolKind::Class, loc));
        }
    }
    None
}

/// Recursively search the AST for a definition
fn find_definition_recursive(
    node: Node,
    source: &str,
    target_name: &str,
    file: &Path,
) -> Option<(SymbolKind, Location)> {
    match node.kind() {
        "function_definition" => {
            // Get the name child
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                    if name == target_name {
                        // Check if inside a class by looking at parents
                        let in_class = is_inside_class(node);
                        let kind = if in_class {
                            SymbolKind::Method
                        } else {
                            SymbolKind::Function
                        };
                        let location = Location::with_column(
                            file.display().to_string(),
                            name_node.start_position().row as u32 + 1,
                            name_node.start_position().column as u32,
                        );
                        return Some((kind, location));
                    }
                }
            }
        }
        "class_definition" => {
            // Get the name child
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                    if name == target_name {
                        let location = Location::with_column(
                            file.display().to_string(),
                            name_node.start_position().row as u32 + 1,
                            name_node.start_position().column as u32,
                        );
                        return Some((SymbolKind::Class, location));
                    }
                }
            }
        }
        "assignment" => {
            // Check for variable assignments at module level
            if let Some(left) = node.child_by_field_name("left") {
                if left.kind() == "identifier" {
                    if let Ok(name) = left.utf8_text(source.as_bytes()) {
                        if name == target_name {
                            let location = Location::with_column(
                                file.display().to_string(),
                                left.start_position().row as u32 + 1,
                                left.start_position().column as u32,
                            );
                            return Some((SymbolKind::Variable, location));
                        }
                    }
                }
            }
        }
        _ => {}
    }

    // Search children
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if let Some(result) = find_definition_recursive(child, source, target_name, file) {
                return Some(result);
            }
        }
    }

    None
}

/// Check if a node is inside a class definition
fn is_inside_class(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "class_definition" {
            return true;
        }
        current = n.parent();
    }
    false
}

/// Resolve `symbol` across files in the project.
///
/// Python uses an import-based resolver (parses `from X import Y` /
/// `import X` and follows them); other languages do a project-wide walk
/// of files matching the language's extensions and run
/// [`find_symbol_in_file`] on each. The walk-based approach is correct
/// for the canonical small fixture and large enough to be useful in
/// practice. For projects whose import topology is essential (large TS
/// monorepos, etc.), the daemon-backed `ModuleIndex` already handles
/// resolution and can be plugged in here as a follow-up.
fn resolve_cross_file(
    symbol: &str,
    current_file: &Path,
    project_root: &Path,
    language: Language,
    detector: &mut DefinitionCycleDetector,
    depth: usize,
) -> RemainingResult<Option<DefinitionResult>> {
    // Prevent infinite recursion
    if depth >= MAX_IMPORT_DEPTH {
        return Ok(None);
    }

    // Check for cycle
    if detector.visit(current_file, symbol) {
        return Ok(None);
    }

    if language == Language::Python {
        return resolve_cross_file_python(symbol, current_file, project_root, detector, depth);
    }

    // Generic project walk for the other 17 languages.
    resolve_cross_file_walk(symbol, current_file, project_root, language)
}

/// Python-specific cross-file resolution via parsed import statements
/// (preserves the pre-VAL-015 behaviour for Python).
fn resolve_cross_file_python(
    symbol: &str,
    current_file: &Path,
    project_root: &Path,
    detector: &mut DefinitionCycleDetector,
    depth: usize,
) -> RemainingResult<Option<DefinitionResult>> {
    let source = fs::read_to_string(current_file).map_err(RemainingError::Io)?;
    let imports = extract_imports(&source);

    for (module_path, imported_names) in imports {
        let is_imported = imported_names.is_empty() || imported_names.contains(&symbol.to_string());

        if is_imported {
            if let Some(resolved_path) =
                resolve_module_path(&module_path, current_file, project_root)
            {
                if resolved_path.exists() {
                    let module_source =
                        fs::read_to_string(&resolved_path).map_err(RemainingError::Io)?;

                    if let Some(result) = find_symbol_in_file(
                        symbol,
                        &resolved_path,
                        &module_source,
                        Language::Python,
                    )? {
                        return Ok(Some(result));
                    }

                    if let Some(result) = resolve_cross_file(
                        symbol,
                        &resolved_path,
                        project_root,
                        Language::Python,
                        detector,
                        depth + 1,
                    )? {
                        return Ok(Some(result));
                    }
                }
            }
        }
    }

    Ok(None)
}

/// Generic cross-file resolution: walk the project for files whose
/// extension belongs to `language` and probe each for the symbol.
///
/// Skips the file we already searched (`current_file`) and common
/// non-source directories (`.git`, `target`, `node_modules`, etc.) to
/// avoid pathological scans on real projects.
fn resolve_cross_file_walk(
    symbol: &str,
    current_file: &Path,
    project_root: &Path,
    language: Language,
) -> RemainingResult<Option<DefinitionResult>> {
    let extensions = language.extensions();
    let current_canonical = fs::canonicalize(current_file).ok();

    let walker = walkdir::WalkDir::new(project_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !is_skipped_dir(e.path()));

    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        // Skip non-matching extensions.
        let matches_ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| {
                extensions
                    .iter()
                    .any(|ext| ext.trim_start_matches('.').eq_ignore_ascii_case(e))
            })
            .unwrap_or(false);
        if !matches_ext {
            continue;
        }
        // Skip the file we already searched.
        if let Some(ref c) = current_canonical {
            if let Ok(p) = fs::canonicalize(path) {
                if &p == c {
                    continue;
                }
            }
        }

        let Ok(source) = fs::read_to_string(path) else {
            continue;
        };
        if let Some(result) = find_symbol_in_file(symbol, path, &source, language)? {
            return Ok(Some(result));
        }
    }

    Ok(None)
}

/// Skip well-known non-source directories during the project walk.
///
/// Returning `true` here prunes the directory and its descendants from
/// the walk, which keeps the cross-file resolver from descending into
/// `node_modules`, `target`, build outputs, and version control caches.
fn is_skipped_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    matches!(
        name,
        ".git"
            | ".hg"
            | ".svn"
            | "node_modules"
            | "target"
            | "dist"
            | "build"
            | ".venv"
            | "venv"
            | "__pycache__"
            | ".tox"
            | ".mypy_cache"
            | ".pytest_cache"
            | ".idea"
            | ".vscode"
    )
}

/// Extract import statements from source code
fn extract_imports(source: &str) -> Vec<(String, Vec<String>)> {
    let mut imports = Vec::new();

    for line in source.lines() {
        let line = line.trim();
        if line.starts_with("from ") {
            if let Some(import_idx) = line.find(" import ") {
                let module = &line[5..import_idx];
                let names_str = &line[import_idx + 8..];
                let names: Vec<String> = names_str
                    .split(',')
                    .map(|s| {
                        s.trim()
                            .split(" as ")
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_string()
                    })
                    .filter(|s| !s.is_empty() && s != "*")
                    .collect();
                imports.push((module.trim().to_string(), names));
            }
        } else if let Some(module) = line.strip_prefix("import ") {
            let module = module.split(" as ").next().unwrap_or(module).trim();
            imports.push((module.to_string(), Vec::new()));
        }
    }

    imports
}

/// Resolve a module path to a file path
///
/// Handles both absolute imports (`os.path`) and relative imports (`.utils`, `..pkg.mod`).
/// For relative imports, leading dots indicate the number of parent directories to traverse
/// from the current file's location (1 dot = same package, 2 dots = parent, etc.).
fn resolve_module_path(module: &str, current_file: &Path, project_root: &Path) -> Option<PathBuf> {
    let current_dir = current_file.parent()?;

    // Count leading dots for relative imports
    let dot_count = module.chars().take_while(|&c| c == '.').count();

    if dot_count > 0 {
        // Relative import: strip the leading dots and resolve relative to current package
        let remainder = &module[dot_count..];

        // Navigate up (dot_count - 1) directories from the current file's directory.
        // 1 dot  = same directory as current file
        // 2 dots = parent directory
        // 3 dots = grandparent directory, etc.
        let mut base = current_dir.to_path_buf();
        for _ in 1..dot_count {
            base = base.parent()?.to_path_buf();
        }

        if remainder.is_empty() {
            // "from . import X" - resolve to __init__.py in current package
            let pkg_candidate = base.join("__init__.py");
            if pkg_candidate.exists() {
                return Some(pkg_candidate);
            }
            return None;
        }

        // Convert remaining dotted path to filesystem path
        let rel_path = remainder.replace('.', "/");

        // Try as a module file
        let candidate = base.join(&rel_path).with_extension("py");
        if candidate.exists() {
            return Some(candidate);
        }

        // Try as a package directory
        let pkg_candidate = base.join(&rel_path).join("__init__.py");
        if pkg_candidate.exists() {
            return Some(pkg_candidate);
        }

        return None;
    }

    // Absolute import: try relative to current directory first, then project root
    let rel_path = module.replace('.', "/");

    // Try relative to current file's directory
    let candidate = current_dir.join(&rel_path).with_extension("py");
    if candidate.exists() {
        return Some(candidate);
    }

    // Try as package
    let pkg_candidate = current_dir.join(&rel_path).join("__init__.py");
    if pkg_candidate.exists() {
        return Some(pkg_candidate);
    }

    // Try relative to project root
    let candidate = project_root.join(&rel_path).with_extension("py");
    if candidate.exists() {
        return Some(candidate);
    }

    let pkg_candidate = project_root.join(&rel_path).join("__init__.py");
    if pkg_candidate.exists() {
        return Some(pkg_candidate);
    }

    None
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Check if a symbol is a language builtin
pub fn is_builtin(name: &str, language: &Language) -> bool {
    match language {
        Language::Python => PYTHON_BUILTINS.contains(&name),
        _ => false,
    }
}

/// Detect language from a file extension or an explicit hint.
///
/// Supports all 18 TLDR languages (VAL-015). The hint is the lower-case
/// language name (`"python"`, `"typescript"`, ..., `"ocaml"`); a hint of
/// `"auto"` falls through to extension-based detection via
/// [`Language::from_path`].
fn detect_language(file: &Path, hint: &str) -> RemainingResult<Language> {
    if hint != "auto" {
        let normalized = hint.to_lowercase();
        // Common short aliases.
        let alias = match normalized.as_str() {
            "py" => Some(Language::Python),
            "ts" => Some(Language::TypeScript),
            "tsx" => Some(Language::TypeScript),
            "js" => Some(Language::JavaScript),
            "jsx" => Some(Language::JavaScript),
            "rs" => Some(Language::Rust),
            "golang" => Some(Language::Go),
            "c++" => Some(Language::Cpp),
            "c#" => Some(Language::CSharp),
            "cs" => Some(Language::CSharp),
            "kt" => Some(Language::Kotlin),
            "rb" => Some(Language::Ruby),
            "ex" | "exs" => Some(Language::Elixir),
            "ml" | "mli" => Some(Language::Ocaml),
            _ => None,
        };
        if let Some(lang) = alias {
            return Ok(lang);
        }
        // Match against the canonical lowercase name (matches Language::as_str).
        for lang in Language::all() {
            if lang.as_str() == normalized {
                return Ok(*lang);
            }
        }
        return Err(RemainingError::unsupported_language(hint));
    }

    Language::from_path(file).ok_or_else(|| {
        let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
        RemainingError::unsupported_language(ext)
    })
}

/// Format definition result as text
fn format_definition_text(result: &DefinitionResult) -> String {
    let mut output = String::new();

    output.push_str("=== Definition Result ===\n\n");
    output.push_str(&format!("Symbol: {}\n", result.symbol.name));
    output.push_str(&format!("Kind: {:?}\n", result.symbol.kind));

    if result.symbol.is_builtin {
        output.push_str("Type: Built-in\n");
        if let Some(ref module) = result.symbol.module {
            output.push_str(&format!("Module: {}\n", module));
        }
    } else if let Some(ref location) = result.definition {
        output.push_str("\nDefinition Location:\n");
        output.push_str(&format!("  File: {}\n", location.file));
        output.push_str(&format!("  Line: {}\n", location.line));
        if location.column > 0 {
            output.push_str(&format!("  Column: {}\n", location.column));
        }
    } else {
        output.push_str("\nDefinition: Not found\n");
    }

    if let Some(ref type_def) = result.type_definition {
        output.push_str("\nType Definition:\n");
        output.push_str(&format!("  File: {}\n", type_def.file));
        output.push_str(&format!("  Line: {}\n", type_def.line));
    }

    if let Some(ref docstring) = result.symbol.docstring {
        output.push_str(&format!("\nDocstring:\n  {}\n", docstring));
    }

    output
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_builtin_python() {
        assert!(is_builtin("len", &Language::Python));
        assert!(is_builtin("print", &Language::Python));
        assert!(is_builtin("range", &Language::Python));
        assert!(!is_builtin("my_func", &Language::Python));
    }

    #[test]
    fn test_cycle_detector() {
        let mut detector = DefinitionCycleDetector::new();

        // First visit should return false (not a cycle)
        assert!(!detector.visit(Path::new("file.py"), "symbol"));

        // Second visit to same location should return true (cycle)
        assert!(detector.visit(Path::new("file.py"), "symbol"));

        // Different location should return false
        assert!(!detector.visit(Path::new("other.py"), "symbol"));
    }

    #[test]
    fn test_detect_language() {
        assert_eq!(
            detect_language(Path::new("test.py"), "auto").unwrap(),
            Language::Python
        );
    }

    #[test]
    fn test_detect_language_with_hint() {
        assert_eq!(
            detect_language(Path::new("test.txt"), "python").unwrap(),
            Language::Python
        );
    }

    #[test]
    fn test_extract_imports() {
        let source = r#"
from os import path, getcwd
from sys import argv
import json
import re as regex
"#;
        let imports = extract_imports(source);

        assert_eq!(imports.len(), 4);
        assert_eq!(imports[0].0, "os");
        assert!(imports[0].1.contains(&"path".to_string()));
        assert!(imports[0].1.contains(&"getcwd".to_string()));
        assert_eq!(imports[1].0, "sys");
        assert!(imports[1].1.contains(&"argv".to_string()));
        assert_eq!(imports[2].0, "json");
        assert_eq!(imports[3].0, "re");
    }

    #[test]
    fn test_extract_imports_relative() {
        let source = r#"
from .utils import echo, make_str
from .exceptions import Abort
from ._utils import FLAG_NEEDS_VALUE
from . import types
"#;
        let imports = extract_imports(source);

        assert_eq!(imports.len(), 4);
        // Relative imports should preserve the dot prefix
        assert_eq!(imports[0].0, ".utils");
        assert!(imports[0].1.contains(&"echo".to_string()));
        assert!(imports[0].1.contains(&"make_str".to_string()));
        assert_eq!(imports[1].0, ".exceptions");
        assert!(imports[1].1.contains(&"Abort".to_string()));
        assert_eq!(imports[2].0, "._utils");
        assert!(imports[2].1.contains(&"FLAG_NEEDS_VALUE".to_string()));
        assert_eq!(imports[3].0, ".");
        assert!(imports[3].1.contains(&"types".to_string()));
    }

    #[test]
    fn test_resolve_module_path_relative_import() {
        // Create a temp directory structure simulating a Python package
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("mypkg");
        fs::create_dir_all(&pkg).unwrap();

        // Create files
        fs::write(pkg.join("__init__.py"), "").unwrap();
        fs::write(pkg.join("core.py"), "from .utils import helper\n").unwrap();
        fs::write(pkg.join("utils.py"), "def helper(): pass\n").unwrap();

        let current_file = pkg.join("core.py");
        let project_root = dir.path();

        // Relative import ".utils" from core.py should resolve to utils.py in the same directory
        let resolved = resolve_module_path(".utils", &current_file, project_root);
        assert!(
            resolved.is_some(),
            "resolve_module_path should find .utils relative to core.py"
        );
        assert_eq!(
            resolved.unwrap(),
            pkg.join("utils.py"),
            "Should resolve to sibling utils.py"
        );
    }

    #[test]
    fn test_resolve_module_path_relative_import_subpackage() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("mypkg");
        let sub = pkg.join("sub");
        fs::create_dir_all(&sub).unwrap();

        fs::write(pkg.join("__init__.py"), "").unwrap();
        fs::write(sub.join("__init__.py"), "").unwrap();
        fs::write(pkg.join("core.py"), "").unwrap();
        fs::write(sub.join("helpers.py"), "def helper(): pass\n").unwrap();

        let current_file = pkg.join("core.py");
        let project_root = dir.path();

        // ".sub.helpers" from core.py should resolve to sub/helpers.py
        let resolved = resolve_module_path(".sub.helpers", &current_file, project_root);
        assert!(
            resolved.is_some(),
            "resolve_module_path should find .sub.helpers relative to core.py"
        );
        assert_eq!(
            resolved.unwrap(),
            sub.join("helpers.py"),
            "Should resolve to sub/helpers.py"
        );
    }

    #[test]
    fn test_cross_file_definition_via_relative_import() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("mypkg");
        fs::create_dir_all(&pkg).unwrap();

        fs::write(pkg.join("__init__.py"), "").unwrap();
        fs::write(
            pkg.join("core.py"),
            "from .utils import echo\n\ndef main():\n    echo('hello')\n",
        )
        .unwrap();
        fs::write(pkg.join("utils.py"), "def echo(msg):\n    print(msg)\n").unwrap();

        // Look for 'echo' starting from core.py with project context
        let result =
            find_definition_by_name("echo", &pkg.join("core.py"), Some(dir.path()), "python");

        assert!(
            result.is_ok(),
            "Should find echo via cross-file resolution: {:?}",
            result.err()
        );
        let result = result.unwrap();
        assert_eq!(result.symbol.name, "echo");
        assert_eq!(result.symbol.kind, SymbolKind::Function);
        assert!(
            result.definition.is_some(),
            "Should have a definition location"
        );
        let def_loc = result.definition.unwrap();
        assert!(
            def_loc.file.contains("utils.py"),
            "Definition should be in utils.py, got: {}",
            def_loc.file
        );
        assert_eq!(def_loc.line, 1, "echo is defined on line 1 of utils.py");
    }

    // -------------------------------------------------------------------------
    // VAL-015: multi-language go-to-definition
    //
    // Until VAL-015, find_definition_by_name and find_definition_by_position
    // returned UnsupportedLanguage for any non-Python file. These tests
    // verify the generalisation: the dispatch reuses each language handler's
    // CallGraphLanguageSupport::extract_definitions API to locate the
    // definition site of a top-level function in a single file.
    //
    // Coverage: Python (regression), TypeScript (brace-language family),
    // Rust (strict-types), Go (semicolon-free), Java (OOP).
    // -------------------------------------------------------------------------

    #[test]
    fn test_find_definition_typescript_function() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("main.ts");
        fs::write(
            &file,
            "export function target_fn(): number { return 42; }\n\
             export function caller(): void { target_fn(); }\n",
        )
        .unwrap();

        let result = find_definition_by_name("target_fn", &file, None, "typescript")
            .expect("definition lookup should succeed for TypeScript");
        assert_eq!(result.symbol.name, "target_fn");
        assert_eq!(result.symbol.kind, SymbolKind::Function);
        let loc = result.definition.expect("definition location must be Some");
        assert_eq!(loc.line, 1, "target_fn is on line 1, got {}", loc.line);
    }

    #[test]
    fn test_find_definition_rust_function() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        fs::write(
            &file,
            "fn helper() -> i32 { 1 }\n\nfn target_fn() -> i32 { helper() }\n",
        )
        .unwrap();

        let result = find_definition_by_name("target_fn", &file, None, "rust")
            .expect("definition lookup should succeed for Rust");
        assert_eq!(result.symbol.name, "target_fn");
        assert_eq!(result.symbol.kind, SymbolKind::Function);
        let loc = result.definition.expect("definition location must be Some");
        assert_eq!(loc.line, 3, "target_fn is on line 3, got {}", loc.line);
    }

    #[test]
    fn test_find_definition_go_function() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("main.go");
        fs::write(
            &file,
            "package main\n\nfunc target_fn() int { return 1 }\n\nfunc main() { target_fn() }\n",
        )
        .unwrap();

        let result = find_definition_by_name("target_fn", &file, None, "go")
            .expect("definition lookup should succeed for Go");
        assert_eq!(result.symbol.name, "target_fn");
        assert_eq!(result.symbol.kind, SymbolKind::Function);
        let loc = result.definition.expect("definition location must be Some");
        assert_eq!(loc.line, 3, "target_fn is on line 3, got {}", loc.line);
    }

    #[test]
    fn test_find_definition_java_method() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Main.java");
        // Java requires methods inside a class; the matrix fixture follows
        // the same pattern.
        fs::write(
            &file,
            "class Main {\n    public static int target_fn() { return 1; }\n    public static void main(String[] args) { target_fn(); }\n}\n",
        )
        .unwrap();

        let result = find_definition_by_name("target_fn", &file, None, "java")
            .expect("definition lookup should succeed for Java");
        assert_eq!(result.symbol.name, "target_fn");
        // Methods inside a class must report Method, not Function.
        assert_eq!(
            result.symbol.kind,
            SymbolKind::Method,
            "Java method inside class should be Method, got {:?}",
            result.symbol.kind
        );
        let loc = result.definition.expect("definition location must be Some");
        assert_eq!(loc.line, 2, "target_fn is on line 2, got {}", loc.line);
    }

    #[test]
    fn test_find_definition_class_typescript() {
        // Classes must surface as SymbolKind::Class regardless of language.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("widget.ts");
        fs::write(&file, "export class Widget {\n    render(): void {}\n}\n").unwrap();

        let result = find_definition_by_name("Widget", &file, None, "typescript")
            .expect("definition lookup should succeed for TS class");
        assert_eq!(result.symbol.name, "Widget");
        assert_eq!(
            result.symbol.kind,
            SymbolKind::Class,
            "Widget should be Class kind, got {:?}",
            result.symbol.kind
        );
        let loc = result.definition.expect("definition location must be Some");
        assert_eq!(loc.line, 1);
    }

    #[test]
    fn test_find_definition_position_rust() {
        // Position-based lookup: jump from a call site to the definition.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        let source = "fn target_fn() -> i32 { 1 }\n\nfn caller() -> i32 { target_fn() }\n";
        fs::write(&file, source).unwrap();

        // Position of the `target_fn` reference inside caller.
        // Line 3, column 22 (0-indexed) — points at `target_fn` in the call.
        // "fn caller() -> i32 { target_fn() }"
        //  0123456789012345678901
        //                       ^ col 21 = 't'
        let result = find_definition_by_position(&file, 3, 22, None, "rust")
            .expect("position-based lookup should succeed for Rust");
        assert_eq!(result.symbol.name, "target_fn");
        let loc = result.definition.expect("definition location must be Some");
        assert_eq!(loc.line, 1, "definition is on line 1");
    }

    #[test]
    fn test_detect_language_all_18() {
        // All 18 languages must be detectable from extension or hint.
        // This catches missing entries in detect_language as we add support.
        let cases: &[(&str, &str, Language)] = &[
            ("a.py", "auto", Language::Python),
            ("a.ts", "auto", Language::TypeScript),
            ("a.tsx", "auto", Language::TypeScript),
            ("a.js", "auto", Language::JavaScript),
            ("a.jsx", "auto", Language::JavaScript),
            ("a.rs", "auto", Language::Rust),
            ("a.go", "auto", Language::Go),
            ("a.java", "auto", Language::Java),
            ("a.c", "auto", Language::C),
            ("a.h", "auto", Language::C),
            ("a.cpp", "auto", Language::Cpp),
            ("a.cc", "auto", Language::Cpp),
            ("a.hpp", "auto", Language::Cpp),
            ("a.rb", "auto", Language::Ruby),
            ("a.kt", "auto", Language::Kotlin),
            ("a.swift", "auto", Language::Swift),
            ("a.cs", "auto", Language::CSharp),
            ("a.scala", "auto", Language::Scala),
            ("a.php", "auto", Language::Php),
            ("a.lua", "auto", Language::Lua),
            ("a.luau", "auto", Language::Luau),
            ("a.ex", "auto", Language::Elixir),
            ("a.exs", "auto", Language::Elixir),
            ("a.ml", "auto", Language::Ocaml),
        ];
        for (path, hint, expected) in cases {
            let got = detect_language(Path::new(path), hint)
                .unwrap_or_else(|e| panic!("detect_language failed for {}: {:?}", path, e));
            assert_eq!(got, *expected, "wrong language for {}", path);
        }
    }

    // -------------------------------------------------------------------------
    // definition-name-resolution-v1 — three-pass resolver tests
    //
    // Before this milestone, `tldr definition <file> <line> <col>` only
    // resolved when the cursor sat ON a function/class declaration. Cursors
    // on USAGE sites returned `<unknown at FILE:LINE:COL>`. The three-pass
    // resolver fixes that:
    //   Pass 1 — local scope (params, let/var bindings)
    //   Pass 2 — file scope (existing handler)
    //   Pass 3 — import scope (`import X` aliases)
    // -------------------------------------------------------------------------

    #[test]
    fn test_definition_resolves_local_param() {
        // Cursor on the usage of a parameter `x` in `def foo(x): return x + 1`
        // should resolve to the parameter, not return <unknown>.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("local.py");
        // Line 1: def foo(x):
        // Line 2:     return x + 1
        fs::write(&file, "def foo(x):\n    return x + 1\n").unwrap();

        // Cursor on `x` in `return x + 1` — column 11 of line 2.
        let result = find_definition_by_position(&file, 2, 11, None, "python")
            .expect("local-scope resolution should succeed");
        assert_eq!(result.symbol.name, "x");
        assert_eq!(
            result.symbol.kind,
            SymbolKind::Parameter,
            "should resolve local `x` as Parameter, got {:?}",
            result.symbol.kind
        );
        let def = result.definition.expect("definition location must be Some");
        assert_eq!(def.line, 1, "param `x` is declared on line 1, got {}", def.line);
    }

    #[test]
    fn test_definition_resolves_file_scope_function() {
        // Cursor on a usage of `helper` should resolve to its top-level
        // declaration line.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("filescope.py");
        // Line 1: def helper():
        // Line 2:     return 1
        // Line 3:
        // Line 4: def main():
        // Line 5:     return helper()
        fs::write(
            &file,
            "def helper():\n    return 1\n\ndef main():\n    return helper()\n",
        )
        .unwrap();

        // Cursor on `helper` in `return helper()` — column 11 of line 5.
        let result = find_definition_by_position(&file, 5, 11, None, "python")
            .expect("file-scope resolution should succeed");
        assert_eq!(result.symbol.name, "helper");
        assert_eq!(result.symbol.kind, SymbolKind::Function);
        let def = result.definition.expect("definition location must be Some");
        assert_eq!(
            def.line, 1,
            "helper is declared on line 1, got {}",
            def.line
        );
    }

    #[test]
    fn test_definition_resolves_import_alias() {
        // Cursor on `click` in `click.echo(...)` should resolve to the
        // `import click` line — this is the canonical BUG-24 repro.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("imports.py");
        // Line 1: import click
        // Line 2:
        // Line 3: def main():
        // Line 4:     click.echo("hi")
        fs::write(
            &file,
            "import click\n\ndef main():\n    click.echo(\"hi\")\n",
        )
        .unwrap();

        // Cursor on `click` at column 4 of line 4.
        let result = find_definition_by_position(&file, 4, 4, None, "python")
            .expect("import-scope resolution should succeed");
        assert_eq!(result.symbol.name, "click");
        let def = result
            .definition
            .expect("import-scope resolution must produce a definition location");
        assert_eq!(
            def.line, 1,
            "import click is on line 1, got {}",
            def.line
        );
    }

    #[test]
    fn test_definition_unresolved_message() {
        // Cursor on a name that doesn't exist in any scope should produce a
        // payload whose symbol.name contains `unresolved at`.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("unresolved.py");
        // Line 1: x = 1
        // Line 2: print(nonexistent_name)
        fs::write(&file, "x = 1\nprint(nonexistent_name)\n").unwrap();

        // Cursor on `nonexistent_name` — column 6 of line 2.
        let err = find_definition_by_position(&file, 2, 6, None, "python")
            .expect_err("unresolved name must produce an error");
        let msg = err.to_string();
        assert!(
            msg.contains("unresolved at"),
            "error should mention 'unresolved at', got: {}",
            msg
        );
        assert!(
            msg.contains("nonexistent_name"),
            "error should mention the symbol, got: {}",
            msg
        );
    }

    #[test]
    fn test_definition_resolves_js_import_alias() {
        // JS namespace import: `import express from "express"` and a usage
        // `express()` — cursor on `express` should resolve to the import.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("app.js");
        // Line 1: const express = require("express");
        // ... we use ES-module style for the parser:
        // Line 1: import express from "express";
        // Line 2: const app = express();
        fs::write(
            &file,
            "import express from \"express\";\nconst app = express();\n",
        )
        .unwrap();

        // Cursor on `express` in `express()` — column 12 of line 2.
        let result = find_definition_by_position(&file, 2, 12, None, "javascript")
            .expect("JS import resolution should succeed");
        assert_eq!(result.symbol.name, "express");
        let def = result.definition.expect("definition location must be Some");
        assert_eq!(def.line, 1, "import is on line 1, got {}", def.line);
    }

    #[test]
    fn test_definition_resolves_rust_let_binding() {
        // Cursor on a usage of a `let`-bound local should resolve to the
        // let-binding.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        // Line 1: fn main() {
        // Line 2:     let counter = 42;
        // Line 3:     println!("{}", counter);
        // Line 4: }
        fs::write(
            &file,
            "fn main() {\n    let counter = 42;\n    println!(\"{}\", counter);\n}\n",
        )
        .unwrap();

        // Cursor on `counter` in `println!` — column 19 of line 3.
        let result = find_definition_by_position(&file, 3, 19, None, "rust")
            .expect("Rust let-binding resolution should succeed");
        assert_eq!(result.symbol.name, "counter");
        assert_eq!(result.symbol.kind, SymbolKind::Variable);
        let def = result.definition.expect("definition location must be Some");
        assert_eq!(
            def.line, 2,
            "let counter is on line 2, got {}",
            def.line
        );
    }
}

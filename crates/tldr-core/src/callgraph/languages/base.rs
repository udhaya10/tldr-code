//! Base helpers for language handlers.
//!
//! This module provides shared utility functions used by all language handlers:
//! - Path normalization
//! - Safe source file reading with UTF-8/Latin-1 fallback
//! - Tree-sitter node text extraction
//! - Call type classification
//!
//! # Spec Reference
//!
//! See `migration/spec/callgraph-spec.md` Section 8.2 for the full specification.

use std::fs;
use std::io;
use std::path::Path;

use tree_sitter::Node;

use super::super::cross_file_types::{CallType, ImportDef};

// =============================================================================
// Path Normalization
// =============================================================================

/// Normalize a path to use forward slashes.
///
/// This ensures consistent path representation across platforms.
///
/// # Arguments
///
/// * `path` - The path to normalize
/// * `root` - Optional root to make path relative to
///
/// # Returns
///
/// A string with forward slashes, optionally relative to root.
///
/// # Example
///
/// ```rust
/// use tldr_core::callgraph::languages::base::normalize_path;
/// use std::path::Path;
///
/// assert_eq!(normalize_path(Path::new("src\\main.py"), None), "src/main.py");
/// assert_eq!(
///     normalize_path(Path::new("/project/src/main.py"), Some(Path::new("/project"))),
///     "src/main.py"
/// );
/// ```
pub fn normalize_path(path: &Path, root: Option<&Path>) -> String {
    let path_str = if let Some(root) = root {
        // Make relative to root if possible
        path.strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string()
    } else {
        path.to_string_lossy().to_string()
    };

    // Convert backslashes to forward slashes
    path_str.replace('\\', "/")
}

// =============================================================================
// Safe File Reading
// =============================================================================

/// Read a source file with UTF-8/Latin-1 fallback.
///
/// Attempts to read the file as UTF-8 first. If that fails due to invalid
/// UTF-8 sequences, falls back to Latin-1 (ISO-8859-1) encoding.
///
/// # Arguments
///
/// * `path` - Path to the source file
///
/// # Returns
///
/// The file contents as a String, or an IO error.
///
/// # Encoding Strategy
///
/// 1. Try UTF-8 (most common, and what Rust expects)
/// 2. If invalid UTF-8, decode as Latin-1 (every byte is valid Latin-1)
///
/// This matches the Python implementation which uses similar fallback logic.
///
/// # Example
///
/// ```rust,ignore
/// use tldr_core::callgraph::languages::base::read_source_safely;
/// use std::path::Path;
///
/// let source = read_source_safely(Path::new("src/main.py"))?;
/// ```
pub fn read_source_safely(path: &Path) -> Result<String, io::Error> {
    // Read raw bytes
    let bytes = fs::read(path)?;

    // Try UTF-8 first
    match String::from_utf8(bytes.clone()) {
        Ok(s) => Ok(s),
        Err(_) => {
            // Fall back to Latin-1 (ISO-8859-1)
            // Every byte is valid in Latin-1, so this always succeeds
            Ok(bytes.iter().map(|&b| b as char).collect())
        }
    }
}

// =============================================================================
// Tree-Sitter Helpers
// =============================================================================

/// Extract text content from a tree-sitter node.
///
/// # Arguments
///
/// * `node` - The tree-sitter node
/// * `source` - The source code as a byte slice
///
/// # Returns
///
/// The text content of the node, or an empty string if extraction fails.
///
/// # Example
///
/// ```rust,ignore
/// let text = get_node_text(&node, source.as_bytes());
/// ```
pub fn get_node_text<'a>(node: &Node, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or("")
}

/// Extract text content from a tree-sitter node, owned version.
///
/// Returns an owned String instead of a reference.
pub fn get_node_text_owned(node: &Node, source: &[u8]) -> String {
    get_node_text(node, source).to_string()
}

/// Extract function name from a call node.
///
/// This handles various call patterns:
/// - Direct call: `func()` -> "func"
/// - Attribute call: `obj.method()` -> "obj.method"
/// - Chained call: `a.b.c()` -> "a.b.c"
///
/// # Arguments
///
/// * `node` - A "call" node from tree-sitter
/// * `source` - The source code
///
/// # Returns
///
/// The extracted call name, or None if extraction fails.
pub fn extract_call_name(node: &Node, source: &str) -> Option<String> {
    let source_bytes = source.as_bytes();

    // Look for the function part of the call
    // Different languages use different node types:
    // - Python: "call" with "function" child
    // - TypeScript: "call_expression" with "function" child
    // - Go: "call_expression" with "function" child

    // Try common child names
    for child_name in &["function", "callee", "receiver"] {
        if let Some(func_node) = node.child_by_field_name(child_name) {
            return Some(get_node_text_owned(&func_node, source_bytes));
        }
    }

    // Fallback: use the first named child
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            // Skip argument lists
            if child.kind().contains("argument") {
                continue;
            }
            return Some(get_node_text_owned(&child, source_bytes));
        }
    }

    None
}

/// Determine call type from AST context.
///
/// Classifies a call based on:
/// - Whether target is a known local function (Intra)
/// - Whether target has a dot (Attr/Method)
/// - Whether it's a static/class method call (Static)
///
/// # Arguments
///
/// * `target` - The call target string
/// * `defined_funcs` - Set of function names defined in the current file
///
/// # Returns
///
/// The classified CallType.
pub fn determine_call_type(
    target: &str,
    defined_funcs: &std::collections::HashSet<String>,
) -> CallType {
    // Check for attribute/method access
    if target.contains('.') {
        // Could be method call (obj.method) or module access (os.path.join)
        // Default to Attr; type resolver may upgrade to Method later
        return CallType::Attr;
    }

    // Check for static method call (primarily PHP)
    if target.contains("::") {
        return CallType::Static;
    }

    // Check if it's a local/intra-file call
    if defined_funcs.contains(target) {
        return CallType::Intra;
    }

    // Default to Direct (will be resolved via imports)
    CallType::Direct
}

// =============================================================================
// Import Helpers
// =============================================================================

/// Helper to create ImportDef from parsed data.
///
/// # Arguments
///
/// * `module` - Module path
/// * `names` - Imported names
/// * `is_from` - True for "from X import Y" style
/// * `level` - Relative import level (0 = absolute)
///
/// # Example
///
/// ```rust
/// use tldr_core::callgraph::languages::base::make_import;
///
/// // from os.path import join
/// let imp = make_import("os.path", &["join"], true, 0);
/// assert!(imp.is_from);
/// assert_eq!(imp.names, vec!["join"]);
///
/// // from . import utils
/// let imp = make_import("", &["utils"], true, 1);
/// assert!(imp.is_relative());
/// ```
pub fn make_import(module: &str, names: &[&str], is_from: bool, level: u8) -> ImportDef {
    ImportDef {
        module: module.to_string(),
        is_from,
        names: names.iter().map(|s| s.to_string()).collect(),
        alias: None,
        aliases: None,
        resolved_module: None,
        is_default: false,
        is_namespace: false,
        is_mod: false,
        level,
        is_type_checking: false,
        scope: None,
        line: None,
    }
}

/// Helper to create ImportDef with an alias.
pub fn make_import_with_alias(module: &str, alias: &str, level: u8) -> ImportDef {
    ImportDef {
        module: module.to_string(),
        is_from: false,
        names: vec![],
        alias: Some(alias.to_string()),
        aliases: None,
        resolved_module: None,
        is_default: false,
        is_namespace: false,
        is_mod: false,
        level,
        is_type_checking: false,
        scope: None,
        line: None,
    }
}

// =============================================================================
// Tree Walking
// =============================================================================

/// Iterator that walks all nodes in a tree-sitter tree.
///
/// Performs a depth-first traversal of the entire tree.
pub struct TreeWalker<'a> {
    cursor: tree_sitter::TreeCursor<'a>,
    done: bool,
}

impl<'a> TreeWalker<'a> {
    /// Create a new tree walker starting from the given node.
    pub fn new(node: Node<'a>) -> Self {
        Self {
            cursor: node.walk(),
            done: false,
        }
    }
}

impl<'a> Iterator for TreeWalker<'a> {
    type Item = Node<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }

        let node = self.cursor.node();

        // Try to go to first child
        if self.cursor.goto_first_child() {
            return Some(node);
        }

        // Try to go to next sibling
        if self.cursor.goto_next_sibling() {
            return Some(node);
        }

        // Go up until we can go to a sibling
        loop {
            if !self.cursor.goto_parent() {
                self.done = true;
                return Some(node);
            }
            if self.cursor.goto_next_sibling() {
                return Some(node);
            }
        }
    }
}

/// Walk all nodes in a tree.
///
/// # Example
///
/// ```rust,ignore
/// for node in walk_tree(tree.root_node()) {
///     if node.kind() == "function_definition" {
///         // Process function
///     }
/// }
/// ```
pub fn walk_tree(node: Node<'_>) -> TreeWalker<'_> {
    TreeWalker::new(node)
}

// =============================================================================
// Tests
// =============================================================================

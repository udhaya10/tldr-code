//! Reference counting for dead code rescue
//!
//! Counts how many times each identifier appears across the codebase
//! using tree-sitter AST parsing. Used to "rescue" functions from dead
//! code reports when they are referenced multiple times (indicating
//! they are used, just not through call-graph edges).
//!
//! # Algorithm
//! 1. Walk each file's AST using TreeCursor
//! 2. Collect all identifier nodes appropriate for the language
//! 3. Count occurrences of each identifier name
//! 4. A function with ref_count > 1 and name length >= 3 is "rescued"

use std::collections::HashMap;

use tree_sitter::Tree;

use crate::types::Language;

/// Returns the tree-sitter node type names that represent identifiers for the given language.
///
/// Each language has different AST node types for identifiers. This function
/// maps our `Language` enum to the relevant tree-sitter node type strings.
pub fn identifier_node_types(language: Language) -> &'static [&'static str] {
    match language {
        Language::Python => &["identifier"],
        Language::TypeScript | Language::JavaScript => &[
            "identifier",
            "property_identifier",
            "shorthand_property_identifier",
            "type_identifier",
        ],
        Language::Go => &["identifier", "field_identifier", "type_identifier"],
        Language::Rust => &["identifier", "field_identifier", "type_identifier"],
        Language::Java => &["identifier", "type_identifier"],
        Language::C | Language::Cpp => &["identifier", "field_identifier", "type_identifier"],
        Language::Ruby => &["identifier", "constant"],
        Language::Php => &["name"],
        Language::Kotlin => &["identifier"],
        Language::Swift => &["simple_identifier", "type_identifier"],
        Language::CSharp => &["identifier"],
        Language::Scala => &["identifier"],
        Language::Elixir => &["identifier"],
        Language::Lua | Language::Luau => &["identifier"],
        Language::Ocaml => &["value_name", "type_constructor"],
    }
}

/// Walk the tree-sitter AST and count all identifier occurrences.
///
/// # Arguments
/// * `tree` - Parsed tree-sitter AST
/// * `source` - Source code bytes (used to extract identifier text)
/// * `language` - Programming language (determines which node types to count)
///
/// # Returns
/// HashMap mapping identifier name to occurrence count.
pub fn count_identifiers_in_tree(
    tree: &Tree,
    source: &[u8],
    language: Language,
) -> HashMap<String, usize> {
    let id_types = identifier_node_types(language);
    let mut counts: HashMap<String, usize> = HashMap::new();

    // Use TreeCursor for efficient depth-first traversal
    let mut cursor = tree.walk();
    let mut reached_root = false;

    loop {
        let node = cursor.node();

        // Check if this node is an identifier type we care about
        if id_types.contains(&node.kind()) {
            // Extract text from source bytes
            let start = node.start_byte();
            let end = node.end_byte();
            if start <= end && end <= source.len() {
                if let Ok(text) = std::str::from_utf8(&source[start..end]) {
                    if !text.is_empty() {
                        *counts.entry(text.to_string()).or_insert(0) += 1;
                    }
                }
            }
        }

        // Depth-first traversal: try going to first child, then next sibling,
        // then walk back up to parent and try next sibling, etc.
        if cursor.goto_first_child() {
            continue;
        }
        if cursor.goto_next_sibling() {
            continue;
        }

        // Walk back up until we find a node with a next sibling or reach root
        loop {
            if !cursor.goto_parent() {
                reached_root = true;
                break;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }

        if reached_root {
            break;
        }
    }

    counts
}

/// Check if a function name is "rescued" by reference counting.
///
/// A name is rescued if:
/// - It appears more than once in the ref_counts (ref_count > 1)
/// - The name is at least 3 characters long (short names are too collision-prone)
/// - For qualified names like "MyClass.method", checks the bare name after the last "."
///
/// # Arguments
/// * `name` - Function/method name to check
/// * `ref_counts` - Map of identifier name to occurrence count
///
/// # Returns
/// `true` if the name should be rescued from dead code reports
pub fn is_rescued_by_refcount(name: &str, ref_counts: &HashMap<String, usize>) -> bool {
    // Extract the bare name (after the last "." or ":") for qualified names
    // Supports: "MyClass.method" (Python, JS, etc.) and "module:method" (Lua)
    let bare_name = if name.contains('.') {
        name.rsplit('.').next().unwrap_or(name)
    } else if name.contains(':') {
        name.rsplit(':').next().unwrap_or(name)
    } else {
        name
    };

    // Names shorter than 3 characters need a higher refcount threshold to avoid
    // false rescues from collision-prone names (i, j, x, id, etc.).
    // But very high refcounts (>= 5) indicate genuine usage even for short names.
    let min_refs = if bare_name.len() < 3 { 5 } else { 2 };

    // Check bare name first (covers both qualified and unqualified cases)
    if let Some(&count) = ref_counts.get(bare_name) {
        if count >= min_refs {
            return true;
        }
    }

    // If the full qualified name differs from the bare name, also check it
    if bare_name != name {
        if let Some(&count) = ref_counts.get(name) {
            if count >= min_refs {
                return true;
            }
        }
    }

    false
}

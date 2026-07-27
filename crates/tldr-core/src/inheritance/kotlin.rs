//! Kotlin class extraction for inheritance analysis
//!
//! Extracts class, interface, object, enum, and data class definitions from Kotlin
//! source code using tree-sitter. Handles:
//! - Class inheritance (: SuperClass)
//! - Interface implementation (: Interface1, Interface2)
//! - Abstract classes
//! - Interface declarations
//! - Object declarations (singleton with delegation)
//! - Enum classes (with interface implementation)
//! - Data classes
//! - Generic type parameters (stripped to base type)
//! - Constructor invocation in delegation specifiers

use std::path::Path;

use tree_sitter::Node;

use crate::ast::parser::ParserPool;
use crate::types::{InheritanceNode, Language};
use crate::TldrResult;

/// Extract class, interface, object, and enum definitions from Kotlin source code
pub fn extract_classes(
    source: &str,
    file_path: &Path,
    parser_pool: &ParserPool,
) -> TldrResult<Vec<InheritanceNode>> {
    let tree = parser_pool.parse(source, Language::Kotlin)?;
    let mut classes = Vec::new();

    let root = tree.root_node();
    visit_node(&root, source, file_path, &mut classes);

    Ok(classes)
}

fn visit_node(node: &Node, source: &str, file_path: &Path, classes: &mut Vec<InheritanceNode>) {
    match node.kind() {
        "class_declaration" => {
            if let Some(class) = extract_class_declaration(node, source, file_path) {
                classes.push(class);
            }
        }
        "object_declaration" => {
            if let Some(obj) = extract_object_declaration(node, source, file_path) {
                classes.push(obj);
            }
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_node(&child, source, file_path, classes);
    }
}

/// Extract a class_declaration node.
///
/// Kotlin grammar:
/// - `class` or `interface` keyword determines type
/// - `identifier` child gives the name
/// - `delegation_specifiers` sibling contains base classes/interfaces
/// - `modifiers` may contain `abstract`, `data`, `enum`, `sealed`, `open`
fn extract_class_declaration(
    node: &Node,
    source: &str,
    file_path: &Path,
) -> Option<InheritanceNode> {
    // Find identifier child for the class name
    let name = find_identifier(node, source)?;
    let line = node.start_position().row as u32 + 1;

    let is_interface = is_interface_declaration(node, source);
    let is_abstract = has_modifier(node, source, "abstract");
    let is_enum = has_modifier(node, source, "enum");

    let mut class_node =
        InheritanceNode::new(name, file_path.to_path_buf(), line, Language::Kotlin);

    if is_interface {
        class_node.interface = Some(true);
    }
    if is_abstract {
        class_node.is_abstract = Some(true);
    }

    // Extract delegation specifiers (base classes/interfaces)
    let bases = extract_delegation_specifiers(node, source);
    class_node.bases = bases;

    // Mark enum classes but still capture their bases
    if is_enum {
        // Enum classes can implement interfaces
    }

    Some(class_node)
}

/// Extract an object_declaration node (Kotlin singleton object).
///
/// Kotlin grammar:
/// - `object` keyword
/// - `identifier` child gives the name
/// - `delegation_specifiers` sibling contains interfaces
fn extract_object_declaration(
    node: &Node,
    source: &str,
    file_path: &Path,
) -> Option<InheritanceNode> {
    let name = find_identifier(node, source)?;
    let line = node.start_position().row as u32 + 1;

    let mut obj_node = InheritanceNode::new(name, file_path.to_path_buf(), line, Language::Kotlin);

    // Extract delegation specifiers
    let bases = extract_delegation_specifiers(node, source);
    obj_node.bases = bases;

    Some(obj_node)
}

/// Find the identifier child of a declaration node
fn find_identifier(node: &Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            return child
                .utf8_text(source.as_bytes())
                .ok()
                .map(|s| s.to_string());
        }
    }
    None
}

/// Check if a class_declaration is actually an interface
/// (has `interface` keyword instead of `class`)
fn is_interface_declaration(node: &Node, source: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "interface" {
            return true;
        }
        // Stop scanning after identifier since keywords come before the name
        if child.kind() == "identifier" {
            break;
        }
    }
    // Also check: if the first non-modifier token is "interface"
    let _ = source; // already used in loop
    false
}

/// Check if a declaration node has a specific modifier
fn has_modifier(node: &Node, source: &str, modifier: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" {
            return has_modifier_in_modifiers(&child, source, modifier);
        }
    }
    false
}

/// Recursively check modifiers node for a specific modifier keyword
fn has_modifier_in_modifiers(node: &Node, source: &str, modifier: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // The modifier is typically nested: modifiers > inheritance_modifier > abstract
        // or modifiers > class_modifier > enum/data/sealed
        if let Ok(text) = child.utf8_text(source.as_bytes()) {
            if text == modifier {
                return true;
            }
        }
        // Recurse into modifier wrapper nodes
        if has_modifier_in_modifiers(&child, source, modifier) {
            return true;
        }
    }
    false
}

/// Extract base classes/interfaces from delegation_specifiers
///
/// Kotlin AST structure:
/// ```text
/// delegation_specifiers
///   delegation_specifier
///     constructor_invocation       (class inheritance: Animal(name))
///       user_type
///         identifier "Animal"
///     user_type                    (interface implementation: Serializable)
///       identifier "Serializable"
/// ```
fn extract_delegation_specifiers(node: &Node, source: &str) -> Vec<String> {
    let mut bases = Vec::new();

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "delegation_specifiers" {
            let mut spec_cursor = child.walk();
            for spec in child.children(&mut spec_cursor) {
                if spec.kind() == "delegation_specifier" {
                    if let Some(name) = extract_type_from_delegation_specifier(&spec, source) {
                        bases.push(name);
                    }
                }
            }
        }
    }

    bases
}

/// Extract the type name from a single delegation_specifier node.
///
/// A delegation_specifier can contain:
/// - `constructor_invocation` with `user_type > identifier` (class inheritance)
/// - `user_type > identifier` directly (interface implementation)
fn extract_type_from_delegation_specifier(node: &Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "constructor_invocation" => {
                // Class inheritance: Animal(name)
                // -> user_type > identifier
                return extract_type_name_from_user_type_parent(&child, source);
            }
            "user_type" => {
                // Interface implementation: Serializable
                // -> identifier
                return extract_identifier_from_user_type(&child, source);
            }
            _ => {}
        }
    }
    None
}

/// Extract type name from a node that has a user_type child
fn extract_type_name_from_user_type_parent(node: &Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "user_type" {
            return extract_identifier_from_user_type(&child, source);
        }
    }
    None
}

/// Extract the identifier from a user_type node
///
/// Handles:
/// - Simple type: `user_type > identifier "Animal"`
/// - Generic type: `user_type > identifier "List" > type_arguments > ...`
///   We just extract the base name.
fn extract_identifier_from_user_type(node: &Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" || child.kind() == "type_identifier" {
            return child
                .utf8_text(source.as_bytes())
                .ok()
                .map(|s| s.to_string());
        }
    }
    None
}

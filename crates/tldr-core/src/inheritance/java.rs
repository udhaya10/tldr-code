//! Java class extraction for inheritance analysis
//!
//! Extracts class, interface, enum, and record definitions from Java source code
//! using tree-sitter. Handles:
//! - Class inheritance (extends)
//! - Interface implementation (implements)
//! - Abstract classes
//! - Interface declarations (with extends)
//! - Enum declarations (with implements)
//! - Record declarations (with implements)
//! - Generic type parameters (stripped to base type)
//! - Scoped/qualified type names (e.g., Outer.Inner)

use std::path::Path;

use tree_sitter::{Node, Tree};

use crate::ast::parser::ParserPool;
use crate::types::{InheritanceNode, Language};
use crate::TldrResult;

/// Extract class, interface, enum, and record definitions from Java source code
pub fn extract_classes(
    source: &str,
    file_path: &Path,
    parser_pool: &ParserPool,
) -> TldrResult<Vec<InheritanceNode>> {
    let tree = parser_pool.parse(source, Language::Java)?;
    let mut classes = Vec::new();

    extract_declarations(&tree, source, file_path, &mut classes);

    Ok(classes)
}

fn extract_declarations(
    tree: &Tree,
    source: &str,
    file_path: &Path,
    classes: &mut Vec<InheritanceNode>,
) {
    let root = tree.root_node();
    visit_node(&root, source, file_path, classes);
}

fn visit_node(node: &Node, source: &str, file_path: &Path, classes: &mut Vec<InheritanceNode>) {
    match node.kind() {
        "class_declaration" => {
            if let Some(class) = extract_class(node, source, file_path) {
                classes.push(class);
            }
        }
        "interface_declaration" => {
            if let Some(iface) = extract_interface(node, source, file_path) {
                classes.push(iface);
            }
        }
        "enum_declaration" => {
            if let Some(enum_node) = extract_enum(node, source, file_path) {
                classes.push(enum_node);
            }
        }
        "record_declaration" => {
            if let Some(record) = extract_record(node, source, file_path) {
                classes.push(record);
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
/// Java grammar fields:
/// - `name` -> identifier
/// - `superclass` -> superclass node containing a type
/// - `interfaces` -> super_interfaces node containing a type_list
/// - modifiers child may contain "abstract" keyword
fn extract_class(node: &Node, source: &str, file_path: &Path) -> Option<InheritanceNode> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();

    let line = node.start_position().row as u32 + 1;
    let mut class_node = InheritanceNode::new(name, file_path.to_path_buf(), line, Language::Java);

    let mut bases = Vec::new();

    // Extract superclass (extends)
    if let Some(superclass) = node.child_by_field_name("superclass") {
        extract_types_from_node(&superclass, source, &mut bases);
    }

    // Extract interfaces (implements)
    if let Some(interfaces) = node.child_by_field_name("interfaces") {
        extract_type_list_from_node(&interfaces, source, &mut bases);
    }

    class_node.bases = bases;

    // Check for abstract modifier
    if has_modifier(node, source, "abstract") {
        class_node.is_abstract = Some(true);
    }

    Some(class_node)
}

/// Extract an interface_declaration node.
///
/// Java grammar:
/// - `name` -> identifier
/// - child `extends_interfaces` containing a type_list
fn extract_interface(node: &Node, source: &str, file_path: &Path) -> Option<InheritanceNode> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();

    let line = node.start_position().row as u32 + 1;
    let mut iface_node = InheritanceNode::new(name, file_path.to_path_buf(), line, Language::Java);
    iface_node.interface = Some(true);

    let mut bases = Vec::new();

    // Interface extends are in a child node called "extends_interfaces"
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if child.kind() == "extends_interfaces" {
                extract_type_list_from_node(&child, source, &mut bases);
            }
        }
    }

    iface_node.bases = bases;

    Some(iface_node)
}

/// Extract an enum_declaration node.
///
/// Java grammar:
/// - `name` -> identifier
/// - `interfaces` -> super_interfaces containing type_list
fn extract_enum(node: &Node, source: &str, file_path: &Path) -> Option<InheritanceNode> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();

    let line = node.start_position().row as u32 + 1;
    let mut enum_node = InheritanceNode::new(name, file_path.to_path_buf(), line, Language::Java);

    let mut bases = Vec::new();

    // Enums can implement interfaces
    if let Some(interfaces) = node.child_by_field_name("interfaces") {
        extract_type_list_from_node(&interfaces, source, &mut bases);
    }

    enum_node.bases = bases;

    Some(enum_node)
}

/// Extract a record_declaration node.
///
/// Java grammar:
/// - `name` -> identifier
/// - `interfaces` -> super_interfaces containing type_list
fn extract_record(node: &Node, source: &str, file_path: &Path) -> Option<InheritanceNode> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();

    let line = node.start_position().row as u32 + 1;
    let mut record_node = InheritanceNode::new(name, file_path.to_path_buf(), line, Language::Java);

    let mut bases = Vec::new();

    // Records can implement interfaces
    if let Some(interfaces) = node.child_by_field_name("interfaces") {
        extract_type_list_from_node(&interfaces, source, &mut bases);
    }

    record_node.bases = bases;

    Some(record_node)
}

/// Extract types directly from a node (e.g., superclass node contains a single type)
fn extract_types_from_node(node: &Node, source: &str, bases: &mut Vec<String>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(name) = extract_type_name(&child, source) {
            bases.push(name);
        }
    }
}

/// Extract types from a node that contains a type_list child (e.g., super_interfaces, extends_interfaces)
fn extract_type_list_from_node(node: &Node, source: &str, bases: &mut Vec<String>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "type_list" {
            let mut inner_cursor = child.walk();
            for type_child in child.children(&mut inner_cursor) {
                if let Some(name) = extract_type_name(&type_child, source) {
                    bases.push(name);
                }
            }
        } else if let Some(name) = extract_type_name(&child, source) {
            // Direct type child (fallback)
            bases.push(name);
        }
    }
}

/// Extract a type name from a type node.
///
/// Handles:
/// - `type_identifier` -> "Animal"
/// - `generic_type` -> "List" (strips type parameters)
/// - `scoped_type_identifier` -> "Outer.Inner" (qualified name)
fn extract_type_name(node: &Node, source: &str) -> Option<String> {
    match node.kind() {
        "type_identifier" => node
            .utf8_text(source.as_bytes())
            .ok()
            .map(|s| s.to_string()),
        "generic_type" => {
            // Generic<T> -> just the base type name
            // First named child should be type_identifier or scoped_type_identifier
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    match child.kind() {
                        "type_identifier" => {
                            return child
                                .utf8_text(source.as_bytes())
                                .ok()
                                .map(|s| s.to_string());
                        }
                        "scoped_type_identifier" => {
                            return extract_scoped_type_name(&child, source);
                        }
                        _ => {}
                    }
                }
            }
            None
        }
        "scoped_type_identifier" => extract_scoped_type_name(node, source),
        _ => None,
    }
}

/// Extract a scoped type identifier like `java.util.List` or `Outer.Inner`.
/// Returns the full dotted name.
fn extract_scoped_type_name(node: &Node, source: &str) -> Option<String> {
    node.utf8_text(source.as_bytes())
        .ok()
        .map(|s| s.to_string())
}

/// Check if a declaration node has a specific modifier (e.g., "abstract", "final")
fn has_modifier(node: &Node, source: &str, modifier: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" {
            let mut mod_cursor = child.walk();
            for mod_child in child.children(&mut mod_cursor) {
                if let Ok(text) = mod_child.utf8_text(source.as_bytes()) {
                    if text == modifier {
                        return true;
                    }
                }
            }
        }
    }
    false
}

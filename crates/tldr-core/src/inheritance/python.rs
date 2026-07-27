//! Python class extraction
//!
//! Extracts class definitions from Python source code using tree-sitter.
//! Handles:
//! - Regular class inheritance
//! - ABC abstract classes
//! - Protocols (typing.Protocol)
//! - Metaclasses (A12 mitigation)
//! - @abstractmethod decorators

use std::path::Path;

use tree_sitter::{Node, Tree};

use crate::ast::parser::ParserPool;
use crate::types::{InheritanceNode, Language};
use crate::TldrResult;

/// Extract class definitions from Python source code
pub fn extract_classes(
    source: &str,
    file_path: &Path,
    parser_pool: &ParserPool,
) -> TldrResult<Vec<InheritanceNode>> {
    let tree = parser_pool.parse(source, Language::Python)?;
    let mut classes = Vec::new();

    extract_classes_from_tree(&tree, source, file_path, &mut classes);

    Ok(classes)
}

fn extract_classes_from_tree(
    tree: &Tree,
    source: &str,
    file_path: &Path,
    classes: &mut Vec<InheritanceNode>,
) {
    let root = tree.root_node();
    let mut cursor = root.walk();

    // Walk all children looking for class definitions
    for child in root.children(&mut cursor) {
        if child.kind() == "class_definition" {
            if let Some(class) = extract_class_def(&child, source, file_path) {
                classes.push(class);
            }
        }
        // Recurse into decorated definitions
        if child.kind() == "decorated_definition" {
            for inner in child.children(&mut child.walk()) {
                if inner.kind() == "class_definition" {
                    if let Some(class) = extract_class_def(&inner, source, file_path) {
                        classes.push(class);
                    }
                }
            }
        }
    }
}

fn extract_class_def(node: &Node, source: &str, file_path: &Path) -> Option<InheritanceNode> {
    // Get class name
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();

    let line = node.start_position().row as u32 + 1;

    let mut class_node = InheritanceNode::new(
        name.clone(),
        file_path.to_path_buf(),
        line,
        Language::Python,
    );

    // Extract bases from argument_list (superclasses)
    if let Some(args) = node.child_by_field_name("superclasses") {
        let mut bases = Vec::new();
        let mut metaclass = None;

        for i in 0..args.child_count() {
            if let Some(child) = args.child(i) {
                match child.kind() {
                    // Simple identifier base class
                    "identifier" => {
                        if let Ok(base_name) = child.utf8_text(source.as_bytes()) {
                            bases.push(base_name.to_string());
                        }
                    }
                    // Attribute access like typing.Protocol
                    "attribute" => {
                        if let Some(base_name) = extract_attribute_name(&child, source) {
                            bases.push(base_name);
                        }
                    }
                    // Generic like Generic[T]
                    "subscript" => {
                        if let Some(base_name) = extract_subscript_name(&child, source) {
                            bases.push(base_name);
                        }
                    }
                    // Keyword argument like metaclass=ABCMeta (A12)
                    "keyword_argument" => {
                        if let Some((key, value)) = extract_keyword_arg(&child, source) {
                            if key == "metaclass" {
                                metaclass = Some(value);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        class_node.bases = bases;
        class_node.metaclass = metaclass;
    }

    // Check for @abstractmethod decorator
    if has_abstractmethod_decorator(node, source) || class_node.bases.contains(&"ABC".to_string()) {
        class_node.is_abstract = Some(true);
    }

    // Check for Protocol base
    if class_node
        .bases
        .iter()
        .any(|b| b == "Protocol" || b.ends_with(".Protocol"))
    {
        class_node.protocol = Some(true);
    }

    Some(class_node)
}

/// Extract attribute name like `typing.Protocol` -> "Protocol"
fn extract_attribute_name(node: &Node, source: &str) -> Option<String> {
    // Get the attribute (rightmost part)
    let attr = node.child_by_field_name("attribute")?;
    attr.utf8_text(source.as_bytes())
        .ok()
        .map(|s| s.to_string())
}

/// Extract subscript name like `Generic[T]` -> "Generic"
fn extract_subscript_name(node: &Node, source: &str) -> Option<String> {
    let value = node.child_by_field_name("value")?;
    match value.kind() {
        "identifier" => value
            .utf8_text(source.as_bytes())
            .ok()
            .map(|s| s.to_string()),
        "attribute" => extract_attribute_name(&value, source),
        _ => None,
    }
}

/// Extract keyword argument like `metaclass=ABCMeta` -> ("metaclass", "ABCMeta")
fn extract_keyword_arg(node: &Node, source: &str) -> Option<(String, String)> {
    let name = node.child_by_field_name("name")?;
    let value = node.child_by_field_name("value")?;

    let key = name.utf8_text(source.as_bytes()).ok()?;
    let val = match value.kind() {
        "identifier" => value.utf8_text(source.as_bytes()).ok()?.to_string(),
        "attribute" => extract_attribute_name(&value, source)?,
        _ => return None,
    };

    Some((key.to_string(), val))
}

/// Check if class has @abstractmethod decorated methods
fn has_abstractmethod_decorator(class_node: &Node, source: &str) -> bool {
    // Look in the class body for decorated methods
    if let Some(body) = class_node.child_by_field_name("body") {
        for i in 0..body.child_count() {
            if let Some(child) = body.child(i) {
                if child.kind() == "decorated_definition" {
                    // Check decorators
                    for j in 0..child.child_count() {
                        if let Some(decorator) = child.child(j) {
                            if decorator.kind() == "decorator" {
                                if let Ok(text) = decorator.utf8_text(source.as_bytes()) {
                                    if text.contains("abstractmethod") {
                                        return true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

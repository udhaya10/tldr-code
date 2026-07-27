//! Java language handler for call graph analysis.
//!
//! This module provides Java-specific call graph support using tree-sitter-java.
//!
//! # Import Patterns Supported
//!
//! | Pattern | ImportDef |
//! |---------|-----------|
//! | `import java.util.List;` | `{module: "java.util.List"}` |
//! | `import java.util.*;` | `{module: "java.util.*", names: ["*"]}` |
//! | `import static java.lang.Math.PI;` | `{module: "java.lang.Math.PI", is_static: true}` |
//! | `import static java.util.Arrays.*;` | `{module: "java.util.Arrays.*", is_static: true}` |
//!
//! # Call Extraction
//!
//! - Direct calls: `method()` -> CallType::Direct or CallType::Intra
//! - Method calls: `obj.method()` -> CallType::Attr
//! - Static calls: `Class.method()` -> CallType::Attr
//! - Constructor calls: `new Class()` -> CallType::Direct
//! - Chained calls: `obj.method1().method2()` -> multiple CallType::Attr
//!
//! # Spec Reference
//!
//! See `migration/spec/callgraph-spec.md` Section 9.5 for Java-specific details.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use tree_sitter::{Node, Parser, Tree};

use super::base::{get_node_text, walk_tree};
use super::common::extend_calls_if_any;
use super::{CallGraphLanguageSupport, ParseError};
use crate::callgraph::cross_file_types::{CallSite, CallType, ClassDef, FuncDef, ImportDef};

// =============================================================================
// Java Handler
// =============================================================================

/// Java language handler using tree-sitter-java.
///
/// Supports:
/// - Import parsing (standard, wildcard, static imports)
/// - Call extraction (direct, method, constructor, static)
/// - Class and interface method tracking
/// - Nested class support
#[derive(Debug, Default)]
pub struct JavaHandler;

impl JavaHandler {
    /// Creates a new JavaHandler.
    pub fn new() -> Self {
        Self
    }

    /// Parse the source code into a tree-sitter Tree.
    fn parse_source(&self, source: &str) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_java::LANGUAGE.into())
            .map_err(|e| ParseError::ParseFailed {
                file: std::path::PathBuf::new(),
                message: format!("Failed to set Java language: {}", e),
            })?;

        parser
            .parse(source, None)
            .ok_or_else(|| ParseError::ParseFailed {
                file: std::path::PathBuf::new(),
                message: "Parser returned None".to_string(),
            })
    }

    /// Parse an import declaration node.
    fn parse_import_node(&self, node: &Node, source: &[u8]) -> Option<ImportDef> {
        if node.kind() != "import_declaration" {
            return None;
        }

        let text = get_node_text(node, source);
        let is_static = text.contains("static ");
        let is_wildcard = text.trim_end_matches(';').ends_with('*');

        // Find the scoped_identifier or identifier for the import path
        let mut module: Option<String> = None;

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                match child.kind() {
                    "scoped_identifier" => {
                        module = Some(get_node_text(&child, source).to_string());
                    }
                    "identifier" => {
                        if module.is_none() {
                            module = Some(get_node_text(&child, source).to_string());
                        }
                    }
                    "asterisk" => {
                        // Wildcard import
                        if let Some(ref mut m) = module {
                            if !m.ends_with(".*") {
                                m.push_str(".*");
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        let module = module?;

        let mut import_def = if is_wildcard {
            let mut imp = ImportDef::from_import(module, vec!["*".to_string()]);
            imp.is_namespace = true;
            imp
        } else {
            ImportDef::simple_import(module)
        };

        if is_static {
            // Mark as static import using a custom field
            // We'll use the 'is_type_checking' field to indicate static (a bit of a hack)
            // Or we can extend ImportDef later
            import_def.is_type_checking = true; // Using as is_static marker
        }

        Some(import_def)
    }

    /// Collect all class, interface, and method definitions.
    fn collect_definitions(
        &self,
        tree: &Tree,
        source: &[u8],
    ) -> (HashSet<String>, HashSet<String>) {
        let mut methods = HashSet::new();
        let mut classes = HashSet::new();

        for node in walk_tree(tree.root_node()) {
            match node.kind() {
                "method_declaration" => {
                    // Get method name
                    if let Some(name) = self.get_identifier_from_node(&node, source) {
                        methods.insert(name);
                    }
                }
                "constructor_declaration" => {
                    // Constructor name matches class name
                    if let Some(name) = self.get_identifier_from_node(&node, source) {
                        methods.insert(name.clone());
                        classes.insert(name);
                    }
                }
                "class_declaration" | "interface_declaration" | "enum_declaration" => {
                    if let Some(name) = self.get_identifier_from_node(&node, source) {
                        classes.insert(name);
                    }
                }
                _ => {}
            }
        }

        (methods, classes)
    }

    /// Get the identifier (name) from a declaration node.
    fn get_identifier_from_node(&self, node: &Node, source: &[u8]) -> Option<String> {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "identifier" {
                    return Some(get_node_text(&child, source).to_string());
                }
            }
        }
        None
    }

    /// Extract calls from a method/constructor body.
    fn extract_calls_from_node(
        &self,
        node: &Node,
        source: &[u8],
        defined_methods: &HashSet<String>,
        defined_classes: &HashSet<String>,
        caller: &str,
    ) -> Vec<CallSite> {
        let mut calls = Vec::new();

        for child in walk_tree(*node) {
            match child.kind() {
                "method_invocation" => {
                    let line = child.start_position().row as u32 + 1;

                    // Parse method invocation: obj.method() or method()
                    let mut object_name: Option<String> = None;
                    let mut method_name: Option<String> = None;
                    let mut saw_dot = false;
                    let mut first_identifier: Option<String> = None;

                    for i in 0..child.child_count() {
                        if let Some(c) = child.child(i) {
                            match c.kind() {
                                "identifier" => {
                                    let text = get_node_text(&c, source).to_string();
                                    if first_identifier.is_none() {
                                        first_identifier = Some(text);
                                    } else if saw_dot {
                                        // This is the method name after a dot
                                        object_name = first_identifier.take();
                                        method_name = Some(text);
                                    } else {
                                        method_name = Some(text);
                                    }
                                }
                                "." => {
                                    saw_dot = true;
                                }
                                "this" => {
                                    object_name = Some("this".to_string());
                                }
                                "super" => {
                                    object_name = Some("super".to_string());
                                }
                                "field_access" => {
                                    // obj.field.method() - get the full receiver
                                    object_name = Some(get_node_text(&c, source).to_string());
                                }
                                "argument_list" => {
                                    // Skip argument list
                                }
                                _ => {}
                            }
                        }
                    }

                    // If no method_name found, first_identifier is the method
                    if method_name.is_none() {
                        method_name = first_identifier;
                    }

                    if let Some(method) = method_name {
                        if let Some(obj) = object_name {
                            // Method call on object
                            let target = format!("{}.{}", obj, method);
                            calls.push(CallSite::new(
                                caller.to_string(),
                                target,
                                CallType::Attr,
                                Some(line),
                                None,
                                Some(obj),
                                None,
                            ));
                        } else {
                            // Direct method call
                            let call_type = if defined_methods.contains(&method) {
                                CallType::Intra
                            } else {
                                CallType::Direct
                            };
                            calls.push(CallSite::new(
                                caller.to_string(),
                                method,
                                call_type,
                                Some(line),
                                None,
                                None,
                                None,
                            ));
                        }
                    }
                }
                "object_creation_expression" => {
                    // new ClassName()
                    let line = child.start_position().row as u32 + 1;

                    for i in 0..child.child_count() {
                        if let Some(c) = child.child(i) {
                            if c.kind() == "type_identifier" {
                                let class_name = get_node_text(&c, source).to_string();
                                let call_type = if defined_classes.contains(&class_name) {
                                    CallType::Intra
                                } else {
                                    CallType::Direct
                                };
                                calls.push(CallSite::new(
                                    caller.to_string(),
                                    class_name,
                                    call_type,
                                    Some(line),
                                    None,
                                    None,
                                    None,
                                ));
                                break;
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        calls
    }

    fn recurse_children_for_call_extraction(
        &self,
        node: Node,
        source: &[u8],
        defined_methods: &HashSet<String>,
        defined_classes: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        current_class: &mut Option<String>,
    ) {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                self.process_extract_calls_node(
                    child,
                    source,
                    defined_methods,
                    defined_classes,
                    calls_by_func,
                    current_class,
                );
            }
        }
    }

    fn process_extract_calls_node(
        &self,
        node: Node,
        source: &[u8],
        defined_methods: &HashSet<String>,
        defined_classes: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        current_class: &mut Option<String>,
    ) {
        match node.kind() {
            "class_declaration" | "interface_declaration" | "enum_declaration" => {
                self.handle_type_declaration_calls(
                    node,
                    source,
                    defined_methods,
                    defined_classes,
                    calls_by_func,
                    current_class,
                );
            }
            "method_declaration" | "constructor_declaration" => {
                self.handle_method_or_constructor_calls(
                    node,
                    source,
                    defined_methods,
                    defined_classes,
                    calls_by_func,
                    current_class,
                );
            }
            "field_declaration" => {
                self.handle_field_declaration_calls(
                    node,
                    source,
                    defined_methods,
                    defined_classes,
                    calls_by_func,
                    current_class,
                );
            }
            "static_initializer" => {
                self.handle_static_initializer_calls(
                    node,
                    source,
                    defined_methods,
                    defined_classes,
                    calls_by_func,
                    current_class,
                );
            }
            "block" => {
                self.handle_block_calls(
                    node,
                    source,
                    defined_methods,
                    defined_classes,
                    calls_by_func,
                    current_class,
                );
            }
            "enum_constant" => {
                self.handle_enum_constant_calls(
                    node,
                    source,
                    defined_methods,
                    defined_classes,
                    calls_by_func,
                    current_class,
                );
            }
            _ => {
                self.recurse_children_for_call_extraction(
                    node,
                    source,
                    defined_methods,
                    defined_classes,
                    calls_by_func,
                    current_class,
                );
            }
        }
    }

    fn handle_type_declaration_calls(
        &self,
        node: Node,
        source: &[u8],
        defined_methods: &HashSet<String>,
        defined_classes: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        current_class: &mut Option<String>,
    ) {
        let mut class_name: Option<String> = None;
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "identifier" {
                    class_name = Some(get_node_text(&child, source).to_string());
                    break;
                }
            }
        }

        let old_class = current_class.take();
        *current_class = class_name;
        self.recurse_children_for_call_extraction(
            node,
            source,
            defined_methods,
            defined_classes,
            calls_by_func,
            current_class,
        );
        *current_class = old_class;
    }

    fn handle_method_or_constructor_calls(
        &self,
        node: Node,
        source: &[u8],
        defined_methods: &HashSet<String>,
        defined_classes: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        current_class: &Option<String>,
    ) {
        let mut method_name: Option<String> = None;
        let mut body: Option<Node> = None;

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                match child.kind() {
                    "identifier" => {
                        if method_name.is_none() {
                            method_name = Some(get_node_text(&child, source).to_string());
                        }
                    }
                    "block" | "constructor_body" => body = Some(child),
                    _ => {}
                }
            }
        }

        let (Some(name), Some(body_node)) = (method_name, body) else {
            return;
        };
        let caller = if let Some(class) = current_class {
            format!("{class}.{name}")
        } else {
            name
        };
        let calls = self.extract_calls_from_node(
            &body_node,
            source,
            defined_methods,
            defined_classes,
            &caller,
        );
        if !calls.is_empty() {
            calls_by_func.insert(caller, calls);
        }
    }

    fn handle_field_declaration_calls(
        &self,
        node: Node,
        source: &[u8],
        defined_methods: &HashSet<String>,
        defined_classes: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        current_class: &Option<String>,
    ) {
        let Some(class) = current_class.as_deref() else {
            return;
        };
        let is_static = (0..node.child_count()).any(|i| {
            node.child(i).is_some_and(|child| {
                child.kind() == "modifiers" && get_node_text(&child, source).contains("static")
            })
        });
        let caller = if is_static {
            format!("{class}.<clinit>")
        } else {
            format!("{class}.<init>")
        };
        let calls =
            self.extract_calls_from_node(&node, source, defined_methods, defined_classes, &caller);
        extend_calls_if_any(calls_by_func, caller, calls);
    }

    fn handle_static_initializer_calls(
        &self,
        node: Node,
        source: &[u8],
        defined_methods: &HashSet<String>,
        defined_classes: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        current_class: &Option<String>,
    ) {
        let Some(class) = current_class.as_deref() else {
            return;
        };
        let caller = format!("{class}.<clinit>");
        let calls =
            self.extract_calls_from_node(&node, source, defined_methods, defined_classes, &caller);
        extend_calls_if_any(calls_by_func, caller, calls);
    }

    fn handle_block_calls(
        &self,
        node: Node,
        source: &[u8],
        defined_methods: &HashSet<String>,
        defined_classes: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        current_class: &mut Option<String>,
    ) {
        let is_instance_init = node
            .parent()
            .is_some_and(|parent| parent.kind() == "class_body");
        if !is_instance_init {
            self.recurse_children_for_call_extraction(
                node,
                source,
                defined_methods,
                defined_classes,
                calls_by_func,
                current_class,
            );
            return;
        }

        let Some(class) = current_class.as_deref() else {
            return;
        };
        let caller = format!("{class}.<init>");
        let calls =
            self.extract_calls_from_node(&node, source, defined_methods, defined_classes, &caller);
        extend_calls_if_any(calls_by_func, caller, calls);
    }

    fn handle_enum_constant_calls(
        &self,
        node: Node,
        source: &[u8],
        defined_methods: &HashSet<String>,
        defined_classes: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        current_class: &Option<String>,
    ) {
        let Some(class) = current_class.as_deref() else {
            return;
        };
        let caller = format!("{class}.<clinit>");
        let calls =
            self.extract_calls_from_node(&node, source, defined_methods, defined_classes, &caller);
        extend_calls_if_any(calls_by_func, caller, calls);
    }
}

impl CallGraphLanguageSupport for JavaHandler {
    fn name(&self) -> &str {
        "java"
    }

    fn extensions(&self) -> &[&str] {
        &[".java"]
    }

    fn parse_imports(&self, source: &str, _path: &Path) -> Result<Vec<ImportDef>, ParseError> {
        let tree = self.parse_source(source)?;
        let source_bytes = source.as_bytes();
        let mut imports = Vec::new();

        for node in walk_tree(tree.root_node()) {
            if node.kind() == "import_declaration" {
                if let Some(imp) = self.parse_import_node(&node, source_bytes) {
                    imports.push(imp);
                }
            }
        }

        Ok(imports)
    }

    fn extract_calls(
        &self,
        _path: &Path,
        source: &str,
        tree: &Tree,
    ) -> Result<HashMap<String, Vec<CallSite>>, ParseError> {
        let source_bytes = source.as_bytes();
        let (defined_methods, defined_classes) = self.collect_definitions(tree, source_bytes);
        let mut calls_by_func: HashMap<String, Vec<CallSite>> = HashMap::new();
        let mut current_class: Option<String> = None;
        self.process_extract_calls_node(
            tree.root_node(),
            source_bytes,
            &defined_methods,
            &defined_classes,
            &mut calls_by_func,
            &mut current_class,
        );

        Ok(calls_by_func)
    }

    fn extract_definitions(
        &self,
        source: &str,
        _path: &Path,
        tree: &Tree,
    ) -> Result<(Vec<FuncDef>, Vec<ClassDef>), super::ParseError> {
        let source_bytes = source.as_bytes();
        let mut funcs = Vec::new();
        let mut classes = Vec::new();

        for node in walk_tree(tree.root_node()) {
            match node.kind() {
                "method_declaration" | "constructor_declaration" => {
                    if let Some(name) = self.get_identifier_from_node(&node, source_bytes) {
                        let line = node.start_position().row as u32 + 1;
                        let end_line = node.end_position().row as u32 + 1;

                        // Check if inside a class
                        let mut class_name = None;
                        let mut parent = node.parent();
                        while let Some(p) = parent {
                            if p.kind() == "class_body" {
                                if let Some(gp) = p.parent() {
                                    if gp.kind() == "class_declaration"
                                        || gp.kind() == "interface_declaration"
                                    {
                                        class_name =
                                            self.get_identifier_from_node(&gp, source_bytes);
                                    }
                                }
                                break;
                            }
                            parent = p.parent();
                        }

                        if let Some(cn) = class_name {
                            funcs.push(FuncDef::method(name, cn, line, end_line));
                        } else {
                            funcs.push(FuncDef::function(name, line, end_line));
                        }
                    }
                }
                "class_declaration" | "interface_declaration" | "enum_declaration" => {
                    if let Some(name) = self.get_identifier_from_node(&node, source_bytes) {
                        let line = node.start_position().row as u32 + 1;
                        let end_line = node.end_position().row as u32 + 1;

                        // Collect method names and base classes
                        let mut methods = Vec::new();
                        let mut bases = Vec::new();

                        for i in 0..node.child_count() {
                            if let Some(child) = node.child(i) {
                                if child.kind() == "superclass"
                                    || child.kind() == "super_interfaces"
                                {
                                    for j in 0..child.child_count() {
                                        if let Some(tc) = child.child(j) {
                                            if tc.kind() == "type_identifier" {
                                                bases.push(
                                                    get_node_text(&tc, source_bytes).to_string(),
                                                );
                                            }
                                            if tc.kind() == "type_list" {
                                                for k in 0..tc.child_count() {
                                                    if let Some(t) = tc.child(k) {
                                                        if t.kind() == "type_identifier" {
                                                            bases.push(
                                                                get_node_text(&t, source_bytes)
                                                                    .to_string(),
                                                            );
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                if child.kind() == "class_body" {
                                    for j in 0..child.named_child_count() {
                                        if let Some(member) = child.named_child(j) {
                                            if member.kind() == "method_declaration"
                                                || member.kind() == "constructor_declaration"
                                            {
                                                if let Some(mn) = self
                                                    .get_identifier_from_node(&member, source_bytes)
                                                {
                                                    methods.push(mn);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        classes.push(ClassDef::new(name, line, end_line, methods, bases));
                    }
                }
                _ => {}
            }
        }

        Ok((funcs, classes))
    }
}

// =============================================================================
// Tests
// =============================================================================

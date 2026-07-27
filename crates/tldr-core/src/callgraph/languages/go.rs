//! Go language handler for call graph analysis.
//!
//! This module provides Go-specific call graph support using tree-sitter-go.
//!
//! # Import Patterns Supported
//!
//! | Pattern | ImportDef |
//! |---------|-----------|
//! | `import "pkg"` | `{module: "pkg"}` |
//! | `import alias "pkg"` | `{module: "pkg", alias: "alias"}` |
//! | `import . "pkg"` | `{module: "pkg", alias: "."}` (dot import) |
//! | `import _ "pkg"` | `{module: "pkg", alias: "_"}` (blank import) |
//! | `import ( "a"; "b" )` | Multiple ImportDefs |
//!
//! # Call Extraction
//!
//! - Package-qualified calls: `pkg.Func()` -> CallType::Attr
//! - Method calls: `receiver.Method()` -> CallType::Attr
//! - Local calls: `Func()` -> CallType::Direct or CallType::Intra
//!
//! # Spec Reference
//!
//! See `migration/spec/callgraph-spec.md` Section 9.2 for Go-specific details.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use tree_sitter::{Node, Parser, Tree};

use super::base::{get_node_text, walk_tree};
use super::{CallGraphLanguageSupport, ParseError};
use crate::callgraph::cross_file_types::{CallSite, CallType, ClassDef, FuncDef, ImportDef};

// =============================================================================
// Go Handler
// =============================================================================

/// Go language handler using tree-sitter-go.
///
/// Supports:
/// - Import parsing (single and grouped imports, with aliases)
/// - Call extraction (package-qualified, method, local)
/// - Method declarations with receivers
#[derive(Debug, Default)]
pub struct GoHandler;

impl GoHandler {
    /// Creates a new GoHandler.
    pub fn new() -> Self {
        Self
    }

    /// Parse the source code into a tree-sitter Tree.
    fn parse_source(&self, source: &str) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .map_err(|e| ParseError::ParseFailed {
                file: std::path::PathBuf::new(),
                message: format!("Failed to set Go language: {}", e),
            })?;

        parser
            .parse(source, None)
            .ok_or_else(|| ParseError::ParseFailed {
                file: std::path::PathBuf::new(),
                message: "Parser returned None".to_string(),
            })
    }

    /// Parse a single import spec (potentially with alias).
    fn parse_import_spec(&self, node: &Node, source: &[u8]) -> Option<ImportDef> {
        let mut alias = None;
        let mut module = None;

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                match child.kind() {
                    "package_identifier" | "dot" | "blank_identifier" => {
                        // This is the alias (including . and _)
                        let text = get_node_text(&child, source);
                        alias = Some(text.to_string());
                    }
                    "interpreted_string_literal" => {
                        // This is the module path - strip quotes
                        let text = get_node_text(&child, source);
                        module = Some(text.trim_matches('"').to_string());
                    }
                    _ => {}
                }
            }
        }

        module.map(|m| {
            let mut imp = ImportDef::simple_import(m);
            imp.alias = alias;
            imp
        })
    }

    /// Collect all function names defined in the file.
    fn collect_definitions(&self, tree: &Tree, source: &[u8]) -> HashSet<String> {
        let mut functions = HashSet::new();

        for node in walk_tree(tree.root_node()) {
            match node.kind() {
                "function_declaration" => {
                    // Regular function: func Name() {}
                    if let Some(name_node) = node.child_by_field_name("name") {
                        functions.insert(get_node_text(&name_node, source).to_string());
                    }
                }
                "method_declaration" => {
                    // Method: func (r *Receiver) Name() {}
                    if let Some(name_node) = node.child_by_field_name("name") {
                        let method_name = get_node_text(&name_node, source).to_string();
                        functions.insert(method_name.clone());

                        // Also add with receiver type
                        if let Some(receiver_type) = self.get_receiver_type(&node, source) {
                            functions.insert(format!("{}.{}", receiver_type, method_name));
                        }
                    }
                }
                "type_declaration" => {
                    // Type definitions (struct, interface)
                    for i in 0..node.named_child_count() {
                        if let Some(spec) = node.named_child(i) {
                            if spec.kind() == "type_spec" {
                                if let Some(name_node) = spec.child_by_field_name("name") {
                                    functions.insert(get_node_text(&name_node, source).to_string());
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        functions
    }

    /// Get the receiver type from a method declaration.
    fn get_receiver_type(&self, node: &Node, source: &[u8]) -> Option<String> {
        // Find the parameter_list that is the receiver
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "parameter_list" {
                    // First parameter list is the receiver
                    for j in 0..child.named_child_count() {
                        if let Some(param) = child.named_child(j) {
                            if param.kind() == "parameter_declaration" {
                                // Look for the type
                                for k in 0..param.named_child_count() {
                                    if let Some(type_node) = param.named_child(k) {
                                        match type_node.kind() {
                                            "pointer_type" => {
                                                // *Type - find the type identifier
                                                for l in 0..type_node.named_child_count() {
                                                    if let Some(inner) = type_node.named_child(l) {
                                                        if inner.kind() == "type_identifier" {
                                                            return Some(
                                                                get_node_text(&inner, source)
                                                                    .to_string(),
                                                            );
                                                        }
                                                    }
                                                }
                                            }
                                            "type_identifier" => {
                                                return Some(
                                                    get_node_text(&type_node, source).to_string(),
                                                );
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }
                    }
                    break; // Only check first parameter_list (receiver)
                }
            }
        }
        None
    }

    /// Extract calls from a function body.
    fn extract_calls_from_func(
        &self,
        node: &Node,
        source: &[u8],
        defined_funcs: &HashSet<String>,
        caller: &str,
    ) -> Vec<CallSite> {
        let mut calls = Vec::new();

        for child in walk_tree(*node) {
            if child.kind() == "call_expression" {
                let line = child.start_position().row as u32 + 1;

                // Get the function being called (first child)
                if let Some(func_node) = child.child(0) {
                    match func_node.kind() {
                        "identifier" => {
                            // Direct call: Func()
                            let target = get_node_text(&func_node, source).to_string();
                            let call_type = if defined_funcs.contains(&target) {
                                CallType::Intra
                            } else {
                                CallType::Direct
                            };
                            calls.push(CallSite::new(
                                caller.to_string(),
                                target,
                                call_type,
                                Some(line),
                                None,
                                None,
                                None,
                            ));
                        }
                        "selector_expression" => {
                            // Package or method call: pkg.Func() or obj.Method()
                            // Also handles chained calls: s.repo.Save() where
                            // the outer selector wraps an inner selector.
                            let mut receiver = None;
                            let mut method = None;

                            for i in 0..func_node.child_count() {
                                if let Some(sc) = func_node.child(i) {
                                    match sc.kind() {
                                        "identifier" => {
                                            if receiver.is_none() {
                                                receiver =
                                                    Some(get_node_text(&sc, source).to_string());
                                            }
                                        }
                                        "selector_expression" => {
                                            // Nested selector: e.g., s.repo in s.repo.Save()
                                            // Extract the field_identifier from the inner selector
                                            // as the receiver for the outer call
                                            for j in 0..sc.child_count() {
                                                if let Some(inner) = sc.child(j) {
                                                    if inner.kind() == "field_identifier" {
                                                        receiver = Some(
                                                            get_node_text(&inner, source)
                                                                .to_string(),
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                        "field_identifier" => {
                                            method = Some(get_node_text(&sc, source).to_string());
                                        }
                                        _ => {}
                                    }
                                }
                            }

                            if let (Some(recv), Some(meth)) = (receiver, method) {
                                let target = format!("{}.{}", recv, meth);
                                // Check if method is defined locally
                                let call_type = if defined_funcs.contains(&meth) {
                                    CallType::Intra
                                } else {
                                    CallType::Attr
                                };
                                calls.push(CallSite::new(
                                    caller.to_string(),
                                    target.clone(),
                                    call_type,
                                    Some(line),
                                    None,
                                    Some(recv),
                                    None,
                                ));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        calls
    }
}

impl CallGraphLanguageSupport for GoHandler {
    fn name(&self) -> &str {
        "go"
    }

    fn extensions(&self) -> &[&str] {
        &[".go"]
    }

    fn parse_imports(&self, source: &str, _path: &Path) -> Result<Vec<ImportDef>, ParseError> {
        let tree = self.parse_source(source)?;
        let source_bytes = source.as_bytes();
        let mut imports = Vec::new();

        for node in walk_tree(tree.root_node()) {
            if node.kind() == "import_declaration" {
                // Process import declaration
                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i) {
                        match child.kind() {
                            "import_spec" => {
                                // Single import
                                if let Some(imp) = self.parse_import_spec(&child, source_bytes) {
                                    imports.push(imp);
                                }
                            }
                            "import_spec_list" => {
                                // Grouped imports: import ( "a"; "b" )
                                for j in 0..child.named_child_count() {
                                    if let Some(spec) = child.named_child(j) {
                                        if spec.kind() == "import_spec" {
                                            if let Some(imp) =
                                                self.parse_import_spec(&spec, source_bytes)
                                            {
                                                imports.push(imp);
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
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
        let defined_funcs = self.collect_definitions(tree, source_bytes);
        let mut calls_by_func: HashMap<String, Vec<CallSite>> = HashMap::new();

        for node in walk_tree(tree.root_node()) {
            match node.kind() {
                "function_declaration" => {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        let func_name = get_node_text(&name_node, source_bytes).to_string();

                        if let Some(body) = node.child_by_field_name("body") {
                            let calls = self.extract_calls_from_func(
                                &body,
                                source_bytes,
                                &defined_funcs,
                                &func_name,
                            );
                            if !calls.is_empty() {
                                calls_by_func.insert(func_name, calls);
                            }
                        }
                    }
                }
                "method_declaration" => {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        let method_name = get_node_text(&name_node, source_bytes).to_string();

                        // Build full method name with receiver type
                        let full_name = if let Some(receiver_type) =
                            self.get_receiver_type(&node, source_bytes)
                        {
                            format!("{}.{}", receiver_type, method_name)
                        } else {
                            method_name
                        };

                        if let Some(body) = node.child_by_field_name("body") {
                            let calls = self.extract_calls_from_func(
                                &body,
                                source_bytes,
                                &defined_funcs,
                                &full_name,
                            );
                            if !calls.is_empty() {
                                calls_by_func.insert(full_name, calls);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Extract calls from package-level variable and constant initializers
        // into a synthetic <module> caller (consistent with Python/Luau/PHP).
        let mut module_calls = Vec::new();
        for i in 0..tree.root_node().child_count() {
            if let Some(child) = tree.root_node().child(i) {
                match child.kind() {
                    "var_declaration" | "const_declaration" => {
                        // Iterate over var_spec / const_spec children
                        // Both single and grouped (var (...)) declarations share
                        // the same structure: the var_declaration contains
                        // var_spec children directly or via a named child list.
                        for spec_node in walk_tree(child) {
                            if spec_node.kind() == "var_spec" || spec_node.kind() == "const_spec" {
                                // Extract calls from the value expression(s)
                                // The value is in the expression_list or the
                                // last named children of the spec.
                                let calls = self.extract_calls_from_func(
                                    &spec_node,
                                    source_bytes,
                                    &defined_funcs,
                                    "<module>",
                                );
                                module_calls.extend(calls);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        if !module_calls.is_empty() {
            calls_by_func.insert("<module>".to_string(), module_calls);
        }

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
                "function_declaration" => {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        let name = get_node_text(&name_node, source_bytes).to_string();
                        let line = node.start_position().row as u32 + 1;
                        let end_line = node.end_position().row as u32 + 1;
                        funcs.push(FuncDef::function(name, line, end_line));
                    }
                }
                "method_declaration" => {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        let method_name = get_node_text(&name_node, source_bytes).to_string();
                        let line = node.start_position().row as u32 + 1;
                        let end_line = node.end_position().row as u32 + 1;

                        if let Some(receiver_type) = self.get_receiver_type(&node, source_bytes) {
                            funcs.push(FuncDef::method(method_name, receiver_type, line, end_line));
                        } else {
                            funcs.push(FuncDef::function(method_name, line, end_line));
                        }
                    }
                }
                "type_declaration" => {
                    for i in 0..node.named_child_count() {
                        if let Some(spec) = node.named_child(i) {
                            if spec.kind() == "type_spec" {
                                if let Some(name_node) = spec.child_by_field_name("name") {
                                    let name = get_node_text(&name_node, source_bytes).to_string();
                                    let line = spec.start_position().row as u32 + 1;
                                    let end_line = spec.end_position().row as u32 + 1;
                                    // Check if it is a struct type or interface type
                                    let mut is_type_def = false;
                                    let mut bases = Vec::new();
                                    let mut methods = Vec::new();
                                    for j in 0..spec.child_count() {
                                        if let Some(child) = spec.child(j) {
                                            if child.kind() == "struct_type" {
                                                is_type_def = true;
                                                // Extract embedded fields as bases
                                                // Go embedding: field_declaration with type but no name
                                                for fl in 0..child.named_child_count() {
                                                    if let Some(field_list) = child.named_child(fl)
                                                    {
                                                        if field_list.kind()
                                                            == "field_declaration_list"
                                                        {
                                                            for k in
                                                                0..field_list.named_child_count()
                                                            {
                                                                if let Some(field) =
                                                                    field_list.named_child(k)
                                                                {
                                                                    if field.kind()
                                                                        == "field_declaration"
                                                                    {
                                                                        // Embedded if no name field
                                                                        let has_name = field
                                                                            .child_by_field_name(
                                                                                "name",
                                                                            )
                                                                            .is_some();
                                                                        if !has_name {
                                                                            // Get the type (may be pointer-wrapped)
                                                                            if let Some(type_node) = field.child_by_field_name("type") {
                                                                                let type_text = get_node_text(&type_node, source_bytes);
                                                                                bases.push(type_text.to_string());
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                break;
                                            } else if child.kind() == "interface_type" {
                                                is_type_def = true;
                                                // Extract method signatures from interface
                                                // interface_type contains method_elem children with field_identifier
                                                for mi in 0..child.named_child_count() {
                                                    if let Some(method_node) = child.named_child(mi)
                                                    {
                                                        if method_node.kind() == "method_elem" {
                                                            // method_elem's first named child with kind field_identifier is the name
                                                            for mk in 0..method_node.child_count() {
                                                                if let Some(mc) =
                                                                    method_node.child(mk)
                                                                {
                                                                    if mc.kind()
                                                                        == "field_identifier"
                                                                    {
                                                                        methods.push(
                                                                            get_node_text(
                                                                                &mc,
                                                                                source_bytes,
                                                                            )
                                                                            .to_string(),
                                                                        );
                                                                        break;
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                break;
                                            }
                                        }
                                    }
                                    if is_type_def {
                                        classes.push(ClassDef::new(
                                            name, line, end_line, methods, bases,
                                        ));
                                    }
                                }
                            }
                        }
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

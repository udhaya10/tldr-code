//! C language handler for call graph analysis.
//!
//! This module provides C-specific call graph support using tree-sitter-c.
//!
//! # Import Patterns Supported
//!
//! | Pattern | ImportDef |
//! |---------|-----------|
//! | `#include <header.h>` | `{module: "header.h", is_namespace: true}` (system) |
//! | `#include "header.h"` | `{module: "header.h", is_namespace: false}` (local) |
//!
//! # Call Extraction
//!
//! - Direct calls: `func()` -> CallType::Direct or CallType::Intra
//! - Function pointer calls: `(*func_ptr)()` -> CallType::Direct
//! - Struct member function pointers: `obj->callback()` -> CallType::Attr
//! - Global/static/const initializer calls: `int x = foo();` -> `<module>` -> foo
//! - Default parameter calls (GNU extension): `void f(int x = val())` -> f -> val
//! - Designated initializer calls: `.field = func()` in struct initializers
//!
//! # Spec Reference
//!
//! See `migration/spec/callgraph-spec.md` Section 9.3 for C-specific details.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use tree_sitter::{Node, Tree};

use super::base::{get_node_text, walk_tree};
use super::{CallGraphLanguageSupport, ParseError};
use crate::callgraph::cross_file_types::{CallSite, CallType, ClassDef, FuncDef, ImportDef};

// =============================================================================
// C Handler
// =============================================================================

/// C language handler using tree-sitter-c.
///
/// Supports:
/// - Include parsing (system and local headers)
/// - Call extraction (direct, function pointer, struct member)
/// - Global/static/const initializer calls at file scope (`<module>` caller)
/// - Default parameter calls (GNU C extension)
/// - Macro call detection (limited)
#[derive(Debug, Default)]
pub struct CHandler;

impl CHandler {
    /// Creates a new CHandler.
    pub fn new() -> Self {
        Self
    }

    /// Get the function name from a function_definition node.
    fn get_function_name(&self, node: &Node, source: &[u8]) -> Option<String> {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                match child.kind() {
                    "function_declarator" => {
                        // Regular function: int func()
                        for j in 0..child.child_count() {
                            if let Some(dc) = child.child(j) {
                                if dc.kind() == "identifier" {
                                    return Some(get_node_text(&dc, source).to_string());
                                }
                            }
                        }
                    }
                    "pointer_declarator" => {
                        // Pointer return: int* func()
                        for j in 0..child.child_count() {
                            if let Some(pc) = child.child(j) {
                                if pc.kind() == "function_declarator" {
                                    for k in 0..pc.child_count() {
                                        if let Some(dc) = pc.child(k) {
                                            if dc.kind() == "identifier" {
                                                return Some(
                                                    get_node_text(&dc, source).to_string(),
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        None
    }

    /// Collect all function names defined in the file.
    fn collect_definitions(&self, tree: &Tree, source: &[u8]) -> HashSet<String> {
        let mut functions = HashSet::new();

        for node in walk_tree(tree.root_node()) {
            if node.kind() == "function_definition" {
                if let Some(name) = self.get_function_name(&node, source) {
                    functions.insert(name);
                }
            }
        }

        functions
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

                // Get the function being called
                if let Some(func_node) = child.child(0) {
                    match func_node.kind() {
                        "identifier" => {
                            // Direct call: func()
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
                        "parenthesized_expression" => {
                            // Function pointer call: (*func_ptr)() or (func_ptr)()
                            let inner_text = get_node_text(&func_node, source).to_string();
                            // Try to extract the identifier
                            let target = inner_text
                                .trim_matches(|c| c == '(' || c == ')' || c == '*')
                                .to_string();
                            if !target.is_empty() {
                                calls.push(CallSite::new(
                                    caller.to_string(),
                                    target,
                                    CallType::Direct,
                                    Some(line),
                                    None,
                                    None,
                                    None,
                                ));
                            }
                        }
                        "field_expression" => {
                            // Struct member call: obj->callback() or obj.callback()
                            let mut receiver = None;
                            let mut field = None;

                            for i in 0..func_node.child_count() {
                                if let Some(fc) = func_node.child(i) {
                                    match fc.kind() {
                                        "identifier" => {
                                            if receiver.is_none() {
                                                receiver =
                                                    Some(get_node_text(&fc, source).to_string());
                                            }
                                        }
                                        "field_identifier" => {
                                            field = Some(get_node_text(&fc, source).to_string());
                                        }
                                        _ => {}
                                    }
                                }
                            }

                            if let Some(f) = field {
                                calls.push(CallSite::new(
                                    caller.to_string(),
                                    f.clone(),
                                    CallType::Attr,
                                    Some(line),
                                    None,
                                    receiver,
                                    None,
                                ));
                            }
                        }
                        _ => {
                            // Other patterns - try to get the text
                            let target = get_node_text(&func_node, source).to_string();
                            if !target.is_empty() {
                                calls.push(CallSite::new(
                                    caller.to_string(),
                                    target,
                                    CallType::Direct,
                                    Some(line),
                                    None,
                                    None,
                                    None,
                                ));
                            }
                        }
                    }
                }
            }
        }

        calls
    }

    /// Extract calls from default parameter values in a function_declarator.
    /// Handles GNU C extension default parameters and shared C/C++ headers.
    /// e.g. void foo(int x = compute(), int y = create())
    fn extract_default_param_calls(
        &self,
        func_declarator: &Node,
        source: &[u8],
        defined_funcs: &HashSet<String>,
        caller: &str,
    ) -> Vec<CallSite> {
        let mut calls = Vec::new();
        for child in walk_tree(*func_declarator) {
            if child.kind() == "optional_parameter_declaration" {
                let param_calls =
                    self.extract_calls_from_func(&child, source, defined_funcs, caller);
                calls.extend(param_calls);
            }
        }
        calls
    }
}

impl CallGraphLanguageSupport for CHandler {
    fn name(&self) -> &str {
        "c"
    }

    fn extensions(&self) -> &[&str] {
        &[".c", ".h"]
    }

    fn parse_imports(&self, source: &str, _path: &Path) -> Result<Vec<ImportDef>, ParseError> {
        let tree = super::c_common::parse_source_with_language(
            source,
            tree_sitter_c::LANGUAGE.into(),
            "C",
        )?;
        Ok(super::c_common::parse_preproc_imports(&tree, source))
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

        let root = tree.root_node();

        // Walk only top-level children (translation_unit direct children)
        for i in 0..root.child_count() {
            let Some(node) = root.child(i) else { continue };

            match node.kind() {
                "function_definition" => {
                    if let Some(func_name) = self.get_function_name(&node, source_bytes) {
                        let mut all_calls = Vec::new();

                        for j in 0..node.child_count() {
                            if let Some(child) = node.child(j) {
                                match child.kind() {
                                    // Function body calls
                                    "compound_statement" => {
                                        let calls = self.extract_calls_from_func(
                                            &child,
                                            source_bytes,
                                            &defined_funcs,
                                            &func_name,
                                        );
                                        all_calls.extend(calls);
                                    }
                                    // Default parameter calls (GNU C extension / C++ headers)
                                    "function_declarator" => {
                                        let calls = self.extract_default_param_calls(
                                            &child,
                                            source_bytes,
                                            &defined_funcs,
                                            &func_name,
                                        );
                                        all_calls.extend(calls);
                                    }
                                    _ => {}
                                }
                            }
                        }

                        if !all_calls.is_empty() {
                            calls_by_func.insert(func_name.clone(), all_calls);
                        }
                    }
                }
                // Global/static/const variable initializer calls at file scope
                "declaration" => {
                    let has_call = walk_tree(node).any(|n| n.kind() == "call_expression");
                    if has_call {
                        let calls = self.extract_calls_from_func(
                            &node,
                            source_bytes,
                            &defined_funcs,
                            "<module>",
                        );
                        if !calls.is_empty() {
                            calls_by_func
                                .entry("<module>".to_string())
                                .or_default()
                                .extend(calls);
                        }
                    }
                }
                _ => {}
            }
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
                "function_definition" => {
                    if let Some(name) = self.get_function_name(&node, source_bytes) {
                        let line = node.start_position().row as u32 + 1;
                        let end_line = node.end_position().row as u32 + 1;
                        funcs.push(FuncDef::function(name, line, end_line));
                    }
                }
                "struct_specifier" => {
                    // Only capture structs with a body (definition, not just declaration)
                    let mut has_body = false;
                    let mut struct_name = None;
                    for i in 0..node.child_count() {
                        if let Some(child) = node.child(i) {
                            if child.kind() == "type_identifier" {
                                struct_name = Some(get_node_text(&child, source_bytes).to_string());
                            }
                            if child.kind() == "field_declaration_list" {
                                has_body = true;
                            }
                        }
                    }
                    if has_body {
                        if let Some(name) = struct_name {
                            let line = node.start_position().row as u32 + 1;
                            let end_line = node.end_position().row as u32 + 1;
                            classes.push(ClassDef::simple(name, line, end_line));
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

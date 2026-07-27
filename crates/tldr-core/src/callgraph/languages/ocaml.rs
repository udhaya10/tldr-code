//! OCaml language handler for call graph analysis.
//!
//! This module provides OCaml-specific call graph support using tree-sitter-ocaml.
//!
//! # Import Patterns Supported
//!
//! | Pattern | ImportDef |
//! |---------|-----------|
//! | `open List` | `{module: "List", is_from: true, names: ["*"]}` |
//! | `open Core.List` | `{module: "Core.List", is_from: true, names: ["*"]}` |
//! | `module M = List` | `{module: "List", alias: "M"}` |
//! | `include SomeModule` | `{module: "SomeModule", is_namespace: true}` |
//!
//! # Call Extraction
//!
//! - Direct calls: `func arg` -> CallType::Direct or CallType::Intra
//! - Qualified calls: `List.map f xs` -> CallType::Attr
//! - Pipe operator: `x |> f |> g` -> CallType::Direct for each function
//! - Application expressions: `f x y` -> CallType::Direct/Intra
//!
//! # OCaml-Specific Patterns
//!
//! - `let` bindings for function definitions
//! - `let rec` for recursive functions
//! - `module M = struct ... end` for nested modules
//! - Curried function application: `f x y` is `(f x) y`
//!
//! # Spec Reference
//!
//! See `migration/spec/callgraph-spec.md` Section 9.14 for OCaml-specific details.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use tree_sitter::{Node, Parser, Tree};

use super::base::{get_node_text, walk_tree};
use super::common::{extend_calls_if_any, insert_calls_if_any, qualify_name, receiver_from_target};
use super::{CallGraphLanguageSupport, ParseError};
use crate::callgraph::cross_file_types::{CallSite, CallType, ClassDef, FuncDef, ImportDef};

// =============================================================================
// OCaml Handler
// =============================================================================

/// OCaml language handler using tree-sitter-ocaml.
///
/// Supports:
/// - Open statements (`open List`)
/// - Module aliases (`module M = List`)
/// - Include statements (`include SomeModule`)
/// - Let bindings for function definitions
/// - Application expressions (function calls)
/// - Pipe operator (`|>`)
/// - Qualified module paths (`List.map`)
#[derive(Debug, Default)]
pub struct OcamlHandler;

impl OcamlHandler {
    /// Creates a new OcamlHandler.
    pub fn new() -> Self {
        Self
    }

    /// Parse the source code into a tree-sitter Tree.
    fn parse_source(&self, source: &str) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();

        // tree-sitter-ocaml provides LANGUAGE_OCAML constant for .ml files
        // Note: ABI compatibility may vary; tests can be marked #[ignore] if needed
        parser
            .set_language(&tree_sitter_ocaml::LANGUAGE_OCAML.into())
            .map_err(|e| ParseError::ParseFailed {
                file: std::path::PathBuf::new(),
                message: format!("Failed to set OCaml language: {}", e),
            })?;

        parser
            .parse(source, None)
            .ok_or_else(|| ParseError::ParseFailed {
                file: std::path::PathBuf::new(),
                message: "Parser returned None".to_string(),
            })
    }

    /// Extract full module path from a module_path or module_name node.
    ///
    /// Module paths can be nested: `Core.List` becomes `module_path(module_path(Core).List)`
    fn extract_module_path(&self, node: &Node, source: &[u8]) -> Option<String> {
        match node.kind() {
            "module_name" | "module_type_name" | "constructor_name" => {
                Some(get_node_text(node, source).to_string())
            }
            "module_path" | "extended_module_path" => {
                let mut parts: Vec<String> = Vec::new();
                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i) {
                        match child.kind() {
                            "module_path" | "extended_module_path" => {
                                if let Some(sub_path) = self.extract_module_path(&child, source) {
                                    parts.push(sub_path);
                                }
                            }
                            "module_name" | "module_type_name" | "constructor_name" => {
                                parts.push(get_node_text(&child, source).to_string());
                            }
                            _ => {}
                        }
                    }
                }
                if parts.is_empty() {
                    None
                } else {
                    Some(parts.join("."))
                }
            }
            _ => None,
        }
    }

    /// Extract value path (possibly qualified) from a value_path or value_name node.
    ///
    /// Can be: `value_name` OR `module_path.value_name`
    fn extract_value_path(&self, node: &Node, source: &[u8]) -> Option<String> {
        match node.kind() {
            "value_name" | "value_pattern" => Some(get_node_text(node, source).to_string()),
            "value_path" => {
                let mut parts: Vec<String> = Vec::new();
                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i) {
                        match child.kind() {
                            "module_path" | "extended_module_path" => {
                                if let Some(mod_path) = self.extract_module_path(&child, source) {
                                    parts.push(mod_path);
                                }
                            }
                            "value_name" => {
                                parts.push(get_node_text(&child, source).to_string());
                            }
                            _ => {}
                        }
                    }
                }
                if parts.is_empty() {
                    None
                } else {
                    Some(parts.join("."))
                }
            }
            _ => None,
        }
    }

    /// Parse an open_module node.
    ///
    /// OCaml `open Module.Path` brings the module's contents into scope.
    fn parse_open_module(&self, node: &Node, source: &[u8]) -> Option<ImportDef> {
        if node.kind() != "open_module" && node.kind() != "open_statement" {
            return None;
        }

        // Find the module path child
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if let Some(module_name) = self.extract_module_path(&child, source) {
                    // open Module is like "from Module import *" in Python
                    let mut imp = ImportDef::wildcard_import(&module_name);
                    imp.is_from = true;
                    return Some(imp);
                }
            }
        }
        None
    }

    /// Parse a module_definition node for module aliases.
    ///
    /// OCaml `module M = List` creates an alias.
    fn parse_module_alias(&self, node: &Node, source: &[u8]) -> Option<ImportDef> {
        if node.kind() != "module_definition" {
            return None;
        }

        let mut alias_name: Option<String> = None;
        let mut target_module: Option<String> = None;

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "module_binding" {
                    // Find the module name (alias) and the target
                    for j in 0..child.child_count() {
                        if let Some(binding_child) = child.child(j) {
                            match binding_child.kind() {
                                "module_name" => {
                                    if alias_name.is_none() {
                                        alias_name =
                                            Some(get_node_text(&binding_child, source).to_string());
                                    } else {
                                        // Second module_name is the target
                                        target_module =
                                            Some(get_node_text(&binding_child, source).to_string());
                                    }
                                }
                                "module_path" | "extended_module_path" => {
                                    target_module =
                                        self.extract_module_path(&binding_child, source);
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        // Only create import if we have both alias and target, and they're different
        // (not a full module definition)
        if let (Some(alias), Some(target)) = (alias_name, target_module) {
            let mut imp = ImportDef::simple_import(&target);
            imp.alias = Some(alias);
            return Some(imp);
        }
        None
    }

    /// Parse an include_module node.
    ///
    /// OCaml `include SomeModule` includes all definitions.
    fn parse_include_module(&self, node: &Node, source: &[u8]) -> Option<ImportDef> {
        if node.kind() != "include_module" && node.kind() != "include_statement" {
            return None;
        }

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if let Some(module_name) = self.extract_module_path(&child, source) {
                    let mut imp = ImportDef::wildcard_import(&module_name);
                    imp.is_namespace = true;
                    return Some(imp);
                }
            }
        }
        None
    }

    /// Collect all function definitions (let bindings).
    fn collect_definitions(&self, tree: &Tree, source: &[u8]) -> HashSet<String> {
        let mut definitions = HashSet::new();

        fn collect_from_node(
            node: Node,
            source: &[u8],
            definitions: &mut HashSet<String>,
            module_path: &[String],
        ) {
            match node.kind() {
                "module_definition" => {
                    // Extract module name and recurse into body
                    let mut mod_name: Option<String> = None;
                    let mut body: Option<Node> = None;

                    for i in 0..node.child_count() {
                        if let Some(child) = node.child(i) {
                            if child.kind() == "module_binding" {
                                for j in 0..child.child_count() {
                                    if let Some(binding_child) = child.child(j) {
                                        match binding_child.kind() {
                                            "module_name" => {
                                                mod_name = Some(
                                                    get_node_text(&binding_child, source)
                                                        .to_string(),
                                                );
                                            }
                                            "structure" | "module_content" => {
                                                body = Some(binding_child);
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if let Some(name) = mod_name {
                        definitions.insert(name.clone());
                        let mut new_path = module_path.to_vec();
                        new_path.push(name);

                        if let Some(structure) = body {
                            for i in 0..structure.child_count() {
                                if let Some(child) = structure.child(i) {
                                    collect_from_node(child, source, definitions, &new_path);
                                }
                            }
                        }
                    }
                    return;
                }
                "value_definition" | "let_binding" => {
                    // let foo = expr or let rec foo = expr
                    for i in 0..node.child_count() {
                        if let Some(child) = node.child(i) {
                            if child.kind() == "let_binding" || child.kind() == "value_definition" {
                                // Find the value_name
                                for j in 0..child.child_count() {
                                    if let Some(binding_child) = child.child(j) {
                                        if binding_child.kind() == "value_name"
                                            || binding_child.kind() == "value_pattern"
                                        {
                                            let func_name =
                                                get_node_text(&binding_child, source).to_string();
                                            // VAL-018: skip `_`/`()` wildcard
                                            // bindings (see process_let_binding_calls
                                            // for the rationale).
                                            if func_name == "_"
                                                || func_name == "()"
                                                || func_name.is_empty()
                                            {
                                                continue;
                                            }
                                            definitions.insert(func_name.clone());
                                            // Also add qualified name
                                            if !module_path.is_empty() {
                                                let qualified = format!(
                                                    "{}.{}",
                                                    module_path.join("."),
                                                    func_name
                                                );
                                                definitions.insert(qualified);
                                            }
                                        }
                                    }
                                }
                            } else if child.kind() == "value_name"
                                || child.kind() == "value_pattern"
                            {
                                let func_name = get_node_text(&child, source).to_string();
                                if func_name == "_" || func_name == "()" || func_name.is_empty() {
                                    continue;
                                }
                                definitions.insert(func_name.clone());
                                if !module_path.is_empty() {
                                    let qualified =
                                        format!("{}.{}", module_path.join("."), func_name);
                                    definitions.insert(qualified);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }

            // Recurse into children
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    collect_from_node(child, source, definitions, module_path);
                }
            }
        }

        collect_from_node(tree.root_node(), source, &mut definitions, &[]);
        definitions
    }

    /// Extract calls from a function body.
    fn extract_calls_from_node(
        &self,
        node: &Node,
        source: &[u8],
        defined_funcs: &HashSet<String>,
        caller: &str,
    ) -> Vec<CallSite> {
        let mut calls = Vec::new();

        for child in walk_tree(*node) {
            let call_name = match child.kind() {
                "application_expression" | "application" => {
                    self.extract_application_call_name(&child, source)
                }
                "infix_expression" => self.extract_pipe_call_name(&child, source),
                _ => None,
            };

            if let Some(name) = call_name {
                let line = child.start_position().row as u32 + 1;
                calls.push(self.build_call_site(caller, name, line, defined_funcs));
            }
        }

        calls
    }

    fn extract_application_call_name(&self, node: &Node, source: &[u8]) -> Option<String> {
        let func_child = node.child(0)?;
        let name = self.extract_value_path(&func_child, source).or_else(|| {
            let text = get_node_text(&func_child, source).to_string();
            if !text.is_empty() && !text.contains(' ') {
                Some(text)
            } else {
                None
            }
        })?;

        if name.starts_with('(') || name.is_empty() {
            return None;
        }
        Some(name)
    }

    fn extract_pipe_call_name(&self, node: &Node, source: &[u8]) -> Option<String> {
        let mut found_pipe = false;
        for i in 0..node.child_count() {
            let Some(infix_child) = node.child(i) else {
                continue;
            };
            let text = get_node_text(&infix_child, source);
            if text == "|>" {
                found_pipe = true;
                continue;
            }
            if found_pipe && infix_child.kind() != "infix_operator" {
                return self.extract_value_path(&infix_child, source).or_else(|| {
                    let value = text.to_string();
                    if !value.is_empty() && !value.contains(' ') {
                        Some(value)
                    } else {
                        None
                    }
                });
            }
        }
        None
    }

    fn build_call_site(
        &self,
        caller: &str,
        name: String,
        line: u32,
        defined_funcs: &HashSet<String>,
    ) -> CallSite {
        let (call_type, receiver) = self.classify_call_target(&name, defined_funcs);
        CallSite::new(
            caller.to_string(),
            name,
            call_type,
            Some(line),
            None,
            receiver,
            None,
        )
    }

    fn classify_call_target(
        &self,
        name: &str,
        defined_funcs: &HashSet<String>,
    ) -> (CallType, Option<String>) {
        if name.contains('.') {
            return (CallType::Attr, receiver_from_target(name, '.'));
        }
        if defined_funcs.contains(name) {
            return (CallType::Intra, None);
        }
        (CallType::Direct, None)
    }

    fn is_function_binding(binding: &Node) -> bool {
        (0..binding.child_count())
            .filter_map(|idx| binding.child(idx))
            .any(|child| child.kind() == "parameter")
    }

    fn is_unit_pattern(binding: &Node, source: &[u8]) -> bool {
        let Some(pattern) = binding.child_by_field_name("pattern") else {
            return false;
        };
        pattern.kind() == "unit"
            || (pattern.kind() == "parenthesized_pattern"
                && get_node_text(&pattern, source) == "()")
    }

    fn extract_binding_name(binding: &Node, source: &[u8]) -> Option<String> {
        if let Some(pattern) = binding.child_by_field_name("pattern") {
            if matches!(pattern.kind(), "value_name" | "value_pattern") {
                return Some(get_node_text(&pattern, source).to_string());
            }
        }

        for i in 0..binding.child_count() {
            let Some(child) = binding.child(i) else {
                continue;
            };
            if matches!(child.kind(), "value_name" | "value_pattern") {
                return Some(get_node_text(&child, source).to_string());
            }
        }
        None
    }

    fn find_named_child_text(
        &self,
        node: &Node,
        field_name: &str,
        fallback_kind: &str,
        source: &[u8],
    ) -> Option<String> {
        if let Some(named) = node.child_by_field_name(field_name) {
            return Some(get_node_text(&named, source).to_string());
        }
        for i in 0..node.child_count() {
            let Some(child) = node.child(i) else {
                continue;
            };
            if child.kind() == fallback_kind {
                return Some(get_node_text(&child, source).to_string());
            }
        }
        None
    }

    fn recurse_children_for_calls(
        &self,
        node: Node,
        source: &[u8],
        defined_funcs: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        module_path: &[String],
    ) {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                self.process_calls_node(child, source, defined_funcs, calls_by_func, module_path);
            }
        }
    }

    fn process_calls_node(
        &self,
        node: Node,
        source: &[u8],
        defined_funcs: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        module_path: &[String],
    ) {
        match node.kind() {
            "module_definition" => {
                self.process_module_definition_calls(
                    node,
                    source,
                    defined_funcs,
                    calls_by_func,
                    module_path,
                );
                return;
            }
            "value_definition" => {
                self.process_value_definition_calls(
                    node,
                    source,
                    defined_funcs,
                    calls_by_func,
                    module_path,
                );
                return;
            }
            "let_binding" => {
                self.process_let_binding_calls(
                    node,
                    source,
                    defined_funcs,
                    calls_by_func,
                    module_path,
                );
                return;
            }
            "class_definition" => {
                self.process_class_definition_calls(
                    node,
                    source,
                    defined_funcs,
                    calls_by_func,
                    module_path,
                );
                return;
            }
            _ => {}
        }

        self.recurse_children_for_calls(node, source, defined_funcs, calls_by_func, module_path);
    }

    fn process_module_definition_calls(
        &self,
        node: Node,
        source: &[u8],
        defined_funcs: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        module_path: &[String],
    ) {
        for i in 0..node.child_count() {
            let Some(child) = node.child(i) else {
                continue;
            };
            if child.kind() != "module_binding" {
                continue;
            }

            let mod_name = self.find_named_child_text(&child, "name", "module_name", source);
            let body = child.child_by_field_name("body");
            let Some(name) = mod_name else {
                continue;
            };

            let mut new_path = module_path.to_vec();
            new_path.push(name);

            let Some(body_node) = body else {
                continue;
            };
            match body_node.kind() {
                "structure" | "module_content" => {
                    self.recurse_children_for_calls(
                        body_node,
                        source,
                        defined_funcs,
                        calls_by_func,
                        &new_path,
                    );
                }
                "module_application" => {
                    let Some(functor) = body_node.child_by_field_name("functor") else {
                        continue;
                    };
                    let functor_name = get_node_text(&functor, source).to_string();
                    let line = body_node.start_position().row as u32 + 1;
                    let caller = if module_path.is_empty() {
                        "<module>".to_string()
                    } else {
                        module_path.join(".")
                    };
                    let call = CallSite::new(
                        caller.clone(),
                        functor_name,
                        CallType::Direct,
                        Some(line),
                        None,
                        None,
                        None,
                    );
                    extend_calls_if_any(calls_by_func, caller, vec![call]);
                }
                _ => {}
            }
        }
    }

    fn process_value_definition_calls(
        &self,
        node: Node,
        source: &[u8],
        defined_funcs: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        module_path: &[String],
    ) {
        for i in 0..node.child_count() {
            let Some(child) = node.child(i) else {
                continue;
            };
            if child.kind() == "let_binding" {
                self.process_let_binding_calls(
                    child,
                    source,
                    defined_funcs,
                    calls_by_func,
                    module_path,
                );
            }
        }
    }

    fn process_let_binding_calls(
        &self,
        binding: Node,
        source: &[u8],
        defined_funcs: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        module_path: &[String],
    ) {
        let body = binding.child_by_field_name("body");
        let has_params = Self::is_function_binding(&binding);
        let is_unit = Self::is_unit_pattern(&binding, source);

        if is_unit {
            let module_caller = if module_path.is_empty() {
                "<module>".to_string()
            } else {
                format!("{}.<module>", module_path.join("."))
            };

            let calls = if let Some(body_node) = body {
                self.extract_calls_from_node(&body_node, source, defined_funcs, &module_caller)
            } else {
                self.extract_calls_from_node(&binding, source, defined_funcs, "<module>")
            };

            extend_calls_if_any(calls_by_func, "<module>".to_string(), calls);
            return;
        }

        let Some(name) = Self::extract_binding_name(&binding, source) else {
            return;
        };
        // VAL-018: `let _ = expr in body` is a wildcard binding used to
        // discard a value (typically inside function bodies). It is NOT
        // a named definition and must not surface as a caller in the
        // call graph — its calls were already attributed to the
        // enclosing function via `extract_calls_from_node` walking
        // through this node. Skip without inserting.
        if name == "_" {
            return;
        }
        let module_prefix = if module_path.is_empty() {
            None
        } else {
            Some(module_path.join("."))
        };
        let full_name = qualify_name(module_prefix.as_deref(), &name, ".");

        if has_params {
            let mut all_calls = Vec::new();
            if let Some(body_node) = body {
                all_calls.extend(self.extract_calls_from_node(
                    &body_node,
                    source,
                    defined_funcs,
                    &full_name,
                ));
            }
            for i in 0..binding.child_count() {
                let Some(child) = binding.child(i) else {
                    continue;
                };
                if child.kind() != "parameter" {
                    continue;
                }
                if let Some(default_val) = child.child_by_field_name("default") {
                    all_calls.extend(self.extract_calls_from_node(
                        &default_val,
                        source,
                        defined_funcs,
                        &full_name,
                    ));
                }
            }

            if all_calls.is_empty() {
                return;
            }

            insert_calls_if_any(calls_by_func, full_name.clone(), all_calls.clone());
            if full_name != name {
                insert_calls_if_any(calls_by_func, name, all_calls);
            }
            return;
        }

        let module_calls = if let Some(body_node) = body {
            self.extract_calls_from_node(&body_node, source, defined_funcs, "<module>")
        } else {
            self.extract_calls_from_node(&binding, source, defined_funcs, "<module>")
        };
        extend_calls_if_any(calls_by_func, "<module>".to_string(), module_calls);
    }

    fn process_class_definition_calls(
        &self,
        node: Node,
        source: &[u8],
        defined_funcs: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        module_path: &[String],
    ) {
        for i in 0..node.child_count() {
            let Some(child) = node.child(i) else {
                continue;
            };
            if child.kind() != "class_binding" {
                continue;
            }

            let class_name = self.find_named_child_text(&child, "name", "class_name", source);
            let Some(body) = child.child_by_field_name("body") else {
                continue;
            };
            if body.kind() != "object_expression" {
                continue;
            }

            self.process_class_body_calls(
                body,
                source,
                defined_funcs,
                calls_by_func,
                module_path,
                class_name.as_deref(),
            );
        }
    }

    fn process_class_body_calls(
        &self,
        obj_expr: Node,
        source: &[u8],
        defined_funcs: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        module_path: &[String],
        class_name: Option<&str>,
    ) {
        let class_prefix = class_name.unwrap_or("_anon_class");

        for i in 0..obj_expr.child_count() {
            let Some(child) = obj_expr.child(i) else {
                continue;
            };

            match child.kind() {
                "method_definition" => {
                    let method_name =
                        self.find_named_child_text(&child, "name", "method_name", source);
                    let Some(method_name) = method_name else {
                        continue;
                    };

                    let caller = if module_path.is_empty() {
                        format!("{class_prefix}.{method_name}")
                    } else {
                        format!("{}.{}.{}", module_path.join("."), class_prefix, method_name)
                    };

                    let search_node = child.child_by_field_name("body").unwrap_or(child);
                    let calls =
                        self.extract_calls_from_node(&search_node, source, defined_funcs, &caller);
                    insert_calls_if_any(calls_by_func, caller, calls);
                }
                "class_initializer" => {
                    let caller = if module_path.is_empty() {
                        format!("{class_prefix}.<init>")
                    } else {
                        format!("{}.{}.<init>", module_path.join("."), class_prefix)
                    };

                    let init_expr = child.child_by_field_name("initializer").or_else(|| {
                        (0..child.child_count())
                            .filter_map(|idx| child.child(idx))
                            .find(|ic| ic.kind() != "initializer" && ic.kind() != "comment")
                    });

                    let search_node = init_expr.unwrap_or(child);
                    let calls =
                        self.extract_calls_from_node(&search_node, source, defined_funcs, &caller);
                    insert_calls_if_any(calls_by_func, caller, calls);
                }
                _ => {}
            }
        }
    }
}

impl CallGraphLanguageSupport for OcamlHandler {
    fn name(&self) -> &str {
        "ocaml"
    }

    fn extensions(&self) -> &[&str] {
        &[".ml", ".mli"]
    }

    fn parse_imports(&self, source: &str, _path: &Path) -> Result<Vec<ImportDef>, ParseError> {
        let tree = self.parse_source(source)?;
        let source_bytes = source.as_bytes();
        let mut imports = Vec::new();

        for node in walk_tree(tree.root_node()) {
            match node.kind() {
                "open_module" | "open_statement" => {
                    if let Some(imp) = self.parse_open_module(&node, source_bytes) {
                        imports.push(imp);
                    }
                }
                "module_definition" => {
                    // Check if it's a module alias (module M = X)
                    if let Some(imp) = self.parse_module_alias(&node, source_bytes) {
                        imports.push(imp);
                    }
                }
                "include_module" | "include_statement" => {
                    if let Some(imp) = self.parse_include_module(&node, source_bytes) {
                        imports.push(imp);
                    }
                }
                _ => {}
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
        self.process_calls_node(
            tree.root_node(),
            source_bytes,
            &defined_funcs,
            &mut calls_by_func,
            &[],
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

        fn collect_defs(
            node: Node,
            source: &[u8],
            funcs: &mut Vec<FuncDef>,
            classes: &mut Vec<ClassDef>,
            module_path: &[String],
        ) {
            match node.kind() {
                "module_definition" => {
                    let mut mod_name: Option<String> = None;
                    let mut body: Option<Node> = None;

                    for i in 0..node.child_count() {
                        if let Some(child) = node.child(i) {
                            if child.kind() == "module_binding" {
                                for j in 0..child.child_count() {
                                    if let Some(bc) = child.child(j) {
                                        match bc.kind() {
                                            "module_name" => {
                                                mod_name =
                                                    Some(get_node_text(&bc, source).to_string());
                                            }
                                            "structure" | "module_content" => {
                                                body = Some(bc);
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if let Some(name) = mod_name {
                        let line = node.start_position().row as u32 + 1;
                        let end_line = node.end_position().row as u32 + 1;
                        classes.push(ClassDef::simple(&name, line, end_line));

                        let mut new_path = module_path.to_vec();
                        new_path.push(name);

                        if let Some(structure) = body {
                            for i in 0..structure.child_count() {
                                if let Some(child) = structure.child(i) {
                                    collect_defs(child, source, funcs, classes, &new_path);
                                }
                            }
                        }
                    }
                    return;
                }
                "value_definition" | "let_binding" => {
                    for i in 0..node.child_count() {
                        if let Some(child) = node.child(i) {
                            if child.kind() == "let_binding" || child.kind() == "value_definition" {
                                for j in 0..child.child_count() {
                                    if let Some(bc) = child.child(j) {
                                        if bc.kind() == "value_name" || bc.kind() == "value_pattern"
                                        {
                                            let func_name = get_node_text(&bc, source).to_string();
                                            // VAL-018: skip wildcard `_` and unit `()`
                                            // patterns. `let _ = expr in body` is a
                                            // value-discard expression, not a function
                                            // definition. Without this filter, the
                                            // resolver picks `_` over the enclosing
                                            // function (smaller line span wins) and
                                            // produces edges with src_func="_".
                                            if func_name == "_"
                                                || func_name == "()"
                                                || func_name.is_empty()
                                            {
                                                continue;
                                            }
                                            let line = node.start_position().row as u32 + 1;
                                            let end_line = node.end_position().row as u32 + 1;

                                            if module_path.is_empty() {
                                                funcs.push(FuncDef::function(
                                                    func_name, line, end_line,
                                                ));
                                            } else {
                                                funcs.push(FuncDef::method(
                                                    func_name,
                                                    module_path.join("."),
                                                    line,
                                                    end_line,
                                                ));
                                            }
                                        }
                                    }
                                }
                            } else if child.kind() == "value_name"
                                || child.kind() == "value_pattern"
                            {
                                let func_name = get_node_text(&child, source).to_string();
                                // VAL-018: same filter as above branch (see comment).
                                if func_name == "_" || func_name == "()" || func_name.is_empty() {
                                    continue;
                                }
                                let line = node.start_position().row as u32 + 1;
                                let end_line = node.end_position().row as u32 + 1;

                                if module_path.is_empty() {
                                    funcs.push(FuncDef::function(func_name, line, end_line));
                                } else {
                                    funcs.push(FuncDef::method(
                                        func_name,
                                        module_path.join("."),
                                        line,
                                        end_line,
                                    ));
                                }
                            }
                        }
                    }
                }
                _ => {}
            }

            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    collect_defs(child, source, funcs, classes, module_path);
                }
            }
        }

        collect_defs(
            tree.root_node(),
            source_bytes,
            &mut funcs,
            &mut classes,
            &[],
        );

        Ok((funcs, classes))
    }
}

// =============================================================================
// Tests
// =============================================================================

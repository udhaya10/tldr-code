//! Elixir language handler for call graph analysis.
//!
//! This module provides Elixir-specific call graph support using tree-sitter-elixir.
//!
//! # Import Patterns Supported
//!
//! | Pattern | ImportDef |
//! |---------|-----------|
//! | `alias MyApp.Module` | `{module: "MyApp.Module", alias: "Module"}` |
//! | `alias MyApp.Module, as: M` | `{module: "MyApp.Module", alias: "M"}` |
//! | `import Enum` | `{module: "Enum", is_from: true, names: ["*"]}` |
//! | `import Enum, only: [map: 2]` | `{module: "Enum", is_from: true, names: ["map"]}` |
//! | `use GenServer` | `{module: "GenServer"}` |
//! | `require Logger` | `{module: "Logger"}` |
//!
//! # Call Extraction
//!
//! - Qualified calls: `Module.func()` -> CallType::Attr
//! - Direct calls: `func()` -> CallType::Direct or CallType::Intra
//! - Pipeline calls: `data |> func()` -> tracked as direct/attr calls
//!
//! # Module Naming Convention
//!
//! Elixir modules follow CamelCase naming:
//! - `MyApp.Users.Admin` -> `lib/my_app/users/admin.ex`
//!
//! # Spec Reference
//!
//! See `migration/spec/callgraph-spec.md` Section 9 for language-specific details.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use tree_sitter::{Node, Parser, Tree};

use super::base::{get_node_text, walk_tree};
use super::common::{extend_calls_if_any, insert_calls_if_any};
use super::{CallGraphLanguageSupport, ParseError};
use crate::callgraph::cross_file_types::{CallSite, CallType, ClassDef, FuncDef, ImportDef};

// =============================================================================
// Elixir Handler
// =============================================================================

/// Elixir keywords that should be skipped during call extraction.
/// These are definition forms and import directives, not function calls.
const ELIXIR_SKIP_KEYWORDS: &[&str] = &[
    "alias",
    "import",
    "use",
    "require",
    "def",
    "defp",
    "defmodule",
    "defmacro",
    "defmacrop",
    "defstruct",
    "defprotocol",
    "defimpl",
];

/// Returns true if the given name is an Elixir keyword that should be skipped.
fn is_elixir_skip_keyword(name: &str) -> bool {
    ELIXIR_SKIP_KEYWORDS.contains(&name)
}

/// Elixir language handler using tree-sitter-elixir.
///
/// Supports:
/// - Import parsing (alias, import, use, require)
/// - Call extraction (qualified, direct, pipeline)
/// - Module and function tracking
/// - `<module>` synthetic function for module-level calls
#[derive(Debug, Default)]
pub struct ElixirHandler;

impl ElixirHandler {
    /// Creates a new ElixirHandler.
    pub fn new() -> Self {
        Self
    }

    /// Parse the source code into a tree-sitter Tree.
    fn parse_source(&self, source: &str) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_elixir::LANGUAGE.into())
            .map_err(|e| ParseError::ParseFailed {
                file: std::path::PathBuf::new(),
                message: format!("Failed to set Elixir language: {}", e),
            })?;

        parser
            .parse(source, None)
            .ok_or_else(|| ParseError::ParseFailed {
                file: std::path::PathBuf::new(),
                message: "Parser returned None".to_string(),
            })
    }

    /// Parse an alias/import/use/require call node.
    ///
    /// Elixir import patterns:
    /// - `alias MyApp.Module` - Creates shorthand
    /// - `alias MyApp.Module, as: M` - Creates custom shorthand
    /// - `import Enum` - Imports all functions
    /// - `import Enum, only: [map: 2]` - Selective import
    /// - `use GenServer` - Invokes __using__ macro
    /// - `require Logger` - Compile-time require
    fn parse_import_call(&self, node: &Node, source: &[u8]) -> Option<ImportDef> {
        if node.kind() != "call" {
            return None;
        }

        // Get the function name (alias, import, use, require)
        let mut func_name: Option<String> = None;
        let mut arguments: Option<Node> = None;

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                match child.kind() {
                    "identifier" => {
                        if func_name.is_none() {
                            func_name = Some(get_node_text(&child, source).to_string());
                        }
                    }
                    "arguments" => {
                        arguments = Some(child);
                    }
                    _ => {}
                }
            }
        }

        let func = func_name?;
        if !matches!(func.as_str(), "alias" | "import" | "use" | "require") {
            return None;
        }

        let args = arguments?;

        // Parse the module argument and optional keywords
        let mut module: Option<String> = None;
        let mut alias_name: Option<String> = None;
        let mut only_names: Vec<String> = Vec::new();

        for i in 0..args.child_count() {
            if let Some(child) = args.child(i) {
                if !child.is_named() {
                    continue;
                }

                match child.kind() {
                    "alias" => {
                        // Module reference like Phoenix.Controller or MyApp
                        module = Some(get_node_text(&child, source).to_string());
                    }
                    "dot" => {
                        // Qualified module name like MyApp.Users.Admin
                        module = Some(get_node_text(&child, source).to_string());
                    }
                    "keywords" => {
                        // Keyword arguments like "as: AliasName" or "only: [func: 1]"
                        self.parse_keywords(&child, source, &mut alias_name, &mut only_names);
                    }
                    _ => {}
                }
            }
        }

        let module = module?;

        // Build ImportDef based on import type
        match func.as_str() {
            "alias" => {
                // Alias creates a shorthand for the module
                // Default alias is the last part of the module name
                let default_alias = module.split('.').next_back().unwrap_or(&module).to_string();
                let final_alias = alias_name.unwrap_or(default_alias);

                Some(ImportDef::import_as(&module, final_alias))
            }
            "import" => {
                // Import brings functions into namespace
                if only_names.is_empty() {
                    // Wildcard import
                    Some(ImportDef::wildcard_import(&module))
                } else {
                    // Selective import
                    Some(ImportDef::from_import(&module, only_names))
                }
            }
            "use" => {
                // Use invokes the __using__ macro
                // Treated as an absolute import
                Some(ImportDef::simple_import(&module))
            }
            "require" => {
                // Require is for compile-time dependency
                Some(ImportDef::simple_import(&module))
            }
            _ => None,
        }
    }

    /// Parse keyword arguments from an import statement.
    fn parse_keywords(
        &self,
        node: &Node,
        source: &[u8],
        alias_name: &mut Option<String>,
        only_names: &mut Vec<String>,
    ) {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "pair" {
                    let mut key: Option<String> = None;
                    let mut value: Option<Node> = None;

                    for j in 0..child.child_count() {
                        if let Some(pair_child) = child.child(j) {
                            match pair_child.kind() {
                                "keyword" => {
                                    let text = get_node_text(&pair_child, source);
                                    // Keywords end with ":" and may have trailing whitespace
                                    // Trim first to remove whitespace, then remove the colon
                                    key = Some(text.trim().trim_end_matches(':').to_string());
                                }
                                "atom" => {
                                    // Atom key (less common)
                                    let text = get_node_text(&pair_child, source);
                                    key = Some(text.trim_start_matches(':').to_string());
                                }
                                _ => {
                                    if value.is_none() && pair_child.is_named() {
                                        value = Some(pair_child);
                                    }
                                }
                            }
                        }
                    }

                    if let (Some(k), Some(v)) = (key, value) {
                        match k.as_str() {
                            "as" => {
                                // as: AliasName
                                *alias_name = Some(get_node_text(&v, source).to_string());
                            }
                            "only" => {
                                // only: [func: arity, func2: arity]
                                self.parse_only_list(&v, source, only_names);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    /// Parse the `only:` list from an import statement.
    fn parse_only_list(&self, node: &Node, source: &[u8], names: &mut Vec<String>) {
        // The list contains pairs like func_name: arity
        for child in walk_tree(*node) {
            if child.kind() == "pair" {
                // Get the function name (key)
                for i in 0..child.child_count() {
                    if let Some(pair_child) = child.child(i) {
                        if pair_child.kind() == "keyword" {
                            let text = get_node_text(&pair_child, source);
                            // Trim whitespace first, then remove the colon
                            let func_name = text.trim().trim_end_matches(':').to_string();
                            if !func_name.is_empty() {
                                names.push(func_name);
                            }
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Collect all function definitions in the file.
    fn collect_definitions(
        &self,
        tree: &Tree,
        source: &[u8],
    ) -> (HashSet<String>, HashSet<String>) {
        let mut functions = HashSet::new();
        let mut modules = HashSet::new();

        for node in walk_tree(tree.root_node()) {
            if node.kind() == "call" {
                // Check for defmodule, def, defp
                let mut func_name: Option<String> = None;

                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i) {
                        if child.kind() == "identifier" {
                            func_name = Some(get_node_text(&child, source).to_string());
                            break;
                        }
                    }
                }

                if let Some(ref name) = func_name {
                    match name.as_str() {
                        "defmodule" => {
                            // Get module name from arguments
                            if let Some(mod_name) = self.get_first_alias_arg(&node, source) {
                                modules.insert(mod_name.clone());
                                // Also add the simple name
                                if let Some(simple) = mod_name.split('.').next_back() {
                                    modules.insert(simple.to_string());
                                }
                            }
                        }
                        "def" | "defp" => {
                            // Get function name from arguments
                            if let Some(fn_name) = self.get_function_name(&node, source) {
                                functions.insert(fn_name);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        (functions, modules)
    }

    /// Get the first alias argument from a call node (for defmodule).
    fn get_first_alias_arg(&self, node: &Node, source: &[u8]) -> Option<String> {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "arguments" {
                    for j in 0..child.child_count() {
                        if let Some(arg) = child.child(j) {
                            if arg.kind() == "alias" || arg.kind() == "dot" {
                                return Some(get_node_text(&arg, source).to_string());
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// Get the function name from a def/defp call.
    ///
    /// Handles multiple AST shapes:
    /// - `def func_name` -> arguments > identifier
    /// - `def func_name(args)` -> arguments > call > identifier
    /// - `def func_name(x) when guard(x)` -> arguments > binary_operator > call > identifier
    fn get_function_name(&self, node: &Node, source: &[u8]) -> Option<String> {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "arguments" {
                    for j in 0..child.child_count() {
                        if let Some(arg) = child.child(j) {
                            match arg.kind() {
                                "call" => {
                                    // def func_name(args) do ... end
                                    return self.extract_identifier_from_node(&arg, source);
                                }
                                "identifier" => {
                                    // def func_name (no args)
                                    return Some(get_node_text(&arg, source).to_string());
                                }
                                "binary_operator" => {
                                    // def func_name(x) when guard(x), do: ...
                                    // The left child of the binary_operator is the function call/name
                                    for k in 0..arg.child_count() {
                                        if let Some(bin_child) = arg.child(k) {
                                            match bin_child.kind() {
                                                "call" => {
                                                    return self.extract_identifier_from_node(
                                                        &bin_child, source,
                                                    );
                                                }
                                                "identifier" => {
                                                    return Some(
                                                        get_node_text(&bin_child, source)
                                                            .to_string(),
                                                    );
                                                }
                                                _ => {}
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
        }
        None
    }

    /// Extract the first identifier child from a node.
    fn extract_identifier_from_node(&self, node: &Node, source: &[u8]) -> Option<String> {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "identifier" {
                    return Some(get_node_text(&child, source).to_string());
                }
            }
        }
        None
    }

    /// Extract the guard and default param calls from a def/defp node's arguments.
    ///
    /// Handles:
    /// - Guard clauses: `def foo(x) when is_valid(x)` -> extracts is_valid
    /// - Default params: `def foo(x \\ default_val())` -> extracts default_val
    fn extract_def_arg_calls(
        &self,
        node: &Node,
        source: &[u8],
        defined_functions: &HashSet<String>,
        caller: &str,
    ) -> Vec<CallSite> {
        let mut calls = Vec::new();

        // Find the arguments node of the def/defp call
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "arguments" {
                    // Walk all nodes inside arguments looking for:
                    // 1. Guard clause calls (binary_operator with "when")
                    // 2. Default param calls (binary_operator with "\\")
                    for descendant in walk_tree(child) {
                        if descendant.kind() == "binary_operator" {
                            let text = get_node_text(&descendant, source);
                            if text.contains("when") {
                                // Guard clause: extract calls from the right side of "when"
                                // The children are: left_expr, "when" keyword, right_expr
                                let mut found_when = false;
                                for k in 0..descendant.child_count() {
                                    if let Some(bin_child) = descendant.child(k) {
                                        if !bin_child.is_named()
                                            && get_node_text(&bin_child, source).trim() == "when"
                                        {
                                            found_when = true;
                                            continue;
                                        }
                                        if found_when {
                                            // Extract calls from guard expression
                                            let guard_calls = self.extract_calls_from_node(
                                                &bin_child,
                                                source,
                                                defined_functions,
                                                &HashSet::new(),
                                                caller,
                                            );
                                            calls.extend(guard_calls);
                                        }
                                    }
                                }
                            } else if text.contains("\\\\") {
                                // Default param: `x \\ default_val()`
                                // The right child of \\ is the default value
                                let mut found_default = false;
                                for k in 0..descendant.child_count() {
                                    if let Some(bin_child) = descendant.child(k) {
                                        if !bin_child.is_named()
                                            && get_node_text(&bin_child, source).trim() == "\\\\"
                                        {
                                            found_default = true;
                                            continue;
                                        }
                                        if found_default && bin_child.is_named() {
                                            // Extract calls from default value expression
                                            let default_calls = self.extract_calls_from_node(
                                                &bin_child,
                                                source,
                                                defined_functions,
                                                &HashSet::new(),
                                                caller,
                                            );
                                            calls.extend(default_calls);
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

        calls
    }

    /// Extract calls from a module attribute's value expression.
    ///
    /// For `@attr compute()`, the AST is:
    /// `unary_operator -> call(attr) -> arguments -> call(compute)`
    /// We walk the arguments of the inner call to find actual function calls.
    fn extract_attr_value_calls(
        &self,
        unary_node: &Node,
        source: &[u8],
        defined_functions: &HashSet<String>,
        caller: &str,
        calls: &mut Vec<CallSite>,
    ) {
        for ci in 0..unary_node.child_count() {
            if let Some(inner_call) = unary_node.child(ci) {
                if inner_call.kind() != "call" {
                    continue;
                }
                // Find the arguments node inside the @attr call
                for ai in 0..inner_call.child_count() {
                    if let Some(arg_node) = inner_call.child(ai) {
                        if arg_node.kind() == "arguments" {
                            // Walk arguments to find actual function calls
                            for sub in walk_tree(arg_node) {
                                if sub.kind() == "call" {
                                    if let Some(name) =
                                        self.extract_identifier_from_node(&sub, source)
                                    {
                                        if !is_elixir_skip_keyword(&name) {
                                            let sub_line = sub.start_position().row as u32 + 1;
                                            let call_type = if defined_functions.contains(&name) {
                                                CallType::Intra
                                            } else {
                                                CallType::Direct
                                            };
                                            calls.push(CallSite::new(
                                                caller.to_string(),
                                                name,
                                                call_type,
                                                Some(sub_line),
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
                }
            }
        }
    }

    /// Extract calls from a function body node.
    fn extract_calls_from_node(
        &self,
        node: &Node,
        source: &[u8],
        defined_functions: &HashSet<String>,
        _defined_modules: &HashSet<String>,
        caller: &str,
    ) -> Vec<CallSite> {
        let mut calls = Vec::new();

        for child in walk_tree(*node) {
            match child.kind() {
                "call" => {
                    let line = child.start_position().row as u32 + 1;

                    // Parse call structure
                    let mut func_name: Option<String> = None;

                    for i in 0..child.child_count() {
                        if let Some(c) = child.child(i) {
                            if c.kind() == "identifier" {
                                func_name = Some(get_node_text(&c, source).to_string());
                                break;
                            }
                        }
                    }

                    // Skip import-related and definition calls
                    if let Some(ref name) = func_name {
                        if is_elixir_skip_keyword(name) {
                            continue;
                        }

                        // Direct function call
                        let call_type = if defined_functions.contains(name) {
                            CallType::Intra
                        } else {
                            CallType::Direct
                        };

                        calls.push(CallSite::new(
                            caller.to_string(),
                            name.clone(),
                            call_type,
                            Some(line),
                            None,
                            None,
                            None,
                        ));
                    }
                }
                "dot" => {
                    // Qualified call: Module.func()
                    let line = child.start_position().row as u32 + 1;
                    let text = get_node_text(&child, source).to_string();

                    if text.contains('.') {
                        // Extract receiver (everything before last dot)
                        let parts: Vec<&str> = text.rsplitn(2, '.').collect();
                        if parts.len() == 2 {
                            let receiver = parts[1].to_string();
                            calls.push(CallSite::new(
                                caller.to_string(),
                                text.clone(),
                                CallType::Attr,
                                Some(line),
                                None,
                                Some(receiver),
                                None,
                            ));
                        }
                    }
                }
                "binary_operator" => {
                    // Check for pipe operator |>
                    // The piped function call is tracked as a regular call
                    // We process children recursively via walk_tree
                }
                "unary_operator" => {
                    // Module attribute: @attr compute()
                    // AST: unary_operator -> call(attr) -> arguments -> call(compute)
                    // We extract calls from the attribute's value arguments.
                    self.extract_attr_value_calls(
                        &child,
                        source,
                        defined_functions,
                        caller,
                        &mut calls,
                    );
                }
                _ => {}
            }
        }

        calls
    }

    fn module_caller_name(current_module: &Option<String>) -> String {
        if let Some(module) = current_module {
            format!("<{module}>")
        } else {
            "<module>".to_string()
        }
    }

    fn recurse_extract_calls_node(
        &self,
        node: Node,
        source: &[u8],
        defined_functions: &HashSet<String>,
        defined_modules: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        current_module: &mut Option<String>,
    ) {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                self.process_extract_calls_node(
                    child,
                    source,
                    defined_functions,
                    defined_modules,
                    calls_by_func,
                    current_module,
                );
            }
        }
    }

    fn process_extract_calls_node(
        &self,
        node: Node,
        source: &[u8],
        defined_functions: &HashSet<String>,
        defined_modules: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        current_module: &mut Option<String>,
    ) {
        if node.kind() == "unary_operator" {
            let caller = Self::module_caller_name(current_module);
            let calls = self.extract_calls_from_node(
                &node,
                source,
                defined_functions,
                defined_modules,
                &caller,
            );
            extend_calls_if_any(calls_by_func, caller, calls);
            return;
        }

        if node.kind() == "call"
            && self.process_call_dispatch(
                node,
                source,
                defined_functions,
                defined_modules,
                calls_by_func,
                current_module,
            )
        {
            return;
        }

        self.recurse_extract_calls_node(
            node,
            source,
            defined_functions,
            defined_modules,
            calls_by_func,
            current_module,
        );
    }

    fn process_call_dispatch(
        &self,
        node: Node,
        source: &[u8],
        defined_functions: &HashSet<String>,
        defined_modules: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        current_module: &mut Option<String>,
    ) -> bool {
        let mut call_name: Option<String> = None;
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "identifier" {
                    call_name = Some(get_node_text(&child, source).to_string());
                    break;
                }
            }
        }

        let Some(name) = call_name else {
            return false;
        };
        match name.as_str() {
            "defmodule" => {
                self.process_defmodule_call(
                    node,
                    source,
                    defined_functions,
                    defined_modules,
                    calls_by_func,
                    current_module,
                );
                true
            }
            "def" | "defp" => {
                self.process_function_definition_call(
                    node,
                    source,
                    defined_functions,
                    defined_modules,
                    calls_by_func,
                    current_module,
                );
                true
            }
            "alias" | "import" | "require" => true,
            _ => {
                let parent_is_module_level = node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "do_block" || parent.kind() == "source");
                if current_module.is_some() || parent_is_module_level {
                    let caller = Self::module_caller_name(current_module);
                    let dsl_calls = self.extract_calls_from_node(
                        &node,
                        source,
                        defined_functions,
                        defined_modules,
                        &caller,
                    );
                    extend_calls_if_any(calls_by_func, caller, dsl_calls);
                    return true;
                }
                false
            }
        }
    }

    fn process_defmodule_call(
        &self,
        node: Node,
        source: &[u8],
        defined_functions: &HashSet<String>,
        defined_modules: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        current_module: &mut Option<String>,
    ) {
        let old_module = current_module.take();
        *current_module = self.get_first_alias_arg(&node, source);

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "do_block" {
                    self.process_extract_calls_node(
                        child,
                        source,
                        defined_functions,
                        defined_modules,
                        calls_by_func,
                        current_module,
                    );
                }
            }
        }

        *current_module = old_module;
    }

    fn process_function_definition_call(
        &self,
        node: Node,
        source: &[u8],
        defined_functions: &HashSet<String>,
        defined_modules: &HashSet<String>,
        calls_by_func: &mut HashMap<String, Vec<CallSite>>,
        current_module: &Option<String>,
    ) {
        let Some(function_name) = self.get_function_name(&node, source) else {
            return;
        };
        let full_name = if let Some(module) = current_module {
            format!("{module}.{function_name}")
        } else {
            function_name.clone()
        };

        let mut all_calls =
            self.extract_def_arg_calls(&node, source, defined_functions, &full_name);
        let mut body: Option<Node> = None;
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "do_block" {
                    body = Some(child);
                    break;
                }
            }
        }
        if body.is_none() {
            for i in 0..node.child_count() {
                let Some(child) = node.child(i) else {
                    continue;
                };
                if child.kind() != "arguments" {
                    continue;
                }
                for j in 0..child.child_count() {
                    if let Some(arg) = child.child(j) {
                        if arg.kind() == "keywords" {
                            body = Some(arg);
                            break;
                        }
                    }
                }
            }
        }

        if let Some(body_node) = body {
            all_calls.extend(self.extract_calls_from_node(
                &body_node,
                source,
                defined_functions,
                defined_modules,
                &full_name,
            ));
        }

        if all_calls.is_empty() {
            return;
        }

        insert_calls_if_any(calls_by_func, full_name, all_calls.clone());
        if current_module.is_some() {
            insert_calls_if_any(calls_by_func, function_name, all_calls);
        }
    }

    fn is_defmodule_call(&self, node: &Node, source: &[u8]) -> bool {
        if node.kind() != "call" {
            return false;
        }
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "identifier" && get_node_text(&child, source) == "defmodule" {
                    return true;
                }
            }
        }
        false
    }

    fn collect_module_level_calls(
        &self,
        tree: &Tree,
        source: &[u8],
        defined_functions: &HashSet<String>,
        defined_modules: &HashSet<String>,
    ) -> Vec<CallSite> {
        let mut module_calls = Vec::new();
        for node in tree.root_node().children(&mut tree.root_node().walk()) {
            if self.is_defmodule_call(&node, source) {
                continue;
            }
            module_calls.extend(self.extract_calls_from_node(
                &node,
                source,
                defined_functions,
                defined_modules,
                "<module>",
            ));
        }
        module_calls
    }
}

impl CallGraphLanguageSupport for ElixirHandler {
    fn name(&self) -> &str {
        "elixir"
    }

    fn extensions(&self) -> &[&str] {
        &[".ex", ".exs"]
    }

    fn parse_imports(&self, source: &str, _path: &Path) -> Result<Vec<ImportDef>, ParseError> {
        let tree = self.parse_source(source)?;
        let source_bytes = source.as_bytes();
        let mut imports = Vec::new();

        for node in walk_tree(tree.root_node()) {
            if node.kind() == "call" {
                if let Some(imp) = self.parse_import_call(&node, source_bytes) {
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
        let (defined_functions, defined_modules) = self.collect_definitions(tree, source_bytes);
        let mut calls_by_func: HashMap<String, Vec<CallSite>> = HashMap::new();
        let mut current_module: Option<String> = None;
        self.process_extract_calls_node(
            tree.root_node(),
            source_bytes,
            &defined_functions,
            &defined_modules,
            &mut calls_by_func,
            &mut current_module,
        );

        let module_calls = self.collect_module_level_calls(
            tree,
            source_bytes,
            &defined_functions,
            &defined_modules,
        );
        insert_calls_if_any(&mut calls_by_func, "<module>".to_string(), module_calls);

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
            if node.kind() == "call" {
                let mut func_name: Option<String> = None;

                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i) {
                        if child.kind() == "identifier" {
                            func_name = Some(get_node_text(&child, source_bytes).to_string());
                            break;
                        }
                    }
                }

                if let Some(ref name) = func_name {
                    match name.as_str() {
                        "defmodule" => {
                            if let Some(mod_name) = self.get_first_alias_arg(&node, source_bytes) {
                                let line = node.start_position().row as u32 + 1;
                                let end_line = node.end_position().row as u32 + 1;
                                classes.push(ClassDef::simple(mod_name, line, end_line));
                            }
                        }
                        "def" | "defp" => {
                            if let Some(fn_name) = self.get_function_name(&node, source_bytes) {
                                let line = node.start_position().row as u32 + 1;
                                let end_line = node.end_position().row as u32 + 1;

                                // Check if inside a defmodule
                                let mut module_name = None;
                                let mut parent = node.parent();
                                while let Some(p) = parent {
                                    if p.kind() == "call" {
                                        let mut p_name: Option<String> = None;
                                        for i in 0..p.child_count() {
                                            if let Some(child) = p.child(i) {
                                                if child.kind() == "identifier" {
                                                    p_name = Some(
                                                        get_node_text(&child, source_bytes)
                                                            .to_string(),
                                                    );
                                                    break;
                                                }
                                            }
                                        }
                                        if p_name.as_deref() == Some("defmodule") {
                                            module_name =
                                                self.get_first_alias_arg(&p, source_bytes);
                                            break;
                                        }
                                    }
                                    parent = p.parent();
                                }

                                if let Some(mn) = module_name {
                                    funcs.push(FuncDef::method(fn_name, mn, line, end_line));
                                } else {
                                    funcs.push(FuncDef::function(fn_name, line, end_line));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok((funcs, classes))
    }
}

// =============================================================================
// Tests
// =============================================================================

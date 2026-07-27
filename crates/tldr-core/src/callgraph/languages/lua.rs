//! Lua language handler for call graph analysis.
//!
//! This module provides Lua-specific call graph support using tree-sitter-lua.
//!
//! # Import Patterns Supported
//!
//! | Pattern | ImportDef |
//! |---------|-----------|
//! | `require('module')` | `{module: "module", is_from: false}` |
//! | `require 'module'` | `{module: "module", is_from: false}` |
//! | `dofile('path.lua')` | `{module: "path.lua", is_from: false}` |
//! | `loadfile('path.lua')` | `{module: "path.lua", is_from: false}` |
//! | `local M = require('mod')` | `{module: "mod", alias: "M"}` |
//!
//! # Call Extraction
//!
//! - Direct calls: `func()` -> CallType::Direct or CallType::Intra
//! - Attribute calls: `module.func()` -> CallType::Attr (dot syntax)
//! - Method calls: `obj:method()` -> CallType::Method (colon syntax, self passed implicitly)
//!
//! # Lua-Specific Notes
//!
//! - Lua uses `require` for module imports (similar to Ruby)
//! - `dofile` and `loadfile` execute/load files by path
//! - Dot notation (`M.func`) is for table/module access
//! - Colon notation (`obj:method`) passes self implicitly as first argument
//!
//! # Spec Reference
//!
//! See `migration/spec/callgraph-spec.md` Section 9.x for Lua-specific details.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use tree_sitter::{Node, Parser, Tree};

use super::base::{get_node_text, walk_tree};
use super::{CallGraphLanguageSupport, ParseError};
use crate::callgraph::cross_file_types::{CallSite, CallType, ClassDef, FuncDef, ImportDef};

// =============================================================================
// Lua Handler
// =============================================================================

/// Lua language handler using tree-sitter-lua.
///
/// Supports:
/// - Import parsing (require, dofile, loadfile)
/// - Call extraction (direct, attribute via dot, method via colon)
/// - Function definition tracking
/// - `<module>` synthetic function for module-level calls
#[derive(Debug, Default)]
pub struct LuaHandler;

impl LuaHandler {
    /// Creates a new LuaHandler.
    pub fn new() -> Self {
        Self
    }

    /// Parse the source code into a tree-sitter Tree.
    fn parse_source(&self, source: &str) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_lua::LANGUAGE.into())
            .map_err(|e| ParseError::ParseFailed {
                file: std::path::PathBuf::new(),
                message: format!("Failed to set Lua language: {}", e),
            })?;

        parser
            .parse(source, None)
            .ok_or_else(|| ParseError::ParseFailed {
                file: std::path::PathBuf::new(),
                message: "Parser returned None".to_string(),
            })
    }

    /// Extract string content from a Lua string node.
    ///
    /// Handles:
    /// - Double quoted: `"string"`
    /// - Single quoted: `'string'`
    /// - Long brackets: `[[string]]`
    fn extract_lua_string(&self, node: &Node, source: &[u8]) -> Option<String> {
        let text = get_node_text(node, source);

        // Strip quotes based on format
        if (text.starts_with('"') && text.ends_with('"') && text.len() >= 2)
            || (text.starts_with('\'') && text.ends_with('\'') && text.len() >= 2)
        {
            Some(text[1..text.len() - 1].to_string())
        } else if text.starts_with("[[") && text.ends_with("]]") && text.len() >= 4 {
            Some(text[2..text.len() - 2].to_string())
        } else {
            // Return as-is if no recognized quote format
            Some(text.to_string())
        }
    }

    /// Parse a require/dofile/loadfile call node.
    ///
    /// Returns (import_type, module_path) if this is an import call.
    fn parse_require_node(&self, node: &Node, source: &[u8]) -> Option<(String, String)> {
        // Lua import calls are function_call nodes
        // Structure varies by call style:
        // - require("module") -> function_call with identifier + arguments
        // - require "module"  -> function_call with identifier + string (no parens)

        let mut func_name: Option<String> = None;
        let mut module_path: Option<String> = None;

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                match child.kind() {
                    "identifier" => {
                        func_name = Some(get_node_text(&child, source).to_string());
                    }
                    "arguments" => {
                        // Find the first string argument
                        for j in 0..child.child_count() {
                            if let Some(arg) = child.child(j) {
                                if arg.kind() == "string" {
                                    module_path = self.extract_lua_string(&arg, source);
                                    break;
                                }
                            }
                        }
                    }
                    "string" => {
                        // Direct string argument (require "module" syntax)
                        module_path = self.extract_lua_string(&child, source);
                    }
                    _ => {}
                }
            }
        }

        let func = func_name?;
        let module = module_path?;

        // Only handle require, dofile, loadfile
        match func.as_str() {
            "require" | "dofile" | "loadfile" => Some((func, module)),
            _ => None,
        }
    }

    /// Collect all function definitions in the file.
    ///
    /// Tracks:
    /// - `function foo()` declarations
    /// - `function M.foo()` module function declarations
    /// - `function M:foo()` method declarations
    /// - `local foo = function()` variable declarations with function values
    fn collect_definitions(&self, tree: &Tree, source: &[u8]) -> HashSet<String> {
        let mut funcs = HashSet::new();

        for node in walk_tree(tree.root_node()) {
            match node.kind() {
                "function_declaration" => {
                    // Get function name from different patterns
                    for i in 0..node.child_count() {
                        if let Some(child) = node.child(i) {
                            match child.kind() {
                                "identifier" => {
                                    // Simple: function foo()
                                    funcs.insert(get_node_text(&child, source).to_string());
                                    break;
                                }
                                "dot_index_expression" => {
                                    // Module function: function M.foo()
                                    // Extract the last identifier (function name)
                                    if let Some(name) = self.extract_last_identifier(&child, source)
                                    {
                                        funcs.insert(name);
                                    }
                                    break;
                                }
                                "method_index_expression" => {
                                    // Method: function M:foo()
                                    // Extract the last identifier (method name)
                                    if let Some(name) = self.extract_last_identifier(&child, source)
                                    {
                                        funcs.insert(name);
                                    }
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                }
                "variable_declaration" => {
                    // Handle: local foo = function() ... end
                    self.collect_function_from_variable_decl(&node, source, &mut funcs);
                }
                "assignment_statement" => {
                    // Handle: handler = function() ... end
                    //         MyModule.func = function() ... end
                    if let Some((name, _qualified, _body)) =
                        self.get_func_from_assignment(&node, source)
                    {
                        funcs.insert(name);
                    }
                }
                _ => {}
            }
        }

        funcs
    }

    /// Extract the last identifier from a dot or method index expression.
    fn extract_last_identifier(&self, node: &Node, source: &[u8]) -> Option<String> {
        let mut last_ident: Option<String> = None;

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "identifier" {
                    last_ident = Some(get_node_text(&child, source).to_string());
                }
            }
        }

        last_ident
    }

    /// Collect function name from variable declaration with function value.
    fn collect_function_from_variable_decl(
        &self,
        node: &Node,
        source: &[u8],
        funcs: &mut HashSet<String>,
    ) {
        // Structure: variable_declaration -> assignment_statement -> variable_list + expression_list
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "assignment_statement" {
                    let mut var_name: Option<String> = None;
                    let mut has_function = false;

                    for j in 0..child.child_count() {
                        if let Some(subchild) = child.child(j) {
                            match subchild.kind() {
                                "variable_list" => {
                                    // Get first identifier
                                    for k in 0..subchild.child_count() {
                                        if let Some(var) = subchild.child(k) {
                                            if var.kind() == "identifier" {
                                                var_name =
                                                    Some(get_node_text(&var, source).to_string());
                                                break;
                                            }
                                        }
                                    }
                                }
                                "expression_list" => {
                                    // Check if any expression is a function_definition
                                    for k in 0..subchild.child_count() {
                                        if let Some(expr) = subchild.child(k) {
                                            if expr.kind() == "function_definition" {
                                                has_function = true;
                                                break;
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }

                    if let (Some(name), true) = (var_name, has_function) {
                        funcs.insert(name);
                    }
                }
            }
        }
    }

    /// Extract calls from a node, recursively.
    ///
    /// Also detects function references: identifiers that match defined functions
    /// and are used as arguments (callbacks), e.g. `table.sort(list, compare)`.
    fn extract_calls_from_node(
        &self,
        node: &Node,
        source: &[u8],
        defined_funcs: &HashSet<String>,
        caller: &str,
    ) -> Vec<CallSite> {
        let mut calls = Vec::new();
        let mut refs = HashSet::new();

        for child in walk_tree(*node) {
            match child.kind() {
                "function_call" => {
                    if let Some(call_site) =
                        self.parse_function_call(&child, source, defined_funcs, caller)
                    {
                        calls.push(call_site);
                    }
                }
                "identifier" => {
                    // Check for function references (identifiers passed as arguments)
                    let name = get_node_text(&child, source);
                    if defined_funcs.contains(name) {
                        // Only count as Ref if the identifier is inside an arguments node
                        // and is NOT the function being called
                        if let Some(parent) = child.parent() {
                            if parent.kind() == "arguments" {
                                refs.insert(name.to_string());
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Add function references as Ref call sites
        for ref_name in refs {
            let line = node.start_position().row as u32 + 1;
            calls.push(CallSite::new(
                caller.to_string(),
                ref_name,
                CallType::Ref,
                Some(line),
                None,
                None,
                None,
            ));
        }

        calls
    }

    /// Parse a function_call node and create a CallSite.
    fn parse_function_call(
        &self,
        node: &Node,
        source: &[u8],
        defined_funcs: &HashSet<String>,
        caller: &str,
    ) -> Option<CallSite> {
        let line = node.start_position().row as u32 + 1;

        // Check each child to determine call type
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                match child.kind() {
                    "identifier" => {
                        // Simple call: foo()
                        let target = get_node_text(&child, source).to_string();

                        // Skip import-related calls
                        if target == "require" || target == "dofile" || target == "loadfile" {
                            return None;
                        }

                        let call_type = if defined_funcs.contains(&target) {
                            CallType::Intra
                        } else {
                            CallType::Direct
                        };

                        return Some(CallSite::new(
                            caller.to_string(),
                            target,
                            call_type,
                            Some(line),
                            None,
                            None,
                            None,
                        ));
                    }
                    "dot_index_expression" => {
                        // Attribute call: module.func() or obj.method()
                        return self.parse_dot_call(&child, source, caller, line);
                    }
                    "method_index_expression" => {
                        // Method call: obj:method()
                        return self.parse_colon_call(&child, source, caller, line);
                    }
                    _ => {}
                }
            }
        }

        None
    }

    /// Parse a dot-syntax call (module.func or obj.method).
    ///
    /// Handles both simple calls (`module.func()`) and chained calls
    /// (`a.b().c()`) where the receiver is a function_call node.
    fn parse_dot_call(
        &self,
        node: &Node,
        source: &[u8],
        caller: &str,
        line: u32,
    ) -> Option<CallSite> {
        let mut identifiers = Vec::new();
        let mut has_non_ident_receiver = false;
        let mut non_ident_receiver_text: Option<String> = None;

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                match child.kind() {
                    "identifier" => {
                        identifiers.push(get_node_text(&child, source).to_string());
                    }
                    "function_call" | "method_index_expression" | "dot_index_expression" => {
                        // Chained call: receiver is a call expression
                        has_non_ident_receiver = true;
                        non_ident_receiver_text = Some(get_node_text(&child, source).to_string());
                    }
                    _ => {}
                }
            }
        }

        if identifiers.len() >= 2 {
            let receiver = identifiers[0].clone();
            let method = identifiers.last().unwrap().clone();
            let target = format!("{}.{}", receiver, method);

            Some(CallSite::new(
                caller.to_string(),
                target,
                CallType::Attr,
                Some(line),
                None,
                Some(receiver),
                None,
            ))
        } else if has_non_ident_receiver && identifiers.len() == 1 {
            // Chained call: something().method
            let method = identifiers[0].clone();
            let receiver_text = non_ident_receiver_text.unwrap_or_default();
            let target = format!("{}.{}", receiver_text, method);

            Some(CallSite::new(
                caller.to_string(),
                target,
                CallType::Attr,
                Some(line),
                None,
                Some(receiver_text),
                None,
            ))
        } else if identifiers.len() == 1 {
            // Single identifier - treat as the full expression
            let target = get_node_text(node, source).to_string();
            let receiver = identifiers[0].clone();

            Some(CallSite::new(
                caller.to_string(),
                target,
                CallType::Attr,
                Some(line),
                None,
                Some(receiver),
                None,
            ))
        } else {
            None
        }
    }

    /// Parse a colon-syntax call (obj:method).
    ///
    /// Handles both simple calls (`obj:method()`) and chained calls
    /// (`obj:method1():method2()`) where the receiver is a function_call node.
    fn parse_colon_call(
        &self,
        node: &Node,
        source: &[u8],
        caller: &str,
        line: u32,
    ) -> Option<CallSite> {
        let mut identifiers = Vec::new();
        let mut has_non_ident_receiver = false;
        let mut non_ident_receiver_text: Option<String> = None;

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                match child.kind() {
                    "identifier" => {
                        identifiers.push(get_node_text(&child, source).to_string());
                    }
                    "function_call" | "method_index_expression" | "dot_index_expression" => {
                        // Chained call: receiver is a call expression, not a simple identifier
                        has_non_ident_receiver = true;
                        non_ident_receiver_text = Some(get_node_text(&child, source).to_string());
                    }
                    _ => {}
                }
            }
        }

        if identifiers.len() >= 2 {
            // Simple case: obj:method
            let receiver = identifiers[0].clone();
            let method = identifiers.last().unwrap().clone();
            let target = format!("{}:{}", receiver, method);

            Some(CallSite::new(
                caller.to_string(),
                target,
                CallType::Method,
                Some(line),
                None,
                Some(receiver),
                None,
            ))
        } else if has_non_ident_receiver && identifiers.len() == 1 {
            // Chained call: something():method
            // The method name is the single identifier we found
            let method = identifiers[0].clone();
            let receiver_text = non_ident_receiver_text.unwrap_or_default();
            let target = format!("{}:{}", receiver_text, method);

            Some(CallSite::new(
                caller.to_string(),
                target,
                CallType::Method,
                Some(line),
                None,
                Some(receiver_text),
                None,
            ))
        } else {
            None
        }
    }
}

impl CallGraphLanguageSupport for LuaHandler {
    fn name(&self) -> &str {
        "lua"
    }

    fn extensions(&self) -> &[&str] {
        &[".lua"]
    }

    fn parse_imports(&self, source: &str, _path: &Path) -> Result<Vec<ImportDef>, ParseError> {
        let tree = self.parse_source(source)?;
        let source_bytes = source.as_bytes();
        let mut imports = Vec::new();

        // Track which function_call nodes are inside variable_declarations
        // so we don't process them twice
        let mut processed_calls = HashSet::new();

        // First pass: process variable_declarations with aliased requires
        for node in walk_tree(tree.root_node()) {
            if node.kind() == "variable_declaration" {
                // Check if this declares an alias for a require
                if let Some((alias, import_info, call_id)) =
                    self.extract_aliased_require(&node, source_bytes)
                {
                    let mut import_def = ImportDef::simple_import(import_info.1);
                    import_def.alias = Some(alias);
                    imports.push(import_def);
                    processed_calls.insert(call_id);
                }
            }
        }

        // Second pass: process standalone require calls (not in variable_declarations)
        for node in walk_tree(tree.root_node()) {
            if node.kind() == "function_call" {
                let call_id = node.id();
                if !processed_calls.contains(&call_id) {
                    if let Some((_, module_path)) = self.parse_require_node(&node, source_bytes) {
                        let import_def = ImportDef::simple_import(module_path);
                        imports.push(import_def);
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

        // Process function declarations
        for node in walk_tree(tree.root_node()) {
            if node.kind() == "function_declaration" {
                if let Some((simple_name, qualified_name, body)) =
                    self.get_function_name_and_body(&node, source_bytes)
                {
                    let calls = self.extract_calls_from_node(
                        &body,
                        source_bytes,
                        &defined_funcs,
                        &qualified_name,
                    );
                    if !calls.is_empty() {
                        // Store with qualified name for cross-scope tracking
                        calls_by_func.insert(qualified_name.clone(), calls.clone());
                        // Also store with simple name for backward compatibility
                        if simple_name != qualified_name {
                            calls_by_func.insert(simple_name, calls);
                        }
                    }
                }
            }
        }

        // Process variable declarations with function values
        for node in walk_tree(tree.root_node()) {
            if node.kind() == "variable_declaration" {
                if let Some((simple_name, qualified_name, body)) =
                    self.get_func_from_var_decl(&node, source_bytes)
                {
                    let calls = self.extract_calls_from_node(
                        &body,
                        source_bytes,
                        &defined_funcs,
                        &qualified_name,
                    );
                    if !calls.is_empty() {
                        // Store with qualified name for cross-scope tracking
                        calls_by_func.insert(qualified_name.clone(), calls.clone());
                        // Also store with simple name for backward compatibility
                        if simple_name != qualified_name {
                            calls_by_func.insert(simple_name, calls);
                        }
                    }
                }
            }
        }

        // Process top-level assignment_statements with function values
        // e.g., MyModule.func = function() ... end
        //        handler = function() ... end
        for node in tree.root_node().children(&mut tree.root_node().walk()) {
            if node.kind() == "assignment_statement" {
                if let Some((simple_name, qualified_name, body)) =
                    self.get_func_from_assignment(&node, source_bytes)
                {
                    let calls = self.extract_calls_from_node(
                        &body,
                        source_bytes,
                        &defined_funcs,
                        &qualified_name,
                    );
                    if !calls.is_empty() {
                        // Store with qualified name for cross-scope tracking
                        calls_by_func.insert(qualified_name.clone(), calls.clone());
                        // Also store with simple name for backward compatibility
                        if simple_name != qualified_name {
                            calls_by_func.insert(simple_name, calls);
                        }
                    }
                }
            }
        }

        // Extract module-level calls into synthetic <module> function
        let mut module_calls = Vec::new();
        for node in tree.root_node().children(&mut tree.root_node().walk()) {
            // Skip function declarations and variable declarations with functions
            if node.kind() == "function_declaration" {
                continue;
            }
            if node.kind() == "variable_declaration" {
                // Check if this is a function definition
                if self.get_func_from_var_decl(&node, source_bytes).is_some() {
                    continue;
                }
            }
            // Skip assignment_statements with function values
            if node.kind() == "assignment_statement"
                && self.get_func_from_assignment(&node, source_bytes).is_some()
            {
                continue;
            }

            let calls =
                self.extract_calls_from_node(&node, source_bytes, &defined_funcs, "<module>");
            module_calls.extend(calls);
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
        // Lua has no classes, only return funcs

        for node in walk_tree(tree.root_node()) {
            match node.kind() {
                "function_declaration" => {
                    let line = node.start_position().row as u32 + 1;
                    let end_line = node.end_position().row as u32 + 1;

                    for i in 0..node.child_count() {
                        if let Some(child) = node.child(i) {
                            match child.kind() {
                                "identifier" => {
                                    let name = get_node_text(&child, source_bytes).to_string();
                                    funcs.push(FuncDef::function(name, line, end_line));
                                    break;
                                }
                                "dot_index_expression" => {
                                    if let Some(name) =
                                        self.extract_last_identifier(&child, source_bytes)
                                    {
                                        funcs.push(FuncDef::function(name, line, end_line));
                                    }
                                    break;
                                }
                                "method_index_expression" => {
                                    if let Some(name) =
                                        self.extract_last_identifier(&child, source_bytes)
                                    {
                                        funcs.push(FuncDef::function(name, line, end_line));
                                    }
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                }
                "variable_declaration" => {
                    // Handle: local foo = function() ... end
                    let mut var_names: Vec<String> = Vec::new();
                    let mut has_function = false;
                    let line = node.start_position().row as u32 + 1;
                    let end_line = node.end_position().row as u32 + 1;

                    for i in 0..node.child_count() {
                        if let Some(child) = node.child(i) {
                            if child.kind() == "assignment_statement" {
                                for j in 0..child.child_count() {
                                    if let Some(subchild) = child.child(j) {
                                        if subchild.kind() == "variable_list" {
                                            for k in 0..subchild.child_count() {
                                                if let Some(var) = subchild.child(k) {
                                                    if var.kind() == "identifier" {
                                                        var_names.push(
                                                            get_node_text(&var, source_bytes)
                                                                .to_string(),
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                        if subchild.kind() == "expression_list" {
                                            for k in 0..subchild.child_count() {
                                                if let Some(expr) = subchild.child(k) {
                                                    if expr.kind() == "function_definition" {
                                                        has_function = true;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if has_function {
                        for name in var_names {
                            funcs.push(FuncDef::function(name, line, end_line));
                        }
                    }
                }
                "assignment_statement" => {
                    // Handle: handler = function() ... end
                    //         MyModule.func = function() ... end
                    if let Some((name, _qualified, _body)) =
                        self.get_func_from_assignment(&node, source_bytes)
                    {
                        let line = node.start_position().row as u32 + 1;
                        let end_line = node.end_position().row as u32 + 1;
                        funcs.push(FuncDef::function(name, line, end_line));
                    }
                }
                _ => {}
            }
        }

        Ok((funcs, Vec::new()))
    }
}

impl LuaHandler {
    /// Extract an aliased require from a variable declaration.
    ///
    /// Returns (alias, (import_type, module_path), call_node_id) if found.
    fn extract_aliased_require(
        &self,
        node: &Node,
        source: &[u8],
    ) -> Option<(String, (String, String), usize)> {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "assignment_statement" {
                    let mut var_name: Option<String> = None;
                    let mut require_info: Option<((String, String), usize)> = None;

                    for j in 0..child.child_count() {
                        if let Some(subchild) = child.child(j) {
                            match subchild.kind() {
                                "variable_list" => {
                                    // Get first identifier as variable name
                                    for k in 0..subchild.child_count() {
                                        if let Some(var) = subchild.child(k) {
                                            if var.kind() == "identifier" {
                                                var_name =
                                                    Some(get_node_text(&var, source).to_string());
                                                break;
                                            }
                                        }
                                    }
                                }
                                "expression_list" => {
                                    // Look for require call in expression directly
                                    for inner in walk_tree(subchild) {
                                        if inner.kind() == "function_call" {
                                            if let Some((import_type, module_path)) =
                                                self.parse_require_node(&inner, source)
                                            {
                                                require_info =
                                                    Some(((import_type, module_path), inner.id()));
                                                break;
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }

                    // If we found both a variable name and a require, return them
                    if let (Some(alias), Some((import_info, call_id))) = (var_name, require_info) {
                        return Some((alias, import_info, call_id));
                    }
                }
            }
        }
        None
    }

    /// Get function name and body from a function declaration.
    /// Returns (simple_name, qualified_name, body) where qualified_name includes table prefix.
    fn get_function_name_and_body<'a>(
        &self,
        node: &'a Node,
        source: &[u8],
    ) -> Option<(String, String, Node<'a>)> {
        let mut simple_name: Option<String> = None;
        let mut qualified_name: Option<String> = None;
        let mut body: Option<Node<'a>> = None;

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                match child.kind() {
                    "identifier" => {
                        let name = get_node_text(&child, source).to_string();
                        simple_name = Some(name.clone());
                        qualified_name = Some(name);
                    }
                    "dot_index_expression" => {
                        // Table function: M.func or MyModule.sub.func
                        // Extract both simple name and qualified name
                        if let Some(name) = self.extract_last_identifier(&child, source) {
                            simple_name = Some(name);
                        }
                        // Get full qualified name like "M.func"
                        let full_text = get_node_text(&child, source).to_string();
                        qualified_name = Some(full_text);
                    }
                    "method_index_expression" => {
                        // Method: M:method
                        if let Some(name) = self.extract_last_identifier(&child, source) {
                            simple_name = Some(name);
                        }
                        // Get full qualified name like "M:method"
                        let full_text = get_node_text(&child, source).to_string();
                        qualified_name = Some(full_text);
                    }
                    "block" => {
                        body = Some(child);
                    }
                    _ => {}
                }
            }
        }

        if let (Some(simple), Some(qualified), Some(b)) = (simple_name, qualified_name, body) {
            Some((simple, qualified, b))
        } else {
            None
        }
    }

    /// Check if a function name represents a table method (contains . or :).
    /// Returns the table name if it's a table method, None otherwise.
    fn _get_table_prefix(&self, func_name: &str) -> Option<String> {
        if func_name.contains('.') {
            func_name.split('.').next().map(|s| s.to_string())
        } else if func_name.contains(':') {
            func_name.split(':').next().map(|s| s.to_string())
        } else {
            None
        }
    }

    /// Get function name and body from a variable declaration with function value.
    /// Returns (simple_name, qualified_name, body) where qualified_name includes table prefix.
    fn get_func_from_var_decl<'a>(
        &self,
        node: &'a Node,
        source: &[u8],
    ) -> Option<(String, String, Node<'a>)> {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "assignment_statement" {
                    let mut simple_name: Option<String> = None;
                    let mut qualified_name: Option<String> = None;
                    let mut func_body: Option<Node<'a>> = None;

                    for j in 0..child.child_count() {
                        if let Some(subchild) = child.child(j) {
                            match subchild.kind() {
                                "variable_list" => {
                                    for k in 0..subchild.child_count() {
                                        if let Some(var) = subchild.child(k) {
                                            match var.kind() {
                                                "identifier" => {
                                                    let name =
                                                        get_node_text(&var, source).to_string();
                                                    simple_name = Some(name.clone());
                                                    qualified_name = Some(name);
                                                    break;
                                                }
                                                "dot_index_expression" => {
                                                    // M.func = function() ... end
                                                    if let Some(name) =
                                                        self.extract_last_identifier(&var, source)
                                                    {
                                                        simple_name = Some(name);
                                                    }
                                                    qualified_name = Some(
                                                        get_node_text(&var, source).to_string(),
                                                    );
                                                    break;
                                                }
                                                "method_index_expression" => {
                                                    // M:method = function() ... end
                                                    if let Some(name) =
                                                        self.extract_last_identifier(&var, source)
                                                    {
                                                        simple_name = Some(name);
                                                    }
                                                    qualified_name = Some(
                                                        get_node_text(&var, source).to_string(),
                                                    );
                                                    break;
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                }
                                "expression_list" => {
                                    for k in 0..subchild.child_count() {
                                        if let Some(expr) = subchild.child(k) {
                                            if expr.kind() == "function_definition" {
                                                // Get the body (block) from function_definition
                                                for l in 0..expr.child_count() {
                                                    if let Some(part) = expr.child(l) {
                                                        if part.kind() == "block" {
                                                            func_body = Some(part);
                                                            break;
                                                        }
                                                    }
                                                }
                                                break;
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }

                    if let (Some(simple), Some(qualified), Some(body)) =
                        (simple_name, qualified_name, func_body)
                    {
                        return Some((simple, qualified, body));
                    }
                }
            }
        }
        None
    }

    /// Get function name and body from a top-level assignment_statement with function value.
    ///
    /// Handles patterns like:
    /// - `handler = function() ... end`
    /// - `MyModule.func = function() ... end`
    ///
    /// These are NOT wrapped in `variable_declaration` (no `local` keyword).
    /// Returns (simple_name, qualified_name, body) where qualified_name includes table prefix.
    fn get_func_from_assignment<'a>(
        &self,
        node: &'a Node,
        source: &[u8],
    ) -> Option<(String, String, Node<'a>)> {
        if node.kind() != "assignment_statement" {
            return None;
        }

        let mut simple_name: Option<String> = None;
        let mut qualified_name: Option<String> = None;
        let mut func_body: Option<Node<'a>> = None;

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                match child.kind() {
                    "variable_list" => {
                        // Try to extract name from first variable
                        for k in 0..child.child_count() {
                            if let Some(var) = child.child(k) {
                                match var.kind() {
                                    "identifier" => {
                                        // Simple: handler = function() end
                                        let name = get_node_text(&var, source).to_string();
                                        simple_name = Some(name.clone());
                                        qualified_name = Some(name);
                                        break;
                                    }
                                    "dot_index_expression" => {
                                        // Dotted: MyModule.func = function() end
                                        // Use the last identifier as the simple function name
                                        if let Some(name) =
                                            self.extract_last_identifier(&var, source)
                                        {
                                            simple_name = Some(name);
                                        }
                                        // Get full qualified name
                                        qualified_name =
                                            Some(get_node_text(&var, source).to_string());
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    "expression_list" => {
                        for k in 0..child.child_count() {
                            if let Some(expr) = child.child(k) {
                                if expr.kind() == "function_definition" {
                                    // Get the body (block) from function_definition
                                    for l in 0..expr.child_count() {
                                        if let Some(part) = expr.child(l) {
                                            if part.kind() == "block" {
                                                func_body = Some(part);
                                                break;
                                            }
                                        }
                                    }
                                    break;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        if let (Some(simple), Some(qualified), Some(body)) =
            (simple_name, qualified_name, func_body)
        {
            Some((simple, qualified, body))
        } else {
            None
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

//! Python language handler for call graph analysis.
//!
//! This module provides Python-specific call graph support using tree-sitter-python.
//!
//! # Import Patterns Supported
//!
//! | Pattern | ImportDef |
//! |---------|-----------|
//! | `import os` | `{module: "os", is_from: false}` |
//! | `import os as o` | `{module: "os", alias: "o"}` |
//! | `from os import path` | `{module: "os", is_from: true, names: ["path"]}` |
//! | `from os import path as p` | `{module: "os", names: ["path"], aliases: {"p": "path"}}` |
//! | `from . import types` | `{module: "", is_from: true, names: ["types"], level: 1}` |
//! | `from ..utils import helper` | `{module: "utils", names: ["helper"], level: 2}` |
//! | `from pkg import *` | `{module: "pkg", names: ["*"]}` |
//!
//! # Call Extraction
//!
//! - Direct calls: `func()` -> CallType::Direct or CallType::Intra
//! - Attribute calls: `obj.method()` -> CallType::Attr
//! - Function references: `map(func, ...)` -> CallType::Ref
//!
//! # Spec Reference
//!
//! See `migration/spec/callgraph-spec.md` Section 9.1 for Python-specific details.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use tree_sitter::{Node, Parser, Tree};

use super::base::{get_node_text, walk_tree};
use super::{CallGraphLanguageSupport, ParseError};
use crate::callgraph::cross_file_types::{CallSite, CallType, ClassDef, FuncDef, ImportDef};

// =============================================================================
// Python Handler
// =============================================================================

/// Python language handler using tree-sitter-python.
///
/// Supports:
/// - Import parsing (all Python import styles including relative imports)
/// - Call extraction (direct, method, attribute, references)
/// - TYPE_CHECKING block detection
/// - Nested function tracking via parent_function
/// - `<module>` synthetic function for module-level calls
#[derive(Debug, Default)]
pub struct PythonHandler;

impl PythonHandler {
    /// Creates a new PythonHandler.
    pub fn new() -> Self {
        Self
    }

    /// Parse the source code into a tree-sitter Tree.
    fn parse_source(&self, source: &str) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .map_err(|e| ParseError::ParseFailed {
                file: std::path::PathBuf::new(),
                message: format!("Failed to set Python language: {}", e),
            })?;

        parser
            .parse(source, None)
            .ok_or_else(|| ParseError::ParseFailed {
                file: std::path::PathBuf::new(),
                message: "Parser returned None".to_string(),
            })
    }

    /// Check if a node is inside a TYPE_CHECKING block.
    fn is_in_type_checking_block(&self, node: &Node, source: &[u8]) -> bool {
        let mut current = node.parent();
        while let Some(parent) = current {
            if parent.kind() == "if_statement" {
                // Check if the condition is TYPE_CHECKING
                if let Some(condition) = parent.child_by_field_name("condition") {
                    let cond_text = get_node_text(&condition, source);
                    if cond_text == "TYPE_CHECKING"
                        || cond_text == "typing.TYPE_CHECKING"
                        || cond_text.ends_with(".TYPE_CHECKING")
                    {
                        return true;
                    }
                }
            }
            current = parent.parent();
        }
        false
    }

    /// Parse a single import statement node.
    fn parse_import_statement(&self, node: &Node, source: &[u8]) -> Vec<ImportDef> {
        let mut imports = Vec::new();

        match node.kind() {
            "import_statement" => {
                // import X, import X as Y
                for i in 0..node.named_child_count() {
                    if let Some(child) = node.named_child(i) {
                        match child.kind() {
                            "dotted_name" => {
                                let module = get_node_text(&child, source).to_string();
                                imports.push(ImportDef::simple_import(module));
                            }
                            "aliased_import" => {
                                // import X as Y
                                let mut module = String::new();
                                let mut alias = None;
                                for j in 0..child.named_child_count() {
                                    if let Some(gc) = child.named_child(j) {
                                        match gc.kind() {
                                            "dotted_name" => {
                                                module = get_node_text(&gc, source).to_string();
                                            }
                                            "identifier" => {
                                                alias =
                                                    Some(get_node_text(&gc, source).to_string());
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                if !module.is_empty() {
                                    let mut imp = ImportDef::simple_import(module);
                                    imp.alias = alias;
                                    imports.push(imp);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            "import_from_statement" => {
                // from X import Y, from . import Y, from ..X import Y
                let mut module = String::new();
                let mut level: u8 = 0;
                let mut names = Vec::new();
                let mut aliases: HashMap<String, String> = HashMap::new();
                let mut is_wildcard = false;

                // Handle relative imports
                // tree-sitter-python uses a "relative_import" node containing dots and module
                // e.g., "from . import X" has relative_import="."
                // e.g., "from ..utils import X" has relative_import="..utils"
                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i) {
                        if child.kind() == "relative_import" {
                            let text = get_node_text(&child, source);
                            // Count leading dots
                            for c in text.chars() {
                                if c == '.' {
                                    level += 1;
                                } else {
                                    break;
                                }
                            }
                            // Extract module name (part after dots)
                            let module_part: String =
                                text.chars().skip_while(|&c| c == '.').collect();
                            if !module_part.is_empty() {
                                module = module_part;
                            }
                            break;
                        }
                    }
                }

                // For non-relative imports, get module name from module_name field
                if level == 0 {
                    if let Some(module_node) = node.child_by_field_name("module_name") {
                        module = get_node_text(&module_node, source).to_string();
                    }
                }

                // Parse imported names
                for i in 0..node.named_child_count() {
                    if let Some(child) = node.named_child(i) {
                        match child.kind() {
                            "dotted_name" | "identifier" => {
                                // Skip the module name itself
                                let text = get_node_text(&child, source);
                                if text != module && !text.is_empty() {
                                    names.push(text.to_string());
                                }
                            }
                            "aliased_import" => {
                                // from X import Y as Z
                                let mut orig_name = String::new();
                                let mut alias_name = None;
                                for j in 0..child.named_child_count() {
                                    if let Some(gc) = child.named_child(j) {
                                        match gc.kind() {
                                            "dotted_name" | "identifier" => {
                                                if orig_name.is_empty() {
                                                    orig_name =
                                                        get_node_text(&gc, source).to_string();
                                                } else {
                                                    alias_name = Some(
                                                        get_node_text(&gc, source).to_string(),
                                                    );
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                if !orig_name.is_empty() {
                                    names.push(orig_name.clone());
                                    if let Some(alias) = alias_name {
                                        aliases.insert(alias, orig_name);
                                    }
                                }
                            }
                            "wildcard_import" => {
                                is_wildcard = true;
                            }
                            _ => {}
                        }
                    }
                }

                if is_wildcard {
                    names = vec!["*".to_string()];
                }

                // Create the ImportDef
                let mut imp = if level > 0 {
                    ImportDef::relative_import(module, names, level)
                } else {
                    ImportDef::from_import(module, names)
                };

                if !aliases.is_empty() {
                    imp.aliases = Some(aliases);
                }

                // Check if inside TYPE_CHECKING block
                imp.is_type_checking = self.is_in_type_checking_block(node, source);

                imports.push(imp);
            }
            _ => {}
        }

        imports
    }

    /// Collect all function and class names defined in the file.
    fn collect_definitions(
        &self,
        tree: &Tree,
        source: &[u8],
    ) -> (HashSet<String>, HashSet<String>) {
        let mut functions = HashSet::new();
        let mut classes = HashSet::new();

        for node in walk_tree(tree.root_node()) {
            match node.kind() {
                "function_definition" => {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        functions.insert(get_node_text(&name_node, source).to_string());
                    }
                }
                "class_definition" => {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        classes.insert(get_node_text(&name_node, source).to_string());
                    }
                }
                _ => {}
            }
        }

        (functions, classes)
    }

    /// Extract calls from a function body node.
    fn extract_calls_from_node(
        &self,
        node: &Node,
        source: &[u8],
        defined_funcs: &HashSet<String>,
        defined_classes: &HashSet<String>,
        caller: &str,
        line_offset: u32,
    ) -> Vec<CallSite> {
        let mut calls = Vec::new();
        let mut refs = HashSet::new();

        // Walk the node tree
        for child in walk_tree(*node) {
            match child.kind() {
                "call" => {
                    // Get the function being called
                    if let Some(func_node) = child.child_by_field_name("function") {
                        let line = child.start_position().row as u32 + 1 + line_offset;

                        match func_node.kind() {
                            "identifier" => {
                                // Direct call: func()
                                let target = get_node_text(&func_node, source).to_string();
                                let call_type = if defined_funcs.contains(&target)
                                    || defined_classes.contains(&target)
                                {
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
                            "attribute" => {
                                // Attribute call: obj.method()
                                let full_target = get_node_text(&func_node, source).to_string();
                                // Extract receiver (obj) from obj.method
                                let receiver = if let Some(obj_node) =
                                    func_node.child_by_field_name("object")
                                {
                                    Some(get_node_text(&obj_node, source).to_string())
                                } else {
                                    // Fallback: split on first dot
                                    full_target.split('.').next().map(|s| s.to_string())
                                };

                                calls.push(CallSite::new(
                                    caller.to_string(),
                                    full_target,
                                    CallType::Attr,
                                    Some(line),
                                    None,
                                    receiver,
                                    None,
                                ));
                            }
                            _ => {
                                // Other call patterns (subscript, etc.)
                                let target = get_node_text(&func_node, source).to_string();
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
                "identifier" => {
                    // Check for function references (not in calls, but used as values)
                    let name = get_node_text(&child, source);
                    if defined_funcs.contains(name) {
                        // Check if this identifier is NOT the function part of a call
                        if let Some(parent) = child.parent() {
                            if parent.kind() != "call"
                                && parent.child_by_field_name("function").as_ref() != Some(&child)
                            {
                                refs.insert(name.to_string());
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Add function references
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
}

impl CallGraphLanguageSupport for PythonHandler {
    fn name(&self) -> &str {
        "python"
    }

    fn extensions(&self) -> &[&str] {
        &[".py", ".pyi"]
    }

    fn parse_imports(&self, source: &str, _path: &Path) -> Result<Vec<ImportDef>, ParseError> {
        let tree = self.parse_source(source)?;
        let source_bytes = source.as_bytes();
        let mut imports = Vec::new();

        for node in walk_tree(tree.root_node()) {
            match node.kind() {
                "import_statement" | "import_from_statement" => {
                    imports.extend(self.parse_import_statement(&node, source_bytes));
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
        let (defined_funcs, defined_classes) = self.collect_definitions(tree, source_bytes);
        let mut calls_by_func: HashMap<String, Vec<CallSite>> = HashMap::new();

        // Extract calls from each function (includes default params and decorators)
        for node in walk_tree(tree.root_node()) {
            if node.kind() == "function_definition" {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let func_name = get_node_text(&name_node, source_bytes).to_string();

                    // FIX: Determine if this function is a method inside a class
                    // by walking up the parent chain to find the enclosing class.
                    // This ensures calls from ClassA.method and ClassB.method are
                    // recorded separately with qualified caller names.
                    let mut caller_name = func_name.clone();
                    let mut current = node.parent();
                    while let Some(parent) = current {
                        if parent.kind() == "block" {
                            if let Some(gp) = parent.parent() {
                                if gp.kind() == "class_definition" {
                                    if let Some(class_name_node) = gp.child_by_field_name("name") {
                                        let class_name =
                                            get_node_text(&class_name_node, source_bytes);
                                        caller_name = format!("{}.{}", class_name, func_name);
                                    }
                                    break;
                                }
                            }
                        }
                        current = parent.parent();
                    }

                    let mut func_calls = Vec::new();

                    // Pattern 9: Extract calls from decorators
                    // In tree-sitter-python, decorated functions are wrapped in
                    // `decorated_definition` which has `decorator` + `function_definition`
                    // as siblings. The decorator is NOT a child of function_definition.
                    if let Some(parent) = node.parent() {
                        if parent.kind() == "decorated_definition" {
                            for i in 0..parent.child_count() {
                                if let Some(child) = parent.child(i) {
                                    if child.kind() == "decorator" {
                                        // Only extract actual calls from decorators
                                        // @app.route("/api") has a call node inside
                                        // @login_required does NOT (just identifier/attribute)
                                        let decorator_calls = self.extract_calls_from_node(
                                            &child,
                                            source_bytes,
                                            &defined_funcs,
                                            &defined_classes,
                                            &caller_name,
                                            0,
                                        );
                                        func_calls.extend(decorator_calls);
                                    }
                                }
                            }
                        }
                    }

                    // Pattern 6/7: Extract calls from default parameter values
                    if let Some(params_node) = node.child_by_field_name("parameters") {
                        let param_calls = self.extract_calls_from_node(
                            &params_node,
                            source_bytes,
                            &defined_funcs,
                            &defined_classes,
                            &caller_name,
                            0,
                        );
                        func_calls.extend(param_calls);
                    }

                    // Extract calls from the function body (existing behavior)
                    if let Some(body_node) = node.child_by_field_name("body") {
                        let calls = self.extract_calls_from_node(
                            &body_node,
                            source_bytes,
                            &defined_funcs,
                            &defined_classes,
                            &caller_name,
                            0,
                        );
                        func_calls.extend(calls);
                    }

                    if !func_calls.is_empty() {
                        calls_by_func
                            .entry(caller_name)
                            .or_default()
                            .extend(func_calls);
                    }
                }
            }
        }

        // Pattern 3/21: Extract calls from class body field initializers
        for node in walk_tree(tree.root_node()) {
            if node.kind() == "class_definition" {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let class_name = get_node_text(&name_node, source_bytes).to_string();

                    if let Some(body) = node.child_by_field_name("body") {
                        let mut class_calls = Vec::new();
                        // Walk direct children of the class body (block node)
                        // Skip function_definition and class_definition (methods/nested classes)
                        for i in 0..body.named_child_count() {
                            if let Some(child) = body.named_child(i) {
                                if matches!(
                                    child.kind(),
                                    "function_definition"
                                        | "class_definition"
                                        | "decorated_definition"
                                ) {
                                    continue;
                                }
                                // Extract calls from class-level statements
                                // e.g., timeout = compute_timeout(), name = Column(String(50))
                                let calls = self.extract_calls_from_node(
                                    &child,
                                    source_bytes,
                                    &defined_funcs,
                                    &defined_classes,
                                    &class_name,
                                    0,
                                );
                                class_calls.extend(calls);
                            }
                        }
                        if !class_calls.is_empty() {
                            calls_by_func
                                .entry(class_name)
                                .or_default()
                                .extend(class_calls);
                        }
                    }
                }
            }
        }

        // Extract module-level calls into synthetic <module> function
        let mut module_calls = Vec::new();
        for node in tree.root_node().children(&mut tree.root_node().walk()) {
            // Skip function and class definitions
            if matches!(node.kind(), "function_definition" | "class_definition") {
                continue;
            }

            // Extract calls from this module-level statement
            let calls = self.extract_calls_from_node(
                &node,
                source_bytes,
                &defined_funcs,
                &defined_classes,
                "<module>",
                0,
            );
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
        let mut classes = Vec::new();

        for node in walk_tree(tree.root_node()) {
            match node.kind() {
                "function_definition" => {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        let name = get_node_text(&name_node, source_bytes).to_string();
                        let line = node.start_position().row as u32 + 1;
                        let end_line = node.end_position().row as u32 + 1;

                        // Check if inside a class
                        let mut class_name = None;
                        let mut parent = node.parent();
                        while let Some(p) = parent {
                            if p.kind() == "block" {
                                if let Some(gp) = p.parent() {
                                    if gp.kind() == "class_definition" {
                                        if let Some(cn) = gp.child_by_field_name("name") {
                                            class_name =
                                                Some(get_node_text(&cn, source_bytes).to_string());
                                        }
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
                "class_definition" => {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        let class_name = get_node_text(&name_node, source_bytes).to_string();
                        let line = node.start_position().row as u32 + 1;
                        let end_line = node.end_position().row as u32 + 1;

                        // Collect base classes from argument_list
                        let mut bases = Vec::new();
                        if let Some(args) = node.child_by_field_name("superclasses") {
                            for i in 0..args.child_count() {
                                if let Some(arg) = args.child(i) {
                                    if arg.kind() == "identifier" {
                                        bases.push(get_node_text(&arg, source_bytes).to_string());
                                    }
                                }
                            }
                        }

                        // Collect method names from the body
                        let mut methods = Vec::new();
                        if let Some(body) = node.child_by_field_name("body") {
                            for i in 0..body.named_child_count() {
                                if let Some(child) = body.named_child(i) {
                                    if child.kind() == "function_definition" {
                                        if let Some(fn_name) = child.child_by_field_name("name") {
                                            methods.push(
                                                get_node_text(&fn_name, source_bytes).to_string(),
                                            );
                                        }
                                    }
                                }
                            }
                        }

                        classes.push(ClassDef::new(class_name, line, end_line, methods, bases));
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

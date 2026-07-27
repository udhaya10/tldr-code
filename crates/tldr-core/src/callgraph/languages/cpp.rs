//! C++ language handler for call graph analysis.
//!
//! This module provides C++-specific call graph support using tree-sitter-cpp.
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
//! - Member calls: `obj.method()` / `ptr->method()` -> CallType::Attr
//! - Qualified calls: `ns::func()` / `Class::staticMethod()` -> CallType::Static
//! - Template calls: `func<T>()` -> target includes template args
//! - Constructor calls: indexed and tracked
//!
//! # C++ Specific Features
//!
//! - Namespace resolution: `ns::func()`
//! - Templates: `func<T>()`
//! - Constructors/destructors
//! - `ptr->method()` and `obj.method()`
//!
//! # Spec Reference
//!
//! See `migration/spec/callgraph-spec.md` Section 9.6 for C/C++-specific details.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use tree_sitter::{Node, Tree};

use super::base::{get_node_text, walk_tree};
use super::{CallGraphLanguageSupport, ParseError};
use crate::callgraph::cross_file_types::{CallSite, CallType, ClassDef, FuncDef, ImportDef};

// =============================================================================
// C++ Handler
// =============================================================================

/// C++ language handler using tree-sitter-cpp.
///
/// Extends C support with:
/// - Namespaces and qualified names
/// - Classes and methods
/// - Templates
/// - Constructors and destructors
#[derive(Debug, Default)]
pub struct CppHandler;

impl CppHandler {
    /// Creates a new CppHandler.
    pub fn new() -> Self {
        Self
    }

    /// Get the function/method name from a function_definition node.
    ///
    /// Handles:
    /// - Regular functions: `void foo() {}`
    /// - Class methods: `void method() {}` inside class
    /// - Pointer return types: `int* func()`
    /// - Qualified names: `void Class::method() {}`
    fn get_function_name(&self, node: &Node, source: &[u8]) -> Option<String> {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                match child.kind() {
                    "function_declarator" => {
                        // Look for identifier, field_identifier, or qualified_identifier
                        for j in 0..child.child_count() {
                            if let Some(dc) = child.child(j) {
                                match dc.kind() {
                                    "identifier" | "field_identifier" => {
                                        return Some(get_node_text(&dc, source).to_string());
                                    }
                                    "qualified_identifier" => {
                                        // Handle Class::method() or ns::func()
                                        return Some(get_node_text(&dc, source).to_string());
                                    }
                                    "destructor_name" => {
                                        // Handle ~ClassName()
                                        return Some(get_node_text(&dc, source).to_string());
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    "pointer_declarator" => {
                        // Pointer return type like int* func()
                        for j in 0..child.child_count() {
                            if let Some(pc) = child.child(j) {
                                if pc.kind() == "function_declarator" {
                                    for k in 0..pc.child_count() {
                                        if let Some(dc) = pc.child(k) {
                                            match dc.kind() {
                                                "identifier" | "field_identifier" => {
                                                    return Some(
                                                        get_node_text(&dc, source).to_string(),
                                                    );
                                                }
                                                "qualified_identifier" => {
                                                    return Some(
                                                        get_node_text(&dc, source).to_string(),
                                                    );
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    "reference_declarator" => {
                        // Reference return type like int& func()
                        for j in 0..child.child_count() {
                            if let Some(rc) = child.child(j) {
                                if rc.kind() == "function_declarator" {
                                    for k in 0..rc.child_count() {
                                        if let Some(dc) = rc.child(k) {
                                            match dc.kind() {
                                                "identifier" | "field_identifier" => {
                                                    return Some(
                                                        get_node_text(&dc, source).to_string(),
                                                    );
                                                }
                                                "qualified_identifier" => {
                                                    return Some(
                                                        get_node_text(&dc, source).to_string(),
                                                    );
                                                }
                                                _ => {}
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

    /// Collect all function, method, and class definitions.
    fn collect_definitions(
        &self,
        tree: &Tree,
        source: &[u8],
    ) -> (HashSet<String>, HashSet<String>) {
        let mut functions = HashSet::new();
        let mut classes = HashSet::new();

        struct Walker<'a> {
            functions: &'a mut HashSet<String>,
            classes: &'a mut HashSet<String>,
            current_class: Option<String>,
            current_namespace: Option<String>,
        }

        fn walk_node(walker: &mut Walker, node: Node, source: &[u8], handler: &CppHandler) {
            match node.kind() {
                "namespace_definition" => {
                    // Get namespace name
                    let mut ns_name = None;
                    for i in 0..node.child_count() {
                        if let Some(child) = node.child(i) {
                            if child.kind() == "namespace_identifier" {
                                ns_name = Some(get_node_text(&child, source).to_string());
                                break;
                            }
                        }
                    }

                    if let Some(ns) = ns_name {
                        let old_ns = walker.current_namespace.clone();
                        walker.current_namespace = Some(if let Some(ref old) = old_ns {
                            format!("{}::{}", old, ns)
                        } else {
                            ns
                        });

                        for i in 0..node.child_count() {
                            if let Some(child) = node.child(i) {
                                walk_node(walker, child, source, handler);
                            }
                        }

                        walker.current_namespace = old_ns;
                        return;
                    }
                }
                "class_specifier" | "struct_specifier" => {
                    // Get class/struct name
                    let mut class_name = None;
                    for i in 0..node.child_count() {
                        if let Some(child) = node.child(i) {
                            if child.kind() == "type_identifier" {
                                class_name = Some(get_node_text(&child, source).to_string());
                                break;
                            }
                        }
                    }

                    if let Some(name) = class_name {
                        // Add class to index
                        walker.classes.insert(name.clone());
                        if let Some(ref ns) = walker.current_namespace {
                            walker.classes.insert(format!("{}::{}", ns, name));
                        }

                        let old_class = walker.current_class.clone();
                        walker.current_class = Some(name);

                        for i in 0..node.child_count() {
                            if let Some(child) = node.child(i) {
                                walk_node(walker, child, source, handler);
                            }
                        }

                        walker.current_class = old_class;
                        return;
                    }
                }
                "function_definition" => {
                    if let Some(name) = handler.get_function_name(&node, source) {
                        // Add the base name
                        walker.functions.insert(name.clone());

                        // Add with class prefix if inside a class
                        if let Some(ref class) = walker.current_class {
                            walker.functions.insert(format!("{}::{}", class, name));
                        }

                        // Add with namespace prefix
                        if let Some(ref ns) = walker.current_namespace {
                            walker.functions.insert(format!("{}::{}", ns, name));
                            if let Some(ref class) = walker.current_class {
                                walker
                                    .functions
                                    .insert(format!("{}::{}::{}", ns, class, name));
                            }
                        }
                    }
                }
                _ => {}
            }

            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    walk_node(walker, child, source, handler);
                }
            }
        }

        let mut walker = Walker {
            functions: &mut functions,
            classes: &mut classes,
            current_class: None,
            current_namespace: None,
        };

        walk_node(&mut walker, tree.root_node(), source, self);

        (functions, classes)
    }

    /// Extract calls from a function body.
    fn extract_calls_from_func(
        &self,
        node: &Node,
        source: &[u8],
        defined_funcs: &HashSet<String>,
        _defined_classes: &HashSet<String>,
        caller: &str,
    ) -> Vec<CallSite> {
        let mut calls = Vec::new();

        for child in walk_tree(*node) {
            if child.kind() == "call_expression" {
                let line = child.start_position().row as u32 + 1;

                // Get the callee (what's being called)
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
                        "field_expression" => {
                            // Member call: obj.method() or ptr->method()
                            let mut obj_name: Option<String> = None;
                            let mut method_name: Option<String> = None;

                            for i in 0..func_node.child_count() {
                                if let Some(fc) = func_node.child(i) {
                                    match fc.kind() {
                                        "identifier" => {
                                            if obj_name.is_none() {
                                                obj_name =
                                                    Some(get_node_text(&fc, source).to_string());
                                            }
                                        }
                                        "field_identifier" => {
                                            method_name =
                                                Some(get_node_text(&fc, source).to_string());
                                        }
                                        "this" => {
                                            obj_name = Some("this".to_string());
                                        }
                                        _ => {}
                                    }
                                }
                            }

                            if let Some(method) = method_name {
                                let target = if let Some(ref obj) = obj_name {
                                    format!("{}.{}", obj, method)
                                } else {
                                    method.clone()
                                };

                                calls.push(CallSite::new(
                                    caller.to_string(),
                                    target,
                                    CallType::Attr,
                                    Some(line),
                                    None,
                                    obj_name,
                                    None,
                                ));
                            }
                        }
                        "qualified_identifier" => {
                            // Qualified call: ns::func() or Class::staticMethod()
                            let full_name = get_node_text(&func_node, source).to_string();

                            // Check if it's a known local function
                            let call_type = if defined_funcs.contains(&full_name) {
                                CallType::Intra
                            } else {
                                // Extract the last component to check
                                let last_part = full_name.rsplit("::").next().unwrap_or(&full_name);
                                if defined_funcs.contains(last_part) {
                                    CallType::Intra
                                } else {
                                    // Use Static for qualified calls (Class::method or ns::func)
                                    CallType::Static
                                }
                            };

                            calls.push(CallSite::new(
                                caller.to_string(),
                                full_name,
                                call_type,
                                Some(line),
                                None,
                                None,
                                None,
                            ));
                        }
                        "template_function" => {
                            // Template call: func<T>()
                            let target = get_node_text(&func_node, source).to_string();

                            // Extract the base name without template args for lookup
                            let base_name = target.split('<').next().unwrap_or(&target).to_string();

                            let call_type = if defined_funcs.contains(&base_name) {
                                CallType::Intra
                            } else {
                                CallType::Direct
                            };

                            calls.push(CallSite::new(
                                caller.to_string(),
                                target, // Keep full name with template args
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
                        _ => {
                            // Other patterns - try to extract text
                            let target = get_node_text(&func_node, source).to_string();
                            if !target.is_empty() && !target.contains(' ') {
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
}

impl CallGraphLanguageSupport for CppHandler {
    fn name(&self) -> &str {
        "cpp"
    }

    fn extensions(&self) -> &[&str] {
        &[".cpp", ".cc", ".cxx", ".hpp", ".hh", ".hxx", ".h"]
    }

    fn parse_imports(&self, source: &str, _path: &Path) -> Result<Vec<ImportDef>, ParseError> {
        let tree = super::c_common::parse_source_with_language(
            source,
            tree_sitter_cpp::LANGUAGE.into(),
            "C++",
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
        let (defined_funcs, defined_classes) = self.collect_definitions(tree, source_bytes);
        let mut calls_by_func: HashMap<String, Vec<CallSite>> = HashMap::new();

        struct FuncWalker<'a> {
            handler: &'a CppHandler,
            source: &'a [u8],
            defined_funcs: &'a HashSet<String>,
            defined_classes: &'a HashSet<String>,
            calls_by_func: &'a mut HashMap<String, Vec<CallSite>>,
            current_class: Option<String>,
        }

        fn walk_for_calls(walker: &mut FuncWalker, node: Node) {
            match node.kind() {
                "class_specifier" | "struct_specifier" => {
                    // Get class name
                    let mut class_name = None;
                    for i in 0..node.child_count() {
                        if let Some(child) = node.child(i) {
                            if child.kind() == "type_identifier" {
                                class_name = Some(get_node_text(&child, walker.source).to_string());
                                break;
                            }
                        }
                    }

                    if class_name.is_some() {
                        let old_class = walker.current_class.clone();
                        walker.current_class = class_name;

                        for i in 0..node.child_count() {
                            if let Some(child) = node.child(i) {
                                walk_for_calls(walker, child);
                            }
                        }

                        walker.current_class = old_class;
                        return;
                    }
                }
                "function_definition" => {
                    if let Some(func_name) = walker.handler.get_function_name(&node, walker.source)
                    {
                        // Build full name with class prefix if applicable
                        let full_name = if let Some(ref class) = walker.current_class {
                            // Only add prefix if not already qualified
                            if func_name.contains("::") {
                                func_name.clone()
                            } else {
                                format!("{}::{}", class, func_name)
                            }
                        } else {
                            func_name.clone()
                        };

                        let mut all_calls = Vec::new();

                        for i in 0..node.child_count() {
                            if let Some(child) = node.child(i) {
                                match child.kind() {
                                    // Pattern #1: Function body calls
                                    "compound_statement" => {
                                        let calls = walker.handler.extract_calls_from_func(
                                            &child,
                                            walker.source,
                                            walker.defined_funcs,
                                            walker.defined_classes,
                                            &full_name,
                                        );
                                        all_calls.extend(calls);
                                    }
                                    // Pattern #11: Constructor member initializer list
                                    // e.g. Child(int a) : Parent(compute(a)), x_(transform(a)) { ... }
                                    "field_initializer_list" => {
                                        let calls = walker.handler.extract_calls_from_func(
                                            &child,
                                            walker.source,
                                            walker.defined_funcs,
                                            walker.defined_classes,
                                            &full_name,
                                        );
                                        all_calls.extend(calls);
                                    }
                                    // Pattern #6/#7: Default parameter calls
                                    // e.g. void foo(int x = compute()) or Klass(int x = default_val())
                                    "function_declarator" => {
                                        let calls = extract_default_param_calls(
                                            walker.handler,
                                            &child,
                                            walker.source,
                                            walker.defined_funcs,
                                            walker.defined_classes,
                                            &full_name,
                                        );
                                        all_calls.extend(calls);
                                    }
                                    _ => {}
                                }
                            }
                        }

                        if !all_calls.is_empty() {
                            walker
                                .calls_by_func
                                .insert(full_name.clone(), all_calls.clone());
                            // Also store with just the base name
                            if full_name != func_name {
                                walker.calls_by_func.insert(func_name.clone(), all_calls);
                            }
                        }
                    }
                    return; // Don't recurse into function bodies again
                }
                // Pattern #3: NSDMI (non-static data member initializers)
                // e.g. class Config { int timeout = compute_default(); };
                // Instance fields -> ClassName.<init>, static fields -> ClassName.<clinit>
                "field_declaration" => {
                    if let Some(ref class) = walker.current_class {
                        // Check if this field_declaration contains a call_expression
                        // (indicating a field initializer with a function call)
                        let has_call = walk_tree(node).any(|n| n.kind() == "call_expression");
                        if has_call {
                            // Check if static
                            let is_static = (0..node.child_count()).any(|i| {
                                node.child(i).is_some_and(|c| {
                                    c.kind() == "storage_class_specifier"
                                        && get_node_text(&c, walker.source) == "static"
                                })
                            });

                            let caller = if is_static {
                                format!("{}.<clinit>", class)
                            } else {
                                format!("{}.<init>", class)
                            };

                            let calls = walker.handler.extract_calls_from_func(
                                &node,
                                walker.source,
                                walker.defined_funcs,
                                walker.defined_classes,
                                &caller,
                            );

                            if !calls.is_empty() {
                                walker
                                    .calls_by_func
                                    .entry(caller)
                                    .or_default()
                                    .extend(calls);
                            }
                        }
                    }
                    // Don't recurse further - we've handled this node
                    return;
                }
                // Pattern #10/#22: Global/namespace-level variable initializers
                // e.g. auto global_var = compute_global();
                // e.g. const auto config = create_config();
                // These are `declaration` nodes at translation_unit or namespace level
                "declaration" => {
                    // Only handle if NOT inside a function (i.e., at module/namespace level)
                    // We know we're at module level if current_class is None and we're
                    // being called from top-level walk (not inside a function_definition)
                    if walker.current_class.is_none() {
                        let has_call = walk_tree(node).any(|n| n.kind() == "call_expression");
                        if has_call {
                            let calls = walker.handler.extract_calls_from_func(
                                &node,
                                walker.source,
                                walker.defined_funcs,
                                walker.defined_classes,
                                "<module>",
                            );
                            if !calls.is_empty() {
                                walker
                                    .calls_by_func
                                    .entry("<module>".to_string())
                                    .or_default()
                                    .extend(calls);
                            }
                        }
                    }
                    return;
                }
                _ => {}
            }

            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    walk_for_calls(walker, child);
                }
            }
        }

        /// Extract calls from default parameter values in a function_declarator.
        /// Handles pattern #6 (constructor default params) and #7 (function default params).
        /// e.g. void foo(int x = compute(), string s = create("test"))
        fn extract_default_param_calls(
            handler: &CppHandler,
            func_declarator: &Node,
            source: &[u8],
            defined_funcs: &HashSet<String>,
            defined_classes: &HashSet<String>,
            caller: &str,
        ) -> Vec<CallSite> {
            let mut calls = Vec::new();
            for child in walk_tree(*func_declarator) {
                if child.kind() == "optional_parameter_declaration" {
                    // Extract calls from the default value expression
                    let param_calls = handler.extract_calls_from_func(
                        &child,
                        source,
                        defined_funcs,
                        defined_classes,
                        caller,
                    );
                    calls.extend(param_calls);
                }
            }
            calls
        }

        let mut walker = FuncWalker {
            handler: self,
            source: source_bytes,
            defined_funcs: &defined_funcs,
            defined_classes: &defined_classes,
            calls_by_func: &mut calls_by_func,
            current_class: None,
        };

        walk_for_calls(&mut walker, tree.root_node());

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

        // Use a recursive walker to track class/namespace context
        struct DefWalker<'a> {
            funcs: &'a mut Vec<FuncDef>,
            classes: &'a mut Vec<ClassDef>,
            current_class: Option<String>,
        }

        fn walk_for_defs(walker: &mut DefWalker, node: Node, source: &[u8], handler: &CppHandler) {
            match node.kind() {
                "class_specifier" | "struct_specifier" => {
                    let mut class_name = None;
                    for i in 0..node.child_count() {
                        if let Some(child) = node.child(i) {
                            if child.kind() == "type_identifier" {
                                class_name = Some(get_node_text(&child, source).to_string());
                                break;
                            }
                        }
                    }

                    if let Some(name) = class_name {
                        let line = node.start_position().row as u32 + 1;
                        let end_line = node.end_position().row as u32 + 1;

                        // Collect methods
                        let mut methods = Vec::new();
                        let old_class = walker.current_class.clone();
                        walker.current_class = Some(name.clone());

                        for i in 0..node.child_count() {
                            if let Some(child) = node.child(i) {
                                if child.kind() == "field_declaration_list" {
                                    for j in 0..child.child_count() {
                                        if let Some(member) = child.child(j) {
                                            if member.kind() == "function_definition" {
                                                if let Some(fn_name) =
                                                    handler.get_function_name(&member, source)
                                                {
                                                    methods.push(fn_name.clone());
                                                    let m_line =
                                                        member.start_position().row as u32 + 1;
                                                    let m_end =
                                                        member.end_position().row as u32 + 1;
                                                    walker.funcs.push(FuncDef::method(
                                                        fn_name, &name, m_line, m_end,
                                                    ));
                                                }
                                            }
                                            // Recurse into nested classes
                                            walk_for_defs(walker, member, source, handler);
                                        }
                                    }
                                }
                            }
                        }

                        // Collect base classes
                        let mut bases = Vec::new();
                        for i in 0..node.child_count() {
                            if let Some(child) = node.child(i) {
                                if child.kind() == "base_class_clause" {
                                    for j in 0..child.child_count() {
                                        if let Some(base) = child.child(j) {
                                            if base.kind() == "type_identifier" {
                                                bases
                                                    .push(get_node_text(&base, source).to_string());
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        walker
                            .classes
                            .push(ClassDef::new(name, line, end_line, methods, bases));
                        walker.current_class = old_class;
                        return; // Already recursed into children above
                    }
                }
                "function_definition" => {
                    if let Some(name) = handler.get_function_name(&node, source) {
                        let line = node.start_position().row as u32 + 1;
                        let end_line = node.end_position().row as u32 + 1;

                        if let Some(ref class_name) = walker.current_class {
                            walker.funcs.push(FuncDef::method(
                                name,
                                class_name.clone(),
                                line,
                                end_line,
                            ));
                        } else {
                            walker.funcs.push(FuncDef::function(name, line, end_line));
                        }
                    }
                }
                _ => {}
            }

            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    walk_for_defs(walker, child, source, handler);
                }
            }
        }

        let mut walker = DefWalker {
            funcs: &mut funcs,
            classes: &mut classes,
            current_class: None,
        };

        walk_for_defs(&mut walker, tree.root_node(), source_bytes, self);

        Ok((funcs, classes))
    }
}

// =============================================================================
// Tests
// =============================================================================

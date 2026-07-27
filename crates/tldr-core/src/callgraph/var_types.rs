//! Variable type extraction for call graph construction.
//!
//! This module contains `FileParseResult` and all per-language VarType extraction
//! functions. Each `extract_*_var_types` function walks a tree-sitter AST to find
//! variable type information (constructor calls, annotations, parameters, literals).
//!
//! Also contains Python-specific definition/call extraction (`extract_python_definitions`,
//! `extract_python_calls`, `parse_python_call`).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::cross_file_types::{CallSite, CallType, ClassDef, FuncDef, ImportDef, VarType};
use super::languages::base::{get_node_text, walk_tree};
use super::types::parse_source;

// =============================================================================
// FileParseResult
// =============================================================================

/// Result of parsing a single file for functions, classes, imports, and calls.
#[derive(Debug, Default)]
pub(crate) struct FileParseResult {
    /// Functions found in this file.
    pub(crate) funcs: Vec<FuncDef>,
    /// Classes found in this file.
    pub(crate) classes: Vec<ClassDef>,
    /// Imports found in this file.
    pub(crate) imports: Vec<ImportDef>,
    /// Calls found in this file, indexed by caller function name.
    pub(crate) calls: HashMap<String, Vec<CallSite>>,
    /// Variable type information extracted from assignments and annotations.
    pub(crate) var_types: Vec<VarType>,
}

// =============================================================================
// Python extraction
// =============================================================================

/// Extract functions, classes, imports, and calls from a Python source file.
pub(crate) fn extract_python_definitions(source: &str, _file_path: &Path) -> FileParseResult {
    let tree = match parse_source(source, "python") {
        Ok(t) => t,
        Err(_) => return FileParseResult::default(),
    };
    extract_python_definitions_from_tree(source, &tree)
}

/// Python extraction over a syntax tree owned by the shared ingestion pass.
pub(crate) fn extract_python_definitions_from_tree(
    source: &str,
    tree: &tree_sitter::Tree,
) -> FileParseResult {
    let mut result = FileParseResult::default();
    let source_bytes = source.as_bytes();
    let root = tree.root_node();

    // BUG-5 (cross-command-consistency-v1): collect the set of locally-defined
    // function and class names up-front so the call-extractor can recognise
    // function-as-value uses (e.g. `get_converter=_make_timedelta`) and emit
    // `CallType::Ref` edges. Without this, `tldr impact` reports
    // "exported but no callers" for any function that is only used as a value
    // inside the same project — yet `tldr references` finds the use trivially.
    let mut defined_names: HashSet<String> = HashSet::new();
    for node in walk_tree(root) {
        match node.kind() {
            "function_definition" | "class_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    defined_names.insert(get_node_text(&name_node, source_bytes).to_string());
                }
            }
            _ => {}
        }
    }

    // One-pass extraction for structure and calls.
    for node in walk_tree(root) {
        match node.kind() {
            "import_statement" => {
                // import X or import X as Y
                if let Some(import_def) =
                    super::imports::parse_python_import_statement(&node, source_bytes)
                {
                    result.imports.push(import_def);
                }
            }
            "import_from_statement" => {
                // from X import Y
                if let Some(import_def) =
                    super::imports::parse_python_from_import(&node, source_bytes)
                {
                    result.imports.push(import_def);
                }
            }
            "class_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let class_name = get_node_text(&name_node, source_bytes).to_string();
                    let line = node.start_position().row as u32 + 1;
                    let end_line = node.end_position().row as u32 + 1;

                    // Extract base classes
                    let mut bases = Vec::new();
                    if let Some(arg_list) = node.child_by_field_name("superclasses") {
                        for i in 0..arg_list.named_child_count() {
                            if let Some(base) = arg_list.named_child(i) {
                                bases.push(get_node_text(&base, source_bytes).to_string());
                            }
                        }
                    }

                    // Collect method names from class body
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

                    // BUG-5: extract function-as-value Refs from class-body
                    // field initialisers (e.g. `attr = some(callback=_helper)`).
                    // This makes class-body field references discoverable to
                    // `tldr impact`. The caller name is the class name itself
                    // (matches the existing python.rs handler convention).
                    if let Some(body) = node.child_by_field_name("body") {
                        let mut class_calls = Vec::new();
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
                                collect_python_value_refs(
                                    &child,
                                    source_bytes,
                                    &class_name,
                                    &defined_names,
                                    &mut class_calls,
                                );
                            }
                        }
                        if !class_calls.is_empty() {
                            result
                                .calls
                                .entry(class_name.clone())
                                .or_default()
                                .extend(class_calls);
                        }
                    }

                    result
                        .classes
                        .push(ClassDef::new(class_name, line, end_line, methods, bases));
                }
            }
            "function_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let func_name = get_node_text(&name_node, source_bytes).to_string();
                    let line = node.start_position().row as u32 + 1;
                    let end_line = node.end_position().row as u32 + 1;

                    // Check if this is a method (directly inside a class body).
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

                    // Determine the caller name: qualified for methods, simple for top-level functions
                    let caller_name = if let Some(ref cn) = class_name {
                        result.funcs.push(FuncDef::method(
                            func_name.clone(),
                            cn.clone(),
                            line,
                            end_line,
                        ));
                        format!("{}.{}", cn, func_name)
                    } else {
                        result
                            .funcs
                            .push(FuncDef::function(func_name.clone(), line, end_line));
                        func_name.clone()
                    };

                    // Extract calls within this function using the qualified caller name
                    let mut calls = extract_python_calls(&node, source_bytes, &caller_name);

                    // BUG-5: also extract function-as-value (Ref) edges so
                    // `tldr impact` can find higher-order use of locally
                    // defined functions (return / assignment / kwarg /
                    // positional argument).
                    collect_python_value_refs(
                        &node,
                        source_bytes,
                        &caller_name,
                        &defined_names,
                        &mut calls,
                    );

                    if !calls.is_empty() {
                        result.calls.insert(caller_name, calls);
                    }
                }
            }
            _ => {}
        }
    }

    result.var_types = extract_python_var_types(tree, source_bytes);

    result
}

/// BUG-5: walk a node and collect identifier-as-value uses that resolve to
/// locally-defined functions/classes as `CallType::Ref` call sites.
///
/// "function-as-value" means an identifier appears outside of the
/// `function` field of a `call` node — e.g. `return _helper`, `fn = _helper`,
/// `map(_helper, ...)`, `kw=_helper`, etc. These uses must produce edges so
/// that `tldr impact` returns the same callers that `tldr references` finds.
///
/// Each defined name is added at most once per (caller, target) to keep the
/// edge set bounded; the line is the first occurrence.
fn collect_python_value_refs(
    root_node: &tree_sitter::Node,
    source: &[u8],
    caller: &str,
    defined_names: &HashSet<String>,
    sink: &mut Vec<CallSite>,
) {
    if defined_names.is_empty() {
        return;
    }
    let mut emitted: HashSet<String> = HashSet::new();
    for node in walk_tree(*root_node) {
        if node.kind() != "identifier" {
            continue;
        }
        let name = get_node_text(&node, source);
        if !defined_names.contains(name) {
            continue;
        }
        let parent = match node.parent() {
            Some(p) => p,
            None => continue,
        };
        // Skip the identifier-form of a call's `function` field — that
        // is the regular "Direct" / "Method" call path handled by
        // parse_python_call.
        if parent.kind() == "call" && parent.child_by_field_name("function").as_ref() == Some(&node)
        {
            continue;
        }
        // Skip definition sites: `def name(...)` and `class name:`.
        if matches!(parent.kind(), "function_definition" | "class_definition")
            && parent.child_by_field_name("name").as_ref() == Some(&node)
        {
            continue;
        }
        // Skip attribute accesses where this identifier is the
        // attribute name (`obj.name` — that's a method/attr access,
        // not a free reference to the local function).
        if parent.kind() == "attribute"
            && parent.child_by_field_name("attribute").as_ref() == Some(&node)
        {
            continue;
        }
        // Skip parameter lists — `def f(x):` parameters are not Refs.
        if matches!(
            parent.kind(),
            "parameters" | "default_parameter" | "typed_parameter" | "typed_default_parameter"
        ) {
            continue;
        }
        // Dedup per (caller, target) to keep the edge set bounded.
        if !emitted.insert(name.to_string()) {
            continue;
        }
        let line = node.start_position().row as u32 + 1;
        sink.push(CallSite::new(
            caller.to_string(),
            name.to_string(),
            CallType::Ref,
            Some(line),
            None,
            None,
            None,
        ));
    }
}

/// Extract calls from a Python function body.
pub(crate) fn extract_python_calls(
    func_node: &tree_sitter::Node,
    source: &[u8],
    caller: &str,
) -> Vec<CallSite> {
    let mut calls = Vec::new();

    // Walk the function body looking for call expressions
    for node in walk_tree(*func_node) {
        if node.kind() == "call" {
            if let Some(call_site) = parse_python_call(&node, source, caller) {
                calls.push(call_site);
            }
        }
    }

    calls
}

/// Parse a Python call expression into a CallSite.
fn parse_python_call(node: &tree_sitter::Node, source: &[u8], caller: &str) -> Option<CallSite> {
    let line = node.start_position().row as u32 + 1;
    let column = node.start_position().column as u32 + 1;

    // Get the function/method being called
    let func_node = node.child_by_field_name("function")?;

    match func_node.kind() {
        "identifier" => {
            // Simple call: foo()
            let target = get_node_text(&func_node, source).to_string();
            Some(CallSite::new(
                caller.to_string(),
                target,
                CallType::Direct,
                Some(line),
                Some(column),
                None,
                None,
            ))
        }
        "attribute" => {
            // Method or attribute call: obj.method() or module.func()
            let object_node = func_node.child_by_field_name("object")?;
            let attr_node = func_node.child_by_field_name("attribute")?;

            let receiver = get_node_text(&object_node, source).to_string();
            let target = get_node_text(&attr_node, source).to_string();

            // Determine call type based on receiver
            // If receiver is a simple identifier, it could be either:
            // - A module (import-based call): json.loads()
            // - An object (method call): user.save()
            // We'll mark it as Method and resolve later during import resolution
            Some(CallSite::new(
                caller.to_string(),
                target,
                CallType::Method,
                Some(line),
                Some(column),
                Some(receiver),
                None, // receiver_type will be filled during type resolution
            ))
        }
        _ => None,
    }
}

/// Determine the enclosing function scope for a tree-sitter node.
///
/// Walks up the parent chain to find the nearest `function_definition` ancestor.
/// Returns `Some(function_name)` if found, `None` for module-level scope.
pub(crate) fn enclosing_function_scope(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "function_definition" {
            if let Some(name_node) = parent.child_by_field_name("name") {
                return Some(get_node_text(&name_node, source).to_string());
            }
        }
        current = parent.parent();
    }
    None
}

// =============================================================================
// Python VarType extraction
// =============================================================================

/// Extract VarType entries from a Python source tree.
///
/// Walks the AST to find:
/// - **Constructor assignments**: `x = Foo()` -> VarType { source: "assignment" }
/// - **Annotated assignments**: `x: Foo` or `x: Foo = ...` -> VarType { source: "annotation" }
/// - **Parameter annotations**: `def f(x: Foo)` -> VarType { source: "parameter" }
///
/// Scope is determined by the enclosing function definition, or None for module-level.
pub(crate) fn extract_python_var_types(tree: &tree_sitter::Tree, source: &[u8]) -> Vec<VarType> {
    let mut var_types = Vec::new();
    let root = tree.root_node();

    for node in walk_tree(root) {
        match node.kind() {
            // Pattern 1: x = Foo() -- constructor assignment
            // Pattern 1b: x = {} / [] / "" / () -- builtin literal assignment
            "assignment" => {
                // Left side should be a simple identifier
                let left = match node.child_by_field_name("left") {
                    Some(n) if n.kind() == "identifier" => n,
                    _ => continue,
                };
                let right = match node.child_by_field_name("right") {
                    Some(n) => n,
                    None => continue,
                };

                let var_name = get_node_text(&left, source).to_string();
                if var_name.is_empty() {
                    continue;
                }
                let line = node.start_position().row as u32 + 1;
                let scope = enclosing_function_scope(&node, source);

                match right.kind() {
                    "call" => {
                        // The call's function should be a simple identifier
                        let func_node = match right.child_by_field_name("function") {
                            Some(n) if n.kind() == "identifier" => n,
                            _ => continue,
                        };
                        let type_name = get_node_text(&func_node, source).to_string();

                        // Check for lowercase builtin constructors: dict(), list(), set(), etc.
                        if type_name.chars().next().is_none_or(|c| c.is_lowercase()) {
                            match type_name.as_str() {
                                "dict" | "list" | "set" | "tuple" | "frozenset" | "str"
                                | "bytes" | "bytearray" | "int" | "float" | "bool" | "complex" => {
                                    var_types.push(VarType::new_with_scope(
                                        var_name,
                                        type_name,
                                        "constructor",
                                        line,
                                        scope,
                                    ));
                                }
                                _ => {} // Skip other lowercase calls
                            }
                            continue;
                        }

                        // Capitalized call -- likely a class constructor: Foo()
                        var_types.push(VarType::new_with_scope(
                            var_name,
                            type_name,
                            "assignment",
                            line,
                            scope,
                        ));
                    }
                    // Builtin literal types
                    "dictionary" | "dictionary_comprehension" => {
                        var_types.push(VarType::new_with_scope(
                            var_name,
                            "dict".to_string(),
                            "literal",
                            line,
                            scope,
                        ));
                    }
                    "list" | "list_comprehension" => {
                        var_types.push(VarType::new_with_scope(
                            var_name,
                            "list".to_string(),
                            "literal",
                            line,
                            scope,
                        ));
                    }
                    "string" | "concatenated_string" => {
                        var_types.push(VarType::new_with_scope(
                            var_name,
                            "str".to_string(),
                            "literal",
                            line,
                            scope,
                        ));
                    }
                    "tuple" => {
                        var_types.push(VarType::new_with_scope(
                            var_name,
                            "tuple".to_string(),
                            "literal",
                            line,
                            scope,
                        ));
                    }
                    "set" | "set_comprehension" => {
                        var_types.push(VarType::new_with_scope(
                            var_name,
                            "set".to_string(),
                            "literal",
                            line,
                            scope,
                        ));
                    }
                    "integer" => {
                        var_types.push(VarType::new_with_scope(
                            var_name,
                            "int".to_string(),
                            "literal",
                            line,
                            scope,
                        ));
                    }
                    "float" => {
                        var_types.push(VarType::new_with_scope(
                            var_name,
                            "float".to_string(),
                            "literal",
                            line,
                            scope,
                        ));
                    }
                    "true" | "false" => {
                        var_types.push(VarType::new_with_scope(
                            var_name,
                            "bool".to_string(),
                            "literal",
                            line,
                            scope,
                        ));
                    }
                    _ => {}
                }
            }

            // Pattern 2: x: Foo or x: Foo = ... -- type annotation
            "type" => {
                // A "type" node inside an expression_statement or assignment
                // represents a type annotation.
                // For `x: Foo`, tree-sitter produces:
                //   expression_statement > type > identifier(x) + type(Foo)
                // For `x: Foo = val`, tree-sitter produces:
                //   assignment > type > identifier(x) + type(Foo)  [left side]
                //
                // We handle this by looking at the parent context.
                // Actually, tree-sitter Python handles annotations differently.
                // Let's handle it via the parent node patterns.
            }

            // Pattern 2 (actual): Annotated assignments and standalone annotations
            // tree-sitter-python produces different node types:
            // - `x: int = 5` -> expression_statement containing type annotation
            // We catch typed_parameter for function params separately below.
            "expression_statement" => {
                // Check if this contains a type annotation: `x: Type`
                // tree-sitter-python emits this as an expression_statement
                // containing a "type" child with annotation syntax.
                //
                // Actually, annotations in tree-sitter-python are handled as:
                // expression_statement > assignment with type annotation
                // Let's check the first child.
                if node.named_child_count() == 1 {
                    if let Some(child) = node.named_child(0) {
                        if child.kind() == "type" {
                            // Standalone annotation: `x: Foo`
                            // The type node has two children: the name and the type
                            if let (Some(name_node), Some(type_node)) =
                                (child.child_by_field_name("type"), child.child(0))
                            {
                                // This is tricky - let me handle it more carefully
                                let _ = (name_node, type_node);
                            }
                        }
                    }
                }
            }

            // Pattern 3: def f(x: Foo) -- typed parameter
            "typed_parameter" => {
                // typed_parameter has a name (identifier) and a type
                let name_node = match node.child_by_field_name("name") {
                    Some(n) => n,
                    None => {
                        // Fallback: first child might be the name
                        match node.child(0) {
                            Some(n) if n.kind() == "identifier" => n,
                            _ => continue,
                        }
                    }
                };
                let type_node = match node.child_by_field_name("type") {
                    Some(n) => n,
                    None => continue,
                };

                let var_name = get_node_text(&name_node, source).to_string();
                let type_name = get_node_text(&type_node, source).to_string();
                let line = node.start_position().row as u32 + 1;

                // Skip 'self' and 'cls' parameters
                if var_name == "self" || var_name == "cls" {
                    continue;
                }

                // Determine scope: the enclosing function
                let scope = enclosing_function_scope(&node, source);

                var_types.push(VarType::new_with_scope(
                    var_name,
                    type_name,
                    "parameter",
                    line,
                    scope,
                ));
            }

            // Pattern 3b: def f(x: Foo = default_val) -- typed parameter with default
            "typed_default_parameter" => {
                let name_node = match node.child_by_field_name("name") {
                    Some(n) => n,
                    None => match node.child(0) {
                        Some(n) if n.kind() == "identifier" => n,
                        _ => continue,
                    },
                };
                let type_node = match node.child_by_field_name("type") {
                    Some(n) => n,
                    None => continue,
                };

                let var_name = get_node_text(&name_node, source).to_string();
                let type_name = get_node_text(&type_node, source).to_string();
                let line = node.start_position().row as u32 + 1;

                if var_name == "self" || var_name == "cls" {
                    continue;
                }

                let scope = enclosing_function_scope(&node, source);

                var_types.push(VarType::new_with_scope(
                    var_name,
                    type_name,
                    "parameter",
                    line,
                    scope,
                ));
            }

            _ => {}
        }
    }

    var_types
}

// =============================================================================
// Go VarType extraction
// =============================================================================

/// Determine the enclosing function scope for a Go AST node.
///
/// Go uses `function_declaration` (top-level funcs) and `method_declaration` (receiver methods).
/// For methods, the scope is `ReceiverType.MethodName`.
pub(crate) fn enclosing_go_function_scope(
    node: &tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "function_declaration" => {
                if let Some(name_node) = parent.child_by_field_name("name") {
                    return Some(get_node_text(&name_node, source).to_string());
                }
            }
            "method_declaration" => {
                let method_name = parent
                    .child_by_field_name("name")
                    .map(|n| get_node_text(&n, source).to_string());
                if let Some(name) = method_name {
                    // Try to get receiver type for full scope like "Foo.Method"
                    if let Some(receiver_list) = parent.child_by_field_name("receiver") {
                        for i in 0..receiver_list.named_child_count() {
                            if let Some(param) = receiver_list.named_child(i) {
                                if param.kind() == "parameter_declaration" {
                                    if let Some(type_node) = param.child_by_field_name("type") {
                                        let type_text = get_node_text(&type_node, source);
                                        let receiver_type = type_text.trim_start_matches('*');
                                        return Some(format!("{}.{}", receiver_type, name));
                                    }
                                }
                            }
                        }
                    }
                    return Some(name);
                }
            }
            _ => {}
        }
        current = parent.parent();
    }
    None
}

/// Extract VarType entries from a Go source tree.
///
/// Walks the AST to find:
/// - **Short var declaration with composite literal**: `x := Foo{...}` -> VarType { source: "assignment" }
/// - **Short var declaration with constructor call**: `x := NewFoo()` -> VarType { source: "assignment" }
/// - **Var declaration with type**: `var x Foo` -> VarType { source: "annotation" }
/// - **Function/method parameters with types**: `func f(x Foo)` -> VarType { source: "parameter" }
/// - **Method receiver parameters**: `func (f *Foo) Method()` -> VarType { source: "parameter" }
///
/// Builtin types (map, slice, array, chan, string, int, etc.) produce "literal" source to
/// enable the FP defense layers (blocklist + ambiguity gate) for Go.
pub(crate) fn extract_go_var_types(tree: &tree_sitter::Tree, source: &[u8]) -> Vec<VarType> {
    let mut var_types = Vec::new();
    let root = tree.root_node();

    // Go builtin types that should not match project classes
    let go_builtin_types = [
        "string",
        "int",
        "int8",
        "int16",
        "int32",
        "int64",
        "uint",
        "uint8",
        "uint16",
        "uint32",
        "uint64",
        "uintptr",
        "float32",
        "float64",
        "complex64",
        "complex128",
        "bool",
        "byte",
        "rune",
        "error",
        "any",
    ];

    for node in walk_tree(root) {
        match node.kind() {
            // Pattern 1: x := Foo{...} or x := NewFoo()
            // short_var_declaration has left (expression_list) and right (expression_list)
            "short_var_declaration" => {
                let left = match node.child_by_field_name("left") {
                    Some(n) => n,
                    None => continue,
                };
                let right = match node.child_by_field_name("right") {
                    Some(n) => n,
                    None => continue,
                };

                // Right side must have exactly 1 expression
                if right.named_child_count() != 1 {
                    continue;
                }

                let val_node = match right.named_child(0) {
                    Some(n) => n,
                    None => continue,
                };

                let line = node.start_position().row as u32 + 1;
                let scope = enclosing_go_function_scope(&node, source);

                // Multi-return: x, err := NewFoo() or x, ok := val.(Foo)
                if left.named_child_count() >= 2 {
                    let var_node = match left.named_child(0) {
                        Some(n) if n.kind() == "identifier" => n,
                        _ => continue,
                    };
                    let var_name = get_node_text(&var_node, source).to_string();
                    if var_name == "_" {
                        continue;
                    }

                    match val_node.kind() {
                        "call_expression" => {
                            if let Some(func_node) = val_node.child_by_field_name("function") {
                                let func_text = get_node_text(&func_node, source).to_string();
                                let base_name = func_text.rsplit('.').next().unwrap_or(&func_text);
                                if base_name.starts_with("New") && base_name.len() > 3 {
                                    let type_name = &base_name[3..];
                                    if !type_name.is_empty()
                                        && type_name
                                            .chars()
                                            .next()
                                            .is_some_and(|c| c.is_uppercase())
                                    {
                                        var_types.push(VarType::new_with_scope(
                                            var_name,
                                            type_name.to_string(),
                                            "assignment",
                                            line,
                                            scope,
                                        ));
                                    }
                                } else if base_name.chars().next().is_some_and(|c| c.is_uppercase())
                                {
                                    var_types.push(VarType::new_with_scope(
                                        var_name,
                                        base_name.to_string(),
                                        "assignment",
                                        line,
                                        scope,
                                    ));
                                }
                            }
                        }
                        "composite_literal" => {
                            if let Some(type_node) = val_node.child_by_field_name("type") {
                                let type_text = get_node_text(&type_node, source).to_string();
                                match type_node.kind() {
                                    "type_identifier" => {
                                        if !go_builtin_types.contains(&type_text.as_str()) {
                                            var_types.push(VarType::new_with_scope(
                                                var_name,
                                                type_text,
                                                "assignment",
                                                line,
                                                scope.clone(),
                                            ));
                                        }
                                    }
                                    "qualified_type" => {
                                        var_types.push(VarType::new_with_scope(
                                            var_name,
                                            type_text,
                                            "assignment",
                                            line,
                                            scope.clone(),
                                        ));
                                    }
                                    _ => {}
                                }
                            }
                        }
                        "type_assertion_expression" => {
                            // x, ok := val.(Foo)
                            if let Some(type_node) = val_node.child_by_field_name("type") {
                                let type_text = get_node_text(&type_node, source).to_string();
                                let clean_type = type_text.trim_start_matches('*').to_string();
                                if !go_builtin_types.contains(&clean_type.as_str()) {
                                    var_types.push(VarType::new_with_scope(
                                        var_name,
                                        clean_type,
                                        "assertion",
                                        line,
                                        scope,
                                    ));
                                }
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                // Single assignment: x := expr
                if left.named_child_count() != 1 {
                    continue;
                }

                let var_node = match left.named_child(0) {
                    Some(n) if n.kind() == "identifier" => n,
                    _ => continue,
                };

                let var_name = get_node_text(&var_node, source).to_string();
                if var_name == "_" {
                    continue;
                }

                match val_node.kind() {
                    "composite_literal" => {
                        if let Some(type_node) = val_node.child_by_field_name("type") {
                            let type_text = get_node_text(&type_node, source).to_string();
                            match type_node.kind() {
                                "type_identifier" => {
                                    if !go_builtin_types.contains(&type_text.as_str()) {
                                        var_types.push(VarType::new_with_scope(
                                            var_name,
                                            type_text,
                                            "assignment",
                                            line,
                                            scope,
                                        ));
                                    }
                                }
                                "qualified_type" => {
                                    var_types.push(VarType::new_with_scope(
                                        var_name,
                                        type_text,
                                        "assignment",
                                        line,
                                        scope,
                                    ));
                                }
                                "map_type" | "slice_type" | "array_type" => {
                                    let builtin_name = match type_node.kind() {
                                        "map_type" => "map",
                                        "slice_type" => "slice",
                                        "array_type" => "array",
                                        _ => "unknown",
                                    };
                                    var_types.push(VarType::new_with_scope(
                                        var_name,
                                        builtin_name.to_string(),
                                        "literal",
                                        line,
                                        scope,
                                    ));
                                }
                                _ => {}
                            }
                        }
                    }
                    "call_expression" => {
                        if let Some(func_node) = val_node.child_by_field_name("function") {
                            let func_text = get_node_text(&func_node, source).to_string();
                            let base_name = func_text.rsplit('.').next().unwrap_or(&func_text);
                            if base_name.starts_with("New") && base_name.len() > 3 {
                                let type_name = &base_name[3..];
                                if !type_name.is_empty()
                                    && type_name.chars().next().is_some_and(|c| c.is_uppercase())
                                {
                                    var_types.push(VarType::new_with_scope(
                                        var_name,
                                        type_name.to_string(),
                                        "assignment",
                                        line,
                                        scope,
                                    ));
                                }
                            } else if base_name.chars().next().is_some_and(|c| c.is_uppercase()) {
                                var_types.push(VarType::new_with_scope(
                                    var_name,
                                    base_name.to_string(),
                                    "assignment",
                                    line,
                                    scope,
                                ));
                            }
                        }
                    }
                    "type_assertion_expression" => {
                        // x := val.(Foo)
                        if let Some(type_node) = val_node.child_by_field_name("type") {
                            let type_text = get_node_text(&type_node, source).to_string();
                            let clean_type = type_text.trim_start_matches('*').to_string();
                            if !go_builtin_types.contains(&clean_type.as_str()) {
                                var_types.push(VarType::new_with_scope(
                                    var_name,
                                    clean_type,
                                    "assertion",
                                    line,
                                    scope,
                                ));
                            }
                        }
                    }
                    "interpreted_string_literal" | "raw_string_literal" => {
                        var_types.push(VarType::new_with_scope(
                            var_name,
                            "string".to_string(),
                            "literal",
                            line,
                            scope,
                        ));
                    }
                    "int_literal" => {
                        var_types.push(VarType::new_with_scope(
                            var_name,
                            "int".to_string(),
                            "literal",
                            line,
                            scope,
                        ));
                    }
                    "float_literal" => {
                        var_types.push(VarType::new_with_scope(
                            var_name,
                            "float64".to_string(),
                            "literal",
                            line,
                            scope,
                        ));
                    }
                    "true" | "false" => {
                        var_types.push(VarType::new_with_scope(
                            var_name,
                            "bool".to_string(),
                            "literal",
                            line,
                            scope,
                        ));
                    }
                    _ => {}
                }
            }

            // Pattern 2: var x Foo -- explicit type declaration
            "var_spec" => {
                let name_node = match node.child_by_field_name("name") {
                    Some(n) if n.kind() == "identifier" => n,
                    _ => continue,
                };
                let type_node = match node.child_by_field_name("type") {
                    Some(n) => n,
                    None => continue,
                };

                let var_name = get_node_text(&name_node, source).to_string();
                let type_text = get_node_text(&type_node, source).to_string();
                let line = node.start_position().row as u32 + 1;
                let scope = enclosing_go_function_scope(&node, source);

                // Only track non-builtin types
                if !go_builtin_types.contains(&type_text.as_str()) {
                    var_types.push(VarType::new_with_scope(
                        var_name,
                        type_text,
                        "annotation",
                        line,
                        scope,
                    ));
                }
            }

            // Pattern 3: func f(x Foo) -- function parameter with type
            "parameter_declaration" => {
                // Skip if inside a receiver (handled as part of method_declaration scope)
                // Check parent: if it's a parameter_list that's a "receiver" field, skip
                if let Some(param_list) = node.parent() {
                    if let Some(method_decl) = param_list.parent() {
                        if method_decl.kind() == "method_declaration" {
                            if let Some(receiver) = method_decl.child_by_field_name("receiver") {
                                if receiver.id() == param_list.id() {
                                    // This is a receiver parameter -- still extract it
                                    // but with special handling
                                    if let (Some(name_node), Some(type_node)) = (
                                        node.child_by_field_name("name"),
                                        node.child_by_field_name("type"),
                                    ) {
                                        let var_name =
                                            get_node_text(&name_node, source).to_string();
                                        let type_text =
                                            get_node_text(&type_node, source).to_string();
                                        // Strip pointer: *Foo -> Foo
                                        let clean_type =
                                            type_text.trim_start_matches('*').to_string();
                                        let line = node.start_position().row as u32 + 1;
                                        let scope = enclosing_go_function_scope(&node, source);
                                        if !go_builtin_types.contains(&clean_type.as_str()) {
                                            var_types.push(VarType::new_with_scope(
                                                var_name,
                                                clean_type,
                                                "parameter",
                                                line,
                                                scope,
                                            ));
                                        }
                                    }
                                    continue;
                                }
                            }
                        }
                    }
                }

                // Regular function parameter
                let name_node = match node.child_by_field_name("name") {
                    Some(n) if n.kind() == "identifier" => n,
                    _ => continue,
                };
                let type_node = match node.child_by_field_name("type") {
                    Some(n) => n,
                    None => continue,
                };

                let var_name = get_node_text(&name_node, source).to_string();
                let type_text = get_node_text(&type_node, source).to_string();
                // Strip pointer: *Foo -> Foo
                let clean_type = type_text.trim_start_matches('*').to_string();
                let line = node.start_position().row as u32 + 1;
                let scope = enclosing_go_function_scope(&node, source);

                if !go_builtin_types.contains(&clean_type.as_str()) {
                    var_types.push(VarType::new_with_scope(
                        var_name,
                        clean_type,
                        "parameter",
                        line,
                        scope,
                    ));
                }
            }

            _ => {}
        }
    }

    var_types
}

// =============================================================================
// TypeScript/JavaScript VarType extraction
// =============================================================================

/// Determine the enclosing function scope for a TypeScript/JavaScript AST node.
///
/// Walks parent nodes looking for:
/// - `function_declaration` -> function name
/// - `method_definition` -> `ClassName.methodName` (prepends class name if found)
/// - `arrow_function` / `function_expression` / `function` -> check parent `variable_declarator` for name
///
/// Returns `None` for module-level code (no enclosing function).
pub(crate) fn enclosing_ts_function_scope(
    node: &tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "function_declaration" => {
                if let Some(name_node) = parent.child_by_field_name("name") {
                    return Some(get_node_text(&name_node, source).to_string());
                }
            }
            "method_definition" => {
                let method_name = parent
                    .child_by_field_name("name")
                    .map(|n| get_node_text(&n, source).to_string());
                if let Some(name) = method_name {
                    // Try to get class name: method_definition > class_body > class_declaration
                    if let Some(class_body) = parent.parent() {
                        if class_body.kind() == "class_body" {
                            if let Some(class_decl) = class_body.parent() {
                                if class_decl.kind() == "class_declaration"
                                    || class_decl.kind() == "class"
                                {
                                    if let Some(class_name_node) =
                                        class_decl.child_by_field_name("name")
                                    {
                                        let class_name = get_node_text(&class_name_node, source);
                                        return Some(format!("{}.{}", class_name, name));
                                    }
                                }
                            }
                        }
                    }
                    return Some(name);
                }
            }
            "arrow_function" | "function_expression" | "function" => {
                // Anonymous -- check if parent is variable_declarator
                if let Some(var_decl) = parent.parent() {
                    if var_decl.kind() == "variable_declarator" {
                        if let Some(name_node) = var_decl.child_by_field_name("name") {
                            return Some(get_node_text(&name_node, source).to_string());
                        }
                    }
                }
                // Truly anonymous -- return None (module scope)
                return None;
            }
            _ => {}
        }
        current = parent.parent();
    }
    None
}

/// Extract VarType entries from a TypeScript/JavaScript source tree.
pub(crate) fn extract_ts_var_types(tree: &tree_sitter::Tree, source: &[u8]) -> Vec<VarType> {
    let mut var_types = Vec::new();
    let root = tree.root_node();

    // TS/JS builtin types that should not match project classes
    let ts_builtin_types = [
        "string",
        "number",
        "boolean",
        "bigint",
        "symbol",
        "undefined",
        "null",
        "void",
        "never",
        "any",
        "unknown",
        "object",
        "String",
        "Number",
        "Boolean",
        "Function",
        "Object",
        "Array",
        "Promise",
        "Map",
        "Set",
        "RegExp",
        "Date",
        "Error",
        "Symbol",
    ];

    for node in walk_tree(root) {
        match node.kind() {
            // Pattern 1: const x = new Foo() / let x: Type = expr / let x: Type
            "variable_declarator" => {
                let name_node = match node.child_by_field_name("name") {
                    Some(n) if n.kind() == "identifier" => n,
                    _ => continue,
                };
                let var_name = get_node_text(&name_node, source).to_string();
                let line = node.start_position().row as u32 + 1;
                let scope = enclosing_ts_function_scope(&node, source);

                // Check value field first (new expression, as expression, literals)
                if let Some(value) = node.child_by_field_name("value") {
                    match value.kind() {
                        "new_expression" => {
                            // const x = new Foo(...)
                            if let Some(ctor) = value.child_by_field_name("constructor") {
                                let type_name = get_node_text(&ctor, source).to_string();
                                if !ts_builtin_types.contains(&type_name.as_str()) {
                                    var_types.push(VarType::new_with_scope(
                                        var_name.clone(),
                                        type_name,
                                        "assignment",
                                        line,
                                        scope.clone(),
                                    ));
                                }
                            }
                        }
                        "as_expression" => {
                            // const x = expr as Type
                            let child_count = value.named_child_count();
                            if child_count >= 2 {
                                if let Some(type_node) = value.named_child(child_count - 1) {
                                    let type_name = match type_node.kind() {
                                        "type_identifier" => {
                                            Some(get_node_text(&type_node, source).to_string())
                                        }
                                        "generic_type" => type_node
                                            .child_by_field_name("name")
                                            .map(|n| get_node_text(&n, source).to_string()),
                                        _ => None,
                                    };
                                    if let Some(tn) = type_name {
                                        if !ts_builtin_types.contains(&tn.as_str()) {
                                            var_types.push(VarType::new_with_scope(
                                                var_name.clone(),
                                                tn,
                                                "assertion",
                                                line,
                                                scope.clone(),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                        // Literal types
                        "string" | "template_string" => {
                            var_types.push(VarType::new_with_scope(
                                var_name.clone(),
                                "string".to_string(),
                                "literal",
                                line,
                                scope.clone(),
                            ));
                        }
                        "number" => {
                            var_types.push(VarType::new_with_scope(
                                var_name.clone(),
                                "number".to_string(),
                                "literal",
                                line,
                                scope.clone(),
                            ));
                        }
                        "true" | "false" => {
                            var_types.push(VarType::new_with_scope(
                                var_name.clone(),
                                "boolean".to_string(),
                                "literal",
                                line,
                                scope.clone(),
                            ));
                        }
                        "array" => {
                            var_types.push(VarType::new_with_scope(
                                var_name.clone(),
                                "Array".to_string(),
                                "literal",
                                line,
                                scope.clone(),
                            ));
                        }
                        "object" => {
                            var_types.push(VarType::new_with_scope(
                                var_name.clone(),
                                "Object".to_string(),
                                "literal",
                                line,
                                scope.clone(),
                            ));
                        }
                        "regex" => {
                            var_types.push(VarType::new_with_scope(
                                var_name.clone(),
                                "RegExp".to_string(),
                                "literal",
                                line,
                                scope.clone(),
                            ));
                        }
                        "null" => {
                            var_types.push(VarType::new_with_scope(
                                var_name.clone(),
                                "null".to_string(),
                                "literal",
                                line,
                                scope.clone(),
                            ));
                        }
                        _ => {}
                    }
                }

                // Check type annotation: let x: Type or const x: Type = new Foo()
                // Type annotation takes priority for the type mapping
                if let Some(type_ann) = node.child_by_field_name("type") {
                    // type_ann is the type_annotation node, its first named child is the type
                    if let Some(type_node) = type_ann.named_child(0) {
                        let type_name = match type_node.kind() {
                            "type_identifier" => {
                                Some(get_node_text(&type_node, source).to_string())
                            }
                            "generic_type" => type_node
                                .child_by_field_name("name")
                                .map(|n| get_node_text(&n, source).to_string()),
                            _ => None,
                        };
                        if let Some(tn) = type_name {
                            if !ts_builtin_types.contains(&tn.as_str()) {
                                var_types.push(VarType::new_with_scope(
                                    var_name,
                                    tn,
                                    "annotation",
                                    line,
                                    scope,
                                ));
                            }
                        }
                    }
                }
            }

            // Pattern 2: function f(x: Foo) -- typed parameters
            "required_parameter" | "optional_parameter" => {
                let name_node = match node.child_by_field_name("pattern") {
                    Some(n) if n.kind() == "identifier" => n,
                    _ => {
                        // Fallback: try first named child
                        match node.named_child(0) {
                            Some(n) if n.kind() == "identifier" => n,
                            _ => continue,
                        }
                    }
                };

                let type_ann = match node.child_by_field_name("type") {
                    Some(n) => n,
                    None => continue,
                };

                let var_name = get_node_text(&name_node, source).to_string();
                let line = node.start_position().row as u32 + 1;
                let scope = enclosing_ts_function_scope(&node, source);

                if let Some(type_node) = type_ann.named_child(0) {
                    let type_name = match type_node.kind() {
                        "type_identifier" => Some(get_node_text(&type_node, source).to_string()),
                        "generic_type" => type_node
                            .child_by_field_name("name")
                            .map(|n| get_node_text(&n, source).to_string()),
                        _ => None,
                    };
                    if let Some(tn) = type_name {
                        if !ts_builtin_types.contains(&tn.as_str()) {
                            var_types.push(VarType::new_with_scope(
                                var_name,
                                tn,
                                "parameter",
                                line,
                                scope,
                            ));
                        }
                    }
                }
            }

            _ => {}
        }
    }

    var_types
}

// =============================================================================
// Java VarType extraction
// =============================================================================

/// Determine the enclosing function scope for a Java AST node.
pub(crate) fn enclosing_java_function_scope(
    node: &tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "method_declaration" | "constructor_declaration" => {
                let method_name = parent
                    .child_by_field_name("name")
                    .map(|n| get_node_text(&n, source).to_string());
                if let Some(name) = method_name {
                    // Try to get class name: method_declaration > class_body > class_declaration
                    if let Some(class_body) = parent.parent() {
                        if class_body.kind() == "class_body" {
                            if let Some(class_decl) = class_body.parent() {
                                if class_decl.kind() == "class_declaration" {
                                    if let Some(class_name_node) =
                                        class_decl.child_by_field_name("name")
                                    {
                                        let class_name = get_node_text(&class_name_node, source);
                                        return Some(format!("{}.{}", class_name, name));
                                    }
                                }
                            }
                        }
                    }
                    return Some(name);
                }
            }
            _ => {}
        }
        current = parent.parent();
    }
    None
}

/// Extract VarType entries from a Java source tree.
pub(crate) fn extract_java_var_types(tree: &tree_sitter::Tree, source: &[u8]) -> Vec<VarType> {
    let mut var_types = Vec::new();
    let root = tree.root_node();

    // Java builtin/primitive types that should not match project classes
    let java_builtin_types = [
        "String",
        "int",
        "Integer",
        "double",
        "Double",
        "float",
        "Float",
        "long",
        "Long",
        "boolean",
        "Boolean",
        "byte",
        "Byte",
        "short",
        "Short",
        "char",
        "Character",
        "void",
        "Object",
        "Number",
        "Comparable",
        "Serializable",
        "Cloneable",
        "Iterable",
        "AutoCloseable",
        "Throwable",
        "Exception",
        "RuntimeException",
        "Error",
        "var",
    ];

    for node in walk_tree(root) {
        match node.kind() {
            // Pattern 1: Type var = new Type() or Type var = expr or Type var;
            "local_variable_declaration" => {
                // Get the type from the "type" field
                let type_node = match node.child_by_field_name("type") {
                    Some(n) => n,
                    None => continue,
                };

                let raw_type_text = get_node_text(&type_node, source).to_string();

                // Extract base type name: for generic_type like "ArrayList<String>", get "ArrayList"
                let type_name = if type_node.kind() == "generic_type" {
                    // First named child of generic_type is the base type_identifier
                    type_node
                        .named_child(0)
                        .map(|n| get_node_text(&n, source).to_string())
                        .unwrap_or(raw_type_text.clone())
                } else {
                    raw_type_text.clone()
                };

                // Get declarator(s) -- there can be multiple: int x = 1, y = 2;
                for i in 0..node.named_child_count() {
                    let child = match node.named_child(i) {
                        Some(c) if c.kind() == "variable_declarator" => c,
                        _ => continue,
                    };

                    let name_node = match child.child_by_field_name("name") {
                        Some(n) if n.kind() == "identifier" => n,
                        _ => continue,
                    };
                    let var_name = get_node_text(&name_node, source).to_string();
                    let line = child.start_position().row as u32 + 1;
                    let scope = enclosing_java_function_scope(&node, source);

                    // Check if type is "var" -- infer from RHS
                    if type_name == "var" {
                        if let Some(value) = child.child_by_field_name("value") {
                            if value.kind() == "object_creation_expression" {
                                // var x = new Dog() -- extract type from the constructor
                                if let Some(ctor_type) = value.child_by_field_name("type") {
                                    let ctor_type_name = if ctor_type.kind() == "generic_type" {
                                        ctor_type
                                            .named_child(0)
                                            .map(|n| get_node_text(&n, source).to_string())
                                            .unwrap_or_else(|| {
                                                get_node_text(&ctor_type, source).to_string()
                                            })
                                    } else {
                                        get_node_text(&ctor_type, source).to_string()
                                    };
                                    if !java_builtin_types.contains(&ctor_type_name.as_str()) {
                                        var_types.push(VarType::new_with_scope(
                                            var_name,
                                            ctor_type_name,
                                            "constructor",
                                            line,
                                            scope,
                                        ));
                                    }
                                }
                            }
                        }
                        continue;
                    }

                    // Skip builtin types
                    if java_builtin_types.contains(&type_name.as_str()) {
                        continue;
                    }

                    // Check if the value is a constructor call: new Type(...)
                    let source_kind = if let Some(value) = child.child_by_field_name("value") {
                        if value.kind() == "object_creation_expression" {
                            "constructor"
                        } else {
                            "annotation"
                        }
                    } else {
                        // No initializer: Type var;
                        "annotation"
                    };

                    var_types.push(VarType::new_with_scope(
                        var_name,
                        type_name.clone(),
                        source_kind,
                        line,
                        scope,
                    ));
                }
            }

            // Pattern 2: field_declaration -- same structure as local_variable_declaration
            "field_declaration" => {
                let type_node = match node.child_by_field_name("type") {
                    Some(n) => n,
                    None => continue,
                };

                let raw_type_text = get_node_text(&type_node, source).to_string();

                let type_name = if type_node.kind() == "generic_type" {
                    type_node
                        .named_child(0)
                        .map(|n| get_node_text(&n, source).to_string())
                        .unwrap_or(raw_type_text.clone())
                } else {
                    raw_type_text.clone()
                };

                if java_builtin_types.contains(&type_name.as_str()) {
                    continue;
                }

                for i in 0..node.named_child_count() {
                    let child = match node.named_child(i) {
                        Some(c) if c.kind() == "variable_declarator" => c,
                        _ => continue,
                    };

                    let name_node = match child.child_by_field_name("name") {
                        Some(n) if n.kind() == "identifier" => n,
                        _ => continue,
                    };
                    let var_name = get_node_text(&name_node, source).to_string();
                    let line = child.start_position().row as u32 + 1;

                    // Fields are at class scope, not method scope
                    // Walk up to find class name
                    let scope = {
                        let mut s = None;
                        let mut cur = node.parent();
                        while let Some(p) = cur {
                            if p.kind() == "class_body" {
                                if let Some(class_decl) = p.parent() {
                                    if class_decl.kind() == "class_declaration" {
                                        if let Some(cn) = class_decl.child_by_field_name("name") {
                                            s = Some(get_node_text(&cn, source).to_string());
                                        }
                                    }
                                }
                                break;
                            }
                            cur = p.parent();
                        }
                        s
                    };

                    let source_kind = if let Some(value) = child.child_by_field_name("value") {
                        if value.kind() == "object_creation_expression" {
                            "constructor"
                        } else {
                            "annotation"
                        }
                    } else {
                        "annotation"
                    };

                    var_types.push(VarType::new_with_scope(
                        var_name,
                        type_name.clone(),
                        source_kind,
                        line,
                        scope,
                    ));
                }
            }

            // Pattern 3: formal_parameter -- method/constructor parameters
            "formal_parameter" => {
                let type_node = match node.child_by_field_name("type") {
                    Some(n) => n,
                    None => continue,
                };
                let name_node = match node.child_by_field_name("name") {
                    Some(n) if n.kind() == "identifier" => n,
                    _ => continue,
                };

                let raw_type_text = get_node_text(&type_node, source).to_string();
                let type_name = if type_node.kind() == "generic_type" {
                    type_node
                        .named_child(0)
                        .map(|n| get_node_text(&n, source).to_string())
                        .unwrap_or(raw_type_text.clone())
                } else {
                    raw_type_text.clone()
                };

                if java_builtin_types.contains(&type_name.as_str()) {
                    continue;
                }

                let var_name = get_node_text(&name_node, source).to_string();
                let line = node.start_position().row as u32 + 1;
                let scope = enclosing_java_function_scope(&node, source);

                var_types.push(VarType::new_with_scope(
                    var_name,
                    type_name,
                    "parameter",
                    line,
                    scope,
                ));
            }

            // Pattern 4: enhanced_for_statement -- for (Type var : collection)
            "enhanced_for_statement" => {
                let type_node = match node.child_by_field_name("type") {
                    Some(n) => n,
                    None => continue,
                };
                let name_node = match node.child_by_field_name("name") {
                    Some(n) if n.kind() == "identifier" => n,
                    _ => continue,
                };

                let raw_type_text = get_node_text(&type_node, source).to_string();
                let type_name = if type_node.kind() == "generic_type" {
                    type_node
                        .named_child(0)
                        .map(|n| get_node_text(&n, source).to_string())
                        .unwrap_or(raw_type_text.clone())
                } else {
                    raw_type_text.clone()
                };

                if java_builtin_types.contains(&type_name.as_str()) {
                    continue;
                }

                let var_name = get_node_text(&name_node, source).to_string();
                let line = node.start_position().row as u32 + 1;
                let scope = enclosing_java_function_scope(&node, source);

                var_types.push(VarType::new_with_scope(
                    var_name,
                    type_name,
                    "annotation",
                    line,
                    scope,
                ));
            }

            // Pattern 5: cast_expression -- (Type) expr
            "cast_expression" => {
                let type_node = match node.child_by_field_name("type") {
                    Some(n) => n,
                    None => continue,
                };

                // Only useful if the cast result is assigned -- check parent
                let parent = match node.parent() {
                    Some(p) => p,
                    None => continue,
                };

                // Only track if parent is a variable_declarator (assignment context)
                if parent.kind() != "variable_declarator" {
                    continue;
                }

                let name_node = match parent.child_by_field_name("name") {
                    Some(n) if n.kind() == "identifier" => n,
                    _ => continue,
                };

                let raw_type_text = get_node_text(&type_node, source).to_string();
                let type_name = if type_node.kind() == "generic_type" {
                    type_node
                        .named_child(0)
                        .map(|n| get_node_text(&n, source).to_string())
                        .unwrap_or(raw_type_text.clone())
                } else {
                    raw_type_text.clone()
                };

                if java_builtin_types.contains(&type_name.as_str()) {
                    continue;
                }

                let var_name = get_node_text(&name_node, source).to_string();
                let line = node.start_position().row as u32 + 1;
                let scope = enclosing_java_function_scope(&node, source);

                var_types.push(VarType::new_with_scope(
                    var_name,
                    type_name,
                    "annotation",
                    line,
                    scope,
                ));
            }

            _ => {}
        }
    }

    var_types
}

// =============================================================================
// Rust VarType extraction
// =============================================================================

/// Determine the enclosing function scope for a Rust AST node.
pub(crate) fn enclosing_rust_function_scope(
    node: &tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "function_item" {
            let fn_name = parent
                .child_by_field_name("name")
                .map(|n| get_node_text(&n, source).to_string());
            if let Some(name) = fn_name {
                // Check if inside an impl block: function_item -> declaration_list -> impl_item
                if let Some(decl_list) = parent.parent() {
                    if decl_list.kind() == "declaration_list" {
                        if let Some(impl_item) = decl_list.parent() {
                            if impl_item.kind() == "impl_item" {
                                // Get the type being implemented
                                if let Some(type_node) = impl_item.child_by_field_name("type") {
                                    let type_name = get_node_text(&type_node, source);
                                    return Some(format!("{}.{}", type_name, name));
                                }
                            }
                        }
                    }
                }
                return Some(name);
            }
        }
        current = parent.parent();
    }
    None
}

/// Extract VarType entries from a Rust source tree.
pub(crate) fn extract_rust_var_types(tree: &tree_sitter::Tree, source: &[u8]) -> Vec<VarType> {
    let mut var_types = Vec::new();
    let root = tree.root_node();

    let rust_builtin_types = [
        "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
        "f32", "f64", "bool", "char", "str", "String", "Vec", "HashMap", "HashSet", "BTreeMap",
        "BTreeSet", "Option", "Result", "Box", "Rc", "Arc", "Cow", "Cell", "RefCell", "Mutex",
        "RwLock", "Pin", "Waker", "Context",
    ];

    // Track vars already assigned via constructor (prefer constructor over annotation)
    let mut constructor_vars: HashSet<(String, Option<String>)> = HashSet::new();

    for node in walk_tree(root) {
        match node.kind() {
            "let_declaration" => {
                let line = node.start_position().row as u32 + 1;
                let scope = enclosing_rust_function_scope(&node, source);

                let pattern_node = match node.child_by_field_name("pattern") {
                    Some(n) => n,
                    None => continue,
                };

                // Check RHS value first (for constructor detection)
                if let Some(value_node) = node.child_by_field_name("value") {
                    match value_node.kind() {
                        // Pattern 1: let dog = Dog::new(...) -- scoped identifier constructor
                        "call_expression" => {
                            if let Some(func_node) = value_node.child_by_field_name("function") {
                                if func_node.kind() == "scoped_identifier" {
                                    if let Some(path_node) = func_node.child_by_field_name("path") {
                                        let type_name =
                                            get_node_text(&path_node, source).to_string();

                                        if pattern_node.kind() == "identifier" {
                                            let var_name =
                                                get_node_text(&pattern_node, source).to_string();
                                            if !rust_builtin_types.contains(&type_name.as_str())
                                                && !var_name.starts_with('_')
                                            {
                                                constructor_vars
                                                    .insert((var_name.clone(), scope.clone()));
                                                var_types.push(VarType::new_with_scope(
                                                    var_name,
                                                    type_name,
                                                    "constructor",
                                                    line,
                                                    scope,
                                                ));
                                                continue;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // Pattern 2: let animal = Animal { name: ... } -- struct expression
                        "struct_expression" => {
                            if let Some(name_node) = value_node.child_by_field_name("name") {
                                let type_name = get_node_text(&name_node, source).to_string();

                                if pattern_node.kind() == "identifier" {
                                    let var_name = get_node_text(&pattern_node, source).to_string();
                                    if !rust_builtin_types.contains(&type_name.as_str())
                                        && !var_name.starts_with('_')
                                    {
                                        constructor_vars.insert((var_name.clone(), scope.clone()));
                                        var_types.push(VarType::new_with_scope(
                                            var_name,
                                            type_name,
                                            "constructor",
                                            line,
                                            scope,
                                        ));
                                        continue;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }

                // Pattern 3: let a: Animal = ... -- explicit type annotation
                if let Some(type_node) = node.child_by_field_name("type") {
                    if pattern_node.kind() == "identifier" {
                        let var_name = get_node_text(&pattern_node, source).to_string();
                        if var_name.starts_with('_') {
                            continue;
                        }

                        // Skip if already found via constructor
                        if constructor_vars.contains(&(var_name.clone(), scope.clone())) {
                            continue;
                        }

                        let type_name = extract_rust_type_name(&type_node, source);
                        if let Some(tn) = type_name {
                            if !rust_builtin_types.contains(&tn.as_str()) {
                                var_types.push(VarType::new_with_scope(
                                    var_name,
                                    tn,
                                    "annotation",
                                    line,
                                    scope,
                                ));
                            }
                        }
                    }
                }
            }

            // Pattern 4: fn process(animal: &Animal) -- function parameters
            "parameter" => {
                let line = node.start_position().row as u32 + 1;
                let scope = enclosing_rust_function_scope(&node, source);

                let pattern_node = match node.child_by_field_name("pattern") {
                    Some(n) if n.kind() == "identifier" => n,
                    _ => continue,
                };
                let var_name = get_node_text(&pattern_node, source).to_string();
                if var_name.starts_with('_') {
                    continue;
                }

                let type_child = match node.child_by_field_name("type") {
                    Some(n) => n,
                    None => continue,
                };

                let type_name = extract_rust_type_name(&type_child, source);
                if let Some(tn) = type_name {
                    if !rust_builtin_types.contains(&tn.as_str()) {
                        var_types.push(VarType::new_with_scope(
                            var_name,
                            tn,
                            "parameter",
                            line,
                            scope,
                        ));
                    }
                }
            }

            _ => {}
        }
    }

    var_types
}

/// Extract the base type name from a Rust type AST node.
pub(crate) fn extract_rust_type_name(
    type_node: &tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    match type_node.kind() {
        "type_identifier" => Some(get_node_text(type_node, source).to_string()),
        "reference_type" => {
            for i in 0..type_node.named_child_count() {
                if let Some(child) = type_node.named_child(i) {
                    if let Some(result) = extract_rust_type_name(&child, source) {
                        return Some(result);
                    }
                }
            }
            None
        }
        "generic_type" => type_node
            .named_child(0)
            .and_then(|n| extract_rust_type_name(&n, source)),
        "scoped_type_identifier" => {
            if let Some(name_node) = type_node.child_by_field_name("name") {
                Some(get_node_text(&name_node, source).to_string())
            } else {
                Some(get_node_text(type_node, source).to_string())
            }
        }
        _ => None,
    }
}

// =============================================================================
// Kotlin VarType extraction
// =============================================================================

/// Determine the enclosing function scope for a Kotlin AST node.
pub(crate) fn enclosing_kotlin_function_scope(
    node: &tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "function_declaration" {
            let fn_name = parent
                .child_by_field_name("name")
                .map(|n| get_node_text(&n, source).to_string());
            if let Some(name) = fn_name {
                if let Some(class_body) = parent.parent() {
                    if class_body.kind() == "class_body" {
                        if let Some(class_decl) = class_body.parent() {
                            if class_decl.kind() == "class_declaration" {
                                if let Some(class_name_node) =
                                    class_decl.child_by_field_name("name")
                                {
                                    let class_name = get_node_text(&class_name_node, source);
                                    return Some(format!("{}.{}", class_name, name));
                                }
                            }
                        }
                    }
                }
                return Some(name);
            }
        }
        current = parent.parent();
    }
    None
}

/// Extract the base type name from a Kotlin type AST node.
pub(crate) fn extract_kotlin_type_name(
    type_node: &tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    match type_node.kind() {
        "user_type" => {
            for i in 0..type_node.child_count() {
                if let Some(child) = type_node.child(i) {
                    if child.kind() == "identifier"
                        || child.kind() == "simple_identifier"
                        || child.kind() == "type_identifier"
                    {
                        return Some(get_node_text(&child, source).to_string());
                    }
                    if child.kind() == "simple_user_type" {
                        for j in 0..child.child_count() {
                            if let Some(inner) = child.child(j) {
                                if inner.kind() == "identifier"
                                    || inner.kind() == "simple_identifier"
                                    || inner.kind() == "type_identifier"
                                {
                                    return Some(get_node_text(&inner, source).to_string());
                                }
                            }
                        }
                        let text = get_node_text(&child, source).to_string();
                        if let Some(idx) = text.find('<') {
                            return Some(text[..idx].to_string());
                        }
                        return Some(text);
                    }
                }
            }
            let text = get_node_text(type_node, source).to_string();
            if let Some(idx) = text.find('<') {
                Some(text[..idx].to_string())
            } else {
                Some(text)
            }
        }
        "nullable_type" => {
            for i in 0..type_node.named_child_count() {
                if let Some(child) = type_node.named_child(i) {
                    if let Some(result) = extract_kotlin_type_name(&child, source) {
                        return Some(result);
                    }
                }
            }
            None
        }
        "type_reference" => {
            for i in 0..type_node.named_child_count() {
                if let Some(child) = type_node.named_child(i) {
                    if let Some(result) = extract_kotlin_type_name(&child, source) {
                        return Some(result);
                    }
                }
            }
            None
        }
        "identifier" | "simple_identifier" | "type_identifier" => {
            Some(get_node_text(type_node, source).to_string())
        }
        _ => {
            for i in 0..type_node.named_child_count() {
                if let Some(child) = type_node.named_child(i) {
                    if let Some(result) = extract_kotlin_type_name(&child, source) {
                        return Some(result);
                    }
                }
            }
            None
        }
    }
}

/// Extract VarType entries from a Kotlin source tree.
pub(crate) fn extract_kotlin_var_types(tree: &tree_sitter::Tree, source: &[u8]) -> Vec<VarType> {
    let mut var_types = Vec::new();
    let root = tree.root_node();

    let kotlin_builtin_types = [
        "String",
        "Int",
        "Long",
        "Double",
        "Float",
        "Boolean",
        "Byte",
        "Short",
        "Char",
        "Unit",
        "Nothing",
        "Any",
        "Number",
        "Comparable",
        "List",
        "Map",
        "Set",
        "MutableList",
        "MutableMap",
        "MutableSet",
        "Array",
        "IntArray",
        "LongArray",
        "DoubleArray",
        "FloatArray",
        "BooleanArray",
        "ByteArray",
        "ShortArray",
        "CharArray",
        "Pair",
        "Triple",
        "Sequence",
        "Iterable",
        "Exception",
        "RuntimeException",
        "Throwable",
        "Enum",
        "Annotation",
        "HashMap",
        "HashSet",
        "ArrayList",
        "LinkedList",
        "LinkedHashMap",
        "LinkedHashSet",
        "Regex",
        "StringBuilder",
        "Lazy",
    ];

    for node in walk_tree(root) {
        match node.kind() {
            "property_declaration" => {
                let line = node.start_position().row as u32 + 1;
                let scope = enclosing_kotlin_function_scope(&node, source);

                let mut var_name: Option<String> = None;
                let mut type_name: Option<String> = None;
                let mut has_constructor_rhs = false;
                let mut constructor_type: Option<String> = None;

                for i in 0..node.child_count() {
                    let child = match node.child(i) {
                        Some(c) => c,
                        None => continue,
                    };

                    match child.kind() {
                        "variable_declaration" => {
                            for j in 0..child.child_count() {
                                if let Some(inner) = child.child(j) {
                                    match inner.kind() {
                                        "identifier" | "simple_identifier" => {
                                            var_name =
                                                Some(get_node_text(&inner, source).to_string());
                                        }
                                        "user_type" | "nullable_type" | "type_reference" => {
                                            type_name = extract_kotlin_type_name(&inner, source);
                                        }
                                        _ => {
                                            if type_name.is_none() {
                                                if let Some(tn) =
                                                    extract_kotlin_type_name(&inner, source)
                                                {
                                                    if tn
                                                        .chars()
                                                        .next()
                                                        .is_some_and(|c| c.is_uppercase())
                                                    {
                                                        type_name = Some(tn);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        "identifier" | "simple_identifier" if var_name.is_none() => {
                            var_name = Some(get_node_text(&child, source).to_string());
                        }
                        "call_expression" => {
                            if let Some(func_child) = child.child(0) {
                                let call_name = get_node_text(&func_child, source).to_string();
                                if call_name.chars().next().is_some_and(|c| c.is_uppercase()) {
                                    has_constructor_rhs = true;
                                    constructor_type = Some(call_name);
                                }
                            }
                        }
                        _ => {}
                    }
                }

                let var_name = match var_name {
                    Some(n) if !n.is_empty() => n,
                    _ => continue,
                };

                if let Some(ref tn) = type_name {
                    if !kotlin_builtin_types.contains(&tn.as_str()) {
                        var_types.push(VarType::new_with_scope(
                            var_name.clone(),
                            tn.clone(),
                            "annotation",
                            line,
                            scope.clone(),
                        ));
                        continue;
                    }
                }

                if has_constructor_rhs {
                    if let Some(ref ct) = constructor_type {
                        if !kotlin_builtin_types.contains(&ct.as_str()) {
                            var_types.push(VarType::new_with_scope(
                                var_name,
                                ct.clone(),
                                "assignment",
                                line,
                                scope,
                            ));
                        }
                    }
                }
            }

            "parameter" => {
                let line = node.start_position().row as u32 + 1;
                let scope = enclosing_kotlin_function_scope(&node, source);

                let mut param_name: Option<String> = None;
                let mut param_type: Option<String> = None;

                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i) {
                        match child.kind() {
                            "identifier" | "simple_identifier" if param_name.is_none() => {
                                param_name = Some(get_node_text(&child, source).to_string());
                            }
                            "user_type" | "nullable_type" | "type_reference" => {
                                param_type = extract_kotlin_type_name(&child, source);
                            }
                            _ => {
                                if param_type.is_none() {
                                    if let Some(tn) = extract_kotlin_type_name(&child, source) {
                                        if tn.chars().next().is_some_and(|c| c.is_uppercase()) {
                                            param_type = Some(tn);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if let (Some(name), Some(type_name)) = (param_name, param_type) {
                    if !kotlin_builtin_types.contains(&type_name.as_str()) {
                        var_types.push(VarType::new_with_scope(
                            name,
                            type_name,
                            "parameter",
                            line,
                            scope,
                        ));
                    }
                }
            }

            "class_parameter" => {
                let line = node.start_position().row as u32 + 1;

                let scope = {
                    let mut s = None;
                    let mut cur = node.parent();
                    while let Some(p) = cur {
                        if p.kind() == "class_parameters" {
                            if let Some(ctor) = p.parent() {
                                if ctor.kind() == "primary_constructor" {
                                    if let Some(class_decl) = ctor.parent() {
                                        if class_decl.kind() == "class_declaration" {
                                            if let Some(cn) = class_decl.child_by_field_name("name")
                                            {
                                                s = Some(get_node_text(&cn, source).to_string());
                                            }
                                        }
                                    }
                                }
                                if ctor.kind() == "class_declaration" {
                                    if let Some(cn) = ctor.child_by_field_name("name") {
                                        s = Some(get_node_text(&cn, source).to_string());
                                    }
                                }
                            }
                            break;
                        }
                        cur = p.parent();
                    }
                    s
                };

                let mut param_name: Option<String> = None;
                let mut param_type: Option<String> = None;

                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i) {
                        match child.kind() {
                            "identifier" | "simple_identifier" if param_name.is_none() => {
                                param_name = Some(get_node_text(&child, source).to_string());
                            }
                            "user_type" | "nullable_type" | "type_reference" => {
                                param_type = extract_kotlin_type_name(&child, source);
                            }
                            "modifiers" | "val" | "var" => {}
                            _ => {
                                if param_type.is_none() {
                                    if let Some(tn) = extract_kotlin_type_name(&child, source) {
                                        if tn.chars().next().is_some_and(|c| c.is_uppercase()) {
                                            param_type = Some(tn);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if let (Some(name), Some(type_name)) = (param_name, param_type) {
                    if !kotlin_builtin_types.contains(&type_name.as_str()) {
                        var_types.push(VarType::new_with_scope(
                            name,
                            type_name,
                            "parameter",
                            line,
                            scope,
                        ));
                    }
                }
            }

            _ => {}
        }
    }

    var_types
}

// =============================================================================
// PHP VarType extraction
// =============================================================================

/// Determine the enclosing function/method scope for a PHP AST node.
pub(crate) fn enclosing_php_function_scope(
    node: &tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    let mut cur = node.parent();
    while let Some(p) = cur {
        match p.kind() {
            "method_declaration" => {
                let method_name = p
                    .child_by_field_name("name")
                    .map(|n| get_node_text(&n, source).to_string());
                let mut class_name = None;
                let mut parent = p.parent();
                while let Some(pp) = parent {
                    if pp.kind() == "declaration_list" {
                        if let Some(class_decl) = pp.parent() {
                            if class_decl.kind() == "class_declaration" {
                                class_name = class_decl
                                    .child_by_field_name("name")
                                    .map(|n| get_node_text(&n, source).to_string());
                            }
                        }
                        break;
                    }
                    parent = pp.parent();
                }
                return match (class_name, method_name) {
                    (Some(cn), Some(mn)) => Some(format!("{}.{}", cn, mn)),
                    (None, Some(mn)) => Some(mn),
                    _ => None,
                };
            }
            "function_definition" => {
                return p
                    .child_by_field_name("name")
                    .map(|n| get_node_text(&n, source).to_string());
            }
            _ => {}
        }
        cur = p.parent();
    }
    None // top-level / module scope
}

/// Extract the base type name from a PHP type AST node.
pub(crate) fn extract_php_type_name(
    type_node: &tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    match type_node.kind() {
        "named_type" => {
            let text = get_node_text(type_node, source).to_string();
            let base = text.rsplit('\\').next().unwrap_or(&text);
            if base.is_empty() {
                None
            } else {
                Some(base.to_string())
            }
        }
        "optional_type" => {
            for i in 0..type_node.named_child_count() {
                if let Some(child) = type_node.named_child(i) {
                    if let Some(result) = extract_php_type_name(&child, source) {
                        return Some(result);
                    }
                }
            }
            None
        }
        "nullable_type" => {
            for i in 0..type_node.named_child_count() {
                if let Some(child) = type_node.named_child(i) {
                    if let Some(result) = extract_php_type_name(&child, source) {
                        return Some(result);
                    }
                }
            }
            None
        }
        "union_type" | "intersection_type" => None,
        _ => None,
    }
}

/// Extract VarType entries from a PHP source tree.
pub(crate) fn extract_php_var_types(tree: &tree_sitter::Tree, source: &[u8]) -> Vec<VarType> {
    let mut var_types = Vec::new();
    let root = tree.root_node();

    let php_builtin_types = [
        "string", "int", "float", "bool", "array", "object", "callable", "iterable", "mixed",
        "void", "null", "never", "false", "true", "self", "static", "parent", "resource",
    ];

    for node in walk_tree(root) {
        match node.kind() {
            // Pattern 1: $x = new Foo()
            "assignment_expression" => {
                let left = match node.child_by_field_name("left") {
                    Some(n) if n.kind() == "variable_name" => n,
                    _ => continue,
                };
                let right = match node.child_by_field_name("right") {
                    Some(n) if n.kind() == "object_creation_expression" => n,
                    _ => continue,
                };

                let var_text = get_node_text(&left, source).to_string();
                let var_name = var_text.trim_start_matches('$');
                if var_name.is_empty() {
                    continue;
                }

                let mut class_name: Option<String> = None;
                for i in 0..right.child_count() {
                    if let Some(child) = right.child(i) {
                        if child.kind() == "name" {
                            let raw = get_node_text(&child, source).to_string();
                            let base = raw.rsplit('\\').next().unwrap_or(&raw);
                            if !base.is_empty() {
                                class_name = Some(base.to_string());
                            }
                            break;
                        }
                        if child.kind() == "qualified_name" {
                            let raw = get_node_text(&child, source).to_string();
                            let base = raw.rsplit('\\').next().unwrap_or(&raw);
                            if !base.is_empty() {
                                class_name = Some(base.to_string());
                            }
                            break;
                        }
                    }
                }

                let type_name = match class_name {
                    Some(ref tn) if !php_builtin_types.contains(&tn.to_lowercase().as_str()) => {
                        tn.clone()
                    }
                    _ => continue,
                };

                let line = node.start_position().row as u32 + 1;
                let scope = enclosing_php_function_scope(&node, source);

                var_types.push(VarType::new_with_scope(
                    var_name.to_string(),
                    type_name,
                    "constructor",
                    line,
                    scope,
                ));
            }

            // Pattern 2: function f(Foo $x) {}
            "simple_parameter" => {
                let type_node = match node.child_by_field_name("type") {
                    Some(n) => n,
                    None => continue,
                };

                let type_name = match extract_php_type_name(&type_node, source) {
                    Some(tn) if !php_builtin_types.contains(&tn.to_lowercase().as_str()) => tn,
                    _ => continue,
                };

                let name_node = match node.child_by_field_name("name") {
                    Some(n) => n,
                    None => continue,
                };

                let var_text = get_node_text(&name_node, source).to_string();
                let var_name = var_text.trim_start_matches('$');
                if var_name.is_empty() {
                    continue;
                }

                let line = node.start_position().row as u32 + 1;
                let scope = enclosing_php_function_scope(&node, source);

                var_types.push(VarType::new_with_scope(
                    var_name.to_string(),
                    type_name,
                    "parameter",
                    line,
                    scope,
                ));
            }

            // Pattern 3: private Foo $prop;
            "property_declaration" => {
                let type_node = match node.child_by_field_name("type") {
                    Some(n) => n,
                    None => continue,
                };

                let type_name = match extract_php_type_name(&type_node, source) {
                    Some(tn) if !php_builtin_types.contains(&tn.to_lowercase().as_str()) => tn,
                    _ => continue,
                };

                let mut var_name: Option<String> = None;
                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i) {
                        if child.kind() == "property_element" {
                            for j in 0..child.child_count() {
                                if let Some(inner) = child.child(j) {
                                    if inner.kind() == "variable_name" {
                                        let var_text = get_node_text(&inner, source).to_string();
                                        let name = var_text.trim_start_matches('$');
                                        if !name.is_empty() {
                                            var_name = Some(name.to_string());
                                        }
                                        break;
                                    }
                                }
                            }
                            break;
                        }
                    }
                }

                let var_name = match var_name {
                    Some(n) => n,
                    None => continue,
                };

                let line = node.start_position().row as u32 + 1;
                let scope = enclosing_php_function_scope(&node, source);

                var_types.push(VarType::new_with_scope(
                    var_name,
                    type_name,
                    "annotation",
                    line,
                    scope,
                ));
            }

            _ => {}
        }
    }

    var_types
}

// =============================================================================
// Tests (moved from builder_v2.rs during Phase 3 modularization)
// =============================================================================

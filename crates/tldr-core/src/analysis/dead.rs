//! Dead code analysis (spec Section 2.2.3)
//!
//! Find functions that are never called (dead code).
//!
//! # Exclusion Patterns (not considered dead)
//! - App entry: main, __main__, cli, app, run, start, create_app
//! - Test: test_*, pytest_*, Test*, Benchmark*, setUp, tearDown
//! - Lifecycle: onCreate, onStart, onDestroy, init, destroy, etc.
//! - Handlers: handle*, Handle*, on_*, before_*, after_*
//! - Hooks: load, configure, request, response, invoke, call, execute
//! - HTTP: ServeHTTP, doGet, doPost, handler
//! - Dunder methods (__init__, __str__, etc.)
//! - Custom patterns from entry_points parameter
//!
//! # Performance
//! - O(E + V) where E = edges, V = functions

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::types::{DeadCodeReport, FunctionRef, ProjectCallGraph};
use crate::TldrResult;

// Re-export for convenience
#[allow(unused_imports)]
use super::refcount::is_rescued_by_refcount;

/// Analyze dead (unreachable) code.
///
/// # Arguments
/// * `call_graph` - Project call graph
/// * `all_functions` - All functions in the project
/// * `entry_points` - Optional custom entry point patterns
///
/// # Returns
/// * `Ok(DeadCodeReport)` - Dead code analysis results
pub fn dead_code_analysis(
    call_graph: &ProjectCallGraph,
    all_functions: &[FunctionRef],
    entry_points: Option<&[String]>,
) -> TldrResult<DeadCodeReport> {
    // Build set of all functions that are called
    let mut called_functions: HashSet<FunctionRef> = HashSet::new();

    for edge in call_graph.edges() {
        called_functions.insert(FunctionRef::new(
            edge.dst_file.clone(),
            edge.dst_func.clone(),
        ));
    }

    // Build set of all functions that call others (callers are entry points)
    let mut callers: HashSet<FunctionRef> = HashSet::new();
    for edge in call_graph.edges() {
        callers.insert(FunctionRef::new(
            edge.src_file.clone(),
            edge.src_func.clone(),
        ));
    }

    // Find dead functions, classifying into "definitely dead" and "possibly dead"
    let mut dead_functions: Vec<FunctionRef> = Vec::new();
    let mut possibly_dead: Vec<FunctionRef> = Vec::new();
    let mut by_file: HashMap<PathBuf, Vec<String>> = HashMap::new();

    for func_ref in all_functions {
        // Skip if called by anyone
        if called_functions.contains(func_ref) {
            continue;
        }

        // Skip if matches entry point patterns
        if is_entry_point_name(&func_ref.name, entry_points) {
            continue;
        }

        // Skip dunder methods (__init__, __str__, etc.) - called implicitly by runtime
        // Check both bare name and Class.method format (also supports Lua module:method)
        let bare_name = if func_ref.name.contains('.') {
            func_ref.name.rsplit('.').next().unwrap_or(&func_ref.name)
        } else if func_ref.name.contains(':') {
            func_ref.name.rsplit(':').next().unwrap_or(&func_ref.name)
        } else {
            &func_ref.name
        };

        // PHP magic methods (leading __ without trailing __)
        // These are implicitly called by PHP runtime
        static PHP_MAGIC: &[&str] = &[
            "__construct",
            "__destruct",
            "__call",
            "__callStatic",
            "__get",
            "__set",
            "__isset",
            "__unset",
            "__sleep",
            "__wakeup",
            "__serialize",
            "__unserialize",
            "__toString",
            "__invoke",
            "__set_state",
            "__clone",
            "__debugInfo",
        ];
        if PHP_MAGIC.contains(&bare_name) {
            continue;
        }

        if bare_name.starts_with("__") && bare_name.ends_with("__") {
            continue;
        }

        // Skip trait/interface methods (they are called implicitly by the type system)
        if func_ref.is_trait_method {
            continue;
        }

        // Skip test functions (they are called by the test runner)
        if func_ref.is_test {
            continue;
        }

        // Skip decorated/annotated functions (they are called by frameworks)
        if func_ref.has_decorator {
            continue;
        }

        // Classify: public/exported but uncalled -> possibly dead (may be API surface)
        // Private/unenriched and uncalled -> definitely dead
        if func_ref.is_public {
            possibly_dead.push(func_ref.clone());
        } else {
            dead_functions.push(func_ref.clone());
            by_file
                .entry(func_ref.file.clone())
                .or_default()
                .push(func_ref.name.clone());
        }
    }

    let total_dead = dead_functions.len();
    let total_possibly_dead = possibly_dead.len();
    let total_functions = all_functions.len();
    // med-low-schema-cleanup-v1 (N15): round percentage to 2 decimal places at
    // serialization to avoid 15-digit IEEE-754 noise in the JSON output
    // (e.g. `0.10893246187363835`). 2 decimals is the human-meaningful
    // precision for "percent dead".
    let dead_percentage = if total_functions > 0 {
        round_pct((total_dead as f64 / total_functions as f64) * 100.0)
    } else {
        0.0
    };

    Ok(DeadCodeReport {
        dead_functions,
        possibly_dead,
        by_file,
        total_dead,
        total_possibly_dead,
        total_functions,
        dead_percentage,
    })
}

/// Round a percentage value to 2 decimal places.
///
/// med-low-schema-cleanup-v1 (N15): clamps `f64` percentage fields to a
/// human-meaningful precision (`12.34`) so the JSON output is stable
/// across platforms / floating-point rounding.
#[inline]
fn round_pct(p: f64) -> f64 {
    (p * 100.0).round() / 100.0
}

/// Analyze dead (unreachable) code using reference counting instead of a call graph.
///
/// This is an alternative to `dead_code_analysis()` that uses identifier reference
/// counts to determine liveness. A function with `ref_count > 1` is considered alive
/// because it is referenced somewhere beyond its definition. A function with
/// `ref_count == 1` (only definition) is dead, subject to the same exclusion patterns
/// as the call-graph-based analysis.
///
/// Short names (< 3 characters) need a higher refcount threshold (>= 5) to be rescued,
/// since collision-prone names like `i`, `j`, `id` inflate counts artificially.
///
/// # Arguments
/// * `all_functions` - All functions in the project
/// * `ref_counts` - Map of identifier name to occurrence count across codebase
/// * `entry_points` - Optional custom entry point patterns
///
/// # Returns
/// * `Ok(DeadCodeReport)` - Dead code analysis results (backward compatible)
pub fn dead_code_analysis_refcount(
    all_functions: &[FunctionRef],
    ref_counts: &HashMap<String, usize>,
    entry_points: Option<&[String]>,
) -> TldrResult<DeadCodeReport> {
    let mut dead_functions: Vec<FunctionRef> = Vec::new();
    let mut possibly_dead: Vec<FunctionRef> = Vec::new();
    let mut by_file: HashMap<PathBuf, Vec<String>> = HashMap::new();

    for func_ref in all_functions {
        // Skip if matches entry point patterns (C4)
        if is_entry_point_name(&func_ref.name, entry_points) {
            continue;
        }

        // Skip dunder methods (__init__, __str__, etc.) - called implicitly by runtime (C5)
        // Check both bare name and Class.method format (also supports Lua module:method)
        let bare_name = if func_ref.name.contains('.') {
            func_ref.name.rsplit('.').next().unwrap_or(&func_ref.name)
        } else if func_ref.name.contains(':') {
            func_ref.name.rsplit(':').next().unwrap_or(&func_ref.name)
        } else {
            &func_ref.name
        };

        // PHP magic methods (leading __ without trailing __)
        // These are implicitly called by PHP runtime
        static PHP_MAGIC: &[&str] = &[
            "__construct",
            "__destruct",
            "__call",
            "__callStatic",
            "__get",
            "__set",
            "__isset",
            "__unset",
            "__sleep",
            "__wakeup",
            "__serialize",
            "__unserialize",
            "__toString",
            "__invoke",
            "__set_state",
            "__clone",
            "__debugInfo",
        ];
        if PHP_MAGIC.contains(&bare_name) {
            continue;
        }

        if bare_name.starts_with("__") && bare_name.ends_with("__") {
            continue;
        }

        // Skip trait/interface methods (C6)
        if func_ref.is_trait_method {
            continue;
        }

        // Skip test functions (C7)
        if func_ref.is_test {
            continue;
        }

        // Skip decorated/annotated functions (C8)
        if func_ref.has_decorator {
            continue;
        }

        // Check refcount: if rescued by refcount (ref_count > 1, name >= 3 chars) -> alive (C2)
        if is_rescued_by_refcount(&func_ref.name, ref_counts) {
            continue;
        }

        // Not rescued -> classify by visibility (C9)
        // Enrich with the actual ref_count for the output
        let mut enriched = func_ref.clone();
        // Look up by bare name (for Class.method, use the bare method name for refcount)
        let lookup_name = bare_name;
        enriched.ref_count = ref_counts.get(lookup_name).copied().unwrap_or(0) as u32;

        if func_ref.is_public {
            possibly_dead.push(enriched);
        } else {
            by_file
                .entry(func_ref.file.clone())
                .or_default()
                .push(func_ref.name.clone());
            dead_functions.push(enriched);
        }
    }

    let total_dead = dead_functions.len();
    let total_possibly_dead = possibly_dead.len();
    let total_functions = all_functions.len();
    // med-low-schema-cleanup-v1 (N15): see `round_pct`.
    let dead_percentage = if total_functions > 0 {
        round_pct((total_dead as f64 / total_functions as f64) * 100.0)
    } else {
        0.0
    };

    Ok(DeadCodeReport {
        dead_functions,
        possibly_dead,
        by_file,
        total_dead,
        total_possibly_dead,
        total_functions,
        dead_percentage,
    })
}

/// Check if a function name matches entry point patterns
fn is_entry_point_name(name: &str, custom_patterns: Option<&[String]>) -> bool {
    // Standard entry point names
    let standard_patterns = [
        // Application entry points
        "main",
        "__main__",
        "cli",
        "app",
        "run",
        "start",
        // Test setup/teardown
        "setup",
        "teardown",
        "setUp",
        "tearDown",
        // Python ASGI/WSGI
        "create_app",
        "make_app",
        // Go HTTP
        "ServeHTTP",
        "Handler",
        "handler",
        // C/system callbacks
        "OnLoad",
        "OnInit",
        "OnExit",
        // Android/Kotlin lifecycle
        "onCreate",
        "onStart",
        "onStop",
        "onResume",
        "onPause",
        "onDestroy",
        "onBind",
        "onClick",
        "onCreateView",
        // Java Servlet / Spring
        "doGet",
        "doPost",
        "doPut",
        "doDelete",
        "init",
        "destroy",
        "service",
        // Plugin/middleware hooks
        "load",
        "configure",
        "request",
        "response",
        "error",
        "invoke",
        "call",
        "execute",
        // Next.js instrumentation hooks
        "register",
        "onRequestError",
    ];

    if standard_patterns.contains(&name) {
        return true;
    }

    // Extract bare method name from "Class.method" or "module:method" format
    let bare_name = if name.contains('.') {
        name.rsplit('.').next().unwrap_or(name)
    } else if name.contains(':') {
        name.rsplit(':').next().unwrap_or(name)
    } else {
        name
    };
    if bare_name != name && standard_patterns.contains(&bare_name) {
        return true;
    }

    // Test function patterns
    if name.starts_with("test_") || name.starts_with("pytest_") {
        return true;
    }

    // Test patterns on bare method name too
    if bare_name != name && (bare_name.starts_with("test_") || bare_name.starts_with("pytest_")) {
        return true;
    }

    // Go-style test functions (TestXxx, BenchmarkXxx, ExampleXxx)
    if name.starts_with("Test") || name.starts_with("Benchmark") || name.starts_with("Example") {
        return true;
    }

    // Java/Kotlin @Test annotation convention (methods starting with "test")
    if bare_name.starts_with("test") {
        return true;
    }

    // Prefix patterns for handlers/hooks across languages
    if bare_name.starts_with("handle") || bare_name.starts_with("Handle") {
        return true;
    }
    if bare_name.starts_with("on_")
        || bare_name.starts_with("before_")
        || bare_name.starts_with("after_")
    {
        return true;
    }

    // Check custom patterns
    if let Some(patterns) = custom_patterns {
        for pattern in patterns {
            if name == pattern {
                return true;
            }
            // Support simple glob patterns
            if pattern.ends_with('*') {
                let prefix = pattern.trim_end_matches('*');
                if name.starts_with(prefix) {
                    return true;
                }
            }
            if pattern.starts_with('*') {
                let suffix = pattern.trim_start_matches('*');
                if name.ends_with(suffix) {
                    return true;
                }
            }
        }
    }

    false
}

/// Build a human-readable signature string from function name, parameters, and return type.
///
/// Examples:
/// - `build_signature("calculate", &["x", "y"], Some("int"))` -> `"calculate(x, y) -> int"`
/// - `build_signature("helper", &[], None)` -> `"helper()"`
fn build_signature(name: &str, params: &[String], return_type: Option<&str>) -> String {
    let params_str = params.join(", ");
    match return_type {
        Some(rt) if !rt.is_empty() => format!("{}({}) -> {}", name, params_str, rt),
        _ => format!("{}({})", name, params_str),
    }
}

/// Extract all functions from a project for dead code analysis.
///
/// This is a helper function that can be used to gather all functions
/// from the AST extraction phase. It enriches FunctionRef with metadata
/// from the AST (decorators, visibility, test status, trait context)
/// to reduce false positives in dead code analysis.
pub fn collect_all_functions(
    module_infos: &[(PathBuf, crate::types::ModuleInfo)],
) -> Vec<FunctionRef> {
    let mut functions = Vec::new();

    for (file_path, info) in module_infos {
        let language = info.language;
        let is_test_file = is_test_file_path(file_path);
        let is_framework_entry =
            is_framework_entry_file(file_path, language) || has_framework_directive(file_path);

        // Add top-level functions
        for func in &info.functions {
            let is_public = infer_visibility_from_name(
                &func.name,
                language,
                !func.decorators.is_empty(),
                &func.decorators,
            );
            let has_decorator = !func.decorators.is_empty() || (is_framework_entry && is_public);
            let is_test = is_test_file
                || is_test_function_name(&func.name)
                || has_test_decorator(&func.decorators);
            let signature = build_signature(&func.name, &func.params, func.return_type.as_deref());

            functions.push(FunctionRef {
                file: file_path.clone(),
                name: func.name.clone(),
                line: func.line_number,
                signature,
                ref_count: 0,
                is_public,
                is_test,
                is_trait_method: false,
                has_decorator,
                decorator_names: func.decorators.clone(),
            });
        }

        // Add class methods
        for class in &info.classes {
            let is_trait = is_trait_or_interface(class, language);

            for method in &class.methods {
                let full_name = format!("{}.{}", class.name, method.name);
                let is_public = infer_visibility_from_name(
                    &method.name,
                    language,
                    !method.decorators.is_empty(),
                    &method.decorators,
                );
                let has_decorator =
                    !method.decorators.is_empty() || (is_framework_entry && is_public);
                let is_test = is_test_file
                    || is_test_function_name(&method.name)
                    || has_test_decorator(&method.decorators);
                let signature =
                    build_signature(&method.name, &method.params, method.return_type.as_deref());

                functions.push(FunctionRef {
                    file: file_path.clone(),
                    name: full_name,
                    line: method.line_number,
                    signature,
                    ref_count: 0,
                    is_public,
                    is_test,
                    is_trait_method: is_trait,
                    has_decorator,
                    decorator_names: method.decorators.clone(),
                });
            }
        }
    }

    functions
}

/// Check if a file path looks like a test file
fn is_test_file_path(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    let file_name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");

    // Common test file patterns across languages
    file_name.starts_with("test_")
        || file_name.ends_with("_test")
        || file_name.ends_with("_tests")
        || file_name.ends_with("_spec")
        || file_name.starts_with("Test")
        || file_name.ends_with("Test")
        || file_name.ends_with("Tests")
        || file_name.ends_with("Spec")
        || path_str.contains("/test/")
        || path_str.contains("/tests/")
        || path_str.contains("/spec/")
        || path_str.contains("/__tests__/")
}

/// Check if a function name looks like a test function
fn is_test_function_name(name: &str) -> bool {
    let bare = name.rsplit('.').next().unwrap_or(name);
    bare.starts_with("test_")
        || bare.starts_with("Test")
        || bare.starts_with("Benchmark")
        || bare.starts_with("Example")
}

/// Check if any decorator indicates a test
fn has_test_decorator(decorators: &[String]) -> bool {
    decorators.iter().any(|d| {
        let lower = d.to_lowercase();
        // Direct test markers (covers Python `@pytest.mark.parametrize`, generic
        // `test`/`testXxx`, plus Rust `#[test]`).
        if lower == "test" || lower == "pytest.mark.parametrize" || lower.starts_with("test") {
            return true;
        }
        // Rust ecosystem test attributes: `#[tokio::test]`, `#[async_std::test]`,
        // `#[wasm_bindgen_test]`, `#[rstest]`, `#[proptest]`, `#[serial_test::serial]`,
        // and any `cfg(test)` / `cfg_attr(test, ...)` (synthesized for functions that
        // live inside `#[cfg(test)] mod tests {}` / `mod tests {}`).
        lower.contains("::test")
            || lower.starts_with("tokio::test")
            || lower.starts_with("async_std::test")
            || lower.starts_with("wasm_bindgen_test")
            || lower.starts_with("rstest")
            || lower.starts_with("proptest")
            || lower.contains("cfg(test")
            || lower.contains("cfg_attr(test")
    })
}

/// Infer visibility from function name based on language conventions.
///
/// This is a heuristic approach - not perfect, but vastly better than
/// treating everything as private (which causes 95-100% FP rate).
fn infer_visibility_from_name(
    name: &str,
    language: crate::types::Language,
    _has_decorator: bool,
    _decorators: &[String],
) -> bool {
    use crate::types::Language;

    let bare_name = name.rsplit('.').next().unwrap_or(name);

    match language {
        // Python: no leading underscore = public (convention)
        Language::Python => !bare_name.starts_with('_'),

        // Go: uppercase first letter = exported
        Language::Go => bare_name
            .chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false),

        // Rust: we can't tell from name alone, but `pub` functions are
        // the majority in library crates. Without AST visibility info,
        // treat non-underscore-prefixed as possibly public.
        // The AST extraction code should set this more precisely.
        Language::Rust => !bare_name.starts_with('_'),

        // TypeScript/JavaScript: functions with decorators like @export
        // or those not starting with _ are typically public
        Language::TypeScript | Language::JavaScript => !bare_name.starts_with('_'),

        // Java/Kotlin/C#/Scala: typically all non-private methods are public.
        // Without explicit `private` keyword info, treat as public unless
        // name starts with underscore or is clearly internal.
        Language::Java | Language::Kotlin | Language::CSharp | Language::Scala => {
            !bare_name.starts_with('_')
        }

        // C/C++: static functions are private; others are public.
        // We can't tell from name, so treat as public by default.
        Language::C | Language::Cpp => true,

        // Ruby: methods after `private` keyword are private.
        // Convention: leading underscore = private.
        Language::Ruby => !bare_name.starts_with('_'),

        // PHP: has explicit public/private/protected keywords.
        // Convention: leading underscore = private.
        Language::Php => !bare_name.starts_with('_'),

        // Elixir: functions starting with _ are private (defp vs def)
        Language::Elixir => !bare_name.starts_with('_'),

        // Lua/Luau: local = private, module table = public
        // Convention: _M:method = public (module API), _prefix = private
        Language::Lua | Language::Luau => {
            // _M:method is always public — _M is the module export table
            if name.starts_with("_M:") || name.starts_with("_M.") {
                return true;
            }
            // Extract method name after : (Lua method call syntax)
            let lua_bare = if let Some(pos) = bare_name.find(':') {
                &bare_name[pos + 1..]
            } else {
                bare_name
            };
            !lua_bare.starts_with('_')
        }

        // OCaml: .mli files define public interface
        // Convention: leading underscore = private
        Language::Ocaml => !bare_name.starts_with('_'),

        // Swift: default is internal, not public
        Language::Swift => !bare_name.starts_with('_'),
    }
}

/// Check if a class looks like a trait/interface/protocol/abstract class
fn is_trait_or_interface(
    class: &crate::types::ClassInfo,
    language: crate::types::Language,
) -> bool {
    use crate::types::Language;

    let name = &class.name;

    // Check bases for common trait/interface patterns
    let has_abstract_base = class
        .bases
        .iter()
        .any(|b| b == "ABC" || b == "ABCMeta" || b == "Protocol" || b == "Interface");

    if has_abstract_base {
        return true;
    }

    // Check class decorators for abstract/interface/trait/protocol/module indicators.
    // AST extractors tag ClassInfo with these decorators:
    //   - PHP: "interface" for interfaces, "trait" for traits
    //   - Scala: "trait" for traits (via inheritance extractor)
    //   - Swift: "protocol" for protocols (via inheritance extractor)
    //   - Ruby: "module" for modules used as mixins
    //   - Rust: "trait" for trait items (when extracted by simple extractor)
    let has_type_decorator = class.decorators.iter().any(|d| {
        d == "abstract" || d == "interface" || d == "protocol" || d == "trait" || d == "module"
    });

    if has_type_decorator {
        return true;
    }

    match language {
        // Rust: traits are extracted as "classes" by some AST extractor paths.
        // The decorator check above handles cases where "trait" is set.
        // Without a decorator, we cannot reliably distinguish traits from structs
        // by name alone, so return false.
        Language::Rust => false,

        // Go: interfaces follow naming conventions.
        // Common Go interfaces end in "-er" (Reader, Writer, Handler, Stringer)
        // or have explicit "Interface" suffix.
        Language::Go => {
            // Explicit "Interface" suffix
            if name.ends_with("Interface") {
                return true;
            }
            // Common Go single-method interface pattern: capitalized name ending in "er"
            // e.g., Reader, Writer, Closer, Handler, Stringer, Formatter
            // Must be at least 3 chars and start uppercase to avoid false positives
            if name.len() >= 3
                && name.ends_with("er")
                && name
                    .chars()
                    .next()
                    .map(|c| c.is_uppercase())
                    .unwrap_or(false)
            {
                return true;
            }
            false
        }

        // Java/Kotlin: interfaces are common
        Language::Java | Language::Kotlin => {
            // Check for interface-like naming convention (IFoo pattern)
            name.starts_with('I')
                && name.len() > 1
                && name
                    .chars()
                    .nth(1)
                    .map(|c| c.is_uppercase())
                    .unwrap_or(false)
        }

        // C#: interface naming convention (IFoo)
        Language::CSharp => {
            name.starts_with('I')
                && name.len() > 1
                && name
                    .chars()
                    .nth(1)
                    .map(|c| c.is_uppercase())
                    .unwrap_or(false)
        }

        // Swift: protocols follow naming conventions.
        // Common suffixes: "Protocol", "Delegate", "DataSource", "able"/"ible"
        Language::Swift => {
            name.ends_with("Protocol")
                || name.ends_with("Delegate")
                || name.ends_with("DataSource")
                || name.ends_with("able")
                || name.ends_with("ible")
        }

        // Scala: traits use IFoo convention or end in common trait suffixes.
        // The decorator check above handles the "trait" tag from the extractor.
        Language::Scala => {
            // IFoo convention (same as Java)
            name.starts_with('I')
                && name.len() > 1
                && name
                    .chars()
                    .nth(1)
                    .map(|c| c.is_uppercase())
                    .unwrap_or(false)
        }

        // PHP: interfaces and traits are tagged by the extractor with decorators
        // ("interface" or "trait"), handled by the decorator check above.
        // Additional naming convention: IFoo pattern
        Language::Php => {
            name.starts_with('I')
                && name.len() > 1
                && name
                    .chars()
                    .nth(1)
                    .map(|c| c.is_uppercase())
                    .unwrap_or(false)
        }

        // Ruby: modules used as mixins/interfaces.
        // Common naming patterns: ends in "able", "ible", or includes "Mixin"
        Language::Ruby => {
            name.ends_with("able") || name.ends_with("ible") || name.contains("Mixin")
        }

        _ => false,
    }
}

/// Check if a file is a framework entry point (called by framework, not user code).
///
/// Functions in framework entry files are invoked by the framework runtime, not
/// by user code. Their absence from the call graph doesn't mean they are dead.
/// All exported/public functions in these files should be excluded from dead code analysis.
fn is_framework_entry_file(path: &Path, language: crate::types::Language) -> bool {
    use crate::types::Language;

    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let path_str = path.to_string_lossy();

    match language {
        Language::TypeScript | Language::JavaScript => {
            // Next.js App Router conventions
            matches!(
                file_name,
                "page.tsx"
                    | "page.ts"
                    | "page.jsx"
                    | "page.js"
                    | "layout.tsx"
                    | "layout.ts"
                    | "layout.jsx"
                    | "layout.js"
                    | "route.tsx"
                    | "route.ts"
                    | "route.jsx"
                    | "route.js"
                    | "loading.tsx"
                    | "loading.ts"
                    | "loading.jsx"
                    | "loading.js"
                    | "error.tsx"
                    | "error.ts"
                    | "error.jsx"
                    | "error.js"
                    | "not-found.tsx"
                    | "not-found.ts"
                    | "not-found.jsx"
                    | "not-found.js"
                    | "template.tsx"
                    | "template.ts"
                    | "template.jsx"
                    | "template.js"
                    | "default.tsx"
                    | "default.ts"
                    | "default.jsx"
                    | "default.js"
                    | "middleware.ts"
                    | "middleware.js"
                    | "manifest.ts"
                    | "manifest.js"
                    | "opengraph-image.tsx"
                    | "opengraph-image.ts"
                    | "sitemap.ts"
                    | "sitemap.js"
                    | "robots.ts"
                    | "robots.js"
            )
            // SvelteKit conventions
            || matches!(
                file_name,
                "+page.svelte"
                    | "+layout.svelte"
                    | "+error.svelte"
                    | "+page.ts"
                    | "+page.js"
                    | "+page.server.ts"
                    | "+page.server.js"
                    | "+layout.ts"
                    | "+layout.js"
                    | "+layout.server.ts"
                    | "+layout.server.js"
                    | "+server.ts"
                    | "+server.js"
            )
            // Nuxt conventions (files in pages/, layouts/, middleware/ dirs)
            || (path_str.contains("/pages/") && file_name.ends_with(".vue"))
            || (path_str.contains("/layouts/") && file_name.ends_with(".vue"))
            || (path_str.contains("/middleware/")
                && (file_name.ends_with(".ts") || file_name.ends_with(".js")))
            // Remix conventions
            || path_str.contains("/routes/")
            // Astro pages
            || (path_str.contains("/pages/") && file_name.ends_with(".astro"))
        }
        Language::Python => {
            // Django conventions
            file_name == "views.py"
                || file_name == "admin.py"
                || file_name == "urls.py"
                || file_name == "models.py"
                || file_name == "forms.py"
                || file_name == "serializers.py"
                || file_name == "signals.py"
                || file_name == "apps.py"
                || file_name == "middleware.py"
                || file_name == "context_processors.py"
                // Flask/FastAPI
                || file_name == "wsgi.py"
                || file_name == "asgi.py"
                || file_name == "conftest.py"
                // Celery
                || file_name == "tasks.py"
        }
        Language::Ruby => {
            // Rails conventions
            (path_str.contains("/controllers/") && file_name.ends_with("_controller.rb"))
                || (path_str.contains("/models/") && file_name.ends_with(".rb"))
                || (path_str.contains("/helpers/") && file_name.ends_with("_helper.rb"))
                || (path_str.contains("/mailers/") && file_name.ends_with("_mailer.rb"))
                || (path_str.contains("/jobs/") && file_name.ends_with("_job.rb"))
                || (path_str.contains("/channels/") && file_name.ends_with("_channel.rb"))
                || file_name == "application.rb"
                || file_name == "routes.rb"
                || file_name == "schema.rb"
        }
        Language::Java | Language::Kotlin => {
            // Spring Boot conventions
            file_name.ends_with("Controller.java")
                || file_name.ends_with("Controller.kt")
                || file_name.ends_with("Service.java")
                || file_name.ends_with("Service.kt")
                || file_name.ends_with("Repository.java")
                || file_name.ends_with("Repository.kt")
                || file_name.ends_with("Configuration.java")
                || file_name.ends_with("Configuration.kt")
                || file_name.ends_with("Application.java")
                || file_name.ends_with("Application.kt")
                // Android
                || file_name.ends_with("Activity.java")
                || file_name.ends_with("Activity.kt")
                || file_name.ends_with("Fragment.java")
                || file_name.ends_with("Fragment.kt")
                || file_name.ends_with("ViewModel.java")
                || file_name.ends_with("ViewModel.kt")
        }
        Language::CSharp => {
            // ASP.NET conventions. VAL-018: `Program.cs` and `Startup.cs`
            // are also the conventional entry point names for non-ASP.NET
            // C# applications (console apps, libraries with a CLI driver),
            // where they are NOT framework-rescued. Disambiguate by
            // reading the file: only treat `Program.cs`/`Startup.cs` as
            // ASP.NET when they reference ASP.NET-specific APIs.
            // Without this, dead-code analysis silently rescues every
            // public method in `Program.cs` even in plain console apps,
            // hiding real dead functions.
            if file_name.ends_with("Controller.cs")
                || file_name.ends_with("Hub.cs")
                || file_name.ends_with("Middleware.cs")
                || (path_str.contains("/Pages/") && file_name.ends_with(".cshtml.cs"))
            {
                return true;
            }
            if file_name == "Program.cs" || file_name == "Startup.cs" {
                if let Ok(content) = std::fs::read_to_string(path) {
                    return content.contains("Microsoft.AspNetCore")
                        || content.contains("WebApplication")
                        || content.contains("IApplicationBuilder")
                        || content.contains("IHostBuilder")
                        || content.contains("IWebHostBuilder")
                        || content.contains("IServiceCollection");
                }
            }
            false
        }
        Language::Go => {
            // Go HTTP handlers are typically in handler files
            file_name == "main.go"
                || file_name.ends_with("_handler.go")
                || file_name.ends_with("_handlers.go")
        }
        Language::Php => {
            // Laravel conventions
            (path_str.contains("/Controllers/") && file_name.ends_with(".php"))
                || (path_str.contains("/Middleware/") && file_name.ends_with(".php"))
                || (path_str.contains("/Models/") && file_name.ends_with(".php"))
                || (path_str.contains("/Providers/") && file_name.ends_with(".php"))
                || file_name == "routes.php"
                || file_name == "web.php"
                || file_name == "api.php"
        }
        Language::Elixir => {
            // Phoenix conventions
            (path_str.contains("/controllers/") && file_name.ends_with("_controller.ex"))
                || (path_str.contains("/live/") && file_name.ends_with("_live.ex"))
                || (path_str.contains("/channels/") && file_name.ends_with("_channel.ex"))
                || file_name == "router.ex"
                || file_name == "endpoint.ex"
        }
        Language::Swift => {
            // SwiftUI / iOS conventions
            file_name.ends_with("View.swift")
                || file_name.ends_with("ViewController.swift")
                || file_name.ends_with("App.swift")
                || file_name.ends_with("Delegate.swift")
        }
        Language::Scala => {
            // Play Framework conventions
            (path_str.contains("/controllers/") && file_name.ends_with(".scala"))
                || file_name == "routes"
        }
        _ => false,
    }
}

/// Check if a file contains a framework directive that makes exports externally reachable.
///
/// React Server Components use `'use server'` and `'use client'` directives at the
/// top of files. All exports from such files are framework entry points.
fn has_framework_directive(path: &Path) -> bool {
    // Only relevant for JS/TS files
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if !matches!(ext, "ts" | "tsx" | "js" | "jsx" | "mjs") {
        return false;
    }

    // Read first few lines looking for directives
    if let Ok(content) = std::fs::read_to_string(path) {
        for line in content.lines().take(5) {
            let trimmed = line.trim();
            if trimmed == r#""use server""#
                || trimmed == r#"'use server'"#
                || trimmed == r#""use server";"#
                || trimmed == r#"'use server';"#
                || trimmed == r#""use client""#
                || trimmed == r#"'use client'"#
                || trimmed == r#""use client";"#
                || trimmed == r#"'use client';"#
            {
                return true;
            }
            // Skip empty lines and comments
            if !trimmed.is_empty()
                && !trimmed.starts_with("//")
                && !trimmed.starts_with("/*")
                && !trimmed.starts_with('*')
            {
                // If we hit a non-directive, non-comment line, stop looking
                // (directives must be at the top of the file)
                if !trimmed.starts_with('"') && !trimmed.starts_with('\'') {
                    break;
                }
            }
        }
    }
    false
}

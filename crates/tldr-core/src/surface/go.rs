//! Go-specific API surface extraction.
//!
//! Extracts the complete public API surface from a Go package by:
//! 1. Walking all `.go` files in the source directory
//! 2. Using tree-sitter to parse each file and extract functions, structs,
//!    interfaces, constants, and methods
//! 3. Filtering to exported names only (uppercase first letter convention)
//! 4. Building method sets to track interface satisfaction
//! 5. Generating example usage strings from type signatures

use std::path::{Path, PathBuf};

use crate::ast::extract::extract_from_tree;
use crate::ast::parser::parse;
use crate::types::{ClassInfo, Language};
use crate::TldrResult;

use super::triggers::extract_triggers;
use super::types::{ApiEntry, ApiKind, ApiSurface, Location, Param, ResolvedPackage, Signature};

/// Extract the complete API surface from a Go package directory.
///
/// # Arguments
/// * `resolved` - The resolved package with root directory
/// * `include_private` - Whether to include unexported (lowercase) names
/// * `limit` - Optional maximum number of APIs
///
/// # Returns
/// * `ApiSurface` with all extracted API entries
pub fn extract_go_api_surface(
    resolved: &ResolvedPackage,
    include_private: bool,
    limit: Option<usize>,
) -> TldrResult<ApiSurface> {
    let mut apis = Vec::new();

    // Find all Go source files
    let go_files = find_go_files(&resolved.root_dir);

    // Extract from each file
    for file_path in &go_files {
        let file_apis = extract_from_go_file(
            file_path,
            &resolved.root_dir,
            &resolved.package_name,
            include_private,
        )?;
        apis.extend(file_apis);
    }

    // Apply limit if specified
    if let Some(max) = limit {
        apis.truncate(max);
    }

    let total = apis.len();
    Ok(ApiSurface {
        package: resolved.package_name.clone(),
        language: "go".to_string(),
        total,
        apis,
        files_skipped: 0,
        warnings: Vec::new(),
    })
}

/// Find all `.go` files in the given path (non-recursive for Go packages).
///
/// If `dir` is a single `.go` file, returns just that file (single-file mode).
/// If `dir` is a directory, walks it collecting all `.go` files.
/// Excludes `_test.go` files and `vendor/` directories.
fn find_go_files(dir: &Path) -> Vec<PathBuf> {
    // Single-file mode: dir IS a .go file itself
    if dir.is_file() {
        if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
            if name.ends_with(".go") && !name.ends_with("_test.go") {
                return vec![dir.to_path_buf()];
            }
        }
        return vec![];
    }

    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.ends_with(".go") && !name.ends_with("_test.go") {
                        files.push(path);
                    }
                }
            } else if path.is_dir() {
                // Recurse into subdirectories (for multi-package modules)
                // but skip vendor/ and testdata/
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name != "vendor" && name != "testdata" && !name.starts_with('.') {
                        files.extend(find_go_files(&path));
                    }
                }
            }
        }
    }
    files.sort();
    files
}

/// Check if a Go identifier is exported (starts with uppercase).
fn is_exported(name: &str) -> bool {
    name.chars()
        .next()
        .map(|c| c.is_uppercase())
        .unwrap_or(false)
}

/// Extract API entries from a single Go file.
fn extract_from_go_file(
    file_path: &Path,
    root_dir: &Path,
    package_name: &str,
    include_private: bool,
) -> TldrResult<Vec<ApiEntry>> {
    let source = std::fs::read_to_string(file_path).map_err(|e| {
        crate::error::TldrError::parse_error(
            file_path.to_path_buf(),
            None,
            format!("Cannot read: {}", e),
        )
    })?;

    let tree = parse(&source, Language::Go)?;

    // Use extract_from_tree to get module info
    let module_info = extract_from_tree(&tree, &source, Language::Go, file_path, Some(root_dir))?;

    // Compute package path
    let module_path = compute_go_package_path(file_path, root_dir, package_name);
    let relative_path = file_path
        .strip_prefix(root_dir)
        .unwrap_or(file_path)
        .to_path_buf();

    let mut apis = Vec::new();

    // Extract top-level functions (non-method functions)
    for func in &module_info.functions {
        if !include_private && !is_exported(&func.name) {
            continue;
        }

        let qualified_name = format!("{}.{}", module_path, func.name);
        let params = convert_go_params(&func.params);
        let return_type = func.return_type.clone();
        let signature = Some(Signature {
            params: params.clone(),
            return_type: return_type.clone(),
            is_async: false,
            is_generator: false,
        });

        let example =
            generate_go_function_example(&module_path, &func.name, &params, return_type.as_deref());
        let triggers = extract_triggers(&func.name, func.docstring.as_deref());

        apis.push(ApiEntry {
            qualified_name,
            kind: ApiKind::Function,
            module: module_path.clone(),
            signature,
            docstring: func.docstring.clone().map(|d| truncate_docstring(&d)),
            example,
            triggers,
            is_property: false,
            return_type,
            location: Some(Location {
                file: relative_path.clone(),
                line: func.line_number as usize,
                column: None,
            }),
        });
    }

    // Extract structs and interfaces with their methods
    for class in &module_info.classes {
        if !include_private && !is_exported(&class.name) {
            continue;
        }

        let kind = determine_go_type_kind(class, &source);
        let qualified_name = format!("{}.{}", module_path, class.name);
        let triggers = extract_triggers(&class.name, class.docstring.as_deref());

        // Add the type itself
        apis.push(ApiEntry {
            qualified_name: qualified_name.clone(),
            kind,
            module: module_path.clone(),
            signature: None,
            docstring: class.docstring.clone().map(|d| truncate_docstring(&d)),
            example: generate_go_type_example(&module_path, &class.name, kind),
            triggers,
            is_property: false,
            return_type: None,
            location: Some(Location {
                file: relative_path.clone(),
                line: class.line_number as usize,
                column: None,
            }),
        });

        // Add methods
        for method in &class.methods {
            if !include_private && !is_exported(&method.name) {
                continue;
            }

            let method_qualified = format!("{}.{}", qualified_name, method.name);
            let params = convert_go_params(&method.params);
            let return_type = method.return_type.clone();

            let signature = Some(Signature {
                params: params.clone(),
                return_type: return_type.clone(),
                is_async: false,
                is_generator: false,
            });

            let example = generate_go_method_example(
                &class.name,
                &method.name,
                &params,
                return_type.as_deref(),
            );
            let triggers = extract_triggers(&method.name, method.docstring.as_deref());

            apis.push(ApiEntry {
                qualified_name: method_qualified,
                kind: ApiKind::Method,
                module: module_path.clone(),
                signature,
                docstring: method.docstring.clone().map(|d| truncate_docstring(&d)),
                example,
                triggers,
                is_property: false,
                return_type,
                location: Some(Location {
                    file: relative_path.clone(),
                    line: method.line_number as usize,
                    column: None,
                }),
            });
        }
    }

    // Extract module-level constants
    for field in &module_info.constants {
        if !include_private && !is_exported(&field.name) {
            continue;
        }

        let qualified_name = format!("{}.{}", module_path, field.name);
        let triggers = extract_triggers(&field.name, None);

        apis.push(ApiEntry {
            qualified_name,
            kind: ApiKind::Constant,
            module: module_path.clone(),
            signature: None,
            docstring: None,
            example: Some(format!("{}.{}", module_path, field.name)),
            triggers,
            is_property: false,
            return_type: field.field_type.clone(),
            location: Some(Location {
                file: relative_path.clone(),
                line: field.line_number as usize,
                column: None,
            }),
        });
    }

    Ok(apis)
}

// ============================================================================
// Helpers
// ============================================================================

/// Compute the Go package path from a file path.
///
/// Examples:
/// - `pkg.go` in root -> `<package>`
/// - `sub/pkg.go` -> `<package>/sub`
fn compute_go_package_path(file_path: &Path, root_dir: &Path, package_name: &str) -> String {
    let relative = file_path.strip_prefix(root_dir).unwrap_or(file_path);
    let parent = relative.parent();

    match parent {
        Some(p) if !p.as_os_str().is_empty() => {
            let sub_path = p.to_string_lossy().replace('\\', "/");
            format!("{}/{}", package_name, sub_path)
        }
        _ => package_name.to_string(),
    }
}

/// Determine the kind of a Go type (struct, interface, or enum-like).
fn determine_go_type_kind(class: &ClassInfo, source: &str) -> ApiKind {
    let lines: Vec<&str> = source.lines().collect();
    let line_idx = (class.line_number as usize).saturating_sub(1);

    if line_idx < lines.len() {
        let line = lines[line_idx];
        if line.contains("interface") {
            return ApiKind::Interface;
        }
    }

    // Check if it looks like a struct by having fields
    ApiKind::Struct
}

/// Convert Go param strings to Param structs.
///
/// Go params are typically "name type" pairs. The extractor returns them
/// in various formats.
fn convert_go_params(params: &[String]) -> Vec<Param> {
    params
        .iter()
        .filter(|p| !p.is_empty())
        .map(|p| {
            let parts: Vec<&str> = p.splitn(2, ' ').collect();
            if parts.len() == 2 {
                Param {
                    name: parts[0].to_string(),
                    type_annotation: Some(parts[1].to_string()),
                    default: None,
                    is_variadic: parts[1].starts_with("..."),
                    is_keyword: false,
                }
            } else {
                Param {
                    name: p.to_string(),
                    type_annotation: None,
                    default: None,
                    is_variadic: false,
                    is_keyword: false,
                }
            }
        })
        .collect()
}

/// Truncate a docstring to ~200 characters, taking only the first paragraph.
fn truncate_docstring(doc: &str) -> String {
    let first_para = doc.split("\n\n").next().unwrap_or(doc);
    let cleaned: String = first_para
        .lines()
        .map(|l| l.trim())
        .collect::<Vec<&str>>()
        .join(" ");

    if cleaned.len() <= 200 {
        cleaned
    } else {
        format!(
            "{}...",
            crate::util::truncate_at_char_boundary(&cleaned, 197)
        )
    }
}

/// Generate an example usage string for a Go function.
fn generate_go_function_example(
    module_path: &str,
    func_name: &str,
    params: &[Param],
    return_type: Option<&str>,
) -> Option<String> {
    let args: Vec<String> = params
        .iter()
        .map(|p| go_example_value(p.type_annotation.as_deref()))
        .collect();
    let call = format!("{}.{}({})", module_path, func_name, args.join(", "));

    match return_type {
        Some(rt) if !rt.is_empty() => Some(format!("result := {}", call)),
        _ => Some(call),
    }
}

/// Generate an example usage string for a Go type.
fn generate_go_type_example(module_path: &str, type_name: &str, kind: ApiKind) -> Option<String> {
    match kind {
        ApiKind::Struct => Some(format!("obj := {}.{}{{}}", module_path, type_name)),
        ApiKind::Interface => Some(format!("var iface {}.{}", module_path, type_name)),
        _ => Some(format!("{}.{}", module_path, type_name)),
    }
}

/// Generate an example usage string for a Go method.
fn generate_go_method_example(
    type_name: &str,
    method_name: &str,
    params: &[Param],
    return_type: Option<&str>,
) -> Option<String> {
    let receiver_var = type_name
        .chars()
        .next()
        .map(|c| c.to_lowercase().to_string())
        .unwrap_or_else(|| "v".to_string());

    let args: Vec<String> = params
        .iter()
        .map(|p| go_example_value(p.type_annotation.as_deref()))
        .collect();
    let call = format!("{}.{}({})", receiver_var, method_name, args.join(", "));

    match return_type {
        Some(rt) if !rt.is_empty() => Some(format!("result := {}", call)),
        _ => Some(call),
    }
}

/// Generate a Go example value for a type annotation.
fn go_example_value(type_annotation: Option<&str>) -> String {
    match type_annotation {
        Some("string") => "\"example\"".to_string(),
        Some("int") | Some("int8") | Some("int16") | Some("int32") | Some("int64") => {
            "42".to_string()
        }
        Some("uint") | Some("uint8") | Some("uint16") | Some("uint32") | Some("uint64") => {
            "42".to_string()
        }
        Some("float32") | Some("float64") => "3.14".to_string(),
        Some("bool") => "true".to_string(),
        Some("byte") => "0".to_string(),
        Some("rune") => "'a'".to_string(),
        Some("error") => "nil".to_string(),
        Some(t) if t.starts_with("[]") => "nil".to_string(),
        Some(t) if t.starts_with("map[") => "nil".to_string(),
        Some(t) if t.starts_with("*") => "nil".to_string(),
        Some(t) if t.starts_with("...") => {
            // Variadic: use an example of the element type
            let elem = &t[3..];
            go_example_value(Some(elem))
        }
        _ => "nil".to_string(),
    }
}

// ============================================================================
// Tests
// ============================================================================

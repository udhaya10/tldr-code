//! Rust-specific API surface extraction.
//!
//! Extracts the complete public API surface from a Rust crate by:
//! 1. Reading `Cargo.toml` to find the crate root (`src/lib.rs`)
//! 2. Walking all `.rs` files in the source tree
//! 3. Using tree-sitter to parse each file and extract pub functions, structs,
//!    traits, enums, constants, and impl blocks
//! 4. Filtering to `pub` items only (distinguishing `pub(crate)` from `pub`)
//! 5. Extracting derive macros to generate synthetic API entries
//! 6. Generating example usage strings from type signatures

use std::path::{Path, PathBuf};

use crate::ast::extract::extract_from_tree;
use crate::ast::parser::parse;
use crate::types::{ClassInfo, Language};
use crate::TldrResult;

use super::language_profile::is_noise_dir;
use super::sort_apis_by_static_preference;
use super::triggers::extract_triggers;
use super::types::{ApiEntry, ApiKind, ApiSurface, Location, Param, ResolvedPackage, Signature};

/// Extract the complete API surface from a Rust crate.
///
/// # Arguments
/// * `resolved` - The resolved package with root directory
/// * `include_private` - Whether to include non-pub items
/// * `limit` - Optional maximum number of APIs
///
/// # Returns
/// * `ApiSurface` with all extracted API entries
pub fn extract_rust_api_surface(
    resolved: &ResolvedPackage,
    include_private: bool,
    limit: Option<usize>,
) -> TldrResult<ApiSurface> {
    let mut apis = Vec::new();

    // Find all Rust source files
    let rs_files = find_rust_files(&resolved.root_dir);

    // Extract from each file
    for file_path in &rs_files {
        let file_apis = extract_from_rust_file(
            file_path,
            &resolved.root_dir,
            &resolved.package_name,
            include_private,
        )?;
        apis.extend(file_apis);
    }

    add_crate_root_reexports(&mut apis, &resolved.root_dir, &resolved.package_name);
    sort_apis_by_static_preference(&mut apis, "rust");

    // Apply limit if specified
    if let Some(max) = limit {
        apis.truncate(max);
    }

    let total = apis.len();
    Ok(ApiSurface {
        package: resolved.package_name.clone(),
        language: "rust".to_string(),
        total,
        apis,
        files_skipped: 0,
        warnings: Vec::new(),
    })
}

/// Extract API entries from a single Rust file.
fn extract_from_rust_file(
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

    let tree = parse(&source, Language::Rust)?;

    // Use extract_from_tree to get module info
    let module_info = extract_from_tree(&tree, &source, Language::Rust, file_path, Some(root_dir))?;

    // Compute module path from file path
    let module_path = compute_rust_module_path(file_path, root_dir, package_name);
    let relative_path = file_path
        .strip_prefix(root_dir)
        .unwrap_or(file_path)
        .to_path_buf();

    let mut apis = Vec::new();

    // Extract top-level functions
    for func in &module_info.functions {
        if !include_private && !is_rust_item_public(&source, func.line_number as usize) {
            continue;
        }

        let qualified_name = format!("{}::{}", module_path, func.name);
        let params = convert_rust_params(&func.params);
        let return_type = func.return_type.clone();
        let signature = Some(Signature {
            params: params.clone(),
            return_type: return_type.clone(),
            is_async: func.is_async,
            is_generator: false,
        });

        let example = generate_rust_function_example(
            &module_path,
            &func.name,
            &params,
            return_type.as_deref(),
        );
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

    // Extract structs, traits, enums with their methods
    for class in &module_info.classes {
        let kind = determine_rust_class_kind(class, &source);

        if !include_private && !is_rust_item_public(&source, class.line_number as usize) {
            continue;
        }

        let qualified_name = format!("{}::{}", module_path, class.name);
        let triggers = extract_triggers(&class.name, class.docstring.as_deref());

        // Add the type itself
        apis.push(ApiEntry {
            qualified_name: qualified_name.clone(),
            kind,
            module: module_path.clone(),
            signature: None,
            docstring: class.docstring.clone().map(|d| truncate_docstring(&d)),
            example: generate_rust_type_example(&module_path, &class.name, kind),
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
        // For traits, all declared methods are implicitly public (no `pub` keyword).
        // Only structs/enums need per-method visibility checks.
        let is_trait = kind == ApiKind::Trait;
        for method in &class.methods {
            if !include_private
                && !is_trait
                && !is_rust_item_public(&source, method.line_number as usize)
            {
                continue;
            }

            let method_qualified = format!("{}::{}", qualified_name, method.name);
            let params = convert_rust_params(&method.params);
            let return_type = method.return_type.clone();
            let is_static = !method
                .params
                .iter()
                .any(|p| p == "self" || p.contains("self"));

            let method_kind = if is_static {
                ApiKind::StaticMethod
            } else {
                ApiKind::Method
            };

            let signature = Some(Signature {
                params: params.clone(),
                return_type: return_type.clone(),
                is_async: method.is_async,
                is_generator: false,
            });

            let example = generate_rust_method_example(
                &class.name,
                &method.name,
                is_static,
                &params,
                return_type.as_deref(),
            );
            let triggers = extract_triggers(&method.name, method.docstring.as_deref());

            apis.push(ApiEntry {
                qualified_name: method_qualified,
                kind: method_kind,
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

        // Extract derive macros and add synthetic entries
        let derives = extract_derives(&source, class.line_number as usize);
        for derive in &derives {
            if let Some(synthetic) =
                synthetic_from_derive(derive, &qualified_name, &module_path, &relative_path)
            {
                apis.push(synthetic);
            }
        }
    }

    // Extract module-level constants
    for field in &module_info.constants {
        if !include_private {
            if let Some(ref vis) = field.visibility {
                if !vis.starts_with("pub") {
                    continue;
                }
            } else {
                continue;
            }
        }

        let qualified_name = format!("{}::{}", module_path, field.name);
        let triggers = extract_triggers(&field.name, None);

        apis.push(ApiEntry {
            qualified_name,
            kind: ApiKind::Constant,
            module: module_path.clone(),
            signature: None,
            docstring: None,
            example: Some(format!("{}::{}", module_path, field.name)),
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

/// Compute the Rust module path from a file path.
///
/// Examples:
/// - `src/lib.rs` -> `<crate>`
/// - `src/surface/mod.rs` -> `<crate>::surface`
/// - `src/fix/rust_lang.rs` -> `<crate>::fix::rust_lang`
fn compute_rust_module_path(file_path: &Path, root_dir: &Path, crate_name: &str) -> String {
    let relative = file_path.strip_prefix(root_dir).unwrap_or(file_path);
    let relative_str = relative.to_string_lossy();

    // Strip "src/" prefix if present
    let module_part = relative_str.strip_prefix("src/").unwrap_or(&relative_str);

    // Strip .rs extension
    let module_part = module_part.strip_suffix(".rs").unwrap_or(module_part);

    // Handle special cases
    if module_part == "lib" || module_part == "main" {
        return crate_name.to_string();
    }

    // Handle mod.rs -> parent directory name
    let module_part = module_part.strip_suffix("/mod").unwrap_or(module_part);

    // Convert path separators to ::
    let module_path = module_part.replace('/', "::");

    format!("{}::{}", crate_name, module_path)
}

/// Convert raw Rust parameter strings to structured Params.
///
/// Raw params look like: `["self", "name: &str", "count: usize"]`
fn convert_rust_params(raw_params: &[String]) -> Vec<Param> {
    raw_params
        .iter()
        .map(|p| {
            let p = p.trim();
            if p == "self" || p == "&self" || p == "&mut self" || p == "mut self" {
                Param {
                    name: "self".to_string(),
                    type_annotation: Some(p.to_string()),
                    default: None,
                    is_variadic: false,
                    is_keyword: false,
                }
            } else if let Some((name, type_ann)) = p.split_once(':') {
                Param {
                    name: name.trim().to_string(),
                    type_annotation: Some(type_ann.trim().to_string()),
                    default: None,
                    is_variadic: false,
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

/// Determine the kind of a Rust "class" (struct, trait, or enum).
fn determine_rust_class_kind(class: &ClassInfo, source: &str) -> ApiKind {
    // Check the source line at the class definition
    let lines: Vec<&str> = source.lines().collect();
    if class.line_number > 0 && (class.line_number as usize) <= lines.len() {
        let line = lines[class.line_number as usize - 1].trim();
        if line.contains("trait ") {
            return ApiKind::Trait;
        }
        if line.contains("enum ") {
            return ApiKind::Enum;
        }
    }
    ApiKind::Struct
}

/// Check if a Rust item at the given line is public.
fn is_rust_item_public(source: &str, line_number: usize) -> bool {
    let lines: Vec<&str> = source.lines().collect();
    if line_number == 0 || line_number > lines.len() {
        return false;
    }
    let line = lines[line_number - 1].trim();
    line.starts_with("pub ") || line.starts_with("pub(")
}

/// Extract `#[derive(...)]` attributes from the lines before a struct/enum definition.
fn extract_derives(source: &str, struct_line: usize) -> Vec<String> {
    let lines: Vec<&str> = source.lines().collect();
    let mut derives = Vec::new();

    // Look at lines before the struct definition for #[derive(...)]
    for i in (0..struct_line.saturating_sub(1)).rev() {
        let line = lines[i].trim();
        if line.starts_with("#[derive(") || line.starts_with("#[derive (") {
            // Extract the derive list
            if let Some(start) = line.find('(') {
                if let Some(end) = line.rfind(')') {
                    let inner = &line[start + 1..end];
                    for item in inner.split(',') {
                        let item = item.trim();
                        if !item.is_empty() {
                            derives.push(item.to_string());
                        }
                    }
                }
            }
        } else if !line.starts_with("#[") && !line.starts_with("///") && !line.is_empty() {
            // Stop when we hit non-attribute/non-doc lines
            break;
        }
    }

    derives
}

/// Create synthetic API entries for derive macros.
///
/// For example, `#[derive(Clone)]` implies `MyStruct::clone()` exists.
fn synthetic_from_derive(
    derive: &str,
    parent_name: &str,
    module: &str,
    file: &Path,
) -> Option<ApiEntry> {
    let (method_name, return_desc) = match derive {
        "Clone" => ("clone", "Self"),
        "Debug" => return None, // Debug is for formatting, not a callable API
        "Default" => ("default", "Self"),
        "Hash" => return None, // Hash::hash() is rarely called directly
        "PartialEq" | "Eq" => return None, // Operators, not methods
        "PartialOrd" | "Ord" => return None,
        "Serialize" => return None, // serde::Serialize is generic, not a direct method
        "Deserialize" => return None,
        _ => return None,
    };

    Some(ApiEntry {
        qualified_name: format!("{}::{}", parent_name, method_name),
        kind: ApiKind::Method,
        module: module.to_string(),
        signature: Some(Signature {
            params: vec![Param {
                name: "self".to_string(),
                type_annotation: Some("&self".to_string()),
                default: None,
                is_variadic: false,
                is_keyword: false,
            }],
            return_type: Some(return_desc.to_string()),
            is_async: false,
            is_generator: false,
        }),
        docstring: Some(format!("Derived from `#[derive({})]`", derive)),
        example: None,
        triggers: vec![method_name.to_string(), "derive".to_string()],
        is_property: false,
        return_type: Some(return_desc.to_string()),
        location: Some(Location {
            file: file.to_path_buf(),
            line: 0,
            column: None,
        }),
    })
}

/// Truncate a docstring to approximately 200 characters, preserving the first paragraph.
fn truncate_docstring(doc: &str) -> String {
    let first_para = doc.split("\n\n").next().unwrap_or(doc);
    let cleaned: String = first_para
        .lines()
        .map(|l| l.trim().trim_start_matches("///").trim())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();

    if cleaned.len() > 200 {
        format!(
            "{}...",
            crate::util::truncate_at_char_boundary(&cleaned, 197)
        )
    } else {
        cleaned
    }
}

#[derive(Debug)]
struct RustReexport {
    target_path: String,
    public_name: String,
}

fn add_crate_root_reexports(apis: &mut Vec<ApiEntry>, root_dir: &Path, crate_name: &str) {
    let root_file = ["src/lib.rs", "src/main.rs"]
        .into_iter()
        .map(|path| root_dir.join(path))
        .find(|path| path.is_file());
    let Some(root_file) = root_file else {
        return;
    };

    let Ok(source) = std::fs::read_to_string(root_file) else {
        return;
    };

    let reexports = parse_crate_root_reexports(&source);
    if reexports.is_empty() {
        return;
    }

    let existing = apis.clone();
    let mut added_names = std::collections::HashSet::new();
    for api in &existing {
        added_names.insert(api.qualified_name.clone());
    }

    for reexport in reexports {
        let target_prefix = qualify_reexport_target(crate_name, &reexport.target_path);
        let alias_prefix = format!("{crate_name}::{}", reexport.public_name);

        for api in &existing {
            let Some(aliased_name) = rewrite_reexported_qualified_name(
                &api.qualified_name,
                &target_prefix,
                &alias_prefix,
            ) else {
                continue;
            };

            if !added_names.insert(aliased_name.clone()) {
                continue;
            }

            let mut aliased_api = api.clone();
            aliased_api.qualified_name = aliased_name;
            aliased_api.module = crate_name.to_string();
            apis.push(aliased_api);
        }
    }
}

fn parse_crate_root_reexports(source: &str) -> Vec<RustReexport> {
    source
        .lines()
        .filter_map(parse_simple_rust_reexport)
        .collect()
}

fn parse_simple_rust_reexport(line: &str) -> Option<RustReexport> {
    let trimmed = line.trim();
    if !trimmed.starts_with("pub use ") || !trimmed.ends_with(';') {
        return None;
    }

    let body = trimmed
        .strip_prefix("pub use ")?
        .trim_end_matches(';')
        .trim();

    if body.contains('{') || body.contains('}') || body.contains('*') || body.contains(',') {
        return None;
    }

    let (target_path, public_name) = if let Some((target, alias)) = body.rsplit_once(" as ") {
        (target.trim(), alias.trim())
    } else {
        let public_name = body.rsplit("::").next()?.trim();
        (body, public_name)
    };

    let target_path = target_path
        .strip_prefix("crate::")
        .or_else(|| target_path.strip_prefix("self::"))
        .unwrap_or(target_path)
        .trim();

    if target_path.is_empty() || public_name.is_empty() {
        return None;
    }

    Some(RustReexport {
        target_path: target_path.to_string(),
        public_name: public_name.to_string(),
    })
}

fn qualify_reexport_target(crate_name: &str, target_path: &str) -> String {
    if target_path.starts_with(crate_name) {
        target_path.to_string()
    } else {
        format!("{crate_name}::{target_path}")
    }
}

fn rewrite_reexported_qualified_name(
    original_name: &str,
    target_prefix: &str,
    alias_prefix: &str,
) -> Option<String> {
    if original_name == target_prefix {
        return Some(alias_prefix.to_string());
    }

    original_name
        .strip_prefix(target_prefix)
        .filter(|suffix| suffix.starts_with("::"))
        .map(|suffix| format!("{alias_prefix}{suffix}"))
}

/// Walk a directory recursively to find all Rust source files.
pub fn find_rust_files(root: &Path) -> Vec<PathBuf> {
    if root.is_file() {
        return root
            .extension()
            .and_then(|ext| ext.to_str())
            .filter(|ext| *ext == "rs")
            .map(|_| vec![root.to_path_buf()])
            .unwrap_or_default();
    }
    let mut files = Vec::new();
    find_rust_files_recursive(root, &mut files);
    files.sort();
    files
}

/// Recursive helper for finding Rust files.
fn find_rust_files_recursive(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !dir_name.starts_with('.') && !is_noise_dir(Language::Rust, dir_name) {
                find_rust_files_recursive(&path, files);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}

/// Generate an example usage string for a Rust function.
fn generate_rust_function_example(
    module: &str,
    name: &str,
    params: &[Param],
    return_type: Option<&str>,
) -> Option<String> {
    let args = rust_example_args(params, false);
    let ret_prefix = if return_type.is_some() {
        "let result = "
    } else {
        ""
    };
    Some(format!("{}{}::{}({})", ret_prefix, module, name, args))
}

/// Generate an example usage string for a Rust method.
fn generate_rust_method_example(
    type_name: &str,
    method_name: &str,
    is_static: bool,
    params: &[Param],
    return_type: Option<&str>,
) -> Option<String> {
    let args = rust_example_args(params, !is_static);
    let ret_prefix = if return_type.is_some() {
        "let result = "
    } else {
        ""
    };

    if is_static {
        Some(format!(
            "{}{}::{}({})",
            ret_prefix, type_name, method_name, args
        ))
    } else {
        let var = type_name.to_lowercase();
        Some(format!("{}{}.{}({})", ret_prefix, var, method_name, args))
    }
}

/// Generate an example for a Rust type (struct/enum/trait).
fn generate_rust_type_example(module: &str, name: &str, kind: ApiKind) -> Option<String> {
    match kind {
        ApiKind::Struct => Some(format!(
            "let {} = {}::{}::new(/* ... */);",
            name.to_lowercase(),
            module,
            name
        )),
        ApiKind::Enum => Some(format!("let val = {}::{}::default();", module, name)),
        ApiKind::Trait => None,
        _ => None,
    }
}

/// Format example arguments for Rust code.
fn rust_example_args(params: &[Param], skip_self: bool) -> String {
    params
        .iter()
        .filter(|p| if skip_self { p.name != "self" } else { true })
        .filter(|p| p.name != "self")
        .map(|p| rust_example_for_type(p.type_annotation.as_deref()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Generate an example value for a Rust type.
fn rust_example_for_type(type_ann: Option<&str>) -> String {
    match type_ann {
        Some("&str") | Some("&'_ str") | Some("&'static str") => "\"example\"".to_string(),
        Some("String") => "\"example\".to_string()".to_string(),
        Some("usize") | Some("u32") | Some("u64") | Some("i32") | Some("i64") => "42".to_string(),
        Some("u8") | Some("i8") => "0".to_string(),
        Some("u16") | Some("i16") => "0".to_string(),
        Some("f32") | Some("f64") => "1.0".to_string(),
        Some("bool") => "true".to_string(),
        Some("char") => "'a'".to_string(),
        Some(t) if t.starts_with("&[") => "&[]".to_string(),
        Some(t) if t.starts_with("Vec<") => "vec![]".to_string(),
        Some(t) if t.starts_with("Option<") => "None".to_string(),
        Some(t) if t.starts_with("&") => "&Default::default()".to_string(),
        Some("Self") => "Self::default()".to_string(),
        _ => "/* ... */".to_string(),
    }
}

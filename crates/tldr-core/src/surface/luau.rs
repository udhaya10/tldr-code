//! Luau-specific API surface extraction.
//!
//! Luau is a typed superset of Lua used by Roblox and others. It adds:
//!
//! - Optional type annotations on parameters and return types
//!   (`function f(x: number): string ... end`)
//! - `local function` (private) vs. `function` (public) at the module level
//! - `export type Name = ...` as a public type alias
//!
//! The public/private heuristic mirrors the language's conventions:
//!
//! - Top-level `function name(...)` (no `local`) is **public**.
//! - `local function name(...)` and `local name = function(...)` are **private**
//!   unless the surrounding module re-exports them via a returned table or
//!   assignment to a module table (the same pattern Lua uses).
//! - A function exported through `M.name = function(...)` or
//!   `function M.name(...)` is **public** when `M` is the returned module table.
//! - Names beginning with `_` are conventionally private.
//!
//! `export type Foo = ...` is surfaced as an [`ApiKind::TypeAlias`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ast::extract::extract_from_tree;
use crate::ast::parser::parse;
use crate::fs::{read_to_string_tolerant, ReadOutcome};
use crate::types::Language;
use crate::TldrResult;

use super::sort_apis_by_static_preference;
use super::triggers::extract_triggers;
use super::types::{ApiEntry, ApiKind, ApiSurface, Location, Param, ResolvedPackage, Signature};

/// Extract the public Luau API surface for a resolved package.
///
/// Walks the resolved package's root directory for `.luau` and `.lua` files
/// (Luau is a Lua superset, so `.lua` files in a Luau project are valid).
/// For each file we run the AST-based extractor (which already understands
/// typed parameters and typed return values for Luau) and then apply
/// the language's public/private heuristic plus any module-table re-exports.
pub fn extract_luau_api_surface(
    resolved: &ResolvedPackage,
    include_private: bool,
    limit: Option<usize>,
) -> TldrResult<ApiSurface> {
    let mut apis = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut files_skipped: usize = 0;

    for file_path in find_luau_files(&resolved.root_dir) {
        if let Some(entries) = extract_from_luau_file(
            &file_path,
            &resolved.root_dir,
            &resolved.package_name,
            include_private,
            &mut warnings,
            &mut files_skipped,
        )? {
            apis.extend(entries);
        }
    }

    sort_apis_by_static_preference(&mut apis, "luau");

    if let Some(max) = limit {
        apis.truncate(max);
    }

    let total = apis.len();
    Ok(ApiSurface {
        package: resolved.package_name.clone(),
        language: "luau".to_string(),
        total,
        apis,
        files_skipped,
        warnings,
    })
}

/// Find all `.luau` and `.lua` files under `dir` (recursively).
fn find_luau_files(dir: &Path) -> Vec<PathBuf> {
    if dir.is_file() {
        return dir
            .extension()
            .and_then(|ext| ext.to_str())
            .filter(|ext| matches!(*ext, "luau" | "lua"))
            .map(|_| vec![dir.to_path_buf()])
            .unwrap_or_default();
    }

    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if !name.starts_with('.') {
                        files.extend(find_luau_files(&path));
                    }
                }
            } else if matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("luau" | "lua")
            ) {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn extract_from_luau_file(
    file_path: &Path,
    root_dir: &Path,
    package_name: &str,
    include_private: bool,
    warnings: &mut Vec<String>,
    files_skipped: &mut usize,
) -> TldrResult<Option<Vec<ApiEntry>>> {
    let source = match read_to_string_tolerant(file_path).map_err(|e| {
        crate::error::TldrError::parse_error(
            file_path.to_path_buf(),
            None,
            format!("Cannot read: {}", e),
        )
    })? {
        ReadOutcome::Ok(s) => s,
        ReadOutcome::NonUtf8 { byte_offset } => {
            *files_skipped += 1;
            warnings.push(format!(
                "Skipped {}: invalid UTF-8 at byte {}",
                file_path.display(),
                byte_offset
            ));
            return Ok(None);
        }
    };

    // Pick the right grammar: .luau files use the Luau grammar; .lua files
    // in a Luau project are still parsed as Lua (the Luau grammar is a strict
    // superset, so for a `.lua` file the Lua grammar is the correct fit).
    let language = match file_path.extension().and_then(|ext| ext.to_str()) {
        Some("luau") => Language::Luau,
        _ => Language::Lua,
    };

    let tree = parse(&source, language)?;
    let module_info = extract_from_tree(&tree, &source, language, file_path, Some(root_dir))?;
    let module_path = compute_module_path(file_path, root_dir, package_name);
    let relative_path = file_path
        .strip_prefix(root_dir)
        .unwrap_or(file_path)
        .to_path_buf();

    let exported_table = returned_module_table(&source);
    let returned_keys = returned_table_keys(&source);
    let mut apis = Vec::new();

    for func in module_info.functions {
        let line_text = source
            .lines()
            .nth(func.line_number.saturating_sub(1) as usize)
            .unwrap_or("")
            .trim();

        // Module-table dotted/colon export: `function M.name(...)` or
        // `M.name = function(...)`.
        let module_export_name = if let Some(table_name) = exported_table.as_deref() {
            parse_table_export(line_text, table_name)
        } else {
            None
        }
        .or_else(|| returned_keys.get(&func.name).cloned());

        let is_local = is_local_function(line_text);
        let is_underscore = func.name.starts_with('_');
        let is_publicly_exported = module_export_name.is_some();

        // Decide visibility:
        //   - Module-table exports are always public.
        //   - Top-level `function name(...)` (no `local`) is public unless
        //     the name is conventionally private (`_name`).
        //   - Everything else is private.
        let is_public = is_publicly_exported || (!is_local && !is_underscore);

        if !is_public && !include_private {
            continue;
        }

        // Skip raw nested table-method definitions when no surrounding module
        // table is exported (e.g. `function obj:method(...)` on an internal
        // table) -- those are not meaningful module-level APIs.
        if !is_publicly_exported && line_text.contains('.') && !line_text.starts_with("function ") {
            continue;
        }

        let exposed_name = module_export_name
            .clone()
            .unwrap_or_else(|| func.name.clone());

        let params: Vec<Param> = func
            .params
            .iter()
            .map(|name| Param {
                name: name.clone(),
                type_annotation: None,
                default: None,
                is_variadic: name == "...",
                is_keyword: false,
            })
            .collect();

        apis.push(ApiEntry {
            qualified_name: format!("{}.{}", module_path, exposed_name),
            kind: ApiKind::Function,
            module: module_path.clone(),
            signature: Some(Signature {
                params: params.clone(),
                return_type: func.return_type.clone(),
                is_async: false,
                is_generator: false,
            }),
            docstring: func.docstring,
            example: Some(format!(
                "{}({})",
                exposed_name,
                params
                    .iter()
                    .map(|p| p.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            triggers: extract_triggers(&exposed_name, None),
            is_property: false,
            return_type: func.return_type,
            location: Some(Location {
                file: relative_path.clone(),
                line: func.line_number as usize,
                column: None,
            }),
        });
    }

    // `export type Name = ...` declarations are public type aliases.
    for export in find_exported_types(&source) {
        apis.push(ApiEntry {
            qualified_name: format!("{}.{}", module_path, export.name),
            kind: ApiKind::TypeAlias,
            module: module_path.clone(),
            signature: None,
            docstring: None,
            example: Some(format!("{}.{}", module_path, export.name)),
            triggers: extract_triggers(&export.name, None),
            is_property: false,
            return_type: None,
            location: Some(Location {
                file: relative_path.clone(),
                line: export.line,
                column: None,
            }),
        });
    }

    Ok(Some(apis))
}

fn compute_module_path(file_path: &Path, root_dir: &Path, package_name: &str) -> String {
    let relative = file_path.strip_prefix(root_dir).unwrap_or(file_path);
    let mut parts: Vec<String> = relative
        .iter()
        .map(|part| part.to_string_lossy().to_string())
        .collect();
    // Drop the file name and strip its extension to recover a module-style path.
    if let Some(last) = parts.pop() {
        let stem = Path::new(&last)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or(last);
        if !stem.is_empty() {
            parts.push(stem);
        }
    }
    if parts.is_empty() {
        package_name.to_string()
    } else {
        format!("{}.{}", package_name, parts.join("."))
    }
}

/// Detect if a line declares a `local function ...` (Luau private) or
/// `local name = function(...)`.
fn is_local_function(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("local function")
        || trimmed.starts_with("local ") && trimmed.contains("function")
}

fn returned_module_table(source: &str) -> Option<String> {
    source.lines().rev().find_map(|line| {
        let trimmed = line.trim();
        trimmed
            .strip_prefix("return ")
            .map(str::trim)
            .filter(|rest| {
                !rest.contains('{') && rest.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
            })
            .map(|name| name.to_string())
    })
}

fn returned_table_keys(source: &str) -> HashMap<String, String> {
    let mut exports = HashMap::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(body) = trimmed
            .strip_prefix("return {")
            .and_then(|s| s.strip_suffix('}'))
        {
            for item in body.split(',') {
                let part = item.trim();
                if let Some((key, value)) = part.split_once('=') {
                    let export_key = key.trim().to_string();
                    let local_name = value.trim().trim_start_matches("M.").to_string();
                    exports.insert(local_name, export_key);
                }
            }
        }
    }
    exports
}

fn parse_table_export(line: &str, table_name: &str) -> Option<String> {
    for separator in [".", ":"] {
        let needle = format!("{table_name}{separator}");
        if let Some(rest) = line.split(&needle).nth(1) {
            let name = rest
                .split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                .next()
                .unwrap_or("")
                .to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

struct ExportedType {
    name: String,
    line: usize,
}

/// Scan the source text for `export type Foo = ...` declarations.
///
/// Tree-sitter-luau represents these as `type_definition` nodes nested inside
/// an export-style construct, but the surface layer only needs the names and
/// line numbers. A line-based scan is robust against grammar variations and
/// stays in lockstep with how `lua.rs` resolves its module-table re-exports.
fn find_exported_types(source: &str) -> Vec<ExportedType> {
    let mut out = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        let rest = match trimmed.strip_prefix("export type ") {
            Some(rest) => rest,
            None => continue,
        };
        let name: String = rest
            .chars()
            .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
            .collect();
        if !name.is_empty() {
            out.push(ExportedType {
                name,
                line: index + 1,
            });
        }
    }
    out
}

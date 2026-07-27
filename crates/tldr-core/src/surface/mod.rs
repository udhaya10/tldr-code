//! API surface extraction for libraries and packages.
//!
//! This module extracts machine-readable API surfaces from installed packages,
//! producing structured data about every public function, method, class, constant,
//! and type alias. The output includes:
//!
//! - Qualified names and module paths
//! - Typed signatures with parameter defaults
//! - Docstrings (first paragraph, truncated)
//! - Example usage strings (templated from types)
//! - Trigger keywords for intent-based retrieval
//!
//! # Relationship to behavioral contracts
//!
//! The CLI `contracts` command (in `tldr-cli/src/commands/contracts/`) extracts
//! *behavioral* contracts (pre/postconditions from guard clauses and assertions).
//! This module (`surface`) extracts *structural* API shapes from a library.
//! They are complementary, not overlapping.
//!
//! # Supported languages
//!
//! - **Python** (Phase 1): Full support via tree-sitter + C extension fallback
//! - **Rust** (Phase 3): Full support via tree-sitter + Cargo.toml resolution
//! - **TypeScript** (Phase 4): Full support via tree-sitter + .d.ts parsing
//! - **Go** (Phase 5): Full support via tree-sitter + exported-name filtering
//! - **JavaScript** (Phase 6): Full support via tree-sitter + JSDoc parsing

pub mod c_lang;
pub mod cpp;
pub mod csharp;
pub mod elixir;
pub mod examples;
pub mod go;
pub mod java;
pub mod javascript;
pub mod kotlin;
pub mod language_profile;
pub mod lua;
pub mod luau;
pub mod ocaml;
pub mod php;
pub mod python;
pub mod resolve;
pub mod ruby;
pub mod rust_lang;
pub mod scala;
pub mod swift;
pub mod triggers;
pub mod types;
pub mod typescript;

// Re-export core types for public API
pub use types::{ApiEntry, ApiKind, ApiSurface, Location, Param, Signature};

use crate::TldrResult;
use language_profile::static_preference_score;

/// Extract the complete API surface for a package.
///
/// This is the main entry point. It resolves the package path, determines the
/// language, and dispatches to the language-specific extractor.
///
/// # Arguments
/// * `target` - Package name (e.g., "flask") or directory path
/// * `lang` - Optional language hint (auto-detected if not specified)
/// * `include_private` - Whether to include private APIs
/// * `limit` - Optional maximum number of APIs
/// * `lookup` - Optional: return only the API matching this qualified name
///
/// # Returns
/// * `Ok(ApiSurface)` - The extracted API surface
/// * `Err(TldrError)` - If resolution or extraction fails
pub fn extract_api_surface(
    target: &str,
    lang: Option<&str>,
    include_private: bool,
    limit: Option<usize>,
    lookup: Option<&str>,
) -> TldrResult<ApiSurface> {
    // Resolve the target to a package directory.
    //
    // M-Z11 (deps-and-surface-graceful-degrade-v1): when the resolver
    // cannot find a static entrypoint (e.g. a TypeScript/JavaScript
    // directory whose `package.json` only ships `scripts` and no
    // `main`/`module`/`exports`), we soft-fail with an empty surface
    // and a structured warning rather than aborting with exit code 10.
    // This matches the graceful-degrade pattern other commands use for
    // oversize files and lets `tldr surface` always emit valid JSON.
    let resolved = match resolve::resolve_target(target, lang) {
        Ok(r) => r,
        Err(e) if is_missing_entrypoint_error(&e) => {
            let detected = lang.or_else(|| detect_lang_from_path(target));
            let effective_lang = detected.unwrap_or("python");
            let package_name = std::path::Path::new(target)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(target)
                .to_string();
            return Ok(ApiSurface {
                package: package_name,
                language: effective_lang.to_string(),
                total: 0,
                apis: Vec::new(),
                files_skipped: 0,
                warnings: vec![format!("Skipped {}: {}", target, e)],
            });
        }
        Err(e) => return Err(e),
    };

    // Determine language and dispatch
    let detected = lang.or_else(|| detect_lang_from_path(target));
    let effective_lang = detected.unwrap_or("python");

    let mut surface = match effective_lang {
        "c" => c_lang::extract_c_api_surface(&resolved, include_private, limit)?,
        "cpp" => cpp::extract_cpp_api_surface(&resolved, include_private, limit)?,
        "elixir" => elixir::extract_elixir_api_surface(&resolved, include_private, limit)?,
        "csharp" => csharp::extract_csharp_api_surface(&resolved, include_private, limit)?,
        "python" => python::extract_python_api_surface(&resolved, include_private, limit)?,
        "rust" => rust_lang::extract_rust_api_surface(&resolved, include_private, limit)?,
        "typescript" => {
            typescript::extract_typescript_api_surface(&resolved, include_private, limit)?
        }
        "go" => go::extract_go_api_surface(&resolved, include_private, limit)?,
        "java" => java::extract_java_api_surface(&resolved, include_private, limit)?,
        "javascript" | "js" => {
            javascript::extract_javascript_api_surface(&resolved, include_private, limit)?
        }
        "kotlin" => kotlin::extract_kotlin_api_surface(&resolved, include_private, limit)?,
        "lua" => lua::extract_lua_api_surface(&resolved, include_private, limit)?,
        "luau" => luau::extract_luau_api_surface(&resolved, include_private, limit)?,
        "ocaml" => ocaml::extract_ocaml_api_surface(&resolved, include_private, limit)?,
        "php" => php::extract_php_api_surface(&resolved, include_private, limit)?,
        "scala" => scala::extract_scala_api_surface(&resolved, include_private, limit)?,
        "swift" => swift::extract_swift_api_surface(&resolved, include_private, limit)?,
        "ruby" => ruby::extract_ruby_api_surface(&resolved, include_private, limit)?,
        other => {
            return Err(crate::error::TldrError::UnsupportedLanguage(format!(
                "API surface extraction not yet supported for language: {}",
                other
            )))
        }
    };

    // Handle --lookup: filter to a single API entry
    if let Some(lookup_name) = lookup {
        surface.apis.retain(|api| {
            api.qualified_name == lookup_name
                || api.qualified_name.ends_with(&format!(".{}", lookup_name))
        });
        surface.total = surface.apis.len();
    }

    Ok(surface)
}

/// Recognise the "no static entrypoint found" parse error from
/// [`resolve::resolve_target`] so [`extract_api_surface`] can soft-fail with
/// an empty surface + warning rather than aborting the command.
///
/// The resolver currently emits this error from
/// `resolve_node_package_from_dir_inner` for TypeScript/JavaScript directories
/// whose `package.json` lacks a `main`/`module`/`exports`/`bin` entrypoint and
/// where no standard entrypoint file is present. We match on the message
/// suffix so future translations of the user-facing string remain compatible
/// (M-Z11: deps-and-surface-graceful-degrade-v1).
fn is_missing_entrypoint_error(err: &crate::error::TldrError) -> bool {
    if let crate::error::TldrError::ParseError { message, .. } = err {
        return message.contains("no supported static entrypoint was found");
    }
    false
}

pub(crate) fn sort_apis_by_static_preference(apis: &mut [ApiEntry], language: &str) {
    apis.sort_by(|left, right| {
        let left_score = api_static_preference_score(left, language);
        let right_score = api_static_preference_score(right, language);

        right_score
            .cmp(&left_score)
            .then_with(|| left.qualified_name.cmp(&right.qualified_name))
            .then_with(|| left.module.cmp(&right.module))
            .then_with(|| api_location_path(left).cmp(&api_location_path(right)))
            .then_with(|| api_location_line(left).cmp(&api_location_line(right)))
            .then_with(|| left.kind.to_string().cmp(&right.kind.to_string()))
    });
}

fn api_static_preference_score(api: &ApiEntry, language: &str) -> i32 {
    api.location
        .as_ref()
        .map(|location| static_preference_score(language, &location.file))
        .unwrap_or_default()
}

fn api_location_path(api: &ApiEntry) -> String {
    api.location
        .as_ref()
        .map(|location| location.file.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn api_location_line(api: &ApiEntry) -> usize {
    api.location
        .as_ref()
        .map(|location| location.line)
        .unwrap_or_default()
}

/// Detect language from a file path or directory.
///
/// For file paths, returns `Some(lang)` when the target has a recognisable
/// extension. For directories, delegates to [`crate::types::Language::from_directory`]
/// — the single canonical directory-level detector (see VAL-002) which applies
/// manifest priority (`tsconfig.json`, `Cargo.toml`, `go.mod`, ...) before
/// falling back to extension majority.
///
/// Returns `None` for bare package names (no `/` or `.`) so callers can fall
/// back to their own default. Also returns `None` for empty directories or
/// directories with no recognised source files.
fn detect_lang_from_path(target: &str) -> Option<&'static str> {
    let path = std::path::Path::new(target);

    // If the target is a directory on disk, delegate to the canonical
    // directory-level detector in `crate::types::Language::from_directory`.
    if path.is_dir() {
        return crate::types::Language::from_directory(path).map(|l| l.as_str());
    }

    // Only attempt extension-based detection when the target looks like a file
    // path: it must contain a '/' or a '.' (i.e., have an extension).
    if !target.contains('/') && !target.contains('.') {
        return None;
    }

    detect_lang_from_filename(target)
}

/// Map a single file name/path to a language based on its extension.
fn detect_lang_from_filename(target: &str) -> Option<&'static str> {
    // Strip a leading directory component so we can match the file name.
    let file_name = target.rsplit('/').next().unwrap_or(target);

    // Handle compound extensions first (e.g., ".d.ts").
    if file_name.ends_with(".d.ts") {
        return Some("typescript");
    }

    // Match on the final extension.
    let ext = file_name.rsplit('.').next().unwrap_or("");
    match ext {
        "py" => Some("python"),
        "rs" => Some("rust"),
        "ts" | "tsx" => Some("typescript"),
        "go" => Some("go"),
        "java" => Some("java"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Some("cpp"),
        "js" | "mjs" => Some("javascript"),
        "cs" => Some("csharp"),
        "kt" | "kts" => Some("kotlin"),
        "lua" => Some("lua"),
        "luau" => Some("luau"),
        "ml" | "mli" => Some("ocaml"),
        "php" => Some("php"),
        "scala" => Some("scala"),
        "swift" => Some("swift"),
        "rb" => Some("ruby"),
        "ex" | "exs" => Some("elixir"),
        _ => None,
    }
}

/// Format an API surface as human-readable text.
pub fn format_api_surface_text(surface: &ApiSurface) -> String {
    let mut output = String::new();

    output.push_str(&format!(
        "API Surface: {} ({}) - {} APIs\n",
        surface.package, surface.language, surface.total
    ));
    output.push_str(&"=".repeat(60));
    output.push('\n');

    for api in &surface.apis {
        output.push('\n');

        // Kind and name
        output.push_str(&format!("[{}] {}\n", api.kind, api.qualified_name));

        // Signature
        if let Some(sig) = &api.signature {
            let params_str: Vec<String> = sig
                .params
                .iter()
                .map(|p| {
                    let mut s = p.name.clone();
                    if p.is_variadic {
                        s = format!("*{}", s);
                    }
                    if p.is_keyword {
                        s = format!("**{}", s);
                    }
                    if let Some(t) = &p.type_annotation {
                        s = format!("{}: {}", s, t);
                    }
                    if let Some(d) = &p.default {
                        s = format!("{} = {}", s, d);
                    }
                    s
                })
                .collect();

            let ret = sig
                .return_type
                .as_ref()
                .map(|r| format!(" -> {}", r))
                .unwrap_or_default();

            let async_prefix = if sig.is_async { "async " } else { "" };

            output.push_str(&format!(
                "  {}({}){}\n",
                async_prefix,
                params_str.join(", "),
                ret
            ));
        }

        // Docstring
        if let Some(doc) = &api.docstring {
            output.push_str(&format!("  {}\n", doc));
        }

        // Example
        if let Some(ex) = &api.example {
            output.push_str(&format!("  Example: {}\n", ex));
        }

        // Triggers
        if !api.triggers.is_empty() {
            output.push_str(&format!("  Triggers: {}\n", api.triggers.join(", ")));
        }

        // Location
        if let Some(loc) = &api.location {
            output.push_str(&format!(
                "  Location: {}:{}\n",
                loc.file.display(),
                loc.line
            ));
        }
    }

    output
}

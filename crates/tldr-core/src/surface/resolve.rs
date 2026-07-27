//! Package path resolution for API surface extraction.
//!
//! Resolves a package name to its source directory on disk:
//! - Python: `python3 -c "import <pkg>; print(<pkg>.__file__)"` -> site-packages path
//! - (Other languages planned for later phases)
//!
//! Also handles `__all__` extraction from Python `__init__.py` files.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use crate::error::TldrError;
use crate::types::Language;
use crate::TldrResult;

use super::language_profile::{entrypoint_candidates, is_noise_dir};
use super::types::ResolvedPackage;

/// Resolve a Python package name to its source directory.
///
/// Uses `python3 -c "import <pkg>; print(<pkg>.__file__)"` to find the
/// installed package path, then determines the package root directory.
///
/// # Arguments
/// * `package_name` - Python package name (e.g., "json", "flask", "numpy")
///
/// # Returns
/// * `Ok(ResolvedPackage)` with the root directory and metadata
/// * `Err(TldrError::PackageNotFound)` if the package cannot be imported
pub fn resolve_python_package(package_name: &str) -> TldrResult<ResolvedPackage> {
    // Validate package name to prevent injection
    if !is_valid_python_identifier(package_name) {
        return Err(TldrError::parse_error(
            PathBuf::new(),
            None,
            format!("Invalid Python package name: {}", package_name),
        ));
    }

    let output = Command::new("python3")
        .arg("-c")
        .arg(format!(
            "import {pkg}; print({pkg}.__file__)",
            pkg = package_name
        ))
        .output()
        .map_err(|e| {
            TldrError::parse_error(
                PathBuf::new(),
                None,
                format!(
                    "Failed to run python3 to resolve package '{}': {}",
                    package_name, e
                ),
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Check if the error is because of missing __file__ (C built-in module).
        // Built-in modules like itertools, sys, etc. have no __file__ attribute.
        let stderr_str = stderr.trim();
        if stderr_str.contains("__file__") || stderr_str.contains("AttributeError") {
            // This is a C built-in module. Return a marker path so callers
            // know to use introspection instead of file-based extraction.
            return Ok(ResolvedPackage {
                root_dir: PathBuf::from(format!("<builtin:{}>", package_name)),
                package_name: package_name.to_string(),
                is_pure_source: false,
                public_names: None,
            });
        }

        // Check if the module can be imported at all (maybe __file__ failed but import works)
        let import_check = Command::new("python3")
            .arg("-c")
            .arg(format!("import {}", package_name))
            .output();

        if let Ok(check_output) = import_check {
            if check_output.status.success() {
                // Module exists but has no __file__ -- it's a built-in
                return Ok(ResolvedPackage {
                    root_dir: PathBuf::from(format!("<builtin:{}>", package_name)),
                    package_name: package_name.to_string(),
                    is_pure_source: false,
                    public_names: None,
                });
            }
        }

        return Err(TldrError::parse_error(
            PathBuf::new(),
            None,
            format!(
                "Cannot import Python package '{}': {}",
                package_name, stderr_str
            ),
        ));
    }

    let file_path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Handle the case where __file__ prints "None" (some C extensions)
    if file_path_str == "None" || file_path_str.is_empty() {
        return Ok(ResolvedPackage {
            root_dir: PathBuf::from(format!("<builtin:{}>", package_name)),
            package_name: package_name.to_string(),
            is_pure_source: false,
            public_names: None,
        });
    }

    let file_path = PathBuf::from(&file_path_str);

    // Determine the package root directory
    let root_dir = if file_path.ends_with("__init__.py") {
        // Package with __init__.py -> parent directory is the package root
        file_path
            .parent()
            .ok_or_else(|| {
                TldrError::parse_error(
                    file_path.clone(),
                    None,
                    format!("Cannot determine parent directory of '{}'", file_path_str),
                )
            })?
            .to_path_buf()
    } else {
        // Single-file module (e.g., csv.py) -> store the file path itself as root_dir.
        // find_python_files() detects this case and returns just the file, preventing
        // the resolver from walking the entire stdlib parent directory.
        file_path.clone()
    };

    // Check if package has pure Python source
    let is_pure_source = has_python_source(&root_dir, package_name, &file_path);

    // Extract __all__ from __init__.py if it exists
    let init_file = root_dir.join("__init__.py");
    let public_names = if init_file.exists() {
        extract_all_names(&init_file)
    } else {
        None
    };

    Ok(ResolvedPackage {
        root_dir,
        package_name: package_name.to_string(),
        is_pure_source,
        public_names,
    })
}

/// Resolve a target string to a package directory.
///
/// The target can be:
/// - A directory path (e.g., "./src/mylib/")
/// - A Python package name (e.g., "flask")
///
/// # Arguments
/// * `target` - Package name or directory path
/// * `lang` - Optional language hint
///
/// # Returns
/// * `Ok(ResolvedPackage)` with the resolved path
pub fn resolve_target(target: &str, lang: Option<&str>) -> TldrResult<ResolvedPackage> {
    let target_path = Path::new(target);

    // If it's a directory, use it directly unless the language supports
    // manifest-driven local package entrypoint resolution.
    if target_path.is_dir() {
        if matches!(lang, Some("typescript" | "ts" | "javascript" | "js"))
            && target_path.join("package.json").exists()
        {
            let language = match lang {
                Some("typescript") | Some("ts") => Language::TypeScript,
                Some("javascript") | Some("js") => Language::JavaScript,
                _ => unreachable!("guarded by matches!"),
            };
            return resolve_node_package_from_dir(
                target_path,
                target_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(target),
                language,
            );
        }

        let package_name = target_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(target)
            .to_string();

        return Ok(ResolvedPackage {
            root_dir: target_path.to_path_buf(),
            package_name,
            is_pure_source: true,
            public_names: None,
        });
    }

    // If it's a single file, extract from just that file.
    // This prevents over-resolution where e.g. "main.go" gets resolved
    // to the entire Go package directory via `go list`.
    if target_path.is_file() {
        let package_name = target_path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or(target)
            .to_string();

        return Ok(ResolvedPackage {
            root_dir: target_path.to_path_buf(),
            package_name,
            is_pure_source: true,
            public_names: None,
        });
    }

    // Otherwise, resolve as a package name based on language
    match lang {
        Some("python") | None => resolve_python_package(target),
        Some("rust") => resolve_rust_crate(target, None),
        Some("go") => resolve_go_package(target),
        Some("typescript") | Some("ts") => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            resolve_node_package(target, &cwd, Language::TypeScript)
        }
        Some("javascript") | Some("js") => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            resolve_node_package(target, &cwd, Language::JavaScript)
        }
        Some(other) => Err(TldrError::UnsupportedLanguage(format!(
            "Package resolution not yet supported for language: {}",
            other
        ))),
    }
}

/// Resolve a Rust crate to its source directory.
///
/// Reads `Cargo.toml` to find the crate root, typically `src/lib.rs` or `src/main.rs`.
/// If a manifest path is provided, reads from that location. Otherwise, looks for
/// `Cargo.toml` in the current directory or tries to find the crate in `~/.cargo/registry`.
///
/// # Arguments
/// * `crate_name` - Crate name (e.g., "serde") or path containing Cargo.toml
/// * `manifest_path` - Optional explicit path to Cargo.toml
///
/// # Returns
/// * `Ok(ResolvedPackage)` with the source directory
pub fn resolve_rust_crate(
    crate_name: &str,
    manifest_path: Option<&Path>,
) -> TldrResult<ResolvedPackage> {
    // If a manifest path is provided, use it directly
    if let Some(manifest) = manifest_path {
        let cargo_toml = if manifest.is_dir() {
            manifest.join("Cargo.toml")
        } else {
            manifest.to_path_buf()
        };

        if !cargo_toml.exists() {
            return Err(TldrError::parse_error(
                cargo_toml,
                None,
                "Cargo.toml not found at specified path".to_string(),
            ));
        }

        let root_dir = cargo_toml.parent().unwrap_or(Path::new(".")).to_path_buf();
        return Ok(ResolvedPackage {
            root_dir,
            package_name: crate_name.to_string(),
            is_pure_source: true,
            public_names: None,
        });
    }

    // Try current directory
    let cwd_cargo = PathBuf::from("Cargo.toml");
    if cwd_cargo.exists() {
        return Ok(ResolvedPackage {
            root_dir: PathBuf::from("."),
            package_name: crate_name.to_string(),
            is_pure_source: true,
            public_names: None,
        });
    }

    // Try to find in cargo registry
    if let Some(home) = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))
    {
        let registry = home.join("registry").join("src");
        if registry.exists() {
            // Walk registry sources looking for matching crate
            if let Ok(entries) = std::fs::read_dir(&registry) {
                for entry in entries.flatten() {
                    let index_dir = entry.path();
                    if index_dir.is_dir() {
                        if let Ok(crates) = std::fs::read_dir(&index_dir) {
                            for crate_entry in crates.flatten() {
                                let name = crate_entry.file_name();
                                let name_str = name.to_string_lossy();
                                if name_str.starts_with(crate_name)
                                    && (name_str.len() == crate_name.len()
                                        || name_str.as_bytes()[crate_name.len()] == b'-')
                                {
                                    let crate_dir = crate_entry.path();
                                    if crate_dir.join("Cargo.toml").exists() {
                                        return Ok(ResolvedPackage {
                                            root_dir: crate_dir,
                                            package_name: crate_name.to_string(),
                                            is_pure_source: true,
                                            public_names: None,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Err(TldrError::parse_error(
        PathBuf::new(),
        None,
        format!(
            "Cannot find Rust crate '{}'. Provide --manifest-path or run from a directory with Cargo.toml",
            crate_name
        ),
    ))
}

/// Resolve a Go package to its source directory.
///
/// Uses `go list -json <import-path>` to find the package directory.
/// Falls back to checking `GOPATH/src/<import-path>` if `go list` is unavailable.
///
/// # Arguments
/// * `import_path` - Go import path (e.g., "net/http", "encoding/json", or a local path)
///
/// # Returns
/// * `Ok(ResolvedPackage)` with the source directory
/// * `Err(TldrError)` if the package cannot be found
pub fn resolve_go_package(import_path: &str) -> TldrResult<ResolvedPackage> {
    // Try `go list -json` first
    let output = Command::new("go")
        .arg("list")
        .arg("-json")
        .arg(import_path)
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let json_str = String::from_utf8_lossy(&output.stdout);
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&json_str) {
                if let Some(dir) = value.get("Dir").and_then(|d| d.as_str()) {
                    let root_dir = PathBuf::from(dir);
                    let package_name = value
                        .get("Name")
                        .and_then(|n| n.as_str())
                        .unwrap_or(import_path)
                        .to_string();

                    return Ok(ResolvedPackage {
                        root_dir,
                        package_name,
                        is_pure_source: true,
                        public_names: None,
                    });
                }
            }
        }
    }

    // Fallback: check GOPATH/src
    if let Ok(gopath) = std::env::var("GOPATH") {
        let src_dir = PathBuf::from(&gopath).join("src").join(import_path);
        if src_dir.is_dir() {
            let package_name = import_path
                .rsplit('/')
                .next()
                .unwrap_or(import_path)
                .to_string();
            return Ok(ResolvedPackage {
                root_dir: src_dir,
                package_name,
                is_pure_source: true,
                public_names: None,
            });
        }
    }

    Err(TldrError::parse_error(
        PathBuf::new(),
        None,
        format!(
            "Cannot find Go package '{}'. Make sure `go` is on PATH and the package is available.",
            import_path
        ),
    ))
}

/// Resolve a TypeScript/JavaScript package to its source or declaration directory.
///
/// Searches for `node_modules/<package>/package.json`, reads the `"types"` or
/// `"typings"` field to locate `.d.ts` declaration files, and falls back to
/// `index.d.ts` or `index.ts` in the package directory.
///
/// The search starts at `search_root` and walks up parent directories until
/// `node_modules/<package>/package.json` is found.
///
/// # Arguments
/// * `package_name` - npm package name (e.g., "express", "lodash")
/// * `search_root` - Directory to start searching for node_modules (usually cwd)
///
/// # Returns
/// * `Ok(ResolvedPackage)` with the directory containing the types entry point
/// * `Err(TldrError)` if the package cannot be found
pub fn resolve_typescript_package(
    package_name: &str,
    search_root: &Path,
) -> TldrResult<ResolvedPackage> {
    resolve_node_package(package_name, search_root, Language::TypeScript)
}

/// Resolve a Node package to the directory containing its static entrypoint.
fn resolve_node_package(
    package_name: &str,
    search_root: &Path,
    language: Language,
) -> TldrResult<ResolvedPackage> {
    // Walk up from search_root looking for node_modules/<package>/package.json
    let mut current = search_root.to_path_buf();
    loop {
        let pkg_json_path = current
            .join("node_modules")
            .join(package_name)
            .join("package.json");

        if pkg_json_path.exists() {
            let pkg_dir = current.join("node_modules").join(package_name);
            return resolve_node_package_from_dir(&pkg_dir, package_name, language);
        }

        // Walk up to parent directory
        match current.parent() {
            Some(parent) if parent != current => {
                current = parent.to_path_buf();
            }
            _ => break,
        }
    }

    Err(TldrError::parse_error(
        PathBuf::new(),
        None,
        format!(
            "Cannot find {} package '{}'. No node_modules/{}/package.json found in '{}' or any parent directory.",
            language.as_str(),
            package_name,
            package_name,
            search_root.display()
        ),
    ))
}

/// Given a package directory containing package.json, determine the package
/// entry point and return a `ResolvedPackage`.
fn resolve_node_package_from_dir(
    pkg_dir: &Path,
    package_name: &str,
    language: Language,
) -> TldrResult<ResolvedPackage> {
    resolve_node_package_from_dir_inner(pkg_dir, package_name, language, true)
}

fn resolve_node_package_from_dir_inner(
    pkg_dir: &Path,
    package_name: &str,
    language: Language,
    allow_workspace_scan: bool,
) -> TldrResult<ResolvedPackage> {
    let pkg_json_path = pkg_dir.join("package.json");

    // Read and parse package.json
    let pkg_json_content = std::fs::read_to_string(&pkg_json_path).map_err(|e| {
        TldrError::parse_error(
            pkg_json_path.clone(),
            None,
            format!("Cannot read package.json: {}", e),
        )
    })?;

    let pkg_json: serde_json::Value = serde_json::from_str(&pkg_json_content).map_err(|e| {
        TldrError::parse_error(
            pkg_json_path.clone(),
            None,
            format!("Invalid JSON in package.json: {}", e),
        )
    })?;
    let workspace_patterns = workspace_patterns(&pkg_json);
    let is_workspace_aggregator = pkg_json
        .get("private")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        && !workspace_patterns.is_empty();

    let manifest_candidates = package_manifest_entry_candidates(&pkg_json, language);
    let profile_candidates = if is_workspace_aggregator {
        Vec::new()
    } else {
        entrypoint_candidates(language)
            .iter()
            .copied()
            .filter(|entry| *entry != "package.json")
            .map(str::to_string)
            .collect::<Vec<_>>()
    };

    for candidate in manifest_candidates.into_iter().chain(profile_candidates) {
        if let Some(root_dir) = resolve_existing_entrypoint_dir(pkg_dir, &candidate) {
            return Ok(ResolvedPackage {
                root_dir,
                package_name: package_name.to_string(),
                is_pure_source: true,
                public_names: None,
            });
        }
    }

    // Last resort: use the package directory itself (may contain .ts files in subfolders)
    // Check if there are any source files for the requested language in the package root.
    let has_ts_files = std::fs::read_dir(pkg_dir)
        .ok()
        .map(|entries| {
            entries.flatten().any(|e| {
                let path = e.path();
                let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
                match language {
                    Language::TypeScript => ext == "ts" || ext == "tsx",
                    Language::JavaScript => ext == "js" || ext == "mjs" || ext == "cjs",
                    _ => false,
                }
            })
        })
        .unwrap_or(false);

    if has_ts_files && !is_workspace_aggregator {
        return Ok(ResolvedPackage {
            root_dir: pkg_dir.to_path_buf(),
            package_name: package_name.to_string(),
            is_pure_source: true,
            public_names: None,
        });
    }

    if allow_workspace_scan {
        if let Some(resolved) = resolve_workspace_package_from_dir(
            pkg_dir,
            package_name,
            language,
            &workspace_patterns,
            &pkg_json,
        )? {
            return Ok(resolved);
        }
    }

    Err(TldrError::parse_error(
        pkg_dir.to_path_buf(),
        None,
        format!(
            "{} package '{}' found at {} but no supported static entrypoint was found. \
             Ensure package.json points to a static entrypoint, or the package contains a standard entrypoint file.",
            language.as_str(),
            package_name,
            pkg_dir.display()
        ),
    ))
}

fn resolve_workspace_package_from_dir(
    pkg_dir: &Path,
    package_name: &str,
    language: Language,
    workspace_patterns: &[String],
    pkg_json: &serde_json::Value,
) -> TldrResult<Option<ResolvedPackage>> {
    if !pkg_json
        .get("private")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(None);
    }

    if workspace_patterns.is_empty() {
        return Ok(None);
    }

    let mut candidates: Vec<(i32, PathBuf, ResolvedPackage)> =
        workspace_package_dirs(pkg_dir, workspace_patterns)
            .into_iter()
            .filter_map(|workspace_dir| {
                let resolved = resolve_node_package_from_dir_inner(
                    &workspace_dir,
                    package_name,
                    language,
                    false,
                )
                .ok()?;
                let relative_workspace_dir = workspace_dir.strip_prefix(pkg_dir).ok()?;
                let relative_root = resolved.root_dir.strip_prefix(&workspace_dir).ok()?;
                let score = workspace_candidate_score(
                    package_name,
                    language,
                    relative_workspace_dir,
                    &workspace_dir,
                    relative_root,
                );

                Some((score, workspace_dir, resolved))
            })
            .collect();

    candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));

    let Some((best_score, _, best_resolved)) = candidates.first() else {
        return Ok(None);
    };

    if *best_score <= 0 {
        return Ok(None);
    }

    if let Some((second_score, second_path, _)) = candidates.get(1) {
        if *second_score == *best_score {
            let best_path = &candidates[0].1;
            return Err(TldrError::parse_error(
                pkg_dir.to_path_buf(),
                None,
                format!(
                    "Ambiguous {} workspace package selection for '{}': both '{}' and '{}' matched equally. Resolve a more specific subdirectory instead.",
                    language.as_str(),
                    package_name,
                    best_path.display(),
                    second_path.display(),
                ),
            ));
        }
    }

    Ok(Some(best_resolved.clone()))
}

fn resolve_existing_entrypoint_dir(pkg_dir: &Path, candidate: &str) -> Option<PathBuf> {
    let resolved_path = normalize_path(&pkg_dir.join(candidate));
    if !resolved_path.exists() {
        return None;
    }

    Some(if resolved_path.is_file() {
        normalize_path(resolved_path.parent().unwrap_or(pkg_dir))
    } else {
        resolved_path
    })
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }

    normalized
}

fn package_manifest_entry_candidates(
    pkg_json: &serde_json::Value,
    language: Language,
) -> Vec<String> {
    let mut candidates = Vec::new();

    if matches!(language, Language::TypeScript | Language::JavaScript) {
        push_manifest_string(pkg_json.get("types"), &mut candidates);
        push_manifest_string(pkg_json.get("typings"), &mut candidates);
    }

    if let Some(exports) = pkg_json.get("exports") {
        collect_exports_entrypoints(exports, language, &mut candidates);
    }

    if language == Language::JavaScript {
        push_manifest_string(pkg_json.get("main"), &mut candidates);
        push_manifest_string(pkg_json.get("module"), &mut candidates);
        push_manifest_string(pkg_json.get("browser"), &mut candidates);
        push_manifest_string(pkg_json.get("source"), &mut candidates);
        push_manifest_string(pkg_json.get("jsnext:main"), &mut candidates);
    }

    dedupe_candidates(candidates)
}

fn push_manifest_string(value: Option<&serde_json::Value>, candidates: &mut Vec<String>) {
    if let Some(path) = value.and_then(|value| value.as_str()) {
        candidates.push(path.to_string());
    }
}

fn collect_exports_entrypoints(
    value: &serde_json::Value,
    language: Language,
    candidates: &mut Vec<String>,
) {
    match value {
        serde_json::Value::String(path) => candidates.push(path.clone()),
        serde_json::Value::Array(values) => {
            for entry in values {
                collect_exports_entrypoints(entry, language, candidates);
            }
        }
        serde_json::Value::Object(map) => {
            let preferred_keys: &[&str] = match language {
                Language::TypeScript => &["types", "default", "import", "require", "."],
                Language::JavaScript => &["default", "import", "require", "browser", "."],
                _ => &["."],
            };

            for key in preferred_keys {
                if let Some(entry) = map.get(*key) {
                    collect_exports_entrypoints(entry, language, candidates);
                }
            }

            for (key, entry) in map {
                if !preferred_keys.contains(&key.as_str()) {
                    collect_exports_entrypoints(entry, language, candidates);
                }
            }
        }
        _ => {}
    }
}

fn dedupe_candidates(candidates: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut deduped = Vec::new();
    for candidate in candidates {
        if is_package_json_candidate(&candidate) {
            continue;
        }

        if seen.insert(candidate.clone()) {
            deduped.push(candidate);
        }
    }
    deduped
}

fn is_package_json_candidate(candidate: &str) -> bool {
    Path::new(candidate)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("package.json"))
}

pub(crate) fn public_entry_files_for_resolved_package(
    root_dir: &Path,
    language: Language,
) -> Vec<PathBuf> {
    let Some(package_root) = nearest_package_root(root_dir) else {
        return fallback_root_entry_files(root_dir, language);
    };

    let pkg_json_path = package_root.join("package.json");
    let Ok(content) = std::fs::read_to_string(pkg_json_path) else {
        return fallback_root_entry_files(root_dir, language);
    };
    let Ok(pkg_json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return fallback_root_entry_files(root_dir, language);
    };

    let mut entry_files = Vec::new();
    for candidate in package_manifest_entry_candidates(&pkg_json, language) {
        let resolved_path = normalize_path(&package_root.join(candidate));
        if resolved_path.is_file() {
            if let Ok(relative) = resolved_path.strip_prefix(root_dir) {
                entry_files.push(relative.to_path_buf());
            }
        }
    }

    if entry_files.is_empty() {
        entry_files = fallback_root_entry_files(root_dir, language);
    }

    entry_files.sort();
    entry_files.dedup();
    entry_files
}

fn nearest_package_root(root_dir: &Path) -> Option<PathBuf> {
    root_dir
        .ancestors()
        .find(|path| path.join("package.json").is_file())
        .map(Path::to_path_buf)
}

fn fallback_root_entry_files(root_dir: &Path, language: Language) -> Vec<PathBuf> {
    entrypoint_candidates(language)
        .iter()
        .copied()
        .filter(|entry| *entry != "package.json")
        .map(|entry| normalize_path(&root_dir.join(entry)))
        .filter(|path| path.is_file())
        .filter_map(|path| path.strip_prefix(root_dir).ok().map(Path::to_path_buf))
        .collect()
}

fn workspace_patterns(pkg_json: &serde_json::Value) -> Vec<String> {
    match pkg_json.get("workspaces") {
        Some(serde_json::Value::Array(entries)) => entries
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_string)
            .collect(),
        Some(serde_json::Value::Object(map)) => map
            .get("packages")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn workspace_package_dirs(pkg_dir: &Path, patterns: &[String]) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut dirs = Vec::new();

    for pattern in patterns {
        for workspace_dir in expand_workspace_pattern(pkg_dir, pattern) {
            let workspace_dir = normalize_path(&workspace_dir);
            if workspace_dir.join("package.json").is_file() && seen.insert(workspace_dir.clone()) {
                dirs.push(workspace_dir);
            }
        }
    }

    dirs
}

fn expand_workspace_pattern(pkg_dir: &Path, pattern: &str) -> Vec<PathBuf> {
    let Some(prefix) = pattern.strip_suffix("/*") else {
        return vec![pkg_dir.join(pattern)];
    };

    std::fs::read_dir(pkg_dir.join(prefix))
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect()
}

fn workspace_candidate_score(
    package_name: &str,
    language: Language,
    relative_workspace_dir: &Path,
    workspace_dir: &Path,
    relative_root: &Path,
) -> i32 {
    let mut score = static_workspace_path_score(language, relative_workspace_dir);

    if relative_workspace_dir
        .components()
        .next()
        .is_some_and(|component| component.as_os_str() == "packages")
    {
        score += 80;
    }

    let package_json_path = workspace_dir.join("package.json");
    if let Ok(content) = std::fs::read_to_string(package_json_path) {
        if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) {
            let manifest_name = manifest.get("name").and_then(serde_json::Value::as_str);
            score += package_identity_score(
                package_name,
                manifest_name,
                workspace_dir.file_name().and_then(|name| name.to_str()),
            );
        }
    }

    score + super::language_profile::static_preference_score(language, relative_root)
}

fn static_workspace_path_score(language: Language, relative_workspace_dir: &Path) -> i32 {
    let segments: Vec<String> = relative_workspace_dir
        .components()
        .filter_map(|component| match component {
            Component::Normal(segment) => Some(segment.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();

    if segments.is_empty() {
        return 0;
    }

    if segments
        .iter()
        .any(|segment| is_noise_dir(language, segment.as_str()))
    {
        return -120;
    }

    0
}

fn package_identity_score(
    requested: &str,
    manifest_name: Option<&str>,
    directory_name: Option<&str>,
) -> i32 {
    let requested_norm = normalize_package_token(requested);
    let requested_tokens = package_tokens(requested);
    let mut score = 0;

    for candidate in [
        manifest_name,
        manifest_name.and_then(scope_basename),
        directory_name,
    ] {
        let Some(candidate) = candidate else {
            continue;
        };

        let candidate_norm = normalize_package_token(candidate);
        if !requested_norm.is_empty() && requested_norm == candidate_norm {
            score += 200;
            continue;
        }

        if !requested_norm.is_empty()
            && (!candidate_norm.is_empty())
            && (requested_norm.contains(&candidate_norm)
                || candidate_norm.contains(&requested_norm))
        {
            score += 120;
        }

        let overlap = token_overlap(&requested_tokens, &package_tokens(candidate));
        if overlap > 0 {
            score += overlap as i32 * 40;
        }
    }

    score
}

fn scope_basename(name: &str) -> Option<&str> {
    name.rsplit('/').next()
}

fn normalize_package_token(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn package_tokens(value: &str) -> std::collections::HashSet<String> {
    value
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

fn token_overlap(
    left: &std::collections::HashSet<String>,
    right: &std::collections::HashSet<String>,
) -> usize {
    left.intersection(right).count()
}

/// Check if a Python identifier is valid (prevents injection in import command).
fn is_valid_python_identifier(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }

    // Allow dotted names like "xml.etree.ElementTree"
    for part in name.split('.') {
        if part.is_empty() {
            return false;
        }
        let mut chars = part.chars();
        // First char must be alpha or underscore
        if let Some(first) = chars.next() {
            if !first.is_alphabetic() && first != '_' {
                return false;
            }
        }
        // Rest must be alphanumeric or underscore
        for c in chars {
            if !c.is_alphanumeric() && c != '_' {
                return false;
            }
        }
    }

    true
}

/// Check if a package directory contains pure Python source files.
fn has_python_source(root_dir: &Path, _package_name: &str, init_file: &Path) -> bool {
    // If __init__.py exists and there are .py files, it's pure source
    if init_file.ends_with("__init__.py") && root_dir.join("__init__.py").exists() {
        // Check for at least one .py file beyond __init__.py
        if let Ok(entries) = std::fs::read_dir(root_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("py")
                    && path.file_name().and_then(|n| n.to_str()) != Some("__init__.py")
                {
                    return true;
                }
            }
        }
        // Only __init__.py found, still counts as pure source
        return true;
    }

    // If it's a single .py file
    if init_file.extension().and_then(|e| e.to_str()) == Some("py") {
        return true;
    }

    false
}

/// Extract `__all__` names from a Python `__init__.py` file.
///
/// Parses the file with tree-sitter to find `__all__ = [...]` assignments
/// and returns the list of exported names.
fn extract_all_names(init_file: &Path) -> Option<Vec<String>> {
    let source = std::fs::read_to_string(init_file).ok()?;
    extract_all_names_from_source(&source)
}

/// Extract `__all__` names from Python source code.
///
/// Looks for patterns like:
/// ```python
/// __all__ = ["foo", "bar", "baz"]
/// __all__ = ('foo', 'bar', 'baz')
/// ```
pub fn extract_all_names_from_source(source: &str) -> Option<Vec<String>> {
    use crate::ast::parser::parse;
    use crate::Language;

    let tree = parse(source, Language::Python).ok()?;
    let root = tree.root_node();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        if child.kind() == "expression_statement" {
            if let Some(inner) = child.child(0) {
                if inner.kind() == "assignment" {
                    if let Some(left) = inner.child_by_field_name("left") {
                        let name = &source[left.byte_range()];
                        if name == "__all__" {
                            if let Some(right) = inner.child_by_field_name("right") {
                                return Some(extract_string_list_elements(&right, source));
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

/// Extract string elements from a list or tuple literal node.
fn extract_string_list_elements(node: &tree_sitter::Node, source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() == "string" {
            let text = &source[child.byte_range()];
            // Strip quotes
            let unquoted = text
                .trim_start_matches(&['"', '\''][..])
                .trim_end_matches(&['"', '\''][..]);
            if !unquoted.is_empty() {
                names.push(unquoted.to_string());
            }
        }
    }

    names
}

/// Walk a directory (or accept a single file) to find all Python source files.
///
/// If `root` is a `.py` file path (single-file module), returns just that file.
/// If `root` is a directory, walks recursively collecting all `.py` files.
/// Returns paths sorted for deterministic output.
pub fn find_python_files(root: &Path) -> Vec<PathBuf> {
    // Single-file module: root_dir IS the .py file itself
    if root.is_file() && root.extension().and_then(|e| e.to_str()) == Some("py") {
        return vec![root.to_path_buf()];
    }
    let mut files = Vec::new();
    find_python_files_recursive(root, &mut files, root.join("__init__.py").is_file());
    files.sort();
    files
}

/// Recursive helper for finding Python files.
fn find_python_files_recursive(dir: &Path, files: &mut Vec<PathBuf>, in_package_tree: bool) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !should_skip_python_dir(dir_name, in_package_tree) {
                let child_in_package_tree = in_package_tree || path.join("__init__.py").is_file();
                find_python_files_recursive(&path, files, child_in_package_tree);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("py") {
            files.push(path);
        }
    }
}

fn should_skip_python_dir(dir_name: &str, in_package_tree: bool) -> bool {
    if dir_name.starts_with('.') {
        return true;
    }

    match dir_name {
        "__pycache__" | "node_modules" | ".git" => true,
        _ if in_package_tree => false,
        _ => {
            let lower = dir_name.to_ascii_lowercase();
            is_noise_dir(crate::types::Language::Python, &lower)
                || matches!(
                    lower.as_str(),
                    "sample" | "samples" | "demo" | "demos" | "tutorial" | "tutorials"
                )
        }
    }
}

/// Check if a resolved package path is a built-in module marker.
///
/// When `resolve_python_package` encounters a C built-in module (no `__file__`),
/// it stores the path as `<builtin:module_name>`. This function detects that
/// pattern so callers can dispatch to introspection-based extraction.
pub fn is_builtin_marker_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.starts_with("<builtin:") && s.ends_with('>')
}

/// Introspect a Python built-in or C extension module to get its public names.
///
/// Uses `python3 -c "import <module>; print(\\n.join(...))"` to get the list
/// of public names from `dir(module)`. This is a best-effort approach for
/// modules that have no parseable `.py` source (e.g., `itertools`, `sys`).
///
/// # Arguments
/// * `module_name` - Python module name (e.g., "itertools", "sys")
///
/// # Returns
/// * `Ok(Vec<String>)` - List of public names (functions, classes, constants)
/// * `Err(TldrError)` - If the module cannot be imported or python3 is unavailable
pub fn introspect_builtin_module(module_name: &str) -> TldrResult<Vec<String>> {
    if !is_valid_python_identifier(module_name) {
        return Err(TldrError::parse_error(
            PathBuf::new(),
            None,
            format!("Invalid Python module name: {}", module_name),
        ));
    }

    let script = format!(
        "import {mod}\nfor name in dir({mod}):\n    if not name.startswith('_'):\n        print(name)",
        mod = module_name
    );

    let output = Command::new("python3")
        .arg("-c")
        .arg(&script)
        .output()
        .map_err(|e| {
            TldrError::parse_error(
                PathBuf::new(),
                None,
                format!(
                    "Failed to run python3 for built-in module introspection of '{}': {}",
                    module_name, e
                ),
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(TldrError::parse_error(
            PathBuf::new(),
            None,
            format!(
                "Cannot introspect Python module '{}': {}",
                module_name,
                stderr.trim()
            ),
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let names: Vec<String> = stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    Ok(names)
}

/// Check if a directory contains C extension files (.so, .pyd).
pub fn has_c_extensions(dir: &Path) -> bool {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if ext == "so" || ext == "pyd" {
                    return true;
                }
            }
        }
    }
    false
}

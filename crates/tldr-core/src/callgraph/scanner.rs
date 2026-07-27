//! File scanning, language detection, and directory walking for Builder V2.
//!
//! This module handles project file discovery:
//! - Language support detection and normalization
//! - ScannedFile struct with TOCTOU protection
//! - Directory walking with ignore patterns
//! - Symlink cycle detection
//!
//! Depends only on types.rs (BuildConfig, BuildError) and crate::types::Language.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::WalkBuilder;

use super::types::{BuildConfig, BuildError};
use crate::types::Language;

// =============================================================================
// Language Support
// =============================================================================

const SUPPORTED_LANGUAGES: &[&str] = &[
    "python",
    "typescript",
    "javascript",
    "go",
    "rust",
    "java",
    "c",
    "cpp",
    "csharp",
    "kotlin",
    "scala",
    "swift",
    "php",
    "ruby",
    "lua",
    "luau",
    "elixir",
    "ocaml",
];

pub(crate) fn normalize_language_string(language: &str) -> String {
    match language.to_lowercase().as_str() {
        "py" => "python".to_string(),
        "ts" | "tsx" => "typescript".to_string(),
        "js" | "jsx" => "javascript".to_string(),
        "golang" => "go".to_string(),
        "rs" => "rust".to_string(),
        "rb" => "ruby".to_string(),
        "kt" => "kotlin".to_string(),
        "c++" | "cxx" => "cpp".to_string(),
        "c#" | "cs" => "csharp".to_string(),
        "ex" => "elixir".to_string(),
        "ml" => "ocaml".to_string(),
        other => other.to_string(),
    }
}

/// Check if a language is supported.
pub(crate) fn is_supported_language(language: &str) -> bool {
    SUPPORTED_LANGUAGES.contains(&language.to_lowercase().as_str())
}

// =============================================================================
// ScannedFile (Mitigation M2.7 - TOCTOU Protection)
// =============================================================================

/// A scanned file with metadata for TOCTOU protection.
///
/// Per Mitigation M2.7: "Record mtime during scan for later validation."
/// This allows detection of files that change between scanning and parsing.
///
/// # Example
/// ```rust,ignore
/// let files = scan_project_files(root, "python", &config)?;
/// for scanned in files {
///     // Later, before parsing, we can verify the file hasn't changed
///     if scanned.verify_unchanged().is_err() {
///         eprintln!("File modified: {:?}", scanned.path);
///         continue;
///     }
/// }
/// ```
#[derive(Debug, Clone)]
pub struct ScannedFile {
    /// Path to the file (absolute).
    pub path: PathBuf,

    /// File modification time at scan time.
    pub mtime: SystemTime,

    /// File size in bytes at scan time.
    pub size: u64,
}

impl ScannedFile {
    /// Create a new ScannedFile from path with current metadata.
    pub fn from_path(path: PathBuf) -> std::io::Result<Self> {
        let metadata = fs::metadata(&path)?;
        Ok(Self {
            path,
            mtime: metadata.modified()?,
            size: metadata.len(),
        })
    }

    /// Verify the file hasn't changed since scanning.
    ///
    /// Returns `Ok(())` if file metadata matches, `Err` with description otherwise.
    pub fn verify_unchanged(&self) -> Result<(), String> {
        let current_meta =
            fs::metadata(&self.path).map_err(|e| format!("Cannot read file: {}", e))?;

        let current_mtime = current_meta
            .modified()
            .map_err(|e| format!("Cannot read mtime: {}", e))?;

        if current_mtime != self.mtime {
            return Err(format!(
                "File modified: scanned at {:?}, now {:?}",
                self.mtime, current_mtime
            ));
        }

        if current_meta.len() != self.size {
            return Err(format!(
                "File size changed: was {} bytes, now {} bytes",
                self.size,
                current_meta.len()
            ));
        }

        Ok(())
    }
}

// =============================================================================
// File Discovery (Spec Section 14.4 Step 2)
// =============================================================================

/// Directories to always skip during file discovery (regardless of
/// the requested language).
///
/// language-specific-bugs-v1 (P14.AGG14-7): `build` and `dist` were
/// previously listed here unconditionally, so a TypeScript repo that
/// keeps its actual source under `src/build/` (e.g. ts-dom-gen, where
/// `src/build/emitter.ts` is the entire implementation surface) would
/// have every authored file silently excluded — `tldr calls` returned
/// 0 nodes / 0 edges. For JS/TS specifically, defer the `build` /
/// `dist` skip to [`should_skip_build_or_dist_for_lang`] so the walker
/// only excludes them when the language convention treats them as
/// generated output (Rust / Java / Kotlin / Go / Python builds).
const SKIP_DIRECTORIES: &[&str] = &[
    ".git",
    "__pycache__",
    "node_modules",
    ".tox",
    "venv",
    ".venv",
    "__pypackages__",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    "target",     // Rust
    ".next",      // Next.js
    ".nuxt",      // Nuxt.js
    "vendor",     // Go, PHP
    ".bundle",    // Ruby
    "Pods",       // iOS
    ".gradle",    // Gradle
    ".idea",      // JetBrains
    ".vscode",    // VSCode
    ".eggs",      // Python
    "*.egg-info", // Python
    ".coverage",  // Python coverage
    "htmlcov",    // Python coverage
];

/// language-specific-bugs-v1 (P14.AGG14-7): per-language gate for the
/// `build` and `dist` directories. JS/TS projects routinely keep
/// authored source under these names (`src/build/`, monorepo
/// `packages/x/dist/`); other languages (Rust uses `target/`, Java uses
/// `build/` for gradle output, Python uses `build/` for setup.py) treat
/// them as build sinks. When the requested language is JavaScript or
/// TypeScript, do NOT skip these dirs — defer to `.gitignore` if the
/// project genuinely wants them excluded.
fn should_skip_build_or_dist_for_lang(name: &str, language: &str) -> bool {
    if !matches!(name, "build" | "dist" | "out" | "bin" | "obj") {
        return false;
    }
    let is_js_ts = matches!(
        language.to_lowercase().as_str(),
        "javascript" | "typescript" | "js" | "ts" | "jsx" | "tsx"
    );
    !is_js_ts
}

fn resolve_scan_roots(root: &Path, config: &BuildConfig) -> Result<Vec<PathBuf>, BuildError> {
    if !config.use_workspace_config {
        return Ok(vec![root.to_path_buf()]);
    }

    if config.workspace_roots.is_empty() {
        return Err(BuildError::WorkspaceConfig(
            "Workspace roots not provided".to_string(),
        ));
    }

    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    for workspace_root in &config.workspace_roots {
        let candidate = if workspace_root.is_absolute() {
            workspace_root.clone()
        } else {
            root.join(workspace_root)
        };
        let candidate = dunce::simplified(&candidate).to_path_buf();

        if !candidate.exists() {
            return Err(BuildError::WorkspaceConfig(format!(
                "Workspace root not found: {}",
                candidate.display()
            )));
        }
        if !candidate.is_dir() {
            return Err(BuildError::WorkspaceConfig(format!(
                "Workspace root is not a directory: {}",
                candidate.display()
            )));
        }
        if !candidate.starts_with(root) {
            return Err(BuildError::WorkspaceConfig(format!(
                "Workspace root {} is outside project root {}",
                candidate.display(),
                root.display()
            )));
        }

        if seen.insert(candidate.clone()) {
            roots.push(candidate);
        }
    }

    if roots.is_empty() {
        return Err(BuildError::WorkspaceConfig(
            "Workspace roots resolved to empty set".to_string(),
        ));
    }

    Ok(roots)
}

/// Scan a project directory for source files of the specified language.
///
/// This function discovers all relevant source files in the project,
/// respecting ignore patterns and detecting symlink cycles.
///
/// # Arguments
/// * `root` - Project root directory (must exist and be a directory)
/// * `language` - Language to scan for (e.g., "python", "typescript")
/// * `config` - Build configuration with ignore settings
///
/// # Returns
/// * `Ok(Vec<ScannedFile>)` - List of discovered files with metadata
/// * `Err(BuildError)` - If root doesn't exist or language is unsupported
///
/// # Behavior
/// - Returns empty Vec for empty projects (not an error)
/// - Skips standard ignored directories (.git, __pycache__, node_modules, etc.)
/// - Respects .tldrignore patterns when `config.respect_ignore` is true
/// - Detects and breaks symlink cycles via canonical path comparison
/// - Restricts scanning to workspace roots when `config.use_workspace_config` is true
///
/// # Example
/// ```rust,ignore
/// let config = BuildConfig {
///     language: "python".to_string(),
///     respect_ignore: true,
///     ..Default::default()
/// };
/// let files = scan_project_files(root, "python", &config)?;
/// println!("Found {} Python files", files.len());
/// ```
pub fn scan_project_files(
    root: &Path,
    language: &str,
    config: &BuildConfig,
) -> Result<Vec<ScannedFile>, BuildError> {
    // Validate root exists and is a directory
    if !root.exists() {
        return Err(BuildError::RootNotFound(root.to_path_buf()));
    }
    if !root.is_dir() {
        return Err(BuildError::RootNotFound(root.to_path_buf()));
    }

    // Get canonical root for symlink cycle detection
    let canonical_root = root.canonicalize().map_err(BuildError::Io)?;

    let scan_roots = resolve_scan_roots(root, config)?;

    // Get language extensions
    let extensions = get_language_extensions(language)?;

    // Track visited canonical paths for symlink cycle detection
    let mut visited_dirs: HashSet<PathBuf> = HashSet::new();
    visited_dirs.insert(canonical_root.clone());
    for scan_root in &scan_roots {
        if let Ok(canonical) = scan_root.canonicalize() {
            visited_dirs.insert(canonical);
        }
    }

    let mut files = Vec::new();
    let mut seen_files: HashSet<PathBuf> = HashSet::new();

    for scan_root in scan_roots {
        // Walk the directory tree
        let lang_for_filter = language.to_string();
        let mut walker = WalkBuilder::new(&scan_root);
        walker
            .hidden(true)
            .git_ignore(config.respect_ignore)
            .git_global(config.respect_ignore)
            .git_exclude(config.respect_ignore)
            .parents(config.respect_ignore)
            .follow_links(true)
            .filter_entry(move |entry| {
                let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                let file_name = entry.file_name().to_string_lossy();

                if entry.depth() > 0 && file_name.starts_with('.') {
                    return false;
                }
                if is_dir && should_skip_directory(&file_name) {
                    return false;
                }
                if is_dir && should_skip_build_or_dist_for_lang(&file_name, &lang_for_filter) {
                    return false;
                }
                true
            });
        if config.respect_ignore {
            walker.add_custom_ignore_filename(crate::walker::TLDRIGNORE_FILE);
        }

        for entry_result in walker.build() {
            let entry = match entry_result {
                Ok(e) => e,
                Err(err) => {
                    // Log and skip errors (permission denied, broken symlinks, etc.)
                    if config.verbose {
                        eprintln!("Warning: skipping entry: {}", err);
                    }
                    continue;
                }
            };

            // Handle symlink cycle detection for directories
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                if let Ok(canonical) = entry.path().canonicalize() {
                    if !visited_dirs.insert(canonical.clone()) {
                        // Already visited this directory - symlink cycle detected
                        if config.verbose {
                            eprintln!(
                                "Warning: symlink cycle detected at {:?}, skipping",
                                entry.path()
                            );
                        }
                        continue;
                    }
                }
                continue; // Directories don't get added to files list
            }

            // Only process regular files
            if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                continue;
            }

            let path = entry.path();

            // Check file extension matches language
            if !has_matching_extension(path, &extensions) {
                continue;
            }

            if !seen_files.insert(path.to_path_buf()) {
                continue;
            }

            // Create ScannedFile with metadata
            match ScannedFile::from_path(path.to_path_buf()) {
                Ok(scanned) => files.push(scanned),
                Err(err) => {
                    if config.verbose {
                        eprintln!("Warning: cannot read metadata for {:?}: {}", path, err);
                    }
                }
            }
        }
    }

    // high-bundle-progress-determinism-coverage-v1 (N2): sort scanned files
    // so the parallel index-build phase processes them in a stable order.
    // `walkdir` does not guarantee directory-iteration order on macOS, and
    // when two functions in different files share a `simple_module` alias,
    // the first writer wins — so a different scan order produces a
    // different func_index and therefore a different edge set.
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Check if a path should be skipped based on configuration.
///
/// This is a utility function for determining whether to process a path.
///
/// # Arguments
/// * `path` - Path to check
/// * `config` - Build configuration
///
/// # Returns
/// * `true` if the path should be skipped
/// * `false` if the path should be processed
pub fn should_skip_path(path: &Path, _config: &BuildConfig) -> bool {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();

    // Skip hidden files/directories
    if file_name.starts_with('.') {
        return true;
    }

    // Skip known directories
    if path.is_dir() && should_skip_directory(&file_name) {
        return true;
    }

    // Note: .tldrignore checking is done separately in scan_project_files
    // because it requires the gitignore matcher and relative path calculation

    false
}

/// Check if a directory name should be skipped.
pub(crate) fn should_skip_directory(name: &str) -> bool {
    SKIP_DIRECTORIES.iter().any(|skip| {
        if skip.contains('*') {
            // Simple glob pattern matching for patterns like "*.egg-info"
            let pattern = skip.replace("*", "");
            name.ends_with(&pattern)
        } else {
            name == *skip
        }
    })
}

/// Get file extensions for a language.
pub(crate) fn get_language_extensions(language: &str) -> Result<Vec<&'static str>, BuildError> {
    // Map language string to Language enum
    let lang = match language.to_lowercase().as_str() {
        "python" => Language::Python,
        "typescript" => Language::TypeScript,
        "javascript" => Language::JavaScript,
        "go" => Language::Go,
        "rust" => Language::Rust,
        "java" => Language::Java,
        "c" => Language::C,
        "cpp" => Language::Cpp,
        "csharp" => Language::CSharp,
        "kotlin" => Language::Kotlin,
        "scala" => Language::Scala,
        "swift" => Language::Swift,
        "php" => Language::Php,
        "ruby" => Language::Ruby,
        "lua" => Language::Lua,
        "luau" => Language::Luau,
        "elixir" => Language::Elixir,
        "ocaml" => Language::Ocaml,
        _ => return Err(BuildError::UnsupportedLanguage(language.to_string())),
    };

    // language-coverage-fixes-v1 (P4.BUG-N1, P4.BUG-N5): use
    // `scan_extensions()` so the call-graph scanner picks up `.h` for
    // C++ projects and the JS/TS sibling family for mixed React/Node
    // directories. The per-file callgraph handler still ignores
    // foreign-extension files via its own `extensions()` filter, so
    // this only affects file enumeration, not parsing dispatch.
    Ok(lang.scan_extensions().to_vec())
}

/// Check if a path has a matching file extension.
fn has_matching_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            let with_dot = format!(".{}", ext.to_lowercase());
            extensions.iter().any(|e| e.to_lowercase() == with_dot)
        })
        .unwrap_or(false)
}

/// Load .tldrignore patterns from project root.
fn load_tldrignore(root: &Path) -> Option<Gitignore> {
    let tldrignore_path = root.join(".tldrignore");

    if !tldrignore_path.exists() {
        return None;
    }

    let mut builder = GitignoreBuilder::new(root);

    // Add patterns from .tldrignore
    if builder.add(&tldrignore_path).is_some() {
        // Error adding file, return None
        return None;
    }

    builder.build().ok()
}

/// Filter a list of paths through `.tldrignore` patterns.
///
/// Loads `.tldrignore` from `root` and removes any paths that match its
/// patterns. If no `.tldrignore` file exists, returns the input unchanged.
///
/// Uses `matched_path_or_any_parents` so that directory patterns like
/// `corpus/` correctly filter files nested under that directory.
pub fn filter_tldrignored(root: &Path, paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let ignore = match load_tldrignore(root) {
        Some(ig) => ig,
        None => return paths,
    };

    paths
        .into_iter()
        .filter(|p| {
            let is_dir = p.is_dir();
            !ignore.matched_path_or_any_parents(p, is_dir).is_ignore()
        })
        .collect()
}

// =============================================================================
// Tests
// =============================================================================

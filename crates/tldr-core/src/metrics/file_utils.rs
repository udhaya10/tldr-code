//! File utilities for metrics analysis (Session 15)
//!
//! This module provides file handling utilities for metrics commands:
//! - Binary file detection
//! - File size validation
//! - Symlink safety
//! - Skip patterns (node_modules, .git, etc.)
//!
//! # Mitigations
//!
//! - CM-1: Large files (>10MB) and circular symlinks cause crashes
//! - CM-2: Encoding issues handled via encoding.rs module

use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::types::Language;
use crate::walker::ProjectWalker;
use crate::TldrError;

// =============================================================================
// Walk Options
// =============================================================================

/// Options for walking source files in a directory.
#[derive(Debug, Clone)]
pub struct WalkOptions {
    /// Filter to specific language (None = all supported languages)
    pub lang: Option<Language>,
    /// Exclude patterns (glob syntax)
    pub exclude: Vec<String>,
    /// Include hidden files/directories (default: false)
    pub include_hidden: bool,
    /// Respect .gitignore rules (default: true)
    pub gitignore: bool,
    /// Maximum files to return (0 = unlimited)
    pub max_files: usize,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            lang: None,
            exclude: Vec::new(),
            include_hidden: false,
            gitignore: true,
            max_files: 0,
        }
    }
}

/// Walk a path and return source files matching the given options.
///
/// - If `path` is a file: returns `vec![path.to_path_buf()]` (no filtering applied,
///   caller is responsible for language validation on single-file input).
/// - If `path` is a directory: walks recursively, filtering by language support
///   and the provided options.
///
/// # Errors
///
/// Returns `TldrError::PathNotFound` if the path does not exist.
///
/// # Warnings
///
/// Walk errors and max_files truncation are reported via the returned warnings vec.
pub fn walk_source_files(
    path: &Path,
    options: &WalkOptions,
) -> Result<(Vec<PathBuf>, Vec<String>), TldrError> {
    if !path.exists() {
        return Err(TldrError::PathNotFound(path.to_path_buf()));
    }

    // Single file: return as-is without filtering
    if path.is_file() {
        return Ok((vec![path.to_path_buf()], vec![]));
    }

    // Directory walk via the canonical ProjectWalker (TLDR-boa.3): honors
    // `.gitignore`/`.tldrignore`, hidden files, `DEFAULT_EXCLUDE_DIRS` and
    // generated-dir sentinels — the same policy every other walk uses.
    // `should_skip_path` is kept as a backstop so the `--include-hidden` path
    // still applies its `.github`/`.claude` exception exactly; it is redundant
    // in the default case where ProjectWalker already pruned these.
    let mut files = Vec::new();
    let mut warnings = Vec::new();
    let mut had_entries = false;

    let mut walker = ProjectWalker::new(path).respect_gitignore(options.gitignore);
    if options.include_hidden {
        walker = walker.include_hidden();
    }

    for entry in walker.iter() {
        let entry_path = entry.path();

        // Skip directories (ProjectWalker yields them so the walk can descend).
        if entry_path.is_dir() {
            continue;
        }

        had_entries = true;

        // Check max files limit
        if options.max_files > 0 && files.len() >= options.max_files {
            warnings.push(format!(
                "Stopped after {} files (max_files limit)",
                options.max_files
            ));
            break;
        }

        // Get relative path for pattern checking
        let relative_path = entry_path.strip_prefix(path).unwrap_or(entry_path);

        // Backstop skip patterns (node_modules, .git, etc.) — see note above.
        if should_skip_path(relative_path) {
            continue;
        }

        // Skip paths matching user exclude patterns
        if should_exclude(relative_path, &options.exclude) {
            continue;
        }

        // Detect language - skip unsupported files
        let lang = match Language::from_path(entry_path) {
            Some(l) => l,
            None => continue,
        };

        // Filter by language if specified
        if let Some(filter_lang) = options.lang {
            if lang != filter_lang {
                continue;
            }
        }

        files.push(entry_path.to_path_buf());
    }

    // Warn if directory had entries but no supported source files
    if files.is_empty() && had_entries {
        warnings.push(format!(
            "No supported source files found in {}",
            path.display()
        ));
    }

    Ok((files, warnings))
}

/// Check if a path should be excluded based on glob patterns.
///
/// Used by the directory walker and LOC analysis to filter out files
/// matching user-specified exclude patterns.
pub fn should_exclude(path: &Path, patterns: &[String]) -> bool {
    let path_str = path.to_string_lossy();

    for pattern in patterns {
        if let Ok(glob) = glob::Pattern::new(pattern) {
            if glob.matches(&path_str) {
                return true;
            }
        }
    }

    false
}

// =============================================================================
// Constants
// =============================================================================

/// Default maximum file size in bytes (10MB)
pub const DEFAULT_MAX_FILE_SIZE: usize = 10 * 1024 * 1024;

/// Default maximum file size in megabytes
pub const DEFAULT_MAX_FILE_SIZE_MB: usize = 10;

// The default skip list was unified onto the canonical
// `crate::walker::DEFAULT_EXCLUDE_DIRS` in TLDR-boa.7. The dotdir entries the
// old `SKIP_DIRS` carried beyond the canonical list (`.svn`/`.hg`/`.venv`/
// `.env`/`.idea`/`.vscode`) are still pruned — by the hidden-file check in
// [`should_skip_path_with_lang`], not by name.

/// File extensions that are typically binary
const BINARY_EXTENSIONS: &[&str] = &[
    // Images
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "webp", "svg", "tiff", "psd",
    // Audio/Video
    "mp3", "mp4", "avi", "mkv", "mov", "wav", "flac", "ogg", "webm", // Archives
    "zip", "tar", "gz", "bz2", "xz", "7z", "rar", // Binaries
    "exe", "dll", "so", "dylib", "a", "o", "obj", "class", "pyc", "pyo",
    // Documents (binary formats)
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", // Databases
    "db", "sqlite", "sqlite3", // Fonts
    "ttf", "otf", "woff", "woff2", "eot", // Other
    "lock", "bin", "dat", "pak",
];

// =============================================================================
// File Size Utilities
// =============================================================================

/// Check if a file exceeds the maximum allowed size.
///
/// # Arguments
///
/// * `path` - Path to the file
/// * `max_mb` - Maximum allowed size in megabytes
///
/// # Returns
///
/// * `Ok(())` - File is within size limit
/// * `Err(TldrError::FileTooLarge)` - File exceeds size limit
///
/// # Example
///
/// ```rust,ignore
/// use tldr_core::metrics::file_utils::check_file_size;
///
/// check_file_size(Path::new("large_file.py"), 10)?;
/// ```
pub fn check_file_size(path: &Path, max_mb: usize) -> Result<(), TldrError> {
    let metadata = fs::metadata(path)?;
    let size_bytes = metadata.len() as usize;
    let max_bytes = max_mb * 1024 * 1024;

    if size_bytes > max_bytes {
        let size_mb = size_bytes / (1024 * 1024);
        return Err(TldrError::FileTooLarge {
            path: path.to_path_buf(),
            size_mb,
            max_mb,
        });
    }

    Ok(())
}

/// Get file size in bytes.
pub fn get_file_size(path: &Path) -> Result<usize, TldrError> {
    let metadata = fs::metadata(path)?;
    Ok(metadata.len() as usize)
}

// =============================================================================
// Binary File Detection
// =============================================================================

/// Check if a file is binary by examining its content.
///
/// This function reads the first 8KB of the file and checks for null bytes.
/// Also checks file extension against known binary extensions.
///
/// # Arguments
///
/// * `path` - Path to the file
///
/// # Returns
///
/// * `true` - File is binary
/// * `false` - File appears to be text
///
/// # Example
///
/// ```rust,ignore
/// use tldr_core::metrics::file_utils::is_binary_file;
///
/// if is_binary_file(Path::new("image.png")) {
///     println!("Skipping binary file");
/// }
/// ```
pub fn is_binary_file(path: &Path) -> bool {
    // First check extension
    if let Some(ext) = path.extension() {
        if let Some(ext_str) = ext.to_str() {
            if BINARY_EXTENSIONS.contains(&ext_str.to_lowercase().as_str()) {
                return true;
            }
        }
    }

    // Then check content (first 8KB for null bytes)
    match fs::File::open(path) {
        Ok(mut file) => {
            let mut buffer = [0u8; 8192];
            match file.read(&mut buffer) {
                Ok(bytes_read) => buffer[..bytes_read].contains(&0),
                Err(_) => false, // Treat read errors as non-binary
            }
        }
        Err(_) => false, // Treat open errors as non-binary
    }
}

/// Check if a file has a binary extension (without reading content).
pub fn has_binary_extension(path: &Path) -> bool {
    if let Some(ext) = path.extension() {
        if let Some(ext_str) = ext.to_str() {
            return BINARY_EXTENSIONS.contains(&ext_str.to_lowercase().as_str());
        }
    }
    false
}

// =============================================================================
// Skip Pattern Utilities
// =============================================================================

/// Check if a path should be skipped based on common patterns.
///
/// Skips:
/// - Hidden files/directories (starting with .)
/// - node_modules, .git, __pycache__, etc.
/// - Build directories (target, build, dist)
///
/// # Arguments
///
/// * `path` - Path to check
///
/// # Returns
///
/// * `true` - Path should be skipped
/// * `false` - Path should be processed
///
/// # Example
///
/// ```rust,ignore
/// use tldr_core::metrics::file_utils::should_skip_path;
///
/// assert!(should_skip_path(Path::new("node_modules/package/index.js")));
/// assert!(!should_skip_path(Path::new("src/main.rs")));
/// ```
pub fn should_skip_path(path: &Path) -> bool {
    should_skip_path_with_lang(path, None)
}

// The local JS/TS-preserved copy was removed in TLDR-boa.7;
// [`should_skip_path_with_lang`] now uses `crate::walker::JS_TS_PRESERVED_DIRS`
// directly — the same gate `ProjectWalker` and the callgraph scanner share.

/// Like [`should_skip_path`] but with optional language context. When
/// language is JavaScript or TypeScript, the JS/TS-friendly subset of the
/// canonical `DEFAULT_EXCLUDE_DIRS` is preserved (deferred to `.gitignore`).
///
/// cross-cutting-and-clear-fix-bugs-v1 (P18.X4).
pub fn should_skip_path_with_lang(path: &Path, lang: Option<crate::types::Language>) -> bool {
    let preserve_js_ts = matches!(
        lang,
        Some(crate::types::Language::JavaScript) | Some(crate::types::Language::TypeScript)
    );
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            if let Some(name_str) = name.to_str() {
                // Skip hidden directories/files (but not . or ..)
                if name_str.starts_with('.') && name_str.len() > 1 {
                    // Allow .github, .claude directories
                    if !matches!(name_str, ".github" | ".claude") {
                        return true;
                    }
                }

                // Skip known directories (canonical list, shared with
                // `ProjectWalker` — TLDR-boa.7).
                if crate::walker::DEFAULT_EXCLUDE_DIRS.contains(&name_str) {
                    if preserve_js_ts && crate::walker::JS_TS_PRESERVED_DIRS.contains(&name_str) {
                        // JS/TS hint active and this is a name JS/TS
                        // callers commonly use for authored source —
                        // defer to `.gitignore`.
                        continue;
                    }
                    return true;
                }
            }
        }
    }
    false
}

/// Get the set of directories that should be skipped (the canonical
/// `DEFAULT_EXCLUDE_DIRS` — TLDR-boa.7 unified the old metrics-only list onto it).
pub fn skip_directories() -> HashSet<&'static str> {
    crate::walker::DEFAULT_EXCLUDE_DIRS
        .iter()
        .copied()
        .collect()
}

// =============================================================================
// Symlink Safety Utilities
// =============================================================================

/// Resolve a symlink safely, preventing circular references and external targets.
///
/// # Arguments
///
/// * `path` - Path to resolve (may be a symlink)
/// * `project_root` - Optional project root to validate target is within project
///
/// # Returns
///
/// * `Ok(PathBuf)` - Resolved path (canonical)
/// * `Err(TldrError::SymlinkCycle)` - Circular symlink detected
/// * `Err(TldrError::PathTraversal)` - Symlink points outside project
///
/// # Example
///
/// ```rust,ignore
/// use tldr_core::metrics::file_utils::resolve_symlink_safely;
///
/// let resolved = resolve_symlink_safely(
///     Path::new("link_to_file"),
///     Some(Path::new("/project/root"))
/// )?;
/// ```
pub fn resolve_symlink_safely(
    path: &Path,
    project_root: Option<&Path>,
) -> Result<PathBuf, TldrError> {
    // Track visited symlink paths (before resolution) to detect cycles
    let mut visited_links = HashSet::new();
    let mut current = path.to_path_buf();

    // Maximum symlink depth to prevent infinite loops
    const MAX_DEPTH: usize = 40;

    for _ in 0..MAX_DEPTH {
        // Check if it's a symlink
        let metadata = match fs::symlink_metadata(&current) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(TldrError::PathNotFound(current));
            }
            Err(e) => return Err(TldrError::IoError(e)),
        };

        if metadata.file_type().is_symlink() {
            // Track this symlink path to detect cycles
            // Use the absolute path of the symlink itself (not target)
            let link_abs = if current.is_absolute() {
                current.clone()
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(&current))
                    .unwrap_or_else(|_| current.clone())
            };

            if visited_links.contains(&link_abs) {
                return Err(TldrError::SymlinkCycle(path.to_path_buf()));
            }
            visited_links.insert(link_abs);

            // Read the symlink target
            let target = fs::read_link(&current)?;
            // Resolve relative targets
            current = if target.is_relative() {
                current.parent().map(|p| p.join(&target)).unwrap_or(target)
            } else {
                target
            };
        } else {
            // Not a symlink, we're done - get canonical path
            let canonical = match current.canonicalize() {
                Ok(c) => c,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(TldrError::PathNotFound(current));
                }
                Err(e) => return Err(TldrError::IoError(e)),
            };

            // Validate target is within project root (if specified)
            if let Some(root) = project_root {
                let root_canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
                if !canonical.starts_with(&root_canonical) {
                    return Err(TldrError::PathTraversal(path.to_path_buf()));
                }
            }
            return Ok(canonical);
        }
    }

    // Exceeded max depth - likely a cycle we couldn't detect
    Err(TldrError::SymlinkCycle(path.to_path_buf()))
}

/// Check if a path is a symlink.
pub fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

// =============================================================================
// Path Validation Utilities
// =============================================================================

/// Validate that a path is within a project root (no path traversal).
///
/// # Arguments
///
/// * `path` - Path to validate
/// * `project_root` - Root directory path must be within
///
/// # Returns
///
/// * `true` - Path is within project root
/// * `false` - Path is outside project root or contains traversal
pub fn is_path_within_project(path: &Path, project_root: &Path) -> bool {
    // Canonicalize both paths
    let path_canonical = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };

    let root_canonical = match project_root.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };

    path_canonical.starts_with(&root_canonical)
}

/// Check if a path contains path traversal patterns (.. components).
pub fn contains_path_traversal(path: &Path) -> bool {
    for component in path.components() {
        if let std::path::Component::ParentDir = component {
            return true;
        }
    }
    false
}

// =============================================================================
// Tests
// =============================================================================

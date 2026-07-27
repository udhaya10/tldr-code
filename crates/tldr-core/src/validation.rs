//! Shared validation helpers for CLI and daemon handlers
//!
//! These functions reduce duplication between:
//! - CLI commands (sync, returns TldrError)
//! - Daemon handlers (async, wraps to HandlerError)
//! - MCP tools (sync, wraps to JsonRpcError)

use std::path::{Path, PathBuf};

use crate::{Language, TldrError, TldrResult};

/// Resolve and validate a file path.
///
/// # Arguments
/// * `file` - The file path string (may be relative or absolute)
/// * `project` - Optional project root to resolve relative paths against
///
/// # Returns
/// * `Ok(PathBuf)` - Canonical path to the file
/// * `Err(TldrError::PathNotFound)` - File doesn't exist
/// * `Err(TldrError::PathTraversal)` - Path escapes project root
///
/// # Examples
///
/// ```rust,ignore
/// use tldr_core::validation::validate_file_path;
/// use std::path::Path;
///
/// // Relative path with project root
/// let result = validate_file_path("src/main.rs", Some(Path::new("/app")));
///
/// // Absolute path
/// let result = validate_file_path("/app/src/main.rs", None);
///
/// // Path traversal blocked
/// let result = validate_file_path("../escape.rs", Some(Path::new("/app/src")));
/// assert!(result.is_err()); // PathTraversal error
/// ```
pub fn validate_file_path(file: &str, project: Option<&Path>) -> TldrResult<PathBuf> {
    let path = PathBuf::from(file);

    // Resolve to absolute path
    let resolved = if path.is_absolute() {
        path.clone()
    } else if let Some(proj) = project {
        proj.join(&path)
    } else {
        std::env::current_dir()
            .map_err(TldrError::IoError)?
            .join(&path)
    };

    // Canonicalize (resolves symlinks, checks existence)
    // Use dunce for Windows compatibility (M18)
    let canonical =
        dunce::canonicalize(&resolved).map_err(|_| TldrError::PathNotFound(resolved.clone()))?;

    // Check for path traversal if project specified
    if let Some(proj) = project {
        let canonical_proj =
            dunce::canonicalize(proj).map_err(|_| TldrError::PathNotFound(proj.to_path_buf()))?;

        if !canonical.starts_with(&canonical_proj) {
            return Err(TldrError::PathTraversal(path));
        }
    }

    Ok(canonical)
}

/// Detect or parse programming language.
///
/// # Arguments
/// * `lang` - Optional explicit language string
/// * `path` - File path to detect language from (if lang is None)
///
/// # Returns
/// * `Ok(Language)` - Detected or parsed language
/// * `Err(TldrError::UnsupportedLanguage)` - Unknown language string
/// * `Err(TldrError::UnsupportedLanguage)` - Could not detect from path
///
/// # Examples
///
/// ```rust,ignore
/// use tldr_core::validation::detect_or_parse_language;
/// use tldr_core::Language;
/// use std::path::Path;
///
/// // Explicit language
/// let lang = detect_or_parse_language(Some("python"), Path::new("any.txt")).unwrap();
/// assert_eq!(lang, Language::Python);
///
/// // Auto-detect from extension
/// let lang = detect_or_parse_language(None, Path::new("script.py")).unwrap();
/// assert_eq!(lang, Language::Python);
///
/// // Error on unknown
/// let result = detect_or_parse_language(None, Path::new("file.xyz"));
/// assert!(result.is_err()); // UnsupportedLanguage error
/// ```
pub fn detect_or_parse_language(lang: Option<&str>, path: &Path) -> TldrResult<Language> {
    if let Some(lang_str) = lang {
        // Parse explicit language
        lang_str
            .parse()
            .map_err(|_| TldrError::UnsupportedLanguage(lang_str.to_string()))
    } else {
        // Detect from extension
        Language::from_path(path).ok_or_else(|| {
            TldrError::UnsupportedLanguage(format!(
                "Could not detect language for: {}",
                path.display()
            ))
        })
    }
}

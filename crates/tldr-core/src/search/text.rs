//! Regex-based text search
//!
//! Implements the `search` command functionality (spec Section 2.6.1).
//!
//! # Features
//! - Regex pattern matching across files
//! - Context lines around matches
//! - File type filtering by extension
//! - Respects ignore patterns
//! - Skips default directories (node_modules, __pycache__, etc.)

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::error::TldrError;
use crate::TldrResult;

/// A single search match result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchMatch {
    /// File path (relative to search root)
    pub file: PathBuf,
    /// Line number (1-indexed)
    pub line: u32,
    /// The matching line content
    pub content: String,
    /// Context lines before and after (if requested)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Vec<String>>,
}

/// Search files for a regex pattern
///
/// # Arguments
/// * `pattern` - Regex pattern to search for
/// * `root` - Root directory to search in
/// * `extensions` - Optional set of file extensions to include (e.g., `{".py", ".ts"}`)
/// * `context_lines` - Number of context lines before and after each match
/// * `max_results` - Maximum number of matches to return
/// * `max_files` - Maximum number of files to search
///
/// # Returns
/// * `Ok(Vec<SearchMatch>)` - List of matches found
/// * `Err(TldrError)` - If pattern is invalid or other error occurs
///
/// # Example
/// ```ignore
/// use std::collections::HashSet;
/// use tldr_core::search::text::search;
///
/// let extensions: HashSet<String> = [".py".to_string()].into_iter().collect();
/// let matches = search("def main", Path::new("src"), Some(&extensions), 2, 100, 100)?;
/// ```
pub fn search(
    pattern: &str,
    root: &Path,
    extensions: Option<&HashSet<String>>,
    context_lines: usize,
    max_results: usize,
    max_files: usize,
) -> TldrResult<Vec<SearchMatch>> {
    // Compile regex pattern
    let regex = Regex::new(pattern).map_err(|e| TldrError::ParseError {
        file: PathBuf::from("<pattern>"),
        line: None,
        message: format!("Invalid regex: {}", e),
    })?;

    // Validate root path
    if !root.exists() {
        return Err(TldrError::PathNotFound(root.to_path_buf()));
    }

    let canonical_root =
        dunce::canonicalize(root).map_err(|_| TldrError::PathNotFound(root.to_path_buf()))?;

    let mut results = Vec::new();
    let mut files_searched = 0;

    // Walk directory tree (ProjectWalker honors hidden files,
    // DEFAULT_EXCLUDE_DIRS, sentinels, `.gitignore` and `.tldrignore`).
    for entry in crate::walker::ProjectWalker::new(&canonical_root).iter() {
        // Check limits
        if results.len() >= max_results || files_searched >= max_files {
            break;
        }

        let path = entry.path();

        // Skip directories
        if entry
            .file_type()
            .map(|file_type| file_type.is_dir())
            .unwrap_or(false)
        {
            continue;
        }

        // Check extension filter
        if let Some(exts) = extensions {
            let has_match = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| {
                    let ext_with_dot = format!(".{}", e);
                    exts.contains(&ext_with_dot) || exts.contains(e)
                })
                .unwrap_or(false);

            if !has_match {
                continue;
            }
        }

        // Search file
        files_searched += 1;

        let file_matches = search_file(
            path,
            &canonical_root,
            &regex,
            context_lines,
            max_results.saturating_sub(results.len()),
        )?;

        results.extend(file_matches);
    }

    Ok(results)
}

/// Search a single file for matches
fn search_file(
    file_path: &Path,
    root: &Path,
    regex: &Regex,
    context_lines: usize,
    max_matches: usize,
) -> TldrResult<Vec<SearchMatch>> {
    // Read file content
    let content = match fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(e) => {
            // Skip binary files or files with encoding issues
            if e.kind() == std::io::ErrorKind::InvalidData {
                return Ok(Vec::new());
            }
            // Permission errors are recoverable
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                return Ok(Vec::new());
            }
            return Err(e.into());
        }
    };

    let lines: Vec<&str> = content.lines().collect();
    let mut matches = Vec::new();

    // Get relative path
    let relative_path = file_path
        .strip_prefix(root)
        .unwrap_or(file_path)
        .to_path_buf();

    for (line_idx, line) in lines.iter().enumerate() {
        if matches.len() >= max_matches {
            break;
        }

        if regex.is_match(line) {
            let context = if context_lines > 0 {
                Some(get_context(&lines, line_idx, context_lines))
            } else {
                None
            };

            matches.push(SearchMatch {
                file: relative_path.clone(),
                line: (line_idx + 1) as u32,
                content: line.to_string(),
                context,
            });
        }
    }

    Ok(matches)
}

/// Get context lines around a match
fn get_context(lines: &[&str], center_idx: usize, context_count: usize) -> Vec<String> {
    let start = center_idx.saturating_sub(context_count);
    let end = (center_idx + context_count + 1).min(lines.len());

    lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let line_num = start + i + 1;
            if start + i == center_idx {
                format!("{}: > {}", line_num, line)
            } else {
                format!("{}:   {}", line_num, line)
            }
        })
        .collect()
}

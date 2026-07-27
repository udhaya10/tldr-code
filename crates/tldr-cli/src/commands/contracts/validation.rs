//! Input validation and path safety utilities for Contracts & Flow commands.
//!
//! This module provides security-focused validation functions to mitigate:
//! - **TIGER-02**: Path traversal attacks via malicious file paths
//! - **TIGER-03**: Unbounded recursion in CFG/slice computation
//! - **TIGER-04**: Memory exhaustion from large SSA graphs
//! - **TIGER-08**: Stack overflow from deeply nested ASTs
//!
//! All file paths are canonicalized and checked against project boundaries.
//! Resource limits are enforced to prevent denial-of-service conditions.

use std::fs;
use std::path::{Path, PathBuf};

use super::error::{ContractsError, ContractsResult};

// =============================================================================
// Resource Limits (TIGER Mitigations)
// =============================================================================

/// Maximum file size for analysis (10 MB).
/// Files larger than this will be rejected (TIGER-04 partial mitigation).
pub const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Warning threshold for file size (1 MB).
/// Files larger than this emit a warning but are still processed.
pub const WARN_FILE_SIZE: u64 = 1024 * 1024;

/// Maximum CFG/slice recursion depth (TIGER-03 mitigation).
/// Prevents stack overflow from deeply recursive control flow analysis.
pub const MAX_CFG_DEPTH: usize = 1000;

/// Maximum SSA nodes to construct (TIGER-04 mitigation).
/// Prevents memory exhaustion from extremely large SSA graphs.
pub const MAX_SSA_NODES: usize = 100_000;

/// Maximum AST traversal depth (TIGER-08 mitigation).
/// Prevents stack overflow from deeply nested source code.
pub const MAX_AST_DEPTH: usize = 100;

/// Maximum function name length.
pub const MAX_FUNCTION_NAME_LEN: usize = 256;

/// Maximum number of conditions to report per function.
pub const MAX_CONDITIONS_PER_FUNCTION: usize = 100;

// =============================================================================
// Blocked System Directories
// =============================================================================

/// System directories that should never be analyzed (security measure).
/// Note: We specifically target sensitive system directories, not general
/// /var or /private paths which include temp files.
const BLOCKED_PREFIXES: &[&str] = &[
    "/etc/",
    "/etc/passwd",
    "/etc/shadow",
    "/root/",
    "/sys/",
    "/proc/",
    "/dev/",
    "/var/run/",
    "/var/log/",
    "/private/etc/",  // macOS system config
    "C:\\Windows\\",  // Windows
    "C:\\System32\\", // Windows
];

// =============================================================================
// Path Validation (TIGER-02 Mitigation)
// =============================================================================

/// Validate and canonicalize a file path.
///
/// This function:
/// 1. Checks that the path exists
/// 2. Canonicalizes the path (resolves symlinks, `.`, `..`)
/// 3. Rejects paths that escape the project root (if specified)
/// 4. Rejects system directories
/// 5. Validates UTF-8 encoding
///
/// # Arguments
///
/// * `path` - The path to validate
///
/// # Returns
///
/// The canonicalized path if valid, or an error.
///
/// # Errors
///
/// - `ContractsError::FileNotFound` if the file doesn't exist
/// - `ContractsError::PathTraversal` if path escapes project or is a system dir
///
/// # Example
///
/// ```ignore
/// let valid = validate_file_path(Path::new("src/main.rs"))?;
/// assert!(valid.is_absolute());
/// ```
pub fn validate_file_path(path: &Path) -> ContractsResult<PathBuf> {
    // Check file exists
    if !path.exists() {
        return Err(ContractsError::FileNotFound {
            path: path.to_path_buf(),
        });
    }

    // Canonicalize the path (resolves symlinks, .., .)
    let canonical = fs::canonicalize(path).map_err(|_| ContractsError::FileNotFound {
        path: path.to_path_buf(),
    })?;

    // Check for system directories
    let canonical_str = canonical.to_string_lossy();
    for blocked in BLOCKED_PREFIXES {
        // Check with trailing slash for directories, or exact match for files
        if canonical_str.starts_with(blocked) || canonical_str == blocked.trim_end_matches('/') {
            return Err(ContractsError::PathTraversal {
                path: path.to_path_buf(),
            });
        }
    }

    // Validate UTF-8 (path.to_str() returns None if not valid UTF-8)
    if canonical.to_str().is_none() {
        return Err(ContractsError::PathTraversal {
            path: path.to_path_buf(),
        });
    }

    Ok(canonical)
}

/// Validate a file path ensuring it stays within a project root.
///
/// This is stricter than `validate_file_path` - it ensures the resolved
/// path is a descendant of the project root directory.
///
/// # Arguments
///
/// * `path` - The path to validate
/// * `project_root` - The project root directory to stay within
///
/// # Returns
///
/// The canonicalized path if valid and within project root.
///
/// # Errors
///
/// - `ContractsError::FileNotFound` if the file doesn't exist
/// - `ContractsError::PathTraversal` if path escapes project root
pub fn validate_file_path_in_project(path: &Path, project_root: &Path) -> ContractsResult<PathBuf> {
    // First do basic validation
    let canonical = validate_file_path(path)?;

    // Canonicalize project root too
    let canonical_root =
        fs::canonicalize(project_root).map_err(|_| ContractsError::FileNotFound {
            path: project_root.to_path_buf(),
        })?;

    // Check that canonical path starts with canonical root
    if !canonical.starts_with(&canonical_root) {
        return Err(ContractsError::PathTraversal {
            path: path.to_path_buf(),
        });
    }

    Ok(canonical)
}

/// Check if a path contains path traversal patterns.
///
/// This is a quick check for suspicious patterns before canonicalization.
/// Returns true if the path looks suspicious.
pub fn has_path_traversal_pattern(path: &Path) -> bool {
    let path_str = path.to_string_lossy();

    // Check for explicit traversal patterns
    if path_str.contains("..") {
        return true;
    }

    // Check for null bytes (could be used to truncate paths)
    if path_str.contains('\0') {
        return true;
    }

    false
}

// =============================================================================
// Line Number Validation
// =============================================================================

/// Validate line number range.
///
/// Ensures:
/// - start <= end
/// - Both are within valid range (1 to max)
///
/// # Arguments
///
/// * `start` - Start line (1-indexed)
/// * `end` - End line (1-indexed)
/// * `max` - Maximum valid line number (typically file line count)
///
/// # Returns
///
/// Ok(()) if valid, error otherwise.
///
/// # Errors
///
/// - Returns error if start > end
/// - Returns error if either line exceeds max
/// - Returns error if either line is 0
pub fn validate_line_numbers(start: u32, end: u32, max: u32) -> ContractsResult<()> {
    // Lines are 1-indexed
    if start == 0 {
        return Err(ContractsError::LineOutsideFunction {
            line: start,
            function: "unknown".to_string(),
            start: 1,
            end: max,
        });
    }

    if end == 0 {
        return Err(ContractsError::LineOutsideFunction {
            line: end,
            function: "unknown".to_string(),
            start: 1,
            end: max,
        });
    }

    // Start must be <= end
    if start > end {
        return Err(ContractsError::LineOutsideFunction {
            line: start,
            function: "unknown".to_string(),
            start: 1,
            end,
        });
    }

    // Both must be within bounds
    if start > max {
        return Err(ContractsError::LineOutsideFunction {
            line: start,
            function: "unknown".to_string(),
            start: 1,
            end: max,
        });
    }

    if end > max {
        return Err(ContractsError::LineOutsideFunction {
            line: end,
            function: "unknown".to_string(),
            start: 1,
            end: max,
        });
    }

    Ok(())
}

// =============================================================================
// Function Name Validation
// =============================================================================

/// Validate a function name for safety.
///
/// Ensures the name:
/// - Is not empty
/// - Contains only valid identifier characters
/// - Doesn't exceed maximum length
/// - Doesn't contain suspicious characters
///
/// # Arguments
///
/// * `name` - The function name to validate
///
/// # Returns
///
/// Ok(()) if valid, error otherwise.
///
/// # Errors
///
/// - `ContractsError::InvalidFunctionName` for invalid names
pub fn validate_function_name(name: &str) -> ContractsResult<()> {
    // Check empty
    if name.is_empty() {
        return Err(ContractsError::InvalidFunctionName {
            reason: "function name cannot be empty".to_string(),
        });
    }

    // Check length
    if name.len() > MAX_FUNCTION_NAME_LEN {
        return Err(ContractsError::InvalidFunctionName {
            reason: format!(
                "function name too long ({} chars, max {})",
                name.len(),
                MAX_FUNCTION_NAME_LEN
            ),
        });
    }

    // Check for suspicious characters that could be used for injection
    // Valid identifiers: letters, digits, underscore (and some languages allow $)
    let suspicious_chars = [
        ';', '(', ')', '{', '}', '[', ']', '`', '"', '\'', '\\', '/', '\0',
    ];
    for c in name.chars() {
        if suspicious_chars.contains(&c) {
            return Err(ContractsError::InvalidFunctionName {
                reason: format!("function name contains invalid character: '{}'", c),
            });
        }
    }

    // First character should be letter or underscore (standard identifier rules)
    if let Some(first) = name.chars().next() {
        if !first.is_alphabetic() && first != '_' {
            return Err(ContractsError::InvalidFunctionName {
                reason: "function name must start with letter or underscore".to_string(),
            });
        }
    }

    Ok(())
}

// =============================================================================
// Safe File Reading
// =============================================================================

/// Safely read a file with size limits and UTF-8 validation.
///
/// This function:
/// 1. Validates the file path
/// 2. Checks file size against limits
/// 3. Reads the file content
/// 4. Validates UTF-8 encoding
///
/// # Arguments
///
/// * `path` - The path to the file to read
///
/// # Returns
///
/// The file contents as a String if successful.
///
/// # Errors
///
/// - `ContractsError::FileNotFound` if file doesn't exist
/// - `ContractsError::FileTooLarge` if file exceeds MAX_FILE_SIZE
/// - `ContractsError::Io` for other IO errors
pub fn read_file_safe(path: &Path) -> ContractsResult<String> {
    // Validate path first
    let canonical = validate_file_path(path)?;

    // Check file size
    let metadata = fs::metadata(&canonical)?;
    let size = metadata.len();

    if size > MAX_FILE_SIZE {
        return Err(ContractsError::FileTooLarge {
            path: path.to_path_buf(),
            bytes: size,
            max_bytes: MAX_FILE_SIZE,
        });
    }

    // Read the file
    let content = fs::read(&canonical)?;

    // Validate UTF-8
    String::from_utf8(content).map_err(|_| ContractsError::ParseError {
        file: path.to_path_buf(),
        message: "file is not valid UTF-8".to_string(),
    })
}

/// Read a file safely, emitting a warning for large files.
///
/// Like `read_file_safe`, but also logs a warning to stderr for files
/// larger than WARN_FILE_SIZE.
///
/// # Arguments
///
/// * `path` - The path to the file to read
/// * `warn_fn` - Optional callback for warnings (if None, prints to stderr)
///
/// # Returns
///
/// The file contents as a String if successful.
pub fn read_file_safe_with_warning<F>(path: &Path, warn_fn: Option<F>) -> ContractsResult<String>
where
    F: FnOnce(&str),
{
    // Validate path first
    let canonical = validate_file_path(path)?;

    // Check file size
    let metadata = fs::metadata(&canonical)?;
    let size = metadata.len();

    if size > MAX_FILE_SIZE {
        return Err(ContractsError::FileTooLarge {
            path: path.to_path_buf(),
            bytes: size,
            max_bytes: MAX_FILE_SIZE,
        });
    }

    // Warn for large files
    if size > WARN_FILE_SIZE {
        let warning = format!(
            "Warning: {} is large ({:.1} MB), analysis may be slow",
            path.display(),
            size as f64 / 1024.0 / 1024.0
        );
        if let Some(f) = warn_fn {
            f(&warning);
        } else {
            eprintln!("{}", warning);
        }
    }

    // Read the file
    let content = fs::read(&canonical)?;

    // Validate UTF-8
    String::from_utf8(content).map_err(|_| ContractsError::ParseError {
        file: path.to_path_buf(),
        message: "file is not valid UTF-8".to_string(),
    })
}

// =============================================================================
// Depth Checking Utilities
// =============================================================================

/// Check if a depth limit has been exceeded.
///
/// Used for tracking recursion depth in CFG/slice analysis.
pub fn check_depth_limit(current_depth: usize, max_depth: usize) -> ContractsResult<()> {
    if current_depth >= max_depth {
        Err(ContractsError::SliceDepthExceeded {
            max_depth: max_depth as u32,
        })
    } else {
        Ok(())
    }
}

/// Check if SSA node count exceeds limit.
pub fn check_ssa_node_limit(node_count: usize) -> ContractsResult<()> {
    if node_count > MAX_SSA_NODES {
        Err(ContractsError::SsaTooLarge {
            nodes: node_count as u32,
            max_nodes: MAX_SSA_NODES as u32,
        })
    } else {
        Ok(())
    }
}

/// Check if AST depth exceeds limit.
pub fn check_ast_depth(depth: usize, file: &Path) -> ContractsResult<()> {
    if depth > MAX_AST_DEPTH {
        Err(ContractsError::AstTooDeep {
            file: file.to_path_buf(),
            depth: depth as u32,
            max_depth: MAX_AST_DEPTH as u32,
        })
    } else {
        Ok(())
    }
}

// =============================================================================
// Tests
// =============================================================================

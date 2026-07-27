//! Input validation and path safety utilities for Pattern Analysis commands.
//!
//! Provides security-focused validation functions to mitigate:
//! - **T01 - Path Traversal**: BLOCKED_PREFIXES for system directories
//! - **T02 - Project Root Enforcement**: validate_file_path_in_project()
//! - **T03 - Integer Overflow**: Checked arithmetic for depth calculations
//! - **T08 - Memory Exhaustion**: Resource limit constants
//!
//! All file paths are canonicalized and checked against project boundaries.
//! Resource limits are enforced to prevent denial-of-service conditions.

use std::fs;
use std::path::{Path, PathBuf};

use super::error::{PatternsError, PatternsResult};

// =============================================================================
// Resource Limits (TIGER-08 Mitigations)
// =============================================================================

/// Maximum file size for analysis (10 MB).
/// Files larger than this will be rejected.
pub const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Warning threshold for file size (1 MB).
/// Files larger than this emit a warning but are still processed.
pub const WARN_FILE_SIZE: u64 = 1024 * 1024;

/// Maximum files to scan in directory analysis.
pub const MAX_DIRECTORY_FILES: u32 = 1000;

/// Maximum AST traversal depth.
/// Prevents stack overflow from deeply nested source code.
pub const MAX_AST_DEPTH: usize = 100;

/// Maximum recursion depth for analysis algorithms.
/// Used for CFG path enumeration, temporal mining, etc.
pub const MAX_ANALYSIS_DEPTH: usize = 500;

/// Maximum function name length.
pub const MAX_FUNCTION_NAME_LEN: usize = 256;

/// Maximum constraints to report per file.
pub const MAX_CONSTRAINTS_PER_FILE: usize = 500;

/// Maximum methods per class for cohesion analysis.
pub const MAX_METHODS_PER_CLASS: usize = 200;

/// Maximum fields per class for cohesion analysis.
pub const MAX_FIELDS_PER_CLASS: usize = 100;

/// Maximum classes per file.
pub const MAX_CLASSES_PER_FILE: usize = 500;

/// Maximum CFG paths to enumerate (TIGER-04).
/// Prevents unbounded path enumeration in resources command.
pub const MAX_PATHS: usize = 1000;

/// Maximum trigrams to collect (TIGER-05).
/// Prevents memory exhaustion in temporal mining.
pub const MAX_TRIGRAMS: usize = 10000;

/// Maximum class complexity (methods * fields) for analysis.
pub const MAX_CLASS_COMPLEXITY: usize = 500;

// =============================================================================
// Blocked System Directories (TIGER-01)
// =============================================================================

/// System directories that should never be analyzed (security measure).
/// Note: We specifically target sensitive system directories.
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
// Path Validation (TIGER-01, TIGER-02)
// =============================================================================

/// Validate and canonicalize a file path.
///
/// This function:
/// 1. Checks that the path exists
/// 2. Canonicalizes the path (resolves symlinks, `.`, `..`)
/// 3. Rejects system directories
/// 4. Validates UTF-8 encoding
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
/// - `PatternsError::FileNotFound` if the file doesn't exist
/// - `PatternsError::PathTraversal` if path is a system dir or has invalid encoding
///
/// # Example
///
/// ```ignore
/// let valid = validate_file_path(Path::new("src/main.py"))?;
/// assert!(valid.is_absolute());
/// ```
pub fn validate_file_path(path: &Path) -> PatternsResult<PathBuf> {
    // Check file exists
    if !path.exists() {
        return Err(PatternsError::FileNotFound {
            path: path.to_path_buf(),
        });
    }

    // Canonicalize the path (resolves symlinks, .., .)
    let canonical = fs::canonicalize(path).map_err(|_| PatternsError::FileNotFound {
        path: path.to_path_buf(),
    })?;

    // Check for system directories
    let canonical_str = canonical.to_string_lossy();
    for blocked in BLOCKED_PREFIXES {
        // Check with trailing slash for directories, or exact match for files
        if canonical_str.starts_with(blocked) || canonical_str == blocked.trim_end_matches('/') {
            return Err(PatternsError::PathTraversal {
                path: path.to_path_buf(),
            });
        }
    }

    // Validate UTF-8 (path.to_str() returns None if not valid UTF-8)
    if canonical.to_str().is_none() {
        return Err(PatternsError::PathTraversal {
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
/// - `PatternsError::FileNotFound` if the file doesn't exist
/// - `PatternsError::PathTraversal` if path escapes project root
pub fn validate_file_path_in_project(path: &Path, project_root: &Path) -> PatternsResult<PathBuf> {
    // First do basic validation
    let canonical = validate_file_path(path)?;

    // Canonicalize project root too
    let canonical_root =
        fs::canonicalize(project_root).map_err(|_| PatternsError::FileNotFound {
            path: project_root.to_path_buf(),
        })?;

    // Check that canonical path starts with canonical root
    if !canonical.starts_with(&canonical_root) {
        return Err(PatternsError::PathTraversal {
            path: path.to_path_buf(),
        });
    }

    Ok(canonical)
}

/// Validate and canonicalize a directory path.
///
/// # Arguments
///
/// * `path` - The path to validate
///
/// # Returns
///
/// The canonicalized path if valid and is a directory.
///
/// # Errors
///
/// - `PatternsError::FileNotFound` if the directory doesn't exist
/// - `PatternsError::NotADirectory` if the path is not a directory
pub fn validate_directory_path(path: &Path) -> PatternsResult<PathBuf> {
    let canonical = validate_file_path(path)?;

    if !canonical.is_dir() {
        return Err(PatternsError::NotADirectory {
            path: path.to_path_buf(),
        });
    }

    Ok(canonical)
}

/// Check if a path contains path traversal patterns.
///
/// This is a quick check for suspicious patterns before canonicalization.
/// Returns true if the path looks suspicious.
///
/// # Arguments
///
/// * `path` - The path to check
///
/// # Returns
///
/// `true` if the path contains traversal patterns (`..\` or null bytes)
pub fn is_path_traversal_attempt(path: &Path) -> bool {
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
// File Size Validation (TIGER-08)
// =============================================================================

/// Validate file size against limits.
///
/// # Arguments
///
/// * `path` - The path to the file
///
/// # Returns
///
/// The file size in bytes if within limits.
///
/// # Errors
///
/// - `PatternsError::FileNotFound` if file doesn't exist
/// - `PatternsError::FileTooLarge` if file exceeds MAX_FILE_SIZE
pub fn validate_file_size(path: &Path) -> PatternsResult<u64> {
    let canonical = validate_file_path(path)?;

    let metadata = fs::metadata(&canonical)?;
    let size = metadata.len();

    if size > MAX_FILE_SIZE {
        return Err(PatternsError::FileTooLarge {
            path: path.to_path_buf(),
            bytes: size,
            max_bytes: MAX_FILE_SIZE,
        });
    }

    Ok(size)
}

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
/// - `PatternsError::FileNotFound` if file doesn't exist
/// - `PatternsError::FileTooLarge` if file exceeds MAX_FILE_SIZE
/// - `PatternsError::ParseError` if file is not valid UTF-8
/// - `PatternsError::Io` for other IO errors
pub fn read_file_safe(path: &Path) -> PatternsResult<String> {
    // Validate path and size
    let canonical = validate_file_path(path)?;

    let metadata = fs::metadata(&canonical)?;
    let size = metadata.len();

    if size > MAX_FILE_SIZE {
        return Err(PatternsError::FileTooLarge {
            path: path.to_path_buf(),
            bytes: size,
            max_bytes: MAX_FILE_SIZE,
        });
    }

    // Read the file
    let content = fs::read(&canonical)?;

    // Validate UTF-8
    String::from_utf8(content).map_err(|_| PatternsError::ParseError {
        file: path.to_path_buf(),
        message: "file is not valid UTF-8".to_string(),
    })
}

// =============================================================================
// Depth Checking (TIGER-03)
// =============================================================================

/// Check if AST depth limit has been exceeded.
///
/// Uses checked comparison to avoid any overflow issues.
///
/// # Arguments
///
/// * `current_depth` - The current traversal depth
///
/// # Returns
///
/// `Ok(())` if within limits, error otherwise.
///
/// # Errors
///
/// - `PatternsError::DepthLimitExceeded` if depth >= MAX_AST_DEPTH
pub fn check_ast_depth(current_depth: usize) -> PatternsResult<()> {
    if current_depth >= MAX_AST_DEPTH {
        Err(PatternsError::DepthLimitExceeded {
            depth: current_depth.min(u32::MAX as usize) as u32,
            max_depth: MAX_AST_DEPTH as u32,
        })
    } else {
        Ok(())
    }
}

/// Check if analysis depth limit has been exceeded.
///
/// Uses saturating arithmetic to prevent overflow.
///
/// # Arguments
///
/// * `current_depth` - The current analysis depth
///
/// # Returns
///
/// `Ok(())` if within limits, error otherwise.
///
/// # Errors
///
/// - `PatternsError::DepthLimitExceeded` if depth >= MAX_ANALYSIS_DEPTH
pub fn check_analysis_depth(current_depth: usize) -> PatternsResult<()> {
    if current_depth >= MAX_ANALYSIS_DEPTH {
        Err(PatternsError::DepthLimitExceeded {
            depth: current_depth.min(u32::MAX as usize) as u32,
            max_depth: MAX_ANALYSIS_DEPTH as u32,
        })
    } else {
        Ok(())
    }
}

/// Check if directory file count limit has been exceeded.
///
/// # Arguments
///
/// * `count` - The current file count
///
/// # Returns
///
/// `Ok(())` if within limits, error otherwise.
///
/// # Errors
///
/// - `PatternsError::TooManyFiles` if count > MAX_DIRECTORY_FILES
pub fn check_directory_file_count(count: usize) -> PatternsResult<()> {
    if count > MAX_DIRECTORY_FILES as usize {
        Err(PatternsError::TooManyFiles {
            count: count.min(u32::MAX as usize) as u32,
            max_files: MAX_DIRECTORY_FILES,
        })
    } else {
        Ok(())
    }
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
/// `Ok(())` if valid, error otherwise.
///
/// # Errors
///
/// - `PatternsError::InvalidParameter` for invalid names
pub fn validate_function_name(name: &str) -> PatternsResult<()> {
    // Check empty
    if name.is_empty() {
        return Err(PatternsError::InvalidParameter {
            message: "function name cannot be empty".to_string(),
        });
    }

    // Check length
    if name.len() > MAX_FUNCTION_NAME_LEN {
        return Err(PatternsError::InvalidParameter {
            message: format!(
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
            return Err(PatternsError::InvalidParameter {
                message: format!("function name contains invalid character: '{}'", c),
            });
        }
    }

    // First character should be letter or underscore (standard identifier rules)
    if let Some(first) = name.chars().next() {
        if !first.is_alphabetic() && first != '_' {
            return Err(PatternsError::InvalidParameter {
                message: "function name must start with letter or underscore".to_string(),
            });
        }
    }

    Ok(())
}

// =============================================================================
// Checked Arithmetic Utilities (TIGER-03)
// =============================================================================

/// Safely increment a depth counter with overflow protection.
///
/// Returns the incremented value or saturates at usize::MAX.
///
/// # Arguments
///
/// * `depth` - The current depth value
///
/// # Returns
///
/// The incremented depth (or usize::MAX if overflow would occur)
#[inline]
pub fn saturating_depth_increment(depth: usize) -> usize {
    depth.saturating_add(1)
}

/// Safely add to a counter with overflow protection.
///
/// Returns the sum or saturates at the type maximum.
///
/// # Arguments
///
/// * `count` - The current count
/// * `add` - The amount to add
///
/// # Returns
///
/// The sum (or type max if overflow would occur)
#[inline]
pub fn saturating_count_add(count: u32, add: u32) -> u32 {
    count.saturating_add(add)
}

/// Check if a value is within a limit using checked arithmetic.
///
/// # Arguments
///
/// * `value` - The value to check
/// * `limit` - The maximum allowed value
///
/// # Returns
///
/// `true` if value < limit
#[inline]
pub fn within_limit(value: usize, limit: usize) -> bool {
    value < limit
}

// =============================================================================
// Warning Utilities
// =============================================================================

/// Check if a file size is large enough to warrant a warning.
///
/// # Arguments
///
/// * `size` - The file size in bytes
///
/// # Returns
///
/// `true` if size > WARN_FILE_SIZE
#[inline]
pub fn should_warn_file_size(size: u64) -> bool {
    size > WARN_FILE_SIZE
}

/// Format a warning message for a large file.
///
/// # Arguments
///
/// * `path` - The file path
/// * `size` - The file size in bytes
///
/// # Returns
///
/// A formatted warning string
pub fn format_large_file_warning(path: &Path, size: u64) -> String {
    format!(
        "Warning: {} is large ({:.1} MB), analysis may be slow",
        path.display(),
        size as f64 / 1024.0 / 1024.0
    )
}

// =============================================================================
// Near-Limit Warning Utilities
// =============================================================================

/// Check if a count is approaching a limit (>80%).
///
/// # Arguments
///
/// * `count` - The current count
/// * `limit` - The maximum limit
///
/// # Returns
///
/// `true` if count > 80% of limit
#[inline]
pub fn approaching_limit(count: usize, limit: usize) -> bool {
    // Use checked arithmetic to avoid overflow
    let threshold = limit.saturating_mul(80) / 100;
    count > threshold
}

/// Log a warning if approaching a limit.
///
/// # Arguments
///
/// * `count` - The current count
/// * `limit` - The maximum limit
/// * `resource_name` - Name of the resource for the warning message
pub fn warn_if_approaching_limit(count: usize, limit: usize, resource_name: &str) {
    if approaching_limit(count, limit) {
        eprintln!(
            "Warning: {} count ({}) approaching limit ({})",
            resource_name, count, limit
        );
    }
}

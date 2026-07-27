//! Path validation helpers for CLI commands.
//!
//! cli-error-clarity-v2 (P2.BUG-4): commands that operate on a project
//! directory (hubs, impact, whatbreaks, change-impact, …) historically
//! produced confusing errors when given a regular file:
//!
//! - `tldr hubs <file>` → `Error: Path not found: <file>` (false: it exists)
//! - `tldr change-impact <file>` → `Git: Not a directory (os error 20)`
//!   (cryptic; the user has no idea what to do)
//!
//! These helpers normalise the validation so every directory-taking command
//! returns the same clear, actionable error mentioning the file path and
//! suggesting the project root.
//!
//! All helpers return `anyhow::Error` so they can be used directly with the
//! `?` operator inside `run()` methods that already return
//! `anyhow::Result<()>`.

use std::path::Path;

use anyhow::{bail, Result};

/// Validate that `path` exists and is a directory, producing clear error
/// messages on failure.
///
/// `command` is the CLI subcommand name (e.g. `"hubs"`) used to make the
/// error message specific.
pub fn require_directory(path: &Path, command: &str) -> Result<()> {
    if !path.exists() {
        bail!("Path not found: {}", path.display());
    }
    if path.is_file() {
        bail!(
            "{} requires a directory; got file '{}'. Pass the project root \
             or omit the argument to use the current directory.",
            command,
            path.display()
        );
    }
    if !path.is_dir() {
        bail!(
            "{} requires a directory; got non-directory path '{}'. Pass the \
             project root or omit the argument to use the current directory.",
            command,
            path.display()
        );
    }
    Ok(())
}

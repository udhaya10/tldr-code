//! Git baseline extraction for bugbot
//!
//! Retrieves the "before" version of changed files from git so the analysis
//! pipeline can compare baseline vs current and detect regressions.

use std::io::Write;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use tempfile::NamedTempFile;

/// Result of checking baseline status for a file.
#[derive(Debug)]
pub enum BaselineStatus {
    /// File exists at the baseline ref; contains the original content.
    Exists(String),
    /// File is new -- it did not exist at the baseline ref.
    NewFile,
    /// `git show` failed for an unexpected reason (stderr captured).
    GitShowFailed(String),
}

/// Get the content of a file at the given git ref.
///
/// # Arguments
/// * `project` - Project root directory (must be inside a git repo).
/// * `file`    - Path to the file (absolute or relative to `project`).
/// * `base_ref`- Git ref to read from, e.g. `"HEAD"`, `"main"`.
///
/// # Returns
/// * `BaselineStatus::Exists(content)` when the file existed at `base_ref`.
/// * `BaselineStatus::NewFile` when `git show` reports the path does not exist.
/// * `BaselineStatus::GitShowFailed(stderr)` on other git failures.
pub fn get_baseline_content(project: &Path, file: &Path, base_ref: &str) -> Result<BaselineStatus> {
    // Compute relative path from project root.
    // If the file is already relative (or outside the project) we fall through.
    let relative = file.strip_prefix(project).unwrap_or(file);

    // On all platforms git expects forward-slash separators in `ref:path`.
    let relative_str = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/");

    let output = Command::new("git")
        .args(["show", &format!("{}:{}", base_ref, relative_str)])
        .current_dir(project)
        .output()
        .context("Failed to run git show")?;

    if output.status.success() {
        let content =
            String::from_utf8(output.stdout).context("git show output is not valid UTF-8")?;
        Ok(BaselineStatus::Exists(content))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("does not exist")
            || stderr.contains("not exist in")
            || stderr.contains("exists on disk, but not in")
            || stderr.contains("did not match any")
        {
            Ok(BaselineStatus::NewFile)
        } else {
            Ok(BaselineStatus::GitShowFailed(stderr.to_string()))
        }
    }
}

/// Write baseline content to a temporary file with the correct extension.
///
/// The extension is preserved so that tree-sitter can detect the language
/// when parsing the temporary file.  The caller must keep the returned
/// `NamedTempFile` handle alive -- dropping it deletes the file.
pub fn write_baseline_tmpfile(content: &str, file_path: &Path) -> Result<NamedTempFile> {
    let extension = file_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("txt");

    let mut tmpfile = tempfile::Builder::new()
        .prefix("bugbot_baseline_")
        .suffix(&format!(".{}", extension))
        .tempfile()
        .context("Failed to create temp file for baseline")?;

    tmpfile
        .write_all(content.as_bytes())
        .context("Failed to write baseline content to temp file")?;
    tmpfile.flush()?;

    Ok(tmpfile)
}

//! Git change detection for bugbot
//!
//! Detects files changed via git, filtered to the target language.
//! Uses direct `git` commands to list changed files -- no call graph needed.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use tldr_core::Language;

/// Result of detecting changed files in the project.
#[derive(Debug, Clone)]
pub struct ChangeDetectionResult {
    /// Files that changed and match the target language.
    pub changed_files: Vec<PathBuf>,
    /// How changes were detected (e.g. "git:staged", "git:uncommitted").
    pub detection_method: String,
}

/// Run a git command in `project` and return the listed file paths.
///
/// Each non-empty line of stdout is joined with `project` to form an absolute path.
fn git_changed_files(project: &Path, args: &[&str]) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .context("Failed to run git")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git command failed: {}", stderr);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| project.join(l))
        .collect())
}

/// Detect changed files in `project`, filtered to the given `language`.
///
/// # Arguments
/// * `project` - Project root directory (must be inside a git repo)
/// * `base_ref` - Git base reference (e.g. "HEAD", "main", "origin/main")
/// * `staged` - If true, only consider staged changes; otherwise all uncommitted
/// * `language` - Only return files matching this language's extensions
///
/// # Detection Method
/// - `staged == true`  => `"git:staged"`
/// - `staged == false` and `base_ref == "HEAD"` => `"git:uncommitted"`
/// - `staged == false` and `base_ref != "HEAD"` => `"git:{base_ref}...HEAD"`
///
/// # Returns
/// A `ChangeDetectionResult` with the filtered file list and the detection method string.
pub fn detect_changes(
    project: &Path,
    base_ref: &str,
    staged: bool,
    language: &Language,
) -> Result<ChangeDetectionResult> {
    let (raw_files, detection_method) = if staged {
        let files = git_changed_files(project, &["diff", "--name-only", "--staged"])
            .context("Failed to list staged changes")?;
        (files, "git:staged".to_string())
    } else if base_ref == "HEAD" {
        // Uncommitted = modified tracked + staged + untracked
        let mut files = git_changed_files(project, &["diff", "--name-only", "HEAD"])
            .context("Failed to list uncommitted changes")?;
        let staged_files = git_changed_files(project, &["diff", "--name-only", "--staged"])
            .context("Failed to list staged changes")?;
        let untracked = git_changed_files(project, &["ls-files", "--others", "--exclude-standard"])
            .context("Failed to list untracked files")?;
        files.extend(staged_files);
        files.extend(untracked);
        files.sort();
        files.dedup();
        (files, "git:uncommitted".to_string())
    } else {
        let range = format!("{}...HEAD", base_ref);
        let files = git_changed_files(project, &["diff", "--name-only", &range])
            .context("Failed to list base-ref changes")?;
        (files, format!("git:{}...HEAD", base_ref))
    };

    // Filter files to only those matching the target language's extensions.
    let valid_extensions = language.extensions();
    let changed_files: Vec<PathBuf> = raw_files
        .into_iter()
        .filter(|f| {
            f.extension()
                .and_then(|e| e.to_str())
                .map(|ext| {
                    let dotted = format!(".{}", ext);
                    valid_extensions.contains(&dotted.as_str())
                })
                .unwrap_or(false)
        })
        .collect();

    // Filter out paths matching .tldrignore patterns (e.g. corpus/, vendor/).
    let changed_files = tldr_core::callgraph::filter_tldrignored(project, changed_files);

    Ok(ChangeDetectionResult {
        changed_files,
        detection_method,
    })
}

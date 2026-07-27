//! Project root resolution and `.tldr/` seed files.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Default project config written on first `tldr init` when missing.
pub const DEFAULT_CONFIG_JSON: &str = r#"{
  "version": 1,
  "embedding": {
    "provider": "local"
  },
  "semantic": {
    "enabled": true
  }
}
"#;

/// Resolve `--project` / cwd to a canonical absolute directory.
pub fn resolve_project_root(project: &Path) -> Result<PathBuf> {
    let candidate = if project.is_absolute() {
        project.to_path_buf()
    } else {
        std::env::current_dir()
            .context("cannot read current directory")?
            .join(project)
    };
    if !candidate.exists() {
        bail!(
            "project path does not exist: {} — create the directory first",
            candidate.display()
        );
    }
    if !candidate.is_dir() {
        bail!("project path is not a directory: {}", candidate.display());
    }
    candidate.canonicalize().with_context(|| {
        format!(
            "cannot resolve project root {} — refusing so the daemon key stays unique",
            candidate.display()
        )
    })
}

/// Ensure `.tldr/`, default config, and empty `.tldrignore` exist.
///
/// Returns whether config was newly written.
pub fn ensure_project_files(project: &Path) -> Result<ProjectFilesReport> {
    let tldr_dir = project.join(".tldr");
    fs::create_dir_all(&tldr_dir)
        .with_context(|| format!("failed to create {}", tldr_dir.display()))?;

    let config_path = tldr_dir.join("config.json");
    let config_created = if !config_path.exists() {
        fs::write(&config_path, DEFAULT_CONFIG_JSON)
            .with_context(|| format!("failed to write {}", config_path.display()))?;
        true
    } else {
        false
    };

    let ignore_path = project.join(".tldrignore");
    let ignore_created = if !ignore_path.exists() {
        fs::write(&ignore_path, "# tldr ignore patterns (gitignore syntax)\n")
            .with_context(|| format!("failed to write {}", ignore_path.display()))?;
        true
    } else {
        false
    };

    Ok(ProjectFilesReport {
        config_created,
        ignore_created,
        tldr_dir,
    })
}

#[derive(Debug, Clone)]
pub struct ProjectFilesReport {
    pub config_created: bool,
    pub ignore_created: bool,
    pub tldr_dir: PathBuf,
}

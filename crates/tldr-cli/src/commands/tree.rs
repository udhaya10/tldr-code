//! Tree command - Show file tree
//!
//! Displays the file tree structure of a directory.
//! Auto-routes through daemon when available for ~35x speedup.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use tldr_core::types::FileTree;
use tldr_core::{get_file_tree, IgnoreSpec};

use crate::commands::daemon_router::{is_oneshot, route_for_path};
use crate::output::{format_file_tree_text, OutputFormat, OutputWriter};

/// Show file tree structure
#[derive(Debug, Args)]
pub struct TreeArgs {
    /// Directory to scan (default: current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Filter by file extensions (e.g., --ext .py --ext .rs)
    #[arg(long = "ext", short = 'e')]
    pub extensions: Vec<String>,

    /// Include hidden files and directories
    #[arg(long, short = 'H')]
    pub include_hidden: bool,
}

impl TreeArgs {
    /// Run the tree command
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let writer = OutputWriter::new(format, quiet);

        // Normalize extension filters once (leading dot), shared by both paths
        // so the daemon and --oneshot results are byte-identical.
        let ext_vec: Vec<String> = self
            .extensions
            .iter()
            .map(|s| {
                if s.starts_with('.') {
                    s.clone()
                } else {
                    format!(".{}", s)
                }
            })
            .collect();

        // ADR-10 (TLDR-7pp.1.5): daemon is the only serve path; --oneshot is the
        // sole explicit local-compute escape. No silent fallback.
        let tree: FileTree = if is_oneshot() {
            self.compute_local(&ext_vec)?
        } else {
            let params = serde_json::json!({
                "path": self.path,
                "extensions": ext_vec,
                "include_hidden": self.include_hidden,
            });
            route_for_path::<FileTree>(&self.path, "tree", params).into_hit_or_bail("tree")?
        };

        // Single renderer for both paths.
        if writer.is_text() {
            writer.write_text(&format_file_tree_text(&tree, 0))?;
        } else {
            writer.write(&tree)?;
        }
        Ok(())
    }

    /// Local in-process file tree — reached only via `--oneshot`.
    fn compute_local(&self, ext_vec: &[String]) -> Result<FileTree> {
        let extensions: Option<HashSet<String>> = if ext_vec.is_empty() {
            None
        } else {
            Some(ext_vec.iter().cloned().collect())
        };
        Ok(get_file_tree(
            &self.path,
            extensions.as_ref(),
            !self.include_hidden,
            Some(&IgnoreSpec::default()),
        )?)
    }
}

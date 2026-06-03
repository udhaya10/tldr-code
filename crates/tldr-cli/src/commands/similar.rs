//! Similar command - Find similar code fragments
//!
//! PARKED (TLDR-7xz.4): `similar` is seeded similarity — it embeds the source
//! file's own chunks and finds nearest neighbors, then aggregates by file.
//! The warm daemon only exposes a text-query path today, so this command
//! cannot reuse it; the old implementation cold-built a `SemanticIndex` on
//! every invocation (the silent slow path TLDR-7xz removes). It returns at
//! full warm quality with a daemon seeded-similarity API (TLDR-utj).
//!
//! The argument surface is kept intact and fails fast with the standardized
//! message — parked, not silently removed.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::output::OutputFormat;

/// Find similar code fragments
#[derive(Debug, Args)]
pub struct SimilarArgs {
    /// Source file to find similar code for
    pub file: PathBuf,

    /// Specific function name (optional, searches whole file if not specified)
    #[arg(short = 'F', long)]
    pub function: Option<String>,

    /// Maximum number of results
    #[arg(short = 'n', long, default_value = "5")]
    pub top: usize,

    /// Minimum similarity threshold
    #[arg(short = 't', long, default_value = "0.7")]
    pub threshold: f64,

    /// Path to search for similar code (default: current directory)
    #[arg(short, long, default_value = ".")]
    pub path: PathBuf,

    /// Embedding model: arctic-xs, arctic-s, arctic-m, arctic-m-long, arctic-l
    #[arg(short, long)]
    pub model: Option<String>,

    /// Include self in results (by default, the query is excluded)
    #[arg(long)]
    pub include_self: bool,

    /// Disable embedding cache
    #[arg(long)]
    pub no_cache: bool,

    /// M16 (med-cleanup-bundle-v1): emit one row per matching chunk
    /// (legacy behavior). The default — when no `--function` is given
    /// and the target is a whole file — aggregates chunk matches per
    /// destination file and ranks by total similarity.
    #[arg(long)]
    pub by_chunk: bool,
}

impl SimilarArgs {
    /// Run the similar command — parked in this version (TLDR-7xz.4).
    pub fn run(&self, _format: OutputFormat, _quiet: bool) -> Result<()> {
        anyhow::bail!(
            "not available in this version, seeded similarity needs a warm daemon API (it cold-built an index per call) — returning with the new engine"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TLDR-7xz.4: `similar` is parked — it must fail fast with the
    /// standardized message before touching the filesystem or any model.
    #[test]
    fn similar_is_parked_with_standardized_message() {
        let args = SimilarArgs {
            file: PathBuf::from("does-not-need-to-exist.rs"),
            function: None,
            top: 5,
            threshold: 0.7,
            path: PathBuf::from("."),
            model: None,
            include_self: false,
            no_cache: false,
            by_chunk: false,
        };
        let err = args
            .run(OutputFormat::Json, true)
            .expect_err("parked command must fail fast");
        assert!(
            err.to_string().starts_with("not available in this version,"),
            "expected standardized parked message, got: {err}"
        );
    }
}

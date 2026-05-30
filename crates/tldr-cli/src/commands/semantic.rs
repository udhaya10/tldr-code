//! Semantic command - Semantic code search
//!
//! Performs natural language search over code using dense embeddings.
//! Builds an in-memory index and returns semantically similar code chunks.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use tldr_core::config::{find_project_root, TldrConfig};
use tldr_core::semantic::{
    BuildOptions, CacheConfig, ChunkGranularity, EmbeddingModel, IndexSearchOptions, SemanticIndex,
};

use crate::output::{OutputFormat, OutputWriter};

/// Semantic code search using embeddings
#[derive(Debug, Args)]
pub struct SemanticArgs {
    /// Natural language query
    pub query: String,

    /// Path to search (default: current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Maximum number of results
    #[arg(short = 'n', long, default_value = "10")]
    pub top: usize,

    /// Minimum similarity threshold (0.0 to 1.0)
    #[arg(short = 't', long, default_value = "0.5")]
    pub threshold: f64,

    /// Embedding model: arctic-xs, arctic-s, arctic-m, arctic-m-long, arctic-l
    #[arg(short, long)]
    pub model: Option<String>,

    /// Filter by language via file extensions (comma-separated, e.g., `--langs rs,py`).
    ///
    /// Values are parsed by `Language::from_extension`, which accepts file
    /// extensions such as `rs`, `py`, `ts`, `go`, `java`, `rb`, `kt`, `cpp`.
    /// Language names (`rust`, `python`) are NOT accepted here; use the
    /// global `--lang <LANG>` flag above for name-based single-language
    /// selection. Passing an unknown extension silently drops that entry
    /// from the filter.
    ///
    /// Renamed from `--lang` (pre-VAL-009) to avoid a clap TypeId collision
    /// with the global `--lang` arg which is `Option<Language>`.
    #[arg(long = "langs", value_delimiter = ',')]
    pub langs: Option<Vec<String>>,

    /// Disable embedding cache
    #[arg(long)]
    pub no_cache: bool,
}

impl SemanticArgs {
    /// Run the semantic search command
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let writer = OutputWriter::new(format, quiet);

        // Resolve model: CLI flag > config > built-in default
        let project_root = find_project_root(&self.path);
        let config = TldrConfig::resolve(project_root.as_deref());
        let model = EmbeddingModel::resolve(self.model.as_deref(), &config)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        let model_name = self.model.as_deref().unwrap_or("arctic-m");
        writer.progress(&format!(
            "Building semantic index for {} ({} model)...",
            self.path.display(),
            model_name
        ));

        // Build options
        let build_opts = BuildOptions {
            model,
            granularity: ChunkGranularity::Function,
            languages: self.langs.clone(),
            show_progress: !quiet,
            use_cache: !self.no_cache,
        };

        // Cache config
        let cache_config = if self.no_cache {
            None
        } else {
            Some(CacheConfig::default())
        };

        // Build index
        let mut index = SemanticIndex::build(&self.path, build_opts, cache_config)?;

        writer.progress(&format!(
            "Searching {} chunks for '{}'...",
            index.len(),
            self.query
        ));

        // Search options
        let search_opts = IndexSearchOptions {
            top_k: self.top,
            threshold: self.threshold,
            include_snippet: true,
            snippet_lines: 5,
        };

        // Perform search
        let report = index.search(&self.query, &search_opts)?;

        // Output based on format
        if writer.is_text() {
            let text = format_semantic_text(&report);
            writer.write_text(&text)?;
        } else {
            writer.write(&report)?;
        }

        Ok(())
    }
}


/// Format semantic search report for text output
fn format_semantic_text(report: &tldr_core::semantic::SemanticSearchReport) -> String {
    use colored::Colorize;

    let mut output = String::new();

    output.push_str(&format!(
        "{}: \"{}\"\n",
        "Semantic search".bold(),
        report.query.cyan()
    ));
    output.push_str(&format!(
        "Model: {} | Threshold: {:.2} | Searched: {} chunks\n\n",
        format!("{:?}", report.model).yellow(),
        0.5, // threshold from options
        report.total_chunks
    ));

    if report.results.is_empty() {
        output.push_str("No matches found above threshold.\n");
    } else {
        output.push_str(&format!(
            "{} ({} matches):\n\n",
            "Results".bold(),
            report.matches_above_threshold
        ));

        for (i, result) in report.results.iter().enumerate() {
            let func_name = result.function_name.as_deref().unwrap_or("<file>");
            let class_prefix = result
                .class_name
                .as_ref()
                .map(|c| format!("{}::", c))
                .unwrap_or_default();

            output.push_str(&format!(
                "{}. {}:{}{} (score: {:.2})\n",
                i + 1,
                result.file_path.display().to_string().green(),
                class_prefix,
                func_name.blue(),
                result.score
            ));
            output.push_str(&format!(
                "   Lines {}-{}\n",
                result.line_start, result.line_end
            ));

            if !result.snippet.is_empty() {
                output.push_str(&format!("   {}\n", result.snippet.dimmed()));
            }
            output.push('\n');
        }
    }

    output.push_str(&format!("Search completed in {}ms\n", report.latency_ms));

    output
}

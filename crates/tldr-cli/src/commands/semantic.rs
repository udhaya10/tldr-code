//! Semantic command - Semantic code search
//!
//! Performs natural language search over code using dense embeddings.
//! Builds an in-memory index and returns semantically similar code chunks.
//
// TLDR-AUDIT: This is the REAL, shipping semantic path — it drives the
//   in-process `SemanticIndex` (fastembed/Arctic). TLDR-cs5 deleted the dead
//   HTTP stub; TLDR-4er wired RRF fusion — `--hybrid` here fuses this dense
//   index with BM25 via `hybrid_search_with_index`.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use tldr_core::config::{find_project_root, TldrConfig};
use tldr_core::semantic::{
    BuildOptions, CacheConfig, ChunkGranularity, EmbeddingModel, IndexSearchOptions, SemanticIndex,
};
use tldr_core::{hybrid_search_with_index, HybridSearchReport, Language};

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

    /// Minimum similarity threshold (0.0 to 1.0). Default 0.0 = return the
    /// top-N ranked results with no score cutoff. The Arctic query prefix
    /// (default-on) is asymmetric, which lowers absolute query/passage cosine
    /// scores, so a non-zero default would hide correct top-ranked matches
    /// (TLDR-h27). Use `--top` for the result count; set `-t` only to filter.
    #[arg(short = 't', long, default_value = "0.0")]
    pub threshold: f64,

    /// Fuse dense (embedding) search with BM25 keyword search via Reciprocal
    /// Rank Fusion (TLDR-4er). Recovers lexically-strong matches that pure dense
    /// retrieval misses. Results are file-level; `--threshold` does not apply.
    #[arg(long)]
    pub hybrid: bool,

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

        // TLDR-atc: when a warm daemon is running, route the query to its
        // resident `SemanticIndex` instead of paying the cold ~1.1GB cache load
        // + ONNX model reload here. The daemon resolves the same model from the
        // same config (no `model` key => config default), applies the same
        // threshold default (0.0, TLDR-h27), and returns the identical
        // `SemanticSearchReport`, so warm and cold output match.
        //
        // We skip routing for:
        //   * `--hybrid`   — the daemon has no hybrid path; keep that
        //                    best-quality result identical to cold.
        //   * `--no-cache` — the user asked to bypass the cache the daemon
        //                    relies on.
        //   * `--langs`    — the daemon holds an ALL-languages resident index;
        //                    routing a language-filtered query would silently
        //                    return every language. Fall back to cold, which
        //                    builds a langs-filtered index (parity).
        // Any miss (daemon absent, connection error, or build failure) returns
        // `None` and falls through to the cold path below.
        if !self.hybrid && !self.no_cache && self.langs.is_none() {
            use crate::commands::daemon_router::try_daemon_route;

            let mut params = serde_json::json!({
                "query": self.query,
                "top_k": self.top,
                "threshold": self.threshold,
            });
            if let Some(m) = &self.model {
                params["model"] = serde_json::Value::String(m.clone());
            }

            if let Some(report) = try_daemon_route::<tldr_core::semantic::SemanticSearchReport>(
                &self.path,
                "semantic",
                params,
            ) {
                if writer.is_text() {
                    writer.write_text(&format_semantic_text(&report, self.threshold))?;
                } else {
                    writer.write(&report)?;
                }
                return Ok(());
            }
        }

        writer.progress(&format!(
            "Building semantic index for {} ({:?} model)...",
            self.path.display(),
            model
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

        // TLDR-4er: hybrid mode fuses this dense index with BM25 via RRF.
        if self.hybrid {
            let language = Language::from_directory(&self.path).unwrap_or(Language::Python);
            let report =
                hybrid_search_with_index(&mut index, &self.query, &self.path, language, self.top)?;
            if writer.is_text() {
                writer.write_text(&format_hybrid_text(&report))?;
            } else {
                writer.write(&report)?;
            }
            return Ok(());
        }

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
            let text = format_semantic_text(&report, self.threshold);
            writer.write_text(&text)?;
        } else {
            writer.write(&report)?;
        }

        Ok(())
    }
}

/// Format a hybrid (BM25 + dense RRF) search report for text output.
fn format_hybrid_text(report: &HybridSearchReport) -> String {
    use colored::Colorize;

    let mut output = String::new();
    output.push_str(&format!(
        "{}: \"{}\"\n",
        "Hybrid search (BM25 + dense RRF)".bold(),
        report.query.cyan()
    ));
    if let Some(mode) = &report.fallback_mode {
        output.push_str(&format!("Mode: {} (no dense results)\n", mode.yellow()));
    }
    output.push_str(&format!(
        "Candidates: {} | BM25-only: {} | dense-only: {} | overlap: {}\n\n",
        report.total_candidates, report.bm25_only, report.dense_only, report.overlap
    ));

    if report.results.is_empty() {
        output.push_str("No matches found.\n");
        return output;
    }
    for (i, r) in report.results.iter().enumerate() {
        output.push_str(&format!(
            "{}. {} (rrf: {:.4})\n",
            i + 1,
            r.file_path.display().to_string().green(),
            r.rrf_score
        ));
        let ranks = match (r.bm25_rank, r.dense_rank) {
            (Some(b), Some(d)) => format!("   bm25 #{b}, dense #{d}"),
            (Some(b), None) => format!("   bm25 #{b}"),
            (None, Some(d)) => format!("   dense #{d}"),
            (None, None) => String::new(),
        };
        if !ranks.is_empty() {
            output.push_str(&ranks);
            output.push('\n');
        }
    }
    output
}


/// Format semantic search report for text output
fn format_semantic_text(
    report: &tldr_core::semantic::SemanticSearchReport,
    threshold: f64,
) -> String {
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
        threshold,
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

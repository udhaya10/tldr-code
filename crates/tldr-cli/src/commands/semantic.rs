//! Semantic command - Semantic code search
//!
//! Performs natural language search over code using dense embeddings, served
//! exclusively by the warm daemon's resident VectorStore (TLDR-7xz.1). The
//! command has exactly two modes: served warm at full quality, or an honest
//! one-line explanation of why it can't serve — there is NO silent cold
//! fallback (the old `search_with_store` fallthrough is gone).
//!
//! Parked surfaces (`--hybrid`, `--langs`, `--no-cache`) keep their flags but
//! fail fast with the standardized "not available in this version" message —
//! they return at full warm quality with the new engine (TLDR-utj).

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use tldr_core::config::{find_project_root, TldrConfig};
use tldr_core::semantic::EmbeddingModel;

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

    /// [parked] BM25 + dense fusion — not available in this version
    /// (returning with the new warm engine).
    #[arg(long)]
    pub hybrid: bool,

    /// Embedding model: arctic-xs, arctic-s, arctic-m, arctic-m-long, arctic-l
    #[arg(short, long)]
    pub model: Option<String>,

    /// [parked] Language filter — not available in this version
    /// (returning with the new warm engine).
    #[arg(long = "langs", value_delimiter = ',')]
    pub langs: Option<Vec<String>>,

    /// [parked] Bypass the embedding cache — not available in this version
    /// (cold uncached search was removed).
    #[arg(long)]
    pub no_cache: bool,
}

impl SemanticArgs {
    /// Run the semantic search command
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let writer = OutputWriter::new(format, quiet);

        // Parked surfaces (TLDR-7xz.3): keep the flags, fail fast with the
        // standardized message — never a silent cold serve, never a silently
        // removed argument. Checked FIRST so a parked flag never half-runs.
        if self.hybrid {
            anyhow::bail!(
                "not available in this version, hybrid (BM25 + dense) search is moving into the warm daemon engine"
            );
        }
        if self.langs.is_some() {
            anyhow::bail!(
                "not available in this version, language filtering needs a daemon-side filter (the resident index covers all languages)"
            );
        }
        if self.no_cache {
            anyhow::bail!(
                "not available in this version, uncached cold search was removed — semantic queries are served by the warm daemon"
            );
        }

        // Validate the model early (CLI flag > config > built-in default) so a
        // typo'd `--model` fails here instead of after a daemon round-trip.
        // The daemon re-resolves with the same precedence (daemon.rs
        // resolve_semantic_model), so warm and cold-config rank the same model.
        let project_root = find_project_root(&self.path);
        let config = TldrConfig::resolve(project_root.as_deref());
        EmbeddingModel::resolve(self.model.as_deref(), &config)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        // Require-warm routing (TLDR-7xz.1): the warm daemon is the ONLY serve
        // path. Each non-hit maps to honest guidance — the old behavior
        // (fall through to a cold store load + ONNX reload) is gone.
        use crate::commands::daemon_router::{route_semantic, SemanticRoute};

        let mut params = serde_json::json!({
            "query": self.query,
            "top_k": self.top,
            "threshold": self.threshold,
        });
        if let Some(m) = &self.model {
            params["model"] = serde_json::Value::String(m.clone());
        }

        match route_semantic::<tldr_core::semantic::SemanticSearchReport>(&self.path, params) {
            SemanticRoute::Hit(report) => {
                if writer.is_text() {
                    writer.write_text(&format_semantic_text(&report, self.threshold))?;
                } else {
                    writer.write(&report)?;
                }
                Ok(())
            }
            SemanticRoute::DaemonDown => {
                anyhow::bail!("daemon not started — run tldr daemon start")
            }
            SemanticRoute::NotReady(msg) => anyhow::bail!("{msg}"),
            SemanticRoute::Error(e) => anyhow::bail!("semantic search failed: {e}"),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(hybrid: bool, langs: Option<Vec<String>>, no_cache: bool) -> SemanticArgs {
        SemanticArgs {
            query: "anything".to_string(),
            path: PathBuf::from("."),
            top: 10,
            threshold: 0.0,
            hybrid,
            model: None,
            langs,
            no_cache,
        }
    }

    /// TLDR-7xz.3: every parked flag fails fast with the standardized
    /// "not available in this version, <reason>" message — checked before any
    /// model resolution or daemon round-trip, so these tests are hermetic.
    #[test]
    fn parked_flags_fail_fast_with_standardized_message() {
        let cases = [
            args(true, None, false),
            args(false, Some(vec!["rs".into()]), false),
            args(false, None, true),
        ];
        for a in cases {
            let err = a
                .run(crate::output::OutputFormat::Json, true)
                .expect_err("parked flag must fail fast");
            assert!(
                err.to_string().starts_with("not available in this version,"),
                "expected standardized parked message, got: {err}"
            );
        }
    }
}

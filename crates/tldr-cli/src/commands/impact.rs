//! Impact command - Show impact analysis
//!
//! Finds all callers of a function (reverse call graph traversal).
//! Supports `--type-aware` flag for Python type resolution (Phase 7-8).
//!
//! The default path is served from the daemon's generation-pinned resident
//! CSR. `--oneshot` retains the source-compute escape for CI and diagnostics.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use tldr_core::{build_project_call_graph, Language};

use crate::commands::daemon_router::{is_oneshot, route_for_path};
use crate::output::{
    format_impact_compact, format_impact_dot, format_impact_text, OutputFormat, OutputWriter,
};
use crate::path_validation::require_directory;

/// Analyze impact of changing a function
#[derive(Debug, Args)]
pub struct ImpactArgs {
    /// Function name to analyze
    pub function: String,

    /// Project root directory (default: current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Programming language
    #[arg(long, short = 'l')]
    pub lang: Option<Language>,

    /// Maximum traversal depth
    #[arg(long, short = 'd', default_value = "5")]
    pub depth: usize,

    /// Filter by file path
    #[arg(long)]
    pub file: Option<PathBuf>,

    /// Enable type-aware method resolution (resolves self.method() to ClassName.method)
    #[arg(long)]
    pub type_aware: bool,
}

impl ImpactArgs {
    /// Run the impact command
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let writer = OutputWriter::new(format, quiet);

        // Validate path exists AND is a directory BEFORE language detection
        // / progress banner (lang-detect-default-v1).
        // cli-error-clarity-v2 (P2.BUG-4): reject files with a clear message
        // instead of saying "Path not found" or letting downstream surface
        // cryptic IO errors.
        require_directory(&self.path, "impact")?;

        // Determine language (auto-detect from directory, default to Python)
        let language = self
            .lang
            .unwrap_or_else(|| Language::from_directory(&self.path).unwrap_or(Language::Python));

        let type_aware_msg = if self.type_aware { " (type-aware)" } else { "" };

        let mut report = if is_oneshot() {
            self.compute_local(language, &writer, type_aware_msg)?
        } else {
            let params = serde_json::json!({
                "func": self.function,
                "depth": self.depth,
                "file": self.file,
                "language": language,
            });
            route_for_path(&self.path, "impact", params).into_hit_or_bail("impact")?
        };

        // If type-aware was requested, add placeholder stats to indicate it's enabled
        // (actual type resolution is integrated in callgraph builder - Phase 8 full implementation)
        if self.type_aware {
            report.type_resolution = Some(tldr_core::types::TypeResolutionStats {
                enabled: true,
                resolved_high_confidence: 0,
                resolved_medium_confidence: 0,
                fallback_used: 0,
                total_call_sites: 0,
            });
        }

        // Output based on format
        if writer.is_compact() {
            writer.write_text(format_impact_compact(&report).trim_end())?;
        } else if writer.is_text() {
            let text = format_impact_text(&report, self.type_aware);
            writer.write_text(&text)?;
        } else if writer.is_dot() {
            let dot = format_impact_dot(&report);
            writer.write_text(&dot)?;
        } else {
            writer.write(&report)?;
        }

        Ok(())
    }

    fn compute_local(
        &self,
        language: Language,
        writer: &OutputWriter,
        type_aware_msg: &str,
    ) -> Result<tldr_core::ImpactReport> {
        writer.progress(&format!(
            "Building call graph for {} ({:?}){}...",
            self.path.display(),
            language,
            type_aware_msg
        ));

        // Build call graph first
        let graph = build_project_call_graph(&self.path, language, None, true)?;

        writer.progress(&format!(
            "Analyzing impact of {}{}...",
            self.function, type_aware_msg
        ));

        let resident =
            tldr_core::artifact_store::GraphSnapshot::from_project_call_graph(&graph, language);
        let file_filter = self
            .file
            .as_deref()
            .map(|path| path.to_string_lossy().replace('\\', "/"));
        Ok(resident.impact_report(&self.function, self.depth, file_filter.as_deref())?)
    }
}

//! Complexity command - Calculate function complexity metrics
//!
//! Returns ComplexityMetrics with cyclomatic, cognitive, max_nesting, and lines_of_code.
//! Auto-routes through daemon when available for ~35x speedup.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use tldr_core::types::ComplexityMetrics;
use tldr_core::{calculate_complexity, detect_or_parse_language, validate_file_path, Language};

use crate::commands::daemon_router::{is_oneshot, params_with_file_function_lang, route_for_path};
use crate::output::{format_complexity_text, OutputFormat, OutputWriter};

/// Calculate complexity metrics for a function
#[derive(Debug, Args)]
pub struct ComplexityArgs {
    /// file containing the function
    pub file: PathBuf,

    /// Function name to analyze
    pub function: String,

    /// Programming language (auto-detect if not specified)
    #[arg(long, short = 'l')]
    pub lang: Option<Language>,
}

impl ComplexityArgs {
    /// Run the complexity command
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let writer = OutputWriter::new(format, quiet);

        // Validate file path exists (M28: shared validator - returns PathNotFound error)
        let validated_path = validate_file_path(self.file.to_str().unwrap_or_default(), None)?;

        // ADR-10 (TLDR-7pp.1.3): the daemon is the only serve path. `--oneshot`
        // is the sole explicit local-compute escape; otherwise route and fail
        // loudly when the daemon is absent — never a silent local fallback.
        let result = if is_oneshot() {
            self.compute_local(&validated_path, &writer)?
        } else {
            // Registry-driven resolution: route to the running daemon that
            // actually watches this file (the repo-root daemon), not whatever
            // dir happens to hold a stale `.tldr`. No covering daemon =>
            // honest hard-fail, never a silent local fallback (ADR-10).
            let params = params_with_file_function_lang(
                &validated_path,
                &self.function,
                self.lang.as_ref().map(|l| l.as_str()),
            );
            route_for_path::<ComplexityMetrics>(&validated_path, "complexity", params)
                .into_hit_or_bail("complexity")?
        };

        // Output based on format (single renderer for both paths).
        if writer.is_text() {
            writer.write_text(&format_complexity_text(&result))?;
        } else {
            writer.write(&result)?;
        }

        Ok(())
    }

    /// Local in-process compute — reached only via `--oneshot`.
    fn compute_local(
        &self,
        validated_path: &std::path::Path,
        writer: &OutputWriter,
    ) -> Result<ComplexityMetrics> {
        // Detect or parse language (uses shared validator M28)
        let language =
            detect_or_parse_language(self.lang.as_ref().map(|l| l.as_str()), validated_path)?;

        writer.progress(&format!(
            "Calculating complexity for {} in {} ({:?})...",
            self.function,
            validated_path.display(),
            language
        ));

        Ok(calculate_complexity(
            validated_path.to_str().unwrap_or_default(),
            &self.function,
            language,
        )?)
    }
}

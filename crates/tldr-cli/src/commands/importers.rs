//! Importers command - Find all files that import a given module
//!
//! Returns an ImportersReport with module name, list of importing files, and total count.
//! Auto-routes through daemon when available for ~35x speedup.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use colored::Colorize;

use tldr_core::types::ImportersReport;
use tldr_core::{find_importers, Language};

use crate::commands::daemon_router::{is_oneshot, route_for_path};
use crate::output::{format_importers_text, OutputFormat, OutputWriter};

/// Find all files that import a given module
#[derive(Debug, Args)]
pub struct ImportersArgs {
    /// Module name to search for
    pub module: String,

    /// Directory to search (default: current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Programming language (auto-detected from directory if not specified)
    #[arg(long, short = 'l')]
    pub lang: Option<Language>,

    /// Maximum number of importing files to show (0 = unlimited)
    #[arg(long, short = 'm', default_value = "50")]
    pub limit: usize,
}

impl ImportersArgs {
    /// Run the importers command
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let writer = OutputWriter::new(format, quiet);

        // Validate path exists BEFORE language detection / progress banner
        // (lang-detect-default-v1)
        if !self.path.exists() {
            anyhow::bail!("Path not found: {}", self.path.display());
        }

        // Determine language (auto-detect from directory, default to Python).
        // Resolved once and sent on the wire so the daemon uses the same
        // language as compute_local (previously the daemon path dropped --lang
        // and defaulted to Python regardless of the directory contents).
        let language = self
            .lang
            .unwrap_or_else(|| Language::from_directory(&self.path).unwrap_or(Language::Python));

        // ADR-10 (TLDR-7pp.1.5): daemon is the only serve path; `--oneshot` is
        // the sole explicit local-compute escape. No silent fallback. The
        // `--limit` truncation is applied CLI-side (presentation concern) so
        // both paths stay byte-identical.
        let mut result = if is_oneshot() {
            self.compute_local(language, &writer)?
        } else {
            let params = serde_json::json!({
                "module": self.module,
                "path": self.path,
                "language": language,
            });
            route_for_path::<ImportersReport>(&self.path, "importers", params)
                .into_hit_or_bail("importers")?
        };

        self.apply_limit(&mut result);
        self.output_result(&writer, &result)?;

        Ok(())
    }

    /// Local in-process importer search — reached only via `--oneshot`.
    fn compute_local(&self, language: Language, writer: &OutputWriter) -> Result<ImportersReport> {
        writer.progress(&format!(
            "Finding files that import '{}' in {} ({:?})...",
            self.module,
            self.path.display(),
            language
        ));
        Ok(find_importers(&self.path, &self.module, language)?)
    }

    fn apply_limit(&self, report: &mut ImportersReport) {
        if self.limit > 0 && report.importers.len() > self.limit {
            report.importers.truncate(self.limit);
        }
    }

    fn output_result(&self, writer: &OutputWriter, report: &ImportersReport) -> Result<()> {
        if writer.is_text() {
            let shown = report.importers.len();
            let total = report.total;
            let truncated = shown < total;

            let header = if truncated {
                format!(
                    "{} imported by {} files (showing {})\n",
                    format!("\"{}\"", report.module).bold(),
                    total,
                    shown,
                )
            } else {
                format!(
                    "{} imported by {} {}\n",
                    format!("\"{}\"", report.module).bold(),
                    total,
                    if total == 1 { "file" } else { "files" },
                )
            };

            writer.write_text(&format!("{}\n{}", header, format_importers_text(report)))?;
        } else {
            writer.write(report)?;
        }
        Ok(())
    }
}

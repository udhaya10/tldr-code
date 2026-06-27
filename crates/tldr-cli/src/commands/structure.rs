//! Structure command - Show code structure
//!
//! Extracts and displays functions, classes, and imports from source files.
//! Auto-routes through daemon when available for ~35x speedup.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use tldr_core::types::CodeStructure;
use tldr_core::{get_code_structure, IgnoreSpec, Language};

use crate::commands::daemon_router::{is_oneshot, route_for_path};
use crate::output::{format_structure_text, OutputFormat, OutputWriter};

/// Extract code structure (functions, classes, imports)
#[derive(Debug, Args)]
pub struct StructureArgs {
    /// Directory to scan (default: current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Programming language (auto-detected if not specified)
    #[arg(long, short = 'l')]
    pub lang: Option<Language>,

    /// Maximum number of files to process (0 = unlimited)
    #[arg(long, short = 'm', default_value = "0")]
    pub max_results: usize,
}

impl StructureArgs {
    /// Run the structure command
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let writer = OutputWriter::new(format, quiet);

        // Validate path exists BEFORE language detection / progress banner
        // (lang-detect-default-v1: avoid printing misleading "(Python)" banner
        // when the path doesn't exist and from_directory silently returns None.)
        if !self.path.exists() {
            anyhow::bail!("Path not found: {}", self.path.display());
        }

        // Determine language (auto-detect from directory, default to Python).
        // Resolved once and shared by both paths so the daemon and `--oneshot`
        // results are byte-identical (the daemon receives the resolved string).
        let language = self
            .lang
            .unwrap_or_else(|| Language::from_directory(&self.path).unwrap_or(Language::Python));

        // ADR-10 (TLDR-7pp.1.5): daemon is the only serve path; `--oneshot` is
        // the sole explicit local-compute escape. No silent fallback.
        let structure: CodeStructure = if is_oneshot() {
            self.compute_local(language, &writer)?
        } else {
            // Full flag envelope on the wire: language (resolved) + max_results
            // so the daemon computes EXACTLY what compute_local computes,
            // including the default IgnoreSpec (previously the daemon path
            // dropped both the ignore spec and --max-results — a latent parity
            // break this conversion fixes).
            let params = serde_json::json!({
                "path": self.path,
                "language": language.as_str(),
                "max_results": self.max_results,
            });
            route_for_path::<CodeStructure>(&self.path, "structure", params)
                .into_hit_or_bail("structure")?
        };

        // Single renderer for both paths.
        if writer.is_text() {
            let text = format_structure_text(&structure);
            writer.write_text(&text)?;
        } else {
            writer.write(&structure)?;
        }

        Ok(())
    }

    /// Local in-process structure extraction — reached only via `--oneshot`.
    fn compute_local(&self, language: Language, writer: &OutputWriter) -> Result<CodeStructure> {
        writer.progress(&format!(
            "Extracting structure from {} ({:?})...",
            self.path.display(),
            language
        ));

        Ok(get_code_structure(
            &self.path,
            language,
            self.max_results,
            Some(&IgnoreSpec::default()),
        )?)
    }
}

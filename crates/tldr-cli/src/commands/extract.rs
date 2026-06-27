//! Extract command - Extract complete module info from a file
//!
//! Returns functions, classes, imports, and call graph for a single file.
//! Auto-routes through daemon when available for ~35x speedup.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use tldr_core::types::ModuleInfo;
use tldr_core::{extract_file_with_lang, Language};

use crate::commands::daemon_router::{is_oneshot, route_for_path};
use crate::output::{format_module_info_text, OutputFormat, OutputWriter};

/// Extract complete module info from a file
#[derive(Debug, Args)]
pub struct ExtractArgs {
    /// File to extract
    pub file: PathBuf,

    /// Programming language (auto-detected from file extension if not specified)
    #[arg(long, short = 'l')]
    pub lang: Option<Language>,
}

impl ExtractArgs {
    /// Run the extract command
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let writer = OutputWriter::new(format, quiet);

        // cross-command-consistency-v3 (P5.BUG-N1): resolve the language hint
        // BEFORE choosing a route. The user's explicit `--lang` wins over any
        // detection. When the user did not pass `--lang`, apply the
        // sibling-aware widening so `.h` files in C++ projects parse as C++
        // (otherwise the C grammar mis-classifies `class Foo` as a function
        // with `return_type: "class"` and emits zero classes). Resolved once
        // and shared by both paths so daemon and `--oneshot` are byte-identical.
        let resolved_lang: Option<Language> = match self.lang {
            Some(l) => Some(l),
            None => Language::from_path_with_siblings(&self.file),
        };

        // ADR-10 (TLDR-7pp.1.5): daemon is the only serve path; `--oneshot` is
        // the sole explicit local-compute escape. No silent fallback.
        let result: ModuleInfo = if is_oneshot() {
            self.compute_local(resolved_lang, &writer)?
        } else {
            // The resolved language hint travels on the wire so the daemon
            // honors `--lang`/sibling-widening identically (previously the
            // daemon path dropped the hint and used plain extract_file).
            let params = serde_json::json!({
                "file": self.file,
                "language": resolved_lang,
            });
            route_for_path::<ModuleInfo>(&self.file, "extract", params)
                .into_hit_or_bail("extract")?
        };

        // Single renderer for both paths.
        if writer.is_text() {
            writer.write_text(&format_module_info_text(&result))?;
        } else {
            writer.write(&result)?;
        }

        Ok(())
    }

    /// Local in-process extraction — reached only via `--oneshot`.
    fn compute_local(
        &self,
        resolved_lang: Option<Language>,
        writer: &OutputWriter,
    ) -> Result<ModuleInfo> {
        writer.progress(&format!(
            "Extracting module info from {}...",
            self.file.display()
        ));

        // Propagate the resolved language hint so the parser pool honors it
        // instead of falling back to extension-based detection (which breaks
        // `.h` for C++ and any extensionless file with `--lang`).
        Ok(extract_file_with_lang(&self.file, None, resolved_lang)?)
    }
}

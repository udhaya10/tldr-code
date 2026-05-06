//! Extract command - Extract complete module info from a file
//!
//! Returns functions, classes, imports, and call graph for a single file.
//! Auto-routes through daemon when available for ~35x speedup.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use tldr_core::types::ModuleInfo;
use tldr_core::{extract_file_with_lang, Language};

use crate::commands::daemon_router::{params_with_file_lang, try_daemon_route};
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
        // with `return_type: "class"` and emits zero classes).
        let resolved_lang: Option<Language> = match self.lang {
            Some(l) => Some(l),
            None => Language::from_path_with_siblings(&self.file),
        };

        // Try daemon first for cached result (use file's parent as project root)
        let project = self.file.parent().unwrap_or(&self.file);
        if let Some(result) = try_daemon_route::<ModuleInfo>(
            project,
            "extract",
            params_with_file_lang(&self.file, resolved_lang.as_ref().map(|l| l.as_str())),
        ) {
            if writer.is_text() {
                writer.write_text(&format_module_info_text(&result))?;
            } else {
                writer.write(&result)?;
            }
            return Ok(());
        }

        // Fallback to direct compute
        writer.progress(&format!(
            "Extracting module info from {}...",
            self.file.display()
        ));

        // Extract module info, propagating the resolved language hint so the
        // parser pool honors it instead of falling back to extension-based
        // detection (which breaks `.h` for C++ and any extensionless file
        // with `--lang`).
        let result = extract_file_with_lang(&self.file, None, resolved_lang)?;

        // Output based on format
        if writer.is_text() {
            writer.write_text(&format_module_info_text(&result))?;
        } else {
            writer.write(&result)?;
        }

        Ok(())
    }
}

//! Imports command - Parse import statements from a file
//!
//! Returns an array of ImportInfo objects with module, names, is_from, and alias.
//! Auto-routes through daemon when available for ~35x speedup.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use colored::Colorize;
use serde::{Deserialize, Serialize};

use tldr_core::types::ImportInfo;
use tldr_core::{detect_or_parse_language, get_imports, Language};

use crate::commands::daemon_router::{is_oneshot, route_for_path};
use crate::output::{format_imports_text, OutputFormat, OutputWriter};

/// Envelope shape for `tldr imports` JSON output (schema-unification-v1).
///
/// Wraps the previously bare `Vec<ImportInfo>` array in an object so the
/// command's JSON shape matches every other top-level command (`structure`,
/// `vuln`, `dead`, etc., all of which return objects). Closes BUG-18.
///
/// Use `--legacy-array` to emit the old bare-array shape for backward
/// compatibility with tools that hard-coded `jq '.[]'` over the top level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportsEnvelope {
    /// Path of the file that was parsed.
    pub file: String,
    /// Detected (or user-specified) language.
    pub language: String,
    /// Parsed import statements. Always present; empty array if none.
    pub imports: Vec<ImportInfo>,
}

/// Parse import statements from a file
#[derive(Debug, Args)]
pub struct ImportsArgs {
    /// File to parse
    pub file: PathBuf,

    /// Programming language (auto-detect if not specified)
    #[arg(long, short = 'l')]
    pub lang: Option<Language>,

    /// Emit the legacy bare-array JSON shape (`[ImportInfo, ...]`) instead of
    /// the canonical envelope object `{file, language, imports}`. Provided for
    /// backward compatibility with consumers that hard-coded `jq '.[]'` over
    /// the top level. New code should consume the envelope shape.
    #[arg(long = "legacy-array")]
    pub legacy_array: bool,
}

impl ImportsArgs {
    /// Run the imports command
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let writer = OutputWriter::new(format, quiet);

        // Resolve language CLI-side (shared validator M28) so both paths agree
        // on the envelope `language` field and the parse language — the daemon
        // receives the resolved hint and parses identically.
        let language =
            detect_or_parse_language(self.lang.as_ref().map(|l| l.as_str()), &self.file)?;

        // ADR-10 (TLDR-7pp.1.5): daemon is the only serve path; `--oneshot` is
        // the sole explicit local-compute escape. No silent fallback.
        let result: Vec<ImportInfo> = if is_oneshot() {
            self.compute_local(language, &writer)?
        } else {
            // The resolved language travels on the wire (previously the daemon
            // path dropped --lang entirely and re-detected from the extension).
            let params = serde_json::json!({
                "file": self.file,
                "language": language.as_str(),
            });
            route_for_path::<Vec<ImportInfo>>(&self.file, "imports", params)
                .into_hit_or_bail("imports")?
        };

        // Single renderer for both paths.
        if writer.is_text() {
            writer.write_text(&format!(
                "{} ({} imports)\n\n{}",
                self.file.display().to_string().bold(),
                result.len(),
                format_imports_text(&result),
            ))?;
        } else if self.legacy_array {
            writer.write(&result)?;
        } else {
            // schema-unification-v1 BUG-18: wrap in envelope so the top-level
            // JSON is an object (consistent with structure, vuln, dead, etc.)
            // instead of a bare array.
            let envelope = ImportsEnvelope {
                file: self.file.display().to_string(),
                language: language.as_str().to_string(),
                imports: result,
            };
            writer.write(&envelope)?;
        }

        Ok(())
    }

    /// Local in-process import parsing — reached only via `--oneshot`.
    fn compute_local(&self, language: Language, writer: &OutputWriter) -> Result<Vec<ImportInfo>> {
        writer.progress(&format!(
            "Parsing imports from {} ({:?})...",
            self.file.display(),
            language
        ));
        Ok(get_imports(&self.file, language)?)
    }
}

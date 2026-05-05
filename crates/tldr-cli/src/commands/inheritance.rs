//! Inheritance command - Extract and visualize class hierarchies
//!
//! Analyzes class inheritance relationships across a codebase:
//! - Python: class inheritance, ABC, Protocol, metaclasses
//! - TypeScript: class extends, implements, interfaces
//! - Go: struct embedding (modeled as inheritance)
//! - Rust: trait implementations
//!
//! # Output Formats
//!
//! - JSON: Full structured output (default)
//! - DOT: Graphviz format for visualization
//! - text: Human-readable tree format
//!
//! # Mitigations Addressed
//!
//! - A2: Diamond detection uses BFS + set intersection (O(|ancestors|))
//! - A12: Python metaclass extraction
//! - A14: Go struct embedding as Embeds relationships
//! - A16: Rust trait impl blocks
//! - A17: --depth requires --class validation
//! - A19: DOT output properly escapes special characters

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use tldr_core::inheritance::{extract_inheritance, format_dot, format_text, InheritanceOptions};
use tldr_core::Language;

use crate::output::{OutputFormat, OutputWriter};

/// Extract class inheritance hierarchies
#[derive(Debug, Args)]
pub struct InheritanceArgs {
    /// Path to file or directory to analyze (default: current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Programming language (auto-detect if not specified)
    #[arg(long, short = 'l')]
    pub lang: Option<Language>,

    /// Focus on specific class (shows ancestors + descendants)
    #[arg(long, short = 'c')]
    pub class: Option<String>,

    /// Limit traversal depth (requires --class)
    #[arg(long, short = 'd')]
    pub depth: Option<usize>,

    /// Skip ABC/Protocol/mixin/diamond detection
    #[arg(long)]
    pub no_patterns: bool,

    /// Skip external base resolution
    #[arg(long)]
    pub no_external: bool,

    /// Output format override (backwards compatibility, prefer global --format/-f)
    #[arg(long = "output", short = 'o', hide = true, value_parser = parse_inheritance_format)]
    pub output: Option<InheritanceFormat>,
}

/// Inheritance-specific output formats (includes DOT)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InheritanceFormat {
    Json,
    Text,
    Dot,
}

fn parse_inheritance_format(s: &str) -> Result<InheritanceFormat, String> {
    match s.to_lowercase().as_str() {
        "json" => Ok(InheritanceFormat::Json),
        "text" => Ok(InheritanceFormat::Text),
        "dot" | "graphviz" => Ok(InheritanceFormat::Dot),
        _ => Err(format!(
            "Invalid format '{}'. Expected: json, text, or dot",
            s
        )),
    }
}

impl InheritanceArgs {
    /// Run the inheritance command
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let writer = OutputWriter::new(format, quiet);

        writer.progress(&format!(
            "Analyzing inheritance in {}...",
            self.path.display()
        ));

        // Build options
        let options = InheritanceOptions {
            class_filter: self.class.clone(),
            depth: self.depth,
            no_patterns: self.no_patterns,
            no_external: self.no_external,
            ..Default::default()
        };

        // Run analysis
        let report = extract_inheritance(&self.path, self.lang, &options)?;

        // Determine output format
        // surface-gaps-v1 (BUG-19): honor the global `--format dot` flag in
        // addition to the legacy `-o dot` switch. Inheritance graphs are the
        // canonical class-hierarchy DOT use case.
        let inh_format = self.output.unwrap_or_else(|| {
            if writer.is_text() {
                InheritanceFormat::Text
            } else if writer.is_dot() {
                InheritanceFormat::Dot
            } else {
                InheritanceFormat::Json
            }
        });

        // Output based on format
        match inh_format {
            InheritanceFormat::Json => {
                writer.write(&report)?;
            }
            InheritanceFormat::Text => {
                let text = format_text(&report);
                writer.write_text(&text)?;
            }
            InheritanceFormat::Dot => {
                let dot = format_dot(&report);
                writer.write_text(&dot)?;
            }
        }

        // determinism-and-stderr-hygiene-v1 (BUG-18): the summary
        // ("Found N classes in Mms") and diamond-inheritance warning
        // were unconditionally written to stderr, which contaminated
        // the JSON-mode contract — `tldr inheritance <path> 2>/dev/null
        // > out.json` produced a clean JSON file but a non-empty stderr
        // stream, breaking shell pipelines that gate on stderr-empty.
        // Gate on text format: text consumers still see the summary
        // (now via a writer-aware path), JSON consumers get a clean
        // stream. The information is already in the JSON
        // (`report.count`, `report.scan_time_ms`, `report.diamonds`),
        // so no data loss for downstream tooling.
        if !quiet && writer.is_text() {
            eprintln!(
                "Found {} classes in {}ms",
                report.count, report.scan_time_ms
            );

            if !report.diamonds.is_empty() {
                eprintln!(
                    "Warning: {} diamond inheritance pattern(s) detected",
                    report.diamonds.len()
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_inheritance_format() {
        assert_eq!(
            parse_inheritance_format("json").unwrap(),
            InheritanceFormat::Json
        );
        assert_eq!(
            parse_inheritance_format("text").unwrap(),
            InheritanceFormat::Text
        );
        assert_eq!(
            parse_inheritance_format("dot").unwrap(),
            InheritanceFormat::Dot
        );
        assert_eq!(
            parse_inheritance_format("graphviz").unwrap(),
            InheritanceFormat::Dot
        );
        assert_eq!(
            parse_inheritance_format("DOT").unwrap(),
            InheritanceFormat::Dot
        );
        assert!(parse_inheritance_format("invalid").is_err());
    }
}

//! Taint analysis CLI command
//!
//! Provides CFG-based taint analysis to detect security vulnerabilities
//! such as SQL injection, command injection, and code injection.
//!
//! # Usage
//!
//! ```bash
//! tldr taint <file> <function> [-f json|text]
//! ```
//!
//! # Output
//!
//! - JSON: Full TaintInfo structure with sources, sinks, flows
//! - Text: Human-readable summary with vulnerability highlights
//!
//! # Reference
//! - session11-taint-spec.md

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use colored::Colorize;

use tldr_core::ast::ParserPool;
use tldr_core::{compute_taint_with_tree, get_cfg_context, get_dfg_context, Language, TaintInfo};

use crate::output::OutputFormat;

/// Analyze taint flows in a function to detect security vulnerabilities
#[derive(Debug, Args)]
pub struct TaintArgs {
    /// Source file to analyze
    pub file: PathBuf,

    /// Function name to analyze
    pub function: String,

    /// Programming language (auto-detected from file extension if not specified)
    #[arg(long, short = 'l')]
    pub lang: Option<Language>,

    /// Show verbose output with tainted variables per block
    #[arg(long, short = 'v')]
    pub verbose: bool,
}

impl TaintArgs {
    /// Run the taint analysis command
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        use crate::output::OutputWriter;

        let writer = OutputWriter::new(format, quiet);

        // Determine language from file extension or argument
        let language = self
            .lang
            .unwrap_or_else(|| Language::from_path(&self.file).unwrap_or(Language::Python));

        writer.progress(&format!(
            "Analyzing taint flows for {} in {}...",
            self.function,
            self.file.display()
        ));

        // Read source file - ensure it exists
        if !self.file.exists() {
            return Err(anyhow::anyhow!("File not found: {}", self.file.display()));
        }

        let source = std::fs::read_to_string(&self.file)?;

        // Get CFG for the function
        let cfg = get_cfg_context(
            self.file.to_str().unwrap_or_default(),
            &self.function,
            language,
        )?;

        // Get DFG for variable references
        let dfg = get_dfg_context(
            self.file.to_str().unwrap_or_default(),
            &self.function,
            language,
        )?;

        // Compute function line range from CFG blocks to scope statements
        // to only the target function (avoids leaking sources/sinks from
        // other functions in the same file).
        let (fn_start, fn_end) = if cfg.blocks.is_empty() {
            (1u32, source.lines().count() as u32)
        } else {
            let start = cfg.blocks.iter().map(|b| b.lines.0).min().unwrap_or(1);
            let end = cfg
                .blocks
                .iter()
                .map(|b| b.lines.1)
                .max()
                .unwrap_or(source.lines().count() as u32);
            (start, end)
        };

        // Build statements map scoped to function line range
        let statements: HashMap<u32, String> = source
            .lines()
            .enumerate()
            .filter(|(i, _)| {
                let line_num = (i + 1) as u32;
                line_num >= fn_start && line_num <= fn_end
            })
            .map(|(i, line)| ((i + 1) as u32, line.to_string()))
            .collect();

        // Parse source with tree-sitter for AST-based taint detection
        let pool = ParserPool::new();
        let tree = pool.parse(&source, language).ok();

        // M1b VAL-001b: build SSA when possible so taint propagation can use
        // SSA-versioned keys (sanitiser-reassignment correctly clears taint on
        // the post-sanitiser version). Per-language SSA-coverage gap is handled
        // gracefully via `.ok()` — falls back to M1a's VarRef path when SSA
        // construction returns Err or yields an empty function.
        let ssa = tldr_core::ssa::construct::construct_minimal_ssa(&cfg, &dfg).ok();

        // Run taint analysis (AST-based when tree available, regex fallback otherwise)
        let result = compute_taint_with_tree(
            &cfg,
            &dfg.refs,
            &statements,
            tree.as_ref(),
            Some(source.as_bytes()),
            language,
            ssa.as_ref(),
        )?;

        // Output based on format
        match format {
            OutputFormat::Text => {
                let text = format_taint_text(&result, self.verbose);
                writer.write_text(&text)?;
            }
            OutputFormat::Json | OutputFormat::Compact => {
                let json = serde_json::to_string_pretty(&result)
                    .map_err(|e| anyhow::anyhow!("JSON serialization failed: {}", e))?;
                writer.write_text(&json)?;
            }
            OutputFormat::Dot => {
                // DOT not supported for taint analysis, fall back to JSON
                let json = serde_json::to_string_pretty(&result)
                    .map_err(|e| anyhow::anyhow!("JSON serialization failed: {}", e))?;
                writer.write_text(&json)?;
            }
            OutputFormat::Sarif => {
                // SARIF not supported, fall back to JSON
                let json = serde_json::to_string_pretty(&result)
                    .map_err(|e| anyhow::anyhow!("JSON serialization failed: {}", e))?;
                writer.write_text(&json)?;
            }
        }

        Ok(())
    }
}

/// Format taint analysis results for human-readable text output
fn format_taint_text(result: &TaintInfo, verbose: bool) -> String {
    let mut output = String::new();

    // Header
    output.push_str(&format!(
        "{}\n",
        format!("Taint Analysis: {}", result.function_name)
            .bold()
            .cyan()
    ));
    output.push_str(&"=".repeat(50));
    output.push('\n');

    // Sources section
    output.push_str(&format!(
        "\n{} ({}):\n",
        "Sources".bold(),
        result.sources.len()
    ));
    if result.sources.is_empty() {
        output.push_str("  No taint sources detected.\n");
    } else {
        for source in &result.sources {
            output.push_str(&format!(
                "  Line {}: {} ({})\n",
                source.line.to_string().yellow(),
                source.var.green(),
                format!("{:?}", source.source_type).cyan()
            ));
            if let Some(ref stmt) = source.statement {
                output.push_str(&format!("    {}\n", stmt.trim().dimmed()));
            }
        }
    }

    // Sinks section
    output.push_str(&format!("\n{} ({}):\n", "Sinks".bold(), result.sinks.len()));
    if result.sinks.is_empty() {
        output.push_str("  No sinks detected.\n");
    } else {
        for sink in &result.sinks {
            let status = if sink.tainted {
                "TAINTED".red().bold().to_string()
            } else {
                "safe".green().to_string()
            };
            output.push_str(&format!(
                "  Line {}: {} ({}) - {}\n",
                sink.line.to_string().yellow(),
                sink.var.green(),
                format!("{:?}", sink.sink_type).cyan(),
                status
            ));
            if let Some(ref stmt) = sink.statement {
                output.push_str(&format!("    {}\n", stmt.trim().dimmed()));
            }
        }
    }

    // Vulnerabilities section (tainted sinks)
    let vulns: Vec<_> = result.sinks.iter().filter(|s| s.tainted).collect();
    output.push_str(&format!(
        "\n{} ({}):\n",
        "Vulnerabilities".bold().red(),
        vulns.len()
    ));
    if vulns.is_empty() {
        output.push_str(&format!("  {}\n", "No vulnerabilities found.".green()));
    } else {
        for sink in vulns {
            output.push_str(&format!(
                "  {} Line {}: {} flows to {} sink\n",
                "[!]".red().bold(),
                sink.line.to_string().yellow(),
                sink.var.red(),
                format!("{:?}", sink.sink_type).cyan()
            ));
        }
    }

    // Flows section
    if !result.flows.is_empty() {
        output.push_str(&format!(
            "\n{} ({}):\n",
            "Taint Flows".bold(),
            result.flows.len()
        ));
        for flow in &result.flows {
            output.push_str(&format!(
                "  {} (line {}) -> {} (line {})\n",
                flow.source.var.green(),
                flow.source.line,
                flow.sink.var.red(),
                flow.sink.line
            ));
            if !flow.path.is_empty() {
                output.push_str(&format!(
                    "    Path: {}\n",
                    flow.path
                        .iter()
                        .map(|b| b.to_string())
                        .collect::<Vec<_>>()
                        .join(" -> ")
                        .dimmed()
                ));
            }
        }
    }

    // Verbose: tainted variables per block
    if verbose && !result.tainted_vars.is_empty() {
        output.push_str(&format!("\n{}:\n", "Tainted Variables per Block".bold()));
        let mut blocks: Vec<_> = result.tainted_vars.keys().collect();
        blocks.sort();
        for block_id in blocks {
            if let Some(vars) = result.tainted_vars.get(block_id) {
                if !vars.is_empty() {
                    output.push_str(&format!(
                        "  Block {}: {}\n",
                        block_id,
                        vars.iter()
                            .map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                            .yellow()
                    ));
                }
            }
        }
    }

    // Sanitized variables
    if !result.sanitized_vars.is_empty() {
        output.push_str(&format!(
            "\n{}: {}\n",
            "Sanitized Variables".bold(),
            result
                .sanitized_vars
                .iter()
                .map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ")
                .green()
        ));
    }

    output
}

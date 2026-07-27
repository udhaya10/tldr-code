//! Fix command -- diagnose and auto-fix errors from compiler/runtime output.
//!
//! # Subcommands
//!
//! - `tldr fix diagnose` -- parse error text and produce a structured diagnosis
//! - `tldr fix apply` -- apply a fix to source code, writing the patched output
//! - `tldr fix check` -- run test command, diagnose failures, apply fixes in a loop
//!
//! # Examples
//!
//! ```sh
//! # Pipe a Python traceback from clipboard or file
//! tldr fix diagnose --error-file traceback.txt --source app.py
//!
//! # Inline error text
//! tldr fix diagnose --error "NameError: name 'json' is not defined" --source app.py
//!
//! # Apply fix to source (writes to stdout or --output)
//! tldr fix apply --error-file traceback.txt --source app.py
//!
//! # Run test, diagnose, fix, repeat loop
//! tldr fix check --file src/app.py --test-cmd "pytest tests/test_app.py"
//! ```

use std::io::Read;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};

use tldr_core::fix;
use tldr_core::Language;

use crate::output::{OutputFormat, OutputWriter};

/// Diagnose and auto-fix errors from compiler/runtime output.
///
/// Accepts (auto-detected) error text from any of:
///   - Rust:        cargo build / cargo check / rustc errors (E0xxx codes)
///   - C/C++:       gcc / clang diagnostics (`file:line:col: error: ...`)
///   - Python:      tracebacks (NameError, AttributeError, ImportError, ...)
///   - JS/TS:       jest / mocha test output, tsc errors (TS2xxx codes)
///   - Linters:     eslint --format json, ruff, pylint
///
/// The format is auto-detected from the error text — pass it via
/// `--error "..."`, `--error-file path`, or `--stdin`.
#[derive(Debug, Args)]
pub struct FixArgs {
    /// Fix subcommand
    #[command(subcommand)]
    pub command: FixCommand,
}

/// Fix subcommands
#[derive(Debug, Subcommand)]
pub enum FixCommand {
    /// Parse error output and produce a structured diagnosis with optional fix
    Diagnose(FixDiagnoseArgs),
    /// Apply fix edits to source code and write the patched result
    Apply(FixApplyArgs),
    /// Run test command, diagnose failures, apply fixes, and re-run in a loop
    Check(FixCheckArgs),
}

/// Arguments for `tldr fix check`
#[derive(Debug, Args)]
pub struct FixCheckArgs {
    /// Source file to fix
    #[arg(long, short = 'f')]
    pub file: PathBuf,

    /// Test command to run (e.g., "pytest tests/test_app.py")
    #[arg(long, short = 't')]
    pub test_cmd: String,

    /// Maximum number of fix attempts (default: 5)
    #[arg(long, default_value = "5")]
    pub max_attempts: usize,
}

/// Arguments for `tldr fix diagnose`
#[derive(Debug, Args)]
pub struct FixDiagnoseArgs {
    /// Source file to analyze (required for tree-sitter based analysis)
    #[arg(long, short = 's')]
    pub source: PathBuf,

    /// Inline error text (mutually exclusive with --error-file)
    #[arg(long, short = 'e', conflicts_with = "error_file")]
    pub error: Option<String>,

    /// File containing error text (mutually exclusive with --error)
    #[arg(long, conflicts_with = "error")]
    pub error_file: Option<PathBuf>,

    /// Read error text from stdin (when neither --error nor --error-file is given)
    #[arg(long)]
    pub stdin: bool,

    /// Path to API surface JSON file for enhanced analysis (e.g., TS2339 property suggestions)
    #[arg(long)]
    pub api_surface: Option<PathBuf>,
}

/// Arguments for `tldr fix apply`
#[derive(Debug, Args)]
pub struct FixApplyArgs {
    /// Source file to patch
    #[arg(long, short = 's')]
    pub source: PathBuf,

    /// Inline error text
    #[arg(long, short = 'e', conflicts_with = "error_file")]
    pub error: Option<String>,

    /// File containing error text
    #[arg(long, conflicts_with = "error")]
    pub error_file: Option<PathBuf>,

    /// Output file for the patched source (stdout if not specified)
    #[arg(long, short = 'o')]
    pub output: Option<PathBuf>,

    /// Read error text from stdin
    #[arg(long)]
    pub stdin: bool,

    /// Write the patched source back to the original file (in-place fix)
    #[arg(long, short = 'i')]
    pub in_place: bool,

    /// Show a unified diff instead of the full patched source
    #[arg(long, short = 'd')]
    pub diff: bool,

    /// Path to API surface JSON file for enhanced analysis (e.g., TS2339 property suggestions)
    #[arg(long)]
    pub api_surface: Option<PathBuf>,
}

impl FixArgs {
    /// Run the fix command
    pub fn run(&self, format: OutputFormat, _quiet: bool, lang: Option<Language>) -> Result<()> {
        let lang_str = lang.as_ref().map(Language::as_str);
        match &self.command {
            FixCommand::Diagnose(args) => run_diagnose(args, format, lang_str),
            FixCommand::Apply(args) => run_apply(args, format, lang_str),
            FixCommand::Check(args) => run_check(args, format, lang_str),
        }
    }
}

/// Read error text from one of: --error, --error-file, --stdin, or fallback to stdin
fn read_error_text(
    error: &Option<String>,
    error_file: &Option<PathBuf>,
    use_stdin: bool,
) -> Result<String> {
    if let Some(text) = error {
        return Ok(text.clone());
    }

    if let Some(path) = error_file {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow!("Failed to read error file '{}': {}", path.display(), e))?;
        return Ok(text);
    }

    if use_stdin || (error.is_none() && error_file.is_none()) {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| anyhow!("Failed to read from stdin: {}", e))?;
        if buf.is_empty() {
            return Err(anyhow!(
                "No error text provided. Use --error, --error-file, or pipe to stdin."
            ));
        }
        return Ok(buf);
    }

    Err(anyhow!(
        "No error text provided. Use --error, --error-file, --stdin, or pipe to stdin."
    ))
}

/// Compute a minimal unified diff between two strings, line by line.
///
/// Returns a string with lines prefixed by ` ` (unchanged), `-` (removed), or `+` (added).
fn compute_line_diff(old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let mut output = String::new();

    // Walk both sides; emit context / remove / add markers.
    let mut oi = 0;
    let mut ni = 0;
    while oi < old_lines.len() || ni < new_lines.len() {
        if oi < old_lines.len() && ni < new_lines.len() {
            if old_lines[oi] == new_lines[ni] {
                output.push_str(&format!(" {}\n", old_lines[oi]));
                oi += 1;
                ni += 1;
            } else {
                // Lines differ: emit removal then addition
                output.push_str(&format!("-{}\n", old_lines[oi]));
                output.push_str(&format!("+{}\n", new_lines[ni]));
                oi += 1;
                ni += 1;
            }
        } else if oi < old_lines.len() {
            output.push_str(&format!("-{}\n", old_lines[oi]));
            oi += 1;
        } else {
            output.push_str(&format!("+{}\n", new_lines[ni]));
            ni += 1;
        }
    }

    output
}

/// Run the diagnose subcommand
fn run_diagnose(args: &FixDiagnoseArgs, format: OutputFormat, lang: Option<&str>) -> Result<()> {
    let error_text = read_error_text(&args.error, &args.error_file, args.stdin)?;

    if let Some(surface_path) = &args.api_surface {
        eprintln!(
            "Note: API surface enrichment available from '{}'",
            surface_path.display()
        );
    }

    let source = std::fs::read_to_string(&args.source).map_err(|e| {
        anyhow!(
            "Failed to read source file '{}': {}",
            args.source.display(),
            e
        )
    })?;

    let diagnosis = fix::diagnose(&error_text, &source, lang, None);

    match diagnosis {
        Some(diag) => {
            let writer = OutputWriter::new(format, false);
            writer.write(&diag)?;
            Ok(())
        }
        None => Err(anyhow!(
            "Could not parse or diagnose the error. The error format may not be supported yet."
        )),
    }
}

/// Run the apply subcommand
fn run_apply(args: &FixApplyArgs, format: OutputFormat, lang: Option<&str>) -> Result<()> {
    let error_text = read_error_text(&args.error, &args.error_file, args.stdin)?;

    if let Some(surface_path) = &args.api_surface {
        eprintln!(
            "Note: API surface enrichment available from '{}'",
            surface_path.display()
        );
    }

    let source = std::fs::read_to_string(&args.source).map_err(|e| {
        anyhow!(
            "Failed to read source file '{}': {}",
            args.source.display(),
            e
        )
    })?;

    let diagnosis = fix::diagnose(&error_text, &source, lang, None).ok_or_else(|| {
        anyhow!("Could not parse or diagnose the error. The error format may not be supported.")
    })?;

    match &diagnosis.fix {
        Some(fix_data) => {
            let patched = fix::apply_fix(&source, fix_data);

            if args.diff {
                // Show unified diff instead of full patched source
                match format {
                    OutputFormat::Json | OutputFormat::Compact => {
                        let diff_text = compute_line_diff(&source, &patched);
                        let result = serde_json::json!({
                            "diagnosis": diagnosis,
                            "diff": diff_text,
                        });
                        let writer = OutputWriter::new(format, false);
                        writer.write(&result)?;
                    }
                    _ => {
                        let diff_text = compute_line_diff(&source, &patched);
                        print!("{}", diff_text);
                    }
                }
                Ok(())
            } else if args.in_place {
                std::fs::write(&args.source, &patched).map_err(|e| {
                    anyhow!(
                        "Failed to write patched source to '{}': {}",
                        args.source.display(),
                        e
                    )
                })?;
                eprintln!("Fixed: {}", diagnosis.message);
                Ok(())
            } else if let Some(output_path) = &args.output {
                std::fs::write(output_path, &patched).map_err(|e| {
                    anyhow!(
                        "Failed to write patched source to '{}': {}",
                        output_path.display(),
                        e
                    )
                })?;
                eprintln!("Fixed: {}", diagnosis.message);
                Ok(())
            } else {
                // Write to stdout
                match format {
                    OutputFormat::Json | OutputFormat::Compact => {
                        let result = serde_json::json!({
                            "diagnosis": diagnosis,
                            "patched_source": patched,
                        });
                        let writer = OutputWriter::new(format, false);
                        writer.write(&result)?;
                    }
                    _ => {
                        // Text mode: just print the patched source
                        print!("{}", patched);
                    }
                }
                Ok(())
            }
        }
        None => {
            // No fix available -- print the diagnosis as advisory
            eprintln!(
                "No auto-fix available (confidence: {:?}). Diagnosis:",
                diagnosis.confidence
            );
            let writer = OutputWriter::new(format, false);
            writer.write(&diagnosis)?;
            // Exit with non-zero to indicate no fix was applied
            Err(anyhow!(
                "No deterministic fix available for this error. Escalate to a model."
            ))
        }
    }
}

/// Run the check subcommand: test -> diagnose -> fix -> repeat loop.
fn run_check(args: &FixCheckArgs, format: OutputFormat, lang: Option<&str>) -> Result<()> {
    use fix::{run_check_loop, CheckConfig};

    if !args.file.exists() {
        return Err(anyhow!(
            "Source file '{}' does not exist.",
            args.file.display()
        ));
    }

    let config = CheckConfig {
        file: &args.file,
        test_cmd: &args.test_cmd,
        lang,
        max_attempts: args.max_attempts,
    };

    let result = run_check_loop(&config);

    // Report results
    let writer = OutputWriter::new(format, false);
    writer.write(&result)?;

    if result.final_pass {
        eprintln!(
            "All errors fixed in {} iteration{}.",
            result.iterations,
            if result.iterations == 1 { "" } else { "s" }
        );
        Ok(())
    } else {
        Err(anyhow!(
            "Some errors could not be fixed after {} attempt{}.",
            result.iterations,
            if result.iterations == 1 { "" } else { "s" }
        ))
    }
}

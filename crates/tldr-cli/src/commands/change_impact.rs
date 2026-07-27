//! Change Impact command - Find tests affected by code changes
//!
//! Wires tldr-core::change_impact to the CLI (Session 6 Phases 1-5).
//!
//! # Detection Methods
//! - `--files` - Explicit file list
//! - `--base <branch>` - Git diff against base branch (for PRs)
//! - `--staged` - Only staged files
//! - `--uncommitted` - Staged + unstaged (default git mode)
//! - Default: git diff HEAD
//!
//! # Output Formats
//! - JSON (default): Full report structure
//! - Text: Human-readable summary
//! - Runner formats: pytest, pytest-k, jest, go-test, cargo-test

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use tldr_core::{
    change_impact_extended, ChangeImpactReport, ChangeImpactStatus, DetectionMethod, Language,
};

use crate::output::{format_change_impact_text, OutputFormat, OutputWriter};
use crate::path_validation::require_directory;

/// Find tests affected by code changes
#[derive(Debug, Args)]
pub struct ChangeImpactArgs {
    /// Project root directory (default: current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Programming language (auto-detect if not specified)
    #[arg(long, short = 'l')]
    pub lang: Option<Language>,

    // === Change Detection ===
    /// Explicit list of changed files (comma-separated)
    #[arg(long, short = 'F', value_delimiter = ',')]
    pub files: Vec<PathBuf>,

    /// Git base branch for diff (e.g., "origin/main" for PR workflow)
    #[arg(long, short = 'b')]
    pub base: Option<String>,

    /// Only consider staged files (pre-commit workflow)
    #[arg(long)]
    pub staged: bool,

    /// Consider all uncommitted changes (staged + unstaged)
    #[arg(long)]
    pub uncommitted: bool,

    // === Analysis Options ===
    /// Maximum call graph traversal depth
    #[arg(long, short = 'd', default_value = "10")]
    pub depth: usize,

    /// Include import graph in analysis (not just call graph)
    #[arg(long, default_value = "true")]
    pub include_imports: bool,

    /// Custom test file patterns (comma-separated globs)
    #[arg(long, value_delimiter = ',')]
    pub test_patterns: Vec<String>,

    // === Output Options ===
    /// Output format override (backwards compatibility, prefer global --format/-f)
    #[arg(long = "output-format", short = 'o', hide = true)]
    pub output_format: Option<OutputFormat>,

    /// Output format for test runner integration
    #[arg(long, value_enum)]
    pub runner: Option<RunnerFormat>,
}

/// Test runner output formats
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum RunnerFormat {
    /// pytest: space-separated test files
    Pytest,
    /// pytest with -k: pytest test_file.py::TestClass::test_func
    PytestK,
    /// jest --findRelatedTests format
    Jest,
    /// go test -run regex
    GoTest,
    /// cargo test filter
    CargoTest,
}

impl ChangeImpactArgs {
    /// Determine detection method based on CLI flags
    fn determine_detection_method(&self) -> DetectionMethod {
        // Priority: explicit files > --base > --staged > --uncommitted > HEAD
        if !self.files.is_empty() {
            DetectionMethod::Explicit
        } else if let Some(base) = &self.base {
            DetectionMethod::GitBase { base: base.clone() }
        } else if self.staged {
            DetectionMethod::GitStaged
        } else if self.uncommitted {
            DetectionMethod::GitUncommitted
        } else {
            DetectionMethod::GitHead
        }
    }

    /// Run the change-impact command
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let writer = OutputWriter::new(self.output_format.unwrap_or(format), quiet);

        // cli-error-clarity-v2 (P2.BUG-4): reject regular files up-front so
        // callers don't get the cryptic "Git: Not a directory (os error 20)"
        // surfaced from the git invocation downstream.
        require_directory(&self.path, "change-impact")?;

        // Determine language (auto-detect from directory, default to Python)
        let language = self
            .lang
            .unwrap_or_else(|| Language::from_directory(&self.path).unwrap_or(Language::Python));

        // Determine detection method based on flags
        let detection = self.determine_detection_method();

        writer.progress(&format!(
            "Detecting changes via {} for {:?} in {}...",
            detection,
            language,
            self.path.display()
        ));

        // Prepare explicit files if provided
        let explicit_files = if !self.files.is_empty() {
            Some(self.files.clone())
        } else {
            None
        };

        // Call core change_impact_extended function
        let report = change_impact_extended(
            &self.path,
            detection,
            language,
            self.depth,
            self.include_imports,
            &self.test_patterns,
            explicit_files,
        )?;

        // Output based on format/runner — always emit the report (including
        // failure states) so JSON consumers see the new `status` field.
        if let Some(runner) = self.runner {
            let runner_output = format_for_runner(&report, runner);
            println!("{}", runner_output);
        } else if writer.is_text() {
            let text = format_change_impact_text(&report);
            writer.write_text(&text)?;
        } else {
            writer.write(&report)?;
        }

        // Map failure states to a distinct exit code so shell callers can
        // distinguish "no baseline" from "no changes" without parsing JSON.
        match &report.status {
            ChangeImpactStatus::Completed | ChangeImpactStatus::NoChanges => Ok(()),
            ChangeImpactStatus::NoBaseline { reason } => {
                eprintln!(
                    "ERROR: change-impact: no baseline ({reason}). Try --files <path> or --base <ref>."
                );
                std::process::exit(3);
            }
            ChangeImpactStatus::DetectionFailed { reason } => {
                eprintln!(
                    "ERROR: change-impact: detection failed ({reason}). Try --files <path> or --base <ref>."
                );
                std::process::exit(3);
            }
        }
    }
}

/// Format report for specific test runner
fn format_for_runner(report: &ChangeImpactReport, runner: RunnerFormat) -> String {
    match runner {
        RunnerFormat::Pytest => {
            // Space-separated test file paths
            report
                .affected_tests
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(" ")
        }
        RunnerFormat::PytestK => {
            // pytest file::class::function format
            if report.affected_test_functions.is_empty() {
                // Fall back to file-level if no function extraction
                report
                    .affected_tests
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                report
                    .affected_test_functions
                    .iter()
                    .map(|tf| {
                        if let Some(ref class) = tf.class {
                            format!("{}::{}::{}", tf.file.display(), class, tf.function)
                        } else {
                            format!("{}::{}", tf.file.display(), tf.function)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            }
        }
        RunnerFormat::Jest => {
            // --findRelatedTests format (uses changed files, not test files)
            if report.changed_files.is_empty() {
                String::new()
            } else {
                format!(
                    "--findRelatedTests {}",
                    report
                        .changed_files
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            }
        }
        RunnerFormat::GoTest => {
            // go test -run "TestA|TestB" format
            // Extract test function names from affected_functions that look like tests
            let test_names: Vec<String> = report
                .affected_functions
                .iter()
                .filter(|f| f.name.starts_with("Test"))
                .map(|f| f.name.clone())
                .collect();

            if test_names.is_empty() {
                String::new()
            } else {
                format!("-run \"{}\"", test_names.join("|"))
            }
        }
        RunnerFormat::CargoTest => {
            // cargo test filter names (test function names)
            let test_names: Vec<String> = report
                .affected_functions
                .iter()
                .filter(|f| f.name.starts_with("test_"))
                .map(|f| f.name.clone())
                .collect();

            test_names.join(" ")
        }
    }
}

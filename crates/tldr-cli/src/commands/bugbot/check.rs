//! Bugbot check command - analyze uncommitted changes for potential bugs
//!
//! Wires the full pipeline: detect changes, compute baselines, L1 commodity
//! tool execution (clippy, cargo-audit), AST-diff, signature-regression
//! analysis, and born-dead detection.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Result};
use clap::Args;

use tldr_core::Language;

use crate::output::{OutputFormat, OutputWriter};

use super::baseline::{get_baseline_content, write_baseline_tmpfile, BaselineStatus};
use super::changes::detect_changes;
use super::dead::compose_born_dead_scoped;
use super::diff::diff_functions;
use super::l2::types::AnalyzerStatus;
use super::runner::ToolRunner;
use super::signature::compose_signature_regression;
use super::text_format::format_bugbot_text;
use super::tools::{L1Finding, ToolRegistry, ToolResult};
use super::types::{
    BugbotCheckReport, BugbotExitError, BugbotFinding, BugbotSummary, L2AnalyzerResult,
};

/// Run bugbot check on uncommitted changes
#[derive(Debug, Args)]
pub struct BugbotCheckArgs {
    /// Project root directory
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Git base reference to diff against
    #[arg(long, default_value = "HEAD")]
    pub base_ref: String,

    /// Check only staged changes
    #[arg(long)]
    pub staged: bool,

    /// Maximum number of findings to report (0 = unlimited)
    #[arg(long, default_value = "50")]
    pub max_findings: usize,

    /// Do not fail (exit 0) even if findings exist
    #[arg(long)]
    pub no_fail: bool,

    /// Suppress progress messages
    #[arg(long, short)]
    pub quiet: bool,

    /// Disable L1 commodity tool analysis (clippy, cargo-audit, etc.)
    #[arg(long, default_value_t = false)]
    pub no_tools: bool,

    /// Timeout for each L1 tool in seconds
    #[arg(long, default_value_t = 60)]
    pub tool_timeout: u64,
}

impl BugbotCheckArgs {
    /// Run the bugbot check command
    ///
    /// `format` and `quiet` come from the global CLI flags.
    /// `lang` comes from the global `--lang` / `-l` flag (already parsed as `Language` enum).
    pub fn run(&self, format: OutputFormat, quiet: bool, lang: Option<Language>) -> Result<()> {
        let start = Instant::now();
        let writer = OutputWriter::new(format, quiet);
        let mut errors: Vec<String> = Vec::new();
        let mut notes: Vec<String> = Vec::new();

        // Step 1: Resolve language
        let language = match lang {
            Some(l) => l,
            None => match Language::from_directory(&self.path) {
                Some(l) => l,
                None => {
                    bail!("Could not detect language. Use --lang <LANG>");
                }
            },
        };

        let language_str = format!("{:?}", language).to_lowercase();
        let project = std::fs::canonicalize(&self.path)?;

        // Step 1b: First-run detection and auto-scan (PM-34)
        let is_first_run = {
            use super::first_run::{detect_first_run, run_first_run_scan, FirstRunStatus};
            match detect_first_run(&project) {
                FirstRunStatus::FirstRun => {
                    let progress_fn = |msg: &str| writer.progress(msg);
                    match run_first_run_scan(&project, &progress_fn) {
                        Ok(result) => {
                            if !result.baseline_errors.is_empty() {
                                for err in &result.baseline_errors {
                                    errors.push(format!("first-run baseline: {err}"));
                                }
                            }
                            notes.push(format!(
                                "first_run_baseline_built_in_{}ms",
                                result.elapsed_ms
                            ));
                            true
                        }
                        Err(e) => {
                            errors.push(format!("first-run scan failed: {e}"));
                            // Continue anyway -- the L2 engines handle missing caches
                            true
                        }
                    }
                }
                FirstRunStatus::SubsequentRun { .. } => false,
            }
        };

        writer.progress(&format!(
            "Detecting {} changes in {}...",
            language_str,
            project.display()
        ));

        // Step 2: Detect changed files
        let changes = detect_changes(&project, &self.base_ref, self.staged, &language)?;

        // Step 3: Early return if no changes
        if changes.changed_files.is_empty() {
            let report = BugbotCheckReport {
                tool: "bugbot".to_string(),
                mode: "check".to_string(),
                language: language_str,
                base_ref: self.base_ref.clone(),
                detection_method: changes.detection_method,
                timestamp: chrono::Utc::now().to_rfc3339(),
                changed_files: Vec::new(),
                findings: Vec::new(),
                summary: build_summary(&[], 0, 0),
                elapsed_ms: start.elapsed().as_millis() as u64,
                errors: Vec::new(),
                notes: vec!["no_changes_detected".to_string()],
                tool_results: Vec::new(),
                tools_available: Vec::new(),
                tools_missing: Vec::new(),
                l2_engine_results: Vec::new(),
            };

            if writer.is_text() {
                writer.write_text(&format_bugbot_text(&report))?;
            } else {
                writer.write(&report)?;
            }
            return Ok(());
        }

        writer.progress(&format!(
            "Found {} changed {} file(s)",
            changes.changed_files.len(),
            language_str
        ));

        // Step 4: Per-file baseline extraction and AST diff
        let mut all_diffs: HashMap<PathBuf, Vec<crate::commands::remaining::types::ASTChange>> =
            HashMap::new();
        // Keep temp files alive until the pipeline finishes (dropping deletes them)
        let mut _tmpfiles: Vec<tempfile::NamedTempFile> = Vec::new();
        // File contents for L2Context: baseline (pre-change) and current (post-change)
        let mut baseline_contents: HashMap<PathBuf, String> = HashMap::new();
        let mut current_contents: HashMap<PathBuf, String> = HashMap::new();

        for file in &changes.changed_files {
            match get_baseline_content(&project, file, &self.base_ref) {
                Ok(BaselineStatus::Exists(content)) => {
                    if file.exists() {
                        // Save baseline and current file contents for L2 engines
                        let rel_path = file.strip_prefix(&project).unwrap_or(file).to_path_buf();
                        baseline_contents.insert(rel_path.clone(), content.clone());
                        if let Ok(current) = std::fs::read_to_string(file) {
                            current_contents.insert(rel_path, current);
                        }

                        // Normal case: diff baseline vs current
                        match write_baseline_tmpfile(&content, file) {
                            Ok(tmpfile) => {
                                match diff_functions(tmpfile.path(), file) {
                                    Ok(report) => {
                                        all_diffs.insert(file.clone(), report.changes);
                                    }
                                    Err(e) => {
                                        errors.push(format!(
                                            "diff failed for {}: {}",
                                            file.display(),
                                            e
                                        ));
                                    }
                                }
                                _tmpfiles.push(tmpfile);
                            }
                            Err(e) => {
                                errors.push(format!(
                                    "baseline tmpfile failed for {}: {}",
                                    file.display(),
                                    e
                                ));
                            }
                        }
                    } else {
                        // File existed at baseline but is now deleted -- skip for v0.1
                        notes.push(format!("deleted_file:{}", file.display()));
                    }
                }
                Ok(BaselineStatus::NewFile) => {
                    if file.exists() {
                        // Save empty baseline and current file contents for L2 engines
                        let rel_path = file.strip_prefix(&project).unwrap_or(file).to_path_buf();
                        baseline_contents.insert(rel_path.clone(), String::new());
                        if let Ok(current) = std::fs::read_to_string(file) {
                            current_contents.insert(rel_path, current);
                        }

                        // New file: diff against an empty baseline so all functions are Insert
                        let extension = file.extension().and_then(|e| e.to_str()).unwrap_or("txt");
                        match tempfile::Builder::new()
                            .prefix("bugbot_empty_")
                            .suffix(&format!(".{}", extension))
                            .tempfile()
                        {
                            Ok(mut empty_file) => {
                                // Write nothing (empty file)
                                let _ = empty_file.flush();
                                match diff_functions(empty_file.path(), file) {
                                    Ok(report) => {
                                        all_diffs.insert(file.clone(), report.changes);
                                    }
                                    Err(e) => {
                                        errors.push(format!(
                                            "diff (new file) failed for {}: {}",
                                            file.display(),
                                            e
                                        ));
                                    }
                                }
                                _tmpfiles.push(empty_file);
                            }
                            Err(e) => {
                                errors.push(format!(
                                    "empty tmpfile failed for {}: {}",
                                    file.display(),
                                    e
                                ));
                            }
                        }
                    }
                }
                Ok(BaselineStatus::GitShowFailed(msg)) => {
                    errors.push(format!("git show failed for {}: {}", file.display(), msg));
                }
                Err(e) => {
                    errors.push(format!("baseline error for {}: {}", file.display(), e));
                }
            }
        }

        let files_analyzed = all_diffs.len();
        let functions_analyzed: usize = all_diffs.values().map(|v| v.len()).sum();

        writer.progress(&format!(
            "Analyzed {} file(s), {} function-level change(s)",
            files_analyzed, functions_analyzed
        ));

        // Step 4b: Build L2 context and spawn L2 engines on background thread.
        // L2 is CPU-bound (tree-sitter, graph algorithms, data flow) while L1 is
        // I/O-bound (subprocess execution). Running them in parallel reduces wall
        // clock time from ~2.5s to ~1.5s.
        writer.progress("Running L1 + L2 analysis in parallel...");
        let l2_handle = {
            use super::l2::{l2_engine_registry, L2Context};

            let engines = l2_engine_registry();

            // Build L2Context from pipeline data. The L2 engines use changed_files,
            // function-level diffs, and file contents for their analysis.
            let relative_changed: Vec<PathBuf> = changes
                .changed_files
                .iter()
                .filter_map(|f| f.strip_prefix(&project).ok().map(|p| p.to_path_buf()))
                .collect();

            // Build ast_changes with relative paths to match L2Context conventions.
            let relative_diffs: HashMap<
                PathBuf,
                Vec<crate::commands::remaining::types::ASTChange>,
            > = all_diffs
                .iter()
                .map(|(path, changes)| {
                    let rel = path.strip_prefix(&project).unwrap_or(path).to_path_buf();
                    (rel, changes.clone())
                })
                .collect();

            // Create daemon client for this project. If a daemon is running,
            // deferred-tier engines will use cached IR artifacts.
            let daemon = super::l2::daemon_client::create_daemon_client(&project);

            // Convert AST changes to function-level diff for L2 engines
            let function_diff = build_function_diff(&all_diffs, &project);

            let l2_ctx = L2Context::new(
                project.clone(),
                language,
                relative_changed,
                function_diff,
                baseline_contents,
                current_contents,
                relative_diffs,
            )
            .with_first_run(is_first_run)
            .with_base_ref(self.base_ref.clone())
            .with_daemon(daemon);

            // Spawn L2 engines on background thread. L2Context uses DashMap +
            // OnceLock (Send+Sync), and L2Engine: Send+Sync, so both can move.
            std::thread::spawn(move || run_l2_engines(&l2_ctx, &engines))
        };

        // L1 runs on main thread concurrently with L2 (I/O-bound subprocess work)
        if !self.no_tools {
            writer.progress("Running L1 diagnostic tools...");
        }
        let (l1_raw, tool_results, tools_available, tools_missing) =
            run_l1_tools_opt(&project, &language_str, self.no_tools, self.tool_timeout);

        // Convert L1Finding -> BugbotFinding and filter to changed files (PM-3)
        let l1_bugbot: Vec<BugbotFinding> = l1_raw.into_iter().map(BugbotFinding::from).collect();
        let changed_paths: Vec<PathBuf> = changes
            .changed_files
            .iter()
            .filter_map(|f| f.strip_prefix(&project).ok().map(|p| p.to_path_buf()))
            .collect();
        let l1_filtered = filter_l1_findings(l1_bugbot, &changed_paths);
        let l1_count = l1_filtered.len();

        if !tools_available.is_empty() {
            let ran_count = tool_results.len();
            let finding_count: usize = tool_results.iter().map(|r| r.finding_count).sum();
            writer.progress(&format!(
                "L1 tools: {} ran, {} raw findings, {} after filtering to changed files",
                ran_count, finding_count, l1_count
            ));
        }

        // Step 5: Compose signature regression findings (main thread, uses all_diffs)
        let sig_findings = compose_signature_regression(&all_diffs, &project);

        // Step 6: Compose born-dead findings (only if there are Insert changes)
        // Filter inserted functions directly from references (avoids cloning)
        use crate::commands::remaining::types::{ChangeType, NodeKind};
        let inserts: Vec<&crate::commands::remaining::types::ASTChange> = all_diffs
            .values()
            .flat_map(|changes| changes.iter())
            .filter(|c| matches!(c.change_type, ChangeType::Insert))
            .filter(|c| matches!(c.node_kind, NodeKind::Function | NodeKind::Method))
            .collect();
        let dead_findings = if !inserts.is_empty() {
            writer.progress("Scanning for born-dead functions...");
            match compose_born_dead_scoped(&inserts, &changes.changed_files, &project, &language) {
                Ok(findings) => findings,
                Err(e) => {
                    errors.push(format!("born-dead analysis failed: {}", e));
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        // Join L2 thread -- graceful degradation if the thread panicked
        let (l2_engine_findings, l2_engine_results) = l2_handle.join().unwrap_or_else(|_| {
            errors.push("L2 engine thread panicked".to_string());
            (Vec::new(), Vec::new())
        });

        // Step 7: Merge L1 + L2 findings (compose_ + engine findings)
        let compose_l2_count = sig_findings.len() + dead_findings.len();
        let l2_count = compose_l2_count + l2_engine_findings.len();
        let mut findings: Vec<BugbotFinding> = Vec::new();
        findings.extend(l1_filtered);
        findings.extend(sig_findings);
        findings.extend(dead_findings);
        findings.extend(l2_engine_findings);

        // Step 8a: Dedup and prioritize (CK-4)
        use super::l2::dedup::dedup_and_prioritize;
        findings = dedup_and_prioritize(findings, self.max_findings);

        // Step 8b: Composition Engine (PM-41)
        use super::l2::composition::compose_findings;
        findings = compose_findings(findings);

        // Re-sort after composition (composed findings may have different severity)
        findings.sort_by(|a, b| {
            severity_rank(&b.severity)
                .cmp(&severity_rank(&a.severity))
                .then(a.file.cmp(&b.file))
                .then(a.line.cmp(&b.line))
        });

        // Step 9: Build summary (with L1/L2 breakdown)
        let summary = build_summary_with_l1(
            &findings,
            l1_count,
            l2_count,
            files_analyzed,
            functions_analyzed,
            &tool_results,
        );
        let elapsed_ms = start.elapsed().as_millis() as u64;

        // Step 10: Build and emit report
        let report = BugbotCheckReport {
            tool: "bugbot".to_string(),
            mode: "check".to_string(),
            language: language_str,
            base_ref: self.base_ref.clone(),
            detection_method: changes.detection_method,
            timestamp: chrono::Utc::now().to_rfc3339(),
            changed_files: changes.changed_files,
            findings,
            summary,
            elapsed_ms,
            errors,
            notes,
            tool_results,
            tools_available,
            tools_missing,
            l2_engine_results,
        };

        // Output
        if writer.is_text() {
            writer.write_text(&format_bugbot_text(&report))?;
        } else {
            writer.write(&report)?;
        }

        // Exit code for pre-push gating: `tldr bugbot check && git push`
        //
        // Exit codes:
        //   0 = clean (no findings, or --no-fail suppresses failure)
        //   1 = findings detected (analysis succeeded but bugs found)
        //   2 = analysis had errors with no findings (broken pipeline, not "clean")
        //   3 = critical findings detected (highest priority, takes precedence over 1)
        let has_findings = !report.findings.is_empty();
        let has_errors = !report.errors.is_empty();
        let has_critical = report.findings.iter().any(|f| f.severity == "critical");

        // PM-42: Critical findings exit code 3 takes precedence over exit code 1
        if has_critical && !self.no_fail {
            return Err(BugbotExitError::CriticalFindings {
                count: report
                    .findings
                    .iter()
                    .filter(|f| f.severity == "critical")
                    .count(),
            }
            .into());
        }

        if has_findings && !self.no_fail {
            return Err(BugbotExitError::FindingsDetected {
                count: report.findings.len(),
            }
            .into());
        }

        if !has_findings && has_errors && !self.no_fail {
            return Err(BugbotExitError::AnalysisErrors {
                count: report.errors.len(),
            }
            .into());
        }

        Ok(())
    }
}

/// Run L1 commodity diagnostic tools and return their findings and metadata.
///
/// Creates a `ToolRegistry`, detects available tools for the given language,
/// runs all available tools in parallel via `ToolRunner`, and returns:
/// - `l1_findings`: Raw `L1Finding`s from all tools
/// - `tool_results`: Execution results for each tool
/// - `available_names`: Names of tools that were available
/// - `missing_names`: Names of tools that were not installed
///
/// When `no_tools` is `true`, skips all L1 tool execution and returns empty
/// results. This is the `--no-tools` CLI flag path.
///
/// `timeout_secs` controls the per-tool timeout passed to `ToolRunner`.
fn run_l1_tools_opt(
    project_root: &std::path::Path,
    language: &str,
    no_tools: bool,
    timeout_secs: u64,
) -> (Vec<L1Finding>, Vec<ToolResult>, Vec<String>, Vec<String>) {
    if no_tools {
        return (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    }

    let registry = ToolRegistry::new();
    let (available, missing) = registry.detect_available_tools(language);

    let available_names: Vec<String> = available.iter().map(|t| t.name.to_string()).collect();
    let missing_names: Vec<String> = missing.iter().map(|t| t.name.to_string()).collect();

    if available.is_empty() {
        return (Vec::new(), Vec::new(), available_names, missing_names);
    }

    let runner = ToolRunner::new(timeout_secs);
    let (tool_results, l1_findings) = runner.run_tools_parallel(&available, project_root);

    (l1_findings, tool_results, available_names, missing_names)
}

/// Map severity string to a numeric rank for sorting (higher = more severe).
///
/// PM-8: "info" is explicitly ranked below "low" rather than falling through
/// to the wildcard case. Unknown severities get rank 0.
fn severity_rank(severity: &str) -> u8 {
    match severity {
        "critical" => 5,
        "high" => 4,
        "medium" => 3,
        "low" => 2,
        "info" => 1,
        _ => 0,
    }
}

/// Build summary statistics from the final findings list.
fn build_summary(
    findings: &[BugbotFinding],
    files_analyzed: usize,
    functions_analyzed: usize,
) -> BugbotSummary {
    build_summary_with_l1(
        findings,
        0,
        findings.len(),
        files_analyzed,
        functions_analyzed,
        &[],
    )
}

/// Build summary statistics with separate L1 and L2 finding counts.
///
/// Also counts tool execution statistics from `tool_results`.
///
/// F14: The `l1_count` and `l2_count` parameters are hints from the
/// pre-truncation pipeline. After truncation, these may be stale. This
/// function recalculates L1/L2 counts from the actual `findings` slice
/// to ensure `total_findings == l1_findings + l2_findings`.
fn build_summary_with_l1(
    findings: &[BugbotFinding],
    l1_count: usize,
    l2_count: usize,
    files_analyzed: usize,
    functions_analyzed: usize,
    tool_results: &[super::tools::ToolResult],
) -> BugbotSummary {
    let mut by_severity: HashMap<String, usize> = HashMap::new();
    let mut by_type: HashMap<String, usize> = HashMap::new();

    for f in findings {
        *by_severity.entry(f.severity.clone()).or_insert(0) += 1;
        *by_type.entry(f.finding_type.clone()).or_insert(0) += 1;
    }

    let tools_run = tool_results.len();
    let tools_failed = tool_results.iter().filter(|r| !r.success).count();

    // F14: Recalculate L1/L2 counts from actual findings to handle
    // post-truncation consistency. L1 findings have finding_type starting
    // with "tool:" (set by the L1Finding -> BugbotFinding conversion).
    let actual_l1 = findings
        .iter()
        .filter(|f| f.finding_type.starts_with("tool:"))
        .count();
    let actual_l2 = findings.len() - actual_l1;

    // Use the actual counts if they differ from the hints (truncation happened)
    let final_l1 = if actual_l1 + actual_l2 != l1_count + l2_count {
        actual_l1
    } else {
        l1_count
    };
    let final_l2 = if actual_l1 + actual_l2 != l1_count + l2_count {
        actual_l2
    } else {
        l2_count
    };

    BugbotSummary {
        total_findings: findings.len(),
        by_severity,
        by_type,
        files_analyzed,
        functions_analyzed,
        l1_findings: final_l1,
        l2_findings: final_l2,
        tools_run,
        tools_failed,
    }
}

/// Filter L1 findings to only include files in the changed set.
///
/// PM-3: L1 tools scan the whole project, but we only report findings for files
/// that are in the changed set. If `changed_files` is empty (scan mode or no
/// baseline), all findings are returned unfiltered.
fn filter_l1_findings(
    findings: Vec<BugbotFinding>,
    changed_files: &[PathBuf],
) -> Vec<BugbotFinding> {
    if changed_files.is_empty() {
        return findings;
    }
    findings
        .into_iter()
        .filter(|f| {
            changed_files.iter().any(|cf| {
                // Direct match (both relative or both absolute)
                cf == &f.file
                // L1 tools may emit absolute paths; compare by filename suffix
                || f.file.ends_with(cf)
                || cf.ends_with(&f.file)
            })
        })
        .collect()
}

/// Run a single L2 engine, applying language gating and collecting results.
///
/// Returns `Some((findings, result))` if the engine ran, or `Some(([], result))`
/// if it was skipped due to language gating.
fn run_single_engine(
    engine: &dyn super::l2::L2Engine,
    ctx: &super::l2::L2Context,
) -> (Vec<BugbotFinding>, L2AnalyzerResult) {
    // Language gating (PM-37): skip engines that declare specific language
    // support when the context language is not in the supported set.
    let supported = engine.languages();
    if !supported.is_empty() && !supported.contains(&ctx.language) {
        return (
            Vec::new(),
            L2AnalyzerResult {
                name: engine.name().to_string(),
                success: true,
                duration_ms: 0,
                finding_count: 0,
                functions_analyzed: 0,
                functions_skipped: 0,
                status: format!(
                    "Skipped: {} does not support {:?}",
                    engine.name(),
                    ctx.language
                ),
                errors: vec![],
            },
        );
    }

    let start = Instant::now();
    let output = engine.analyze(ctx);
    let duration = start.elapsed().as_millis() as u64;

    let status_str = match &output.status {
        AnalyzerStatus::Complete => "complete".to_string(),
        AnalyzerStatus::Partial { reason } => format!("partial ({})", reason),
        AnalyzerStatus::Skipped { reason } => format!("skipped ({})", reason),
        AnalyzerStatus::TimedOut { partial_findings } => {
            format!("timed out ({} partial findings)", partial_findings)
        }
    };

    let errors = match &output.status {
        AnalyzerStatus::Partial { reason } => vec![reason.clone()],
        AnalyzerStatus::TimedOut { .. } => vec!["Engine timed out".to_string()],
        _ => vec![],
    };

    let result = L2AnalyzerResult {
        name: engine.name().to_string(),
        success: matches!(output.status, AnalyzerStatus::Complete),
        duration_ms: duration,
        finding_count: output.findings.len(),
        functions_analyzed: output.functions_analyzed,
        functions_skipped: output.functions_skipped,
        status: status_str,
        errors,
    };

    (output.findings, result)
}

/// Run all registered L2 analysis engines.
///
/// Iterates over every engine in registration order, collects findings and
/// per-engine result summaries. Returns a tuple of (all_findings, engine_results).
fn run_l2_engines(
    ctx: &super::l2::L2Context,
    engines: &[Box<dyn super::l2::L2Engine>],
) -> (Vec<BugbotFinding>, Vec<L2AnalyzerResult>) {
    let mut all_findings = Vec::new();
    let mut results = Vec::new();

    for engine in engines {
        let (findings, result) = run_single_engine(engine.as_ref(), ctx);
        all_findings.extend(findings);
        results.push(result);
    }

    (all_findings, results)
}

/// Build a `FunctionDiff` from AST-level changes collected during the diff phase.
///
/// Iterates over all file-level `ASTChange` entries and converts function/method
/// changes into the `FunctionChange`, `InsertedFunction`, and `DeletedFunction`
/// types expected by `L2Context`. Non-function nodes (classes, statements, etc.)
/// and unnamed changes are skipped.
///
/// Paths in `all_diffs` are expected to be absolute; they are converted to
/// relative paths by stripping the `project` prefix, matching L2Context
/// conventions.
fn build_function_diff(
    all_diffs: &HashMap<PathBuf, Vec<crate::commands::remaining::types::ASTChange>>,
    project: &std::path::Path,
) -> super::l2::context::FunctionDiff {
    use super::l2::context::{DeletedFunction, FunctionChange, FunctionDiff, InsertedFunction};
    use super::l2::types::FunctionId;
    use crate::commands::remaining::types::{ChangeType, NodeKind};

    let mut changed_fns = Vec::new();
    let mut inserted_fns = Vec::new();
    let mut deleted_fns = Vec::new();

    for (abs_path, changes) in all_diffs {
        let rel_path = abs_path
            .strip_prefix(project)
            .unwrap_or(abs_path)
            .to_path_buf();

        for change in changes {
            // Only process function-level changes
            if !matches!(change.node_kind, NodeKind::Function | NodeKind::Method) {
                continue;
            }

            let name = match &change.name {
                Some(n) => n.clone(),
                None => continue, // Skip unnamed changes
            };

            let def_line = change
                .new_location
                .as_ref()
                .or(change.old_location.as_ref())
                .map(|loc| loc.line as usize)
                .unwrap_or(0);

            let func_id = FunctionId::new(rel_path.clone(), &name, def_line);

            match change.change_type {
                ChangeType::Update => {
                    let old_source = change.old_text.clone().unwrap_or_default();
                    let new_source = change.new_text.clone().unwrap_or_default();
                    changed_fns.push(FunctionChange {
                        id: func_id,
                        name: name.clone(),
                        old_source,
                        new_source,
                    });
                }
                ChangeType::Insert => {
                    let source = change.new_text.clone().unwrap_or_default();
                    inserted_fns.push(InsertedFunction {
                        id: func_id,
                        name: name.clone(),
                        source,
                    });
                }
                ChangeType::Delete => {
                    deleted_fns.push(DeletedFunction {
                        id: func_id,
                        name: name.clone(),
                    });
                }
                ChangeType::Move
                | ChangeType::Rename
                | ChangeType::Extract
                | ChangeType::Inline
                | ChangeType::Format => {
                    // Treat as Update if both old and new texts exist
                    if change.old_text.is_some() && change.new_text.is_some() {
                        changed_fns.push(FunctionChange {
                            id: func_id,
                            name: name.clone(),
                            old_source: change.old_text.clone().unwrap_or_default(),
                            new_source: change.new_text.clone().unwrap_or_default(),
                        });
                    }
                }
            }
        }
    }

    FunctionDiff {
        changed: changed_fns,
        inserted: inserted_fns,
        deleted: deleted_fns,
    }
}

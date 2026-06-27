//! Dead command - Find dead code
//!
//! Identifies functions that are never called (unreachable code).
//! Auto-routes through daemon when available for ~35x speedup.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;
use serde::Serialize;
use tldr_core::walker::ProjectWalker;

/// Maximum number of files to scan in WalkDir traversals.
///
/// Prevents runaway scans in massive monorepos or symlink-heavy layouts.
/// Projects with fewer files are unaffected.
const MAX_FILES: usize = 10_000;

use tldr_core::analysis::dead::dead_code_analysis_refcount;
use tldr_core::analysis::refcount::count_identifiers_in_tree;
use tldr_core::ast::parser::parse_file;
use tldr_core::ast::{extract_file, extract_from_tree};
use tldr_core::types::{DeadCodeReport, ModuleInfo};
use tldr_core::{
    build_project_call_graph, collect_all_functions, dead_code_analysis, FunctionRef, Language,
};

use crate::commands::daemon_router::{is_oneshot, route_for_path};
use crate::output::{OutputFormat, OutputWriter};

/// Find dead (unreachable) code
#[derive(Debug, Args)]
pub struct DeadArgs {
    /// Project root directory (default: current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Programming language
    #[arg(long, short = 'l')]
    pub lang: Option<Language>,

    /// Custom entry point patterns (comma-separated)
    #[arg(long, short = 'e', value_delimiter = ',')]
    pub entry_points: Vec<String>,

    /// Maximum number of dead functions to display
    #[arg(long, default_value = "100")]
    pub max_items: usize,

    /// Use call-graph-based analysis instead of the default reference counting
    #[arg(long)]
    pub call_graph: bool,

    /// Walk vendored/build dirs (node_modules, target, dist, etc.) that would normally be skipped.
    #[arg(long)]
    pub no_default_ignore: bool,
}

impl DeadArgs {
    /// Run the dead command
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let writer = OutputWriter::new(format, quiet);

        // Validate path exists BEFORE language detection / progress banner
        // (lang-detect-default-v1)
        if !self.path.exists() {
            anyhow::bail!("Path not found: {}", self.path.display());
        }

        // Determine language (auto-detect from directory, default to Python).
        // Resolved once and sent on the wire so the daemon uses the same
        // language as compute_local.
        let language = self
            .lang
            .unwrap_or_else(|| Language::from_directory(&self.path).unwrap_or(Language::Python));

        let entry_points: Option<Vec<String>> = if self.entry_points.is_empty() {
            None
        } else {
            Some(self.entry_points.clone())
        };

        // ADR-10 (TLDR-7pp.1.5): daemon is the only serve path; `--oneshot` is
        // the sole explicit local-compute escape. No silent fallback. The
        // full flag envelope (call_graph + no_default_ignore + entry points +
        // resolved language) travels so the daemon runs EXACTLY the analysis
        // compute_local runs (previously the daemon path always used the
        // call-graph analyzer and ignored --call-graph/--no-default-ignore,
        // diverging from the CLI's default reference-counting analysis).
        let report = if is_oneshot() {
            self.compute_local(language, entry_points.as_deref(), &writer)?
        } else {
            let params = serde_json::json!({
                "path": self.path,
                "entry": entry_points,
                "language": language,
                "call_graph": self.call_graph,
                "no_default_ignore": self.no_default_ignore,
            });
            route_for_path::<DeadCodeReport>(&self.path, "dead", params).into_hit_or_bail("dead")?
        };

        // Apply truncation if needed (presentation concern, CLI-side).
        let (truncated_report, truncated, total_count, shown_count) =
            apply_truncation(report, self.max_items);

        // Single renderer for both paths.
        if writer.is_text() {
            let text = format_dead_code_text_truncated(
                &truncated_report,
                truncated,
                total_count,
                shown_count,
            );
            writer.write_text(&text)?;
        } else {
            let _ = (total_count, shown_count); // text path only
            let output = DeadCodeOutput {
                report: truncated_report,
                truncated,
            };
            writer.write(&output)?;
        }

        Ok(())
    }

    /// Local in-process dead-code analysis — reached only via `--oneshot`.
    fn compute_local(
        &self,
        language: Language,
        entry_points: Option<&[String]>,
        writer: &OutputWriter,
    ) -> Result<DeadCodeReport> {
        if self.call_graph {
            writer.progress(&format!(
                "Building call graph for {} ({:?})...",
                self.path.display(),
                language
            ));
        } else {
            writer.progress(&format!(
                "Scanning {} ({:?}) with reference counting...",
                self.path.display(),
                language
            ));
        }
        compute_dead_report(
            &self.path,
            language,
            entry_points,
            self.call_graph,
            self.no_default_ignore,
        )
    }
}

/// Run the dead-code analysis for `path`, shared by the CLI `--oneshot` path
/// and the daemon `Dead` handler so the two produce byte-identical reports
/// (TLDR-7pp.1.5). `call_graph` selects the call-graph analyzer; the default
/// (`false`) is the single-pass reference-counting analyzer.
pub(crate) fn compute_dead_report(
    path: &Path,
    language: Language,
    entry_points: Option<&[String]>,
    call_graph: bool,
    no_default_ignore: bool,
) -> Result<DeadCodeReport> {
    if call_graph {
        // Call-graph path: build graph, then analyze.
        let graph = build_project_call_graph(path, language, None, true)?;
        let module_infos = collect_module_infos(path, language, no_default_ignore);
        let all_functions: Vec<FunctionRef> = collect_all_functions(&module_infos);
        Ok(dead_code_analysis(&graph, &all_functions, entry_points)?)
    } else {
        // Default path: reference counting (single-pass).
        let (module_infos, merged_ref_counts) =
            collect_module_infos_with_refcounts(path, language, no_default_ignore);
        let all_functions: Vec<FunctionRef> = collect_all_functions(&module_infos);
        Ok(dead_code_analysis_refcount(
            &all_functions,
            &merged_ref_counts,
            entry_points,
        )?)
    }
}

/// Check if JS/TS source has a file-level 'use server' or 'use client' directive.
/// This is checked on the source string directly (no file I/O) to avoid path resolution issues.
fn source_has_framework_directive(source: &str, ext: &str) -> bool {
    if !matches!(ext, "ts" | "tsx" | "js" | "jsx" | "mjs") {
        return false;
    }
    for line in source.lines().take(5) {
        let trimmed = line.trim();
        if trimmed == r#""use server""#
            || trimmed == r#"'use server'"#
            || trimmed == r#""use server";"#
            || trimmed == r#"'use server';"#
            || trimmed == r#""use client""#
            || trimmed == r#"'use client'"#
            || trimmed == r#""use client";"#
            || trimmed == r#"'use client';"#
        {
            return true;
        }
        // Skip empty lines and comments
        if !trimmed.is_empty()
            && !trimmed.starts_with("//")
            && !trimmed.starts_with("/*")
            && !trimmed.starts_with('*')
            && !trimmed.starts_with('"')
            && !trimmed.starts_with('\'')
        {
            break;
        }
    }
    false
}

/// Tag all functions and class methods in a ModuleInfo with a synthetic decorator
/// if the source contains a framework directive ('use server'/'use client').
fn tag_directive_functions(info: &mut ModuleInfo, source: &str, path: &Path) {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if source_has_framework_directive(source, ext) {
        for func in &mut info.functions {
            if !func
                .decorators
                .contains(&"use_server_directive".to_string())
            {
                func.decorators.push("use_server_directive".to_string());
            }
        }
        for class in &mut info.classes {
            for method in &mut class.methods {
                if !method
                    .decorators
                    .contains(&"use_server_directive".to_string())
                {
                    method.decorators.push("use_server_directive".to_string());
                }
            }
        }
    }
}

/// inheritance-and-dead-cleanup-v1 (M6): TypeScript declaration files
/// (`.d.ts`) contain only `interface` / `type` / ambient declarations — no
/// executable code. Including them in dead-code analysis produces false
/// "possibly_dead" findings for every declared symbol. Mirrors the
/// oversize-skip pattern used elsewhere in the codebase.
fn is_typescript_declaration_file(path: &Path) -> bool {
    path.to_string_lossy()
        .to_ascii_lowercase()
        .ends_with(".d.ts")
}

/// Collect ModuleInfo from all files in a directory using detailed AST extraction.
///
/// This provides the enriched function metadata (decorators, visibility, etc.)
/// needed for accurate dead code analysis with low false-positive rates.
fn collect_module_infos(
    path: &Path,
    language: Language,
    no_default_ignore: bool,
) -> Vec<(PathBuf, ModuleInfo)> {
    let mut module_infos = Vec::new();

    if path.is_file() {
        // M6: skip .d.ts declaration-only files
        if is_typescript_declaration_file(path) {
            return module_infos;
        }
        if let Ok(mut info) = extract_file(path, path.parent()) {
            if let Ok(source) = std::fs::read_to_string(path) {
                tag_directive_functions(&mut info, &source, path);
            }
            // Use filename only for single files (matches call graph convention)
            let rel_path = path
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| path.to_path_buf());
            module_infos.push((rel_path, info));
        }
    } else {
        // language-coverage-fixes-v1 (P4.BUG-N1, P4.BUG-N5): use
        // `scan_extensions()` so C++ dir scans include `.h` and JS/TS
        // sibling extensions (`.tsx` ↔ `.jsx`) participate together.
        let extensions: &[&str] = language.scan_extensions();
        let mut file_count: usize = 0;
        // residual-bugs-v1 (P15.AGG14-7-cascade): pass the resolved
        // language to the walker so JS/TS source under `src/build/` or
        // `packages/x/dist/` is preserved (mirrors the per-language gate
        // in `crates/tldr-core/src/callgraph/scanner.rs`). Without this
        // hint, `tldr dead /tmp/repos/ts-dom-gen` returned
        // `functions_analyzed: 0` because the walker silently skipped
        // `src/build/`, where the entire authored TypeScript surface
        // lives.
        let mut walker = ProjectWalker::new(path).lang_hint(language);
        if no_default_ignore {
            walker = walker.no_default_ignore();
        }
        for entry in walker.iter() {
            let file_path = entry.path();
            if file_path.is_file() {
                // M6: skip .d.ts declaration-only files
                if is_typescript_declaration_file(file_path) {
                    continue;
                }
                if let Some(ext_str) = file_path.extension().and_then(|e| e.to_str()) {
                    let dotted = format!(".{}", ext_str);
                    if extensions.contains(&dotted.as_str()) {
                        file_count += 1;
                        if file_count > MAX_FILES {
                            eprintln!(
                                "Warning: dead code scan truncated at {} files in {}",
                                MAX_FILES,
                                path.display()
                            );
                            break;
                        }
                        if let Ok(mut info) = extract_file(file_path, Some(path)) {
                            // Tag functions with framework directive from source
                            if let Ok(source) = std::fs::read_to_string(file_path) {
                                tag_directive_functions(&mut info, &source, file_path);
                            }
                            // Use relative path to match call graph edge convention
                            let rel_path = file_path
                                .strip_prefix(path)
                                .unwrap_or(file_path)
                                .to_path_buf();
                            module_infos.push((rel_path, info));
                        }
                    }
                }
            }
        }
    }

    module_infos
}

/// Collect ModuleInfo AND identifier reference counts in a single pass.
///
/// For each file, we parse once with tree-sitter and then run both:
/// - `extract_from_tree()` to get ModuleInfo (functions, classes, imports)
/// - `count_identifiers_in_tree()` to get identifier occurrence counts
///
/// The identifier counts are merged into a single project-wide HashMap.
pub(crate) fn collect_module_infos_with_refcounts(
    path: &Path,
    language: Language,
    no_default_ignore: bool,
) -> (Vec<(PathBuf, ModuleInfo)>, HashMap<String, usize>) {
    let mut module_infos = Vec::new();
    let mut merged_counts: HashMap<String, usize> = HashMap::new();

    if path.is_file() {
        // M6: skip .d.ts declaration-only files (still produce empty
        // module_infos / counts so callers behave gracefully).
        if is_typescript_declaration_file(path) {
            return (module_infos, merged_counts);
        }
        if let Ok((tree, source, lang)) = parse_file(path) {
            // Extract ModuleInfo from the parsed tree
            if let Ok(mut info) = extract_from_tree(&tree, &source, lang, path, path.parent()) {
                tag_directive_functions(&mut info, &source, path);
                let rel_path = path
                    .file_name()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| path.to_path_buf());
                module_infos.push((rel_path, info));
            }
            // Count identifiers from the same parsed tree
            let file_counts = count_identifiers_in_tree(&tree, source.as_bytes(), lang);
            for (name, count) in file_counts {
                *merged_counts.entry(name).or_insert(0) += count;
            }
        }
    } else {
        // language-coverage-fixes-v1 (P4.BUG-N1, P4.BUG-N5): use
        // `scan_extensions()` so C++ dir scans include `.h` and JS/TS
        // sibling extensions (`.tsx` ↔ `.jsx`) participate together.
        let extensions: &[&str] = language.scan_extensions();
        let mut file_count: usize = 0;
        // residual-bugs-v1 (P15.AGG14-7-cascade): pass the resolved
        // language to the walker so JS/TS source under `src/build/` or
        // `packages/x/dist/` is preserved (mirrors the per-language gate
        // in `crates/tldr-core/src/callgraph/scanner.rs`). Without this
        // hint, `tldr dead /tmp/repos/ts-dom-gen` returned
        // `functions_analyzed: 0` because the walker silently skipped
        // `src/build/`, where the entire authored TypeScript surface
        // lives.
        let mut walker = ProjectWalker::new(path).lang_hint(language);
        if no_default_ignore {
            walker = walker.no_default_ignore();
        }
        for entry in walker.iter() {
            let file_path = entry.path();
            if file_path.is_file() {
                // M6: skip .d.ts declaration-only files
                if is_typescript_declaration_file(file_path) {
                    continue;
                }
                if let Some(ext_str) = file_path.extension().and_then(|e| e.to_str()) {
                    let dotted = format!(".{}", ext_str);
                    if extensions.contains(&dotted.as_str()) {
                        file_count += 1;
                        if file_count > MAX_FILES {
                            eprintln!(
                                "Warning: born-dead scan truncated at {} files in {}",
                                MAX_FILES,
                                path.display()
                            );
                            break;
                        }
                        if let Ok((tree, source, lang)) = parse_file(file_path) {
                            // Extract ModuleInfo from the parsed tree
                            if let Ok(mut info) =
                                extract_from_tree(&tree, &source, lang, file_path, Some(path))
                            {
                                // Tag functions with framework directive while we have the source
                                tag_directive_functions(&mut info, &source, file_path);
                                let rel_path = file_path
                                    .strip_prefix(path)
                                    .unwrap_or(file_path)
                                    .to_path_buf();
                                module_infos.push((rel_path, info));
                            }
                            // Count identifiers from the same parsed tree
                            let file_counts =
                                count_identifiers_in_tree(&tree, source.as_bytes(), lang);
                            for (name, count) in file_counts {
                                *merged_counts.entry(name).or_insert(0) += count;
                            }
                        }
                    }
                }
            }
        }
    }

    (module_infos, merged_counts)
}

/// Wrapper struct for JSON output with truncation metadata.
///
/// low-cleanup-bundle-v1 (L5): the previous shape redundantly carried three
/// near-identical counters (`total_dead == total_count == shown_count` on
/// the un-truncated case). We dropped `total_count` (duplicate of the
/// canonical `total_dead` in `DeadCodeReport`) and `shown_count` (always
/// derivable from `dead_functions.len()`), keeping only the boolean
/// `truncated` flag for the rare case the list was clipped by --max-items.
#[derive(Serialize)]
struct DeadCodeOutput {
    #[serde(flatten)]
    report: DeadCodeReport,
    #[serde(skip_serializing_if = "is_false", default)]
    truncated: bool,
}

fn is_false(b: &bool) -> bool {
    !b
}

/// Apply truncation to the report based on max_items.
fn apply_truncation(
    mut report: DeadCodeReport,
    max_items: usize,
) -> (DeadCodeReport, bool, usize, usize) {
    let total_count = report.dead_functions.len();

    if total_count > max_items {
        report.dead_functions.truncate(max_items);
        // Also truncate by_file to match
        let mut count = 0;
        let mut new_by_file = std::collections::HashMap::new();
        for (path, funcs) in report.by_file {
            let remaining = max_items - count;
            if remaining == 0 {
                break;
            }
            let to_take = funcs.len().min(remaining);
            let truncated_funcs: Vec<String> = funcs.into_iter().take(to_take).collect();
            count += truncated_funcs.len();
            new_by_file.insert(path, truncated_funcs);
        }
        report.by_file = new_by_file;
        (report, true, total_count, max_items)
    } else {
        (report, false, total_count, total_count)
    }
}

/// Format dead code report with optional truncation notice.
fn format_dead_code_text_truncated(
    report: &DeadCodeReport,
    truncated: bool,
    total_count: usize,
    shown_count: usize,
) -> String {
    use colored::Colorize;

    let mut output = String::new();

    output.push_str(&format!(
        "Dead Code Analysis\n\nDefinitely dead: {} / {} functions ({:.1}% dead)\n",
        report.total_dead.to_string().red(),
        report.total_functions,
        report.dead_percentage
    ));

    if report.total_possibly_dead > 0 {
        output.push_str(&format!(
            "Possibly dead (public but uncalled): {}\n",
            report.total_possibly_dead.to_string().yellow()
        ));
    }

    output.push('\n');

    if !report.by_file.is_empty() {
        output.push_str("Definitely dead:\n");
        for (file, funcs) in &report.by_file {
            output.push_str(&format!("{}\n", file.display().to_string().green()));
            for func in funcs {
                output.push_str(&format!("  - {}\n", func.red()));
            }
            output.push('\n');
        }
    }

    if truncated {
        output.push_str(&format!(
            "\n[{}: showing {} of {} dead functions]\n",
            "TRUNCATED".yellow(),
            shown_count,
            total_count
        ));
    }

    output
}

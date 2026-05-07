//! Impact command - Show impact analysis
//!
//! Finds all callers of a function (reverse call graph traversal).
//! Supports `--type-aware` flag for Python type resolution (Phase 7-8).
//! Auto-routes through daemon when available for ~35x speedup.

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;

use tldr_core::analysis::references::{ReferenceKind, ReferencesOptions};
use tldr_core::types::{CallerTree, ImpactReport};
use tldr_core::{
    build_project_call_graph, extract_file, find_references, impact_analysis_with_ast_fallback,
    Language,
};

use crate::commands::daemon_router::{params_with_func_depth, try_daemon_route};
use crate::output::{format_impact_dot, format_impact_text, OutputFormat, OutputWriter};
use crate::path_validation::require_directory;

/// Analyze impact of changing a function
#[derive(Debug, Args)]
pub struct ImpactArgs {
    /// Function name to analyze
    pub function: String,

    /// Project root directory (default: current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Programming language
    #[arg(long, short = 'l')]
    pub lang: Option<Language>,

    /// Maximum traversal depth
    #[arg(long, short = 'd', default_value = "5")]
    pub depth: usize,

    /// Filter by file path
    #[arg(long)]
    pub file: Option<PathBuf>,

    /// Enable type-aware method resolution (resolves self.method() to ClassName.method)
    #[arg(long)]
    pub type_aware: bool,
}

impl ImpactArgs {
    /// Run the impact command
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let writer = OutputWriter::new(format, quiet);

        // Validate path exists AND is a directory BEFORE language detection
        // / progress banner (lang-detect-default-v1).
        // cli-error-clarity-v2 (P2.BUG-4): reject files with a clear message
        // instead of saying "Path not found" or letting downstream surface
        // cryptic IO errors.
        require_directory(&self.path, "impact")?;

        // Determine language (auto-detect from directory, default to Python)
        let language = self
            .lang
            .unwrap_or_else(|| Language::from_directory(&self.path).unwrap_or(Language::Python));

        let type_aware_msg = if self.type_aware { " (type-aware)" } else { "" };

        // Try daemon first for cached result
        if let Some(report) = try_daemon_route::<ImpactReport>(
            &self.path,
            "impact",
            params_with_func_depth(&self.function, Some(self.depth)),
        ) {
            // Output based on format
            if writer.is_text() {
                let text = format_impact_text(&report, self.type_aware);
                writer.write_text(&text)?;
                return Ok(());
            } else if writer.is_dot() {
                // surface-gaps-v1 (BUG-19): DOT impact graph (reverse calls).
                let dot = format_impact_dot(&report);
                writer.write_text(&dot)?;
                return Ok(());
            } else {
                writer.write(&report)?;
                return Ok(());
            }
        }

        // Fallback to direct compute
        writer.progress(&format!(
            "Building call graph for {} ({:?}){}...",
            self.path.display(),
            language,
            type_aware_msg
        ));

        // Build call graph first
        let graph = build_project_call_graph(&self.path, language, None, true)?;

        writer.progress(&format!(
            "Analyzing impact of {}{}...",
            self.function, type_aware_msg
        ));

        // Run impact analysis with AST fallback for isolated functions
        // TODO: When type_aware is true, use type-aware call graph building
        // For now, this flag is registered but type resolution is pending full implementation
        let mut report = impact_analysis_with_ast_fallback(
            &graph,
            &self.function,
            self.depth,
            self.file.as_deref(),
            &self.path,
            language,
        )?;

        // language-adapter-fixes-v1 (P13.AGG13-4): for languages whose call
        // graph builder under-reports cross-file edges (notably C# field-typed
        // method calls, Kotlin/Scala/OCaml functor wrappers), the call graph
        // alone leaves `caller_count = 0` even when `tldr explain` and
        // `tldr references` find call sites. Mirror the same fallback explain
        // uses (P12.AGG12-1) so `impact` agrees with `explain`/`references`.
        enrich_targets_with_references(&mut report, &self.path, &self.function, language);

        // If type-aware was requested, add placeholder stats to indicate it's enabled
        // (actual type resolution is integrated in callgraph builder - Phase 8 full implementation)
        if self.type_aware {
            report.type_resolution = Some(tldr_core::types::TypeResolutionStats {
                enabled: true,
                resolved_high_confidence: 0,
                resolved_medium_confidence: 0,
                fallback_used: 0,
                total_call_sites: 0,
            });
        }

        // Output based on format
        if writer.is_text() {
            let text = format_impact_text(&report, self.type_aware);
            writer.write_text(&text)?;
        } else if writer.is_dot() {
            // surface-gaps-v1 (BUG-19): direct-compute DOT impact path.
            let dot = format_impact_dot(&report);
            writer.write_text(&dot)?;
        } else {
            writer.write(&report)?;
        }

        Ok(())
    }
}

/// Enrich `ImpactReport.targets` by running `find_references` for the target
/// function and appending each unique enclosing-caller pair (file, function)
/// that the call graph missed.
///
/// language-adapter-fixes-v1 (P13.AGG13-4): the C# call-graph builder does
/// not always emit edges for field-typed receiver method calls
/// (e.g. `_writer.WriteToken(_root)` where `_writer` is a private field of
/// type `BsonBinaryWriter`). `tldr explain` works around this via
/// `enrich_with_references` (P12.AGG12-1). `tldr impact` had no equivalent,
/// so it kept reporting `caller_count = 0` for the same functions.
///
/// This helper mirrors the explain enrichment surface:
///   1. Run `find_references` with `kinds=[Call]` for `function`.
///   2. For each call site, locate the enclosing function in the call site's
///      file by parsing the file with `extract_file` and finding the
///      function whose `[line_number, line_end]` range contains the call.
///   3. For each target tree (impact may have one or more entries when the
///      function name is not unique), append a top-level synthetic caller
///      entry per unique (caller_function, caller_file) pair, deduplicated
///      against the existing children of that target tree.
fn enrich_targets_with_references(
    report: &mut ImpactReport,
    project_root: &Path,
    function: &str,
    language: Language,
) {
    if report.targets.is_empty() {
        return;
    }

    let mut options = ReferencesOptions::new();
    options.kinds = Some(vec![ReferenceKind::Call]);
    options.language = Some(language.as_str().to_string());
    options.limit = Some(500);

    let refs_report = match find_references(function, project_root, &options) {
        Ok(r) => r,
        Err(_) => return,
    };

    // Cache to avoid re-parsing the same caller file multiple times.
    use std::collections::HashMap;
    let mut file_funcs_cache: HashMap<PathBuf, Vec<(String, u32, u32)>> = HashMap::new();

    // Build the deduplicated set of (caller_func, caller_file) pairs to
    // append. Filter out self-references (a call inside the target function
    // in the same file) so the enrichment never invents a self-recursion
    // edge that the call graph deliberately did not emit.
    let mut additions: Vec<(String, PathBuf, u32)> = Vec::new();
    for r in &refs_report.references {
        let caller_file = r.file.clone();
        let funcs = file_funcs_cache
            .entry(caller_file.clone())
            .or_insert_with(|| collect_functions_with_bounds(&caller_file));
        let enclosing = funcs
            .iter()
            .find(|(_, start, end)| {
                let line = r.line as u32;
                line >= *start && (*end == 0 || line <= *end)
            })
            .map(|(name, _, _)| name.clone())
            .unwrap_or_else(|| "<module>".to_string());

        // Skip self-references (caller name matches `function` AND caller
        // file matches one of the target files we'd be enriching).
        let is_self = report.targets.values().any(|tree| {
            paths_equivalent(&tree.file, project_root, &caller_file)
                && (enclosing == function
                    || last_segment_eq(&enclosing, function))
        });
        if is_self {
            continue;
        }

        let key_pair = (enclosing.clone(), caller_file.clone());
        if additions
            .iter()
            .any(|(n, f, _)| n == &key_pair.0 && f == &key_pair.1)
        {
            continue;
        }
        additions.push((enclosing, caller_file, r.line as u32));
    }

    if additions.is_empty() {
        return;
    }

    // Append additions to each target tree, deduped against existing
    // direct callers.
    for tree in report.targets.values_mut() {
        for (name, file, line) in &additions {
            let already_present = tree.callers.iter().any(|c| {
                &c.function == name
                    && paths_equivalent(&c.file, project_root, file)
            });
            if already_present {
                continue;
            }
            tree.callers.push(CallerTree {
                function: name.clone(),
                file: file.clone(),
                caller_count: 0,
                callers: vec![],
                truncated: false,
                note: Some(format!(
                    "Discovered via references at line {} (call graph missing edge)",
                    line
                )),
                confidence: None,
                receiver_type: None,
            });
            tree.caller_count = tree.callers.len();
            // Replace the "Entry point" note when we now have callers.
            if let Some(n) = &tree.note {
                if n.contains("Entry point") || n.contains("no callers") {
                    tree.note = Some(
                        "caller_count derived from references enrichment (call graph missing cross-file edges)"
                            .to_string(),
                    );
                }
            }
        }
    }
}

/// Path-aware equality used by the references enrichment.
///
/// `find_references` returns absolute paths; `ImpactReport.targets[].file`
/// is whatever the call-graph emitted (usually project-relative). Compare
/// by canonicalised form, with the project-relative variant relative to
/// `project_root`.
fn paths_equivalent(a: &Path, project_root: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    let ca = a
        .canonicalize()
        .or_else(|_| project_root.join(a).canonicalize())
        .ok();
    let cb = b
        .canonicalize()
        .or_else(|_| project_root.join(b).canonicalize())
        .ok();
    match (ca, cb) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// Trailing-segment equality (after `.` or `::`). Used to detect self-
/// references that arrive with a class-qualified caller name (e.g.
/// `BsonBinaryWriter.WriteToken`) against an unqualified target.
fn last_segment_eq(qualified: &str, target: &str) -> bool {
    let last_dot = qualified.rfind('.');
    let last_cc = qualified.rfind("::").map(|i| i + 1);
    let cut = match (last_dot, last_cc) {
        (Some(d), Some(c)) => Some(d.max(c)),
        (Some(d), None) => Some(d),
        (None, Some(c)) => Some(c),
        (None, None) => None,
    };
    match cut {
        Some(i) if i + 1 < qualified.len() => &qualified[i + 1..] == target,
        _ => qualified == target,
    }
}

/// Collect `(function_name, line_start, line_end)` triples for every
/// top-level function and method in `file`. Mirrors the helper used by
/// `explain`'s reference enrichment so impact and explain locate
/// the same enclosing function for a given call site.
fn collect_functions_with_bounds(file: &Path) -> Vec<(String, u32, u32)> {
    let module = match extract_file(file, None) {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<(String, u32, u32)> = Vec::new();
    for f in &module.functions {
        out.push((f.name.clone(), f.line_number, f.line_end));
    }
    for class in &module.classes {
        for m in &class.methods {
            // Index both the bare method name and the qualified Class.method
            // form so the enclosing-function lookup matches whichever shape
            // `find_references` emits.
            out.push((m.name.clone(), m.line_number, m.line_end));
            out.push((
                format!("{}.{}", class.name, m.name),
                m.line_number,
                m.line_end,
            ));
        }
    }
    out
}

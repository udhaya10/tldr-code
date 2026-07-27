//! Vulnerability detection via taint analysis
//!
//! Detects security vulnerabilities by tracking data flow from sources (user input)
//! to sinks (dangerous functions) without proper sanitization.
//!
//! # Vulnerability Types
//!
//! - SQL Injection (CWE-89)
//! - XSS (CWE-79)
//! - Command Injection (CWE-78)
//! - Path Traversal (CWE-22)
//! - SSRF (CWE-918)
//!
//! # TIGER Mitigations
//!
//! - Timeout per file analysis
//!
//! # Example
//!
//! ```bash
//! tldr vuln src/
//! tldr vuln app.py --severity critical
//! tldr vuln app.py --vuln-type sql_injection
//! tldr vuln app.py --format sarif
//! ```

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use clap::Args;
use serde_json::{json, Value};
use tldr_core::walker::ProjectWalker;
use tldr_core::Language;

use super::error::RemainingError;
use super::types::{Severity, TaintFlow, VulnFinding, VulnReport, VulnSummary, VulnType};
use crate::output::OutputFormat;

// =============================================================================
// Constants
// =============================================================================

/// Maximum file size to analyze (10 MB).
///
/// Per-file safety cap for the parser: an oversized file can tie up
/// the tree-sitter parser or the line-scanner indefinitely. Unrelated
/// to the total file count (which is bounded structurally by the
/// walker's gitignore + default excludes rather than by a numeric
/// cap — the legacy `MAX_DIRECTORY_FILES = 1000` cap was removed in
/// VAL-006 because it silently truncated input on medium-to-large
/// repos).
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

// =============================================================================
// CLI Arguments
// =============================================================================

/// Analyze taint flows to detect security vulnerabilities
///
/// Tracks data flow from sources (user input) to sinks (dangerous functions)
/// to detect injection vulnerabilities like SQL injection, XSS, and command injection.
///
/// # Example
///
/// ```bash
/// tldr vuln src/
/// tldr vuln app.py --severity critical
/// tldr vuln app.py --format sarif
/// ```
#[derive(Debug, Args)]
pub struct VulnArgs {
    /// File or directory to analyze
    pub path: PathBuf,

    /// Programming language to filter by (auto-detected if omitted)
    #[arg(long, short = 'l')]
    pub lang: Option<Language>,

    /// Filter by minimum severity level
    #[arg(long)]
    pub severity: Option<Severity>,

    /// Filter by vulnerability type
    #[arg(long, value_name = "TYPE")]
    pub vuln_type: Option<Vec<VulnType>>,

    /// Include informational findings
    #[arg(long)]
    pub include_informational: bool,

    /// Include code-smell findings (e.g., per-`.unwrap()` Panic emissions on Rust files).
    /// Default: false (smells suppressed) to keep production-codebase JSON output focused on
    /// real security findings. Pass `--include-smells` to restore the legacy emission set.
    #[arg(long)]
    pub include_smells: bool,

    /// Include findings on JavaScript/TypeScript test files (paths under `test/`, `tests/`,
    /// `__tests__/`, or filenames ending in `.test.{js,ts,jsx,tsx}`, `.spec.{js,ts,jsx,tsx}`,
    /// or `.e2e.{js,ts}`). Default: false — test-file findings are suppressed because they
    /// exercise sink behavior on synthetic inputs and pollute production-codebase scans.
    /// Pass `--include-tests` to restore them.
    #[arg(long)]
    pub include_tests: bool,

    /// Output file (optional, stdout if not specified)
    #[arg(long, short = 'O')]
    pub output: Option<PathBuf>,

    /// Walk vendored/build dirs (node_modules, target, dist, etc.) that would normally be skipped.
    #[arg(long)]
    pub no_default_ignore: bool,
}

// =============================================================================
// Implementation
// =============================================================================

impl VulnArgs {
    /// Run the vuln command
    pub fn run(&self, format: OutputFormat) -> Result<()> {
        let start = Instant::now();

        // Validate path exists
        if !self.path.exists() {
            return Err(RemainingError::file_not_found(&self.path).into());
        }

        // Resolve the effective language. When the user passes
        // --lang <L> explicitly, honor it as-is (this is the VAL-001
        // contract: the user knows what they're asking for, even if
        // L lies outside the taint engine's native-analysis set).
        //
        // When --lang is omitted, consult Language::from_directory
        // (the VAL-002 detector: manifest-priority + extension
        // majority, skipping vendored trees). The detector returns
        // Some(L) for any recognised language or None for an empty
        // or unrecognised tree.
        //
        // VAL-006: if the autodetected language lies outside the
        // native-analysis set {Python, Rust}, error out early with
        // exit code 2 and a message that points the user at an
        // explicit --lang flag. This prevents the prior silent
        // behavior where `tldr vuln .` on a TypeScript repo would
        // report "0 files scanned" and exit 0 (misleading).
        //
        // The None case (empty/unrecognised tree) preserves the
        // historical empty-report-exit-0 behavior: the user ran the
        // command with no analyzable input; that's not an error.
        let effective_lang: Option<Language> = match self.lang {
            Some(l) => Some(l),
            None => {
                let detected = if self.path.is_dir() {
                    Language::from_directory(&self.path)
                } else {
                    Language::from_path(&self.path)
                };
                if let Some(l) = detected {
                    if !is_natively_analyzed(l) {
                        return Err(RemainingError::autodetect_unsupported(format!(
                            "vuln: taint analysis for {lang} is not yet supported by autodetect; \
                             pass --lang {lang} explicitly to scan this file (the canonical taint \
                             pipeline supports it). Autodetect-by-extension currently routes only \
                             --lang python, --lang rust, --lang typescript, and --lang javascript; \
                             other languages require an explicit --lang flag.",
                            lang = l.as_str()
                        ))
                        .into());
                    }
                }
                detected
            }
        };

        // Collect files to analyze
        let files = collect_files(&self.path, effective_lang, self.no_default_ignore)?;

        // Analyze all files
        let mut all_findings: Vec<VulnFinding> = Vec::new();
        let mut files_scanned: u32 = 0;
        let mut files_skipped: u32 = 0;
        let mut warnings: Vec<String> = Vec::new();

        for file_path in &files {
            // SECURE-UTF8-TOLERANCE-V1: classify non-UTF-8 inputs (e.g.
            // luau parser-test fixtures) before invoking `analyze_file`,
            // which uses strict `fs::read_to_string` and would otherwise
            // surface the failure as an opaque "file_not_found" via its
            // error mapping. Tolerant pre-check lets us emit a structured
            // warning + bump `files_skipped` while still letting genuine
            // I/O failures fall through to the existing silent-skip path.
            match tldr_core::fs::read_to_string_tolerant(file_path) {
                Ok(tldr_core::fs::ReadOutcome::NonUtf8 { byte_offset }) => {
                    files_skipped += 1;
                    warnings.push(format!(
                        "Skipped {}: invalid UTF-8 at byte {}",
                        file_path.display(),
                        byte_offset
                    ));
                    files_scanned += 1;
                    continue;
                }
                _ => {
                    // Either a clean read or an I/O error — defer to the
                    // existing analyze_file path, which already silently
                    // skips on Err().
                }
            }
            if let Ok(findings) = analyze_file(file_path) {
                for finding in findings {
                    all_findings.push(finding);
                }
            }
            files_scanned += 1;
        }

        // Apply filters
        let mut filtered_findings = all_findings;

        // Filter by severity
        if let Some(min_severity) = &self.severity {
            filtered_findings.retain(|f| f.severity.order() <= min_severity.order());
        }

        // Filter by vuln type
        if let Some(types) = &self.vuln_type {
            filtered_findings.retain(|f| types.contains(&f.vuln_type));
        }

        // Filter informational
        if !self.include_informational {
            filtered_findings.retain(|f| f.severity != Severity::Info);
        }

        // Filter code-smell findings (e.g., per-`.unwrap()` Panic emissions
        // from analyze_rust_file's line scanner). Hardening per
        // rust-panic-suppression-v1: suppress smell-class noise by default
        // on production codebases; opt-in via `--include-smells` to restore
        // legacy emission. Predicate is title-prefix bound to the
        // line-scanner's exact emission shape ("Potential Panic From
        // unwrap()") AND vuln_type-bound to Panic, so it cannot
        // accidentally over-match a future canonical-pipeline Panic
        // finding with a different title.
        if !self.include_smells {
            filtered_findings.retain(|f| !is_smell_finding(f));
        }

        // Filter JS/TS test-file findings (js-test-file-suppression-v1).
        // Mirrors the Rust `is_rust_test_file` mask in `analyze_rust_file`,
        // applied at the post-analysis filter layer here so it only suppresses
        // FINDINGS (not file collection) — preserving the unit-test fixtures
        // that the canonical taint engine itself relies on for self-tests.
        // Predicate is JS/TS-only (extension-bound) and requires a recognised
        // test-path component or test-style filename suffix; fixture paths
        // under `fixtures/` are exempted so the vuln_migration_v1 suite's
        // 168/168 RED stays GREEN.
        if !self.include_tests {
            filtered_findings.retain(|f| !is_js_test_file(Path::new(&f.file)));
        }

        // analysis-precision-v1, BUG-10: sort findings by (file, line,
        // vuln_type) ascending in ONE place — post-suppression, pre-output —
        // so JSON, text, and SARIF emitters all enumerate findings in the
        // same order. Pre-fix the JSON output preserved analyzer-emission
        // order while the text formatter walked the same vector — but the
        // ordering was non-deterministic across runs (rayon-driven file
        // analysis fan-out) and visibly differed between runs on the same
        // repo, creating the illusion of different findings between
        // `--format json` and `--format text`.
        filtered_findings
            .sort_by(|a, b| (&a.file, a.line, a.vuln_type).cmp(&(&b.file, b.line, b.vuln_type)));

        // schema-cleanup-v2 (P2.BUG-9): resolve the enclosing function for
        // each finding's `(file, line)`. Pre-fix, vuln findings had no
        // `function` field, so users could not pipe vuln output into
        // `tldr taint <file> <function>` or `tldr slice <file> <function>
        // <line>` without manually scanning the source for the enclosing
        // def. Post-fix, the enrichment runs after sort/filter so the
        // `extract_file` AST pass executes once per unique file across
        // surviving findings (rather than once per finding) — keeps the
        // additive cost ~linear in the number of distinct files. None is
        // assigned for findings whose line is at module scope or whose
        // file fails to parse (graceful degradation: a missing
        // `function` field does not invalidate the rest of the finding).
        enrich_with_enclosing_function(&mut filtered_findings);

        // Build summary.
        //
        // vuln-summary-correctness-v1 (Bug 1 + Bug 2): `files_with_vulns`
        // is computed AFTER all filters (severity, vuln_type,
        // informational, smells, test-files) by collecting unique
        // `file` values from the post-filter `filtered_findings` slice.
        // Pre-fix the counter was populated during raw analysis and
        // could exceed both `total_findings` (because it incremented
        // per finding-event rather than once-per-unique-file) AND the
        // count of unique files in the post-filter findings (because
        // suppressed findings still left their file in the set).
        // Post-fix invariant: `files_with_vulns <= total_findings`,
        // and `files_with_vulns == 0` whenever `total_findings == 0`.
        let unique_files_with_vulns: HashSet<&str> =
            filtered_findings.iter().map(|f| f.file.as_str()).collect();
        let summary = build_summary(&filtered_findings, unique_files_with_vulns.len() as u32);

        // Build report
        let report = VulnReport {
            findings: filtered_findings.clone(),
            summary: Some(summary),
            scan_duration_ms: start.elapsed().as_millis() as u64,
            files_scanned,
            files_skipped,
            warnings,
        };

        // Output
        let output_str = match format {
            OutputFormat::Sarif => {
                let sarif = generate_sarif(&report);
                serde_json::to_string_pretty(&sarif)?
            }
            OutputFormat::Text => format_vuln_text(&report),
            _ => serde_json::to_string_pretty(&report)?,
        };

        if let Some(ref output_path) = self.output {
            fs::write(output_path, &output_str)?;
        } else {
            println!("{}", output_str);
        }

        // determinism-and-stderr-hygiene-v1 (BUG-1): a successful scan exits 0
        // regardless of whether findings were detected. Pre-fix the path
        // returned `Err(RemainingError::findings_detected(_))` whenever
        // `filtered_findings` was non-empty, which surfaced as
        // `Error: 1 findings detected` on stderr AND exit code 2 — both
        // "real error" signals that broke CI integrations (every passing
        // scan-with-findings looked like a tool failure) and contaminated
        // grammar (`1 findings`, plural form for a single finding). The
        // count is already conveyed by `summary.total_findings` in the
        // JSON / SARIF output, so consumers that want a non-zero exit
        // can branch on that. Aligns with `tldr secure`, which already
        // returned `Ok(())` on completion regardless of finding count
        // (see `crates/tldr-cli/src/commands/remaining/secure.rs` —
        // it exits 0 unconditionally on a successful scan).
        Ok(())
    }
}

// =============================================================================
// Function-Field Enrichment (schema-cleanup-v2 P2.BUG-9)
// =============================================================================

/// Resolve the enclosing function name for each finding via `extract_file`.
///
/// Iterates `findings` in-place and sets `f.function = Some(name)` for
/// every finding whose `f.line` falls inside a function or method body
/// extracted from `f.file`. Findings at module scope (no enclosing
/// function), findings on unparseable files, and findings on files
/// outside the supported language set leave `f.function = None`.
///
/// Performance: groups findings by file path so `extract_file` runs at
/// most once per unique file across the post-filter slice (the same
/// file can appear in multiple findings). Failures are swallowed —
/// missing function metadata is non-blocking; the rest of the finding
/// remains valid.
fn enrich_with_enclosing_function(findings: &mut [VulnFinding]) {
    use std::collections::HashMap;
    use tldr_core::ast::extract::extract_file;
    use tldr_core::types::ModuleInfo;

    // Group finding indices by their file path so `extract_file` runs
    // at most once per unique file. Iteration over `findings` preserves
    // the existing post-sort order; we mutate by indexed lookup.
    let mut by_file: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, f) in findings.iter().enumerate() {
        by_file.entry(f.file.clone()).or_default().push(i);
    }

    for (file_str, indices) in by_file {
        let path = Path::new(&file_str);
        // Best-effort extraction; on parse error, leave function = None
        // for every finding on this file (graceful degradation).
        let module: ModuleInfo = match extract_file(path, None) {
            Ok(m) => m,
            Err(_) => continue,
        };
        for idx in indices {
            let line = findings[idx].line;
            findings[idx].function = lookup_enclosing_function(&module, line);
        }
    }
}

/// Walk the module's top-level functions and class methods, returning
/// the innermost (smallest-range) function whose `[line_number,
/// line_end]` window contains `line`. Returns None when no function
/// brackets the line (module-level finding) or when the AST extractor
/// produced 0-valued ranges (legacy extractors that skipped `line_end`).
fn lookup_enclosing_function(module: &tldr_core::types::ModuleInfo, line: u32) -> Option<String> {
    let mut best: Option<(u32, String)> = None; // (range_size, name)

    let mut consider = |start: u32, end: u32, name: &str| {
        // line_end = 0 means the extractor did not populate the range
        // (legacy construction). Skip — we cannot judge containment.
        if end == 0 || start == 0 {
            return;
        }
        if line < start || line > end {
            return;
        }
        let range = end.saturating_sub(start);
        match &best {
            None => best = Some((range, name.to_string())),
            Some((cur_range, _)) => {
                if range < *cur_range {
                    best = Some((range, name.to_string()));
                }
            }
        }
    };

    for f in &module.functions {
        consider(f.line_number, f.line_end, &f.name);
    }
    for c in &module.classes {
        for m in &c.methods {
            consider(m.line_number, m.line_end, &m.name);
        }
    }

    best.map(|(_, name)| name)
}

// =============================================================================
// File Collection
// =============================================================================

/// Collect supported source files to analyze.
fn collect_files(
    path: &Path,
    lang: Option<Language>,
    no_default_ignore: bool,
) -> Result<Vec<PathBuf>, RemainingError> {
    let mut files = Vec::new();

    if path.is_file() {
        // Single file - check size
        let metadata = fs::metadata(path).map_err(|_| RemainingError::file_not_found(path))?;
        if metadata.len() > MAX_FILE_SIZE {
            return Err(RemainingError::file_too_large(path, metadata.len()));
        }
        files.push(path.to_path_buf());
    } else if path.is_dir() {
        // Directory - walk and collect supported source files. The
        // walker is bounded structurally (honors .gitignore and
        // default vendor/build excludes from VAL-001); no numeric
        // file-count cap here, since that silently truncated input
        // on medium-to-large repos (VAL-006).
        let mut walker = ProjectWalker::new(path).max_depth(10);
        if no_default_ignore {
            walker = walker.no_default_ignore();
        }
        for entry in walker.iter() {
            let entry_path = entry.path();
            if entry_path.is_file() && is_supported_source_file(entry_path, lang) {
                // Per-file size cap — an oversized file can stall
                // the parser, but the total file count is not
                // capped.
                if let Ok(metadata) = fs::metadata(entry_path) {
                    if metadata.len() <= MAX_FILE_SIZE {
                        files.push(entry_path.to_path_buf());
                    }
                }
            }
        }
    }

    Ok(files)
}

/// The languages for which the vuln command has a native, dedicated
/// taint-analysis path.
///
/// - Python: tree-sitter-driven intra-procedural taint tracker in
///   `analyze_python_file` + `analyze_node` / `analyze_function`.
/// - Rust: line-scanning unsafe-pattern detector in `analyze_rust_file`.
///
/// Other languages (JS/TS, Go, Java, C, C++, Ruby, Kotlin, Swift,
/// C#, Scala, PHP, Lua/Luau, Elixir, OCaml) fall through to the
/// pattern-based scanner in `tldr_core::security::vuln` — those are
/// meaningful but weaker than the dedicated paths. VAL-006 draws the
/// autodetect-supported set at the native paths so `tldr vuln .`
/// without `--lang` on a non-Python/Rust tree surfaces an explicit
/// error rather than silently delivering weaker analysis.
///
/// An explicit `--lang <L>` bypasses this — the user has signalled
/// they understand which backend will run.
pub(super) fn is_natively_analyzed(lang: Language) -> bool {
    // ux-and-explain-completeness-v1 (P12.AGG12-16): the taint pattern
    // dispatch at `crates/tldr-core/src/security/taint.rs:764` covers
    // ALL 18 supported languages (Python, TypeScript, JavaScript, Go,
    // Java, Rust, C, Cpp, Ruby, Kotlin, Swift, CSharp, Scala, Php,
    // Lua/Luau, Elixir, OCaml). The autodetect gate had been frozen
    // at the four langs the canonical taint engine first shipped for
    // (Python/Rust + the v0.2.2-hotfix-bundle TS/JS promotion), which
    // forced users on every other language to pass `--lang <L>`
    // explicitly even though the pipeline supports them. Bring the
    // gate in lockstep with the engine — `tldr vuln /tmp/repos/<any>`
    // now runs cleanly without `--lang`.
    matches!(
        lang,
        Language::Python
            | Language::Rust
            | Language::TypeScript
            | Language::JavaScript
            | Language::Go
            | Language::Java
            | Language::C
            | Language::Cpp
            | Language::Ruby
            | Language::Kotlin
            | Language::Swift
            | Language::CSharp
            | Language::Scala
            | Language::Php
            | Language::Lua
            | Language::Luau
            | Language::Elixir
            | Language::Ocaml
    )
}

/// Check whether `path` is a source file the vuln scanner should analyze.
///
/// With `lang = Some(L)`, only files matching that language's extensions
/// are accepted. With `lang = None`, we fall back to the historical
/// behavior of `py | rs` (the extensions the taint engine natively
/// supports before multi-language dispatch).
fn is_supported_source_file(path: &Path, lang: Option<Language>) -> bool {
    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) => e,
        None => return false,
    };
    match lang {
        Some(Language::TypeScript) => matches!(ext, "ts" | "tsx"),
        Some(Language::JavaScript) => matches!(ext, "js" | "mjs" | "cjs" | "jsx"),
        Some(Language::Python) => ext == "py",
        Some(Language::Rust) => ext == "rs",
        Some(Language::Go) => ext == "go",
        Some(Language::Java) => ext == "java",
        Some(Language::C) => matches!(ext, "c" | "h"),
        Some(Language::Cpp) => matches!(ext, "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx"),
        Some(Language::CSharp) => ext == "cs",
        Some(Language::Ruby) => ext == "rb",
        Some(Language::Php) => ext == "php",
        Some(Language::Kotlin) => matches!(ext, "kt" | "kts"),
        Some(Language::Swift) => ext == "swift",
        Some(Language::Scala) => ext == "scala",
        Some(Language::Elixir) => matches!(ext, "ex" | "exs"),
        Some(Language::Lua) => ext == "lua",
        Some(Language::Luau) => ext == "luau",
        Some(Language::Ocaml) => matches!(ext, "ml" | "mli"),
        // No --lang: preserve historical behavior of scanning py + rs
        // (the two languages the taint analyzer natively handles).
        None => matches!(ext, "py" | "rs"),
    }
}

// =============================================================================
// File Analysis
// =============================================================================

/// Analyze a single file for vulnerabilities.
///
/// VULN-MIGRATION-V1 M4: the Python branch (formerly a CLI-local
/// tree-sitter walker over `analyze_python_file` + 9 helpers + a
/// `TaintTracker` + per-language source/sink const tables) was
/// collapsed onto the canonical `tldr_core::security::vuln::
/// scan_vulnerabilities` path. Post-M4, every extension EXCEPT `.rs`
/// flowed through the canonical per-function `compute_taint_with_tree`
/// dispatch that handles all 16 supported languages uniformly.
///
/// RUST-VULN-TAINT-PIPELINE-V1 M2 (Reframe C closure): post-M2, `.rs`
/// files run the canonical `scan_vulnerabilities` pipeline AND the
/// line-scanner `analyze_rust_file`, with domain-aware dedup on
/// `(line, VulnType)` tuples for the overlapping `SqlInjection` /
/// `CommandInjection` categories. The legacy "Rust files emit smell
/// findings only" implicit contract is retired. The line scanner
/// continues to emit the 3 Rust-specific smell variants
/// (UnsafeCode, MemorySafety, Panic) plus its narrow SqlInjection /
/// CommandInjection patterns; the canonical pipeline emits the 6
/// base taint VulnTypes (SqlInjection, Xss, CommandInjection,
/// PathTraversal, Ssrf, Deserialization). `dedupe_overlap` drops
/// line-scanner SqlInjection/CommandInjection findings on
/// `(line, VulnType)` tuples already covered by canonical —
/// preserving smell-only findings unconditionally and keeping unique
/// line-scanner emissions.
fn analyze_file(path: &Path) -> Result<Vec<VulnFinding>, RemainingError> {
    let source = fs::read_to_string(path).map_err(|_| RemainingError::file_not_found(path))?;
    let is_rust = matches!(path.extension().and_then(|e| e.to_str()), Some("rs"));

    // Canonical per-language taint pipeline runs for ALL extensions, including .rs
    // (RUST-VULN-TAINT-PIPELINE-V1 M2 dispatch flip — closes Reframe C).
    let mut findings: Vec<VulnFinding> =
        match tldr_core::security::vuln::scan_vulnerabilities(path, None, None) {
            Ok(report) => report
                .findings
                .into_iter()
                .map(|f| {
                    let vuln_type = map_core_vuln_type(f.vuln_type);
                    let severity = match f.severity.to_uppercase().as_str() {
                        "CRITICAL" => Severity::Critical,
                        "HIGH" => Severity::High,
                        "MEDIUM" => Severity::Medium,
                        "LOW" => Severity::Low,
                        _ => Severity::Medium,
                    };
                    let file_str = f.file.display().to_string();
                    // M3 detection-accuracy-v1 BUG-17: when the source and
                    // sink collapse to the same statement (same file + line
                    // + expression text), emit a single-element taint_flow
                    // tagged `direct_sink: true` rather than two duplicate
                    // entries. Pre-fix consumers saw the SAME code snippet
                    // twice with different "Source:" / "Sink:" labels, which
                    // misrepresents the dataflow as a multi-step propagation
                    // when in reality it's a single direct invocation
                    // (e.g. `let file = File::open(path)?` — path is
                    // tainted, File::open is the sink, no propagation).
                    let is_degenerate = f.source.line == f.sink.line
                        && f.source.expression == f.sink.expression
                        && !f.source.expression.is_empty();
                    let taint_flow: Vec<TaintFlow> = if is_degenerate {
                        vec![TaintFlow {
                            file: file_str.clone(),
                            line: f.sink.line,
                            column: 0,
                            code_snippet: f.sink.expression.clone(),
                            description: format!(
                                "Direct sink: {} (source: {})",
                                f.sink.sink_type, f.source.source_type
                            ),
                        }]
                    } else {
                        vec![
                            TaintFlow {
                                file: file_str.clone(),
                                line: f.source.line,
                                column: 0,
                                code_snippet: f.source.expression.clone(),
                                description: format!("Source: {}", f.source.source_type),
                            },
                            TaintFlow {
                                file: file_str.clone(),
                                line: f.sink.line,
                                column: 0,
                                code_snippet: f.sink.expression.clone(),
                                description: format!("Sink: {}", f.sink.sink_type),
                            },
                        ]
                    };
                    VulnFinding {
                        vuln_type,
                        severity,
                        cwe_id: f.cwe_id.unwrap_or_default(),
                        title: format!("{:?}", f.vuln_type),
                        description: format!("{} with unsanitized input", f.sink.sink_type),
                        file: file_str,
                        line: f.sink.line,
                        column: 0,
                        // schema-cleanup-v2 (P2.BUG-9): populated below in
                        // `run` via a single `extract_file` pass per file —
                        // see `enrich_with_enclosing_function`.
                        function: None,
                        taint_flow,
                        remediation: f.remediation.clone(),
                        confidence: 0.85,
                        direct_sink: is_degenerate,
                    }
                })
                .collect(),
            Err(_) => Vec::new(),
        };

    // For .rs additionally run the line scanner — emits the 3 Rust-specific
    // smell variants (UnsafeCode/MemorySafety/Panic) plus the 2 overlapping
    // categories (SqlInjection/CommandInjection) handled by `dedupe_overlap`.
    if is_rust {
        let mut line_findings = analyze_rust_file(path, &source);
        dedupe_overlap(&mut line_findings, &findings);
        findings.extend(line_findings);
    }
    Ok(findings)
}

/// Drop line-scanner findings whose `(line, vuln_type)` tuple is already
/// covered by a canonical finding. Applies only to vuln_type values shared
/// between both layers (`SqlInjection`, `CommandInjection`). Other
/// line-scanner-only types (`UnsafeCode`, `MemorySafety`, `Panic`, etc.)
/// are never dropped — no canonical analog exists.
///
/// RUST-VULN-TAINT-PIPELINE-V1 M2: predicate is `c.line == line_f.line &&
/// c.vuln_type == line_f.vuln_type`. Line-precision matters. Three cases:
/// (a) identical-line plus identical-vuln_type collapses to 1 finding
/// (canonical wins, since line-scanner finding is dropped). (b) same-line
/// plus different-vuln_type keeps both (e.g., line-scanner `UnsafeCode`
/// alongside canonical `CommandInjection` on the same line). (c) same-vuln_type
/// plus different-line keeps both (legitimate distinct sites).
fn dedupe_overlap(line_findings: &mut Vec<VulnFinding>, canonical: &[VulnFinding]) {
    line_findings.retain(|line_f| match line_f.vuln_type {
        VulnType::SqlInjection | VulnType::CommandInjection => !canonical
            .iter()
            .any(|c| c.vuln_type == line_f.vuln_type && c.line == line_f.line),
        _ => true,
    });
}

/// Map a `tldr_core::security::vuln::VulnType` to the CLI-side `VulnType`.
///
/// Pre-VAL-002 (issue #11), this site used a wildcard match arm that
/// silently relabeled every variant outside {SqlInjection, CommandInjection,
/// Xss, PathTraversal} as `SqlInjection`. That mislabeled `Deserialization`
/// and `Ssrf` findings — the user-facing symptom in #11 was Java
/// `ObjectInputStream.readObject()` findings being emitted as
/// `vuln_type: "sql_injection"` in JSON, and the SARIF rules array
/// disagreeing with `results[].ruleId` (rules: `CWE-89` from local
/// vuln_type, results.ruleId: `CWE-502` from the unmodified `cwe_id`),
/// producing an internally inconsistent SARIF document.
///
/// This match is deliberately exhaustive (no `_` arm). When tldr-core
/// adds a new `VulnType` variant in the future, this function fails
/// to compile until the new variant is mapped — preventing a
/// reintroduction of the wildcard mislabel.
fn map_core_vuln_type(core_ty: tldr_core::security::vuln::VulnType) -> VulnType {
    use tldr_core::security::vuln::VulnType as CoreVulnType;
    match core_ty {
        CoreVulnType::SqlInjection => VulnType::SqlInjection,
        CoreVulnType::Xss => VulnType::Xss,
        CoreVulnType::CommandInjection => VulnType::CommandInjection,
        CoreVulnType::PathTraversal => VulnType::PathTraversal,
        CoreVulnType::Ssrf => VulnType::Ssrf,
        CoreVulnType::Deserialization => VulnType::Deserialization,
        CoreVulnType::OpenRedirect => VulnType::OpenRedirect,
    }
}

pub(super) fn analyze_rust_file(path: &Path, source: &str) -> Vec<VulnFinding> {
    let file_path = path.display().to_string();
    let is_test_file = is_rust_test_file(path);
    let mut findings = Vec::new();
    let lines: Vec<&str> = source.lines().collect();
    let mut in_command_block = false;
    let mut command_block_start_line: u32 = 0;

    for (idx, line) in lines.iter().enumerate() {
        let line_number = (idx + 1) as u32;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }

        if (trimmed.contains("unsafe {") || trimmed.starts_with("unsafe{"))
            && !has_nearby_safety_comment(&lines, idx)
        {
            findings.push(rust_finding(
                VulnType::UnsafeCode,
                Severity::High,
                RustFindingMeta {
                    cwe_id: "CWE-242",
                    title: "Unsafe Block Without Safety Rationale",
                    description: "unsafe block found without nearby SAFETY: justification comment",
                },
                RustFindingLocation {
                    file: &file_path,
                    line: line_number,
                    column: trimmed.find("unsafe").unwrap_or(0) as u32,
                },
                "Document invariants with // SAFETY: ... or avoid unsafe when possible",
                0.80,
            ));
        }

        if trimmed.contains("std::mem::transmute(") || trimmed.contains("mem::transmute(") {
            findings.push(rust_finding(
                VulnType::MemorySafety,
                Severity::Critical,
                RustFindingMeta {
                    cwe_id: "CWE-119",
                    title: "Risky transmute Usage",
                    description:
                        "std::mem::transmute can violate type and memory safety guarantees",
                },
                RustFindingLocation {
                    file: &file_path,
                    line: line_number,
                    column: trimmed.find("transmute").unwrap_or(0) as u32,
                },
                "Prefer safe conversions (From/TryFrom, bytemuck) and explicit layout checks",
                0.90,
            ));
        }

        if trimmed.contains("std::ptr::")
            || trimmed.contains("core::ptr::")
            || trimmed.contains("ptr::read(")
            || trimmed.contains("ptr::write(")
        {
            findings.push(rust_finding(
                VulnType::MemorySafety,
                Severity::High,
                RustFindingMeta {
                    cwe_id: "CWE-119",
                    title: "Raw Pointer Operation",
                    description:
                        "raw pointer operation detected; verify lifetimes, aliasing, and bounds",
                },
                RustFindingLocation {
                    file: &file_path,
                    line: line_number,
                    column: trimmed.find("ptr::").unwrap_or(0) as u32,
                },
                "Use safe abstractions/slices where possible and document pointer invariants",
                0.85,
            ));
        }

        if !is_test_file && trimmed.contains(".unwrap()") {
            findings.push(rust_finding(
                VulnType::Panic,
                Severity::Medium,
                RustFindingMeta {
                    cwe_id: "CWE-703",
                    title: "Potential Panic From unwrap()",
                    description: "unwrap() in non-test Rust code can panic in production paths",
                },
                RustFindingLocation {
                    file: &file_path,
                    line: line_number,
                    column: trimmed.find(".unwrap()").unwrap_or(0) as u32,
                },
                "Handle Result/Option explicitly or use expect() with actionable context",
                0.70,
            ));
        }

        if trimmed.contains("format!(")
            && format_string_contains_sql_keyword(trimmed)
            && (trimmed.contains("{}") || trimmed.contains("{") || trimmed.contains("+"))
        {
            findings.push(rust_finding(
                VulnType::SqlInjection,
                Severity::Critical,
                RustFindingMeta {
                    cwe_id: "CWE-89",
                    title: "SQL String Interpolation",
                    description:
                        "SQL query appears to be built via string formatting/interpolation",
                },
                RustFindingLocation {
                    file: &file_path,
                    line: line_number,
                    column: trimmed.find("format!(").unwrap_or(0) as u32,
                },
                "Use parameterized queries via your DB client instead of format!/concatenation",
                0.88,
            ));
        }

        if trimmed.contains("from_utf8_unchecked(")
            || trimmed.contains(".as_bytes()[")
            || trimmed.contains(".as_bytes().get_unchecked(")
        {
            findings.push(rust_finding(
                VulnType::MemorySafety,
                Severity::High,
                RustFindingMeta {
                    cwe_id: "CWE-20",
                    title: "Unchecked Byte/String Conversion",
                    description:
                        "unchecked UTF-8 or byte indexing detected without visible validation",
                },
                RustFindingLocation {
                    file: &file_path,
                    line: line_number,
                    column: trimmed
                        .find("as_bytes")
                        .or_else(|| trimmed.find("from_utf8_unchecked"))
                        .unwrap_or(0) as u32,
                },
                "Validate lengths/UTF-8 before conversion or use checked APIs",
                0.82,
            ));
        }

        if trimmed.contains("Command::new(") || trimmed.contains("std::process::Command::new(") {
            in_command_block = true;
            command_block_start_line = line_number;
        }
        if in_command_block
            && trimmed.contains(".arg(")
            && !trimmed.contains(".arg(\"")
            && !trimmed.contains(".arg('")
        {
            findings.push(rust_finding(
                VulnType::CommandInjection,
                Severity::Critical,
                RustFindingMeta {
                    cwe_id: "CWE-78",
                    title: "Unsanitized Process Argument",
                    description: "Command argument appears to be variable-driven without visible sanitization",
                },
                RustFindingLocation {
                    file: &file_path,
                    line: command_block_start_line.max(line_number),
                    column: trimmed.find(".arg(").unwrap_or(0) as u32,
                },
                "Validate/allowlist user-controlled arguments before passing to Command",
                0.80,
            ));
        }
        if in_command_block && (trimmed.ends_with(';') || trimmed.contains(");")) {
            in_command_block = false;
            command_block_start_line = 0;
        }
    }

    findings
}

struct RustFindingMeta<'a> {
    cwe_id: &'a str,
    title: &'a str,
    description: &'a str,
}

struct RustFindingLocation<'a> {
    file: &'a str,
    line: u32,
    column: u32,
}

fn rust_finding(
    vuln_type: VulnType,
    severity: Severity,
    meta: RustFindingMeta<'_>,
    location: RustFindingLocation<'_>,
    remediation: &str,
    confidence: f64,
) -> VulnFinding {
    VulnFinding {
        vuln_type,
        severity,
        cwe_id: meta.cwe_id.to_string(),
        title: meta.title.to_string(),
        description: meta.description.to_string(),
        file: location.file.to_string(),
        line: location.line,
        column: location.column,
        // schema-cleanup-v2 (P2.BUG-9): populated by
        // `enrich_with_enclosing_function` in `VulnArgs::run`.
        function: None,
        taint_flow: Vec::new(),
        remediation: remediation.to_string(),
        confidence,
        direct_sink: false,
    }
}

fn has_nearby_safety_comment(lines: &[&str], index: usize) -> bool {
    let start = index.saturating_sub(2);
    (start..index).any(|i| lines[i].contains("SAFETY:"))
}

/// Narrowed SQL-keyword predicate for the `format!(...)` SqlInjection trigger
/// in `analyze_rust_file`.
///
/// Hardening per `rust-format-sql-fp-narrowing-v1` (closes a high-severity
/// false-positive class): the legacy predicate uppercased the WHOLE line and
/// substring-matched against {SELECT, INSERT, UPDATE, DELETE, FROM, WHERE},
/// causing false positives on Rust call sites whose method/function names
/// happened to contain a SQL keyword as a substring — most prominently
/// `char::from(...)` and `Box::<T>::from(format!(...))`, where the substring
/// `from(` uppercases to a substring matching the keyword `FROM`. Empirical
/// repro: `tldr vuln --lang rust /tmp/repos/ripgrep/crates` produced 4
/// critical-severity SqlInjection findings on `format!()` callsites with ZERO
/// SQL anywhere in the file (bash/fish/powershell flag formatting + an
/// `err!` macro `Box::<...>::from(format!(...))`).
///
/// The narrowed predicate:
/// 1. Extracts the format-string literal (first `"..."` argument to
///    `format!(`) — if no literal is present (e.g., `format!($($tt)*)`)
///    the predicate returns false (no SQL injection candidate).
/// 2. Applies a word-boundary uppercase substring check against the same
///    keyword set: a keyword matches only when adjacent characters on
///    both sides are non-alphanumeric/non-underscore (or string boundary).
///    This rejects `from(` matching `FROM` while still firing on
///    `"SELECT * FROM users WHERE id = {}"`.
///
/// Trade-off: this is a syntactic line-scanner predicate; a determined
/// attacker can still bypass it (e.g., `format!("{}{}", "SEL", "ECT * ...")`
/// — string concatenation across format args). The canonical taint pipeline
/// (`crates/tldr-core/src/security/...`) handles those cases via the
/// `taint_flow` graph; this predicate exists only to gate the line-scanner's
/// best-effort `format!`-shaped emission and SHOULD be tight to avoid the FP
/// floor that motivated this milestone.
fn format_string_contains_sql_keyword(line: &str) -> bool {
    let Some(literal) = extract_first_format_string_literal(line) else {
        return false;
    };
    let upper = literal.to_uppercase();
    let bytes = upper.as_bytes();
    const KEYWORDS: &[&str] = &["SELECT", "INSERT", "UPDATE", "DELETE", "FROM", "WHERE"];
    for kw in KEYWORDS {
        let kw_bytes = kw.as_bytes();
        let mut start = 0usize;
        while let Some(off) = upper[start..].find(kw) {
            let abs = start + off;
            let before_ok = abs == 0 || !is_word_byte(bytes[abs - 1]);
            let after_idx = abs + kw_bytes.len();
            let after_ok = after_idx >= bytes.len() || !is_word_byte(bytes[after_idx]);
            if before_ok && after_ok {
                return true;
            }
            start = abs + 1;
        }
    }
    false
}

/// Returns true if `b` is an ASCII word byte (letter, digit, or underscore).
/// Used by `format_string_contains_sql_keyword` to enforce keyword word
/// boundaries — rejects `from(` substring-matching `FROM` while preserving
/// `SELECT * FROM users` matching.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Extracts the first string-literal argument to a `format!(` call on `line`,
/// returning `Some(literal_content)` if found and `None` otherwise.
///
/// Handles `\"` escapes inside the literal (skips them so the closing quote
/// is not detected prematurely). Returns `None` when:
/// - `format!(` does not appear on the line, OR
/// - the first non-whitespace char after `format!(` is not `"` (e.g., the
///   `format!($($tt)*)` macro-pass-through case in `crates/ignore`'s `err!`
///   macro), OR
/// - the literal is unterminated (defensive — malformed source).
fn extract_first_format_string_literal(line: &str) -> Option<String> {
    let macro_pos = line.find("format!(")?;
    let after_paren = &line[macro_pos + "format!(".len()..];
    let mut chars = after_paren.char_indices();
    // Skip leading whitespace inside the macro-arg list.
    let (start_idx, start_ch) = loop {
        let (i, c) = chars.next()?;
        if !c.is_whitespace() {
            break (i, c);
        }
    };
    if start_ch != '"' {
        return None;
    }
    // Walk byte-wise from just past the opening quote, honoring `\` escapes.
    let body = &after_paren[start_idx + 1..];
    let mut out = String::new();
    let mut iter = body.chars();
    while let Some(c) = iter.next() {
        if c == '\\' {
            // Consume one escaped char (covers `\"`, `\\`, `\n`, etc.) and
            // preserve it literally — we do NOT need to interpret escapes
            // because the keyword search is uppercase-substring with word
            // boundaries; `\n` and `\t` count as non-word bytes either way.
            if let Some(next) = iter.next() {
                out.push(c);
                out.push(next);
            } else {
                return None;
            }
        } else if c == '"' {
            return Some(out);
        } else {
            out.push(c);
        }
    }
    None
}

/// Predicate: a Rust path is a test-file path.
///
/// Visibility: `pub(super)` so sibling modules in `commands::remaining`
/// (notably `secure.rs`, per `secure-test-file-suppression-v1`) can reuse
/// the same recognition logic and keep the vuln/secure suppression policies
/// in lock-step. Originally private to `vuln.rs`; promoted in M-Z10.
pub(super) fn is_rust_test_file(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    path_str.contains("/tests/")
        || path_str.contains("\\tests\\")
        || path_str.ends_with("_test.rs")
        || path_str.ends_with("tests.rs")
}

/// Predicate: a JavaScript/TypeScript path is a test-file path.
///
/// Mirrors `is_rust_test_file` for the JS/TS ecosystem. Used by the
/// `--include-tests` filter layer in `VulnArgs::run` to suppress findings
/// emitted from synthetic test fixtures by default; pass `--include-tests`
/// to restore them.
///
/// Recognition (BOTH conditions must hold):
///   1. File extension is `.js`, `.jsx`, `.ts`, `.tsx`, `.cjs`, or `.mjs`
///      (extension-bound to scope the predicate to JS/TS — Rust/Python/Java
///      test files are masked elsewhere via their own predicates).
///   2. EITHER the path contains a recognised test-path component
///      (`/test/`, `/tests/`, `/__tests__/`, or backslash equivalents)
///      OR the filename matches a recognised test-style suffix
///      (`.test.<ext>`, `.spec.<ext>`, `.e2e.<ext>`).
///
/// Exemption: paths containing `/fixtures/` (or backslash `\fixtures\`)
/// are NOT treated as test files. The `vuln_migration_v1` suite's
/// fixtures live under `crates/tldr-cli/tests/fixtures/vuln_migration_v1/`
/// — the `tests/` ancestor would otherwise trigger this predicate and
/// suppress every JS/TS positive fixture, breaking 168/168 RED.
///
/// Negative-case examples (NOT test files):
///   * `src/foo.js` — no test-path component, no test suffix.
///   * `lib/test_helper.js` — `test_helper` is not a recognised JS
///     test-suffix convention (`_test.js` is not idiomatic in JS the
///     way `_test.rs` is in Rust); the `test` substring inside the
///     filename does not match.
///   * `crates/tldr-cli/tests/fixtures/vuln_migration_v1/javascript/...js` —
///     fixtures exemption kicks in.
pub(super) fn is_js_test_file(path: &Path) -> bool {
    let path_str = path.to_string_lossy();

    // Extension gate: only JS/TS family files trigger this predicate.
    let ext_match = path_str.ends_with(".js")
        || path_str.ends_with(".jsx")
        || path_str.ends_with(".ts")
        || path_str.ends_with(".tsx")
        || path_str.ends_with(".cjs")
        || path_str.ends_with(".mjs");
    if !ext_match {
        return false;
    }

    // Fixture exemption: vuln_migration_v1 (and any future) test
    // suites that scan files UNDER a `fixtures/` directory must keep
    // emitting findings even though the path includes a `tests/`
    // ancestor.
    if path_str.contains("/fixtures/") || path_str.contains("\\fixtures\\") {
        return false;
    }

    // Test-path component check. Recognises both "embedded" matches
    // (`a/test/b.js`) and "leading" matches (`test/b.js` — relative
    // paths whose first component is the test dir).
    let has_test_path_component = path_str.contains("/test/")
        || path_str.contains("\\test\\")
        || path_str.starts_with("test/")
        || path_str.starts_with("test\\")
        || path_str.contains("/tests/")
        || path_str.contains("\\tests\\")
        || path_str.starts_with("tests/")
        || path_str.starts_with("tests\\")
        || path_str.contains("/__tests__/")
        || path_str.contains("\\__tests__\\")
        || path_str.starts_with("__tests__/")
        || path_str.starts_with("__tests__\\");

    // Test-style filename suffix check (suffixes are extension-prefixed
    // so `foo.test.js` matches but `foo.testimony.js` does not).
    let has_test_filename_suffix = path_str.ends_with(".test.js")
        || path_str.ends_with(".test.jsx")
        || path_str.ends_with(".test.ts")
        || path_str.ends_with(".test.tsx")
        || path_str.ends_with(".test.cjs")
        || path_str.ends_with(".test.mjs")
        || path_str.ends_with(".spec.js")
        || path_str.ends_with(".spec.jsx")
        || path_str.ends_with(".spec.ts")
        || path_str.ends_with(".spec.tsx")
        || path_str.ends_with(".spec.cjs")
        || path_str.ends_with(".spec.mjs")
        || path_str.ends_with(".e2e.js")
        || path_str.ends_with(".e2e.jsx")
        || path_str.ends_with(".e2e.ts")
        || path_str.ends_with(".e2e.tsx")
        || path_str.ends_with(".e2e.cjs")
        || path_str.ends_with(".e2e.mjs");

    has_test_path_component || has_test_filename_suffix
}

/// Predicate: a finding is classified as a code-smell (non-security) emission
/// from `analyze_rust_file`'s line scanner.
///
/// Currently the only smell-class trigger is the per-`.unwrap()` Panic
/// finding (T4 in analyze_rust_file). The predicate is intentionally tight:
/// vuln_type-bound to `VulnType::Panic` AND title-prefix-bound to
/// "Potential Panic" (the exact prefix of the line scanner's emission
/// title "Potential Panic From unwrap()"). The defensive title-prefix
/// guard prevents accidentally over-suppressing a hypothetical future
/// canonical-pipeline Panic finding with a different title.
///
/// Kept local to vuln.rs (grep-near `is_rust_test_file`) so future smell
/// triggers (if any are added) can be enumerated here in one place.
fn is_smell_finding(f: &VulnFinding) -> bool {
    f.vuln_type == VulnType::Panic && f.title.starts_with("Potential Panic")
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Get human-readable name for vulnerability type
fn vuln_type_name(vt: VulnType) -> &'static str {
    match vt {
        VulnType::SqlInjection => "SQL Injection",
        VulnType::Xss => "Cross-Site Scripting (XSS)",
        VulnType::CommandInjection => "Command Injection",
        VulnType::Ssrf => "Server-Side Request Forgery (SSRF)",
        VulnType::PathTraversal => "Path Traversal",
        VulnType::Deserialization => "Insecure Deserialization",
        VulnType::UnsafeCode => "Unsafe Code Risk",
        VulnType::MemorySafety => "Memory Safety Violation",
        VulnType::Panic => "Unchecked Panic Path",
        VulnType::Xxe => "XML External Entity (XXE)",
        VulnType::OpenRedirect => "Open Redirect",
        VulnType::LdapInjection => "LDAP Injection",
        VulnType::XpathInjection => "XPath Injection",
    }
}

/// Build summary statistics
fn build_summary(findings: &[VulnFinding], files_with_vulns: u32) -> VulnSummary {
    let mut by_severity: HashMap<String, u32> = HashMap::new();
    let mut by_type: HashMap<String, u32> = HashMap::new();

    for finding in findings {
        *by_severity.entry(finding.severity.to_string()).or_insert(0) += 1;
        // schema-unification-v1 Bug-02 fix: derive the by_type key from
        // VulnType's serde representation (snake_case via #[serde(rename_all)])
        // so the key matches the `.vuln_type` field on findings — not the
        // historical `format!("{:?}", _).to_lowercase()` which produced
        // "commandinjection" (no separator). serde_json::to_value of a
        // unit-variant enum returns a JSON string, so .as_str() succeeds.
        let key = serde_json::to_value(finding.vuln_type)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| format!("{:?}", finding.vuln_type).to_lowercase());
        *by_type.entry(key).or_insert(0) += 1;
    }

    VulnSummary {
        total_findings: findings.len() as u32,
        by_severity,
        by_type,
        files_with_vulns,
    }
}

// =============================================================================
// Output Formatting
// =============================================================================

/// Format report as human-readable text
fn format_vuln_text(report: &VulnReport) -> String {
    let mut out = String::new();

    out.push_str("=== Vulnerability Scan Results ===\n\n");

    if report.findings.is_empty() {
        out.push_str("No vulnerabilities found.\n");
    } else {
        out.push_str(&format!(
            "Found {} vulnerabilities:\n\n",
            report.findings.len()
        ));

        for (i, finding) in report.findings.iter().enumerate() {
            out.push_str(&format!(
                "{}. [{}] {} ({})\n",
                i + 1,
                finding.severity.to_string().to_uppercase(),
                finding.title,
                finding.cwe_id
            ));
            out.push_str(&format!("   File: {}:{}\n", finding.file, finding.line));
            out.push_str(&format!("   {}\n", finding.description));

            if !finding.taint_flow.is_empty() {
                out.push_str("   Taint Flow:\n");
                for (j, flow) in finding.taint_flow.iter().enumerate() {
                    out.push_str(&format!(
                        "     {}. {}:{} - {}\n",
                        j + 1,
                        flow.file,
                        flow.line,
                        flow.description
                    ));
                    if !flow.code_snippet.is_empty() {
                        out.push_str(&format!("        {}\n", flow.code_snippet.trim()));
                    }
                }
            }

            out.push_str(&format!("   Remediation: {}\n\n", finding.remediation));
        }
    }

    if let Some(summary) = &report.summary {
        out.push_str("=== Summary ===\n");
        out.push_str(&format!(
            "Total: {} vulnerabilities\n",
            summary.total_findings
        ));
        out.push_str(&format!(
            "Files with vulnerabilities: {}\n",
            summary.files_with_vulns
        ));

        if !summary.by_severity.is_empty() {
            out.push_str("By Severity:\n");
            for (sev, count) in &summary.by_severity {
                out.push_str(&format!("  {}: {}\n", sev, count));
            }
        }
    }

    out.push_str(&format!("\nScan duration: {}ms\n", report.scan_duration_ms));
    out.push_str(&format!("Files scanned: {}\n", report.files_scanned));

    out
}

/// Clamp a SARIF region coordinate (startLine / startColumn) to satisfy
/// the SARIF 2.1.0 §3.30.5 / §3.30.6 minimum-value requirement.
///
/// Per spec, both `startLine` and `startColumn` must be >= 1. Internal
/// `VulnFinding` / `TaintFlow` positions are stored as `u32` and may be
/// 0 (default-initialized when the upstream analyzer could not resolve
/// a precise column or — rarely — a precise line). GitHub code scanning
/// rejects SARIF with a value < 1, so we clamp at the emitter boundary.
/// Internal storage and JSON output formats are unaffected; only the
/// SARIF emitter applies the clamp. (vuln-summary-correctness-v1, Bug 3)
#[inline]
fn sarif_clamp_pos(value: u32) -> u32 {
    value.max(1)
}

/// Generate SARIF format output
fn generate_sarif(report: &VulnReport) -> Value {
    let results: Vec<Value> = report
        .findings
        .iter()
        .map(|f| {
            json!({
                "ruleId": f.cwe_id,
                "level": match f.severity {
                    Severity::Critical | Severity::High => "error",
                    Severity::Medium => "warning",
                    Severity::Low | Severity::Info => "note",
                },
                "message": {
                    "text": f.description
                },
                "locations": [{
                    "physicalLocation": {
                        "artifactLocation": {
                            "uri": f.file
                        },
                        "region": {
                            "startLine": sarif_clamp_pos(f.line),
                            "startColumn": sarif_clamp_pos(f.column)
                        }
                    }
                }],
                "codeFlows": if f.taint_flow.is_empty() { None } else {
                    Some(vec![{
                        json!({
                            "threadFlows": [{
                                "locations": f.taint_flow.iter().map(|tf| {
                                    json!({
                                        "location": {
                                            "physicalLocation": {
                                                "artifactLocation": {
                                                    "uri": tf.file
                                                },
                                                "region": {
                                                    "startLine": sarif_clamp_pos(tf.line),
                                                    "startColumn": sarif_clamp_pos(tf.column)
                                                }
                                            },
                                            "message": {
                                                "text": tf.description
                                            }
                                        }
                                    })
                                }).collect::<Vec<_>>()
                            }]
                        })
                    }])
                }
            })
        })
        .collect();

    let rules: Vec<Value> = report
        .findings
        .iter()
        .map(|f| &f.vuln_type)
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|vt| {
            json!({
                "id": vt.cwe_id(),
                "name": vuln_type_name(*vt),
                "shortDescription": {
                    "text": vuln_type_name(*vt)
                },
                "defaultConfiguration": {
                    "level": match vt.default_severity() {
                        Severity::Critical | Severity::High => "error",
                        Severity::Medium => "warning",
                        Severity::Low | Severity::Info => "note",
                    }
                }
            })
        })
        .collect();

    json!({
        "$schema": "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/master/Schemata/sarif-schema-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "tldr-vuln",
                    "version": env!("CARGO_PKG_VERSION"),
                    "informationUri": "https://github.com/tldr-code/tldr-rs",
                    "rules": rules
                }
            },
            "results": results
        }]
    })
}

// =============================================================================
// Unit Tests
// =============================================================================

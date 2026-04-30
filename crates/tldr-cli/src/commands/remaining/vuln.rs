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
                            "vuln: taint analysis for {} is not yet supported by autodetect; \
                             use --lang python, --lang rust, --lang typescript, or --lang javascript \
                             to scan files of a supported language, or omit --lang in a pure \
                             Python/Rust/TypeScript/JavaScript project.",
                            l.as_str()
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
        let mut files_with_vulns: HashSet<String> = HashSet::new();

        for file_path in &files {
            if let Ok(findings) = analyze_file(file_path) {
                for finding in findings {
                    files_with_vulns.insert(finding.file.clone());
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

        // Build summary
        let summary = build_summary(&filtered_findings, files_with_vulns.len() as u32);

        // Build report
        let report = VulnReport {
            findings: filtered_findings.clone(),
            summary: Some(summary),
            scan_duration_ms: start.elapsed().as_millis() as u64,
            files_scanned,
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

        // Exit code: 2 if vulnerabilities found (per spec)
        if !filtered_findings.is_empty() {
            return Err(RemainingError::findings_detected(filtered_findings.len() as u32).into());
        }

        Ok(())
    }
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
fn is_natively_analyzed(lang: Language) -> bool {
    // VAL-011 (M12, v0.2.2-hotfix-bundle): TypeScript and JavaScript
    // promoted into the autodetect-supported set. The taint engine at
    // `crates/tldr-core/src/security/taint.rs:909` already routes both
    // through `TYPESCRIPT_PATTERNS` (sources, sinks, sanitizers all
    // populated; v0.2.2 M7 expanded the sink set with SSRF). The CLI
    // gate just hadn't been told. Pre-VAL-011 the gate listed only
    // Python and Rust, so `tldr vuln <ts-file>` (no `--lang`) exited
    // 2 with "not yet supported" — issue parcadei/tldr-code#1, sub-
    // issue #1.C.
    matches!(
        lang,
        Language::Python | Language::Rust | Language::TypeScript | Language::JavaScript
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
/// flows through the canonical per-function `compute_taint_with_tree`
/// dispatch that handles all 16 supported languages uniformly. The
/// `.rs` branch is preserved per Reframe C — `analyze_rust_file` is a
/// distinct concern (UnsafeCode/MemorySafety/Panic line-scanner, not
/// taint flow).
fn analyze_file(path: &Path) -> Result<Vec<VulnFinding>, RemainingError> {
    let source = fs::read_to_string(path).map_err(|_| RemainingError::file_not_found(path))?;
    if matches!(path.extension().and_then(|e| e.to_str()), Some("rs")) {
        return Ok(analyze_rust_file(path, &source));
    }
    // For all other languages (Python, Go, Java, JS, TS, C, C++, Ruby,
    // PHP, etc.), use tldr-core's multi-language vulnerability scanner.
    match tldr_core::security::vuln::scan_vulnerabilities(path, None, None) {
        Ok(report) => {
            let mut findings = Vec::new();
            for f in report.findings {
                let vuln_type = map_core_vuln_type(f.vuln_type);
                let severity = match f.severity.to_uppercase().as_str() {
                    "CRITICAL" => Severity::Critical,
                    "HIGH" => Severity::High,
                    "MEDIUM" => Severity::Medium,
                    "LOW" => Severity::Low,
                    _ => Severity::Medium,
                };
                let file_str = f.file.display().to_string();
                findings.push(VulnFinding {
                    vuln_type,
                    severity,
                    cwe_id: f.cwe_id.unwrap_or_default(),
                    title: format!("{:?}", f.vuln_type),
                    description: format!("{} with unsanitized input", f.sink.sink_type),
                    file: file_str.clone(),
                    line: f.sink.line,
                    column: 0,
                    taint_flow: vec![
                        TaintFlow {
                            file: file_str.clone(),
                            line: f.source.line,
                            column: 0,
                            code_snippet: f.source.expression.clone(),
                            description: format!("Source: {}", f.source.source_type),
                        },
                        TaintFlow {
                            file: file_str,
                            line: f.sink.line,
                            column: 0,
                            code_snippet: f.sink.expression.clone(),
                            description: format!("Sink: {}", f.sink.sink_type),
                        },
                    ],
                    remediation: f.remediation.clone(),
                    confidence: 0.85,
                });
            }
            Ok(findings)
        }
        Err(_) => Ok(Vec::new()),
    }
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
    }
}

fn analyze_rust_file(path: &Path, source: &str) -> Vec<VulnFinding> {
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
            && contains_sql_keyword(trimmed)
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
        taint_flow: Vec::new(),
        remediation: remediation.to_string(),
        confidence,
    }
}

fn has_nearby_safety_comment(lines: &[&str], index: usize) -> bool {
    let start = index.saturating_sub(2);
    (start..index).any(|i| lines[i].contains("SAFETY:"))
}

fn contains_sql_keyword(text: &str) -> bool {
    let upper = text.to_uppercase();
    ["SELECT", "INSERT", "UPDATE", "DELETE", "FROM", "WHERE"]
        .iter()
        .any(|kw| upper.contains(kw))
}

fn is_rust_test_file(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    path_str.contains("/tests/")
        || path_str.contains("\\tests\\")
        || path_str.ends_with("_test.rs")
        || path_str.ends_with("tests.rs")
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
        *by_type
            .entry(format!("{:?}", finding.vuln_type).to_lowercase())
            .or_insert(0) += 1;
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
                            "startLine": f.line,
                            "startColumn": f.column
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
                                                    "startLine": tf.line,
                                                    "startColumn": tf.column
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // VULN-MIGRATION-V1 M4: tests `test_is_taint_source`, `test_is_taint_sink`,
    // `test_taint_tracker`, and `test_string_interpolation_detection` were
    // deleted because they referenced the now-removed CLI-local taint
    // machinery (TaintSource/TaintSink/PYTHON_SOURCES/PYTHON_SINKS,
    // TaintTracker/TaintInfo, is_taint_source/is_taint_sink/
    // is_string_interpolation_tainted helpers). The Python path post-M4
    // routes through `tldr_core::security::vuln::scan_vulnerabilities`
    // (canonical per-function `compute_taint_with_tree` dispatch); the
    // equivalent regression coverage lives in `vuln_migration_v1_red.rs`
    // (Python f-string flow + positive-detection tests) plus
    // `tldr_core/src/security/vuln.rs` and `taint.rs` unit tests.

    #[test]
    fn test_vuln_type_cwe_mapping() {
        assert_eq!(VulnType::SqlInjection.cwe_id(), "CWE-89");
        assert_eq!(VulnType::Xss.cwe_id(), "CWE-79");
        assert_eq!(VulnType::CommandInjection.cwe_id(), "CWE-78");
    }

    #[test]
    fn test_vuln_type_severity() {
        assert_eq!(
            VulnType::SqlInjection.default_severity(),
            Severity::Critical
        );
        assert_eq!(VulnType::Xss.default_severity(), Severity::High);
        assert_eq!(VulnType::OpenRedirect.default_severity(), Severity::Medium);
    }

    #[test]
    fn test_collect_files_includes_rust() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("a.py"), "print('ok')").unwrap();
        std::fs::write(temp.path().join("b.rs"), "fn main() {}").unwrap();
        std::fs::write(temp.path().join("c.txt"), "ignore").unwrap();

        let files = collect_files(temp.path(), None, false).unwrap();
        assert!(files.iter().any(|f| f.ends_with("a.py")));
        assert!(files.iter().any(|f| f.ends_with("b.rs")));
        assert!(!files.iter().any(|f| f.ends_with("c.txt")));
    }

    #[test]
    fn test_analyze_rust_detects_unsafe_without_safety_comment() {
        let source = r#"
pub fn raw_copy(ptr: *mut u8) {
    unsafe { *ptr = 7; }
}
"#;
        let findings = analyze_rust_file(Path::new("src/lib.rs"), source);
        assert!(findings.iter().any(|f| f.vuln_type == VulnType::UnsafeCode));
    }

    #[test]
    fn test_analyze_rust_detects_command_and_sql_patterns() {
        let source = r#"
use std::process::Command;

pub fn run(user: &str, name: &str) {
    let q = format!("SELECT * FROM users WHERE name = '{}'", name);
    let _ = Command::new("sh").arg(user).output();
}
"#;
        let findings = analyze_rust_file(Path::new("src/lib.rs"), source);
        assert!(findings
            .iter()
            .any(|f| f.vuln_type == VulnType::SqlInjection));
        assert!(findings
            .iter()
            .any(|f| f.vuln_type == VulnType::CommandInjection));
    }

    #[test]
    fn test_analyze_rust_detects_transmute_usage() {
        let source = r#"
use std::mem;

pub fn cast(x: u32) -> i32 {
    unsafe { mem::transmute(x) }
}
"#;
        let findings = analyze_rust_file(Path::new("src/lib.rs"), source);
        assert!(findings
            .iter()
            .any(|f| f.vuln_type == VulnType::MemorySafety && f.title.contains("transmute")));
    }

    #[test]
    fn test_analyze_rust_detects_raw_pointer_operation() {
        let source = r#"
pub unsafe fn read_ptr(p: *const u8) -> u8 {
    std::ptr::read(p)
}
"#;
        let findings = analyze_rust_file(Path::new("src/lib.rs"), source);
        assert!(findings
            .iter()
            .any(|f| f.vuln_type == VulnType::MemorySafety && f.title.contains("Raw Pointer")));
    }

    #[test]
    fn test_analyze_rust_detects_unwrap_in_non_test_code() {
        let source = r#"
pub fn parse(s: &str) -> i32 {
    s.parse::<i32>().unwrap()
}
"#;
        let findings = analyze_rust_file(Path::new("src/lib.rs"), source);
        assert!(findings
            .iter()
            .any(|f| f.vuln_type == VulnType::Panic && f.title.contains("unwrap")));
    }

    #[test]
    fn test_analyze_rust_detects_unchecked_bytes_patterns() {
        let source = r#"
pub fn from_raw(bytes: &[u8]) -> &str {
    unsafe { std::str::from_utf8_unchecked(bytes) }
}
"#;
        let findings = analyze_rust_file(Path::new("src/lib.rs"), source);
        assert!(findings
            .iter()
            .any(|f| f.vuln_type == VulnType::MemorySafety
                && f.title.contains("Unchecked Byte/String Conversion")));
    }
}

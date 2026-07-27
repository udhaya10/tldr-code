//! Vulnerability detection via taint analysis
//!
//! Implements detection of security vulnerabilities as per spec Section 2.9.2:
//! - SQL Injection (user input -> cursor.execute)
//! - XSS (user input -> innerHTML)
//! - Command Injection (user input -> os.system)
//! - Path Traversal (user input -> open/Path)
//! - SSRF (user input -> http.Get / requests.get)
//! - Deserialization (user input -> pickle.load / unserialize)
//!
//! # Architecture (post vuln-migration-v1 M3)
//!
//! `scan_file_vulns` is a thin per-function wrapper over the canonical
//! `tldr_core::security::taint::compute_taint_with_tree` — for every function
//! returned by `extract_functions_detailed` we build a CFG/DFG, run the
//! AST-based taint engine, and project each `TaintFlow` to a `VulnFinding`
//! via the `From` adapters on the (RETAINED) `vuln::TaintSource` /
//! `vuln::TaintSink` output records. The pre-M3 substring two-pass scanner
//! (`get_sources` / `get_sinks` per-language Vec tables, `extract_propagation`,
//! `is_type_coerced`, `is_sanitized_*`) has been deleted; sanitizer awareness
//! and string-literal filtering now flow through the canonical AST sanitizer
//! / `is_in_string` / `is_in_comment` infrastructure (see
//! `sanitizer-removal-v1` and `regex-removal-v1`).
//!
//! # Example
//! ```ignore
//! use tldr_core::security::vuln::{scan_vulnerabilities, VulnType};
//!
//! let report = scan_vulnerabilities(Path::new("src/"), None, None)?;
//! for finding in &report.findings {
//!     println!("{}: {} -> {}", finding.vuln_type, finding.source, finding.sink);
//! }
//! ```

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::ast::extract::{extract_classes_detailed, extract_functions_detailed};
use crate::ast::parser::parse;
use crate::cfg::extractor::extract_cfg_from_tree;
use crate::dfg::extractor::extract_dfg_from_tree_with_cfg;
use crate::error::TldrError;
use crate::security::taint::{
    compute_taint_with_tree, TaintSink as CanonicalTaintSink, TaintSinkType,
    TaintSource as CanonicalTaintSource, TaintSourceType,
};
use crate::types::Language;
use crate::TldrResult;

// =============================================================================
// Types
// =============================================================================

/// Types of vulnerabilities detected
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VulnType {
    /// SQL Injection - unsanitized input to SQL queries
    SqlInjection,
    /// Cross-Site Scripting - unsanitized input to HTML output
    Xss,
    /// Command Injection - unsanitized input to shell commands
    CommandInjection,
    /// Path Traversal - unsanitized input to file operations
    PathTraversal,
    /// Server-Side Request Forgery
    Ssrf,
    /// Unsafe Deserialization
    Deserialization,
    /// Open Redirect (CWE-601) — HTTP redirect target controllable by user input.
    /// (M3 detection-accuracy-v1 BUG-16)
    OpenRedirect,
}

impl std::fmt::Display for VulnType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VulnType::SqlInjection => write!(f, "SQL Injection"),
            VulnType::Xss => write!(f, "Cross-Site Scripting (XSS)"),
            VulnType::CommandInjection => write!(f, "Command Injection"),
            VulnType::PathTraversal => write!(f, "Path Traversal"),
            VulnType::Ssrf => write!(f, "Server-Side Request Forgery"),
            VulnType::Deserialization => write!(f, "Unsafe Deserialization"),
            VulnType::OpenRedirect => write!(f, "Open Redirect"),
        }
    }
}

/// A taint source (user input entry point) — output adapter record
///
/// (vuln-migration-v1 M3, premortem T2/DR2) RETAINED as an output adapter
/// struct populated via `From<crate::security::TaintSource>`. The CLI consumer
/// at `crates/tldr-cli/src/commands/remaining/vuln.rs:679-688` reads
/// `f.source.line / .expression / .source_type` unchanged; preserving this
/// shape avoids a JSON/SARIF schema break.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaintSource {
    /// Variable name containing tainted data
    pub variable: String,
    /// Source description
    pub source_type: String,
    /// Line number
    pub line: u32,
    /// Original expression
    pub expression: String,
}

/// A taint sink (dangerous operation) — output adapter record
///
/// (vuln-migration-v1 M3, premortem T2/DR2) RETAINED as an output adapter
/// struct populated via `From<crate::security::TaintSink>`. The CLI consumer
/// at `crates/tldr-cli/src/commands/remaining/vuln.rs:679-688` reads
/// `f.sink.line / .expression / .sink_type` unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaintSink {
    /// Function/method being called
    pub function: String,
    /// Sink type description
    pub sink_type: String,
    /// Line number
    pub line: u32,
    /// Full call expression
    pub expression: String,
}

/// A single vulnerability finding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VulnFinding {
    /// Type of vulnerability
    pub vuln_type: VulnType,
    /// File containing the vulnerability
    pub file: PathBuf,
    /// Source of tainted data
    pub source: TaintSource,
    /// Sink where tainted data flows
    pub sink: TaintSink,
    /// Taint flow path (variable assignments)
    pub flow_path: Vec<String>,
    /// Severity (based on vuln type and certainty)
    pub severity: String,
    /// Remediation advice
    pub remediation: String,
    /// CWE ID
    pub cwe_id: Option<String>,
}

/// Summary of vulnerability scan
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VulnSummary {
    /// Total vulnerabilities found
    pub total_findings: usize,
    /// Count by vulnerability type
    pub by_type: HashMap<String, usize>,
    /// Files with vulnerabilities
    pub affected_files: usize,
}

/// Report from vulnerability scan
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VulnReport {
    /// All vulnerability findings
    pub findings: Vec<VulnFinding>,
    /// Number of files scanned
    pub files_scanned: usize,
    /// Summary statistics
    pub summary: VulnSummary,
}

// =============================================================================
// Adapters: canonical taint structs → vuln output records
// =============================================================================

impl From<CanonicalTaintSource> for TaintSource {
    fn from(canonical: CanonicalTaintSource) -> Self {
        Self {
            variable: canonical.var,
            source_type: format!("{:?}", canonical.source_type),
            line: canonical.line,
            expression: canonical
                .statement
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
        }
    }
}

impl From<CanonicalTaintSink> for TaintSink {
    fn from(canonical: CanonicalTaintSink) -> Self {
        // The canonical sink has `var` (the tainted argument); the vuln record
        // wants `function` (the call/sink-pattern name). The cleanest synthesis
        // is the `var` itself when the canonical layer didn't preserve a call
        // name — downstream display text uses both `function` and `expression`,
        // and the expression always carries the full statement.
        Self {
            function: canonical.var.clone(),
            sink_type: format!("{:?}", canonical.sink_type),
            line: canonical.line,
            expression: canonical
                .statement
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
        }
    }
}

// =============================================================================
// Helpers: TaintSinkType → VulnType + severity + descriptions
// =============================================================================

/// Project a canonical `TaintSinkType` to the user-facing `VulnType` ontology.
///
/// Exhaustive — adding a `TaintSinkType` variant in M1 forces a compile-time
/// update here (no wildcard arm, by design).
fn vuln_type_from_sink(sink_type: TaintSinkType) -> VulnType {
    match sink_type {
        TaintSinkType::SqlQuery => VulnType::SqlInjection,
        TaintSinkType::ShellExec
        | TaintSinkType::CodeEval
        | TaintSinkType::CodeExec
        | TaintSinkType::CodeCompile => VulnType::CommandInjection,
        TaintSinkType::HtmlOutput => VulnType::Xss,
        TaintSinkType::FileOpen | TaintSinkType::FileWrite => VulnType::PathTraversal,
        TaintSinkType::HttpRequest => VulnType::Ssrf,
        TaintSinkType::Deserialize => VulnType::Deserialization,
        TaintSinkType::OpenRedirect => VulnType::OpenRedirect,
    }
}

/// Get severity string for a vulnerability type.
///
/// Pre-M3 every finding hard-coded "HIGH"; preserved exactly for output
/// stability.
fn severity_for(_vuln_type: VulnType) -> &'static str {
    "HIGH"
}

/// TAINT-FINDING-DEDUPE-V1: rank a `TaintSinkType` by specificity for the
/// per-call-site dedupe in `scan_file_vulns`.
///
/// When two findings collide on `(file, sink.line, source.line,
/// source.variable)` — i.e. the SAME taint flow matched multiple sink
/// patterns at the same call site — we keep the entry with the HIGHEST
/// rank. The ordering reflects "most actionable diagnostic":
///
/// 1. **`SqlQuery`** (rank 110) — the only SQL sink, dedup against itself
///    only; rank kept distinct from the code-execution family for clarity.
/// 2. **`ShellExec`** (rank 100) — directly invokes a shell; an exploit
///    is a one-step OS-level compromise.
/// 3. **`CodeEval`** (rank 95) — `eval()` family: arbitrary in-process
///    code execution, generally more severe than `exec()` because `eval`
///    return values may be exfiltrated.
/// 4. **`CodeExec`** (rank 90) — `exec()` family: runs a code object but
///    does not return the value.
/// 5. **`CodeCompile`** (rank 85) — `compile()` produces a code object
///    that is only dangerous when later executed; least specific of the
///    Code* triple.
/// 6. **`Deserialize`** (rank 80) — RCE-via-deserialization, ranks below
///    the code-execution family because the exploit pathway is gadget-
///    chain-dependent.
/// 7. **`HtmlOutput`** (rank 70) — XSS sink.
/// 8. **`FileOpen`** (rank 60) — read-side path-traversal, more frequent
///    in real corpora than `FileWrite`; preferred when both match.
/// 9. **`FileWrite`** (rank 50).
/// 10. **`HttpRequest`** (rank 40) — SSRF.
///
/// The numeric rank itself is internal; only the relative ordering is
/// observable. The CodeEval > CodeExec > CodeCompile sub-ordering is the
/// load-bearing one for the flask `cli.py:1023`/`config.py:209` case
/// where `eval(compile(...))` triggers all three sink patterns at the
/// same call site.
fn sink_type_precedence(sink_type: TaintSinkType) -> u32 {
    match sink_type {
        TaintSinkType::SqlQuery => 110,
        TaintSinkType::ShellExec => 100,
        TaintSinkType::CodeEval => 95,
        TaintSinkType::CodeExec => 90,
        TaintSinkType::CodeCompile => 85,
        TaintSinkType::Deserialize => 80,
        TaintSinkType::HtmlOutput => 70,
        TaintSinkType::FileOpen => 60,
        TaintSinkType::FileWrite => 50,
        TaintSinkType::HttpRequest => 40,
        // Open-redirect ranks below SSRF; both involve URL flow but
        // the open-redirect surface is generally lower-impact than full SSRF.
        TaintSinkType::OpenRedirect => 35,
    }
}

/// Get a human-readable description for a `TaintSourceType`, partitioned by
/// language.
///
/// Pre-M3 these strings came from `get_sources` per-language Vec tables (e.g.,
/// `"Flask GET parameter"`, `"Express query parameter"`). After the substring
/// scanner is deleted, the canonical taint engine emits enum-typed
/// `TaintSourceType` instead. This helper preserves the descriptive
/// per-language mapping so the JSON `source.source_type` field continues to
/// carry actionable output (R6 mitigation).
fn descriptions_for(source_type: TaintSourceType, language: Language) -> &'static str {
    match (source_type, language) {
        // HTTP parameters / body / cookies / headers
        (TaintSourceType::HttpParam, Language::Python) => "Flask GET/POST parameter",
        (TaintSourceType::HttpParam, Language::JavaScript)
        | (TaintSourceType::HttpParam, Language::TypeScript) => "Express query/route parameter",
        (TaintSourceType::HttpParam, Language::Go) => "HTTP query parameter",
        (TaintSourceType::HttpParam, Language::Java) => "Servlet parameter",
        (TaintSourceType::HttpParam, Language::Ruby) => "Rails parameter",
        (TaintSourceType::HttpParam, Language::Kotlin) => "Ktor query parameter",
        (TaintSourceType::HttpParam, Language::Scala) => "Play query parameter",
        (TaintSourceType::HttpParam, Language::CSharp) => "ASP.NET request parameter",
        (TaintSourceType::HttpParam, Language::Php) => "PHP $_GET / $_POST / $_REQUEST",
        (TaintSourceType::HttpParam, Language::Elixir) => "Phoenix conn.params",
        (TaintSourceType::HttpParam, Language::Lua)
        | (TaintSourceType::HttpParam, Language::Luau) => "OpenResty/ngx request args",
        (TaintSourceType::HttpParam, _) => "HTTP request parameter",

        (TaintSourceType::HttpBody, Language::Python) => "Flask JSON/raw request body",
        (TaintSourceType::HttpBody, Language::JavaScript)
        | (TaintSourceType::HttpBody, Language::TypeScript) => "Express request body",
        (TaintSourceType::HttpBody, _) => "HTTP request body",

        // User input
        (TaintSourceType::UserInput, Language::Python) => "User input from stdin",
        (TaintSourceType::UserInput, Language::Java) => "User input (Scanner / readLine)",
        (TaintSourceType::UserInput, Language::Kotlin) => "User input (readLine)",
        (TaintSourceType::UserInput, Language::Scala) => "User input (StdIn / readLine)",
        (TaintSourceType::UserInput, Language::CSharp) => "User input (Console.ReadLine)",
        (TaintSourceType::UserInput, Language::Swift) => {
            "User input (CommandLine.arguments / readLine)"
        }
        (TaintSourceType::UserInput, Language::Lua)
        | (TaintSourceType::UserInput, Language::Luau) => "User input (io.read)",
        (TaintSourceType::UserInput, _) => "User input from stdin",

        (TaintSourceType::Stdin, Language::C) | (TaintSourceType::Stdin, Language::Cpp) => {
            "Standard input (scanf / fgets / cin)"
        }
        (TaintSourceType::Stdin, _) => "Standard input",

        // Environment / args
        (TaintSourceType::EnvVar, Language::Python) => {
            "Environment variable (os.environ / os.getenv)"
        }
        (TaintSourceType::EnvVar, Language::JavaScript)
        | (TaintSourceType::EnvVar, Language::TypeScript) => "Environment variable (process.env)",
        (TaintSourceType::EnvVar, Language::Go) => "Environment variable (os.Getenv)",
        (TaintSourceType::EnvVar, Language::Rust) => "Environment variable (std::env::var)",
        (TaintSourceType::EnvVar, Language::Java) => "Environment variable (System.getenv)",
        (TaintSourceType::EnvVar, Language::Kotlin) => "Environment variable (System.getenv)",
        (TaintSourceType::EnvVar, Language::Scala) => "Environment variable (sys.env)",
        (TaintSourceType::EnvVar, Language::CSharp) => {
            "Environment variable (Environment.GetEnvironmentVariable)"
        }
        (TaintSourceType::EnvVar, Language::Php) => "Environment variable ($_ENV / getenv)",
        (TaintSourceType::EnvVar, Language::Ruby) => "Environment variable (ENV)",
        (TaintSourceType::EnvVar, Language::C) | (TaintSourceType::EnvVar, Language::Cpp) => {
            "Environment variable (getenv)"
        }
        (TaintSourceType::EnvVar, Language::Lua) | (TaintSourceType::EnvVar, Language::Luau) => {
            "Environment variable (os.getenv)"
        }
        (TaintSourceType::EnvVar, Language::Swift) => {
            "Environment variable (ProcessInfo.environment)"
        }
        (TaintSourceType::EnvVar, Language::Elixir) => "Environment variable (System.get_env)",
        (TaintSourceType::EnvVar, Language::Ocaml) => "Environment variable (Sys.getenv)",

        // File reads
        (TaintSourceType::FileRead, _) => "Untrusted file read",
    }
}

/// Get remediation advice for a vulnerability type
fn get_remediation(vuln_type: VulnType) -> &'static str {
    match vuln_type {
        VulnType::SqlInjection =>
            "Use parameterized queries or prepared statements instead of string concatenation",
        VulnType::Xss =>
            "Sanitize output using context-appropriate encoding (HTML, JavaScript, URL, etc.)",
        VulnType::CommandInjection =>
            "Use subprocess with shell=False and pass arguments as a list, or use shlex.quote()",
        VulnType::PathTraversal =>
            "Validate paths against a whitelist or use realpath() and verify the result is within allowed directories",
        VulnType::Ssrf =>
            "Validate URLs against an allowlist of domains and protocols",
        VulnType::Deserialization =>
            "Avoid deserializing untrusted data, or use safer formats like JSON",
        VulnType::OpenRedirect =>
            "Validate redirect targets against an allowlist of trusted URLs/origins; do not concatenate user input into the redirect target",
    }
}

/// Get CWE ID for a vulnerability type
fn get_cwe_id(vuln_type: VulnType) -> &'static str {
    match vuln_type {
        VulnType::SqlInjection => "CWE-89",
        VulnType::Xss => "CWE-79",
        VulnType::CommandInjection => "CWE-78",
        VulnType::PathTraversal => "CWE-22",
        VulnType::Ssrf => "CWE-918",
        VulnType::Deserialization => "CWE-502",
        VulnType::OpenRedirect => "CWE-601",
    }
}

// =============================================================================
// Main API
// =============================================================================

/// Scan for security vulnerabilities using taint analysis
///
/// # Arguments
/// * `path` - File or directory to scan
/// * `language` - Optional language filter (auto-detect if None)
/// * `vuln_type` - Optional filter for specific vulnerability type
///
/// # Returns
/// * `Ok(VulnReport)` - Report with all findings
/// * `Err(TldrError)` - On file system or parse errors
///
/// # Example
/// ```ignore
/// use tldr_core::security::vuln::{scan_vulnerabilities, VulnType};
///
/// // Scan for all vulnerabilities
/// let report = scan_vulnerabilities(Path::new("src/"), None, None)?;
///
/// // Scan for SQL injection only
/// let report = scan_vulnerabilities(
///     Path::new("src/"),
///     Some(Language::Python),
///     Some(VulnType::SqlInjection),
/// )?;
/// ```
pub fn scan_vulnerabilities(
    path: &Path,
    language: Option<Language>,
    vuln_type: Option<VulnType>,
) -> TldrResult<VulnReport> {
    let mut findings = Vec::new();

    // Collect files to scan
    let files: Vec<PathBuf> = if path.is_file() {
        vec![path.to_path_buf()]
    } else {
        WalkDir::new(path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| {
                let detected = Language::from_path(e.path());
                match (detected, language) {
                    (Some(d), Some(l)) => d == l,
                    (Some(_), None) => true,
                    _ => false,
                }
            })
            .map(|e| e.path().to_path_buf())
            .collect()
    };

    // VULN-MIGRATION-V1 M3 perf: parallelize per-file scan with rayon (per
    // dispatch-contract M3 stop-threshold "If either axis fails, parallelize
    // per-file with rayon BEFORE declaring GREEN"). Per-function CFG/DFG/taint
    // construction is the hot loop — distributing across cores keeps the
    // dir-level wall-clock under the 2x M1 baseline.
    use rayon::prelude::*;
    let scan_results: Vec<Vec<VulnFinding>> = files
        .par_iter()
        .map(|file_path| scan_file_vulns(file_path, vuln_type).unwrap_or_default())
        .collect();
    for file_findings in scan_results {
        if !file_findings.is_empty() {
            findings.extend(file_findings);
        }
    }
    // files_scanned counts every file we attempted, mirroring pre-M3 semantics
    // (the pre-M3 loop incremented on Ok regardless of finding count; we keep
    // the same liberal counting since rayon's `unwrap_or_default()` collapses
    // Err paths to empty Vecs).
    let files_scanned = files.len();

    // Calculate summary
    let mut by_type: HashMap<String, usize> = HashMap::new();
    let mut affected_files: HashSet<PathBuf> = HashSet::new();
    for finding in &findings {
        *by_type.entry(finding.vuln_type.to_string()).or_insert(0) += 1;
        affected_files.insert(finding.file.clone());
    }

    let summary = VulnSummary {
        total_findings: findings.len(),
        by_type,
        affected_files: affected_files.len(),
    };

    Ok(VulnReport {
        findings,
        files_scanned,
        summary,
    })
}

// =============================================================================
// Sanitization post-filters (VULN-MIGRATION-V1 M3 carry-forward)
// =============================================================================
// These two argument-shape sanitization patterns aren't expressible as AST
// sanitizer call-wrappers in the canonical sanitizer dispatch (they depend on
// the SHAPE of the call's arguments, not on whether a sanitizer call wraps the
// tainted variable). They were detected pre-M3 by `is_sanitized_sql` /
// `is_sanitized_command` line-text inspection in `is_sanitized_sink`. Mirrored
// here as scan_file_vulns post-filters to preserve the test_e2e_*
// regression-guard semantics. A future canonical-engine extension can promote
// these to first-class AST sanitizers and delete this code.

/// Detect parameterized SQL: placeholder (`?` / `%s` / `:name`) + tuple/list/dict
/// argument syntax on the same statement.
fn is_parameterized_sql(line: &str) -> bool {
    let has_placeholder = line.contains('?') || line.contains("%s") || has_named_param(line);
    let has_args_collection = line.contains(", (") || line.contains(", [") || line.contains(", {");
    has_placeholder && has_args_collection
}

/// Detect parameter-named placeholder `:user_id`. Avoids URL false positives
/// (`http://`) by requiring the preceding char to be space / `=` / quote.
fn has_named_param(line: &str) -> bool {
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b':' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_alphabetic() {
            if i == 0 {
                return true;
            }
            let prev = bytes[i - 1];
            if prev == b' ' || prev == b'=' || prev == b'\'' || prev == b'"' {
                return true;
            }
        }
    }
    false
}

/// Detect safe subprocess invocation: list-form arguments
/// (`subprocess.{run,call,Popen}([`) or explicit `shell=False`.
/// `shell=True` overrides — always unsafe.
fn is_safe_subprocess_call(line: &str) -> bool {
    if line.contains("shell=True") {
        return false;
    }
    if line.contains("shell=False") {
        return true;
    }
    for prefix in &["subprocess.run(", "subprocess.call(", "subprocess.Popen("] {
        if let Some(pos) = line.find(prefix) {
            let after = &line[pos + prefix.len()..];
            if after.trim_start().starts_with('[') {
                return true;
            }
        }
    }
    false
}

// =============================================================================
// Internal Implementation — per-function compute_taint_with_tree dispatch
// =============================================================================

/// Scan a single file for vulnerabilities.
///
/// (vuln-migration-v1 M3) Per-function loop over the canonical
/// `compute_taint_with_tree`:
///   1. Parse the file ONCE with tree-sitter (tree reused across functions).
///   2. Enumerate functions via `extract_functions_detailed`.
///   3. For each function: build CFG + DFG (which scope to that function via
///      `find_function_node`), construct minimal SSA where possible, scope the
///      `statements` map to the function's CFG block range, and run
///      `compute_taint_with_tree`.
///   4. Project each `TaintFlow` to a `VulnFinding` via the adapter `From`
///      impls + `vuln_type_from_sink` classification.
///
/// String-literal and comment FP suppression: the canonical engine filters
/// those upstream via `is_in_string` / `is_in_comment` (regex-removal-v1 +
/// sanitizer-removal-v1). The pre-M3 substring scanner had no such filtering,
/// which is why string-literal regression-guard fixtures FAILED on the 14
/// fall-through languages — that class of FP is closed at M3.
fn scan_file_vulns(path: &Path, vuln_filter: Option<VulnType>) -> TldrResult<Vec<VulnFinding>> {
    let content = std::fs::read_to_string(path)?;
    let language = Language::from_path(path).ok_or_else(|| {
        TldrError::UnsupportedLanguage(
            path.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("unknown")
                .to_string(),
        )
    })?;

    // Parse ONCE — reused across per-function CFG/DFG/taint passes.
    let tree = match parse(&content, language) {
        Ok(t) => t,
        Err(_) => return Ok(Vec::new()), // graceful degradation on parse failure
    };

    // Enumerate function definitions (name + start line) using the canonical
    // ast::extract helper (visibility extended to pub(crate) in M3 per
    // premortem T3/DR3). Top-level functions ONLY — methods inside classes,
    // structs, traits, objects are returned via `extract_classes_detailed`
    // (Scala `object M { def f ... }`, Java `class C { void f() {} }`, etc.).
    let mut fn_infos: Vec<crate::types::FunctionInfo> =
        extract_functions_detailed(&tree, &content, language);
    let class_infos = extract_classes_detailed(&tree, &content, language);
    for class_info in class_infos {
        fn_infos.extend(class_info.methods);
    }
    // Dedup by (name, line_number) — some extractors may double-emit a function
    // under both the top-level and class-method paths.
    fn_infos.sort_by(|a, b| a.line_number.cmp(&b.line_number).then(a.name.cmp(&b.name)));
    fn_infos.dedup_by(|a, b| a.name == b.name && a.line_number == b.line_number);

    let path_str = path.to_str().unwrap_or_default();
    let path_buf = path.to_path_buf();
    let source_bytes = content.as_bytes();
    let mut findings: Vec<VulnFinding> = Vec::new();

    // FALLBACK: empty function list (e.g., top-level Go statements with no
    // user-defined functions, or extractor missed all). Run a single
    // whole-file taint pass using a synthetic CFG-empty path. We do this by
    // skipping the loop and emitting nothing — preserving existing behavior
    // where files with no functions had no findings.
    if fn_infos.is_empty() {
        return Ok(findings);
    }

    // VULN-MIGRATION-V1 M3 perf: parallelize the per-function CFG/DFG/taint
    // construction with rayon. Each function's analysis is independent (the
    // shared inputs — `tree`, `content`, `language` — are read-only). Tree-
    // sitter's `Tree` is `Send + Sync` so it can be shared across threads.
    // This is the inner-loop parallelism complement to the outer per-file
    // par_iter in `scan_vulnerabilities`; for single-file CLI dispatch
    // (where the outer loop has only 1 element), this inner par_iter is the
    // primary parallelism source.
    use rayon::prelude::*;

    // VULN-FASTPATH-SUBSTRING-PREFILTER-V1 (BUG-26 perf):
    // Pre-compute the (start_line, end_line) body range for each function
    // BEFORE the par_iter so the cheap substring prefilter can run without
    // touching CFG. `fn_infos` is already sorted by `line_number` ascending;
    // the body of fn[i] runs from `fn_infos[i].line_number` to either
    // `fn_infos[i+1].line_number - 1` or EOF. This is a coarse over-
    // approximation (it includes any trailing top-level code between
    // functions in the file), which is correctness-preserving for the
    // prefilter — over-approximating the body text only causes the
    // prefilter to RUN the full analysis more often, never to skip it
    // incorrectly.
    let total_lines = content.lines().count() as u32;
    let mut fn_body_ranges: Vec<(u32, u32)> = Vec::with_capacity(fn_infos.len());
    for (i, fi) in fn_infos.iter().enumerate() {
        let start = fi.line_number.max(1);
        let end = if i + 1 < fn_infos.len() {
            fn_infos[i + 1].line_number.saturating_sub(1).max(start)
        } else {
            total_lines.max(start)
        };
        fn_body_ranges.push((start, end));
    }
    // Slice each function's body text once, lazily-shared via Vec<&str>.
    // `String::lines` allocates iterators not slices; use byte-offset slicing
    // for O(N) total instead of O(N²) for line-walking each function.
    let line_offsets: Vec<usize> = {
        let mut v: Vec<usize> = Vec::with_capacity(total_lines as usize + 1);
        v.push(0);
        for (i, b) in content.bytes().enumerate() {
            if b == b'\n' {
                v.push(i + 1);
            }
        }
        // Push end-of-content sentinel so range slicing always has an upper
        // bound for the last line.
        if *v.last().unwrap_or(&0) != content.len() {
            v.push(content.len());
        }
        v
    };
    let body_slice = |start_line: u32, end_line: u32| -> &str {
        let s = (start_line.saturating_sub(1) as usize).min(line_offsets.len() - 1);
        let e_idx = (end_line as usize).min(line_offsets.len() - 1);
        let start_byte = line_offsets[s];
        let end_byte = line_offsets[e_idx];
        &content[start_byte..end_byte]
    };

    // TAINT-FINDING-DEDUPE-V1: tag each candidate finding with its canonical
    // `TaintSinkType` so the merge phase can rank colliding entries by sink
    // specificity (most-specific wins; see `sink_type_precedence`).
    let per_fn_findings: Vec<Vec<(VulnFinding, TaintSinkType)>> = fn_infos
        .par_iter()
        .enumerate()
        .map(|(idx, fn_info)| {
            // VULN-FASTPATH-SUBSTRING-PREFILTER-V1: cheap substring check
            // before any CFG/DFG/taint construction. A `TaintFlow` requires
            // BOTH a source AND a sink in the same function; if neither
            // call-name appears anywhere in the body's source text, no
            // flow is possible. Substring match is a SUPERSET of the AST
            // detector — hits inside string literals or comments still
            // run the full analysis (canonical AST `is_in_string` /
            // sanitizer dispatch resolves those FPs at the detector
            // layer). A clean miss is a true negative and safe to skip.
            // See `crate::security::taint::fastpath_pattern_strings` for
            // the needle-set construction and correctness contract.
            let (start_line, end_line) = fn_body_ranges[idx];
            let body_text = body_slice(start_line, end_line);
            if !crate::security::taint::function_body_has_taint_pattern(body_text, language) {
                return Vec::new();
            }
            let cfg = match extract_cfg_from_tree(&tree, &content, &fn_info.name, language) {
                Ok(c) if !c.blocks.is_empty() => c,
                _ => return Vec::new(),
            };
            let dfg = match extract_dfg_from_tree_with_cfg(
                &tree,
                &content,
                &fn_info.name,
                language,
                &cfg,
            ) {
                Ok(d) => d,
                Err(_) => return Vec::new(),
            };
            // Pass `ssa = None` to force the M1a String-keyed worklist path
            // (handles indirect taint matches like
            // `cursor.execute(f"...{tainted}")`).
            let ssa: Option<&crate::ssa::types::SsaFunction> = None;
            let (fn_start, fn_end) = {
                let start = cfg.blocks.iter().map(|b| b.lines.0).min().unwrap_or(1);
                let end = cfg
                    .blocks
                    .iter()
                    .map(|b| b.lines.1)
                    .max()
                    .unwrap_or(content.lines().count() as u32);
                (start, end)
            };
            let statements: HashMap<u32, String> = content
                .lines()
                .enumerate()
                .filter(|(i, _)| {
                    let line_num = (i + 1) as u32;
                    line_num >= fn_start && line_num <= fn_end
                })
                .map(|(i, line)| ((i + 1) as u32, line.to_string()))
                .collect();
            let info = match compute_taint_with_tree(
                &cfg,
                &dfg.refs,
                &statements,
                Some(&tree),
                Some(source_bytes),
                language,
                ssa,
            ) {
                Ok(i) => i,
                Err(_) => return Vec::new(),
            };
            // Build candidate findings; dedup happens in the merge phase below.
            let mut local: Vec<(VulnFinding, TaintSinkType)> = Vec::new();
            for flow in info.flows {
                let canonical_sink_type = flow.sink.sink_type;
                let vuln_type = vuln_type_from_sink(canonical_sink_type);
                if let Some(filter) = vuln_filter {
                    if vuln_type != filter {
                        continue;
                    }
                }
                let stmt_text = flow.sink.statement.as_deref().unwrap_or("");
                if vuln_type == VulnType::SqlInjection && is_parameterized_sql(stmt_text) {
                    continue;
                }
                if vuln_type == VulnType::CommandInjection && is_safe_subprocess_call(stmt_text) {
                    continue;
                }
                // M3 detection-accuracy-v1 BUG-17: degenerate flow suppression.
                //
                // When the canonical engine emits a flow whose source and sink
                // collapse to the same file + line + expression text AND the
                // source/sink variables are identical, the JSON `taint_flow`
                // would emit two identical entries — there is literally no
                // propagation to describe (the engine's source-pattern and
                // sink-pattern matched the SAME identifier on the SAME line).
                //
                // The `source.var == sink.var` guard is load-bearing: a single
                // statement can legitimately host BOTH a source (`id =
                // params[:id]`) AND a sink that USES that tainted `id` later
                // on the same line (`db.execute("... " + id)` — the Ruby/Lua
                // single-line pattern from the v1 RED suite). Those legit
                // flows have distinct sink semantics from the source variable
                // and MUST NOT be suppressed. Empirically Ruby/Lua emit
                // sink.var = the call expression text or a different
                // identifier, so the equality check leaves them alone.
                //
                // The narrower var-mismatch class (e.g. Rust `let f =
                // File::open(path)`) is NOT suppressed here — it survives
                // and is annotated downstream by the CLI emit layer with
                // `direct_sink: true` (see `analyze_file` in
                // crates/tldr-cli/src/commands/remaining/vuln.rs and the
                // `degenerate_source_eq_sink_suppressed_or_annotated` test
                // in crates/tldr-cli/tests/detection_accuracy_v1.rs).
                if flow.source.line == flow.sink.line
                    && flow.source.var == flow.sink.var
                    && flow.source.statement.as_deref().unwrap_or("")
                        == flow.sink.statement.as_deref().unwrap_or("")
                {
                    continue;
                }
                let description = descriptions_for(flow.source.source_type, language).to_string();
                let source_record: TaintSource = flow.source.clone().into();
                let sink_record: TaintSink = flow.sink.clone().into();
                let flow_path: Vec<String> = if flow.path.is_empty() {
                    vec![
                        format!(
                            "{}:{} - taint source",
                            source_record.line, source_record.variable
                        ),
                        format!("{}:{} - sink", sink_record.line, sink_record.function),
                    ]
                } else {
                    flow.path
                        .iter()
                        .map(|bid| format!("block-{}", bid))
                        .collect()
                };
                local.push((
                    VulnFinding {
                        vuln_type,
                        file: path_buf.clone(),
                        source: TaintSource {
                            variable: source_record.variable,
                            source_type: description,
                            line: source_record.line,
                            expression: source_record.expression,
                        },
                        sink: sink_record,
                        flow_path,
                        severity: severity_for(vuln_type).to_string(),
                        remediation: get_remediation(vuln_type).to_string(),
                        cwe_id: Some(get_cwe_id(vuln_type).to_string()),
                    },
                    canonical_sink_type,
                ));
            }
            local
        })
        .collect();

    // TAINT-FINDING-DEDUPE-V1: collapse findings that share
    // `(file, sink.line, source.line, source.variable, vuln_type)`. The
    // same call site commonly matches multiple sink patterns within ONE
    // vuln_type — e.g. `eval(compile(f.read(), startup, "exec"))` triggers
    // CodeEval + CodeExec + CodeCompile in the canonical engine, all
    // mapping to `CommandInjection`, all with the same sink line and the
    // same `(source.line, source.variable)`. Most consumers want a single
    // highest-precedence finding per `(call site, vuln_type)` pair.
    //
    // Tuple INCLUDES `source.variable` — distinct tainted variables on
    // the same line are legitimately distinct findings (e.g.
    // `os.system(env_var + " " + user_input)` has two source vars on one
    // line; both must be retained).
    //
    // Tuple INCLUDES `vuln_type` — a single sink (e.g. PHP
    // `file_get_contents($u)`) can simultaneously be a `PathTraversal`
    // (FileOpen) AND an `Ssrf` (HttpRequest) for the same source variable
    // and source line. These are ORTHOGONAL findings (different remediation,
    // different CWE) and the RED suite asserts ≥1 of each type — collapsing
    // across vuln_type would corrupt that signal. Within-vuln_type dedupe
    // still solves the CodeEval/CodeExec/CodeCompile case (all
    // `CommandInjection`) and the FileOpen/FileWrite case (both
    // `PathTraversal`).
    //
    // Tuple EXCLUDES `sink.function` (the canonical sink var) because the
    // sink var is derived from the sink-pattern detector and varies
    // between overlapping detectors at the same call site (e.g. `f` for
    // FileOpen vs the wrapped expression for Code* sinks); including it
    // would defeat the dedupe.
    //
    // Precedence: pick the entry with highest `sink_type_precedence`
    // rank — see that helper for the ordering rationale (CodeEval >
    // CodeExec > CodeCompile, FileOpen > FileWrite, etc.). On ties the
    // first-encountered entry wins (deterministic via parallel map order
    // collected in `fn_infos` ordering).
    use std::collections::hash_map::Entry;
    let mut best: HashMap<(String, u32, u32, String, VulnType), (VulnFinding, TaintSinkType)> =
        HashMap::new();
    for fn_findings in per_fn_findings {
        for (finding, sink_type) in fn_findings {
            let key = (
                finding.file.display().to_string(),
                finding.sink.line,
                finding.source.line,
                finding.source.variable.clone(),
                finding.vuln_type,
            );
            match best.entry(key) {
                Entry::Vacant(v) => {
                    v.insert((finding, sink_type));
                }
                Entry::Occupied(mut o) => {
                    let cur_rank = sink_type_precedence(o.get().1);
                    let new_rank = sink_type_precedence(sink_type);
                    if new_rank > cur_rank {
                        o.insert((finding, sink_type));
                    }
                }
            }
        }
    }
    findings.extend(best.into_values().map(|(f, _)| f));

    let _ = path_str; // path_str retained for future use; suppress unused warn
    Ok(findings)
}

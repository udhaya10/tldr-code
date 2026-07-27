//! Code smell detection
//!
//! Implements detection of common code smells as per spec Section 2.8.1:
//! - God Class: >20 methods or >500 LOC
//! - Long Method: >50 LOC or cyclomatic complexity >10
//! - Long Parameter List: >5 parameters
//!
//! # Example
//! ```ignore
//! use tldr_core::quality::smells::{detect_smells, ThresholdPreset};
//!
//! let report = detect_smells(Path::new("src/"), ThresholdPreset::Default, None, false)?;
//! for smell in &report.smells {
//!     println!("{}: {} in {}", smell.smell_type, smell.name, smell.file.display());
//! }
//! ```

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ast::extract::extract_file;
use crate::ast::parser::ParserPool;
use crate::callgraph::cross_file_types::{CallGraphIR, CallSite, CallType, FileIR, FuncDef};
use crate::metrics::calculate_all_complexities_file;
use crate::types::inheritance::InheritanceReport;
use crate::types::Language;
use crate::TldrResult;

// =============================================================================
// Types
// =============================================================================

/// Code smell types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmellType {
    /// Class with too many methods or LOC (>20 methods or >500 LOC)
    GodClass,
    /// Method with too many lines or too high complexity (>50 LOC or cyclomatic >10)
    LongMethod,
    /// Function with too many parameters (>5)
    LongParameterList,
    /// Feature Envy - method uses another class's data more than its own
    FeatureEnvy,
    /// Data Clumps - same group of data items appearing together
    DataClumps,
    /// Class with low cohesion (LCOM4 >= 2) - pulled from cohesion analyzer
    LowCohesion,
    /// Modules with tight coupling (score >= 0.6) - pulled from coupling analyzer
    TightCoupling,
    /// Unreachable functions - pulled from dead code analyzer
    DeadCode,
    /// Duplicate code blocks - pulled from similarity analyzer
    CodeClone,
    /// Functions with high cognitive complexity (>= 15) - pulled from complexity analyzer
    HighCognitiveComplexity,
    /// Function with nesting depth > 4 (nested control flow)
    DeepNesting,
    /// Class with many fields but few/no methods (just a data bag)
    DataClass,
    /// Class with only 1 method and 0-1 fields (too trivial for its own class)
    LazyElement,
    /// Long chains of method calls (a.b().c().d().e()) - high coupling to structure
    MessageChain,
    /// Function with many primitive-typed parameters instead of domain objects
    PrimitiveObsession,
    /// Class where >50% methods just delegate to another class
    MiddleMan,
    /// Subclass using <20% of inherited methods
    RefusedBequest,
    /// Two classes with bidirectional internal access
    InappropriateIntimacy,
}

impl std::fmt::Display for SmellType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SmellType::GodClass => write!(f, "God Class"),
            SmellType::LongMethod => write!(f, "Long Method"),
            SmellType::LongParameterList => write!(f, "Long Parameter List"),
            SmellType::FeatureEnvy => write!(f, "Feature Envy"),
            SmellType::DataClumps => write!(f, "Data Clumps"),
            SmellType::LowCohesion => write!(f, "Low Cohesion"),
            SmellType::TightCoupling => write!(f, "Tight Coupling"),
            SmellType::DeadCode => write!(f, "Dead Code"),
            SmellType::CodeClone => write!(f, "Code Clone"),
            SmellType::HighCognitiveComplexity => write!(f, "High Cognitive Complexity"),
            SmellType::DeepNesting => write!(f, "Deep Nesting"),
            SmellType::DataClass => write!(f, "Data Class"),
            SmellType::LazyElement => write!(f, "Lazy Element"),
            SmellType::MessageChain => write!(f, "Message Chain"),
            SmellType::PrimitiveObsession => write!(f, "Primitive Obsession"),
            SmellType::MiddleMan => write!(f, "Middle Man"),
            SmellType::RefusedBequest => write!(f, "Refused Bequest"),
            SmellType::InappropriateIntimacy => write!(f, "Inappropriate Intimacy"),
        }
    }
}

/// Threshold presets for smell detection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThresholdPreset {
    /// Strict thresholds for high-quality codebases
    Strict,
    /// Default thresholds (recommended)
    #[default]
    Default,
    /// Relaxed thresholds for legacy code
    Relaxed,
}

// =============================================================================
// Tier-2 Threshold Constants
// =============================================================================

/// Middle Man delegation ratio — Fowler's Refactoring (60% delegation = smell)
const MM_DELEGATION_RATIO_STRICT: f64 = 0.50;
const MM_DELEGATION_RATIO_DEFAULT: f64 = 0.60;
const MM_DELEGATION_RATIO_RELAXED: f64 = 0.75;
const MM_MIN_METHODS: usize = 3;

/// Refused Bequest usage ratio — Marinescu's BUR metric (<33% = smell)
const RB_USAGE_RATIO_STRICT: f64 = 0.33;
const RB_USAGE_RATIO_DEFAULT: f64 = 0.33;
const RB_USAGE_RATIO_RELAXED: f64 = 0.15;
const RB_MIN_INHERITED_STRICT: usize = 3;
const RB_MIN_INHERITED_DEFAULT: usize = 3;
const RB_MIN_INHERITED_RELAXED: usize = 5;

/// Feature Envy — adapted from Lanza-Marinescu ATFD metric
const FE_MIN_FOREIGN_STRICT: usize = 3;
const FE_MIN_FOREIGN_DEFAULT: usize = 4;
const FE_MIN_FOREIGN_RELAXED: usize = 5;
const FE_RATIO_STRICT: f64 = 1.5;
const FE_RATIO_DEFAULT: f64 = 2.0;
const FE_RATIO_RELAXED: f64 = 3.0;

/// Inappropriate Intimacy — adapted from CodeQL bidirectional coupling
const II_MIN_TOTAL_STRICT: usize = 6;
const II_MIN_TOTAL_DEFAULT: usize = 10;
const II_MIN_TOTAL_RELAXED: usize = 15;
const II_MIN_PER_DIR_STRICT: usize = 2;
const II_MIN_PER_DIR_DEFAULT: usize = 3;
const II_MIN_PER_DIR_RELAXED: usize = 4;

/// Thresholds for code smell detection
#[derive(Debug, Clone)]
pub struct Thresholds {
    /// Max methods in a class before God Class
    pub god_class_methods: usize,
    /// Max LOC in a class before God Class
    pub god_class_loc: usize,
    /// Max LOC in a method before Long Method
    pub long_method_loc: usize,
    /// Max cyclomatic complexity before Long Method
    pub long_method_complexity: u32,
    /// Max parameters before Long Parameter List
    pub long_param_count: usize,
    // Tier-2: Middle Man
    /// Minimum delegation ratio (non-constructor methods that are pure delegators)
    pub middle_man_delegation_ratio: f64,
    /// Minimum number of non-constructor methods before Middle Man is checked
    pub middle_man_min_methods: usize,
    // Tier-2: Refused Bequest
    /// Maximum usage ratio below which Refused Bequest triggers
    pub refused_bequest_usage_ratio: f64,
    /// Minimum inherited methods before Refused Bequest is checked
    pub refused_bequest_min_inherited: usize,
    // Tier-2: Feature Envy
    /// Minimum foreign accesses before Feature Envy is checked
    pub feature_envy_min_foreign: usize,
    /// Minimum ratio of foreign-to-own accesses
    pub feature_envy_ratio: f64,
    // Tier-2: Inappropriate Intimacy
    /// Minimum total bidirectional accesses
    pub intimacy_min_total: usize,
    /// Minimum accesses per direction
    pub intimacy_min_per_direction: usize,
}

impl Thresholds {
    /// Get thresholds for a preset
    pub fn from_preset(preset: ThresholdPreset) -> Self {
        match preset {
            ThresholdPreset::Strict => Self {
                god_class_methods: 10,
                god_class_loc: 250,
                long_method_loc: 25,
                long_method_complexity: 5,
                long_param_count: 3,
                // Tier-2: Strict
                middle_man_delegation_ratio: MM_DELEGATION_RATIO_STRICT,
                middle_man_min_methods: MM_MIN_METHODS,
                refused_bequest_usage_ratio: RB_USAGE_RATIO_STRICT,
                refused_bequest_min_inherited: RB_MIN_INHERITED_STRICT,
                feature_envy_min_foreign: FE_MIN_FOREIGN_STRICT,
                feature_envy_ratio: FE_RATIO_STRICT,
                intimacy_min_total: II_MIN_TOTAL_STRICT,
                intimacy_min_per_direction: II_MIN_PER_DIR_STRICT,
            },
            ThresholdPreset::Default => Self {
                god_class_methods: 20,
                god_class_loc: 500,
                long_method_loc: 50,
                long_method_complexity: 10,
                long_param_count: 5,
                // Tier-2: Default
                middle_man_delegation_ratio: MM_DELEGATION_RATIO_DEFAULT,
                middle_man_min_methods: MM_MIN_METHODS,
                refused_bequest_usage_ratio: RB_USAGE_RATIO_DEFAULT,
                refused_bequest_min_inherited: RB_MIN_INHERITED_DEFAULT,
                feature_envy_min_foreign: FE_MIN_FOREIGN_DEFAULT,
                feature_envy_ratio: FE_RATIO_DEFAULT,
                intimacy_min_total: II_MIN_TOTAL_DEFAULT,
                intimacy_min_per_direction: II_MIN_PER_DIR_DEFAULT,
            },
            ThresholdPreset::Relaxed => Self {
                god_class_methods: 30,
                god_class_loc: 1000,
                long_method_loc: 100,
                long_method_complexity: 15,
                long_param_count: 7,
                // Tier-2: Relaxed
                middle_man_delegation_ratio: MM_DELEGATION_RATIO_RELAXED,
                middle_man_min_methods: MM_MIN_METHODS,
                refused_bequest_usage_ratio: RB_USAGE_RATIO_RELAXED,
                refused_bequest_min_inherited: RB_MIN_INHERITED_RELAXED,
                feature_envy_min_foreign: FE_MIN_FOREIGN_RELAXED,
                feature_envy_ratio: FE_RATIO_RELAXED,
                intimacy_min_total: II_MIN_TOTAL_RELAXED,
                intimacy_min_per_direction: II_MIN_PER_DIR_RELAXED,
            },
        }
    }
}

/// A single code smell finding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmellFinding {
    /// Type of smell detected
    pub smell_type: SmellType,
    /// File containing the smell
    pub file: PathBuf,
    /// Name of the affected element (class or function)
    pub name: String,
    /// Line number where the smell starts
    pub line: u32,
    /// Human-readable reason for the smell
    pub reason: String,
    /// Severity level (1-3, higher is worse)
    pub severity: u8,
    /// Suggestion for fixing (only if requested)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

/// Report from smell detection
#[derive(Debug, Clone, Deserialize)]
pub struct SmellsReport {
    /// All detected smells
    pub smells: Vec<SmellFinding>,
    /// Number of files scanned
    pub files_scanned: usize,
    /// Smells grouped by file
    pub by_file: HashMap<PathBuf, Vec<SmellFinding>>,
    /// Summary statistics
    pub summary: SmellsSummary,
    /// Number of smells excluded because their source file matched a test-file
    /// convention (only populated when `walker_opts.include_tests == false`).
    /// Added in v0.2.3 (#1.D); `#[serde(default)]` keeps old daemon JSON
    /// payloads backward-compatible.
    #[serde(default)]
    pub excluded_test_smells: usize,
    /// Non-fatal advisory messages surfaced for the user (e.g. "8 smell
    /// analyzers require --deep flag"). Added in
    /// determinism-and-stderr-hygiene-v1 (BUG-18) to relocate the
    /// previously-stderr-only `--deep` hint into a structured field that
    /// JSON consumers can introspect AND that the text formatter renders
    /// to stdout. `#[serde(default)]` keeps cached daemon payloads
    /// backward-compatible.
    #[serde(default)]
    pub warnings: Vec<String>,
}

// residual-bugs-v1 (P15.AGG15-4): manual Serialize that mirrors
// `summary.total_smells` to a top-level `total_smells` key. Audit P15
// observed `tldr smells … --format json | jq '.total_smells'` returning
// `null` while `.summary.total_smells` returned the correct count
// across every supported language (rust/java/python/ts/scala/...).
// Mirroring at the top level matches the CLI's documented contract and
// keeps `total_*` consistent with `total_dead`/`total_findings`/etc on
// peer commands. The `summary` block is preserved unchanged so existing
// consumers that drill into `.summary.*` continue to work.
impl Serialize for SmellsReport {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("SmellsReport", 8)?;
        state.serialize_field("smells", &self.smells)?;
        state.serialize_field("files_scanned", &self.files_scanned)?;
        state.serialize_field("by_file", &self.by_file)?;
        state.serialize_field("summary", &self.summary)?;
        state.serialize_field("excluded_test_smells", &self.excluded_test_smells)?;
        state.serialize_field("warnings", &self.warnings)?;
        // Top-level mirror of `summary.total_smells` (P15.AGG15-4).
        state.serialize_field("total_smells", &self.summary.total_smells)?;
        // Top-level mirror of `summary.avg_smells_per_file` for symmetry.
        state.serialize_field("avg_smells_per_file", &self.summary.avg_smells_per_file)?;
        state.end()
    }
}

/// Summary statistics for smell detection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmellsSummary {
    /// Total number of smells found
    pub total_smells: usize,
    /// Count by smell type
    pub by_type: HashMap<String, usize>,
    /// Average smells per file
    pub avg_smells_per_file: f64,
}

/// Optional walker overrides for smell detection.
///
/// Passed to [`detect_smells_with_walker_opts`] to control how project
/// files are discovered. The defaults match the shared
/// [`crate::walker::ProjectWalker`] behavior: skip `node_modules`,
/// `target`, hidden dirs, and honor `.gitignore`.
#[derive(Debug, Default, Clone)]
pub struct SmellsWalkerOpts {
    /// If `true`, walk vendored/build directories (e.g. `node_modules`,
    /// `target`) that are normally skipped by default.
    pub no_default_ignore: bool,
    /// If `Some(lang)`, only scan files matching that language. If `None`,
    /// the directory walker auto-detects the project's *dominant* language
    /// via `Language::from_directory` and filters to that — matching the
    /// behaviour of `tldr structure` (analysis-precision-v1, BUG-12).
    /// Pre-fix `None` meant "scan every supported language", which caused
    /// `files_scanned` to disagree with `tldr structure` on mixed-language
    /// repos (e.g. a Rust project with a single Homebrew `.rb` formula).
    /// `None` + non-directory `path` (single file) still scans whatever
    /// language the file is.
    pub lang: Option<Language>,
    /// Caller-supplied file list. When non-empty, the walker is bypassed and
    /// only these paths are analyzed (filtered to supported languages).
    /// Added in v0.2.3 (#1.D) to support PR-focused scoping.
    pub files: Vec<PathBuf>,
    /// Include findings from test files. Default `false` (PR-review default).
    /// Implicit `true` when `files` is non-empty (caller picked the list).
    /// Added in v0.2.3 (#1.D).
    pub include_tests: bool,
}

// =============================================================================
// Main API
// =============================================================================

/// Detect code smells in a file or directory
///
/// # Arguments
/// * `path` - File or directory to scan
/// * `threshold` - Threshold preset (Strict, Default, Relaxed)
/// * `smell_type` - Optional filter for specific smell type
/// * `suggest` - Whether to include fix suggestions
///
/// # Returns
/// * `Ok(SmellsReport)` - Report with all detected smells
/// * `Err(TldrError)` - On file system or parse errors
///
/// # Example
/// ```ignore
/// use tldr_core::quality::smells::{detect_smells, ThresholdPreset, SmellType};
///
/// // Scan with default thresholds
/// let report = detect_smells(Path::new("src/"), ThresholdPreset::Default, None, false)?;
///
/// // Scan for specific smell type with suggestions
/// let report = detect_smells(
///     Path::new("src/"),
///     ThresholdPreset::Strict,
///     Some(SmellType::GodClass),
///     true,
/// )?;
/// ```
pub fn detect_smells(
    path: &Path,
    threshold: ThresholdPreset,
    smell_type: Option<SmellType>,
    suggest: bool,
) -> TldrResult<SmellsReport> {
    detect_smells_with_walker_opts(
        path,
        threshold,
        smell_type,
        suggest,
        SmellsWalkerOpts::default(),
    )
}

/// Detect code smells with explicit walker options.
///
/// Same as [`detect_smells`] but accepts a [`SmellsWalkerOpts`] to control
/// which directories are walked (e.g. disable vendor-dir skipping).
pub fn detect_smells_with_walker_opts(
    path: &Path,
    threshold: ThresholdPreset,
    smell_type: Option<SmellType>,
    suggest: bool,
    walker_opts: SmellsWalkerOpts,
) -> TldrResult<SmellsReport> {
    let thresholds = Thresholds::from_preset(threshold);
    // Max file size to analyze (500KB) - skip minified/generated files
    const MAX_FILE_SIZE: u64 = 500 * 1024;

    // Collect files to scan.
    //
    // v0.2.3 (#1.D): when `walker_opts.files` is non-empty, bypass the walker
    // entirely and use the explicit list (subject to language + size filters).
    let files: Vec<PathBuf> = if !walker_opts.files.is_empty() {
        let lang_filter = walker_opts.lang;
        walker_opts
            .files
            .iter()
            .filter(|p| p.is_file())
            .filter(|p| Language::from_path(p).is_some())
            // language-coverage-fixes-v1 (P4.BUG-N1, P4.BUG-N5): use
            // `matches_for_scan` to handle C++ `.h` and JS/TS sibling
            // extensions when an explicit lang is requested.
            .filter(|p| match lang_filter {
                Some(requested) => requested.matches_for_scan(p),
                None => true,
            })
            .filter(|p| {
                p.metadata()
                    .map(|m| m.len() <= MAX_FILE_SIZE)
                    .unwrap_or(true)
            })
            .map(|p| p.to_path_buf())
            .collect()
    } else if path.is_file() {
        vec![path.to_path_buf()]
    } else {
        // cross-cutting-and-clear-fix-bugs-v1 (P18.X4): pre-detect the
        // language BEFORE constructing the walker so we can pass a JS/TS
        // hint via `lang_hint`, which tells the walker to keep authored
        // sources under `build/`, `dist/`, etc. (JS/TS conventions place
        // generated *.d.ts files under `build/` or `dist/`, but TS source
        // sometimes lives there too — ts-dom-gen has `src/build/emitter.ts`
        // as its sole source file). Without the hint, the walker skips
        // `build/` and smells reports 0 results.
        let lang_filter = walker_opts.lang.or_else(|| Language::from_directory(path));
        let mut walker = crate::walker::ProjectWalker::new(path);
        if walker_opts.no_default_ignore {
            walker = walker.no_default_ignore();
        }
        if let Some(l) = lang_filter {
            walker = walker.lang_hint(l);
        }
        // language-coverage-fixes-v1 (P4.BUG-N1, P4.BUG-N5): when a
        // language is selected, use `matches_for_scan` so the C/C++
        // header ambiguity (`.h`) and the JS/TS sibling family
        // (`.jsx`/`.tsx`/`.cjs`/`.mjs`) are handled correctly. Falls
        // back to canonical `from_path` when no language is selected.
        let mut paths: Vec<PathBuf> = walker
            .iter()
            .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
            .filter(|e| match lang_filter {
                Some(requested) => requested.matches_for_scan(e.path()),
                None => Language::from_path(e.path()).is_some(),
            })
            .filter(|e| {
                e.metadata()
                    .map(|m| m.len() <= MAX_FILE_SIZE)
                    .unwrap_or(true)
            })
            .map(|e| e.path().to_path_buf())
            .collect();

        // analysis-precision-v1, BUG-12: defensive canonicalize + dedup so
        // that any future symlink / workspace-double-mount / nested-walker
        // scenario cannot inflate `files_scanned` past the true unique-file
        // count. `dunce::canonicalize` falls back to the literal path on
        // failure (e.g. broken symlinks), preserving previous behaviour.
        for p in paths.iter_mut() {
            if let Ok(c) = dunce::canonicalize(&*p) {
                *p = c;
            }
        }
        paths.sort();
        paths.dedup();
        paths
    };

    // Analyze files in parallel using rayon
    let file_results: Vec<Vec<SmellFinding>> = files
        .par_iter()
        .filter_map(|file_path| analyze_file(file_path, &thresholds, smell_type, suggest).ok())
        .collect();

    let files_scanned = file_results.len();
    let raw_smells: Vec<SmellFinding> = file_results.into_iter().flatten().collect();

    // v0.2.3 (#1.D): partition test-file findings out by default.
    // `--include-tests` (or non-empty `files` list) opts back in to repo-wide
    // behavior. Reuses the existing public helper at full path
    // `crate::analysis::clones::is_test_file` (NOT re-exported through
    // `analysis::mod.rs`).
    let (excluded_test_smells, smells): (usize, Vec<SmellFinding>) = if walker_opts.include_tests {
        (0, raw_smells)
    } else {
        let mut excluded = 0usize;
        let mut kept: Vec<SmellFinding> = Vec::with_capacity(raw_smells.len());
        for s in raw_smells {
            if crate::analysis::clones::is_test_file(&s.file) {
                excluded += 1;
            } else {
                kept.push(s);
            }
        }
        (excluded, kept)
    };

    // Group by file
    let mut by_file: HashMap<PathBuf, Vec<SmellFinding>> = HashMap::new();
    for smell in &smells {
        by_file
            .entry(smell.file.clone())
            .or_default()
            .push(smell.clone());
    }

    // Calculate summary
    let mut by_type: HashMap<String, usize> = HashMap::new();
    for smell in &smells {
        *by_type.entry(smell.smell_type.to_string()).or_insert(0) += 1;
    }

    let summary = SmellsSummary {
        total_smells: smells.len(),
        by_type,
        avg_smells_per_file: if files_scanned > 0 {
            smells.len() as f64 / files_scanned as f64
        } else {
            0.0
        },
    };

    Ok(SmellsReport {
        smells,
        files_scanned,
        by_file,
        summary,
        excluded_test_smells,
        warnings: Vec::new(),
    })
}

// =============================================================================
// Internal Implementation
// =============================================================================

/// Analyze a single file for smells
fn analyze_file(
    path: &Path,
    thresholds: &Thresholds,
    smell_filter: Option<SmellType>,
    suggest: bool,
) -> TldrResult<Vec<SmellFinding>> {
    let mut smells = Vec::new();

    let module_info = extract_file(path, None)?;

    if should_analyze_smell(smell_filter, SmellType::GodClass) {
        collect_god_class_smells(path, &module_info.classes, thresholds, suggest, &mut smells);
    }

    let complexity_map = calculate_all_complexities_file(path).unwrap_or_default();
    let all_functions = module_info
        .functions
        .iter()
        .chain(module_info.classes.iter().flat_map(|c| c.methods.iter()));
    for func in all_functions {
        if should_analyze_smell(smell_filter, SmellType::LongParameterList) {
            maybe_add_long_parameter_smell(path, func, thresholds, suggest, &mut smells);
        }
        if should_analyze_smell(smell_filter, SmellType::LongMethod) {
            maybe_add_long_method_smell(
                path,
                func,
                thresholds,
                suggest,
                &complexity_map,
                &mut smells,
            );
        }
    }

    let source = std::fs::read_to_string(path).unwrap_or_default();
    let lang_str = Language::from_path(path)
        .map(|l| format!("{:?}", l).to_lowercase())
        .unwrap_or_default();
    collect_tier1_ast_smells(path, &source, &lang_str, smell_filter, suggest, &mut smells);

    Ok(smells)
}

fn collect_god_class_smells(
    path: &Path,
    classes: &[crate::types::ClassInfo],
    thresholds: &Thresholds,
    suggest: bool,
    smells: &mut Vec<SmellFinding>,
) {
    for class in classes {
        let method_count = class.methods.len();
        let class_loc = estimate_class_loc(class);
        if method_count > thresholds.god_class_methods {
            smells.push(SmellFinding {
                smell_type: SmellType::GodClass,
                file: path.to_path_buf(),
                name: class.name.clone(),
                line: class.line_number,
                reason: format!(
                    "Class has {} methods (threshold: {})",
                    method_count, thresholds.god_class_methods
                ),
                severity: calculate_severity(method_count, thresholds.god_class_methods),
                suggestion: if suggest {
                    Some("Consider splitting this class into smaller, focused classes using the Single Responsibility Principle".to_string())
                } else {
                    None
                },
            });
            continue;
        }
        if class_loc > thresholds.god_class_loc {
            smells.push(SmellFinding {
                smell_type: SmellType::GodClass,
                file: path.to_path_buf(),
                name: class.name.clone(),
                line: class.line_number,
                reason: format!(
                    "Class has {} lines of code (threshold: {})",
                    class_loc, thresholds.god_class_loc
                ),
                severity: calculate_severity(class_loc, thresholds.god_class_loc),
                suggestion: if suggest {
                    Some(
                        "Consider extracting methods and responsibilities into separate classes"
                            .to_string(),
                    )
                } else {
                    None
                },
            });
        }
    }
}

fn maybe_add_long_parameter_smell(
    path: &Path,
    func: &crate::types::FunctionInfo,
    thresholds: &Thresholds,
    suggest: bool,
    smells: &mut Vec<SmellFinding>,
) {
    let param_count = func.params.len();
    if param_count <= thresholds.long_param_count {
        return;
    }
    smells.push(SmellFinding {
        smell_type: SmellType::LongParameterList,
        file: path.to_path_buf(),
        name: func.name.clone(),
        line: func.line_number,
        reason: format!(
            "Function has {} parameters (threshold: {})",
            param_count, thresholds.long_param_count
        ),
        severity: calculate_severity(param_count, thresholds.long_param_count),
        suggestion: if suggest {
            Some(
                "Consider using a parameter object or builder pattern to reduce parameters"
                    .to_string(),
            )
        } else {
            None
        },
    });
}

fn maybe_add_long_method_smell(
    path: &Path,
    func: &crate::types::FunctionInfo,
    thresholds: &Thresholds,
    suggest: bool,
    complexity_map: &std::collections::HashMap<String, crate::types::ComplexityMetrics>,
    smells: &mut Vec<SmellFinding>,
) {
    let Some(metrics) = complexity_map.get(&func.name) else {
        return;
    };
    if metrics.lines_of_code as usize > thresholds.long_method_loc {
        smells.push(SmellFinding {
            smell_type: SmellType::LongMethod,
            file: path.to_path_buf(),
            name: func.name.clone(),
            line: func.line_number,
            reason: format!(
                "Method has {} lines of code (threshold: {})",
                metrics.lines_of_code, thresholds.long_method_loc
            ),
            severity: calculate_severity(
                metrics.lines_of_code as usize,
                thresholds.long_method_loc,
            ),
            suggestion: if suggest {
                Some(
                    "Consider extracting parts of this method into smaller helper methods"
                        .to_string(),
                )
            } else {
                None
            },
        });
        return;
    }
    if metrics.cyclomatic > thresholds.long_method_complexity {
        smells.push(SmellFinding {
            smell_type: SmellType::LongMethod,
            file: path.to_path_buf(),
            name: func.name.clone(),
            line: func.line_number,
            reason: format!(
                "Method has cyclomatic complexity {} (threshold: {})",
                metrics.cyclomatic, thresholds.long_method_complexity
            ),
            severity: calculate_severity(
                metrics.cyclomatic as usize,
                thresholds.long_method_complexity as usize,
            ),
            suggestion: if suggest {
                Some("Consider simplifying control flow or extracting complex conditions into methods".to_string())
            } else {
                None
            },
        });
    }
}

fn collect_tier1_ast_smells(
    path: &Path,
    source: &str,
    lang_str: &str,
    smell_filter: Option<SmellType>,
    suggest: bool,
    smells: &mut Vec<SmellFinding>,
) {
    // Thread the path into every Tier 1 detector so TS/JS files get the
    // right grammar dialect (VAL-004). Without this, JSX produces an
    // error-laden AST and the message-chain detector goes exponential.
    let p = Some(path);
    if should_analyze_smell(smell_filter, SmellType::DeepNesting) {
        append_ast_findings(
            smells,
            detect_deep_nesting_with_path(source, lang_str, p),
            path,
            suggest,
            "Reduce nesting by extracting inner blocks into helper functions or using early returns",
        );
    }
    if should_analyze_smell(smell_filter, SmellType::DataClass) {
        append_ast_findings(
            smells,
            detect_data_classes_with_path(source, lang_str, p),
            path,
            suggest,
            "Consider adding behavior methods or converting to a plain data structure (dataclass, struct, record)",
        );
    }
    if should_analyze_smell(smell_filter, SmellType::LazyElement) {
        append_ast_findings(
            smells,
            detect_lazy_elements_with_path(source, lang_str, p),
            path,
            suggest,
            "Consider inlining this class into its caller or merging with a related class",
        );
    }
    if should_analyze_smell(smell_filter, SmellType::MessageChain) {
        append_ast_findings(
            smells,
            detect_message_chains_with_path(source, lang_str, p),
            path,
            suggest,
            "Apply the Law of Demeter: hide the chain behind a single method call",
        );
    }
    if should_analyze_smell(smell_filter, SmellType::PrimitiveObsession) {
        append_ast_findings(
            smells,
            detect_primitive_obsession_with_path(source, lang_str, p),
            path,
            suggest,
            "Introduce domain types (value objects) instead of passing raw primitives",
        );
    }
}

fn append_ast_findings(
    smells: &mut Vec<SmellFinding>,
    mut findings: Vec<SmellFinding>,
    path: &Path,
    suggest: bool,
    suggestion: &str,
) {
    for finding in &mut findings {
        finding.file = path.to_path_buf();
        if suggest {
            finding.suggestion = Some(suggestion.to_string());
        }
    }
    smells.extend(findings);
}

/// Estimate LOC for a class based on method line numbers
fn estimate_class_loc(class: &crate::types::ClassInfo) -> usize {
    if class.methods.is_empty() {
        return 0;
    }

    let min_line = class.line_number;
    let max_line = class
        .methods
        .iter()
        .map(|m| m.line_number)
        .max()
        .unwrap_or(min_line);

    // Rough estimate: last method line + some buffer
    (max_line - min_line + 20) as usize
}

/// Calculate severity (1-3) based on how much the threshold is exceeded
fn calculate_severity(value: usize, threshold: usize) -> u8 {
    let ratio = value as f64 / threshold as f64;
    if ratio > 2.0 {
        3 // Very severe
    } else if ratio > 1.5 {
        2 // Moderate
    } else {
        1 // Mild
    }
}

// =============================================================================
// Aggregated Severity Helpers
// =============================================================================

/// Calculate severity for low cohesion findings based on LCOM4 value.
///
/// - LCOM4 >= 6: severity 3 (very fragmented class)
/// - LCOM4 >= 4: severity 2 (moderately fragmented)
/// - LCOM4 >= 2: severity 1 (slightly fragmented)
pub(crate) fn cohesion_severity(lcom4: usize) -> u8 {
    if lcom4 >= 6 {
        3
    } else if lcom4 >= 4 {
        2
    } else {
        1
    }
}

/// Calculate severity for tight coupling findings based on coupling score.
///
/// - score >= 0.8: severity 2 (very tight)
/// - score >= 0.6: severity 1 (tight)
pub(crate) fn coupling_severity(score: f64) -> u8 {
    if score >= 0.8 {
        2
    } else {
        1
    }
}

/// Calculate severity for high cognitive complexity findings.
///
/// - cognitive >= 30: severity 3 (extremely complex)
/// - cognitive >= 20: severity 2 (very complex)
/// - cognitive >= 15: severity 1 (complex)
pub(crate) fn cognitive_severity(cognitive: usize) -> u8 {
    if cognitive >= 30 {
        3
    } else if cognitive >= 20 {
        2
    } else {
        1
    }
}

/// Calculate severity for code clone findings based on similarity score.
///
/// - score > 0.8: severity 2 (near-duplicate)
/// - score > 0.6: severity 1 (similar)
pub(crate) fn clone_severity(score: f64) -> u8 {
    if score > 0.8 {
        2
    } else {
        1
    }
}

// =============================================================================
// New Tier-1 Smell Severity Helpers
// =============================================================================

/// Calculate severity for deep nesting findings.
///
/// - depth >= 8: severity 3 (extremely nested)
/// - depth >= 6: severity 2 (very nested)
/// - depth >= 5: severity 1 (nested)
pub(crate) fn nesting_severity(depth: usize) -> u8 {
    if depth >= 8 {
        3
    } else if depth >= 6 {
        2
    } else {
        1
    }
}

/// Calculate severity for data class findings.
///
/// - fields >= 8 AND methods == 0: severity 2 (pure data bag)
/// - fields >= 4 AND methods <= 2: severity 1 (likely data class)
pub(crate) fn data_class_severity(field_count: usize, method_count: usize) -> u8 {
    if field_count >= 8 && method_count == 0 {
        2
    } else {
        1
    }
}

/// Calculate severity for message chain findings.
///
/// - chain >= 6: severity 2 (very long chain)
/// - chain >= 4: severity 1 (long chain)
pub(crate) fn chain_severity(chain_length: usize) -> u8 {
    if chain_length >= 6 {
        2
    } else {
        1
    }
}

/// Calculate severity for primitive obsession findings.
///
/// - primitives >= 6: severity 2 (many primitives)
/// - primitives >= 4: severity 1 (some primitives)
pub(crate) fn primitive_obsession_severity(primitive_count: usize) -> u8 {
    if primitive_count >= 6 {
        2
    } else {
        1
    }
}

// =============================================================================
// Tier-2 Fowler Smell Severity Helpers
// =============================================================================

/// Calculate severity for Middle Man findings based on delegation ratio and count.
///
/// - ratio >= 0.90 AND delegation_count >= 5: severity 3 (near-total delegation)
/// - ratio >= 0.75 OR delegation_count >= 4: severity 2 (heavy delegation)
/// - otherwise: severity 1 (moderate delegation)
pub(crate) fn middle_man_severity(delegation_ratio: f64, delegation_count: usize) -> u8 {
    if delegation_ratio >= 0.90 && delegation_count >= 5 {
        3
    } else if delegation_ratio >= 0.75 || delegation_count >= 4 {
        2
    } else {
        1
    }
}

/// Calculate severity for Refused Bequest findings based on usage ratio and total inherited.
///
/// - usage_ratio == 0.0 AND total_inherited >= 5: severity 3 (uses nothing)
/// - usage_ratio < 0.10 OR (usage_ratio == 0.0 AND total_inherited >= 3): severity 2
/// - otherwise: severity 1
pub(crate) fn refused_bequest_severity(usage_ratio: f64, total_inherited: usize) -> u8 {
    if usage_ratio == 0.0 && total_inherited >= 5 {
        3
    } else if usage_ratio < 0.10 || (usage_ratio == 0.0 && total_inherited >= 3) {
        2
    } else {
        1
    }
}

/// Calculate severity for Feature Envy findings based on foreign vs own access counts.
///
/// - foreign >= 8 AND ratio > 4.0: severity 3 (extreme envy)
/// - foreign >= 5 AND ratio > 2.5: severity 2 (strong envy)
/// - otherwise: severity 1 (mild envy)
pub(crate) fn feature_envy_severity(foreign: usize, own: usize) -> u8 {
    let ratio = foreign as f64 / (own.max(1)) as f64;
    if foreign >= 8 && ratio > 4.0 {
        3
    } else if foreign >= 5 && ratio > 2.5 {
        2
    } else {
        1
    }
}

/// Calculate severity for Inappropriate Intimacy findings based on total accesses
/// and minimum per-direction count.
///
/// - total >= 20 AND min_direction >= 5: severity 3 (extreme intimacy)
/// - total >= 12 AND min_direction >= 3: severity 2 (strong intimacy)
/// - otherwise: severity 1 (mild intimacy)
pub(crate) fn intimacy_severity(total_accesses: usize, min_direction_count: usize) -> u8 {
    if total_accesses >= 20 && min_direction_count >= 5 {
        3
    } else if total_accesses >= 12 && min_direction_count >= 3 {
        2
    } else {
        1
    }
}

// =============================================================================
// Tier-2 Helper Functions
// =============================================================================

/// Get methods for a class, handling Go/Rust where ClassDef.methods is empty.
///
/// Strategy:
/// 1. Try ClassDef.methods first (works for Python, TypeScript, Java)
/// 2. Fall back to filtering FuncDef entries where class_name matches (Go, Rust)
/// 3. Deduplicate by function name
fn get_class_methods_robust<'a>(file_ir: &'a FileIR, class_name: &str) -> Vec<&'a FuncDef> {
    let class_def = file_ir.get_class(class_name);
    let has_methods_list = class_def.map(|c| !c.methods.is_empty()).unwrap_or(false);

    if has_methods_list {
        // Use ClassDef.methods to find matching FuncDefs
        let method_names: HashSet<&str> = class_def
            .unwrap()
            .methods
            .iter()
            .map(|m| m.as_str())
            .collect();
        let mut seen = HashSet::new();
        file_ir
            .funcs
            .iter()
            .filter(|f| {
                f.class_name.as_deref() == Some(class_name)
                    || method_names.contains(f.name.as_str())
            })
            .filter(|f| seen.insert(&f.name))
            .collect()
    } else {
        // Fallback for Go/Rust: join FuncDef by class_name
        file_ir
            .funcs
            .iter()
            .filter(|f| f.class_name.as_deref() == Some(class_name))
            .collect()
    }
}

/// Returns true if the method name is a constructor for the given language.
///
/// Recognized constructors:
/// - Python: `__init__`
/// - JavaScript/TypeScript/TSX/JSX: `constructor`
/// - Rust: `new`
/// - Go: names starting with `New`
/// - Ruby: `initialize`
/// - PHP: `__construct`
/// - Swift: `init`
/// - Scala: `<init>` or `this`
/// - Java/C#/Kotlin: cannot determine without class name (returns false)
/// - C/C++: cannot determine without class name (returns false)
/// - Elixir/Lua: no traditional constructors (returns false)
fn is_constructor(name: &str, language: &str) -> bool {
    match language {
        "python" | "py" => name == "__init__",
        "javascript" | "typescript" | "tsx" | "jsx" | "js" | "ts" => name == "constructor",
        "rust" | "rs" => name == "new",
        "go" => name.starts_with("New"),
        "ruby" | "rb" => name == "initialize",
        "php" => name == "__construct",
        "swift" => name == "init",
        "scala" => name == "<init>" || name == "this",
        "java" | "csharp" | "cs" | "kotlin" | "kt" => false,
        "c" | "cpp" | "c++" => false,
        "elixir" | "ex" | "lua" => false,
        _ => {
            name == "__init__"
                || name == "constructor"
                || name == "new"
                || name == "initialize"
                || name == "__construct"
                || name == "init"
        }
    }
}

/// Returns true if the receiver name is a self-reference for the given language.
///
/// - Python/Rust/Ruby/Swift: `self`
/// - TypeScript/JavaScript/Java/C#/Kotlin/Scala/C++/PHP: `this`
/// - Go: receiver is a named variable (neither `self` nor `this`)
/// - C/Elixir/Lua: no self-reference concept
/// - Unknown: either `self` or `this`
fn is_self_reference(receiver: &str, language: &str) -> bool {
    match language {
        "python" | "py" | "rust" | "rs" | "ruby" | "rb" | "swift" => receiver == "self",
        "typescript" | "javascript" | "tsx" | "jsx" | "ts" | "js" | "java" | "csharp" | "cs"
        | "kotlin" | "kt" | "scala" | "cpp" | "c++" | "php" => receiver == "this",
        "go" | "c" | "elixir" | "ex" | "lua" => false,
        _ => receiver == "self" || receiver == "this",
    }
}

// =============================================================================
// New Tier-1 AST-based Smell Detectors
// =============================================================================

/// Resolve a language string to a `Language` enum value.
fn resolve_language(lang_str: &str) -> Option<Language> {
    match lang_str.to_lowercase().as_str() {
        "python" | "py" => Some(Language::Python),
        "rust" | "rs" => Some(Language::Rust),
        "typescript" | "ts" => Some(Language::TypeScript),
        "javascript" | "js" => Some(Language::JavaScript),
        "go" => Some(Language::Go),
        "java" => Some(Language::Java),
        "c" => Some(Language::C),
        "cpp" | "c++" => Some(Language::Cpp),
        "ruby" | "rb" => Some(Language::Ruby),
        "csharp" | "c#" | "cs" => Some(Language::CSharp),
        "scala" => Some(Language::Scala),
        "php" => Some(Language::Php),
        "lua" => Some(Language::Lua),
        "kotlin" | "kt" => Some(Language::Kotlin),
        "elixir" | "ex" => Some(Language::Elixir),
        _ => None,
    }
}

/// Parse source code into a tree-sitter tree, returning None on failure.
///
/// When `path` is `Some` the parser uses the file extension to pick the
/// right TS/JS grammar dialect. This is critical for `.tsx` / `.jsx`
/// files — without it the TS grammar produces hundreds of ERROR nodes
/// and the downstream smell detectors go pathological (VAL-004).
fn parse_source(
    source: &str,
    lang_str: &str,
    path: Option<&Path>,
) -> Option<(tree_sitter::Tree, Language)> {
    let lang = resolve_language(lang_str)?;
    let pool = ParserPool::new();
    pool.parse_with_path(source, lang, path)
        .ok()
        .map(|tree| (tree, lang))
}

/// Check if a tree-sitter node kind represents a control flow construct that increases nesting.
fn is_nesting_node(kind: &str) -> bool {
    matches!(
        kind,
        // Common across languages
        "if_statement" | "if_expression" |
        "for_statement" | "for_expression" |
        "while_statement" | "while_expression" |
        "try_statement" | "try_expression" |
        "with_statement" |
        "match_statement" | "match_expression" |
        // Rust-specific
        "if_let_expression" |
        "loop_expression" |
        // Go
        "for_clause" |
        // Java / C# / TypeScript
        "for_in_statement" |
        "switch_statement" | "switch_expression" |
        "do_statement" |
        "catch_clause" |
        // Generic
        "try_catch_statement" |
        "except_clause"
    )
}

/// Detect deep nesting in source code using AST analysis.
///
/// Walks the tree-sitter AST, tracking nesting depth of control flow nodes.
/// Reports any function where max nesting depth >= 5.
///
/// # Arguments
/// * `source` - Source code string
/// * `language` - Language name (e.g., "python", "rust")
///
/// # Returns
/// A vector of `SmellFinding` for each function with deep nesting.
pub fn detect_deep_nesting(source: &str, language: &str) -> Vec<SmellFinding> {
    detect_deep_nesting_with_path(source, language, None)
}

/// Path-aware variant of [`detect_deep_nesting`]. When `path` is `Some`
/// and the file extension indicates a TS/JS dialect, the TSX grammar is
/// used — preventing JSX files from entering error-recovery mode.
pub fn detect_deep_nesting_with_path(
    source: &str,
    language: &str,
    path: Option<&Path>,
) -> Vec<SmellFinding> {
    let (tree, _lang) = match parse_source(source, language, path) {
        Some(v) => v,
        None => return Vec::new(),
    };

    let root = tree.root_node();
    let mut findings = Vec::new();

    // Find all function nodes, then measure max nesting depth within each
    find_functions_and_measure_nesting(root, source, &mut findings);

    findings
}

/// Recursively find function-like nodes and measure their nesting depth.
fn find_functions_and_measure_nesting(
    node: tree_sitter::Node,
    source: &str,
    findings: &mut Vec<SmellFinding>,
) {
    let kind = node.kind();
    let is_function = matches!(
        kind,
        "function_definition"
            | "function_declaration"
            | "function_item"
            | "method_definition"
            | "method_declaration"
            | "arrow_function"
            | "function"
            | "closure_expression"
            | "function_expression"
            | "generator_function"
            | "async_function"
            | "function_def"
    );

    if is_function {
        // Get function name
        let func_name =
            extract_function_name(node, source).unwrap_or_else(|| "<anonymous>".to_string());
        let line = node.start_position().row as u32 + 1;

        // Measure max nesting depth within this function's body
        let max_depth = measure_max_nesting_depth(node, 0);

        if max_depth >= 5 {
            findings.push(SmellFinding {
                smell_type: SmellType::DeepNesting,
                file: PathBuf::from("<source>"),
                name: func_name,
                line,
                reason: format!("Function has nesting depth {} (threshold: 5)", max_depth),
                severity: nesting_severity(max_depth),
                suggestion: None,
            });
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // Don't recurse into nested functions - they get their own analysis
        if !is_function
            || !matches!(
                child.kind(),
                "function_definition"
                    | "function_declaration"
                    | "function_item"
                    | "method_definition"
                    | "method_declaration"
            )
        {
            find_functions_and_measure_nesting(child, source, findings);
        }
    }
}

/// Measure the maximum nesting depth of control flow nodes within a subtree.
fn measure_max_nesting_depth(node: tree_sitter::Node, current_depth: usize) -> usize {
    let kind = node.kind();
    let new_depth = if is_nesting_node(kind) {
        current_depth + 1
    } else {
        current_depth
    };

    let mut max_depth = new_depth;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let child_max = measure_max_nesting_depth(child, new_depth);
        if child_max > max_depth {
            max_depth = child_max;
        }
    }

    max_depth
}

/// Extract function name from a tree-sitter function node.
fn extract_function_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    // Try "name" field first (most languages)
    if let Some(name_node) = node.child_by_field_name("name") {
        let name = &source[name_node.byte_range()];
        return Some(name.to_string());
    }
    None
}

/// Detect data classes: classes with many fields but few/no methods.
///
/// A class is considered a "data class" if it has >= 4 fields and <= 2 methods
/// (or the methods/fields ratio is < 0.5).
///
/// # Arguments
/// * `source` - Source code string
/// * `language` - Language name
///
/// # Returns
/// A vector of `SmellFinding` for each data class detected.
pub fn detect_data_classes(source: &str, language: &str) -> Vec<SmellFinding> {
    detect_data_classes_with_path(source, language, None)
}

/// Path-aware variant of [`detect_data_classes`].
pub fn detect_data_classes_with_path(
    source: &str,
    language: &str,
    path: Option<&Path>,
) -> Vec<SmellFinding> {
    let (tree, _lang) = match parse_source(source, language, path) {
        Some(v) => v,
        None => return Vec::new(),
    };

    let root = tree.root_node();
    let mut findings = Vec::new();

    find_classes_and_check_data_class(root, source, &mut findings);

    findings
}

/// Recursively find class nodes and check if they are data classes.
fn find_classes_and_check_data_class(
    node: tree_sitter::Node,
    source: &str,
    findings: &mut Vec<SmellFinding>,
) {
    let kind = node.kind();
    let is_class = matches!(
        kind,
        "class_definition"
            | "class_declaration"
            | "struct_item"
            | "struct_declaration"
            | "interface_declaration"
    );

    if is_class {
        let class_name =
            extract_class_name(node, source).unwrap_or_else(|| "<unknown>".to_string());
        let line = node.start_position().row as u32 + 1;

        let (field_count, method_count) = count_class_members(node, source);

        // Data class: many fields, few methods
        if field_count >= 4 && method_count <= 2 {
            let ratio = if field_count > 0 {
                method_count as f64 / field_count as f64
            } else {
                0.0
            };

            if ratio < 0.5 {
                findings.push(SmellFinding {
                    smell_type: SmellType::DataClass,
                    file: PathBuf::from("<source>"),
                    name: class_name,
                    line,
                    reason: format!(
                        "Class has {} fields and {} methods (data bag, ratio {:.2})",
                        field_count, method_count, ratio
                    ),
                    severity: data_class_severity(field_count, method_count),
                    suggestion: None,
                });
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_classes_and_check_data_class(child, source, findings);
    }
}

/// Extract class name from a tree-sitter node.
fn extract_class_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    if let Some(name_node) = node.child_by_field_name("name") {
        let name = &source[name_node.byte_range()];
        return Some(name.to_string());
    }
    None
}

/// Count fields and methods in a class node.
///
/// Fields are identified by assignment patterns in __init__ (Python),
/// field_declaration nodes (Java/TS/Rust), etc.
/// Methods are function/method definitions inside the class.
fn count_class_members(node: tree_sitter::Node, source: &str) -> (usize, usize) {
    let mut field_count = 0;
    let mut method_count = 0;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();
        match kind {
            // Direct method definitions
            "function_definition"
            | "function_declaration"
            | "method_definition"
            | "method_declaration"
            | "function_item" => {
                method_count += 1;
                // For Python __init__, count self.x = ... assignments as fields
                let func_name = extract_function_name(child, source);
                if func_name.as_deref() == Some("__init__") {
                    field_count += count_self_assignments(child, source);
                }
            }
            // Field declarations (Java, TypeScript, Rust struct fields)
            "field_declaration"
            | "field_definition"
            | "property_declaration"
            | "public_field_definition"
            | "class_variable" => {
                field_count += 1;
            }
            // Rust struct body members
            "field_declaration_list" => {
                let mut inner_cursor = child.walk();
                for inner in child.children(&mut inner_cursor) {
                    if inner.kind() == "field_declaration" {
                        field_count += 1;
                    }
                }
            }
            // Class body (Python, etc.) - recurse
            "class_body" | "block" | "declaration_list" | "class_heritage" => {
                let (f, m) = count_class_members(child, source);
                field_count += f;
                method_count += m;
            }
            _ => {}
        }
    }

    (field_count, method_count)
}

/// Count `self.x = ...` assignments in a Python __init__ method as field indicators.
fn count_self_assignments(node: tree_sitter::Node, source: &str) -> usize {
    let mut count = 0;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "expression_statement" || child.kind() == "assignment" {
            let text = &source[child.byte_range()];
            if text.starts_with("self.") || text.contains("self.") {
                // Count distinct self.attr = patterns
                count += text.matches("self.").count().min(1);
            }
        }
        // Recurse into blocks / function body
        count += count_self_assignments(child, source);
    }
    count
}

/// Detect lazy elements: classes with only 1 method and 0-1 fields.
///
/// A class this small likely doesn't justify its own abstraction.
///
/// # Arguments
/// * `source` - Source code string
/// * `language` - Language name
///
/// # Returns
/// A vector of `SmellFinding` for each lazy element detected.
pub fn detect_lazy_elements(source: &str, language: &str) -> Vec<SmellFinding> {
    detect_lazy_elements_with_path(source, language, None)
}

/// Path-aware variant of [`detect_lazy_elements`].
pub fn detect_lazy_elements_with_path(
    source: &str,
    language: &str,
    path: Option<&Path>,
) -> Vec<SmellFinding> {
    let (tree, _lang) = match parse_source(source, language, path) {
        Some(v) => v,
        None => return Vec::new(),
    };

    let root = tree.root_node();
    let mut findings = Vec::new();

    find_classes_and_check_lazy(root, source, &mut findings);

    findings
}

/// Recursively find class nodes and check if they are lazy elements.
fn find_classes_and_check_lazy(
    node: tree_sitter::Node,
    source: &str,
    findings: &mut Vec<SmellFinding>,
) {
    let kind = node.kind();
    let is_class = matches!(
        kind,
        "class_definition" | "class_declaration" | "struct_item" | "struct_declaration"
    );

    if is_class {
        let class_name =
            extract_class_name(node, source).unwrap_or_else(|| "<unknown>".to_string());
        let line = node.start_position().row as u32 + 1;

        let (field_count, method_count) = count_class_members(node, source);

        if method_count <= 1 && field_count <= 1 {
            findings.push(SmellFinding {
                smell_type: SmellType::LazyElement,
                file: PathBuf::from("<source>"),
                name: class_name,
                line,
                reason: format!(
                    "Class has only {} method(s) and {} field(s) - may not justify its own class",
                    method_count, field_count
                ),
                severity: 1, // Always low severity
                suggestion: None,
            });
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_classes_and_check_lazy(child, source, findings);
    }
}

/// Detect message chains: long chains of method/attribute access.
///
/// Looks for chains of `.member` access deeper than 3 levels,
/// indicating tight coupling to an object's internal structure.
///
/// # Arguments
/// * `source` - Source code string
/// * `language` - Language name
///
/// # Returns
/// A vector of `SmellFinding` for each message chain detected.
pub fn detect_message_chains(source: &str, language: &str) -> Vec<SmellFinding> {
    detect_message_chains_with_path(source, language, None)
}

/// Path-aware variant of [`detect_message_chains`].
///
/// This is the critical path for VAL-004: on JSX files the TS grammar
/// produces an error-laden AST that sends [`find_message_chains`] into
/// pathological, near-exponential traversal. Threading the path through
/// so the TSX grammar is picked keeps the detector linear.
pub fn detect_message_chains_with_path(
    source: &str,
    language: &str,
    path: Option<&Path>,
) -> Vec<SmellFinding> {
    let (tree, _lang) = match parse_source(source, language, path) {
        Some(v) => v,
        None => return Vec::new(),
    };

    let root = tree.root_node();
    let mut findings = Vec::new();
    let mut visited_lines: std::collections::HashSet<u32> = std::collections::HashSet::new();

    find_message_chains(root, source, &mut findings, &mut visited_lines);

    findings
}

/// Recursively find message chains in the AST.
fn find_message_chains(
    node: tree_sitter::Node,
    source: &str,
    findings: &mut Vec<SmellFinding>,
    visited_lines: &mut std::collections::HashSet<u32>,
) {
    let kind = node.kind();

    // Look for attribute/member access patterns
    let is_chain_node = matches!(
        kind,
        "attribute"
            | "member_expression"
            | "field_expression"
            | "call_expression"
            | "method_invocation"
            | "call"
    );

    if is_chain_node {
        let chain_length = measure_chain_length(node);
        let line = node.start_position().row as u32 + 1;

        if chain_length > 3 && !visited_lines.contains(&line) {
            visited_lines.insert(line);
            let chain_text = &source[node.byte_range()];
            let truncated = if chain_text.len() > 60 {
                format!("{}...", &chain_text[..57])
            } else {
                chain_text.to_string()
            };

            findings.push(SmellFinding {
                smell_type: SmellType::MessageChain,
                file: PathBuf::from("<source>"),
                name: truncated,
                line,
                reason: format!("Method chain of length {} (threshold: 3)", chain_length),
                severity: chain_severity(chain_length),
                suggestion: None,
            });
            // Don't recurse into children of this chain - we already counted it
            return;
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_message_chains(child, source, findings, visited_lines);
    }
}

/// Measure the length of a method/attribute chain by walking down the AST.
fn measure_chain_length(node: tree_sitter::Node) -> usize {
    let kind = node.kind();
    let is_access = matches!(
        kind,
        "attribute"
            | "member_expression"
            | "field_expression"
            | "call_expression"
            | "method_invocation"
            | "call"
    );

    if !is_access {
        return 0;
    }

    // The "object" or "value" or "function" child is the part before the dot
    let child_chain = node
        .child_by_field_name("object")
        .or_else(|| node.child_by_field_name("value"))
        .or_else(|| node.child_by_field_name("function"))
        .map(|c| measure_chain_length(c))
        .unwrap_or(0);

    // For call_expression, look at arguments' parent
    if kind == "call_expression" || kind == "call" {
        // The function being called is the chain part
        if let Some(func) = node.child_by_field_name("function") {
            return measure_chain_length(func);
        }
        // Fallback: first child
        if let Some(first) = node.child(0) {
            return measure_chain_length(first);
        }
    }

    1 + child_chain
}

/// Set of primitive type names across languages.
const PRIMITIVE_TYPES: &[&str] = &[
    // Python
    "int", "float", "str", "bool", "bytes", // Rust
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32",
    "f64", "String", "&str", "char", // TypeScript/JavaScript
    "number", "string", "boolean", // Java / C#
    "byte", "short", "long", "double", "Integer", "Long", "Double", "Float", "Boolean",
    // Go
    "int8", "int16", "int32", "int64", "uint8", "uint16", "uint32", "uint64", "float32", "float64",
];

/// Check if a type string is a primitive type.
fn is_primitive_type(type_str: &str) -> bool {
    let trimmed = type_str.trim();
    // Handle reference types like &str, &mut str
    let base = trimmed.trim_start_matches('&').trim_start_matches("mut ");
    PRIMITIVE_TYPES.contains(&base)
}

/// Detect primitive obsession: functions with many primitive-typed parameters.
///
/// Counts parameters with primitive type annotations. If more than 3
/// primitives are found, it's flagged as a smell.
///
/// # Arguments
/// * `source` - Source code string
/// * `language` - Language name
///
/// # Returns
/// A vector of `SmellFinding` for each function with primitive obsession.
pub fn detect_primitive_obsession(source: &str, language: &str) -> Vec<SmellFinding> {
    detect_primitive_obsession_with_path(source, language, None)
}

/// Path-aware variant of [`detect_primitive_obsession`].
pub fn detect_primitive_obsession_with_path(
    source: &str,
    language: &str,
    path: Option<&Path>,
) -> Vec<SmellFinding> {
    let (tree, _lang) = match parse_source(source, language, path) {
        Some(v) => v,
        None => return Vec::new(),
    };

    let root = tree.root_node();
    let mut findings = Vec::new();

    find_functions_and_check_primitives(root, source, &mut findings);

    findings
}

/// Recursively find functions and check for primitive obsession.
fn find_functions_and_check_primitives(
    node: tree_sitter::Node,
    source: &str,
    findings: &mut Vec<SmellFinding>,
) {
    let kind = node.kind();
    let is_function = matches!(
        kind,
        "function_definition"
            | "function_declaration"
            | "function_item"
            | "method_definition"
            | "method_declaration"
            | "arrow_function"
            | "function"
    );

    if is_function {
        let func_name =
            extract_function_name(node, source).unwrap_or_else(|| "<anonymous>".to_string());
        let line = node.start_position().row as u32 + 1;

        let primitive_count = count_primitive_params(node, source);

        if primitive_count > 3 {
            findings.push(SmellFinding {
                smell_type: SmellType::PrimitiveObsession,
                file: PathBuf::from("<source>"),
                name: func_name,
                line,
                reason: format!(
                    "Function has {} primitive parameters (threshold: 3)",
                    primitive_count
                ),
                severity: primitive_obsession_severity(primitive_count),
                suggestion: None,
            });
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_functions_and_check_primitives(child, source, findings);
    }
}

/// Count the number of primitive-typed parameters in a function node.
fn count_primitive_params(node: tree_sitter::Node, source: &str) -> usize {
    // Look for the parameters field
    let params_node = match node.child_by_field_name("parameters") {
        Some(p) => p,
        None => return 0,
    };

    let mut count = 0;
    let mut cursor = params_node.walk();
    for param in params_node.children(&mut cursor) {
        let param_kind = param.kind();
        // Skip self/this parameters and delimiters
        if param_kind == "self" || param_kind == "," || param_kind == "(" || param_kind == ")" {
            continue;
        }

        // Look for type annotation in the parameter
        if let Some(type_node) = param.child_by_field_name("type") {
            let type_text = &source[type_node.byte_range()];
            if is_primitive_type(type_text) {
                count += 1;
            }
        }
        // Python typed_parameter: look for "type" child
        else if param_kind == "typed_parameter" || param_kind == "typed_default_parameter" {
            // Try to find type annotation child
            let mut inner_cursor = param.walk();
            for child in param.children(&mut inner_cursor) {
                if child.kind() == "type" {
                    let type_text = &source[child.byte_range()];
                    if is_primitive_type(type_text) {
                        count += 1;
                    }
                }
            }
        }
    }

    count
}

// =============================================================================
// Tier-2 Fowler Smell Detectors
// =============================================================================

// --- Source-based stubs (backward compatibility for existing contract tests) ---
// Tier-2 smells require --deep mode with CallGraphIR/InheritanceReport.
// These stubs exist so that detect_middle_man(source, lang) still compiles,
// but they return empty since source-only detection is not supported.

/// Detect Middle Man smell from source string.
///
/// This is a backward-compatible stub. Tier-2 Middle Man detection requires
/// `--deep` mode. Use `detect_middle_man_from_callgraph()` for proper detection.
#[deprecated(
    since = "0.2.0",
    note = "Use detect_middle_man_from_callgraph() with --deep mode instead"
)]
pub fn detect_middle_man(_source: &str, _language: &str) -> Vec<SmellFinding> {
    // Tier-2 smells require --deep mode with CallGraphIR
    Vec::new()
}

/// Detect Refused Bequest smell from source string.
///
/// This is a backward-compatible stub. Tier-2 Refused Bequest detection requires
/// `--deep` mode. Use `detect_refused_bequest_from_callgraph()` for proper detection.
#[deprecated(
    since = "0.2.0",
    note = "Use detect_refused_bequest_from_callgraph() with --deep mode instead"
)]
pub fn detect_refused_bequest(_source: &str, _language: &str) -> Vec<SmellFinding> {
    // Tier-2 smells require --deep mode with CallGraphIR + InheritanceReport
    Vec::new()
}

/// Detect Feature Envy smell from source string.
///
/// This is a backward-compatible stub. Tier-2 Feature Envy detection requires
/// `--deep` mode. Use `detect_feature_envy_from_callgraph()` for proper detection.
#[deprecated(
    since = "0.2.0",
    note = "Use detect_feature_envy_from_callgraph() with --deep mode instead"
)]
pub fn detect_feature_envy(_source: &str, _language: &str) -> Vec<SmellFinding> {
    // Tier-2 smells require --deep mode with CallGraphIR
    Vec::new()
}

/// Detect Inappropriate Intimacy smell from source string.
///
/// This is a backward-compatible stub. Tier-2 Inappropriate Intimacy detection requires
/// `--deep` mode. Use `detect_inappropriate_intimacy_from_callgraph()` for proper detection.
#[deprecated(
    since = "0.2.0",
    note = "Use detect_inappropriate_intimacy_from_callgraph() with --deep mode instead"
)]
pub fn detect_inappropriate_intimacy(_source: &str, _language: &str) -> Vec<SmellFinding> {
    // Tier-2 smells require --deep mode with CallGraphIR + InheritanceReport
    Vec::new()
}

// --- CallGraph-based detection stubs (proper signatures for Phases 2-5) ---

/// Detect Middle Man smell from call graph data.
///
/// Identifies classes where more than `thresholds.middle_man_delegation_ratio` of
/// non-constructor methods are pure delegators to another object.
///
/// # Arguments
/// * `file_ir` - File IR containing classes, functions, and call sites
/// * `thresholds` - Threshold configuration
/// * `language` - Language name for constructor/self detection
/// * `suggest` - Whether to include fix suggestions
///
/// # Returns
/// A vector of `SmellFinding` for each Middle Man class detected.
pub fn detect_middle_man_from_callgraph(
    file_ir: &FileIR,
    thresholds: &Thresholds,
    language: &str,
    suggest: bool,
) -> Vec<SmellFinding> {
    /// Design-pattern class names to exclude (case-insensitive).
    /// These classes legitimately delegate as part of their pattern.
    const EXCLUDED_PATTERNS: &[&str] = &[
        "facade",
        "adapter",
        "wrapper",
        "proxy",
        "bridge",
        "decorator",
        "gateway",
    ];

    let mut findings = Vec::new();

    for class in &file_ir.classes {
        let methods = get_class_methods_robust(file_ir, &class.name);

        // Filter out constructors
        let non_constructor_methods: Vec<&FuncDef> = methods
            .iter()
            .filter(|m| !is_constructor(&m.name, language))
            .copied()
            .collect();

        let total = non_constructor_methods.len();
        if total < thresholds.middle_man_min_methods {
            continue;
        }

        // Facade/Adapter exclusion heuristic -- check before costly analysis
        let name_lower = class.name.to_lowercase();
        if EXCLUDED_PATTERNS.iter().any(|p| name_lower.contains(p)) {
            continue;
        }

        // Check each method for pure delegation
        let mut delegation_count: usize = 0;
        let mut delegate_targets: HashMap<String, usize> = HashMap::new();

        for method in &non_constructor_methods {
            let qualified = format!("{}.{}", class.name, method.name);
            let calls = file_ir
                .calls
                .get(&qualified)
                .or_else(|| file_ir.calls.get(&method.name));

            if let Some(calls) = calls {
                // Collect method/attr calls (the ones that represent delegation)
                let method_calls: Vec<&CallSite> = calls
                    .iter()
                    .filter(|c| matches!(c.call_type, CallType::Method | CallType::Attr))
                    .collect();

                // Pure delegation: exactly 1 method call to a non-self receiver
                if method_calls.len() == 1 {
                    let call = method_calls[0];
                    let receiver_is_self = call
                        .receiver
                        .as_ref()
                        .map(|r| {
                            is_self_reference(r, language)
                                || r.starts_with("self.")
                                || r.starts_with("this.")
                        })
                        .unwrap_or(false);

                    // Count non-method/attr calls (e.g., Direct, Intra).
                    // A pure delegator should have no additional logic calls.
                    let non_method_calls = calls
                        .iter()
                        .filter(|c| !matches!(c.call_type, CallType::Method | CallType::Attr))
                        .count();

                    if !receiver_is_self && non_method_calls == 0 {
                        delegation_count += 1;
                        if let Some(ref rt) = call.receiver_type {
                            *delegate_targets.entry(rt.clone()).or_insert(0) += 1;
                        }
                    }
                }
                // method_calls.len() != 1 means either 0 method calls (no delegation)
                // or multiple method calls (not a pure delegator)
            }
            // No calls entry means the method has real logic or is empty -- not a delegator
        }

        let ratio = delegation_count as f64 / total as f64;
        if ratio >= thresholds.middle_man_delegation_ratio {
            let primary_delegate = delegate_targets
                .iter()
                .max_by_key(|(_, count)| *count)
                .map(|(name, _)| name.clone())
                .unwrap_or_else(|| "unknown".to_string());

            findings.push(SmellFinding {
                smell_type: SmellType::MiddleMan,
                file: file_ir.path.clone(),
                name: class.name.clone(),
                line: class.line,
                reason: format!(
                    "Class delegates {}/{} methods ({:.0}%) to {}",
                    delegation_count,
                    total,
                    ratio * 100.0,
                    primary_delegate
                ),
                severity: middle_man_severity(ratio, delegation_count),
                suggestion: if suggest {
                    Some(format!(
                        "Consider removing {} and using {} directly",
                        class.name, primary_delegate
                    ))
                } else {
                    None
                },
            });
        }
    }

    findings
}

/// Detect Refused Bequest smell from call graph and inheritance data.
///
/// Identifies subclasses that use fewer than `thresholds.refused_bequest_usage_ratio`
/// of their parent's concrete, non-abstract methods.
///
/// # Arguments
/// * `call_graph` - Full call graph with cross-file data
/// * `inheritance_report` - Inheritance analysis report
/// * `thresholds` - Threshold configuration
/// * `suggest` - Whether to include fix suggestions
///
/// # Returns
/// A vector of `SmellFinding` for each Refused Bequest detected.
pub fn detect_refused_bequest_from_callgraph(
    call_graph: &CallGraphIR,
    inheritance_report: &InheritanceReport,
    thresholds: &Thresholds,
    suggest: bool,
) -> Vec<SmellFinding> {
    use crate::types::inheritance::{BaseResolution, InheritanceKind};

    let mut findings = Vec::new();

    for edge in &inheritance_report.edges {
        // Skip external/stdlib/unresolved parents
        if edge.external || edge.resolution == BaseResolution::Unresolved {
            continue;
        }

        // Skip Go embedding (XL2) -- not classical inheritance
        if edge.kind == InheritanceKind::Embeds {
            continue;
        }

        // Skip interface implementations -- compliance, not bequest
        if edge.kind == InheritanceKind::Implements {
            continue;
        }

        // Check parent node flags (C5): skip abstract, protocol, mixin parents
        let parent_node = inheritance_report
            .nodes
            .iter()
            .find(|n| n.name == edge.parent);
        if let Some(parent) = parent_node {
            if parent.is_abstract == Some(true)
                || parent.protocol == Some(true)
                || parent.mixin == Some(true)
            {
                continue;
            }
        }

        // Get parent class methods from any file in the call graph
        let parent_concrete_methods = get_parent_concrete_methods(call_graph, &edge.parent);

        // Apply minimum inherited methods threshold
        if parent_concrete_methods.len() < thresholds.refused_bequest_min_inherited {
            continue;
        }

        // Get child class methods (compute once, use for both override check and call analysis)
        let child_file_ir = call_graph.files.get(&edge.child_file);
        let child_methods = child_file_ir
            .map(|fir| get_class_methods_robust(fir, &edge.child))
            .unwrap_or_default();

        let child_method_names: HashSet<&str> =
            child_methods.iter().map(|f| f.name.as_str()).collect();

        // Get all call targets made from any child class method
        let child_call_targets: HashSet<String> = if let Some(fir) = child_file_ir {
            child_methods
                .iter()
                .flat_map(|method| {
                    let qualified = format!("{}.{}", edge.child, method.name);
                    fir.calls
                        .get(&qualified)
                        .into_iter()
                        .chain(fir.calls.get(&method.name).into_iter())
                        .flatten()
                        .map(|c| c.target.clone())
                })
                .collect()
        } else {
            HashSet::new()
        };

        // Count usage: override OR call counts as "used" (C2)
        let mut used_count = 0usize;
        let mut unused_methods = Vec::new();

        for inherited_method in &parent_concrete_methods {
            let is_overridden = child_method_names.contains(inherited_method.as_str());
            let is_called = child_call_targets.contains(inherited_method);

            if is_overridden || is_called {
                used_count += 1;
            } else {
                unused_methods.push(inherited_method.clone());
            }
        }

        let total = parent_concrete_methods.len();
        let usage_ratio = used_count as f64 / total as f64;

        if usage_ratio < thresholds.refused_bequest_usage_ratio {
            let child_line = edge.child_line;

            findings.push(SmellFinding {
                smell_type: SmellType::RefusedBequest,
                file: edge.child_file.clone(),
                name: edge.child.clone(),
                line: child_line,
                reason: format!(
                    "Uses {}/{} ({:.0}%) inherited methods from {}. Unused: {}",
                    used_count,
                    total,
                    usage_ratio * 100.0,
                    edge.parent,
                    if unused_methods.len() <= 5 {
                        unused_methods.join(", ")
                    } else {
                        format!(
                            "{}, ... and {} more",
                            unused_methods[..5].join(", "),
                            unused_methods.len() - 5
                        )
                    }
                ),
                severity: refused_bequest_severity(usage_ratio, total),
                suggestion: if suggest {
                    Some(format!(
                        "Consider composition over inheritance, or remove {} as a base of {}",
                        edge.parent, edge.child
                    ))
                } else {
                    None
                },
            });
        }
    }

    findings
}

/// Get concrete (non-constructor) method names from a parent class across all files in call graph.
///
/// Searches all files in the call graph for the parent class and returns method names
/// excluding constructors (detected via language-agnostic heuristic).
fn get_parent_concrete_methods(call_graph: &CallGraphIR, parent_name: &str) -> Vec<String> {
    let language = &call_graph.language;
    for file_ir in call_graph.files.values() {
        let methods = get_class_methods_robust(file_ir, parent_name);
        if !methods.is_empty() {
            return methods
                .iter()
                .filter(|m| !is_constructor(&m.name, language))
                .map(|m| m.name.clone())
                .collect();
        }
    }
    Vec::new()
}

/// Detect Feature Envy smell from call graph data.
///
/// Identifies methods that access more features (method calls, attribute accesses)
/// of another class than their own class. Uses a dual-threshold approach:
/// - `feature_envy_min_foreign`: minimum number of foreign accesses to consider
/// - `feature_envy_ratio`: minimum ratio of foreign-to-own accesses
///
/// # Role-Based Exclusion (C4)
///
/// Classes whose names (case-insensitive) contain any of the following role
/// keywords are excluded: format, formatter, serialize, serializer, deserialize,
/// handler, visitor, render, renderer, builder, validator, converter, mapper,
/// adapter, factory, transformer, presenter.
///
/// # Edge Cases
///
/// - Static methods (`is_method == false`): skipped
/// - Constructors: skipped
/// - Methods with no calls: skipped
/// - Division by zero when `own_count == 0`: uses `own.max(1)` for ratio
///
/// # Arguments
/// * `file_ir` - File IR containing classes, functions, and call sites
/// * `thresholds` - Threshold configuration
/// * `language` - Language name for self-reference detection
/// * `suggest` - Whether to include fix suggestions
///
/// # Returns
/// A vector of `SmellFinding` for each Feature Envy method detected.
pub fn detect_feature_envy_from_callgraph(
    file_ir: &FileIR,
    thresholds: &Thresholds,
    language: &str,
    suggest: bool,
) -> Vec<SmellFinding> {
    /// Role-based class name patterns to exclude (case-insensitive).
    /// These classes legitimately access foreign data as part of their design role.
    const EXCLUDED_ROLES: &[&str] = &[
        "format",
        "formatter",
        "serialize",
        "serializer",
        "deserialize",
        "handler",
        "visitor",
        "render",
        "renderer",
        "builder",
        "validator",
        "converter",
        "mapper",
        "adapter",
        "factory",
        "transformer",
        "presenter",
    ];

    let mut findings = Vec::new();

    for class in &file_ir.classes {
        // Role-based exclusion (C4)
        let name_lower = class.name.to_lowercase();
        if EXCLUDED_ROLES.iter().any(|r| name_lower.contains(r)) {
            continue;
        }

        let methods = get_class_methods_robust(file_ir, &class.name);

        for method in &methods {
            // Skip constructors
            if is_constructor(&method.name, language) {
                continue;
            }

            // Skip static methods (no self/this parameter)
            if !method.is_method {
                continue;
            }

            let qualified = format!("{}.{}", class.name, method.name);
            let calls = file_ir
                .calls
                .get(&qualified)
                .or_else(|| file_ir.calls.get(&method.name));

            let calls = match calls {
                Some(c) if !c.is_empty() => c,
                _ => continue, // No calls = no envy possible
            };

            // Count accesses by target class
            let mut own_count: usize = 0;
            let mut foreign_counts: HashMap<String, usize> = HashMap::new();

            for call in calls {
                if !matches!(call.call_type, CallType::Method | CallType::Attr) {
                    continue;
                }

                // Determine if this is an own-class or foreign-class call
                let is_own = call
                    .receiver
                    .as_ref()
                    .map(|r| {
                        is_self_reference(r, language)
                            || r.starts_with("self.")
                            || r.starts_with("this.")
                    })
                    .unwrap_or(false);

                if is_own {
                    own_count += 1;
                } else if let Some(ref rt) = call.receiver_type {
                    if rt == &class.name {
                        own_count += 1; // Type resolved to own class
                    } else {
                        *foreign_counts.entry(rt.clone()).or_insert(0) += 1;
                    }
                }
                // Calls with no receiver_type and not self/this are unclassified -- skip
            }

            // Check each foreign class against thresholds
            for (foreign_class, foreign_count) in &foreign_counts {
                if *foreign_count < thresholds.feature_envy_min_foreign {
                    continue;
                }

                let ratio = *foreign_count as f64 / own_count.max(1) as f64;
                if ratio < thresholds.feature_envy_ratio {
                    continue;
                }

                findings.push(SmellFinding {
                    smell_type: SmellType::FeatureEnvy,
                    file: file_ir.path.clone(),
                    name: format!("{}::{}", class.name, method.name),
                    line: method.line,
                    reason: format!(
                        "Accesses {} features of {} but only {} of own class {} (ratio {:.1}:1)",
                        foreign_count, foreign_class, own_count, class.name, ratio
                    ),
                    severity: feature_envy_severity(*foreign_count, own_count),
                    suggestion: if suggest {
                        Some(format!(
                            "Consider moving {} to {} or extracting shared logic",
                            method.name, foreign_class
                        ))
                    } else {
                        None
                    },
                });
            }
        }
    }

    findings
}

/// Metrics tracking bidirectional access between a normalized class pair.
///
/// A pair (A, B) is normalized so that A <= B lexicographically.
/// `a_to_b` counts accesses from class A's methods to class B,
/// `b_to_a` counts accesses from class B's methods to class A.
struct IntimacyPairMetrics {
    /// Number of accesses from class A (lexicographically first) to class B.
    a_to_b: usize,
    /// Number of accesses from class B to class A.
    b_to_a: usize,
    /// Number of private (underscore-prefixed) accesses from A to B.
    a_to_b_private: usize,
    /// Number of private (underscore-prefixed) accesses from B to A.
    b_to_a_private: usize,
}

impl IntimacyPairMetrics {
    /// Create a new metrics tracker with all counts at zero.
    fn new() -> Self {
        Self {
            a_to_b: 0,
            b_to_a: 0,
            a_to_b_private: 0,
            b_to_a_private: 0,
        }
    }

    /// Total bidirectional access count.
    fn total(&self) -> usize {
        self.a_to_b + self.b_to_a
    }

    /// Minimum access count across both directions.
    fn min_direction(&self) -> usize {
        self.a_to_b.min(self.b_to_a)
    }

    /// Check if both directions meet the minimum per-direction threshold.
    fn is_bidirectional_enough(&self, min_per_dir: usize) -> bool {
        self.a_to_b >= min_per_dir && self.b_to_a >= min_per_dir
    }
}

/// Normalize a class pair to (min, max) lexicographic order for consistent deduplication.
fn normalize_class_pair(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

/// Detect Inappropriate Intimacy smell from call graph and inheritance data.
///
/// Identifies pairs of classes with bidirectional internal access exceeding thresholds.
/// This detector operates on the full `CallGraphIR`, enabling cross-file analysis.
///
/// Detection logic:
/// 1. Build a cross-class access graph from all files in the call graph
/// 2. For each class pair, count accesses in both directions
/// 3. Apply thresholds: total accesses >= `intimacy_min_total` AND
///    min(a_to_b, b_to_a) >= `intimacy_min_per_direction`
/// 4. Exclude inheritance-related pairs (parent-child access is expected)
///
/// # Arguments
/// * `call_graph` - Full call graph with cross-file data
/// * `inheritance_report` - Inheritance analysis report for exclusion checking
/// * `thresholds` - Threshold configuration
/// * `suggest` - Whether to include fix suggestions
///
/// # Returns
/// A vector of `SmellFinding` for each intimate class pair detected.
pub fn detect_inappropriate_intimacy_from_callgraph(
    call_graph: &CallGraphIR,
    inheritance_report: &InheritanceReport,
    thresholds: &Thresholds,
    suggest: bool,
) -> Vec<SmellFinding> {
    let mut findings = Vec::new();
    let mut pair_metrics: HashMap<(String, String), IntimacyPairMetrics> = HashMap::new();

    // Build a set of inheritance-related pairs to exclude (parent-child access is expected)
    let inheritance_pairs: HashSet<(String, String)> = inheritance_report
        .edges
        .iter()
        .map(|e| normalize_class_pair(&e.child, &e.parent))
        .collect();

    // Walk all files in the call graph to build the cross-class access graph
    for file_ir in call_graph.files.values() {
        for class in &file_ir.classes {
            let methods = get_class_methods_robust(file_ir, &class.name);

            for method in &methods {
                let qualified = format!("{}.{}", class.name, method.name);
                let calls = file_ir
                    .calls
                    .get(&qualified)
                    .or_else(|| file_ir.calls.get(&method.name));

                if let Some(calls) = calls {
                    for call in calls {
                        // Only consider method calls and attribute accesses
                        if !matches!(call.call_type, CallType::Method | CallType::Attr) {
                            continue;
                        }

                        if let Some(ref rt) = call.receiver_type {
                            // Skip self-class accesses
                            if rt == &class.name {
                                continue;
                            }

                            let pair = normalize_class_pair(&class.name, rt);

                            // Skip inheritance-related pairs
                            if inheritance_pairs.contains(&pair) {
                                continue;
                            }

                            let metrics = pair_metrics
                                .entry(pair.clone())
                                .or_insert_with(IntimacyPairMetrics::new);

                            // Determine direction based on normalized pair ordering
                            if class.name <= *rt {
                                // class is "A" in the normalized pair (A, B)
                                metrics.a_to_b += 1;
                                if call.target.starts_with('_') {
                                    metrics.a_to_b_private += 1;
                                }
                            } else {
                                // class is "B" in the normalized pair (A, B)
                                metrics.b_to_a += 1;
                                if call.target.starts_with('_') {
                                    metrics.b_to_a_private += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Evaluate each class pair against thresholds
    for ((class_a, class_b), metrics) in &pair_metrics {
        if metrics.total() < thresholds.intimacy_min_total {
            continue;
        }
        if !metrics.is_bidirectional_enough(thresholds.intimacy_min_per_direction) {
            continue;
        }

        findings.push(SmellFinding {
            smell_type: SmellType::InappropriateIntimacy,
            file: PathBuf::from("(cross-class)"),
            name: format!("{} <-> {}", class_a, class_b),
            line: 0,
            reason: format!(
                "Bidirectional coupling: {} -> {} ({} calls, {} private), {} -> {} ({} calls, {} private)",
                class_a, class_b, metrics.a_to_b, metrics.a_to_b_private,
                class_b, class_a, metrics.b_to_a, metrics.b_to_a_private
            ),
            severity: intimacy_severity(metrics.total(), metrics.min_direction()),
            suggestion: if suggest {
                Some(format!(
                    "Consider merging {} and {} or extracting shared behavior into a third class",
                    class_a, class_b
                ))
            } else {
                None
            },
        });
    }

    findings
}

// =============================================================================
// Aggregated Smell Detection (--deep mode)
// =============================================================================

/// Detect code smells using aggregated analysis from multiple existing analyzers.
///
/// This runs the original 3 detectors (God Class, Long Method, Long Parameter List)
/// plus pulls findings from:
/// - Cohesion analyzer (LCOM4 >= 2 => LowCohesion smell)
/// - Coupling analyzer (score >= 0.6 => TightCoupling smell)
/// - Dead code analyzer (unreachable => DeadCode smell)
/// - Similarity analyzer (score > 0.6 => CodeClone smell)
/// - Complexity analyzer (cognitive >= 15 => HighCognitiveComplexity smell)
///
/// Each sub-analyzer is called independently; if one fails, it is skipped
/// and the rest continue.
///
/// # Arguments
/// * `path` - File or directory to scan
/// * `threshold` - Threshold preset (Strict, Default, Relaxed)
/// * `smell_type` - Optional filter for specific smell type
/// * `suggest` - Whether to include fix suggestions
///
/// # Returns
/// * `Ok(SmellsReport)` - Combined report with findings from all analyzers
pub fn analyze_smells_aggregated(
    path: &Path,
    threshold: ThresholdPreset,
    smell_type: Option<SmellType>,
    suggest: bool,
) -> TldrResult<SmellsReport> {
    analyze_smells_aggregated_with_walker_opts(
        path,
        threshold,
        smell_type,
        suggest,
        SmellsWalkerOpts::default(),
    )
}

/// Same as [`analyze_smells_aggregated`] but accepts walker options.
///
/// Sub-analyzers (cohesion, dead code, similarity, etc.) still use the
/// default walker; only the top-level smell scan honors `walker_opts`.
/// This matches the spec-defined behavior for `--no-default-ignore`:
/// the base detectors walk vendor dirs when requested, but the deep
/// analyzers use their own defaults.
pub fn analyze_smells_aggregated_with_walker_opts(
    path: &Path,
    threshold: ThresholdPreset,
    smell_type: Option<SmellType>,
    suggest: bool,
    walker_opts: SmellsWalkerOpts,
) -> TldrResult<SmellsReport> {
    let mut all_smells: Vec<SmellFinding> = Vec::new();
    let mut files_scanned: usize = 0;
    // v0.2.3 (#1.D): track findings excluded by the test-file filter so the
    // aggregated path mirrors the base path's `excluded_test_smells` counter.
    let mut excluded_test_smells: usize = 0;
    let include_tests = walker_opts.include_tests;

    if should_run_original_detectors(smell_type) {
        if let Ok(base_report) = detect_smells_with_walker_opts(
            path,
            threshold,
            smell_type,
            suggest,
            walker_opts.clone(),
        ) {
            files_scanned = base_report.files_scanned;
            all_smells.extend(base_report.smells);
            excluded_test_smells += base_report.excluded_test_smells;
        }
    }

    if should_analyze_smell(smell_type, SmellType::LowCohesion) {
        collect_low_cohesion_smells(path, suggest, &mut all_smells);
    }

    let needs_coupling = should_analyze_smell(smell_type, SmellType::TightCoupling);
    let needs_tier2 = needs_tier2_analysis(smell_type);
    let needs_call_graph = needs_coupling || needs_tier2;

    let (root_dir, cg_language) = call_graph_context(path, needs_call_graph);
    let shared_call_graph_ir = build_shared_call_graph_ir(root_dir, &cg_language, needs_call_graph);

    let project_call_graph = if needs_coupling {
        shared_call_graph_ir
            .as_ref()
            .map(crate::callgraph::builder::project_graph_from_ir_ref)
    } else {
        None
    };

    if needs_coupling {
        collect_tight_coupling_smells(
            path,
            &cg_language,
            project_call_graph.as_ref(),
            suggest,
            &mut all_smells,
        );
    }

    if should_analyze_smell(smell_type, SmellType::DeadCode) {
        collect_dead_code_smells(path, suggest, &mut all_smells);
    }

    if should_analyze_smell(smell_type, SmellType::CodeClone) {
        collect_code_clone_smells(path, suggest, &mut all_smells);
    }

    if should_analyze_smell(smell_type, SmellType::HighCognitiveComplexity) {
        collect_high_cognitive_smells(path, suggest, &mut all_smells);
    }

    let inheritance_report = build_inheritance_report(path, needs_tier2);

    let thresholds = Thresholds::from_preset(threshold);

    if should_analyze_smell(smell_type, SmellType::MiddleMan) {
        collect_middle_man_smells(
            shared_call_graph_ir.as_ref(),
            &thresholds,
            suggest,
            &mut all_smells,
        );
    }

    if should_analyze_smell(smell_type, SmellType::RefusedBequest) {
        collect_refused_bequest_smells(
            shared_call_graph_ir.as_ref(),
            inheritance_report.as_ref(),
            &thresholds,
            suggest,
            &mut all_smells,
        );
    }

    if should_analyze_smell(smell_type, SmellType::FeatureEnvy) {
        collect_feature_envy_smells(
            shared_call_graph_ir.as_ref(),
            &thresholds,
            suggest,
            &mut all_smells,
        );
    }

    if should_analyze_smell(smell_type, SmellType::InappropriateIntimacy) {
        collect_inappropriate_intimacy_smells(
            shared_call_graph_ir.as_ref(),
            inheritance_report.as_ref(),
            &thresholds,
            suggest,
            &mut all_smells,
        );
    }

    // v0.2.3 (#1.D): apply the test-file filter to the smells contributed by
    // the deep sub-analyzers (cohesion, coupling, dead, clone, etc.). The base
    // detector path was already filtered inside `detect_smells_with_walker_opts`
    // and its excluded count is already accumulated above.
    if !include_tests {
        let pre = all_smells.len();
        all_smells.retain(|s| !crate::analysis::clones::is_test_file(&s.file));
        excluded_test_smells += pre - all_smells.len();
    }

    sort_smells(&mut all_smells);
    let by_file = build_smells_by_file(&all_smells);

    if files_scanned == 0 && !by_file.is_empty() {
        files_scanned = by_file.len();
    }

    let summary = build_smells_summary(&all_smells, files_scanned);

    Ok(SmellsReport {
        smells: all_smells,
        files_scanned,
        by_file,
        summary,
        excluded_test_smells,
        warnings: Vec::new(),
    })
}

fn should_run_original_detectors(smell_type: Option<SmellType>) -> bool {
    matches!(
        smell_type,
        None | Some(SmellType::GodClass)
            | Some(SmellType::LongMethod)
            | Some(SmellType::LongParameterList)
            | Some(SmellType::FeatureEnvy)
            | Some(SmellType::DataClumps)
            | Some(SmellType::DeepNesting)
            | Some(SmellType::DataClass)
            | Some(SmellType::LazyElement)
            | Some(SmellType::MessageChain)
            | Some(SmellType::PrimitiveObsession)
    )
}

fn should_analyze_smell(smell_type: Option<SmellType>, target: SmellType) -> bool {
    smell_type.is_none() || smell_type == Some(target)
}

fn needs_tier2_analysis(smell_type: Option<SmellType>) -> bool {
    smell_type.is_none()
        || matches!(
            smell_type,
            Some(SmellType::MiddleMan)
                | Some(SmellType::RefusedBequest)
                | Some(SmellType::FeatureEnvy)
                | Some(SmellType::InappropriateIntimacy)
        )
}

fn call_graph_context(path: &Path, needs_call_graph: bool) -> (&Path, String) {
    if !needs_call_graph {
        return (path, String::new());
    }
    if path.is_file() {
        let lang = Language::from_path(path)
            .map(|l| l.to_string().to_lowercase())
            .unwrap_or_else(|| "python".to_string());
        return (path.parent().unwrap_or(path), lang);
    }
    let lang = crate::walker::walk_project(path)
        .find_map(|e| Language::from_path(e.path()))
        .map(|l| l.to_string().to_lowercase())
        .unwrap_or_else(|| "python".to_string());
    (path, lang)
}

fn build_shared_call_graph_ir(
    root_dir: &Path,
    cg_language: &str,
    needs_call_graph: bool,
) -> Option<CallGraphIR> {
    if !needs_call_graph {
        return None;
    }
    use crate::callgraph::builder_v2::{build_project_call_graph_v2, BuildConfig};
    let config = BuildConfig {
        language: cg_language.to_string(),
        ..Default::default()
    };
    build_project_call_graph_v2(root_dir, config).ok()
}

fn collect_low_cohesion_smells(path: &Path, suggest: bool, all_smells: &mut Vec<SmellFinding>) {
    if let Ok(cohesion_report) = crate::quality::cohesion::analyze_cohesion(path, None, 2) {
        for class in &cohesion_report.classes {
            if class.lcom4 < 2 {
                continue;
            }
            all_smells.push(SmellFinding {
                smell_type: SmellType::LowCohesion,
                file: class.file.clone(),
                name: class.name.clone(),
                line: class.line as u32,
                reason: format!(
                    "Class has LCOM4={} (>1 indicates multiple responsibilities)",
                    class.lcom4
                ),
                severity: cohesion_severity(class.lcom4),
                suggestion: if suggest {
                    class.split_suggestion.clone().or_else(|| {
                        Some("Consider splitting this class by responsibility".to_string())
                    })
                } else {
                    None
                },
            });
        }
    }
}

fn collect_tight_coupling_smells(
    path: &Path,
    cg_language: &str,
    project_call_graph: Option<&crate::types::ProjectCallGraph>,
    suggest: bool,
    all_smells: &mut Vec<SmellFinding>,
) {
    let Some(project_call_graph) = project_call_graph else {
        return;
    };
    let lang = cg_language.parse::<Language>().unwrap_or(Language::Python);
    let options = crate::quality::coupling::CouplingOptions {
        max_pairs: 50,
        ..Default::default()
    };
    if let Ok(coupling_report) = crate::quality::coupling::analyze_coupling_with_graph(
        path,
        lang,
        project_call_graph,
        &options,
    ) {
        for pair in &coupling_report.top_pairs {
            if pair.score < 0.6 {
                continue;
            }
            let source_name = pair
                .source
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| pair.source.display().to_string());
            let target_name = pair
                .target
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| pair.target.display().to_string());
            all_smells.push(SmellFinding {
                smell_type: SmellType::TightCoupling,
                file: pair.source.clone(),
                name: format!("{} <-> {}", source_name, target_name),
                line: 0,
                reason: format!(
                    "Coupling score {:.2} ({} calls, {} shared imports)",
                    pair.score,
                    pair.call_count,
                    pair.shared_imports.len()
                ),
                severity: coupling_severity(pair.score),
                suggestion: if suggest {
                    Some(
                        "Consider introducing an interface or mediator to reduce direct coupling"
                            .to_string(),
                    )
                } else {
                    None
                },
            });
        }
    }
}

fn collect_dead_code_smells(path: &Path, suggest: bool, all_smells: &mut Vec<SmellFinding>) {
    if let Ok(dead_report) = crate::quality::dead_code::analyze_dead_code(path, None, &[]) {
        for func in &dead_report.dead_functions {
            all_smells.push(SmellFinding {
                smell_type: SmellType::DeadCode,
                file: func.file.clone(),
                name: func.name.clone(),
                line: func.line as u32,
                reason: format!("Unreachable function ({:?})", func.reason),
                severity: 1,
                suggestion: if suggest {
                    Some("Remove this function or add a call path to it".to_string())
                } else {
                    None
                },
            });
        }
    }
}

fn collect_code_clone_smells(path: &Path, suggest: bool, all_smells: &mut Vec<SmellFinding>) {
    let sim_options = crate::quality::similarity::SimilarityOptions {
        threshold: 0.6,
        max_functions: 500,
        max_pairs: 50,
    };
    if let Ok(sim_report) =
        crate::quality::similarity::find_similar_with_options(path, None, &sim_options)
    {
        for pair in &sim_report.similar_pairs {
            all_smells.push(SmellFinding {
                smell_type: SmellType::CodeClone,
                file: pair.func_a.file.clone(),
                name: format!("{} ~ {}", pair.func_a.name, pair.func_b.name),
                line: pair.func_a.line as u32,
                reason: format!(
                    "Similarity score {:.0}% with {}:{}",
                    pair.score * 100.0,
                    pair.func_b.file.display(),
                    pair.func_b.line
                ),
                severity: clone_severity(pair.score),
                suggestion: if suggest {
                    Some("Consider extracting shared logic into a common function".to_string())
                } else {
                    None
                },
            });
        }
    }
}

fn collect_high_cognitive_smells(path: &Path, suggest: bool, all_smells: &mut Vec<SmellFinding>) {
    let complexity_options = crate::quality::complexity::ComplexityOptions {
        hotspot_threshold: 10,
        max_hotspots: 100,
        include_cognitive: true,
    };
    if let Ok(complexity_report) =
        crate::quality::complexity::analyze_complexity(path, None, Some(complexity_options))
    {
        for func in &complexity_report.functions {
            if func.cognitive < 15 {
                continue;
            }
            all_smells.push(SmellFinding {
                smell_type: SmellType::HighCognitiveComplexity,
                file: func.file.clone(),
                name: func.name.clone(),
                line: func.line as u32,
                reason: format!("Cognitive complexity {} (threshold: 15)", func.cognitive),
                severity: cognitive_severity(func.cognitive),
                suggestion: if suggest {
                    Some(
                        "Simplify control flow, reduce nesting, or extract helper functions"
                            .to_string(),
                    )
                } else {
                    None
                },
            });
        }
    }
}

fn build_inheritance_report(path: &Path, needs_tier2: bool) -> Option<InheritanceReport> {
    if !needs_tier2 {
        return None;
    }
    use crate::inheritance::{extract_inheritance, InheritanceOptions};
    let options = InheritanceOptions::default();
    extract_inheritance(path, None, &options).ok()
}

fn collect_middle_man_smells(
    shared_call_graph_ir: Option<&CallGraphIR>,
    thresholds: &Thresholds,
    suggest: bool,
    all_smells: &mut Vec<SmellFinding>,
) {
    let Some(shared_call_graph_ir) = shared_call_graph_ir else {
        return;
    };
    for file_ir in shared_call_graph_ir.files.values() {
        let lang = inferred_language_name(&file_ir.path);
        let findings = detect_middle_man_from_callgraph(file_ir, thresholds, &lang, suggest);
        all_smells.extend(findings);
    }
}

fn collect_refused_bequest_smells(
    shared_call_graph_ir: Option<&CallGraphIR>,
    inheritance_report: Option<&InheritanceReport>,
    thresholds: &Thresholds,
    suggest: bool,
    all_smells: &mut Vec<SmellFinding>,
) {
    let (Some(shared_call_graph_ir), Some(inheritance_report)) =
        (shared_call_graph_ir, inheritance_report)
    else {
        return;
    };
    let findings = detect_refused_bequest_from_callgraph(
        shared_call_graph_ir,
        inheritance_report,
        thresholds,
        suggest,
    );
    all_smells.extend(findings);
}

fn collect_feature_envy_smells(
    shared_call_graph_ir: Option<&CallGraphIR>,
    thresholds: &Thresholds,
    suggest: bool,
    all_smells: &mut Vec<SmellFinding>,
) {
    let Some(shared_call_graph_ir) = shared_call_graph_ir else {
        return;
    };
    for file_ir in shared_call_graph_ir.files.values() {
        let lang = inferred_language_name(&file_ir.path);
        let findings = detect_feature_envy_from_callgraph(file_ir, thresholds, &lang, suggest);
        all_smells.extend(findings);
    }
}

fn collect_inappropriate_intimacy_smells(
    shared_call_graph_ir: Option<&CallGraphIR>,
    inheritance_report: Option<&InheritanceReport>,
    thresholds: &Thresholds,
    suggest: bool,
    all_smells: &mut Vec<SmellFinding>,
) {
    let (Some(shared_call_graph_ir), Some(inheritance_report)) =
        (shared_call_graph_ir, inheritance_report)
    else {
        return;
    };
    let findings = detect_inappropriate_intimacy_from_callgraph(
        shared_call_graph_ir,
        inheritance_report,
        thresholds,
        suggest,
    );
    all_smells.extend(findings);
}

fn inferred_language_name(path: &Path) -> String {
    Language::from_path(path)
        .map(|l| l.to_string().to_lowercase())
        .unwrap_or_else(|| "python".to_string())
}

fn sort_smells(all_smells: &mut [SmellFinding]) {
    all_smells.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
    });
}

fn build_smells_by_file(all_smells: &[SmellFinding]) -> HashMap<PathBuf, Vec<SmellFinding>> {
    let mut by_file: HashMap<PathBuf, Vec<SmellFinding>> = HashMap::new();
    for smell in all_smells {
        by_file
            .entry(smell.file.clone())
            .or_default()
            .push(smell.clone());
    }
    by_file
}

fn build_smells_summary(all_smells: &[SmellFinding], files_scanned: usize) -> SmellsSummary {
    let mut by_type: HashMap<String, usize> = HashMap::new();
    for smell in all_smells {
        *by_type.entry(smell.smell_type.to_string()).or_insert(0) += 1;
    }
    SmellsSummary {
        total_smells: all_smells.len(),
        by_type,
        avg_smells_per_file: if files_scanned > 0 {
            all_smells.len() as f64 / files_scanned as f64
        } else {
            0.0
        },
    }
}

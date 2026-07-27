//! Shared types for Pattern Analysis commands
//!
//! This module defines all data types used across the patterns analysis
//! commands. Types are designed for JSON serialization with serde.
//!
//! # Commands Using These Types
//!
//! - `cohesion`: LCOM4 class cohesion analysis
//! - `coupling`: Cross-module coupling analysis
//! - `interface`: Public API extraction
//! - `purity`: Function purity/effect analysis
//! - `temporal`: Temporal constraint mining
//! - `behavioral`: Pre/postcondition extraction
//! - `mutability`: Variable/parameter mutation tracking
//! - `resources`: Resource lifecycle analysis

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

// =============================================================================
// Confidence Level
// =============================================================================

/// Confidence level for inferred patterns and analysis results.
///
/// # Serialization
///
/// Serializes to snake_case: `"high"`, `"medium"`, `"low"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// High confidence - direct code evidence
    High,
    /// Medium confidence - inferred from patterns
    #[default]
    Medium,
    /// Low confidence - heuristic or partial evidence
    Low,
}

impl std::fmt::Display for Confidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::High => write!(f, "high"),
            Self::Medium => write!(f, "medium"),
            Self::Low => write!(f, "low"),
        }
    }
}

// =============================================================================
// Docstring Style
// =============================================================================

/// Documentation style detected in source code.
///
/// Used by behavioral analysis to parse docstrings correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DocstringStyle {
    /// Google-style docstrings (Args:, Returns:, Raises:)
    Google,
    /// NumPy-style docstrings (Parameters, Returns sections)
    Numpy,
    /// Sphinx/reST style docstrings (:param:, :returns:, :raises:)
    Sphinx,
    /// Plain docstrings without structured sections
    #[default]
    Plain,
}

impl std::fmt::Display for DocstringStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Google => write!(f, "google"),
            Self::Numpy => write!(f, "numpy"),
            Self::Sphinx => write!(f, "sphinx"),
            Self::Plain => write!(f, "plain"),
        }
    }
}

// =============================================================================
// Effect Type
// =============================================================================

/// Type of side effect detected in code.
///
/// Used by purity and behavioral analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectType {
    /// I/O operations (file, network, console)
    Io,
    /// Writing to global variables
    GlobalWrite,
    /// Writing to object attributes (self.x = ...)
    AttributeWrite,
    /// Modifying collections in place (list.append, dict.update)
    CollectionModify,
    /// Calling functions with potential side effects
    Call,
    /// Calling unknown/unresolved functions (purity cannot be determined)
    UnknownCall,
}

impl std::fmt::Display for EffectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io => write!(f, "io"),
            Self::GlobalWrite => write!(f, "global_write"),
            Self::AttributeWrite => write!(f, "attribute_write"),
            Self::CollectionModify => write!(f, "collection_modify"),
            Self::Call => write!(f, "call"),
            Self::UnknownCall => write!(f, "unknown_call"),
        }
    }
}

// =============================================================================
// Condition Source
// =============================================================================

/// Source of a pre/postcondition constraint.
///
/// Tracks where a constraint was extracted from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionSource {
    /// Guard clause (if x < 0: raise ValueError)
    Guard,
    /// Docstring description
    Docstring,
    /// Type hint annotation
    TypeHint,
    /// Assert statement
    Assertion,
    /// icontract decorator (@require, @ensure)
    Icontract,
    /// deal library decorator
    Deal,
}

impl std::fmt::Display for ConditionSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Guard => write!(f, "guard"),
            Self::Docstring => write!(f, "docstring"),
            Self::TypeHint => write!(f, "type_hint"),
            Self::Assertion => write!(f, "assertion"),
            Self::Icontract => write!(f, "icontract"),
            Self::Deal => write!(f, "deal"),
        }
    }
}

// =============================================================================
// Cohesion Types
// =============================================================================

/// Verdict for cohesion analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CohesionVerdict {
    /// Class is cohesive (LCOM4 = 1)
    Cohesive,
    /// Class could be split (LCOM4 > 1)
    SplitCandidate,
}

impl CohesionVerdict {
    /// Determine verdict from LCOM4 value.
    pub fn from_lcom4(lcom4: u32) -> Self {
        if lcom4 <= 1 {
            Self::Cohesive
        } else {
            Self::SplitCandidate
        }
    }
}

impl std::fmt::Display for CohesionVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cohesive => write!(f, "cohesive"),
            Self::SplitCandidate => write!(f, "split_candidate"),
        }
    }
}

/// Information about a connected component in LCOM4 analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentInfo {
    /// Methods in this component
    pub methods: Vec<String>,
    /// Fields accessed by this component
    pub fields: Vec<String>,
}

/// Cohesion analysis result for a single class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassCohesion {
    /// Class name
    pub class_name: String,
    /// File path where class is defined
    pub file_path: String,
    /// Line number of class definition
    pub line: u32,
    /// LCOM4 value (1 = cohesive, >1 = split candidate)
    pub lcom4: u32,
    /// Number of methods analyzed
    pub method_count: u32,
    /// Number of fields detected
    pub field_count: u32,
    /// Verdict based on LCOM4 value
    pub verdict: CohesionVerdict,
    /// Suggestion for splitting if LCOM4 > 1
    pub split_suggestion: Option<String>,
    /// Connected components (if LCOM4 > 1)
    pub components: Vec<ComponentInfo>,
}

/// Summary of cohesion analysis across multiple classes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CohesionSummary {
    /// Total classes analyzed
    pub total_classes: u32,
    /// Number of cohesive classes
    pub cohesive: u32,
    /// Number of split candidates
    pub split_candidates: u32,
    /// Average LCOM4 value
    pub avg_lcom4: f64,
}

impl Default for CohesionSummary {
    fn default() -> Self {
        Self {
            total_classes: 0,
            cohesive: 0,
            split_candidates: 0,
            avg_lcom4: 0.0,
        }
    }
}

/// Full report from cohesion analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CohesionReport {
    /// Cohesion results per class
    pub classes: Vec<ClassCohesion>,
    /// Summary statistics
    pub summary: CohesionSummary,
}

// =============================================================================
// Coupling Types
// =============================================================================

/// Coupling verdict based on score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CouplingVerdict {
    /// Low coupling (0.0-0.2)
    Low,
    /// Moderate coupling (0.2-0.4)
    Moderate,
    /// High coupling (0.4-0.6)
    High,
    /// Very high coupling (0.6-1.0)
    VeryHigh,
}

impl CouplingVerdict {
    /// Determine verdict from coupling score.
    pub fn from_score(score: f64) -> Self {
        if score < 0.2 {
            Self::Low
        } else if score < 0.4 {
            Self::Moderate
        } else if score < 0.6 {
            Self::High
        } else {
            Self::VeryHigh
        }
    }
}

impl std::fmt::Display for CouplingVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Moderate => write!(f, "moderate"),
            Self::High => write!(f, "high"),
            Self::VeryHigh => write!(f, "very_high"),
        }
    }
}

/// A single cross-module function call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossCall {
    /// Function making the call
    pub caller: String,
    /// Function being called
    pub callee: String,
    /// Line number of the call
    pub line: u32,
}

/// Calls from one module to another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CrossCalls {
    /// Individual call sites
    pub calls: Vec<CrossCall>,
    /// Total count of calls
    pub count: u32,
}

/// Coupling analysis between two modules.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CouplingReport {
    /// Path to first module
    pub path_a: String,
    /// Path to second module
    pub path_b: String,
    /// Calls from A to B
    pub a_to_b: CrossCalls,
    /// Calls from B to A
    pub b_to_a: CrossCalls,
    /// Total cross-module calls
    pub total_calls: u32,
    /// Coupling score (0.0-1.0)
    pub coupling_score: f64,
    /// Verdict based on score
    pub verdict: CouplingVerdict,
}

// =============================================================================
// Purity Types
// =============================================================================

/// Purity analysis result for a single function.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionPurity {
    /// Function name
    pub name: String,
    /// Purity classification: "pure", "impure", or "unknown"
    pub classification: String,
    /// List of detected effects (empty if pure)
    pub effects: Vec<String>,
    /// Confidence level of the analysis
    pub confidence: Confidence,
}

/// Purity report for a single file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilePurityReport {
    /// Source file path
    pub source_file: String,
    /// Purity results per function
    pub functions: Vec<FunctionPurity>,
    /// Count of pure functions
    pub pure_count: u32,
}

/// Full purity report (may include multiple files for directory analysis).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PurityReport {
    /// Per-file reports
    pub files: Vec<FilePurityReport>,
    /// Total functions analyzed
    pub total_functions: u32,
    /// Total pure functions
    pub total_pure: u32,
}

// =============================================================================
// Temporal Types
// =============================================================================

/// Example location for a temporal constraint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemporalExample {
    /// File where the pattern was observed
    pub file: String,
    /// Line number
    pub line: u32,
}

/// A temporal constraint (before -> after sequence).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemporalConstraint {
    /// Method that must come first
    pub before: String,
    /// Method that must come after
    pub after: String,
    /// Number of times this pattern was observed
    pub support: u32,
    /// Confidence (support / total sequences containing 'before')
    pub confidence: f64,
    /// Example locations where this pattern appears
    pub examples: Vec<TemporalExample>,
}

/// A trigram (3-method sequence).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trigram {
    /// The 3-method sequence
    pub sequence: [String; 3],
    /// Number of observations
    pub support: u32,
    /// Confidence score
    pub confidence: f64,
}

/// Metadata about temporal mining.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemporalMetadata {
    /// Number of files analyzed
    pub files_analyzed: u32,
    /// Total sequences extracted
    pub sequences_extracted: u32,
    /// Minimum support threshold used
    pub min_support: u32,
    /// Minimum confidence threshold used
    pub min_confidence: f64,
}

impl Default for TemporalMetadata {
    fn default() -> Self {
        Self {
            files_analyzed: 0,
            sequences_extracted: 0,
            min_support: 2,
            min_confidence: 0.5,
        }
    }
}

/// Full temporal constraint report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemporalReport {
    /// Discovered temporal constraints
    pub constraints: Vec<TemporalConstraint>,
    /// Discovered trigrams (if requested)
    pub trigrams: Vec<Trigram>,
    /// Analysis metadata
    pub metadata: TemporalMetadata,
}

// =============================================================================
// Interface Types
// =============================================================================

/// Information about a public function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionInfo {
    /// Function name
    pub name: String,
    /// Full signature (e.g., "def foo(x: int, y: str) -> bool")
    pub signature: String,
    /// Docstring if present
    pub docstring: Option<String>,
    /// Line number of definition
    pub lineno: u32,
    /// Whether the function is async
    pub is_async: bool,
}

/// Information about a public method within a class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MethodInfo {
    /// Method name
    pub name: String,
    /// Full signature
    pub signature: String,
    /// Whether the method is async
    pub is_async: bool,
}

/// Information about a public class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassInfo {
    /// Class name
    pub name: String,
    /// Line number of definition
    pub lineno: u32,
    /// Base classes
    pub bases: Vec<String>,
    /// Public methods
    pub methods: Vec<MethodInfo>,
    /// Count of private methods (for completeness)
    pub private_method_count: u32,
}

/// Interface (public API) for a single file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterfaceInfo {
    /// File path
    pub file: String,
    /// Public exports of the module.
    ///
    /// schema-cleanup-v1 BUG-22: previously `Option<Vec<String>>` and
    /// emitted as `null` for any file without an explicit Python
    /// `__all__` declaration. Now always a populated array — when
    /// `__all__` is defined, its contents are used verbatim;
    /// otherwise this falls back to the union of public function and
    /// class names, mirroring what would be exported under "import *"
    /// semantics. Empty modules return `[]` (empty array), never
    /// `null`.
    #[serde(default)]
    pub all_exports: Vec<String>,
    /// Public functions
    pub functions: Vec<FunctionInfo>,
    /// Public classes
    pub classes: Vec<ClassInfo>,
}

// =============================================================================
// Resource Types
// =============================================================================

/// Information about a detected resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceInfo {
    /// Variable name holding the resource
    pub name: String,
    /// Type of resource (e.g., "file", "socket", "connection")
    pub resource_type: String,
    /// Line where resource was created/opened
    pub line: u32,
    /// Whether the resource is properly closed
    pub closed: bool,
}

/// Information about a potential resource leak.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeakInfo {
    /// Resource that may be leaked
    pub resource: String,
    /// Line where resource was created
    pub line: u32,
    /// Paths to the leak (if --show-paths enabled)
    pub paths: Option<Vec<String>>,
}

/// Information about a double-close issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoubleCloseInfo {
    /// Resource being closed twice
    pub resource: String,
    /// Line of first close
    pub first_close: u32,
    /// Line of second close
    pub second_close: u32,
}

/// Information about use-after-close issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UseAfterCloseInfo {
    /// Resource being used after close
    pub resource: String,
    /// Line where resource was closed
    pub close_line: u32,
    /// Line where resource is used after close
    pub use_line: u32,
}

/// Suggestion for using context manager.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSuggestion {
    /// Resource that should use context manager
    pub resource: String,
    /// Suggested code pattern
    pub suggestion: String,
}

/// LLM-ready constraint from resource analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceConstraint {
    /// The constraint rule
    pub rule: String,
    /// Context where it applies
    pub context: String,
    /// Confidence level
    pub confidence: f64,
}

/// Summary of resource analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ResourceSummary {
    /// Total resources detected
    pub resources_detected: u32,
    /// Number of leaks found
    pub leaks_found: u32,
    /// Number of double-close issues
    pub double_closes_found: u32,
    /// Number of use-after-close issues
    pub use_after_closes_found: u32,
}

/// Full resource analysis report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceReport {
    /// File analyzed
    pub file: String,
    /// Language
    pub language: String,
    /// Function analyzed (if specific function requested)
    pub function: Option<String>,
    /// Detected resources
    pub resources: Vec<ResourceInfo>,
    /// Potential leaks
    pub leaks: Vec<LeakInfo>,
    /// Double-close issues
    pub double_closes: Vec<DoubleCloseInfo>,
    /// Use-after-close issues
    pub use_after_closes: Vec<UseAfterCloseInfo>,
    /// Context manager suggestions
    pub suggestions: Vec<ContextSuggestion>,
    /// LLM constraints (if --constraints enabled)
    pub constraints: Vec<ResourceConstraint>,
    /// Summary statistics
    pub summary: ResourceSummary,
    /// Analysis time in milliseconds
    pub analysis_time_ms: u64,
}

// =============================================================================
// Behavioral Types
// =============================================================================

/// A precondition on a function parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Precondition {
    /// Parameter name
    pub param: String,
    /// Constraint expression (e.g., "x > 0")
    pub expression: Option<String>,
    /// Human-readable description from docstring
    pub description: Option<String>,
    /// Type hint if present
    pub type_hint: Option<String>,
    /// Source of this condition
    pub source: ConditionSource,
}

/// A postcondition on function return.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Postcondition {
    /// Constraint expression
    pub expression: Option<String>,
    /// Human-readable description
    pub description: Option<String>,
    /// Return type hint
    pub type_hint: Option<String>,
}

/// Information about an exception the function may raise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExceptionInfo {
    /// Exception type (e.g., "ValueError")
    pub exception_type: String,
    /// Description of when it's raised
    pub description: Option<String>,
}

/// Information about yield values (for generators).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct YieldInfo {
    /// Type hint for yielded values
    pub type_hint: Option<String>,
    /// Description of yielded values
    pub description: Option<String>,
}

/// Side effect detected in function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideEffect {
    /// Type of effect
    pub effect_type: EffectType,
    /// Target of the effect (e.g., "self.count", "global_var")
    pub target: Option<String>,
}

/// Behavioral analysis for a single function.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionBehavior {
    /// Function name
    pub function_name: String,
    /// File path
    pub file_path: String,
    /// Line number of function definition
    pub line: u32,
    /// Purity classification: "pure", "impure", or "unknown"
    pub purity_classification: String,
    /// Whether it's a generator
    pub is_generator: bool,
    /// Whether it's an async function
    pub is_async: bool,
    /// Preconditions on parameters
    pub preconditions: Vec<Precondition>,
    /// Postconditions on return
    pub postconditions: Vec<Postcondition>,
    /// Exceptions that may be raised
    pub exceptions: Vec<ExceptionInfo>,
    /// Yield information (if generator)
    pub yields: Vec<YieldInfo>,
    /// Detected side effects
    pub side_effects: Vec<SideEffect>,
}

/// Class invariant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassInvariant {
    /// Invariant expression
    pub expression: String,
}

/// Behavioral analysis for a class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassBehavior {
    /// Class name
    pub class_name: String,
    /// Class invariants
    pub invariants: Vec<ClassInvariant>,
    /// Method behaviors
    pub methods: Vec<FunctionBehavior>,
}

/// Full behavioral analysis report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehavioralReport {
    /// File analyzed
    pub file_path: String,
    /// Detected docstring style
    pub docstring_style: DocstringStyle,
    /// Whether icontract library is used
    pub has_icontract: bool,
    /// Whether deal library is used
    pub has_deal: bool,
    /// Function behaviors
    pub functions: Vec<FunctionBehavior>,
    /// Class behaviors
    pub classes: Vec<ClassBehavior>,
}

// =============================================================================
// Mutability Types
// =============================================================================

/// Mutability information for a variable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VariableMutability {
    /// Variable name
    pub name: String,
    /// Whether the variable is ever reassigned
    pub mutable: bool,
    /// Number of reassignments
    pub reassignments: u32,
    /// Number of in-place mutations
    pub mutations: u32,
}

/// Mutability information for a function parameter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParameterMutability {
    /// Parameter name
    pub name: String,
    /// Whether the parameter is mutated
    pub mutated: bool,
    /// Lines where mutation occurs
    pub mutation_sites: Vec<u32>,
}

/// Collection mutation detected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionMutation {
    /// Variable being mutated
    pub variable: String,
    /// Operation (e.g., "append", "update", "pop")
    pub operation: String,
    /// Line number
    pub line: u32,
}

/// Mutability analysis for a function.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionMutability {
    /// Function name
    pub name: String,
    /// Variable mutability info
    pub variables: Vec<VariableMutability>,
    /// Parameter mutability info
    pub parameters: Vec<ParameterMutability>,
    /// Collection mutations
    pub collection_mutations: Vec<CollectionMutation>,
}

/// Field mutability for a class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldMutability {
    /// Field name
    pub name: String,
    /// Whether the field is mutable after __init__
    pub mutable: bool,
    /// Whether the field is only set in __init__
    pub init_only: bool,
}

/// Mutability analysis for a class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassMutability {
    /// Class name
    pub name: String,
    /// Field mutability info
    pub fields: Vec<FieldMutability>,
}

/// Summary of mutability analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MutabilitySummary {
    /// Functions analyzed
    pub functions_analyzed: u32,
    /// Classes analyzed
    pub classes_analyzed: u32,
    /// Total variables
    pub total_variables: u32,
    /// Mutable variables
    pub mutable_variables: u32,
    /// Immutable variables
    pub immutable_variables: u32,
    /// Percentage of immutable variables
    pub immutable_pct: f64,
    /// Parameters analyzed
    pub parameters_analyzed: u32,
    /// Mutated parameters
    pub mutated_parameters: u32,
    /// Percentage of unmutated parameters
    pub unmutated_pct: f64,
    /// Fields analyzed
    pub fields_analyzed: u32,
    /// Mutable fields
    pub mutable_fields: u32,
    /// Constraints generated (if --constraints)
    pub constraints_generated: u32,
}

impl Default for MutabilitySummary {
    fn default() -> Self {
        Self {
            functions_analyzed: 0,
            classes_analyzed: 0,
            total_variables: 0,
            mutable_variables: 0,
            immutable_variables: 0,
            immutable_pct: 0.0,
            parameters_analyzed: 0,
            mutated_parameters: 0,
            unmutated_pct: 0.0,
            fields_analyzed: 0,
            mutable_fields: 0,
            constraints_generated: 0,
        }
    }
}

/// Full mutability report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MutabilityReport {
    /// File analyzed
    pub file: String,
    /// Language
    pub language: String,
    /// Function mutability results
    pub functions: Vec<FunctionMutability>,
    /// Class mutability results
    pub classes: Vec<ClassMutability>,
    /// Summary statistics
    pub summary: MutabilitySummary,
    /// Analysis time in milliseconds
    pub analysis_time_ms: u64,
}

// =============================================================================
// Output Format
// =============================================================================

/// Output format for command results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum OutputFormat {
    /// JSON output (default)
    #[default]
    Json,

    /// Human-readable text output
    Text,
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json => write!(f, "json"),
            Self::Text => write!(f, "text"),
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

//! Core types for API surface extraction (structural contracts).
//!
//! These types represent the machine-readable API surface of a library or package.
//! They are distinct from the behavioral contracts in the CLI `contracts` command
//! (pre/postconditions) -- these are *structural* contracts: function signatures,
//! parameter types, return types, usage examples, and trigger keywords.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Complete API surface for a library or package.
///
/// Contains all public API entries extracted from the package source or type stubs,
/// along with metadata about the package and language.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApiSurface {
    /// Package name (e.g., "flask", "json", "serde")
    pub package: String,
    /// Language of the package
    pub language: String,
    /// Total number of API entries
    pub total: usize,
    /// Individual API entries
    pub apis: Vec<ApiEntry>,
    /// Number of files skipped during extraction (e.g. invalid UTF-8 fixture
    /// files in parser test corpora). The skipped files are NOT counted in
    /// any per-language file totals.
    #[serde(default)]
    pub files_skipped: usize,
    /// Human-readable warnings collected during extraction. Each entry names
    /// a file that was skipped and why (e.g. "Skipped <path>: invalid UTF-8
    /// at byte 1234"). Empty for clean scans.
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// A single public API entry (function, method, class, constant, type alias).
///
/// Each entry represents one callable or referenceable symbol in the package's
/// public API surface, enriched with usage examples and trigger keywords for
/// intent-based retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiEntry {
    /// Qualified name (e.g., "json.loads", "flask.Flask.route")
    pub qualified_name: String,
    /// Kind of API
    pub kind: ApiKind,
    /// Module path (e.g., "json", "flask.app")
    pub module: String,
    /// Function/method signature (None for constants, type aliases)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<Signature>,
    /// Docstring (first paragraph only, truncated to ~200 chars)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    /// Example usage string (e.g., "result = json.loads(s)")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<String>,
    /// Trigger keywords that map intent to this API
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<String>,
    /// Whether this is a property (vs. method/function)
    #[serde(default)]
    pub is_property: bool,
    /// Return type (if resolvable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_type: Option<String>,
    /// Source file and line number
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
}

/// Kind of API symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApiKind {
    /// Top-level function
    Function,
    /// Instance method
    Method,
    /// Class method (Python `@classmethod`)
    ClassMethod,
    /// Static method (Python `@staticmethod`)
    StaticMethod,
    /// Property accessor (Python `@property`)
    Property,
    /// Class definition
    Class,
    /// Struct definition (Rust, Go)
    Struct,
    /// Trait definition (Rust)
    Trait,
    /// Interface definition (TypeScript, Go)
    Interface,
    /// Enum definition
    Enum,
    /// Module-level constant
    Constant,
    /// Type alias
    TypeAlias,
}

impl std::fmt::Display for ApiKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiKind::Function => write!(f, "function"),
            ApiKind::Method => write!(f, "method"),
            ApiKind::ClassMethod => write!(f, "classmethod"),
            ApiKind::StaticMethod => write!(f, "staticmethod"),
            ApiKind::Property => write!(f, "property"),
            ApiKind::Class => write!(f, "class"),
            ApiKind::Struct => write!(f, "struct"),
            ApiKind::Trait => write!(f, "trait"),
            ApiKind::Interface => write!(f, "interface"),
            ApiKind::Enum => write!(f, "enum"),
            ApiKind::Constant => write!(f, "constant"),
            ApiKind::TypeAlias => write!(f, "type_alias"),
        }
    }
}

/// Function or method signature with typed parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signature {
    /// Parameters with full type information
    pub params: Vec<Param>,
    /// Return type annotation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_type: Option<String>,
    /// Whether the function is declared async
    #[serde(default)]
    pub is_async: bool,
    /// Whether the function is a generator (yield/yield from)
    #[serde(default)]
    pub is_generator: bool,
}

/// A single parameter with type information, defaults, and variadic markers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Param {
    /// Parameter name
    pub name: String,
    /// Type annotation (e.g., "str", "int", "Optional[str]")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_annotation: Option<String>,
    /// Default value (e.g., "None", "42", "\"hello\"")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Whether this is a variadic parameter (*args / ...rest)
    #[serde(default)]
    pub is_variadic: bool,
    /// Whether this is a keyword parameter (**kwargs)
    #[serde(default)]
    pub is_keyword: bool,
}

/// Source location of an API entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Location {
    /// Source file path (relative to package root)
    pub file: PathBuf,
    /// Line number (1-indexed)
    pub line: usize,
    /// Column number (0-indexed, optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
}

/// Result of resolving a package to its source directory.
#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    /// Root directory containing the package source files
    pub root_dir: PathBuf,
    /// Package name
    pub package_name: String,
    /// Whether the source is pure Python (vs. C extension)
    pub is_pure_source: bool,
    /// Names exported via `__all__` (Python), or None if unrestricted
    pub public_names: Option<Vec<String>>,
}

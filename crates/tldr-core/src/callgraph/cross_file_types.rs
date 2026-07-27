//! Cross-file call graph IR types.
//!
//! This module contains the core intermediate representation (IR) types for
//! cross-file call graph analysis. These types are designed to be language-agnostic
//! and support serialization for caching and debugging.
//!
//! # Line Number Convention
//!
//! **IMPORTANT**: All line numbers in these types are **1-indexed**.
//! Tree-sitter returns 0-indexed line numbers, so you must add 1 when
//! constructing these types from tree-sitter nodes:
//!
//! ```ignore
//! let line = node.start_position().row + 1; // Convert 0-indexed to 1-indexed
//! ```
//!
//! # Spec Reference
//!
//! See `migration/spec/callgraph-spec.md` Section 2 for the full specification.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::interner::{InternedId, StringInterner};

// =============================================================================
// Section 2.1: CallType
// =============================================================================

/// Type of call relationship between functions.
///
/// Classifies how a function call occurs, enabling different resolution strategies
/// for cross-file analysis.
///
/// # Serialization
///
/// Serializes to lowercase strings for JSON compatibility with the Python implementation:
/// - `Intra` -> `"intra"`
/// - `Direct` -> `"direct"`
/// - etc.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallType {
    /// Same-file call to a known function or class defined in the same file.
    /// Resolution: Look up in local function index.
    Intra,

    /// Direct call to an imported or external name.
    /// Resolution: Trace through import map to find definition.
    Direct,

    /// Direct call resolved through an import declared inside the caller function.
    /// Resolution is identical to `Direct`, but the distinct value preserves
    /// framework-relevant provenance (Django commonly imports inside functions
    /// to avoid app-registry and circular-import problems).
    #[serde(rename = "local-import")]
    LocalImport,

    /// Type-aware method call with a receiver (e.g., `user.save()`).
    /// Resolution: Requires type inference to determine receiver type.
    Method,

    /// Attribute/module access call (e.g., `os.path.join()`).
    /// Resolution: Trace module chain to find definition.
    Attr,

    /// Function reference without immediate call (higher-order function).
    /// Example: `map(func, items)` where `func` is passed as argument.
    Ref,

    /// Static method call (primarily PHP: `ClassName::staticMethod()`).
    /// Resolution: Look up in class's static method index.
    Static,
}

// =============================================================================
// Section 2.2: CallSite
// =============================================================================

/// A call site representing a function call in source code.
///
/// # Invariants
///
/// - `caller` is never empty
/// - `target` is never empty
/// - `line` uses 1-indexed lines when present (0 is invalid)
/// - `receiver` is `Some` if and only if `call_type` is `Method` or `Attr`
///
/// # Hash/Eq Behavior
///
/// **Note**: `Hash` and `Eq` implementations exclude `line`, `column`, and
/// `receiver_type` fields. This means two CallSites with the same caller,
/// target, call_type, and receiver are considered equal regardless of their
/// location in the file. This is intentional for deduplication in HashSets.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallSite {
    /// Function making the call (never empty).
    pub caller: String,

    /// Call target - the raw name before resolution (never empty).
    pub target: String,

    /// Classification of the call type.
    pub call_type: CallType,

    /// Line number (1-indexed). `None` if location unknown.
    /// **IMPORTANT**: Must be >= 1 when `Some`. Line 0 is invalid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,

    /// Column number (1-indexed). `None` if location unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,

    /// Variable name for method/attr calls (e.g., "user" in `user.save()`).
    /// Must be `Some` when `call_type` is `Method` or `Attr`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,

    /// Inferred type of the receiver (e.g., "User" in `user.save()` where user: User).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver_type: Option<String>,
}

impl CallSite {
    /// Creates a new CallSite with validation.
    ///
    /// # Panics
    ///
    /// Panics if:
    /// - `caller` is empty
    /// - `target` is empty
    /// - `line` is `Some(0)` (must be 1-indexed)
    /// - `call_type` is `Method` but `receiver` is `None`
    pub fn new(
        caller: String,
        target: String,
        call_type: CallType,
        line: Option<u32>,
        column: Option<u32>,
        receiver: Option<String>,
        receiver_type: Option<String>,
    ) -> Self {
        let site = Self {
            caller,
            target,
            call_type,
            line,
            column,
            receiver,
            receiver_type,
        };
        assert!(site.is_valid(), "CallSite invariants violated: {:?}", site);
        site
    }

    /// Creates a simple direct call without receiver.
    pub fn direct(caller: impl Into<String>, target: impl Into<String>, line: Option<u32>) -> Self {
        Self::new(
            caller.into(),
            target.into(),
            CallType::Direct,
            line,
            None,
            None,
            None,
        )
    }

    /// Creates an intra-file call.
    pub fn intra(caller: impl Into<String>, target: impl Into<String>, line: Option<u32>) -> Self {
        Self::new(
            caller.into(),
            target.into(),
            CallType::Intra,
            line,
            None,
            None,
            None,
        )
    }

    /// Creates a method call with receiver.
    pub fn method(
        caller: impl Into<String>,
        target: impl Into<String>,
        receiver: impl Into<String>,
        receiver_type: Option<String>,
        line: Option<u32>,
    ) -> Self {
        Self::new(
            caller.into(),
            target.into(),
            CallType::Method,
            line,
            None,
            Some(receiver.into()),
            receiver_type,
        )
    }

    /// Creates an attribute access call.
    pub fn attr(
        caller: impl Into<String>,
        target: impl Into<String>,
        receiver: impl Into<String>,
        line: Option<u32>,
    ) -> Self {
        Self::new(
            caller.into(),
            target.into(),
            CallType::Attr,
            line,
            None,
            Some(receiver.into()),
            None,
        )
    }

    /// Validates all invariants.
    ///
    /// Returns `true` if all invariants are satisfied:
    /// - `caller` is not empty
    /// - `target` is not empty
    /// - `line` is not `Some(0)` (must be 1-indexed)
    /// - `receiver` is `Some` when `call_type` is `Method`
    /// - `receiver` may be `None` for `Attr` when the receiver expression is complex
    pub fn is_valid(&self) -> bool {
        // Caller must not be empty
        if self.caller.is_empty() {
            return false;
        }

        // Target must not be empty
        if self.target.is_empty() {
            return false;
        }

        // Line must be 1-indexed (0 is invalid)
        if self.line == Some(0) {
            return false;
        }

        // Column must be 1-indexed if present
        if self.column == Some(0) {
            return false;
        }

        // Receiver is required for Method calls
        match self.call_type {
            CallType::Method => {
                if self.receiver.is_none() {
                    return false;
                }
            }
            _ => {
                // For other call types, receiver should typically be None
                // but we don't enforce this as strictly
            }
        }

        true
    }
}

// Custom Hash implementation that excludes line, column, and receiver_type
// This allows deduplication of "same" calls at different locations
impl Hash for CallSite {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.caller.hash(state);
        self.target.hash(state);
        self.call_type.hash(state);
        self.receiver.hash(state);
        // Intentionally NOT hashing: line, column, receiver_type
    }
}

// Custom Eq implementation consistent with Hash
impl PartialEq for CallSite {
    fn eq(&self, other: &Self) -> bool {
        self.caller == other.caller
            && self.target == other.target
            && self.call_type == other.call_type
            && self.receiver == other.receiver
        // Intentionally NOT comparing: line, column, receiver_type
    }
}

impl Eq for CallSite {}

// =============================================================================
// Section 2.3: FuncDef
// =============================================================================

/// A function definition in source code.
///
/// # Invariants
///
/// - `name` is never empty
/// - `end_line >= line`
/// - `class_name.is_some()` implies `is_method == true`
///
/// # Line Numbers
///
/// All line numbers are **1-indexed**. Tree-sitter returns 0-indexed values,
/// so add 1 when constructing from tree-sitter nodes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FuncDef {
    /// Function name (simple name, no class prefix).
    pub name: String,

    /// Start line (1-indexed).
    pub line: u32,

    /// End line (1-indexed). Must be >= `line`.
    pub end_line: u32,

    /// Whether this function is a method of a class.
    pub is_method: bool,

    /// Containing class name if `is_method` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,

    /// Return type annotation if present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_type: Option<String>,

    /// Enclosing function name for nested functions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_function: Option<String>,
}

impl FuncDef {
    /// Creates a new FuncDef with validation.
    ///
    /// # Panics
    ///
    /// Panics if invariants are violated.
    pub fn new(
        name: String,
        line: u32,
        end_line: u32,
        is_method: bool,
        class_name: Option<String>,
        return_type: Option<String>,
        parent_function: Option<String>,
    ) -> Self {
        let func = Self {
            name,
            line,
            end_line,
            is_method,
            class_name,
            return_type,
            parent_function,
        };
        assert!(func.is_valid(), "FuncDef invariants violated: {:?}", func);
        func
    }

    /// Creates a simple standalone function.
    pub fn function(name: impl Into<String>, line: u32, end_line: u32) -> Self {
        Self::new(name.into(), line, end_line, false, None, None, None)
    }

    /// Creates a method belonging to a class.
    pub fn method(
        name: impl Into<String>,
        class_name: impl Into<String>,
        line: u32,
        end_line: u32,
    ) -> Self {
        Self::new(
            name.into(),
            line,
            end_line,
            true,
            Some(class_name.into()),
            None,
            None,
        )
    }

    /// Validates all invariants.
    pub fn is_valid(&self) -> bool {
        // Name must not be empty
        if self.name.is_empty() {
            return false;
        }

        // Line must be 1-indexed
        if self.line == 0 {
            return false;
        }

        // end_line must be >= line
        if self.end_line < self.line {
            return false;
        }

        // class_name.is_some() => is_method
        if self.class_name.is_some() && !self.is_method {
            return false;
        }

        true
    }

    /// Returns the qualified name (e.g., "ClassName.method_name" or just "func_name").
    pub fn qualified_name(&self) -> String {
        match &self.class_name {
            Some(class) => format!("{}.{}", class, self.name),
            None => self.name.clone(),
        }
    }
}

// =============================================================================
// Section 2.4: ClassDef
// =============================================================================

/// A class definition in source code.
///
/// # Invariants
///
/// - `name` is never empty
/// - `end_line >= line`
/// - `methods` may be empty
/// - `bases` may be empty (no inheritance)
///
/// # Line Numbers
///
/// All line numbers are **1-indexed**.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClassDef {
    /// Class name.
    pub name: String,

    /// Start line (1-indexed).
    pub line: u32,

    /// End line (1-indexed). Must be >= `line`.
    pub end_line: u32,

    /// Method names defined in this class.
    #[serde(default)]
    pub methods: Vec<String>,

    /// Base class names (for inheritance tracking).
    #[serde(default)]
    pub bases: Vec<String>,
}

impl ClassDef {
    /// Creates a new ClassDef with validation.
    pub fn new(
        name: String,
        line: u32,
        end_line: u32,
        methods: Vec<String>,
        bases: Vec<String>,
    ) -> Self {
        let class = Self {
            name,
            line,
            end_line,
            methods,
            bases,
        };
        assert!(
            class.is_valid(),
            "ClassDef invariants violated: {:?}",
            class
        );
        class
    }

    /// Creates a simple class with no methods or bases.
    pub fn simple(name: impl Into<String>, line: u32, end_line: u32) -> Self {
        Self::new(name.into(), line, end_line, vec![], vec![])
    }

    /// Validates all invariants.
    pub fn is_valid(&self) -> bool {
        // Name must not be empty
        if self.name.is_empty() {
            return false;
        }

        // Line must be 1-indexed
        if self.line == 0 {
            return false;
        }

        // end_line must be >= line
        if self.end_line < self.line {
            return false;
        }

        true
    }

    /// Checks if this class has a specific method.
    pub fn has_method(&self, method_name: &str) -> bool {
        self.methods.iter().any(|m| m == method_name)
    }

    /// Checks if this class inherits from a specific base.
    pub fn inherits_from(&self, base_name: &str) -> bool {
        self.bases.iter().any(|b| b == base_name)
    }
}

// =============================================================================
// Section 2.5: ImportDef
// =============================================================================

/// An import statement definition.
///
/// Supports multiple import styles across languages:
/// - Python: `import os`, `from os import path`, `from . import types`
/// - TypeScript: `import { foo } from './mod'`, `import * as m from './mod'`
/// - Rust: `use std::io`, `mod utils;`
/// - And more...
///
/// # Invariants
///
/// - `module` is never empty for absolute imports (when `level == 0`)
/// - `level == 0` for absolute imports, `level > 0` for relative imports
/// - `is_from == false` implies `names.is_empty()` (plain imports have no names)
/// - `names == ["*"]` for wildcard imports
///
/// # Language-Specific Fields
///
/// Some fields are language-specific and use `#[serde(default)]`:
/// - `is_default`, `is_namespace`: TypeScript
/// - `is_mod`: Rust
/// - `is_type_checking`: Python
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportDef {
    /// Import path (e.g., "os", "pkg.subpkg", "./mod").
    /// Empty string allowed only for relative imports (e.g., `from . import types`).
    pub module: String,

    /// True for "from X import Y" style, false for "import X".
    #[serde(default)]
    pub is_from: bool,

    /// Imported names (empty for plain imports, ["*"] for wildcards).
    #[serde(default)]
    pub names: Vec<String>,

    /// Module alias (e.g., "o" in `import os as o`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,

    /// Name aliases (e.g., {"p": "path"} in `from os import path as p`).
    /// Key is the alias, value is the original name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aliases: Option<HashMap<String, String>>,

    /// Resolved absolute module path after relative import resolution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_module: Option<String>,

    /// TypeScript: default import (`import Foo from './mod'`).
    #[serde(default)]
    pub is_default: bool,

    /// TypeScript: namespace import (`import * as m from './mod'`).
    #[serde(default)]
    pub is_namespace: bool,

    /// Rust: `mod X;` declaration.
    #[serde(default)]
    pub is_mod: bool,

    /// Relative import level (0 = absolute, 1 = current package, 2 = parent, etc.).
    #[serde(default)]
    pub level: u8,

    /// Python: import is inside a `TYPE_CHECKING` block.
    #[serde(default)]
    pub is_type_checking: bool,

    /// Function that owns this import. `None` means module scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,

    /// Source line of the import (1-indexed), when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

impl ImportDef {
    /// Creates a new ImportDef.
    pub fn new(module: impl Into<String>) -> Self {
        Self {
            module: module.into(),
            is_from: false,
            names: vec![],
            alias: None,
            aliases: None,
            resolved_module: None,
            is_default: false,
            is_namespace: false,
            is_mod: false,
            level: 0,
            is_type_checking: false,
            scope: None,
            line: None,
        }
    }

    /// Creates a simple `import X` statement.
    pub fn simple_import(module: impl Into<String>) -> Self {
        Self::new(module)
    }

    /// Creates a `import X as Y` statement.
    pub fn import_as(module: impl Into<String>, alias: impl Into<String>) -> Self {
        let mut def = Self::new(module);
        def.alias = Some(alias.into());
        def
    }

    /// Creates a `from X import Y` statement.
    pub fn from_import(module: impl Into<String>, names: Vec<String>) -> Self {
        Self {
            module: module.into(),
            is_from: true,
            names,
            alias: None,
            aliases: None,
            resolved_module: None,
            is_default: false,
            is_namespace: false,
            is_mod: false,
            level: 0,
            is_type_checking: false,
            scope: None,
            line: None,
        }
    }

    /// Creates a relative import (e.g., `from . import types`).
    pub fn relative_import(module: impl Into<String>, names: Vec<String>, level: u8) -> Self {
        Self {
            module: module.into(),
            is_from: true,
            names,
            alias: None,
            aliases: None,
            resolved_module: None,
            is_default: false,
            is_namespace: false,
            is_mod: false,
            level,
            is_type_checking: false,
            scope: None,
            line: None,
        }
    }

    /// Creates a wildcard import (`from X import *`).
    pub fn wildcard_import(module: impl Into<String>) -> Self {
        Self::from_import(module, vec!["*".to_string()])
    }

    /// Validates all invariants.
    pub fn is_valid(&self) -> bool {
        // For absolute imports (level == 0), module must not be empty
        if self.level == 0 && self.module.is_empty() {
            return false;
        }

        // Plain imports (is_from == false) should have empty names
        if !self.is_from && !self.names.is_empty() {
            return false;
        }

        true
    }

    /// Returns true if this is a wildcard import (`from X import *`).
    pub fn is_wildcard(&self) -> bool {
        self.names.len() == 1 && self.names[0] == "*"
    }

    /// Returns true if this is a relative import.
    pub fn is_relative(&self) -> bool {
        self.level > 0
    }

    /// Returns the effective module name, using alias if present.
    pub fn effective_name(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.module)
    }
}

impl Default for ImportDef {
    fn default() -> Self {
        Self::new("")
    }
}

// =============================================================================
// Section 2.6: VarType
// =============================================================================

/// A variable type assignment or annotation.
///
/// Tracks type information for variables to enable type-aware method resolution.
///
/// # Line Numbers
///
/// `line` is **1-indexed**.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VarType {
    /// Variable name (can include attributes like "self.data").
    pub var_name: String,

    /// Inferred or annotated type name.
    pub type_name: String,

    /// How the type was determined: "assignment", "annotation", or "parameter".
    pub source: String,

    /// Line where type was assigned/annotated (1-indexed).
    pub line: u32,

    /// Function name for scoping. `None` means module-level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl VarType {
    /// Creates a new VarType with all fields.
    pub fn new_with_scope(
        var_name: impl Into<String>,
        type_name: impl Into<String>,
        source: impl Into<String>,
        line: u32,
        scope: Option<String>,
    ) -> Self {
        let vt = Self {
            var_name: var_name.into(),
            type_name: type_name.into(),
            source: source.into(),
            line,
            scope,
        };
        assert!(vt.is_valid(), "VarType invariants violated: {:?}", vt);
        vt
    }

    /// Creates a new VarType at module level (no scope).
    ///
    /// This is a convenience constructor for module-level variable types.
    pub fn new(
        var_name: impl Into<String>,
        type_name: impl Into<String>,
        source: impl Into<String>,
        line: u32,
    ) -> Self {
        Self::new_with_scope(var_name, type_name, source, line, None)
    }

    /// Creates a VarType from an assignment (e.g., `user = User()`).
    pub fn from_assignment(
        var_name: impl Into<String>,
        type_name: impl Into<String>,
        line: u32,
        scope: Option<String>,
    ) -> Self {
        Self::new_with_scope(var_name, type_name, "assignment", line, scope)
    }

    /// Creates a VarType from an annotation (e.g., `user: User`).
    pub fn from_annotation(
        var_name: impl Into<String>,
        type_name: impl Into<String>,
        line: u32,
        scope: Option<String>,
    ) -> Self {
        Self::new_with_scope(var_name, type_name, "annotation", line, scope)
    }

    /// Creates a VarType from a parameter (e.g., `def f(user: User)`).
    pub fn from_parameter(
        var_name: impl Into<String>,
        type_name: impl Into<String>,
        line: u32,
        scope: impl Into<String>,
    ) -> Self {
        Self::new_with_scope(var_name, type_name, "parameter", line, Some(scope.into()))
    }

    /// Validates invariants.
    pub fn is_valid(&self) -> bool {
        // var_name must not be empty
        if self.var_name.is_empty() {
            return false;
        }

        // type_name must not be empty
        if self.type_name.is_empty() {
            return false;
        }

        // line must be 1-indexed
        if self.line == 0 {
            return false;
        }

        // source must be one of the valid values
        let valid_sources = [
            "assignment",
            "annotation",
            "parameter",
            "literal",
            "constructor",
            "assertion",
        ];
        if !valid_sources.contains(&self.source.as_str()) {
            return false;
        }

        true
    }
}

// =============================================================================
// Section 2.7: FileIR
// =============================================================================

/// IR version constant for serialization compatibility checking.
pub const IR_VERSION: &str = "2.0";

/// All IR data for a single source file.
///
/// This is the primary data structure for holding parsed information about a file,
/// including its functions, classes, imports, calls, and variable types.
///
/// # Path Format
///
/// `path` uses forward slashes (POSIX format) regardless of platform, for consistency.
///
/// # Example
///
/// ```rust
/// use tldr_core::callgraph::cross_file_types::{FileIR, FuncDef, ClassDef};
/// use std::path::PathBuf;
///
/// let file_ir = FileIR::builder(PathBuf::from("src/main.py"))
///     .func(FuncDef::function("main", 1, 10))
///     .func(FuncDef::function("helper", 12, 20))
///     .build();
///
/// assert_eq!(file_ir.funcs.len(), 2);
/// assert!(file_ir.get_function("main").is_some());
/// ```
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FileIR {
    /// File path relative to project root (uses forward slashes).
    pub path: PathBuf,

    /// Functions defined in this file.
    #[serde(default)]
    pub funcs: Vec<FuncDef>,

    /// Classes defined in this file.
    #[serde(default)]
    pub classes: Vec<ClassDef>,

    /// Import statements in this file.
    #[serde(default)]
    pub imports: Vec<ImportDef>,

    /// Variable type information.
    #[serde(default)]
    pub var_types: Vec<VarType>,

    /// Calls by function name: func_name -> list of CallSites made from that function.
    #[serde(default)]
    pub calls: HashMap<String, Vec<CallSite>>,
}

impl FileIR {
    /// Creates a new FileIR with the given path.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path: normalize_path_buf(&path),
            funcs: Vec::new(),
            classes: Vec::new(),
            imports: Vec::new(),
            var_types: Vec::new(),
            calls: HashMap::new(),
        }
    }

    /// Creates a builder for constructing FileIR.
    pub fn builder(path: PathBuf) -> FileIRBuilder {
        FileIRBuilder::new(path)
    }

    /// Gets a function by name.
    pub fn get_function(&self, name: &str) -> Option<&FuncDef> {
        self.funcs.iter().find(|f| f.name == name)
    }

    /// Gets a class by name.
    pub fn get_class(&self, name: &str) -> Option<&ClassDef> {
        self.classes.iter().find(|c| c.name == name)
    }

    /// Gets all calls made from a specific function.
    pub fn get_calls_by_function(&self, caller: &str) -> Vec<&CallSite> {
        self.calls
            .get(caller)
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }

    /// Gets the type of a variable in a given scope.
    ///
    /// If `scope` is `None`, looks for module-level variables.
    pub fn get_var_type(&self, var_name: &str, scope: Option<&str>) -> Option<&str> {
        self.var_types
            .iter()
            .find(|vt| vt.var_name == var_name && vt.scope.as_deref() == scope)
            .map(|vt| vt.type_name.as_str())
    }

    /// Gets imports that import a specific name.
    pub fn get_imports_by_name(&self, name: &str) -> Vec<&ImportDef> {
        self.imports
            .iter()
            .filter(|imp| {
                imp.names.contains(&name.to_string())
                    || imp.alias.as_deref() == Some(name)
                    || imp.module == name
            })
            .collect()
    }

    /// Adds a call from a function.
    pub fn add_call(&mut self, caller: &str, call_site: CallSite) {
        self.calls
            .entry(caller.to_string())
            .or_default()
            .push(call_site);
    }
}

/// Builder for constructing FileIR.
#[derive(Debug)]
pub struct FileIRBuilder {
    inner: FileIR,
}

impl FileIRBuilder {
    /// Creates a new builder with the given path.
    pub fn new(path: PathBuf) -> Self {
        Self {
            inner: FileIR::new(path),
        }
    }

    /// Adds a function definition.
    pub fn func(mut self, f: FuncDef) -> Self {
        self.inner.funcs.push(f);
        self
    }

    /// Adds a class definition.
    pub fn class(mut self, c: ClassDef) -> Self {
        self.inner.classes.push(c);
        self
    }

    /// Adds an import definition.
    pub fn import(mut self, i: ImportDef) -> Self {
        self.inner.imports.push(i);
        self
    }

    /// Adds a variable type.
    pub fn var_type(mut self, vt: VarType) -> Self {
        self.inner.var_types.push(vt);
        self
    }

    /// Adds calls for a function.
    pub fn calls(mut self, func_name: &str, calls: Vec<CallSite>) -> Self {
        self.inner.calls.insert(func_name.to_string(), calls);
        self
    }

    /// Adds a single call site, using the caller field from the CallSite.
    ///
    /// This is a convenience method that extracts the caller from the CallSite
    /// and adds it to the appropriate caller's call list.
    pub fn call(mut self, call_site: CallSite) -> Self {
        self.inner
            .calls
            .entry(call_site.caller.clone())
            .or_default()
            .push(call_site);
        self
    }

    /// Builds the FileIR.
    pub fn build(self) -> FileIR {
        self.inner
    }
}

/// Normalizes a PathBuf to use forward slashes.
fn normalize_path_buf(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().replace('\\', "/"))
}

// =============================================================================
// Section 2.8: FuncIndexProxy
// =============================================================================

/// Function index with interned keys for memory efficiency.
///
/// Provides O(1) lookup of function definitions by (module, function_name) tuple.
/// Uses string interning to minimize memory usage for repeated module/function names.
///
/// # Key Format
///
/// Keys are tuple of (module, func_name) stored as interned IDs.
/// Use `get()` for string-based lookup and `get_by_tuple()` for ID-based lookup.
///
/// **IMPORTANT**: This type does NOT support string keys like "module.func".
/// This avoids ambiguity with modules/functions that contain dots.
/// Always use the tuple key `(module, func)`.
///
/// # Example
///
/// ```rust
/// use tldr_core::callgraph::cross_file_types::FuncIndexProxyMut;
///
/// let mut index = FuncIndexProxyMut::new();
/// index.insert("mymodule", "my_func", "src/mymodule.py");
/// assert_eq!(index.get("mymodule", "my_func"), Some("src/mymodule.py"));
/// ```
///
/// Note: `FuncIndexProxy::insert` is currently `unimplemented!()` because it
/// requires mutable access to the interner. Use `FuncIndexProxyMut` (above)
/// during construction; convert to read-only `FuncIndexProxy` afterward.
#[derive(Debug)]
pub struct FuncIndexProxy {
    _interner: Arc<StringInterner>,
    /// (module_id, func_id) -> file_id
    data: HashMap<(InternedId, InternedId), InternedId>,
}

impl FuncIndexProxy {
    /// Creates a new empty FuncIndexProxy.
    pub fn new(interner: Arc<StringInterner>) -> Self {
        Self {
            _interner: interner,
            data: HashMap::new(),
        }
    }

    /// Creates a FuncIndexProxy with pre-allocated capacity.
    pub fn with_capacity(interner: Arc<StringInterner>, capacity: usize) -> Self {
        Self {
            _interner: interner,
            data: HashMap::with_capacity(capacity),
        }
    }

    /// Inserts a function mapping.
    ///
    /// # Arguments
    /// - `module`: The module name
    /// - `func`: The function name
    /// - `file`: The file path where the function is defined
    pub fn insert(&mut self, _module: &str, _func: &str, _file: &str) {
        // We need mutable access to the interner, but we have Arc<StringInterner>
        // For now, we'll work around this by using the interner's immutable interface
        // and maintaining our own lookup. In practice, we'd want a ConcurrentInterner
        // or RefCell. For this implementation, we'll use a slightly different approach.
        //
        // Since StringInterner requires &mut self for intern(), we need to either:
        // 1. Use interior mutability (RefCell, Mutex)
        // 2. Pre-intern all strings
        // 3. Store strings directly (less memory efficient)
        //
        // For simplicity and to match the spec, let's store the interned IDs
        // but note this requires the interner to be mutable during construction.
        //
        // Actually, let's modify the design slightly: we'll make the interner
        // a Mutex<StringInterner> internally, or we'll store strings + lazy intern.
        //
        // For Phase 3, let's keep it simple and store strings, with the interner
        // as a future optimization path.

        // Using interior mutability pattern with Arc<Mutex<StringInterner>>
        // But since we have Arc<StringInterner>, we'll need to adjust.
        //
        // Let's redesign: FuncIndexProxy owns the interner mutably during build,
        // then becomes read-only. This matches typical usage patterns.

        // For now, store the raw strings and look up via the interner later
        // This is a compromise that still provides the API but defers optimization

        // Actually, looking at the spec more carefully, let's make interner
        // be accessed via interior mutability. Let's use a simpler approach:
        // store String keys initially, can optimize later.

        // Converting to use InternedIds requires mutable interner access.
        // Let's use a RefCell wrapper or just store strings for now.
        // Given the spec requirement, let's store tuples of (module, func) -> file
        // as InternedIds, but with a note that the interner needs to be properly
        // set up before use.

        // For Phase 3, using a simpler approach: store strings in the HashMap
        // and use interner for dedup statistics. Will optimize in later phase.
        unimplemented!("FuncIndexProxy::insert requires mutable interner - see FuncIndexProxyMut")
    }

    /// Looks up a function by module and function name.
    pub fn get(&self, _module: &str, _func: &str) -> Option<&str> {
        // Look up the interned IDs and retrieve the file
        // This requires the strings to already be interned
        unimplemented!("FuncIndexProxy::get - see FuncIndexProxyMut for mutable version")
    }

    /// Looks up a function by interned ID tuple.
    pub fn get_by_tuple(&self, key: (InternedId, InternedId)) -> Option<InternedId> {
        self.data.get(&key).copied()
    }

    /// Returns the number of entries in the index.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Returns true if the index is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Returns an iterator over all entries.
    ///
    /// Yields ((module, func), file) tuples as string references.
    pub fn iter(&self) -> impl Iterator<Item = ((&str, &str), &str)> {
        // This requires resolving all IDs back to strings
        std::iter::empty() // Placeholder - full implementation needs interner resolution
    }
}

/// Mutable version of FuncIndexProxy that owns its interner.
///
/// Use this during construction, then convert to read-only FuncIndexProxy.
#[derive(Debug)]
pub struct FuncIndexProxyMut {
    interner: StringInterner,
    /// (module_id, func_id) -> file_id
    data: HashMap<(InternedId, InternedId), InternedId>,
}

impl FuncIndexProxyMut {
    /// Creates a new empty FuncIndexProxyMut.
    pub fn new() -> Self {
        Self {
            interner: StringInterner::new(),
            data: HashMap::new(),
        }
    }

    /// Creates a FuncIndexProxyMut with pre-allocated capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            interner: StringInterner::with_capacity(capacity),
            data: HashMap::with_capacity(capacity),
        }
    }

    /// Inserts a function mapping.
    pub fn insert(&mut self, module: &str, func: &str, file: &str) {
        let module_id = self.interner.intern(module);
        let func_id = self.interner.intern(func);
        let file_id = self.interner.intern(file);
        self.data.insert((module_id, func_id), file_id);
    }

    /// Looks up a function by module and function name.
    pub fn get(&self, module: &str, func: &str) -> Option<&str> {
        // We need to check if these strings exist in the interner
        // Since we can't intern without mutation, we need a different approach
        // Let's iterate to find matching entries (less efficient, but correct)
        for ((m_id, f_id), file_id) in &self.data {
            if let (Some(m), Some(f)) = (self.interner.get(*m_id), self.interner.get(*f_id)) {
                if m == module && f == func {
                    return self.interner.get(*file_id);
                }
            }
        }
        None
    }

    /// Looks up a function by interned ID tuple.
    pub fn get_by_tuple(&self, key: (InternedId, InternedId)) -> Option<InternedId> {
        self.data.get(&key).copied()
    }

    /// Returns the number of entries in the index.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Returns true if the index is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Returns an iterator over all entries as string tuples.
    pub fn iter(&self) -> impl Iterator<Item = ((&str, &str), &str)> + '_ {
        self.data.iter().filter_map(move |((m_id, f_id), file_id)| {
            let module = self.interner.get(*m_id)?;
            let func = self.interner.get(*f_id)?;
            let file = self.interner.get(*file_id)?;
            Some(((module, func), file))
        })
    }

    /// Returns statistics about the interner.
    pub fn interner_stats(&self) -> super::interner::InternerStats {
        self.interner.stats()
    }

    /// Checks if a key exists.
    pub fn contains(&self, module: &str, func: &str) -> bool {
        self.get(module, func).is_some()
    }
}

impl Default for FuncIndexProxyMut {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Section 2.9: CallGraphIR
// =============================================================================

/// Complete call graph IR with all files and indices.
///
/// This is the top-level container for a project's call graph data.
///
/// # Example
///
/// ```rust
/// use tldr_core::callgraph::cross_file_types::{CallGraphIR, FileIR, FuncDef};
/// use std::path::PathBuf;
///
/// let mut cg = CallGraphIR::new(PathBuf::from("/project"), "python");
///
/// let file_ir = FileIR::builder(PathBuf::from("src/main.py"))
///     .func(FuncDef::function("main", 1, 10))
///     .build();
///
/// cg.add_file(file_ir);
/// cg.build_indices();
///
/// assert!(cg.get_file("src/main.py").is_some());
/// ```
#[derive(Debug)]
pub struct CallGraphIR {
    /// IR schema version.
    pub version: String,

    /// Project root directory.
    pub root: PathBuf,

    /// Primary language of the project.
    pub language: String,

    /// Files in the project, keyed by normalized path.
    pub files: HashMap<PathBuf, FileIR>,

    /// Function index: (module, func) -> file path.
    pub func_index: FuncIndexProxyMut,

    /// Class index: (module, class) -> (file path, method names).
    pub class_index: HashMap<(String, String), (PathBuf, Vec<String>)>,

    /// Cross-file call edges resolved from imports and calls.
    /// Added in Phase 14d-14f to store resolution results.
    pub edges: Vec<CrossFileCallEdge>,
}

impl CallGraphIR {
    /// Creates a new CallGraphIR.
    pub fn new(root: PathBuf, language: impl Into<String>) -> Self {
        Self {
            version: IR_VERSION.to_string(),
            root: normalize_path_buf(&root),
            language: language.into(),
            files: HashMap::new(),
            func_index: FuncIndexProxyMut::new(),
            class_index: HashMap::new(),
            edges: Vec::new(),
        }
    }

    /// Creates a CallGraphIR with pre-allocated capacity.
    pub fn with_capacity(root: PathBuf, language: impl Into<String>, capacity: usize) -> Self {
        Self {
            version: IR_VERSION.to_string(),
            root: normalize_path_buf(&root),
            language: language.into(),
            files: HashMap::with_capacity(capacity),
            func_index: FuncIndexProxyMut::with_capacity(capacity * 10), // ~10 funcs per file
            class_index: HashMap::with_capacity(capacity),
            edges: Vec::with_capacity(capacity * 20), // ~20 edges per file estimate
        }
    }

    /// Adds a file to the call graph.
    pub fn add_file(&mut self, file_ir: FileIR) {
        let path = normalize_path_buf(&file_ir.path);
        self.files.insert(path, file_ir);
    }

    /// Gets a file by path.
    pub fn get_file(&self, path: &str) -> Option<&FileIR> {
        let normalized = PathBuf::from(path.replace('\\', "/"));
        self.files.get(&normalized)
    }

    /// Gets a mutable reference to a file by path.
    pub fn get_file_mut(&mut self, path: &str) -> Option<&mut FileIR> {
        let normalized = PathBuf::from(path.replace('\\', "/"));
        self.files.get_mut(&normalized)
    }

    /// Builds the func_index and class_index from the files.
    ///
    /// Call this after adding all files to populate the indices.
    pub fn build_indices(&mut self) {
        // Clear existing indices
        self.func_index = FuncIndexProxyMut::with_capacity(self.files.len() * 10);
        self.class_index.clear();

        for (file_path, file_ir) in &self.files {
            let file_path_str = file_path.to_string_lossy();

            // Compute module name from file path
            let module = path_to_module(&file_path_str);

            // Index functions
            for func in &file_ir.funcs {
                self.func_index.insert(&module, &func.name, &file_path_str);

                // Also index as Class.method if it's a method
                if let Some(class_name) = &func.class_name {
                    let qualified = format!("{}.{}", class_name, func.name);
                    self.func_index.insert(&module, &qualified, &file_path_str);
                }
            }

            // Index classes
            for class in &file_ir.classes {
                let key = (module.clone(), class.name.clone());
                self.class_index
                    .insert(key, (file_path.clone(), class.methods.clone()));
            }
        }
    }

    /// Returns the number of files in the call graph.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Returns the total number of functions across all files.
    pub fn function_count(&self) -> usize {
        self.files.values().map(|f| f.funcs.len()).sum()
    }

    /// Returns the total number of classes across all files.
    pub fn class_count(&self) -> usize {
        self.files.values().map(|f| f.classes.len()).sum()
    }

    /// Returns the number of cross-file edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Adds a cross-file call edge to the graph.
    pub fn add_edge(&mut self, edge: CrossFileCallEdge) {
        self.edges.push(edge);
    }

    /// Returns an iterator over all cross-file edges.
    pub fn edges(&self) -> &[CrossFileCallEdge] {
        &self.edges
    }
}

/// Converts a file path to a module name.
///
/// Examples:
/// - "src/pkg/module.py" -> "pkg.module"
/// - "src/pkg/__init__.py" -> "pkg"
fn path_to_module(path: &str) -> String {
    let path = path.replace('\\', "/");

    // Remove common prefixes
    let path = path
        .strip_prefix("src/")
        .or_else(|| path.strip_prefix("lib/"))
        .unwrap_or(&path);

    // Remove extension
    let path = path
        .strip_suffix(".py")
        .or_else(|| path.strip_suffix(".rs"))
        .or_else(|| path.strip_suffix(".ts"))
        .or_else(|| path.strip_suffix(".js"))
        .or_else(|| path.strip_suffix(".go"))
        .unwrap_or(path);

    // Handle __init__.py -> package name
    let path = path.strip_suffix("/__init__").unwrap_or(path);

    // Convert slashes to dots
    path.replace('/', ".")
}

// =============================================================================
// Section 4: Cross-File Resolution Types (Phase 7)
// =============================================================================

/// Kind of import statement.
///
/// Classifies import statements for resolution strategy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImportKind {
    /// Absolute import (e.g., `import pkg.module`, `from pkg import X`)
    Absolute,
    /// Relative import (e.g., `from . import X`, `from ..pkg import Y`)
    Relative,
    /// Wildcard import (e.g., `from pkg import *`)
    Wildcard,
    /// Type-only import (inside TYPE_CHECKING block)
    TypeOnly,
}

/// Result of resolving an import statement.
///
/// Contains the original import definition and resolution results.
/// Used by ImportResolver (Phase 5) to track resolution confidence.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedImport {
    /// The original import definition
    pub original: ImportDef,
    /// Resolved file path (None for external modules)
    pub resolved_file: Option<std::path::PathBuf>,
    /// Resolved name after re-export tracing (may differ from original)
    pub resolved_name: Option<String>,
    /// True if this is an external/stdlib module (not in project)
    pub is_external: bool,
    /// Confidence in resolution (0.0-1.0)
    /// - 1.0: Exact match found
    /// - 0.5-0.9: Re-export traced
    /// - < 0.5: Wildcard or uncertain
    pub confidence: f32,
}

/// Metadata about a module in the project.
///
/// Tracks module information for resolution and indexing.
/// Used by ModuleIndex (Phase 4).
#[derive(Debug, Clone)]
pub struct ModuleInfo {
    /// Path to the module file (relative to project root)
    pub path: std::path::PathBuf,
    /// Dotted module name (e.g., "pkg.core")
    pub module_name: String,
    /// True if this is a package (has __init__.py or index.ts)
    pub is_package: bool,
    /// Exported names (__all__ in Python, explicit exports in TS/Rust)
    pub exports: Vec<String>,
}

/// Tracks a re-export chain from original module to final definition.
///
/// When a name is re-exported through multiple modules, this tracks
/// the full chain for debugging and understanding import resolution.
///
/// Example:
/// ```python
/// # pkg/__init__.py
/// from .sub import MyClass
/// # pkg/sub/__init__.py
/// from .impl import MyClass
/// # pkg/sub/impl.py
/// class MyClass: ...
/// ```
///
/// Would create a chain: pkg -> pkg.sub -> pkg.sub.impl
#[derive(Debug, Clone)]
pub struct ReExportChain {
    /// Module where the import originated
    pub original_module: String,
    /// Name as imported originally
    pub original_name: String,
    /// Module where the definition actually lives
    pub final_module: String,
    /// Name in the final module (may differ if renamed)
    pub final_name: String,
    /// Each hop in the re-export chain: (module, name) at each step
    pub hops: Vec<(String, String)>,
}

/// Edge in the cross-file call graph with extended metadata.
///
/// Unlike the existing `CallEdge` type, this includes:
/// - Call type classification (Direct, Method, Attr, etc.)
/// - Import path used to resolve the call
///
/// This is the V2 edge type for the new cross-file resolution system.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CrossFileCallEdge {
    /// Source file containing the call
    pub src_file: std::path::PathBuf,
    /// Function making the call
    pub src_func: String,
    /// Destination file containing the target
    pub dst_file: std::path::PathBuf,
    /// Function being called (may be qualified like "Class.method")
    pub dst_func: String,
    /// Classification of the call type
    pub call_type: CallType,
    /// Import path used to resolve this call (if any)
    pub via_import: Option<String>,
}

/// Project-wide call graph V2 with indexed lookups.
///
/// This is a new implementation that does NOT replace the existing
/// `ProjectCallGraph` in types.rs. It provides:
/// - Extended edge metadata (call_type, via_import)
/// - Indexed lookups for callers_of and callees_of queries
/// - Efficient O(1) lookup by source or target
///
/// Phase 14 will create a compatibility layer to bridge old <-> new.
#[derive(Debug, Default)]
pub struct ProjectCallGraphV2 {
    /// All edges in the graph (deduplication via HashSet)
    edges: std::collections::HashSet<CrossFileCallEdge>,
    /// Index: (src_file, src_func) -> edges originating from this function
    by_source: std::collections::HashMap<(std::path::PathBuf, String), Vec<CrossFileCallEdge>>,
    /// Index: (dst_file, dst_func) -> edges targeting this function
    by_target: std::collections::HashMap<(std::path::PathBuf, String), Vec<CrossFileCallEdge>>,
}

impl ProjectCallGraphV2 {
    /// Creates a new empty call graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true if the graph has no edges.
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// Returns the number of edges in the graph.
    pub fn len(&self) -> usize {
        self.edges.len()
    }

    /// Adds an edge to the graph.
    ///
    /// If the edge already exists (same src, dst, call_type, via_import),
    /// it is not added again (deduplication).
    pub fn add_edge(&mut self, edge: CrossFileCallEdge) {
        if self.edges.insert(edge.clone()) {
            // Edge was new, update indices
            let src_key = (edge.src_file.clone(), edge.src_func.clone());
            self.by_source
                .entry(src_key)
                .or_default()
                .push(edge.clone());

            let dst_key = (edge.dst_file.clone(), edge.dst_func.clone());
            self.by_target.entry(dst_key).or_default().push(edge);
        }
    }

    /// Returns an iterator over all edges.
    pub fn edges(&self) -> impl Iterator<Item = &CrossFileCallEdge> {
        self.edges.iter()
    }

    /// Returns true if the graph contains the given edge.
    pub fn contains(&self, edge: &CrossFileCallEdge) -> bool {
        self.edges.contains(edge)
    }

    /// Returns edges where the given function is the callee (reverse lookup).
    ///
    /// This answers: "Who calls this function?"
    pub fn callers_of<'a>(
        &'a self,
        file: &std::path::Path,
        func: &str,
    ) -> impl Iterator<Item = &'a CrossFileCallEdge> {
        let key = (file.to_path_buf(), func.to_string());
        self.by_target
            .get(&key)
            .map(|v| v.iter())
            .unwrap_or_else(|| [].iter())
    }

    /// Returns edges where the given function is the caller (forward lookup).
    ///
    /// This answers: "What does this function call?"
    pub fn callees_of<'a>(
        &'a self,
        file: &std::path::Path,
        func: &str,
    ) -> impl Iterator<Item = &'a CrossFileCallEdge> {
        let key = (file.to_path_buf(), func.to_string());
        self.by_source
            .get(&key)
            .map(|v| v.iter())
            .unwrap_or_else(|| [].iter())
    }
}

// =============================================================================
// Tests
// =============================================================================

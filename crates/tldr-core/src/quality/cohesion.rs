//! Cohesion Analyzer for Health Command
//!
//! This module provides class cohesion analysis using the LCOM4 metric.
//! It creates a NEW implementation (not reusing debt.rs LCOM4 which returns f64).
//!
//! # LCOM4 Algorithm
//!
//! LCOM4 (Lack of Cohesion of Methods 4) measures class cohesion by counting
//! connected components in the method-field graph:
//!
//! 1. For each class, build a graph where nodes = methods
//! 2. Add edges between methods that share at least one field access
//! 3. Count connected components using Union-Find
//! 4. LCOM4 = component count (usize, NOT normalized!)
//!
//! # Interpretation
//!
//! - LCOM4 = 1: Fully cohesive (all methods share fields, single responsibility)
//! - LCOM4 > 1: Multiple responsibilities, candidate for splitting
//! - LCOM4 = 0: Degenerate case (no methods)
//!
//! # Multi-Language Support
//!
//! - Python: class with def methods
//! - TypeScript/JavaScript: class with methods
//! - Java: class/interface/enum with methods
//! - Go: struct with receiver methods
//! - Rust: struct with impl block methods
//! - Ruby: class with def methods, @instance_variable field access
//! - C#: class/struct/interface with methods, this.field access
//! - Scala: class/object/trait with def methods, this.field access
//! - PHP: class/interface/trait with function methods, $this->field access
//!
//! # References
//!
//! - Chidamber & Kemerer, "A Metrics Suite for Object Oriented Design"
//! - Health spec section 4.2

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::walker::walk_project;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::ast::parser::parse;
use crate::error::TldrError;
use crate::types::Language;
use crate::TldrResult;

// =============================================================================
// Types
// =============================================================================

/// Information about a connected component in the method-field graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentInfo {
    /// Methods in this component
    pub methods: Vec<String>,
    /// Fields accessed by methods in this component
    pub fields: Vec<String>,
}

/// Verdict for class cohesion
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CohesionVerdict {
    /// Class is cohesive (LCOM4 <= threshold)
    Cohesive,
    /// Class should be considered for splitting (LCOM4 > threshold)
    SplitCandidate,
}

/// Cohesion analysis for a single class
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassCohesion {
    /// Class/struct name
    pub name: String,
    /// File path containing the class
    pub file: PathBuf,
    /// Line number where the class starts
    pub line: usize,
    /// Number of methods (excluding dunders)
    pub method_count: usize,
    /// Number of unique fields accessed
    pub field_count: usize,
    /// LCOM4 value: raw connected component count (NOT normalized!)
    /// - 0: no methods (degenerate)
    /// - 1: fully cohesive
    /// - >1: multiple responsibilities
    pub lcom4: usize,
    /// Connected components with their methods and fields
    pub components: Vec<ComponentInfo>,
    /// Cohesion verdict based on threshold
    pub verdict: CohesionVerdict,
    /// Optional suggestion for splitting
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split_suggestion: Option<String>,
}

/// Summary statistics for cohesion analysis
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CohesionSummary {
    /// Total number of classes analyzed
    pub total_classes: usize,
    /// Number of cohesive classes (LCOM4 <= threshold)
    pub cohesive: usize,
    /// Number of split candidates (LCOM4 > threshold)
    pub split_candidates: usize,
    /// Average LCOM4 across all classes (None if no classes)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_lcom4: Option<f64>,
}

/// Complete cohesion analysis report
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CohesionReport {
    /// Number of classes analyzed
    pub classes_analyzed: usize,
    /// Average LCOM4 across all classes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_lcom4: Option<f64>,
    /// Number of classes with low cohesion (LCOM4 > threshold)
    pub low_cohesion_count: usize,
    /// All classes with cohesion data (sorted by LCOM4 descending)
    pub classes: Vec<ClassCohesion>,
    /// Summary statistics
    pub summary: CohesionSummary,
}

/// Options for cohesion analysis
#[derive(Debug, Clone)]
pub struct CohesionOptions {
    /// Include dunder methods in analysis (default: false)
    pub include_dunder: bool,
    /// Threshold for low cohesion detection (default: 2)
    /// Classes with LCOM4 > threshold are flagged as SplitCandidate
    pub low_cohesion_threshold: usize,
}

impl Default for CohesionOptions {
    fn default() -> Self {
        Self {
            include_dunder: false,
            low_cohesion_threshold: 2,
        }
    }
}

// =============================================================================
// Union-Find Data Structure
// =============================================================================

/// Union-Find data structure for LCOM4 connected component calculation.
/// Uses iterative path compression to avoid stack overflow.
struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }

    /// Find root with iterative path compression
    fn find(&mut self, x: usize) -> usize {
        let mut root = x;
        // Find root
        while self.parent[root] != root {
            root = self.parent[root];
        }
        // Path compression
        let mut node = x;
        while self.parent[node] != root {
            let next = self.parent[node];
            self.parent[node] = root;
            node = next;
        }
        root
    }

    /// Union by rank
    fn union(&mut self, x: usize, y: usize) {
        let rx = self.find(x);
        let ry = self.find(y);
        if rx != ry {
            if self.rank[rx] < self.rank[ry] {
                self.parent[rx] = ry;
            } else if self.rank[rx] > self.rank[ry] {
                self.parent[ry] = rx;
            } else {
                self.parent[ry] = rx;
                self.rank[rx] += 1;
            }
        }
    }

    /// Count connected components
    fn count_components(&mut self) -> usize {
        let n = self.parent.len();
        if n == 0 {
            return 0;
        }
        (0..n).map(|i| self.find(i)).collect::<HashSet<_>>().len()
    }

    /// Get component ID for each node (after all unions)
    fn get_components(&mut self) -> Vec<usize> {
        let n = self.parent.len();
        (0..n).map(|i| self.find(i)).collect()
    }
}

// =============================================================================
// Internal Types for Class Extraction
// =============================================================================

/// Method information for LCOM4 calculation
struct MethodInfo {
    name: String,
    start_byte: usize,
    end_byte: usize,
}

/// Class information for LCOM4 calculation
struct ClassInfo {
    name: String,
    line: usize,
    methods: Vec<MethodInfo>,
}

// =============================================================================
// Main API
// =============================================================================

/// Analyze class cohesion using LCOM4 metric
///
/// Scans all supported files in the given path, extracts classes, and computes
/// LCOM4 (connected component count) for each class.
///
/// # Arguments
/// * `path` - Directory or file to analyze
/// * `language` - Optional language filter (auto-detect if None)
/// * `threshold` - LCOM4 threshold for low cohesion (default: 2)
///
/// # Returns
/// * `Ok(CohesionReport)` - Report with cohesion metrics per class
/// * `Err(TldrError)` - On file system errors
///
/// # Behavior
/// - LCOM4 = 1 means cohesive (all methods share fields)
/// - LCOM4 > 1 indicates potential for splitting
/// - Dunder methods (__init__, __str__, etc.) excluded by default
/// - Empty classes return LCOM4 = 0 (degenerate case)
///
/// # Example
/// ```ignore
/// use tldr_core::quality::cohesion::analyze_cohesion;
/// use std::path::Path;
///
/// let report = analyze_cohesion(Path::new("src/"), None, 2)?;
/// for class in &report.classes {
///     if class.lcom4 > 2 {
///         println!("{}: LCOM4={} - consider splitting", class.name, class.lcom4);
///     }
/// }
/// ```
pub fn analyze_cohesion(
    path: &Path,
    language: Option<Language>,
    threshold: usize,
) -> TldrResult<CohesionReport> {
    let options = CohesionOptions {
        include_dunder: false,
        low_cohesion_threshold: threshold,
    };

    analyze_cohesion_with_options(path, language, options)
}

/// Analyze class cohesion with full options
pub fn analyze_cohesion_with_options(
    path: &Path,
    language: Option<Language>,
    options: CohesionOptions,
) -> TldrResult<CohesionReport> {
    // Collect files to analyze
    let file_paths: Vec<PathBuf> = if path.is_file() {
        vec![path.to_path_buf()]
    } else {
        walk_project(path)
            .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
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

    // Analyze each file and collect class cohesion data
    let mut all_classes: Vec<ClassCohesion> = Vec::new();

    for file_path in &file_paths {
        if let Ok(classes) = analyze_file_cohesion(file_path, &options) {
            all_classes.extend(classes);
        }
        // Skip files that fail to parse (graceful degradation)
    }

    // Sort by LCOM4 descending (worst cohesion first)
    all_classes.sort_by(|a, b| b.lcom4.cmp(&a.lcom4));

    // Calculate summary statistics
    let total_classes = all_classes.len();
    let total_lcom4: usize = all_classes.iter().map(|c| c.lcom4).sum();
    let avg_lcom4 = if total_classes > 0 {
        Some(total_lcom4 as f64 / total_classes as f64)
    } else {
        None
    };
    let low_cohesion_count = all_classes
        .iter()
        .filter(|c| c.lcom4 > options.low_cohesion_threshold)
        .count();
    let cohesive_count = all_classes
        .iter()
        .filter(|c| c.verdict == CohesionVerdict::Cohesive)
        .count();

    let summary = CohesionSummary {
        total_classes,
        cohesive: cohesive_count,
        split_candidates: low_cohesion_count,
        avg_lcom4,
    };

    Ok(CohesionReport {
        classes_analyzed: total_classes,
        avg_lcom4,
        low_cohesion_count,
        classes: all_classes,
        summary,
    })
}

/// Analyze cohesion for all classes in a single file
fn analyze_file_cohesion(
    file_path: &Path,
    options: &CohesionOptions,
) -> TldrResult<Vec<ClassCohesion>> {
    let source = std::fs::read_to_string(file_path)?;
    let mut language = Language::from_path(file_path).ok_or_else(|| {
        TldrError::UnsupportedLanguage(
            file_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("unknown")
                .to_string(),
        )
    })?;
    // p19-secondary-fixes-v1 (BUG-P19-08): `Language::from_path` maps
    // `.h` → C. Headers in mixed C++ codebases (tinyxml2.h, Boost,
    // Folly, …) carry the C++ class declarations; cohesion run with
    // `language = C` then dispatches to a `_ => vec![]` arm and emits
    // `classes_analyzed = 0`. When the source contains a `class` /
    // `namespace` keyword, promote to C++ so the new cpp class
    // extractor runs and the count agrees with the
    // `structure --lang cpp` / `interface` surfaces.
    if matches!(language, Language::C)
        && file_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("h") || e.eq_ignore_ascii_case("hpp"))
            .unwrap_or(false)
        && (source.contains("\nclass ")
            || source.contains(" class ")
            || source.contains("namespace "))
    {
        language = Language::Cpp;
    }

    // Parse the file using the global parser pool
    let tree = parse(&source, language)?;
    let root = tree.root_node();

    // Extract classes based on language
    let class_infos = extract_classes(root, &source, language);

    // Compute LCOM4 for each class
    let mut results = Vec::new();
    for class_info in class_infos {
        let cohesion = compute_class_cohesion(&class_info, &source, file_path, options);
        results.push(cohesion);
    }

    Ok(results)
}

/// Extract classes from the AST based on language
fn extract_classes(root: tree_sitter::Node, source: &str, language: Language) -> Vec<ClassInfo> {
    match language {
        Language::Python => extract_python_classes(root, source),
        Language::TypeScript | Language::JavaScript => extract_typescript_classes(root, source),
        Language::Go => extract_go_structs(root, source),
        Language::Rust => extract_rust_structs(root, source),
        Language::Java => extract_java_classes(root, source),
        Language::Ruby => extract_ruby_classes(root, source),
        Language::CSharp => extract_csharp_classes(root, source),
        Language::Scala => extract_scala_classes(root, source),
        Language::Php => extract_php_classes(root, source),
        // p19-secondary-fixes-v1 (BUG-P19-08): cpp `health` previously
        // reported `classes_analyzed=0` while `structure` (after the
        // BUG-P19-05 fix) and `interface` report ~26 for the same
        // header. Add cpp class extraction so the three pipelines agree
        // on the class count surface.
        Language::Cpp => extract_cpp_classes_cohesion(root, source),
        _ => vec![], // Unsupported language
    }
}

/// Extract C++ classes for cohesion analysis (BUG-P19-08).
/// Mirrors the (class_specifier | struct_specifier) handling in
/// `ast::extractor::extract_cpp_classes` including the macro-prefixed
/// misparse recovery (e.g. `class TINYXML2_LIB XMLDocument`).
fn extract_cpp_classes_cohesion(root: tree_sitter::Node, source: &str) -> Vec<ClassInfo> {
    let mut classes = Vec::new();
    extract_cpp_classes_cohesion_recursive(root, source, &mut classes);
    classes
}

fn extract_cpp_classes_cohesion_recursive(
    node: tree_sitter::Node,
    source: &str,
    classes: &mut Vec<ClassInfo>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_specifier" | "struct_specifier" => {
                if let Some(info) = extract_cpp_class_info(&child, source) {
                    classes.push(info);
                }
                if let Some(body) = child.child_by_field_name("body") {
                    extract_cpp_classes_cohesion_recursive(body, source, classes);
                }
                continue;
            }
            "function_definition" | "declaration" => {
                if let Some(info) = extract_cpp_macro_prefixed_class(&child, source) {
                    classes.push(info);
                    continue;
                }
            }
            _ => {}
        }
        extract_cpp_classes_cohesion_recursive(child, source, classes);
    }
}

fn extract_cpp_class_info(node: &tree_sitter::Node, source: &str) -> Option<ClassInfo> {
    let mut name: Option<String> = None;
    if let Some(name_node) = node.child_by_field_name("name") {
        let n = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
        if !n.is_empty() {
            name = Some(n);
        }
    }
    if name.is_none() {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "type_identifier" {
                let n = child.utf8_text(source.as_bytes()).ok()?.to_string();
                if !n.is_empty() {
                    name = Some(n);
                    break;
                }
            }
        }
    }
    let name = name?;
    let line = node.start_position().row + 1;
    let body = node.child_by_field_name("body");
    let methods = body
        .map(|b| extract_cpp_methods(&b, source))
        .unwrap_or_default();
    Some(ClassInfo {
        name,
        line,
        methods,
    })
}

fn extract_cpp_macro_prefixed_class(node: &tree_sitter::Node, source: &str) -> Option<ClassInfo> {
    let type_node = node.child_by_field_name("type")?;
    if type_node.kind() != "class_specifier" && type_node.kind() != "struct_specifier" {
        return None;
    }
    let declarator = node.child_by_field_name("declarator")?;
    if declarator.kind() != "identifier" {
        return None;
    }
    let name = declarator.utf8_text(source.as_bytes()).ok()?.to_string();
    if name.is_empty() {
        return None;
    }
    let line = node.start_position().row + 1;
    // The body of a misparsed macro-class lives in a sibling
    // `compound_statement` rather than a tree-sitter `body` field; pick
    // the first such direct child if present.
    let mut body_methods = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "compound_statement" || child.kind() == "field_declaration_list" {
            body_methods = extract_cpp_methods(&child, source);
            break;
        }
    }
    Some(ClassInfo {
        name,
        line,
        methods: body_methods,
    })
}

fn extract_cpp_methods(body: &tree_sitter::Node, source: &str) -> Vec<MethodInfo> {
    let mut methods = Vec::new();
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        // Inline method definitions appear as `function_definition`
        // inside `field_declaration_list`.
        if child.kind() == "function_definition" {
            if let Some(declarator) = child.child_by_field_name("declarator") {
                if let Some(name) = extract_cpp_method_name(&declarator, source) {
                    methods.push(MethodInfo {
                        name,
                        start_byte: child.start_byte(),
                        end_byte: child.end_byte(),
                    });
                }
            }
        }
    }
    methods
}

fn extract_cpp_method_name(node: &tree_sitter::Node, source: &str) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" | "destructor_name" => {
            Some(node.utf8_text(source.as_bytes()).ok()?.to_string())
        }
        "function_declarator"
        | "pointer_declarator"
        | "reference_declarator"
        | "parenthesized_declarator" => {
            let inner = node.child_by_field_name("declarator")?;
            extract_cpp_method_name(&inner, source)
        }
        _ => None,
    }
}

// =============================================================================
// Python Class Extraction
// =============================================================================

/// Extract Python classes with their methods
fn extract_python_classes(root: tree_sitter::Node, source: &str) -> Vec<ClassInfo> {
    let mut classes = Vec::new();
    extract_python_classes_recursive(root, source, &mut classes);
    classes
}

fn extract_python_classes_recursive(
    node: tree_sitter::Node,
    source: &str,
    classes: &mut Vec<ClassInfo>,
) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_definition" => {
                if let Some(class_info) = extract_python_class_info(&child, source) {
                    classes.push(class_info);
                }
                // Recurse into class body for nested classes (T16 mitigation)
                if let Some(body) = child.child_by_field_name("body") {
                    extract_python_classes_recursive(body, source, classes);
                }
            }
            "decorated_definition" => {
                // Handle decorated classes
                if let Some(def) = child.child_by_field_name("definition") {
                    if def.kind() == "class_definition" {
                        if let Some(class_info) = extract_python_class_info(&def, source) {
                            classes.push(class_info);
                        }
                        // Recurse into class body for nested classes
                        if let Some(body) = def.child_by_field_name("body") {
                            extract_python_classes_recursive(body, source, classes);
                        }
                    }
                }
            }
            _ => {
                // Recurse into other nodes (module level)
                extract_python_classes_recursive(child, source, classes);
            }
        }
    }
}

fn extract_python_class_info(node: &tree_sitter::Node, source: &str) -> Option<ClassInfo> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
    let line = node.start_position().row + 1;

    let body = node.child_by_field_name("body")?;
    let methods = extract_python_methods(&body, source);

    Some(ClassInfo {
        name,
        line,
        methods,
    })
}

fn extract_python_methods(body: &tree_sitter::Node, source: &str) -> Vec<MethodInfo> {
    let mut methods = Vec::new();
    let mut cursor = body.walk();

    for child in body.children(&mut cursor) {
        match child.kind() {
            "function_definition" => {
                if let Some(method) = extract_python_method(&child, source) {
                    methods.push(method);
                }
            }
            "decorated_definition" => {
                if let Some(def) = child.child_by_field_name("definition") {
                    if def.kind() == "function_definition" {
                        if let Some(method) = extract_python_method(&def, source) {
                            methods.push(method);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    methods
}

fn extract_python_method(node: &tree_sitter::Node, source: &str) -> Option<MethodInfo> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();

    Some(MethodInfo {
        name,
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
    })
}

// =============================================================================
// TypeScript/JavaScript Class Extraction
// =============================================================================

/// Extract TypeScript/JavaScript classes with their methods
fn extract_typescript_classes(root: tree_sitter::Node, source: &str) -> Vec<ClassInfo> {
    let mut classes = Vec::new();
    extract_typescript_classes_recursive(root, source, &mut classes);
    classes
}

fn extract_typescript_classes_recursive(
    node: tree_sitter::Node,
    source: &str,
    classes: &mut Vec<ClassInfo>,
) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() == "class_declaration" || child.kind() == "class" {
            if let Some(class_info) = extract_typescript_class_info(&child, source) {
                classes.push(class_info);
            }
        }
        // Recurse into children
        extract_typescript_classes_recursive(child, source, classes);
    }
}

fn extract_typescript_class_info(node: &tree_sitter::Node, source: &str) -> Option<ClassInfo> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
    let line = node.start_position().row + 1;

    let body = node.child_by_field_name("body")?;
    let methods = extract_typescript_methods(&body, source);

    Some(ClassInfo {
        name,
        line,
        methods,
    })
}

fn extract_typescript_methods(body: &tree_sitter::Node, source: &str) -> Vec<MethodInfo> {
    let mut methods = Vec::new();
    let mut cursor = body.walk();

    for child in body.children(&mut cursor) {
        // TypeScript method_definition
        if child.kind() == "method_definition" || child.kind() == "public_field_definition" {
            if let Some(name_node) = child.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                    // Skip constructor for cohesion analysis (similar to __init__)
                    if name != "constructor" {
                        methods.push(MethodInfo {
                            name: name.to_string(),
                            start_byte: child.start_byte(),
                            end_byte: child.end_byte(),
                        });
                    }
                }
            }
        }
    }

    methods
}

// =============================================================================
// Java Class Extraction
// =============================================================================

/// Extract Java classes, interfaces, and enums with their methods
fn extract_java_classes(root: tree_sitter::Node, source: &str) -> Vec<ClassInfo> {
    let mut classes = Vec::new();
    extract_java_classes_recursive(root, source, &mut classes);
    classes
}

fn extract_java_classes_recursive(
    node: tree_sitter::Node,
    source: &str,
    classes: &mut Vec<ClassInfo>,
) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_declaration" | "interface_declaration" | "enum_declaration" => {
                if let Some(class_info) = extract_java_class_info(&child, source) {
                    classes.push(class_info);
                }
                // Recurse into class body for nested classes
                if let Some(body) = child.child_by_field_name("body") {
                    extract_java_classes_recursive(body, source, classes);
                }
            }
            _ => {
                // Recurse into other nodes (program level, etc.)
                extract_java_classes_recursive(child, source, classes);
            }
        }
    }
}

fn extract_java_class_info(node: &tree_sitter::Node, source: &str) -> Option<ClassInfo> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
    let line = node.start_position().row + 1;

    let body = node.child_by_field_name("body")?;
    let methods = extract_java_methods(&body, source);

    Some(ClassInfo {
        name,
        line,
        methods,
    })
}

fn extract_java_methods(body: &tree_sitter::Node, source: &str) -> Vec<MethodInfo> {
    let mut methods = Vec::new();
    let mut cursor = body.walk();

    for child in body.children(&mut cursor) {
        // method_declaration is a regular method; constructor_declaration is excluded
        // (similar to how TypeScript excludes "constructor")
        if child.kind() == "method_declaration" {
            if let Some(name_node) = child.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                    methods.push(MethodInfo {
                        name: name.to_string(),
                        start_byte: child.start_byte(),
                        end_byte: child.end_byte(),
                    });
                }
            }
        }
    }

    methods
}

// =============================================================================
// Go Struct Extraction
// =============================================================================

/// Extract Go structs with their receiver methods
fn extract_go_structs(root: tree_sitter::Node, source: &str) -> Vec<ClassInfo> {
    let mut structs: HashMap<String, ClassInfo> = HashMap::new();

    // First pass: collect all struct declarations
    collect_go_structs(root, source, &mut structs);

    // Second pass: collect receiver methods and associate with structs
    collect_go_methods(root, source, &mut structs);

    structs.into_values().collect()
}

fn collect_go_structs(
    node: tree_sitter::Node,
    source: &str,
    structs: &mut HashMap<String, ClassInfo>,
) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() == "type_declaration" {
            // Look for struct type specs
            let mut type_cursor = child.walk();
            for type_child in child.children(&mut type_cursor) {
                if type_child.kind() == "type_spec" {
                    if let Some(name_node) = type_child.child_by_field_name("name") {
                        if let Some(type_node) = type_child.child_by_field_name("type") {
                            if type_node.kind() == "struct_type" {
                                if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                                    let line = type_child.start_position().row + 1;
                                    structs.insert(
                                        name.to_string(),
                                        ClassInfo {
                                            name: name.to_string(),
                                            line,
                                            methods: Vec::new(),
                                        },
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        // Recurse
        collect_go_structs(child, source, structs);
    }
}

fn collect_go_methods(
    node: tree_sitter::Node,
    source: &str,
    structs: &mut HashMap<String, ClassInfo>,
) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() == "method_declaration" {
            // Extract receiver type
            if let Some(receiver) = child.child_by_field_name("receiver") {
                if let Some(struct_name) = extract_go_receiver_type(&receiver, source) {
                    // Extract method name
                    if let Some(name_node) = child.child_by_field_name("name") {
                        if let Ok(method_name) = name_node.utf8_text(source.as_bytes()) {
                            if let Some(class_info) = structs.get_mut(&struct_name) {
                                class_info.methods.push(MethodInfo {
                                    name: method_name.to_string(),
                                    start_byte: child.start_byte(),
                                    end_byte: child.end_byte(),
                                });
                            }
                        }
                    }
                }
            }
        }
        // Recurse
        collect_go_methods(child, source, structs);
    }
}

fn extract_go_receiver_type(receiver: &tree_sitter::Node, source: &str) -> Option<String> {
    // receiver is parameter_list, find the type inside
    let mut cursor = receiver.walk();
    for child in receiver.children(&mut cursor) {
        if child.kind() == "parameter_declaration" {
            if let Some(type_node) = child.child_by_field_name("type") {
                // Handle pointer receiver (*Type)
                if type_node.kind() == "pointer_type" {
                    if let Some(elem) = type_node.named_child(0) {
                        return elem
                            .utf8_text(source.as_bytes())
                            .ok()
                            .map(|s| s.to_string());
                    }
                } else {
                    return type_node
                        .utf8_text(source.as_bytes())
                        .ok()
                        .map(|s| s.to_string());
                }
            }
        }
    }
    None
}

// =============================================================================
// Rust Struct Extraction
// =============================================================================

/// Extract Rust structs with their impl block methods
fn extract_rust_structs(root: tree_sitter::Node, source: &str) -> Vec<ClassInfo> {
    let mut structs: HashMap<String, ClassInfo> = HashMap::new();

    // First pass: collect all struct declarations
    collect_rust_structs(root, source, &mut structs);

    // Second pass: collect impl block methods and associate with structs
    collect_rust_impl_methods(root, source, &mut structs);

    structs.into_values().collect()
}

fn collect_rust_structs(
    node: tree_sitter::Node,
    source: &str,
    structs: &mut HashMap<String, ClassInfo>,
) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() == "struct_item" {
            if let Some(name_node) = child.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                    let line = child.start_position().row + 1;
                    structs.insert(
                        name.to_string(),
                        ClassInfo {
                            name: name.to_string(),
                            line,
                            methods: Vec::new(),
                        },
                    );
                }
            }
        }
        // Recurse
        collect_rust_structs(child, source, structs);
    }
}

fn collect_rust_impl_methods(
    node: tree_sitter::Node,
    source: &str,
    structs: &mut HashMap<String, ClassInfo>,
) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() == "impl_item" {
            // Get the type being implemented
            if let Some(type_node) = child.child_by_field_name("type") {
                if let Ok(type_name) = type_node.utf8_text(source.as_bytes()) {
                    let type_name = type_name.to_string();

                    // Get the body of the impl block
                    if let Some(body) = child.child_by_field_name("body") {
                        let mut body_cursor = body.walk();
                        for body_child in body.children(&mut body_cursor) {
                            if body_child.kind() == "function_item" {
                                // Skip associated functions (no self parameter).
                                // Only include instance methods (&self, &mut self, self)
                                // for LCOM4 analysis, since associated functions like
                                // new() and default() don't access self.field and would
                                // inflate LCOM4 by forming disconnected components.
                                if !rust_function_has_self(&body_child) {
                                    continue;
                                }
                                if let Some(name_node) = body_child.child_by_field_name("name") {
                                    if let Ok(method_name) = name_node.utf8_text(source.as_bytes())
                                    {
                                        if let Some(class_info) = structs.get_mut(&type_name) {
                                            class_info.methods.push(MethodInfo {
                                                name: method_name.to_string(),
                                                start_byte: body_child.start_byte(),
                                                end_byte: body_child.end_byte(),
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        // Recurse
        collect_rust_impl_methods(child, source, structs);
    }
}

/// Check if a Rust function_item has a self parameter (&self, &mut self, or self).
///
/// In tree-sitter-rust, instance methods have a `self_parameter` node inside
/// the `parameters` field. Associated functions (like `fn new() -> Self`)
/// have no `self_parameter`.
fn rust_function_has_self(function_node: &tree_sitter::Node) -> bool {
    if let Some(params) = function_node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for param_child in params.children(&mut cursor) {
            if param_child.kind() == "self_parameter" {
                return true;
            }
        }
    }
    false
}

// =============================================================================
// Ruby Class Extraction
// =============================================================================

/// Extract Ruby classes with their methods
fn extract_ruby_classes(root: tree_sitter::Node, source: &str) -> Vec<ClassInfo> {
    let mut classes = Vec::new();
    extract_ruby_classes_recursive(root, source, &mut classes);
    classes
}

fn extract_ruby_classes_recursive(
    node: tree_sitter::Node,
    source: &str,
    classes: &mut Vec<ClassInfo>,
) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "class" => {
                if let Some(class_info) = extract_ruby_class_info(&child, source) {
                    classes.push(class_info);
                }
                // Recurse into class body for nested classes
                if let Some(body) = child.child_by_field_name("body") {
                    extract_ruby_classes_recursive(body, source, classes);
                }
            }
            _ => {
                extract_ruby_classes_recursive(child, source, classes);
            }
        }
    }
}

fn extract_ruby_class_info(node: &tree_sitter::Node, source: &str) -> Option<ClassInfo> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
    let line = node.start_position().row + 1;

    let body = node.child_by_field_name("body")?;
    let methods = extract_ruby_methods(&body, source);

    Some(ClassInfo {
        name,
        line,
        methods,
    })
}

/// Extract methods from a Ruby class body (body_statement node).
///
/// Ruby methods are `method` nodes (instance methods) and `singleton_method`
/// nodes (class methods like `self.foo`). For LCOM4 we include both since
/// singleton methods can also access class-level instance variables.
fn extract_ruby_methods(body: &tree_sitter::Node, source: &str) -> Vec<MethodInfo> {
    let mut methods = Vec::new();
    let mut cursor = body.walk();

    for child in body.children(&mut cursor) {
        if child.kind() == "method" || child.kind() == "singleton_method" {
            if let Some(name_node) = child.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                    methods.push(MethodInfo {
                        name: name.to_string(),
                        start_byte: child.start_byte(),
                        end_byte: child.end_byte(),
                    });
                }
            }
        }
    }

    methods
}

// =============================================================================
// C# Class Extraction
// =============================================================================

/// Extract C# classes, structs, and interfaces with their methods
fn extract_csharp_classes(root: tree_sitter::Node, source: &str) -> Vec<ClassInfo> {
    let mut classes = Vec::new();
    extract_csharp_classes_recursive(root, source, &mut classes);
    classes
}

fn extract_csharp_classes_recursive(
    node: tree_sitter::Node,
    source: &str,
    classes: &mut Vec<ClassInfo>,
) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_declaration" | "struct_declaration" | "interface_declaration" => {
                if let Some(class_info) = extract_csharp_class_info(&child, source) {
                    classes.push(class_info);
                }
                // Recurse into class body for nested classes
                if let Some(body) = child.child_by_field_name("body") {
                    extract_csharp_classes_recursive(body, source, classes);
                }
            }
            _ => {
                extract_csharp_classes_recursive(child, source, classes);
            }
        }
    }
}

fn extract_csharp_class_info(node: &tree_sitter::Node, source: &str) -> Option<ClassInfo> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
    let line = node.start_position().row + 1;

    let body = node.child_by_field_name("body")?;
    let methods = extract_csharp_methods(&body, source);

    Some(ClassInfo {
        name,
        line,
        methods,
    })
}

/// Extract methods from a C# class body (declaration_list node).
///
/// Only includes `method_declaration` nodes. Constructors
/// (`constructor_declaration`) are excluded from LCOM4 analysis,
/// consistent with how Java excludes constructors.
fn extract_csharp_methods(body: &tree_sitter::Node, source: &str) -> Vec<MethodInfo> {
    let mut methods = Vec::new();
    let mut cursor = body.walk();

    for child in body.children(&mut cursor) {
        if child.kind() == "method_declaration" {
            if let Some(name_node) = child.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                    methods.push(MethodInfo {
                        name: name.to_string(),
                        start_byte: child.start_byte(),
                        end_byte: child.end_byte(),
                    });
                }
            }
        }
    }

    methods
}

// =============================================================================
// Scala Class Extraction
// =============================================================================

/// Extract Scala classes, objects, and traits with their methods
fn extract_scala_classes(root: tree_sitter::Node, source: &str) -> Vec<ClassInfo> {
    let mut classes = Vec::new();
    extract_scala_classes_recursive(root, source, &mut classes);
    classes
}

fn extract_scala_classes_recursive(
    node: tree_sitter::Node,
    source: &str,
    classes: &mut Vec<ClassInfo>,
) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_definition" | "object_definition" | "trait_definition" => {
                if let Some(class_info) = extract_scala_class_info(&child, source) {
                    classes.push(class_info);
                }
                // Recurse into body for nested classes
                let mut inner_cursor = child.walk();
                for inner_child in child.children(&mut inner_cursor) {
                    if inner_child.kind() == "template_body" || inner_child.kind() == "body" {
                        extract_scala_classes_recursive(inner_child, source, classes);
                    }
                }
            }
            _ => {
                extract_scala_classes_recursive(child, source, classes);
            }
        }
    }
}

fn extract_scala_class_info(node: &tree_sitter::Node, source: &str) -> Option<ClassInfo> {
    // Scala tree-sitter may use "name" field or have identifier as a direct child
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source.as_bytes()).ok().map(|s| s.to_string()))
        .or_else(|| {
            // Fallback: find first identifier child
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "identifier" {
                    return child
                        .utf8_text(source.as_bytes())
                        .ok()
                        .map(|s| s.to_string());
                }
            }
            None
        })?;

    let line = node.start_position().row + 1;
    let methods = extract_scala_methods(node, source);

    Some(ClassInfo {
        name,
        line,
        methods,
    })
}

/// Extract methods from a Scala class/object/trait.
///
/// Scala methods (`function_definition` / `function_declaration`) live inside
/// a `template_body` or `body` child of the class node.
fn extract_scala_methods(node: &tree_sitter::Node, source: &str) -> Vec<MethodInfo> {
    let mut methods = Vec::new();
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() == "template_body" || child.kind() == "body" {
            let mut body_cursor = child.walk();
            for body_child in child.children(&mut body_cursor) {
                if body_child.kind() == "function_definition"
                    || body_child.kind() == "function_declaration"
                {
                    if let Some(name_node) = body_child.child_by_field_name("name") {
                        if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                            methods.push(MethodInfo {
                                name: name.to_string(),
                                start_byte: body_child.start_byte(),
                                end_byte: body_child.end_byte(),
                            });
                        }
                    }
                }
            }
        }
    }

    methods
}

// =============================================================================
// PHP Class Extraction
// =============================================================================

/// Extract PHP classes, interfaces, and traits with their methods
fn extract_php_classes(root: tree_sitter::Node, source: &str) -> Vec<ClassInfo> {
    let mut classes = Vec::new();
    extract_php_classes_recursive(root, source, &mut classes);
    classes
}

fn extract_php_classes_recursive(
    node: tree_sitter::Node,
    source: &str,
    classes: &mut Vec<ClassInfo>,
) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_declaration" | "interface_declaration" | "trait_declaration" => {
                if let Some(class_info) = extract_php_class_info(&child, source) {
                    classes.push(class_info);
                }
                // Recurse into class body for nested classes
                if let Some(body) = child.child_by_field_name("body") {
                    extract_php_classes_recursive(body, source, classes);
                }
            }
            _ => {
                extract_php_classes_recursive(child, source, classes);
            }
        }
    }
}

fn extract_php_class_info(node: &tree_sitter::Node, source: &str) -> Option<ClassInfo> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
    let line = node.start_position().row + 1;

    let body = node.child_by_field_name("body")?;
    let methods = extract_php_methods(&body, source);

    Some(ClassInfo {
        name,
        line,
        methods,
    })
}

/// Extract methods from a PHP class body (declaration_list node).
///
/// Only includes `method_declaration` nodes. Constructors (`__construct`)
/// are included as regular methods since PHP doesn't use a separate AST
/// node type for constructors.
fn extract_php_methods(body: &tree_sitter::Node, source: &str) -> Vec<MethodInfo> {
    let mut methods = Vec::new();
    let mut cursor = body.walk();

    for child in body.children(&mut cursor) {
        if child.kind() == "method_declaration" {
            if let Some(name_node) = child.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                    methods.push(MethodInfo {
                        name: name.to_string(),
                        start_byte: child.start_byte(),
                        end_byte: child.end_byte(),
                    });
                }
            }
        }
    }

    methods
}

// =============================================================================
// LCOM4 Computation
// =============================================================================

/// Check if a method name is a dunder method (__name__)
fn is_dunder_method(name: &str) -> bool {
    name.starts_with("__") && name.ends_with("__")
}

/// Extract self.field accesses from a method's source text (Python)
pub(crate) fn extract_self_accesses(method_source: &str) -> HashSet<String> {
    let mut fields = HashSet::new();

    // Regex to match self.field_name patterns
    // Handles: self.field, self.field_name, self._private_field
    let pattern = Regex::new(r"self\.([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();

    for cap in pattern.captures_iter(method_source) {
        if let Some(field) = cap.get(1) {
            fields.insert(field.as_str().to_string());
        }
    }

    fields
}

/// Extract this.field accesses from a method's source text (TypeScript/JavaScript)
fn extract_this_accesses(method_source: &str) -> HashSet<String> {
    let mut fields = HashSet::new();

    // Regex to match this.field_name patterns
    let pattern = Regex::new(r"this\.([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();

    for cap in pattern.captures_iter(method_source) {
        if let Some(field) = cap.get(1) {
            fields.insert(field.as_str().to_string());
        }
    }

    fields
}

/// Extract field accesses from Go method (receiver.field)
fn extract_go_receiver_accesses(method_source: &str, receiver_name: &str) -> HashSet<String> {
    let mut fields = HashSet::new();

    // Match receiver.field patterns
    let pattern_str = format!(
        r"{}\.([a-zA-Z_][a-zA-Z0-9_]*)",
        regex::escape(receiver_name)
    );
    if let Ok(pattern) = Regex::new(&pattern_str) {
        for cap in pattern.captures_iter(method_source) {
            if let Some(field) = cap.get(1) {
                fields.insert(field.as_str().to_string());
            }
        }
    }

    // Also match common Go receiver patterns like s.field, t.field
    let short_pattern = Regex::new(r"\b([a-z])\.([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();
    for cap in short_pattern.captures_iter(method_source) {
        if let Some(field) = cap.get(2) {
            fields.insert(field.as_str().to_string());
        }
    }

    fields
}

/// Extract field accesses from Rust method (self.field)
fn extract_rust_self_accesses(method_source: &str) -> HashSet<String> {
    let mut fields = HashSet::new();

    // Regex to match self.field_name patterns
    let pattern = Regex::new(r"self\.([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();

    for cap in pattern.captures_iter(method_source) {
        if let Some(field) = cap.get(1) {
            fields.insert(field.as_str().to_string());
        }
    }

    fields
}

/// Extract field accesses from Ruby method (@field instance variables)
fn extract_ruby_instance_var_accesses(method_source: &str) -> HashSet<String> {
    let mut fields = HashSet::new();

    // Match all @-prefixed identifiers (including @@class_vars), then filter.
    // The regex crate does not support lookbehinds, so we capture an optional
    // second '@' and skip matches where it is present (@@class_var).
    let pattern = Regex::new(r"(@?)@([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();

    for cap in pattern.captures_iter(method_source) {
        // If group 1 captured an '@', this is a @@class_var -- skip it.
        if cap.get(1).is_some_and(|m| !m.as_str().is_empty()) {
            continue;
        }
        if let Some(field) = cap.get(2) {
            fields.insert(field.as_str().to_string());
        }
    }

    fields
}

/// Extract field accesses from PHP method ($this->field)
fn extract_php_this_accesses(method_source: &str) -> HashSet<String> {
    let mut fields = HashSet::new();

    // Regex to match $this->field_name patterns
    let pattern = Regex::new(r"\$this->([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();

    for cap in pattern.captures_iter(method_source) {
        if let Some(field) = cap.get(1) {
            fields.insert(field.as_str().to_string());
        }
    }

    fields
}

/// Compute cohesion for a single class
fn compute_class_cohesion(
    class_info: &ClassInfo,
    source: &str,
    file_path: &Path,
    options: &CohesionOptions,
) -> ClassCohesion {
    // Filter out dunder methods if not including them
    let methods: Vec<&MethodInfo> = class_info
        .methods
        .iter()
        .filter(|m| options.include_dunder || !is_dunder_method(&m.name))
        .collect();

    let method_count = methods.len();

    // Special cases (T9 mitigation):
    // - 0 methods: LCOM4 = 0 (degenerate case, can't measure)
    // - 1 method: LCOM4 = 1 (single method is trivially cohesive)
    if method_count == 0 {
        return ClassCohesion {
            name: class_info.name.clone(),
            file: file_path.to_path_buf(),
            line: class_info.line,
            method_count: 0,
            field_count: 0,
            lcom4: 0,
            components: vec![],
            verdict: CohesionVerdict::Cohesive,
            split_suggestion: None,
        };
    }

    if method_count == 1 {
        let method = methods[0];
        let method_source = &source[method.start_byte..method.end_byte];
        let fields = extract_field_accesses(method_source, file_path);
        let field_vec: Vec<String> = fields.into_iter().collect();

        return ClassCohesion {
            name: class_info.name.clone(),
            file: file_path.to_path_buf(),
            line: class_info.line,
            method_count: 1,
            field_count: field_vec.len(),
            lcom4: 1,
            components: vec![ComponentInfo {
                methods: vec![method.name.clone()],
                fields: field_vec,
            }],
            verdict: CohesionVerdict::Cohesive,
            split_suggestion: None,
        };
    }

    // Extract field accesses for each method
    let method_fields: Vec<HashSet<String>> = methods
        .iter()
        .map(|m| {
            let method_source = &source[m.start_byte..m.end_byte];
            extract_field_accesses(method_source, file_path)
        })
        .collect();

    // Collect all unique fields
    let all_fields: HashSet<String> = method_fields.iter().flatten().cloned().collect();
    let field_count = all_fields.len();

    // If no methods access any fields, each method is its own component
    if all_fields.is_empty() {
        let lcom4 = method_count;
        let components: Vec<ComponentInfo> = methods
            .iter()
            .map(|m| ComponentInfo {
                methods: vec![m.name.clone()],
                fields: vec![],
            })
            .collect();

        let verdict = if lcom4 > options.low_cohesion_threshold {
            CohesionVerdict::SplitCandidate
        } else {
            CohesionVerdict::Cohesive
        };

        let split_suggestion = if verdict == CohesionVerdict::SplitCandidate {
            Some(format!(
                "Class has {} disconnected methods with no shared state",
                method_count
            ))
        } else {
            None
        };

        return ClassCohesion {
            name: class_info.name.clone(),
            file: file_path.to_path_buf(),
            line: class_info.line,
            method_count,
            field_count: 0,
            lcom4,
            components,
            verdict,
            split_suggestion,
        };
    }

    // Build Union-Find and connect methods that share fields
    let mut uf = UnionFind::new(method_count);

    for i in 0..method_count {
        for j in (i + 1)..method_count {
            // Check if methods i and j share any fields
            if !method_fields[i].is_disjoint(&method_fields[j]) {
                uf.union(i, j);
            }
        }
    }

    // Count connected components
    let lcom4 = uf.count_components();

    // Build component info
    let component_ids = uf.get_components();
    let mut component_map: HashMap<usize, (Vec<String>, HashSet<String>)> = HashMap::new();

    for (i, &comp_id) in component_ids.iter().enumerate() {
        let entry = component_map
            .entry(comp_id)
            .or_insert_with(|| (Vec::new(), HashSet::new()));
        entry.0.push(methods[i].name.clone());
        entry.1.extend(method_fields[i].iter().cloned());
    }

    let components: Vec<ComponentInfo> = component_map
        .into_values()
        .map(|(methods, fields)| ComponentInfo {
            methods,
            fields: fields.into_iter().collect(),
        })
        .collect();

    let verdict = if lcom4 > options.low_cohesion_threshold {
        CohesionVerdict::SplitCandidate
    } else {
        CohesionVerdict::Cohesive
    };

    let split_suggestion = if verdict == CohesionVerdict::SplitCandidate {
        Some(format!(
            "Consider splitting into {} classes based on {} disconnected method groups",
            lcom4, lcom4
        ))
    } else {
        None
    };

    ClassCohesion {
        name: class_info.name.clone(),
        file: file_path.to_path_buf(),
        line: class_info.line,
        method_count,
        field_count,
        lcom4,
        components,
        verdict,
        split_suggestion,
    }
}

/// Extract field accesses based on file extension/language.
///
/// Uses AST-based extraction when possible, falling back to regex for
/// languages where tree-sitter parsing fails or returns no results.
fn extract_field_accesses(method_source: &str, file_path: &Path) -> HashSet<String> {
    let lang = Language::from_path(file_path);

    match lang {
        Some(language) => extract_field_accesses_ast(method_source, language, None),
        None => {
            // Unknown language: try regex fallback based on extension
            let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            match ext {
                "py" => extract_self_accesses(method_source),
                "ts" | "tsx" | "js" | "jsx" => extract_this_accesses(method_source),
                "go" => extract_go_receiver_accesses(method_source, ""),
                "rs" => extract_rust_self_accesses(method_source),
                "rb" => extract_ruby_instance_var_accesses(method_source),
                "cs" => extract_this_accesses(method_source),
                "scala" | "sc" => extract_this_accesses(method_source),
                "php" => extract_php_this_accesses(method_source),
                _ => HashSet::new(),
            }
        }
    }
}

/// AST-based field access extraction for all 18 supported languages.
///
/// Parses the method source text with tree-sitter and walks the AST looking
/// for field/member access nodes where the object is self/this/receiver.
///
/// Falls back to regex if AST parsing fails.
///
/// # Arguments
/// * `method_source` - Source code of the method body
/// * `language` - The programming language
/// * `receiver_name` - Optional receiver name for Go (e.g., "s" in `func (s *Server)`)
pub fn extract_field_accesses_ast(
    method_source: &str,
    language: Language,
    receiver_name: Option<&str>,
) -> HashSet<String> {
    use crate::security::ast_utils::field_access_info;

    let tree = match parse(method_source, language) {
        Ok(t) => t,
        Err(_) => {
            // Fallback to regex if AST parsing fails
            return extract_field_accesses_regex(method_source, language, receiver_name);
        }
    };

    let mut fields = HashSet::new();
    let source = method_source.as_bytes();
    let patterns = field_access_info(language);

    walk_and_extract_fields(
        &tree.root_node(),
        source,
        language,
        receiver_name,
        patterns,
        &mut fields,
    );

    // If AST found nothing but regex would have found something, fallback
    if fields.is_empty() {
        let regex_fields = extract_field_accesses_regex(method_source, language, receiver_name);
        if !regex_fields.is_empty() {
            return regex_fields;
        }
    }

    fields
}

/// Walk AST nodes recursively and extract field names from field access expressions.
fn walk_and_extract_fields(
    node: &tree_sitter::Node,
    source: &[u8],
    language: Language,
    receiver_name: Option<&str>,
    patterns: &[crate::security::ast_utils::FieldAccessPattern],
    fields: &mut HashSet<String>,
) {
    use crate::security::ast_utils::{is_in_comment, is_in_string};

    let node_kind = node.kind();

    for pattern in patterns {
        if node_kind == pattern.node_kind {
            // Skip if inside a comment or string
            if is_in_comment(node, language) || is_in_string(node, language) {
                continue;
            }

            if let Some(field_name) =
                extract_field_from_pattern(node, source, language, receiver_name, pattern)
            {
                fields.insert(field_name);
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_and_extract_fields(&child, source, language, receiver_name, patterns, fields);
    }
}

/// Extract a field name from a node matching a FieldAccessPattern.
///
/// Returns Some(field_name) if the node is a self/this/receiver field access,
/// None otherwise.
fn extract_field_from_pattern(
    node: &tree_sitter::Node,
    source: &[u8],
    language: Language,
    receiver_name: Option<&str>,
    _pattern: &crate::security::ast_utils::FieldAccessPattern,
) -> Option<String> {
    match language {
        Language::Python => extract_field_with_named_receiver(
            node,
            source,
            "object",
            "attribute",
            "self",
            "call",
            "function",
        ),
        Language::TypeScript | Language::JavaScript => extract_field_with_named_receiver(
            node,
            source,
            "object",
            "property",
            "this",
            "call_expression",
            "function",
        ),
        Language::Go => extract_go_field_access(node, source, receiver_name),
        Language::Rust => extract_field_with_named_receiver(
            node,
            source,
            "value",
            "field",
            "self",
            "call_expression",
            "function",
        ),
        Language::Java => extract_field_with_named_receiver(
            node,
            source,
            "object",
            "field",
            "this",
            "method_invocation",
            "object",
        ),
        Language::CSharp => extract_field_with_positional_receiver(
            node,
            source,
            0,
            "name",
            "this",
            "invocation_expression",
            0,
        ),
        Language::Cpp => extract_field_with_named_receiver(
            node,
            source,
            "argument",
            "field",
            "this",
            "call_expression",
            "function",
        ),
        Language::C => extract_c_field_access(node, source),
        Language::Ruby => extract_ruby_instance_field(node, source),
        Language::Kotlin => extract_navigation_field_access(
            node,
            source,
            "this_expression",
            "this",
            "call_expression",
        ),
        Language::Swift => extract_navigation_field_access(
            node,
            source,
            "self_expression",
            "self",
            "call_expression",
        ),
        Language::Scala => extract_scala_this_field_access(node, source),
        Language::Php => extract_php_this_field_access(node, source),
        Language::Lua | Language::Luau => extract_lua_self_field_access(node, source),
        Language::Elixir => extract_elixir_module_attribute(node, source),
        Language::Ocaml => None,
    }
}

fn extract_field_with_named_receiver(
    node: &tree_sitter::Node,
    source: &[u8],
    receiver_field: &str,
    field_name: &str,
    expected_receiver: &str,
    call_parent_kind: &str,
    call_target_field: &str,
) -> Option<String> {
    use crate::security::ast_utils::node_text;

    let receiver = node.child_by_field_name(receiver_field)?;
    if node_text(&receiver, source) != expected_receiver {
        return None;
    }
    if parent_field_matches_node(node, call_parent_kind, call_target_field) {
        return None;
    }
    Some(node_text(&node.child_by_field_name(field_name)?, source).to_string())
}

fn extract_field_with_positional_receiver(
    node: &tree_sitter::Node,
    source: &[u8],
    receiver_index: usize,
    field_name: &str,
    expected_receiver: &str,
    call_parent_kind: &str,
    call_target_index: usize,
) -> Option<String> {
    use crate::security::ast_utils::node_text;

    let receiver = node.child(receiver_index)?;
    if node_text(&receiver, source) != expected_receiver {
        return None;
    }
    if parent_child_matches_node(node, call_parent_kind, call_target_index) {
        return None;
    }
    Some(node_text(&node.child_by_field_name(field_name)?, source).to_string())
}

fn extract_go_field_access(
    node: &tree_sitter::Node,
    source: &[u8],
    receiver_name: Option<&str>,
) -> Option<String> {
    use crate::security::ast_utils::node_text;

    let operand = node.child_by_field_name("operand")?;
    let operand_text = node_text(&operand, source);
    if !is_go_receiver_match(operand_text, receiver_name) {
        return None;
    }
    if parent_field_matches_node(node, "call_expression", "function") {
        return None;
    }
    Some(node_text(&node.child_by_field_name("field")?, source).to_string())
}

fn is_go_receiver_match(operand_text: &str, receiver_name: Option<&str>) -> bool {
    match receiver_name {
        Some("") | None => is_single_lowercase_identifier(operand_text),
        Some(recv) => operand_text == recv,
    }
}

fn is_single_lowercase_identifier(text: &str) -> bool {
    text.len() == 1 && text.chars().next().is_some_and(|c| c.is_ascii_lowercase())
}

fn extract_c_field_access(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    use crate::security::ast_utils::node_text;

    node.child_by_field_name("argument")?;
    Some(node_text(&node.child_by_field_name("field")?, source).to_string())
}

fn extract_ruby_instance_field(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    use crate::security::ast_utils::node_text;

    let text = node_text(node, source);
    if text.starts_with('@') && !text.starts_with("@@") {
        return Some(text[1..].to_string());
    }
    None
}

fn extract_navigation_field_access(
    node: &tree_sitter::Node,
    source: &[u8],
    self_kind: &str,
    self_text: &str,
    call_parent_kind: &str,
) -> Option<String> {
    use crate::security::ast_utils::node_text;

    let target = node.child(0)?;
    if target.kind() != self_kind && node_text(&target, source) != self_text {
        return None;
    }

    for i in 1..node.child_count() {
        let child = node.child(i)?;
        if child.kind() == "identifier" || child.kind() == "simple_identifier" {
            if parent_child_matches_node(node, call_parent_kind, 0) {
                return None;
            }
            return Some(node_text(&child, source).to_string());
        }
        if child.kind() == "navigation_suffix" {
            if let Some(identifier) = extract_suffix_identifier(&child, source) {
                if parent_child_matches_node(node, call_parent_kind, 0) {
                    return None;
                }
                return Some(identifier);
            }
        }
    }

    None
}

fn extract_suffix_identifier(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    use crate::security::ast_utils::node_text;

    for i in 0..node.child_count() {
        let child = node.child(i)?;
        if child.kind() == "simple_identifier" || child.kind() == "identifier" {
            return Some(node_text(&child, source).to_string());
        }
    }
    None
}

fn extract_scala_this_field_access(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    use crate::security::ast_utils::node_text;

    let mut identifiers = Vec::new();
    for i in 0..node.child_count() {
        let child = node.child(i)?;
        match child.kind() {
            "identifier" | "type_identifier" => {
                identifiers.push(node_text(&child, source).to_string());
            }
            "this" => identifiers.push("this".to_string()),
            _ => {}
        }
    }
    if identifiers.len() >= 2 && identifiers[0] == "this" {
        return Some(identifiers[1].clone());
    }
    None
}

fn extract_php_this_field_access(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    use crate::security::ast_utils::node_text;

    let object = node.child_by_field_name("object")?;
    if node_text(&object, source) != "$this" {
        return None;
    }
    Some(node_text(&node.child_by_field_name("name")?, source).to_string())
}

fn extract_lua_self_field_access(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    use crate::security::ast_utils::node_text;

    let first = node.child(0)?;
    if node_text(&first, source) != "self" {
        return None;
    }

    for i in (0..node.child_count()).rev() {
        let child = node.child(i)?;
        if child.kind() == "identifier" {
            if parent_child_matches_node(node, "function_call", 0) {
                return None;
            }
            return Some(node_text(&child, source).to_string());
        }
    }
    None
}

fn extract_elixir_module_attribute(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    use crate::security::ast_utils::node_text;

    let operator = node.child(0)?;
    if node_text(&operator, source) != "@" {
        return None;
    }
    let name_node = node.child(1)?;
    if name_node.kind() == "call" {
        return Some(node_text(&name_node.child(0)?, source).to_string());
    }
    Some(node_text(&name_node, source).to_string())
}

fn parent_field_matches_node(
    node: &tree_sitter::Node,
    parent_kind: &str,
    field_name: &str,
) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() != parent_kind {
        return false;
    }
    parent
        .child_by_field_name(field_name)
        .is_some_and(|child| child.id() == node.id())
}

fn parent_child_matches_node(
    node: &tree_sitter::Node,
    parent_kind: &str,
    child_index: usize,
) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() != parent_kind {
        return false;
    }
    parent
        .child(child_index)
        .is_some_and(|child| child.id() == node.id())
}

/// Regex-based field access extraction (fallback when AST parsing fails).
fn extract_field_accesses_regex(
    method_source: &str,
    language: Language,
    receiver_name: Option<&str>,
) -> HashSet<String> {
    match language {
        Language::Python => extract_self_accesses(method_source),
        Language::TypeScript | Language::JavaScript => extract_this_accesses(method_source),
        Language::Go => {
            let recv = receiver_name.unwrap_or("");
            extract_go_receiver_accesses(method_source, recv)
        }
        Language::Rust => extract_rust_self_accesses(method_source),
        Language::Ruby => extract_ruby_instance_var_accesses(method_source),
        Language::CSharp => extract_this_accesses(method_source),
        Language::Scala => extract_this_accesses(method_source),
        Language::Php => extract_php_this_accesses(method_source),
        _ => HashSet::new(),
    }
}

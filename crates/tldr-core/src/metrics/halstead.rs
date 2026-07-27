//! Halstead software science metrics
//!
//! This module provides standalone Halstead metrics analysis per function.
//!
//! # Halstead Metrics
//!
//! Based on Maurice Halstead's software science metrics:
//! - n1 = number of distinct operators
//! - n2 = number of distinct operands
//! - N1 = total number of operators
//! - N2 = total number of operands
//!
//! ## Derived Metrics
//! - vocabulary = n1 + n2
//! - length = N1 + N2
//! - volume = length * log2(vocabulary)
//! - difficulty = (n1/2) * (N2/n2)
//! - effort = difficulty * volume
//! - time = effort / 18 (seconds)
//! - bugs = volume / 3000
//!
//! # References
//! - Halstead, M.H. (1977). "Elements of Software Science"

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tree_sitter::Node;

use crate::ast::extract::extract_file;
use crate::ast::function_finder::find_function_node;
use crate::ast::parser::{parse, parse_file};
use crate::metrics::types::HalsteadInfo;
use crate::types::Language;
use crate::TldrResult;

// =============================================================================
// Types
// =============================================================================

/// Threshold status for Halstead metrics
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThresholdStatus {
    /// Metric is below warning thresholds.
    Good,
    /// Metric exceeds warning threshold but not critical.
    Warning,
    /// Metric exceeds the highest configured threshold.
    Bad,
}

/// Threshold violations for a function
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HalsteadThresholds {
    /// Classification for Halstead volume.
    pub volume_status: ThresholdStatus,
    /// Classification for Halstead difficulty.
    pub difficulty_status: ThresholdStatus,
}

impl Default for HalsteadThresholds {
    fn default() -> Self {
        Self {
            volume_status: ThresholdStatus::Good,
            difficulty_status: ThresholdStatus::Good,
        }
    }
}

/// Halstead metrics result for a single function
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionHalstead {
    /// Function name.
    pub name: String,
    /// Source file path.
    pub file: String,
    /// One-based line where the function starts.
    pub line: u32,
    /// Raw Halstead metrics for the function.
    pub metrics: HalsteadInfo,
    /// Threshold classification for the metrics.
    pub thresholds: HalsteadThresholds,
    /// Distinct operators observed in the function when enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operators: Option<Vec<String>>,
    /// Distinct operands observed in the function when enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operands: Option<Vec<String>>,
}

/// Violation record for exceeding thresholds
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HalsteadViolation {
    /// Function name where the violation occurred.
    pub name: String,
    /// Source file path.
    pub file: String,
    /// Metric name (`volume` or `difficulty`).
    pub metric: String,
    /// Observed metric value.
    pub value: f64,
    /// Threshold that was exceeded.
    pub threshold: f64,
}

/// Summary statistics for Halstead analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HalsteadSummary {
    /// Number of analyzed functions.
    pub total_functions: usize,
    /// Mean Halstead volume across analyzed functions.
    pub avg_volume: f64,
    /// Mean Halstead difficulty across analyzed functions.
    pub avg_difficulty: f64,
    /// Mean Halstead effort across analyzed functions.
    pub avg_effort: f64,
    /// Sum of estimated delivered bugs.
    pub total_estimated_bugs: f64,
    /// Number of recorded threshold violations.
    pub violations_count: usize,
}

impl Default for HalsteadSummary {
    fn default() -> Self {
        Self {
            total_functions: 0,
            avg_volume: 0.0,
            avg_difficulty: 0.0,
            avg_effort: 0.0,
            total_estimated_bugs: 0.0,
            violations_count: 0,
        }
    }
}

/// Complete Halstead analysis report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HalsteadReport {
    /// Per-function Halstead metric results.
    pub functions: Vec<FunctionHalstead>,
    /// Threshold violations found during analysis.
    pub violations: Vec<HalsteadViolation>,
    /// Aggregate statistics for the analyzed set.
    pub summary: HalsteadSummary,
    /// Warnings encountered during analysis
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Options for Halstead analysis
#[derive(Debug, Clone, Default)]
pub struct HalsteadOptions {
    /// Specific function to analyze (None = all functions)
    pub function: Option<String>,
    /// Volume threshold for warnings (default: 1000)
    pub volume_threshold: f64,
    /// Difficulty threshold for warnings (default: 20)
    pub difficulty_threshold: f64,
    /// Include list of operators in output
    pub show_operators: bool,
    /// Include list of operands in output
    pub show_operands: bool,
    /// Maximum functions to report (0 = all)
    pub top: usize,
}

impl HalsteadOptions {
    /// Create default Halstead analysis options.
    pub fn new() -> Self {
        Self {
            function: None,
            volume_threshold: 1000.0,
            difficulty_threshold: 20.0,
            show_operators: false,
            show_operands: false,
            top: 0,
        }
    }
}

// =============================================================================
// Main API
// =============================================================================

/// Analyze Halstead metrics for a file
///
/// # Arguments
/// * `path` - Path to the source file
/// * `language` - Programming language (None for auto-detect)
/// * `options` - Analysis options
///
/// # Returns
/// * `Ok(HalsteadReport)` - Report with metrics for all functions
/// * `Err(TldrError)` - On file system or parse errors
///
/// # Example
/// ```ignore
/// use tldr_core::metrics::halstead::{analyze_halstead, HalsteadOptions};
///
/// let report = analyze_halstead(Path::new("src/lib.rs"), None, HalsteadOptions::new())?;
/// for func in &report.functions {
///     println!("{}: volume={:.2}", func.name, func.metrics.volume);
/// }
/// ```
pub fn analyze_halstead(
    path: &Path,
    language: Option<Language>,
    options: HalsteadOptions,
) -> TldrResult<HalsteadReport> {
    // Parse the file
    let (tree, source, detected_lang) = parse_file(path)?;
    let lang = language.unwrap_or(detected_lang);

    // Extract function info to get names and line numbers
    let module = extract_file(path, None)?;

    let mut functions = Vec::new();
    let mut violations = Vec::new();

    // Analyze all functions
    for func_info in &module.functions {
        // Skip if filtering by function name and doesn't match
        if let Some(ref filter) = options.function {
            if &func_info.name != filter {
                continue;
            }
        }

        // Find the function node in the tree
        if let Some(func_node) =
            find_function_node(tree.root_node(), &func_info.name, lang, &source)
        {
            let (metrics, operators_set, operands_set) =
                calculate_function_halstead(func_node, &source, lang);

            let thresholds = evaluate_thresholds(&metrics, &options);

            // Record violations
            if metrics.volume > options.volume_threshold {
                violations.push(HalsteadViolation {
                    name: func_info.name.clone(),
                    file: path.display().to_string(),
                    metric: "volume".to_string(),
                    value: metrics.volume,
                    threshold: options.volume_threshold,
                });
            }
            if metrics.difficulty > options.difficulty_threshold {
                violations.push(HalsteadViolation {
                    name: func_info.name.clone(),
                    file: path.display().to_string(),
                    metric: "difficulty".to_string(),
                    value: metrics.difficulty,
                    threshold: options.difficulty_threshold,
                });
            }

            let func_halstead = FunctionHalstead {
                name: func_info.name.clone(),
                file: path.display().to_string(),
                line: func_info.line_number,
                metrics,
                thresholds,
                operators: if options.show_operators {
                    Some(operators_set.into_iter().collect())
                } else {
                    None
                },
                operands: if options.show_operands {
                    Some(operands_set.into_iter().collect())
                } else {
                    None
                },
            };

            functions.push(func_halstead);
        }
    }

    // Also analyze methods in classes
    for class in &module.classes {
        for method in &class.methods {
            // Skip if filtering by function name and doesn't match
            if let Some(ref filter) = options.function {
                if &method.name != filter {
                    continue;
                }
            }

            if let Some(func_node) =
                find_function_node(tree.root_node(), &method.name, lang, &source)
            {
                let (metrics, operators_set, operands_set) =
                    calculate_function_halstead(func_node, &source, lang);

                let thresholds = evaluate_thresholds(&metrics, &options);

                // Record violations
                if metrics.volume > options.volume_threshold {
                    violations.push(HalsteadViolation {
                        name: method.name.clone(),
                        file: path.display().to_string(),
                        metric: "volume".to_string(),
                        value: metrics.volume,
                        threshold: options.volume_threshold,
                    });
                }
                if metrics.difficulty > options.difficulty_threshold {
                    violations.push(HalsteadViolation {
                        name: method.name.clone(),
                        file: path.display().to_string(),
                        metric: "difficulty".to_string(),
                        value: metrics.difficulty,
                        threshold: options.difficulty_threshold,
                    });
                }

                let func_halstead = FunctionHalstead {
                    name: method.name.clone(),
                    file: path.display().to_string(),
                    line: method.line_number,
                    metrics,
                    thresholds,
                    operators: if options.show_operators {
                        Some(operators_set.into_iter().collect())
                    } else {
                        None
                    },
                    operands: if options.show_operands {
                        Some(operands_set.into_iter().collect())
                    } else {
                        None
                    },
                };

                functions.push(func_halstead);
            }
        }
    }

    // cross-cutting-and-clear-fix-bugs-v1 (P18.X1, Pattern A): for
    // languages whose AST extractor surfaces the same physical method
    // both under `module.functions` and under `module.classes[].methods`
    // (notably Java and Elixir), the loop above pushes one
    // `FunctionHalstead` per surface — emitting every method twice with
    // identical metrics. Dedup by `(name, file, line)` before sorting so
    // each function appears once. We keep the FIRST occurrence to
    // preserve any already-present ordering invariants for non-affected
    // languages where dedup is a no-op.
    {
        use std::collections::HashSet;
        let mut seen: HashSet<(String, String, u32)> = HashSet::new();
        functions.retain(|f| seen.insert((f.name.clone(), f.file.clone(), f.line)));
    }
    // Mirror the same dedup for violations so threshold-violating
    // double-emitted methods don't appear twice in the violations list.
    {
        use std::collections::HashSet;
        let mut seen: HashSet<(String, String, String)> = HashSet::new();
        violations.retain(|v| seen.insert((v.name.clone(), v.file.clone(), v.metric.clone())));
    }

    // Sort by volume (descending) for top-N
    functions.sort_by(|a, b| {
        b.metrics
            .volume
            .partial_cmp(&a.metrics.volume)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Apply top limit if specified
    if options.top > 0 && functions.len() > options.top {
        functions.truncate(options.top);
    }

    // Calculate summary
    let summary = calculate_summary(&functions, violations.len());

    Ok(HalsteadReport {
        functions,
        violations,
        summary,
        warnings: vec![],
    })
}

/// Classify tokens in a function into operators and operands
///
/// Returns (operators, operands) as HashSets
pub fn classify_tokens(
    source: &str,
    language: Language,
) -> TldrResult<(HashSet<String>, HashSet<String>)> {
    let tree = parse(source, language)?;

    let mut operators = HashSet::new();
    let mut operands = HashSet::new();

    classify_node_tokens(
        tree.root_node(),
        source,
        language,
        &mut operators,
        &mut operands,
    );

    Ok((operators, operands))
}

/// Compute Halstead metrics from operator/operand sets
pub fn compute_halstead(
    operators: &HashSet<String>,
    operands: &HashSet<String>,
    total_operators: usize,
    total_operands: usize,
) -> HalsteadInfo {
    HalsteadInfo::from_counts(
        operators.len(),
        operands.len(),
        total_operators,
        total_operands,
    )
}

// =============================================================================
// Internal Helpers
// =============================================================================

/// Calculate Halstead metrics for a single function node
fn calculate_function_halstead(
    func_node: Node,
    source: &str,
    language: Language,
) -> (HalsteadInfo, HashSet<String>, HashSet<String>) {
    let mut operators = HashSet::new();
    let mut operands = HashSet::new();
    let mut total_operators = 0usize;
    let mut total_operands = 0usize;

    // Walk the function subtree
    classify_node_tokens_with_counts(
        func_node,
        source,
        language,
        &mut operators,
        &mut operands,
        &mut total_operators,
        &mut total_operands,
    );

    let metrics = HalsteadInfo::from_counts(
        operators.len(),
        operands.len(),
        total_operators,
        total_operands,
    );

    (metrics, operators, operands)
}

/// Classify tokens into operators/operands (distinct only)
fn classify_node_tokens(
    node: Node,
    source: &str,
    language: Language,
    operators: &mut HashSet<String>,
    operands: &mut HashSet<String>,
) {
    let mut total_ops = 0;
    let mut total_opnds = 0;
    classify_node_tokens_with_counts(
        node,
        source,
        language,
        operators,
        operands,
        &mut total_ops,
        &mut total_opnds,
    );
}

/// Classify tokens with total counts
fn classify_node_tokens_with_counts(
    node: Node,
    source: &str,
    language: Language,
    operators: &mut HashSet<String>,
    operands: &mut HashSet<String>,
    total_operators: &mut usize,
    total_operands: &mut usize,
) {
    let mut stack = vec![node];

    while let Some(current) = stack.pop() {
        let kind = current.kind();
        let text = current.utf8_text(source.as_bytes()).unwrap_or("");

        // Classify based on node kind and language
        if is_operator_node(kind, text, language) {
            operators.insert(normalize_operator(kind, text, language));
            *total_operators += 1;
        } else if is_operand_node(kind, language) {
            operands.insert(text.to_string());
            *total_operands += 1;
        }

        // Add children to stack (depth-first)
        let mut cursor = current.walk();
        if cursor.goto_first_child() {
            loop {
                stack.push(cursor.node());
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }
}

/// Check if a node represents an operator
fn is_operator_node(kind: &str, text: &str, language: Language) -> bool {
    // Keywords that are operators
    let keyword_operators = match language {
        Language::Python => vec![
            "def", "class", "if", "elif", "else", "for", "while", "try", "except", "finally",
            "with", "return", "yield", "raise", "import", "from", "as", "lambda", "and", "or",
            "not", "in", "is", "pass", "break", "continue", "assert", "del", "global", "nonlocal",
            "async", "await", "match", "case",
        ],
        Language::TypeScript | Language::JavaScript => vec![
            "function",
            "class",
            "if",
            "else",
            "for",
            "while",
            "do",
            "switch",
            "case",
            "default",
            "try",
            "catch",
            "finally",
            "return",
            "throw",
            "new",
            "delete",
            "typeof",
            "instanceof",
            "import",
            "export",
            "const",
            "let",
            "var",
            "async",
            "await",
            "yield",
            "break",
            "continue",
            "void",
        ],
        Language::Rust => vec![
            "fn", "struct", "enum", "impl", "trait", "if", "else", "for", "while", "loop", "match",
            "return", "let", "mut", "const", "static", "pub", "use", "mod", "crate", "self",
            "super", "async", "await", "move", "ref", "unsafe", "where", "type",
        ],
        Language::Go => vec![
            "func",
            "type",
            "struct",
            "interface",
            "if",
            "else",
            "for",
            "switch",
            "case",
            "default",
            "select",
            "return",
            "go",
            "defer",
            "chan",
            "map",
            "range",
            "break",
            "continue",
            "goto",
            "fallthrough",
            "package",
            "import",
            "const",
            "var",
        ],
        Language::Java => vec![
            "class",
            "interface",
            "extends",
            "implements",
            "if",
            "else",
            "for",
            "while",
            "do",
            "switch",
            "case",
            "default",
            "try",
            "catch",
            "finally",
            "return",
            "throw",
            "new",
            "instanceof",
            "import",
            "package",
            "public",
            "private",
            "protected",
            "static",
            "final",
            "abstract",
            "synchronized",
            "volatile",
            "transient",
            "native",
            "void",
            "break",
            "continue",
            "assert",
        ],
        Language::C => vec![
            "if", "else", "for", "while", "do", "switch", "case", "default", "return", "goto",
            "break", "continue", "typedef", "struct", "union", "enum", "sizeof", "static",
            "extern", "const", "volatile", "register", "auto", "inline",
        ],
        Language::Cpp => vec![
            "if",
            "else",
            "for",
            "while",
            "do",
            "switch",
            "case",
            "default",
            "return",
            "goto",
            "break",
            "continue",
            "class",
            "struct",
            "union",
            "enum",
            "namespace",
            "using",
            "template",
            "typename",
            "new",
            "delete",
            "try",
            "catch",
            "throw",
            "virtual",
            "override",
            "const",
            "static",
            "extern",
            "inline",
            "constexpr",
            "auto",
            "decltype",
            "sizeof",
            "dynamic_cast",
            "static_cast",
            "reinterpret_cast",
            "const_cast",
        ],
        Language::Ruby => vec![
            "def",
            "class",
            "module",
            "if",
            "elsif",
            "else",
            "unless",
            "for",
            "while",
            "until",
            "do",
            "begin",
            "rescue",
            "ensure",
            "raise",
            "return",
            "yield",
            "block_given?",
            "require",
            "include",
            "extend",
            "attr_reader",
            "attr_writer",
            "attr_accessor",
            "self",
            "super",
            "nil",
            "and",
            "or",
            "not",
            "in",
            "end",
            "case",
            "when",
        ],
        Language::Php => vec![
            "function",
            "class",
            "interface",
            "trait",
            "extends",
            "implements",
            "if",
            "elseif",
            "else",
            "for",
            "foreach",
            "while",
            "do",
            "switch",
            "case",
            "default",
            "try",
            "catch",
            "finally",
            "throw",
            "return",
            "new",
            "instanceof",
            "use",
            "namespace",
            "public",
            "private",
            "protected",
            "static",
            "abstract",
            "final",
            "const",
            "echo",
            "print",
            "isset",
            "unset",
            "empty",
            "array",
            "list",
        ],
        Language::Kotlin => vec![
            "fun",
            "class",
            "object",
            "interface",
            "if",
            "else",
            "for",
            "while",
            "do",
            "when",
            "try",
            "catch",
            "finally",
            "throw",
            "return",
            "break",
            "continue",
            "is",
            "as",
            "in",
            "val",
            "var",
            "import",
            "package",
            "override",
            "open",
            "abstract",
            "sealed",
            "data",
            "companion",
            "suspend",
            "inline",
            "crossinline",
            "noinline",
            "reified",
        ],
        Language::CSharp => vec![
            "class",
            "struct",
            "interface",
            "enum",
            "namespace",
            "using",
            "if",
            "else",
            "for",
            "foreach",
            "while",
            "do",
            "switch",
            "case",
            "default",
            "try",
            "catch",
            "finally",
            "throw",
            "return",
            "new",
            "is",
            "as",
            "typeof",
            "sizeof",
            "ref",
            "out",
            "in",
            "params",
            "public",
            "private",
            "protected",
            "internal",
            "static",
            "virtual",
            "override",
            "abstract",
            "sealed",
            "async",
            "await",
            "yield",
            "break",
            "continue",
            "goto",
            "lock",
            "var",
        ],
        Language::Scala => vec![
            "def", "val", "var", "class", "object", "trait", "extends", "with", "if", "else",
            "for", "while", "do", "match", "case", "try", "catch", "finally", "throw", "return",
            "new", "import", "package", "type", "abstract", "sealed", "final", "override", "lazy",
            "implicit", "yield",
        ],
        Language::Elixir => vec![
            "def",
            "defp",
            "defmodule",
            "defstruct",
            "defprotocol",
            "defimpl",
            "if",
            "else",
            "unless",
            "cond",
            "case",
            "with",
            "for",
            "fn",
            "do",
            "end",
            "raise",
            "rescue",
            "try",
            "catch",
            "after",
            "import",
            "alias",
            "use",
            "require",
            "in",
            "when",
            "and",
            "or",
            "not",
            "pipe_operator",
        ],
        Language::Lua | Language::Luau => vec![
            "function", "if", "then", "elseif", "else", "for", "while", "do", "repeat", "until",
            "return", "break", "local", "end", "in", "and", "or", "not",
        ],
        Language::Ocaml => vec![
            "let",
            "in",
            "if",
            "then",
            "else",
            "match",
            "with",
            "fun",
            "function",
            "rec",
            "and",
            "or",
            "not",
            "mod",
            "type",
            "module",
            "struct",
            "sig",
            "end",
            "open",
            "include",
            "val",
            "begin",
            "try",
            "raise",
            "exception",
            "when",
            "as",
            "of",
        ],
        _ => vec![],
    };

    // Check if it's a keyword operator
    if keyword_operators.contains(&text) {
        return true;
    }

    // Node types that are operators
    matches!(
        kind,
        // Arithmetic and binary operators
        "+" | "-" | "*" | "/" | "%" | "**" | "//" | "@"
        | "binary_operator" | "unary_operator" | "augmented_assignment"

        // Comparison operators
        | "==" | "!=" | "<" | ">" | "<=" | ">=" | "<=>" | "===" | "!=="
        | "comparison_operator"

        // Assignment operators
        | "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "**=" | "//=" | "@="
        | "&=" | "|=" | "^=" | "<<=" | ">>=" | "&&=" | "||=" | "??="
        | "assignment" | "assignment_expression"

        // Logical operators
        | "&&" | "||" | "!" | "and" | "or" | "not"
        | "boolean_operator" | "not_operator"

        // Bitwise operators
        | "&" | "|" | "^" | "~" | "<<" | ">>"

        // Special operators
        | "?:" | "??" | "?." | "=>" | "->" | "::"
        | "conditional_expression" | "ternary_expression"

        // Member access (keep as operator)
        | "."

        // Function/method calls
        | "call" | "call_expression" | "method_call"

        // Member access
        | "attribute" | "subscript" | "member_expression" | "subscript_expression"
    )
}

/// Normalize operator representation
fn normalize_operator(kind: &str, text: &str, _language: Language) -> String {
    // For node types that represent operators, use the kind
    // For actual operator tokens, use the text
    match kind {
        "binary_operator"
        | "unary_operator"
        | "comparison_operator"
        | "boolean_operator"
        | "assignment" => text.to_string(),
        _ => {
            if text.len() <= 3 || is_keyword(text) {
                text.to_string()
            } else {
                kind.to_string()
            }
        }
    }
}

/// Check if text is a keyword
fn is_keyword(text: &str) -> bool {
    matches!(
        text,
        "def"
            | "class"
            | "if"
            | "elif"
            | "else"
            | "for"
            | "while"
            | "try"
            | "except"
            | "finally"
            | "with"
            | "return"
            | "yield"
            | "raise"
            | "import"
            | "from"
            | "as"
            | "lambda"
            | "and"
            | "or"
            | "not"
            | "in"
            | "is"
            | "pass"
            | "break"
            | "continue"
            | "function"
            | "fn"
            | "func"
            | "struct"
            | "enum"
            | "impl"
            | "trait"
            | "match"
            | "case"
            | "const"
            | "let"
            | "mut"
            | "pub"
            | "use"
            | "mod"
            | "async"
            | "await"
    )
}

/// Check if a node represents an operand
fn is_operand_node(kind: &str, _language: Language) -> bool {
    matches!(
        kind,
        // Identifiers
        "identifier" | "property_identifier" | "field_identifier"
        | "shorthand_property_identifier" | "type_identifier"

        // Literals
        | "string" | "string_literal" | "string_content" | "template_string"
        | "integer" | "integer_literal" | "float" | "float_literal"
        | "number" | "number_literal"

        // Boolean/null literals
        | "true" | "false" | "True" | "False"
        | "none" | "None" | "null" | "nil" | "undefined"

        // Special operands
        | "self" | "this" | "super"
    )
}

/// Evaluate threshold status for metrics
fn evaluate_thresholds(metrics: &HalsteadInfo, options: &HalsteadOptions) -> HalsteadThresholds {
    let volume_status = if metrics.volume > options.volume_threshold * 2.0 {
        ThresholdStatus::Bad
    } else if metrics.volume > options.volume_threshold {
        ThresholdStatus::Warning
    } else {
        ThresholdStatus::Good
    };

    let difficulty_status = if metrics.difficulty > options.difficulty_threshold * 2.0 {
        ThresholdStatus::Bad
    } else if metrics.difficulty > options.difficulty_threshold {
        ThresholdStatus::Warning
    } else {
        ThresholdStatus::Good
    };

    HalsteadThresholds {
        volume_status,
        difficulty_status,
    }
}

/// Calculate summary statistics
fn calculate_summary(functions: &[FunctionHalstead], violations_count: usize) -> HalsteadSummary {
    if functions.is_empty() {
        return HalsteadSummary::default();
    }

    let total_volume: f64 = functions.iter().map(|f| f.metrics.volume).sum();
    let total_difficulty: f64 = functions.iter().map(|f| f.metrics.difficulty).sum();
    let total_effort: f64 = functions.iter().map(|f| f.metrics.effort).sum();
    let total_bugs: f64 = functions.iter().map(|f| f.metrics.bugs).sum();

    let count = functions.len() as f64;

    HalsteadSummary {
        total_functions: functions.len(),
        avg_volume: total_volume / count,
        avg_difficulty: total_difficulty / count,
        avg_effort: total_effort / count,
        total_estimated_bugs: total_bugs,
        violations_count,
    }
}

/// Merge multiple Halstead reports into one.
///
/// Combines functions from all reports, sorts by volume descending,
/// applies top-N limit, rebuilds violations, and recalculates summary.
pub fn merge_halstead_reports(
    reports: Vec<HalsteadReport>,
    options: &HalsteadOptions,
) -> HalsteadReport {
    if reports.is_empty() {
        return HalsteadReport {
            functions: vec![],
            violations: vec![],
            summary: HalsteadSummary::default(),
            warnings: vec![],
        };
    }

    // 1. Flatten all functions from all reports
    let mut functions: Vec<FunctionHalstead> = reports
        .iter()
        .flat_map(|r| r.functions.iter().cloned())
        .collect();

    // 2. Merge warnings from all reports
    let warnings: Vec<String> = reports.into_iter().flat_map(|r| r.warnings).collect();

    // 3. Sort by volume descending
    functions.sort_by(|a, b| {
        b.metrics
            .volume
            .partial_cmp(&a.metrics.volume)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // 4. Apply top-N limit
    if options.top > 0 && functions.len() > options.top {
        functions.truncate(options.top);
    }

    // 5. Rebuild violations from the (potentially truncated) function list
    let mut violations = Vec::new();
    for func in &functions {
        if func.metrics.volume > options.volume_threshold {
            violations.push(HalsteadViolation {
                name: func.name.clone(),
                file: func.file.clone(),
                metric: "volume".to_string(),
                value: func.metrics.volume,
                threshold: options.volume_threshold,
            });
        }
        if func.metrics.difficulty > options.difficulty_threshold {
            violations.push(HalsteadViolation {
                name: func.name.clone(),
                file: func.file.clone(),
                metric: "difficulty".to_string(),
                value: func.metrics.difficulty,
                threshold: options.difficulty_threshold,
            });
        }
    }

    // 6. Calculate summary (must compute violations first for count)
    let summary = calculate_summary(&functions, violations.len());

    HalsteadReport {
        functions,
        violations,
        summary,
        warnings,
    }
}

// =============================================================================
// Tests
// =============================================================================

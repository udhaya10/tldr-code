//! L1 commodity tool types and conversion impls
//!
//! Defines types for the L1 diagnostic tool orchestration layer:
//! - `ToolCategory`: classification of tools (linter, security scanner, etc.)
//! - `ToolConfig`: static configuration for a single diagnostic tool
//! - `ToolResult`: execution result from running a tool
//! - `L1Finding`: raw finding from a tool before conversion to `BugbotFinding`

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use super::types::BugbotFinding;

/// Category of commodity diagnostic tool
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    /// Language type checker (e.g., pyright, tsc). Not used for Rust
    /// since clippy subsumes cargo check.
    TypeChecker,
    /// Linter (e.g., clippy, eslint)
    Linter,
    /// Security vulnerability scanner (e.g., cargo-audit)
    SecurityScanner,
}

/// Configuration for a single diagnostic tool
///
/// Uses `&'static str` and `&'static [&'static str]` for zero-allocation registry.
#[derive(Debug, Clone)]
pub struct ToolConfig {
    /// Display name (e.g., "clippy", "cargo-audit")
    pub name: &'static str,
    /// Binary to execute (e.g., "cargo")
    pub binary: &'static str,
    /// Binary to check for availability (e.g., "cargo-clippy").
    /// Different from `binary` for cargo subcommands where the main
    /// binary is "cargo" but detection needs "cargo-clippy". [PM-2]
    pub detection_binary: &'static str,
    /// Arguments to pass (e.g., ["clippy", "--message-format=json"])
    pub args: &'static [&'static str],
    /// Tool category
    pub category: ToolCategory,
    /// Parser identifier (e.g., "cargo", "cargo-audit")
    pub parser: &'static str,
}

/// Result from running a single tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Tool name
    pub name: String,
    /// Tool category
    pub category: ToolCategory,
    /// Whether the tool ran successfully
    pub success: bool,
    /// Execution time in milliseconds
    pub duration_ms: u64,
    /// Number of findings produced
    pub finding_count: usize,
    /// Error message if the tool failed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Process exit code
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

/// L1 finding from a commodity tool before conversion to `BugbotFinding`.
///
/// The `tool` field is set by `ToolRunner` after parsing, not by the parser
/// itself. Parsers set `tool` to an empty string. [PM-6]
#[derive(Debug, Clone)]
pub struct L1Finding {
    /// Tool that produced the finding (set by runner, not parser)
    pub tool: String,
    /// Tool category
    pub category: ToolCategory,
    /// File path (relative to project root)
    pub file: PathBuf,
    /// Line number
    pub line: u32,
    /// Column number
    pub column: u32,
    /// Severity as reported by the tool (e.g., "warning", "error")
    pub native_severity: String,
    /// Normalized severity: "high", "medium", "low", "info"
    pub severity: String,
    /// Human-readable description
    pub message: String,
    /// Tool-specific error/lint code (e.g., "clippy::needless_return")
    pub code: Option<String>,
}

impl From<L1Finding> for BugbotFinding {
    fn from(l1: L1Finding) -> Self {
        BugbotFinding {
            finding_type: format!("tool:{}", l1.tool),
            severity: l1.severity,
            file: l1.file,
            function: String::new(), // L1 findings lack function context
            line: l1.line as usize,
            message: l1.message,
            evidence: serde_json::json!({
                "tool": l1.tool,
                "category": format!("{:?}", l1.category),
                "code": l1.code,
                "native_severity": l1.native_severity,
                "column": l1.column,
            }),
            confidence: None,
            finding_id: None,
        }
    }
}

/// Registry of commodity diagnostic tools per language.
///
/// Maps language names (lowercase strings like "rust", "python") to their
/// configured diagnostic tools. The default registry includes:
/// - Rust: clippy + cargo-audit (NO cargo check -- clippy subsumes it) [PM-1]
///
/// Uses `detection_binary` (not `binary`) for availability probing [PM-2].
pub struct ToolRegistry {
    registry: HashMap<String, Vec<ToolConfig>>,
}

impl ToolRegistry {
    /// Create a new registry with default tool registrations.
    ///
    /// Default Rust tools:
    /// - clippy (linter, detection_binary: "cargo-clippy")
    /// - cargo-audit (security scanner, detection_binary: "cargo-audit")
    ///
    /// CRITICAL [PM-1]: cargo check is NOT included. Clippy subsumes it and
    /// running both would produce duplicate diagnostics plus double compile time.
    pub fn new() -> Self {
        let mut registry = HashMap::new();

        // Rust tools -- ONLY clippy + cargo-audit [PM-1]: cargo check removed,
        // clippy subsumes it.
        registry.insert(
            "rust".to_string(),
            vec![
                ToolConfig {
                    name: "clippy",
                    binary: "cargo",
                    detection_binary: "cargo-clippy", // [PM-2]
                    args: &["clippy", "--message-format=json", "--", "-W", "clippy::all"],
                    category: ToolCategory::Linter,
                    parser: "cargo",
                },
                ToolConfig {
                    name: "cargo-audit",
                    binary: "cargo",
                    detection_binary: "cargo-audit", // [PM-2]
                    args: &["audit", "--json"],
                    category: ToolCategory::SecurityScanner,
                    parser: "cargo-audit",
                },
            ],
        );

        // Python tools -- ruff (fast linter) + pyright (type checker)
        registry.insert(
            "python".to_string(),
            vec![
                ToolConfig {
                    name: "ruff",
                    binary: "ruff",
                    detection_binary: "ruff",
                    args: &["check", "--select=E,F,B,S", "--output-format=json", "."],
                    category: ToolCategory::Linter,
                    parser: "ruff",
                },
                ToolConfig {
                    name: "pyright",
                    binary: "pyright",
                    detection_binary: "pyright",
                    args: &["--outputjson", "."],
                    category: ToolCategory::TypeChecker,
                    parser: "pyright",
                },
            ],
        );

        // JavaScript tools -- eslint
        registry.insert(
            "javascript".to_string(),
            vec![ToolConfig {
                name: "eslint",
                binary: "eslint",
                detection_binary: "eslint",
                args: &["--format", "json", "."],
                category: ToolCategory::Linter,
                parser: "eslint",
            }],
        );

        // TypeScript tools -- eslint (tsc is too slow for L1)
        registry.insert(
            "typescript".to_string(),
            vec![ToolConfig {
                name: "eslint",
                binary: "eslint",
                detection_binary: "eslint",
                args: &["--format", "json", "."],
                category: ToolCategory::Linter,
                parser: "eslint",
            }],
        );

        // Go tools -- golangci-lint
        registry.insert(
            "go".to_string(),
            vec![ToolConfig {
                name: "golangci-lint",
                binary: "golangci-lint",
                detection_binary: "golangci-lint",
                args: &["run", "--out-format", "json"],
                category: ToolCategory::Linter,
                parser: "golangci-lint",
            }],
        );

        // Ruby tools -- rubocop
        registry.insert(
            "ruby".to_string(),
            vec![ToolConfig {
                name: "rubocop",
                binary: "rubocop",
                detection_binary: "rubocop",
                args: &["--format", "json"],
                category: ToolCategory::Linter,
                parser: "rubocop",
            }],
        );

        // Java tools -- checkstyle (plain format, parsed line by line)
        registry.insert(
            "java".to_string(),
            vec![ToolConfig {
                name: "checkstyle",
                binary: "checkstyle",
                detection_binary: "checkstyle",
                args: &["-c", "/google_checks.xml", "-f", "plain", "."],
                category: ToolCategory::Linter,
                parser: "checkstyle",
            }],
        );

        // Kotlin tools -- ktlint
        registry.insert(
            "kotlin".to_string(),
            vec![ToolConfig {
                name: "ktlint",
                binary: "ktlint",
                detection_binary: "ktlint",
                args: &["--reporter=json"],
                category: ToolCategory::Linter,
                parser: "ktlint",
            }],
        );

        // Swift tools -- swiftlint
        registry.insert(
            "swift".to_string(),
            vec![ToolConfig {
                name: "swiftlint",
                binary: "swiftlint",
                detection_binary: "swiftlint",
                args: &["lint", "--reporter", "json"],
                category: ToolCategory::Linter,
                parser: "swiftlint",
            }],
        );

        // C tools -- cppcheck (tab-separated template output)
        registry.insert(
            "c".to_string(),
            vec![ToolConfig {
                name: "cppcheck",
                binary: "cppcheck",
                detection_binary: "cppcheck",
                args: &[
                    "--enable=all",
                    "--template={file}\t{line}\t{column}\t{severity}\t{id}\t{message}",
                    ".",
                ],
                category: ToolCategory::Linter,
                parser: "cppcheck",
            }],
        );

        // C++ tools -- cppcheck (same parser as C)
        registry.insert(
            "cpp".to_string(),
            vec![ToolConfig {
                name: "cppcheck",
                binary: "cppcheck",
                detection_binary: "cppcheck",
                args: &[
                    "--enable=all",
                    "--language=c++",
                    "--template={file}\t{line}\t{column}\t{severity}\t{id}\t{message}",
                    ".",
                ],
                category: ToolCategory::Linter,
                parser: "cppcheck",
            }],
        );

        // PHP tools -- phpstan
        registry.insert(
            "php".to_string(),
            vec![ToolConfig {
                name: "phpstan",
                binary: "phpstan",
                detection_binary: "phpstan",
                args: &["analyse", "--error-format=json", "--no-progress", "."],
                category: ToolCategory::Linter,
                parser: "phpstan",
            }],
        );

        // Lua tools -- luacheck (plain format, parsed line by line)
        registry.insert(
            "lua".to_string(),
            vec![ToolConfig {
                name: "luacheck",
                binary: "luacheck",
                detection_binary: "luacheck",
                args: &["--formatter", "plain", "."],
                category: ToolCategory::Linter,
                parser: "luacheck",
            }],
        );

        Self { registry }
    }

    /// Get all configured tools for a language.
    ///
    /// Returns an empty `Vec` if the language has no registered tools.
    pub fn tools_for_language(&self, lang: &str) -> Vec<&ToolConfig> {
        self.registry
            .get(lang)
            .map(|tools| tools.iter().collect())
            .unwrap_or_default()
    }

    /// Detect which tools are actually installed on the system.
    ///
    /// Probes `detection_binary` (not `binary`) to check availability [PM-2].
    /// For cargo subcommands, this correctly checks for e.g. "cargo-clippy"
    /// rather than just "cargo".
    ///
    /// Returns `(available, missing)` where each is a list of tool configs.
    pub fn detect_available_tools(&self, lang: &str) -> (Vec<&ToolConfig>, Vec<&ToolConfig>) {
        let all_tools = self.tools_for_language(lang);
        let mut available = Vec::new();
        let mut missing = Vec::new();

        for tool in all_tools {
            if which::which(tool.detection_binary).is_ok() {
                available.push(tool);
            } else {
                missing.push(tool);
            }
        }

        (available, missing)
    }

    /// Register a tool for a language.
    ///
    /// Appends the tool to any existing tools for the language.
    pub fn register_tool(&mut self, lang: &str, config: ToolConfig) {
        self.registry
            .entry(lang.to_string())
            .or_default()
            .push(config);
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

//! Output Formatting for Reaching Definitions (RD-14, RD-15, RD-16)
//!
//! This module provides output formatters for reaching definitions reports:
//! - JSON output (RD-15): Structured format for programmatic consumption
//! - Text output (RD-14): Human-readable format for CLI display
//! - Variable filtering (RD-16): Filter output to a specific variable
//!
//! # JSON Schema
//!
//! The JSON output follows the schema from session10-spec.md Section 5.4:
//! ```json
//! {
//!   "function": "process_data",
//!   "file": "src/main.py",
//!   "blocks": [...],
//!   "def_use_chains": [...],
//!   "use_def_chains": [...],
//!   "uninitialized": [...],
//!   "stats": {...}
//! }
//! ```
//!
//! # Text Format
//!
//! The text output follows the format from session10-spec.md Section 5.5:
//! ```text
//! Reaching Definitions for: process_data in src/main.py
//!
//! Block 0 (lines 1-3):
//!     GEN:  {x@1, y@2}
//!     KILL: {}
//!     IN:   {}
//!     OUT:  {x@1, y@2}
//! ...
//! ```

use super::chains::{
    BlockReachingDefs, DefUseChain, Definition, ReachingDefsReport, ReachingDefsStats,
    UninitializedUse, UseDefChain,
};

// =============================================================================
// JSON Output (RD-15)
// =============================================================================

/// Format reaching definitions report as JSON.
///
/// The output is "pretty-printed" with indentation for readability.
///
/// # Arguments
/// * `report` - The reaching definitions report to format
///
/// # Returns
/// * `Result<String, serde_json::Error>` - JSON string or error
///
/// # Example
/// ```ignore
/// let report = build_reaching_defs_report(&cfg, &refs, path);
/// let json = format_reaching_defs_json(&report)?;
/// println!("{}", json);
/// ```
pub fn format_reaching_defs_json(report: &ReachingDefsReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}

/// Format reaching definitions report as compact JSON.
///
/// No whitespace or indentation - suitable for machine processing.
///
/// # Arguments
/// * `report` - The reaching definitions report to format
///
/// # Returns
/// * `Result<String, serde_json::Error>` - Compact JSON string or error
pub fn format_reaching_defs_json_compact(
    report: &ReachingDefsReport,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(report)
}

// =============================================================================
// Text Output (RD-14)
// =============================================================================

/// Options controlling which sections appear in text output.
///
/// Controls visibility of per-block details, chains, header, and statistics
/// in the human-readable text formatter.
///
/// # Defaults
///
/// The default configuration shows header, chains, uninitialized warnings,
/// and statistics, but hides per-block GEN/KILL/IN/OUT details (since
/// `show_in_out` defaults to false in the CLI).
///
/// # Examples
///
/// ```ignore
/// // Default: header + chains + stats (no blocks)
/// let opts = ReachingDefsFormatOptions::default();
///
/// // Chains only: just def-use and use-def chains
/// let opts = ReachingDefsFormatOptions::chains_only();
///
/// // Everything: blocks + chains + header + stats
/// let opts = ReachingDefsFormatOptions {
///     show_blocks: true,
///     ..ReachingDefsFormatOptions::default()
/// };
/// ```
#[derive(Debug, Clone)]
pub struct ReachingDefsFormatOptions {
    /// Show per-block GEN/KILL/IN/OUT sets (controlled by --show-in-out)
    pub show_blocks: bool,
    /// Show def-use and use-def chains
    pub show_chains: bool,
    /// Show potentially uninitialized variable warnings
    pub show_uninitialized: bool,
    /// Show the header line ("Reaching Definitions for: ...")
    pub show_header: bool,
    /// Show the statistics summary at the bottom
    pub show_stats: bool,
}

impl Default for ReachingDefsFormatOptions {
    fn default() -> Self {
        Self {
            show_blocks: false,
            show_chains: true,
            show_uninitialized: true,
            show_header: true,
            show_stats: true,
        }
    }
}

impl ReachingDefsFormatOptions {
    /// Create options that show only def-use/use-def chains.
    ///
    /// Hides header, per-block details, uninitialized warnings, and statistics.
    /// Useful for piping into other tools or getting concise output.
    pub fn chains_only() -> Self {
        Self {
            show_blocks: false,
            show_chains: true,
            show_uninitialized: false,
            show_header: false,
            show_stats: false,
        }
    }
}

/// Format reaching definitions report as human-readable text with options.
///
/// Allows controlling which sections appear in the output via
/// `ReachingDefsFormatOptions`.
///
/// # Arguments
/// * `report` - The reaching definitions report to format
/// * `options` - Controls which sections to include in output
///
/// # Returns
/// * `String` - Formatted text output
///
/// # Example
/// ```ignore
/// let report = build_reaching_defs_report(&cfg, &refs, path);
/// let opts = ReachingDefsFormatOptions::chains_only();
/// let text = format_reaching_defs_text_with_options(&report, &opts);
/// println!("{}", text);
/// ```
pub fn format_reaching_defs_text_with_options(
    report: &ReachingDefsReport,
    options: &ReachingDefsFormatOptions,
) -> String {
    let mut output = String::new();

    // Header
    if options.show_header {
        output.push_str(&format!(
            "Reaching Definitions for: {} in {}\n\n",
            report.function,
            report.file.display()
        ));
    }

    // Blocks with GEN/KILL/IN/OUT sets
    if options.show_blocks {
        for block in &report.blocks {
            output.push_str(&format!(
                "Block {} (lines {}-{}):\n",
                block.id, block.lines.0, block.lines.1
            ));

            output.push_str(&format!("    GEN:  {{{}}}\n", format_def_set(&block.gen)));
            output.push_str(&format!("    KILL: {{{}}}\n", format_def_set(&block.kill)));
            output.push_str(&format!(
                "    IN:   {{{}}}\n",
                format_def_set(&block.in_set)
            ));
            output.push_str(&format!("    OUT:  {{{}}}\n", format_def_set(&block.out)));
            output.push('\n');
        }
    }

    // Def-Use Chains
    if options.show_chains {
        output.push_str("Def-Use Chains:\n");
        if report.def_use_chains.is_empty() {
            output.push_str("    (none)\n");
        } else {
            for chain in &report.def_use_chains {
                let uses: Vec<String> = chain
                    .uses
                    .iter()
                    .map(|u| format!("line {}", u.line))
                    .collect();
                let uses_str = if uses.is_empty() {
                    "(unused)".to_string()
                } else {
                    uses.join(", ")
                };
                output.push_str(&format!(
                    "    {}@{} -> used at: {}\n",
                    chain.definition.var, chain.definition.line, uses_str
                ));
            }
        }
        output.push('\n');

        // Use-Def Chains
        output.push_str("Use-Def Chains:\n");
        if report.use_def_chains.is_empty() {
            output.push_str("    (none)\n");
        } else {
            for chain in &report.use_def_chains {
                let defs: Vec<String> = chain
                    .reaching_defs
                    .iter()
                    .map(|d| format!("line {}", d.line))
                    .collect();
                let defs_str = if defs.is_empty() {
                    "(no reaching definition)".to_string()
                } else {
                    defs.join(", ")
                };
                output.push_str(&format!(
                    "    {}@{} <- defined at: {}\n",
                    chain.var, chain.use_site.line, defs_str
                ));
            }
        }
        output.push('\n');
    }

    // Uninitialized Variables
    if options.show_uninitialized {
        output.push_str("Potentially Uninitialized:\n");
        if report.uninitialized.is_empty() {
            output.push_str("    (none detected)\n");
        } else {
            for uninit in &report.uninitialized {
                output.push_str(&format!(
                    "    {} at line {} ({}): {}\n",
                    uninit.var, uninit.line, uninit.severity, uninit.reason
                ));
            }
        }
        output.push('\n');
    }

    // Statistics
    if options.show_stats {
        output.push_str("---\n");
        output.push_str(&format!("Definitions: {}\n", report.stats.definitions));
        output.push_str(&format!("Uses: {}\n", report.stats.uses));
        output.push_str(&format!("Blocks: {}\n", report.stats.blocks));
        if report.stats.iterations > 0 {
            output.push_str(&format!("Iterations: {}\n", report.stats.iterations));
        }
        if report.stats.uninitialized_count > 0 {
            output.push_str(&format!(
                "Uninitialized: {}\n",
                report.stats.uninitialized_count
            ));
        }
    }

    output
}

/// Format reaching definitions report as human-readable text.
///
/// The output includes:
/// - Header with function name and file
/// - Per-block GEN/KILL/IN/OUT sets
/// - Def-use chains
/// - Use-def chains
/// - Uninitialized variable warnings
/// - Statistics summary
///
/// This is the original function that always shows all sections including
/// per-block details. For selective output, use
/// `format_reaching_defs_text_with_options`.
///
/// # Arguments
/// * `report` - The reaching definitions report to format
///
/// # Returns
/// * `String` - Formatted text output
///
/// # Example
/// ```ignore
/// let report = build_reaching_defs_report(&cfg, &refs, path);
/// let text = format_reaching_defs_text(&report);
/// println!("{}", text);
/// ```
pub fn format_reaching_defs_text(report: &ReachingDefsReport) -> String {
    let mut output = String::new();

    // Header
    output.push_str(&format!(
        "Reaching Definitions for: {} in {}\n\n",
        report.function,
        report.file.display()
    ));

    // Blocks with GEN/KILL/IN/OUT sets
    for block in &report.blocks {
        output.push_str(&format!(
            "Block {} (lines {}-{}):\n",
            block.id, block.lines.0, block.lines.1
        ));

        output.push_str(&format!("    GEN:  {{{}}}\n", format_def_set(&block.gen)));
        output.push_str(&format!("    KILL: {{{}}}\n", format_def_set(&block.kill)));
        output.push_str(&format!(
            "    IN:   {{{}}}\n",
            format_def_set(&block.in_set)
        ));
        output.push_str(&format!("    OUT:  {{{}}}\n", format_def_set(&block.out)));
        output.push('\n');
    }

    // Def-Use Chains
    output.push_str("Def-Use Chains:\n");
    if report.def_use_chains.is_empty() {
        output.push_str("    (none)\n");
    } else {
        for chain in &report.def_use_chains {
            let uses: Vec<String> = chain
                .uses
                .iter()
                .map(|u| format!("line {}", u.line))
                .collect();
            let uses_str = if uses.is_empty() {
                "(unused)".to_string()
            } else {
                uses.join(", ")
            };
            output.push_str(&format!(
                "    {}@{} -> used at: {}\n",
                chain.definition.var, chain.definition.line, uses_str
            ));
        }
    }
    output.push('\n');

    // Use-Def Chains
    output.push_str("Use-Def Chains:\n");
    if report.use_def_chains.is_empty() {
        output.push_str("    (none)\n");
    } else {
        for chain in &report.use_def_chains {
            let defs: Vec<String> = chain
                .reaching_defs
                .iter()
                .map(|d| format!("line {}", d.line))
                .collect();
            let defs_str = if defs.is_empty() {
                "(no reaching definition)".to_string()
            } else {
                defs.join(", ")
            };
            output.push_str(&format!(
                "    {}@{} <- defined at: {}\n",
                chain.var, chain.use_site.line, defs_str
            ));
        }
    }
    output.push('\n');

    // Uninitialized Variables
    output.push_str("Potentially Uninitialized:\n");
    if report.uninitialized.is_empty() {
        output.push_str("    (none detected)\n");
    } else {
        for uninit in &report.uninitialized {
            output.push_str(&format!(
                "    {} at line {} ({}): {}\n",
                uninit.var, uninit.line, uninit.severity, uninit.reason
            ));
        }
    }
    output.push('\n');

    // Statistics
    output.push_str("---\n");
    output.push_str(&format!("Definitions: {}\n", report.stats.definitions));
    output.push_str(&format!("Uses: {}\n", report.stats.uses));
    output.push_str(&format!("Blocks: {}\n", report.stats.blocks));
    if report.stats.iterations > 0 {
        output.push_str(&format!("Iterations: {}\n", report.stats.iterations));
    }
    if report.stats.uninitialized_count > 0 {
        output.push_str(&format!(
            "Uninitialized: {}\n",
            report.stats.uninitialized_count
        ));
    }

    output
}

/// Format a set of definitions as "var@line, var@line, ..."
fn format_def_set(defs: &[Definition]) -> String {
    if defs.is_empty() {
        return String::new();
    }

    let mut parts: Vec<String> = defs
        .iter()
        .map(|d| format!("{}@{}", d.var, d.line))
        .collect();

    // Sort for consistent output
    parts.sort();

    parts.join(", ")
}

// =============================================================================
// Variable Filtering (RD-16)
// =============================================================================

/// Filter reaching definitions report to a specific variable.
///
/// Returns a new report containing only information about the specified variable:
/// - Blocks with GEN/KILL/IN/OUT filtered to that variable
/// - Def-use chains for that variable
/// - Use-def chains for that variable
/// - Uninitialized warnings for that variable
/// - Updated statistics
///
/// # Arguments
/// * `report` - The original report
/// * `var` - Variable name to filter by
///
/// # Returns
/// * `ReachingDefsReport` - Filtered report
///
/// # Example
/// ```ignore
/// let report = build_reaching_defs_report(&cfg, &refs, path);
/// let filtered = filter_reaching_defs_by_variable(&report, "x");
/// // filtered only contains info about variable "x"
/// ```
pub fn filter_reaching_defs_by_variable(
    report: &ReachingDefsReport,
    var: &str,
) -> ReachingDefsReport {
    // Filter blocks
    let blocks: Vec<BlockReachingDefs> = report
        .blocks
        .iter()
        .map(|b| BlockReachingDefs {
            id: b.id,
            lines: b.lines,
            gen: filter_definitions(&b.gen, var),
            kill: filter_definitions(&b.kill, var),
            in_set: filter_definitions(&b.in_set, var),
            out: filter_definitions(&b.out, var),
        })
        .collect();

    // Filter def-use chains
    let def_use_chains: Vec<DefUseChain> = report
        .def_use_chains
        .iter()
        .filter(|c| c.definition.var == var)
        .cloned()
        .collect();

    // Filter use-def chains
    let use_def_chains: Vec<UseDefChain> = report
        .use_def_chains
        .iter()
        .filter(|c| c.var == var)
        .cloned()
        .collect();

    // Filter uninitialized
    let uninitialized: Vec<UninitializedUse> = report
        .uninitialized
        .iter()
        .filter(|u| u.var == var)
        .cloned()
        .collect();

    // Compute filtered stats
    let definitions = def_use_chains.len();
    let uses = use_def_chains.len();
    let stats = ReachingDefsStats {
        definitions,
        uses,
        blocks: report.stats.blocks,
        iterations: report.stats.iterations,
        uninitialized_count: uninitialized.len(),
    };

    ReachingDefsReport {
        function: report.function.clone(),
        file: report.file.clone(),
        blocks,
        def_use_chains,
        use_def_chains,
        uninitialized,
        stats,
        uncertain_defs: report.uncertain_defs.clone(),
        confidence: report.confidence,
    }
}

/// Filter a list of definitions to only those matching a variable name
fn filter_definitions(defs: &[Definition], var: &str) -> Vec<Definition> {
    defs.iter().filter(|d| d.var == var).cloned().collect()
}

// =============================================================================
// Tests
// =============================================================================

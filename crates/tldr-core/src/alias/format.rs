//! Alias Analysis Output Formatting
//!
//! This module provides output formatters for alias analysis results.
//! Supports JSON, human-readable text, and DOT graph formats.
//!
//! # Formats
//!
//! - **JSON**: Machine-readable, suitable for tooling integration
//! - **Text**: Human-readable, for terminal output and debugging
//! - **DOT**: Graphviz format for visualizing alias relationships
//!
//! # Example
//!
//! ```rust,ignore
//! use tldr_core::alias::{AliasInfo, AliasOutputFormat};
//!
//! let info = compute_alias_from_ssa(&ssa)?;
//!
//! // JSON output
//! println!("{}", info.to_json());
//!
//! // Human-readable text
//! println!("{}", info.to_text());
//!
//! // Graphviz DOT
//! println!("{}", info.to_dot());
//!
//! // Generic format dispatch
//! println!("{}", info.format(AliasOutputFormat::Text));
//! ```

use std::collections::{BTreeMap, BTreeSet};

use super::AliasInfo;

// =============================================================================
// Output Format Enum
// =============================================================================

/// Output format for alias analysis results.
///
/// Determines how the analysis results are serialized for display or storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AliasOutputFormat {
    /// JSON format (default) - machine-readable
    #[default]
    Json,
    /// Human-readable text format
    Text,
    /// Graphviz DOT format for visualization
    Dot,
}

impl std::str::FromStr for AliasOutputFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "json" => Ok(AliasOutputFormat::Json),
            "text" | "txt" => Ok(AliasOutputFormat::Text),
            "dot" | "graphviz" => Ok(AliasOutputFormat::Dot),
            _ => Err(format!(
                "Unknown format '{}'. Supported: json, text, dot",
                s
            )),
        }
    }
}

impl std::fmt::Display for AliasOutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AliasOutputFormat::Json => write!(f, "json"),
            AliasOutputFormat::Text => write!(f, "text"),
            AliasOutputFormat::Dot => write!(f, "dot"),
        }
    }
}

// =============================================================================
// AliasInfo Formatting Methods
// =============================================================================

impl AliasInfo {
    /// Format alias info as JSON string.
    ///
    /// Output format:
    /// ```json
    /// {
    ///   "function": "foo",
    ///   "may_alias": [["x", "y"], ["a", "b"]],
    ///   "must_alias": [["x", "y"]],
    ///   "points_to": {
    ///     "x": ["alloc_5", "param_p"],
    ///     "y": ["alloc_5"]
    ///   },
    ///   "allocation_sites": ["alloc_5", "param_p", "unknown_3"]
    /// }
    /// ```
    pub fn to_json(&self) -> String {
        // Convert may_alias to list of pairs (sorted, deduplicated)
        let may_alias_pairs = self.collect_alias_pairs(&self.may_alias);

        // Convert must_alias to list of pairs (sorted, deduplicated)
        let must_alias_pairs = self.collect_alias_pairs(&self.must_alias);

        // Sort points_to for deterministic output
        let sorted_points_to: BTreeMap<_, Vec<_>> = self
            .points_to
            .iter()
            .map(|(k, v)| {
                let mut sorted: Vec<_> = v.iter().cloned().collect();
                sorted.sort();
                (k.clone(), sorted)
            })
            .collect();

        // Collect all allocation sites as sorted list
        let allocation_sites: BTreeSet<_> = self.allocation_sites.values().cloned().collect();

        // Uncertain aliases
        let uncertain: Vec<_> = self
            .uncertain
            .iter()
            .map(|ua| {
                serde_json::json!({
                    "vars": ua.vars,
                    "line": ua.line,
                    "reason": ua.reason,
                })
            })
            .collect();

        let confidence_str = match self.confidence {
            super::types::Confidence::Low => "low",
            super::types::Confidence::Medium => "medium",
            super::types::Confidence::High => "high",
        };

        let mut json = serde_json::json!({
            "function": self.function_name,
            "may_alias": may_alias_pairs,
            "must_alias": must_alias_pairs,
            "points_to": sorted_points_to,
            "allocation_sites": allocation_sites,
            "uncertain": uncertain,
            "confidence": confidence_str,
        });

        // Add language_notes only if non-empty
        if !self.language_notes.is_empty() {
            json.as_object_mut().unwrap().insert(
                "language_notes".to_string(),
                serde_json::Value::String(self.language_notes.clone()),
            );
        }

        serde_json::to_string_pretty(&json).unwrap_or_else(|_| "{}".to_string())
    }

    /// Format alias info as human-readable text.
    ///
    /// Output format:
    /// ```text
    /// Alias Analysis: foo
    /// ====================
    /// May-Alias:
    ///   x <-> y (shared: alloc_5)
    ///   a <-> b (shared: param_p)
    ///
    /// Must-Alias:
    ///   x <-> y
    ///
    /// Points-To:
    ///   x -> {alloc_5, param_p}
    ///   y -> {alloc_5}
    /// ```
    pub fn to_text(&self) -> String {
        let mut output = String::new();

        // Header
        let header = format!("Alias Analysis: {}", self.function_name);
        output.push_str(&header);
        output.push('\n');
        output.push_str(&"=".repeat(header.len()));
        output.push('\n');

        // May-Alias section
        output.push_str("May-Alias:\n");
        let may_pairs = self.collect_alias_pairs(&self.may_alias);
        if may_pairs.is_empty() {
            output.push_str("  (none)\n");
        } else {
            for pair in &may_pairs {
                if pair.len() == 2 {
                    let shared = self.find_shared_locations(&pair[0], &pair[1]);
                    if shared.is_empty() {
                        output.push_str(&format!("  {} <-> {}\n", pair[0], pair[1]));
                    } else {
                        output.push_str(&format!(
                            "  {} <-> {} (shared: {})\n",
                            pair[0],
                            pair[1],
                            shared.join(", ")
                        ));
                    }
                }
            }
        }
        output.push('\n');

        // Must-Alias section
        output.push_str("Must-Alias:\n");
        let must_pairs = self.collect_alias_pairs(&self.must_alias);
        if must_pairs.is_empty() {
            output.push_str("  (none)\n");
        } else {
            for pair in &must_pairs {
                if pair.len() == 2 {
                    output.push_str(&format!("  {} <-> {}\n", pair[0], pair[1]));
                }
            }
        }
        output.push('\n');

        // Points-To section
        output.push_str("Points-To:\n");
        if self.points_to.is_empty() {
            output.push_str("  (none)\n");
        } else {
            // Sort variables for deterministic output
            let sorted_vars: BTreeMap<_, _> = self.points_to.iter().collect();
            for (var, locations) in sorted_vars {
                let mut sorted_locs: Vec<_> = locations.iter().cloned().collect();
                sorted_locs.sort();
                output.push_str(&format!("  {} -> {{{}}}\n", var, sorted_locs.join(", ")));
            }
        }

        output
    }

    /// Format alias info as Graphviz DOT format.
    ///
    /// Output format:
    /// ```dot
    /// digraph alias {
    ///   rankdir=LR;
    ///   x -> alloc_5;
    ///   x -> param_p;
    ///   y -> alloc_5;
    ///   x -> y [style=dashed, label="may"];
    /// }
    /// ```
    pub fn to_dot(&self) -> String {
        let mut output = String::new();

        output.push_str("digraph alias {\n");
        output.push_str("  rankdir=LR;\n");
        output.push_str("  node [shape=box];\n");
        output.push('\n');

        // Define variable nodes
        let mut all_vars: BTreeSet<_> = self.points_to.keys().cloned().collect();
        for vars in self.may_alias.keys() {
            all_vars.insert(vars.clone());
        }
        for vars in self.must_alias.keys() {
            all_vars.insert(vars.clone());
        }

        // Variable nodes (boxes)
        output.push_str("  // Variables\n");
        for var in &all_vars {
            output.push_str(&format!(
                "  \"{}\" [shape=box, style=filled, fillcolor=lightblue];\n",
                escape_dot_label(var)
            ));
        }
        output.push('\n');

        // Location nodes (ellipses)
        output.push_str("  // Abstract Locations\n");
        let mut all_locations: BTreeSet<String> = BTreeSet::new();
        for locs in self.points_to.values() {
            all_locations.extend(locs.iter().cloned());
        }
        for loc in &all_locations {
            output.push_str(&format!(
                "  \"{}\" [shape=ellipse, style=filled, fillcolor=lightyellow];\n",
                escape_dot_label(loc)
            ));
        }
        output.push('\n');

        // Points-to edges (solid arrows)
        output.push_str("  // Points-To Edges\n");
        let sorted_pts: BTreeMap<_, _> = self.points_to.iter().collect();
        for (var, locations) in sorted_pts {
            let mut sorted_locs: Vec<_> = locations.iter().collect();
            sorted_locs.sort();
            for loc in sorted_locs {
                output.push_str(&format!(
                    "  \"{}\" -> \"{}\";\n",
                    escape_dot_label(var),
                    escape_dot_label(loc)
                ));
            }
        }
        output.push('\n');

        // May-alias edges (dashed, bidirectional)
        output.push_str("  // May-Alias Edges\n");
        let may_pairs = self.collect_alias_pairs(&self.may_alias);
        for pair in &may_pairs {
            if pair.len() == 2 {
                output.push_str(&format!(
                    "  \"{}\" -> \"{}\" [style=dashed, label=\"may\", dir=both, color=orange];\n",
                    escape_dot_label(&pair[0]),
                    escape_dot_label(&pair[1])
                ));
            }
        }
        output.push('\n');

        // Must-alias edges (bold, bidirectional)
        output.push_str("  // Must-Alias Edges\n");
        let must_pairs = self.collect_alias_pairs(&self.must_alias);
        for pair in &must_pairs {
            if pair.len() == 2 {
                output.push_str(&format!(
                    "  \"{}\" -> \"{}\" [style=bold, label=\"must\", dir=both, color=green];\n",
                    escape_dot_label(&pair[0]),
                    escape_dot_label(&pair[1])
                ));
            }
        }

        output.push_str("}\n");
        output
    }

    /// Format using the specified output format.
    ///
    /// Dispatches to the appropriate formatter based on the format enum.
    pub fn format(&self, format: AliasOutputFormat) -> String {
        match format {
            AliasOutputFormat::Json => self.to_json(),
            AliasOutputFormat::Text => self.to_text(),
            AliasOutputFormat::Dot => self.to_dot(),
        }
    }

    // =========================================================================
    // Helper Methods
    // =========================================================================

    /// Collect alias relationships as deduplicated, sorted pairs.
    ///
    /// Converts the symmetric map representation to a list of [a, b] pairs
    /// where a < b (lexicographically) to avoid duplicates like [a, b] and [b, a].
    fn collect_alias_pairs(
        &self,
        alias_map: &std::collections::HashMap<String, std::collections::HashSet<String>>,
    ) -> Vec<Vec<String>> {
        let mut pairs: BTreeSet<(String, String)> = BTreeSet::new();

        for (var, aliases) in alias_map {
            for alias in aliases {
                // Normalize pair ordering to avoid duplicates
                let pair = if var < alias {
                    (var.clone(), alias.clone())
                } else {
                    (alias.clone(), var.clone())
                };
                pairs.insert(pair);
            }
        }

        pairs.into_iter().map(|(a, b)| vec![a, b]).collect()
    }

    /// Find shared locations between two variables (for text output annotation).
    fn find_shared_locations(&self, a: &str, b: &str) -> Vec<String> {
        let pts_a = self.points_to.get(a);
        let pts_b = self.points_to.get(b);

        match (pts_a, pts_b) {
            (Some(set_a), Some(set_b)) => {
                let mut shared: Vec<_> = set_a.intersection(set_b).cloned().collect();
                shared.sort();
                shared
            }
            _ => Vec::new(),
        }
    }
}

/// Escape special characters in DOT labels.
fn escape_dot_label(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

// =============================================================================
// Tests
// =============================================================================

//! Patch applicator -- applies `Fix` edits to source text.
//!
//! Takes source code as a string and a `Vec<TextEdit>`, and returns the
//! patched source string. Edits are applied in reverse line order to
//! preserve line numbers for subsequent edits.

use super::types::{EditKind, Fix, TextEdit};

/// Apply a set of text edits to source code.
///
/// The edits are sorted by line number in descending order before application,
/// so that earlier edits don't shift line numbers for later ones.
///
/// Returns the patched source string.
pub fn apply_fix(source: &str, fix: &Fix) -> String {
    apply_edits(source, &fix.edits)
}

/// Apply a vector of text edits to source code.
///
/// Edits are sorted by line number (descending) before application.
pub fn apply_edits(source: &str, edits: &[TextEdit]) -> String {
    if edits.is_empty() {
        return source.to_string();
    }

    let mut lines: Vec<String> = source.lines().map(|l| l.to_string()).collect();

    // If source ends with newline, preserve it
    let ends_with_newline = source.ends_with('\n');

    // Sort edits by line number descending so we apply from bottom to top
    let mut sorted_edits: Vec<&TextEdit> = edits.iter().collect();
    sorted_edits.sort_by(|a, b| b.line.cmp(&a.line));

    for edit in sorted_edits {
        // Line numbers are 1-indexed
        let idx = edit.line.saturating_sub(1);

        match &edit.kind {
            EditKind::InsertBefore => {
                if idx <= lines.len() {
                    lines.insert(idx, edit.new_text.clone());
                }
            }
            EditKind::InsertAfter => {
                let insert_at = (idx + 1).min(lines.len());
                lines.insert(insert_at, edit.new_text.clone());
            }
            EditKind::ReplaceLine => {
                if idx < lines.len() {
                    lines[idx] = edit.new_text.clone();
                }
            }
            EditKind::DeleteLine => {
                if idx < lines.len() {
                    lines.remove(idx);
                }
            }
            EditKind::ReplaceRange { start_col, end_col } => {
                if idx < lines.len() {
                    let line = &lines[idx];
                    let start = (*start_col).min(line.len());
                    let end = (*end_col).min(line.len());
                    let mut new_line = String::new();
                    new_line.push_str(&line[..start]);
                    new_line.push_str(&edit.new_text);
                    new_line.push_str(&line[end..]);
                    lines[idx] = new_line;
                }
            }
        }
    }

    let mut result = lines.join("\n");
    if ends_with_newline && !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

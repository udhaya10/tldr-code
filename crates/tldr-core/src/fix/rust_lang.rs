//! Rust error analyzers -- 5 analyzers ported from FastEdit rust_analyzers.py.
//!
//! Each analyzer is a pure function that takes a `ParsedError`, source code,
//! and a tree-sitter `Tree`, and returns an `Option<Diagnosis>`.
//!
//! # Analyzer Inventory (5 total)
//!
//! | # | Error Code | Analyzer           | Fix                                          |
//! |---|------------|--------------------|----------------------------------------------|
//! | 1 | E0599      | MethodNotFound     | Inject `use <trait>` from TRAIT_IMPORTS table |
//! | 2 | E0277      | TypeMismatch       | Insert `.copied()`, `as usize`, etc.         |
//! | 3 | E0425      | NotInScope         | Inject `use` from KNOWN_ITEMS table          |
//! | 4 | E0433      | FailedToResolve    | Inject `use` from KNOWN_ITEMS table (type-position) |
//! | 5 | E0308      | MismatchedTypes    | Type coercion: &str->String, Option, &       |

use regex::Regex;
use tree_sitter::Tree;

use super::types::{Diagnosis, EditKind, Fix, FixConfidence, FixLocation, ParsedError, TextEdit};

// ============================================================================
// Known-fix lookup tables (data, not code)
// ============================================================================

/// E0599: Method not found -> trait import to add.
///
/// Maps a method name to the `use` statement that brings the required trait
/// into scope. Ported from FastEdit TRAIT_IMPORTS dict.
static TRAIT_IMPORTS: &[(&str, &str)] = &[
    // std::io
    ("read", "use std::io::Read;"),
    ("read_to_string", "use std::io::Read;"),
    ("read_exact", "use std::io::Read;"),
    ("write", "use std::io::Write;"),
    ("write_all", "use std::io::Write;"),
    ("write_fmt", "use std::io::Write;"),
    ("flush", "use std::io::Write;"),
    ("read_line", "use std::io::BufRead;"),
    ("lines", "use std::io::BufRead;"),
    ("seek", "use std::io::Seek;"),
    // std::fmt
    ("write!", "use std::fmt::Write;"),
    // std::str
    ("parse", "use std::str::FromStr;"),
    ("from_str", "use std::str::FromStr;"),
    // std::convert
    ("into", "use std::convert::Into;"),
    ("try_into", "use std::convert::TryInto;"),
    ("try_from", "use std::convert::TryFrom;"),
    ("as_ref", "use std::convert::AsRef;"),
    // std::ops
    ("deref", "use std::ops::Deref;"),
    // std::fmt::Display (for .to_string() on custom types)
    ("display", "use std::fmt::Display;"),
    // std::iter (usually in prelude, but just in case)
    ("collect", "use std::iter::Iterator;"),
];

/// E0425: Not in scope -> use statement to add.
///
/// Maps a type/module name to the `use` statement that brings it into scope.
/// Ported from FastEdit KNOWN_IMPORTS dict.
static KNOWN_ITEMS: &[(&str, &str)] = &[
    ("HashMap", "use std::collections::HashMap;"),
    ("BTreeMap", "use std::collections::BTreeMap;"),
    ("HashSet", "use std::collections::HashSet;"),
    ("BTreeSet", "use std::collections::BTreeSet;"),
    ("VecDeque", "use std::collections::VecDeque;"),
    ("BinaryHeap", "use std::collections::BinaryHeap;"),
    ("Arc", "use std::sync::Arc;"),
    ("Mutex", "use std::sync::Mutex;"),
    ("RwLock", "use std::sync::RwLock;"),
    ("Sender", "use std::sync::mpsc::Sender;"),
    ("Receiver", "use std::sync::mpsc::Receiver;"),
    ("Path", "use std::path::Path;"),
    ("PathBuf", "use std::path::PathBuf;"),
    ("File", "use std::fs::File;"),
    ("OpenOptions", "use std::fs::OpenOptions;"),
    ("Duration", "use std::time::Duration;"),
    ("Instant", "use std::time::Instant;"),
    ("Ordering", "use std::cmp::Ordering;"),
    ("Reverse", "use std::cmp::Reverse;"),
    ("thread", "use std::thread;"),
];

// ============================================================================
// Top-level dispatcher
// ============================================================================

/// Dispatch to the correct Rust analyzer based on error code.
///
/// Returns `Some(Diagnosis)` if an analyzer handled the error, `None` otherwise.
pub fn diagnose_rust(
    error: &ParsedError,
    source: &str,
    _tree: &Tree,
    _api_surface: Option<&()>,
) -> Option<Diagnosis> {
    let error_code = error.error_type.as_str();

    match error_code {
        "E0599" => analyze_e0599(error, source),
        "E0277" => analyze_e0277(error, source),
        "E0425" => analyze_e0425(error, source),
        "E0433" => analyze_e0433(error, source),
        "E0308" => analyze_e0308(error, source),
        _ => None,
    }
}

/// Check whether a given error code has a registered Rust analyzer.
pub fn has_analyzer(error_code: &str) -> bool {
    matches!(error_code, "E0599" | "E0277" | "E0425" | "E0433" | "E0308")
}

// ============================================================================
// Shared helper: inject a `use` statement into Rust source
// ============================================================================

/// Inject a `use` statement into Rust source code.
///
/// Places the new import after the last existing `use` line, or at the top
/// of the file if there are no existing imports. Returns `None` if the import
/// is already present.
fn inject_use_statement(source: &str, use_stmt: &str) -> Option<(String, usize)> {
    // Already present -- no edit needed
    if source.contains(use_stmt) {
        return None;
    }

    let lines: Vec<&str> = source.lines().collect();

    // Find the last `use` line
    let mut last_use_line: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("use ") || trimmed.starts_with("pub use ") {
            last_use_line = Some(i);
        }
    }

    // Insert after the last use, or at the top
    let insert_after_line = last_use_line.unwrap_or(0);
    let line_1indexed = insert_after_line + 1;

    let edit_kind = if last_use_line.is_some() {
        EditKind::InsertAfter
    } else {
        // No existing use statements: insert before line 1 (top of file)
        EditKind::InsertBefore
    };

    let new_text = if last_use_line.is_some() {
        use_stmt.to_string()
    } else {
        // At the top, add a blank line after the import for readability
        format!("{}\n", use_stmt)
    };

    // Compute result text for verification
    let mut result_lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    match edit_kind {
        EditKind::InsertAfter => {
            result_lines.insert(insert_after_line + 1, use_stmt.to_string());
        }
        EditKind::InsertBefore => {
            result_lines.insert(0, use_stmt.to_string());
            result_lines.insert(1, String::new());
        }
        _ => {}
    }

    let mut result = result_lines.join("\n");
    if source.ends_with('\n') && !result.ends_with('\n') {
        result.push('\n');
    }

    Some((new_text, line_1indexed))
}

// ============================================================================
// Analyzer 1: E0599 -- Method not found (missing trait import)
// ============================================================================

/// Analyze E0599: no method named `X` found.
///
/// This usually means a trait method is being called without the trait in scope.
/// The fix is to inject the appropriate `use` statement.
///
/// Handles:
/// - Direct method name lookup in TRAIT_IMPORTS
/// - Compiler hint extraction from children messages
/// - "cannot write"/"cannot read" fallback patterns
fn analyze_e0599(error: &ParsedError, source: &str) -> Option<Diagnosis> {
    let msg = &error.message;

    // Extract method name: "no method named `read_line` found"
    let method_re = Regex::new(r"no method named `(\w+)` found").ok()?;

    let use_stmt = if let Some(caps) = method_re.captures(msg) {
        let method = caps.get(1).unwrap().as_str();

        // Look up in TRAIT_IMPORTS table
        let from_table = TRAIT_IMPORTS
            .iter()
            .find(|(m, _)| *m == method)
            .map(|(_, stmt)| *stmt);

        if let Some(stmt) = from_table {
            if stmt.is_empty() {
                // Method is usually in prelude, no import needed
                return Some(Diagnosis {
                    language: "rust".to_string(),
                    error_code: "E0599".to_string(),
                    message: format!("Method `{}` not found -- may need turbofish syntax", method),
                    location: error.line.map(|l| FixLocation {
                        file: error.file.clone().unwrap_or_default(),
                        line: l,
                        column: error.column,
                    }),
                    confidence: FixConfidence::Low,
                    fix: None,
                });
            }
            stmt.to_string()
        } else {
            // Check compiler hint in children: "the following trait is implemented
            // but not in scope; perhaps add a `use` for it"
            let hint = extract_compiler_hint(&error.raw_text);
            if let Some(h) = hint {
                h
            } else {
                // No known fix
                return Some(Diagnosis {
                    language: "rust".to_string(),
                    error_code: "E0599".to_string(),
                    message: format!("Unknown method `{}` -- needs manual fix", method),
                    location: error.line.map(|l| FixLocation {
                        file: error.file.clone().unwrap_or_default(),
                        line: l,
                        column: error.column,
                    }),
                    confidence: FixConfidence::Low,
                    fix: None,
                });
            }
        }
    } else {
        // Handle "cannot write"/"cannot read" style messages
        if msg.to_lowercase().contains("cannot write") || msg.contains("Write") {
            "use std::io::Write;".to_string()
        } else if msg.to_lowercase().contains("cannot read") || msg.contains("Read") {
            "use std::io::Read;".to_string()
        } else {
            return None;
        }
    };

    // Build the fix
    let (new_text, insert_line) = inject_use_statement(source, &use_stmt)?;

    let edit_kind = if source.lines().any(|l| {
        let t = l.trim();
        t.starts_with("use ") || t.starts_with("pub use ")
    }) {
        EditKind::InsertAfter
    } else {
        EditKind::InsertBefore
    };

    Some(Diagnosis {
        language: "rust".to_string(),
        error_code: "E0599".to_string(),
        message: format!("Method not found -- missing trait import: {}", use_stmt),
        location: error.line.map(|l| FixLocation {
            file: error.file.clone().unwrap_or_default(),
            line: l,
            column: error.column,
        }),
        confidence: FixConfidence::High,
        fix: Some(Fix {
            description: format!("Add `{}`", use_stmt),
            edits: vec![TextEdit {
                line: insert_line,
                column: None,
                kind: edit_kind,
                new_text,
            }],
        }),
    })
}

/// Extract a compiler-suggested `use` statement from raw error text.
///
/// Looks for patterns like: `use std::io::BufRead;` in compiler hint messages.
fn extract_compiler_hint(raw_text: &str) -> Option<String> {
    let hint_re = Regex::new(r"`use ([\w:]+);`").ok()?;
    if let Some(caps) = hint_re.captures(raw_text) {
        let path = caps.get(1).unwrap().as_str();
        return Some(format!("use {};", path));
    }
    None
}

// ============================================================================
// Analyzer 2: E0277 -- Type mismatch (.copied(), as usize, etc.)
// ============================================================================

/// Analyze E0277: the trait bound `X: Y` is not satisfied.
///
/// Common patterns:
/// - "cannot be indexed by `u32`" -> cast index to `usize`
/// - "cannot be built from an iterator over elements of type `&T`" -> `.copied()`
fn analyze_e0277(error: &ParsedError, source: &str) -> Option<Diagnosis> {
    let msg = &error.message;

    // Pattern 1: "cannot be indexed by `u32`" -> cast to usize
    if msg.contains("cannot be indexed by") && msg.contains("u32") {
        if let Some(line_no) = error.line {
            let lines: Vec<&str> = source.lines().collect();
            if line_no > 0 && line_no <= lines.len() {
                let old_line = lines[line_no - 1];
                // Find pattern like `items.get(idx)` and add `as usize`
                let cast_re = Regex::new(r"\b(\w+)\s*\)").ok()?;
                let new_line = cast_re.replace(old_line, "$1 as usize)").to_string();

                if new_line != old_line {
                    return Some(Diagnosis {
                        language: "rust".to_string(),
                        error_code: "E0277".to_string(),
                        message: format!(
                            "Index type mismatch -- cast to usize at line {}",
                            line_no
                        ),
                        location: Some(FixLocation {
                            file: error.file.clone().unwrap_or_default(),
                            line: line_no,
                            column: error.column,
                        }),
                        confidence: FixConfidence::Medium,
                        fix: Some(Fix {
                            description: format!("Cast index to `usize` at line {}", line_no),
                            edits: vec![TextEdit {
                                line: line_no,
                                column: None,
                                kind: EditKind::ReplaceLine,
                                new_text: new_line,
                            }],
                        }),
                    });
                }
            }
        }
    }

    // Pattern 2: "cannot be built from an iterator over elements of type `&T`"
    // Fix: add .copied() before .collect()
    if msg.contains("cannot be built from an iterator") && msg.contains('&') {
        if let Some(line_no) = error.line {
            let lines: Vec<&str> = source.lines().collect();
            // Search nearby lines for .collect() without .copied()/.cloned()
            let search_start = line_no.saturating_sub(3).max(0);
            let search_end = (line_no + 3).min(lines.len());

            for (i, line) in lines[search_start..search_end].iter().enumerate() {
                if line.contains(".collect()")
                    && !line.contains(".copied()")
                    && !line.contains(".cloned()")
                {
                    let actual_line = search_start + i;
                    let new_line = line.replace(".collect()", ".copied().collect()");
                    return Some(Diagnosis {
                        language: "rust".to_string(),
                        error_code: "E0277".to_string(),
                        message: format!(
                            "Iterator yields references -- insert `.copied()` before `.collect()` at line {}",
                            actual_line + 1
                        ),
                        location: Some(FixLocation {
                            file: error.file.clone().unwrap_or_default(),
                            line: actual_line + 1,
                            column: None,
                        }),
                        confidence: FixConfidence::Medium,
                        fix: Some(Fix {
                            description: format!(
                                "Insert `.copied()` before `.collect()` at line {}",
                                actual_line + 1
                            ),
                            edits: vec![TextEdit {
                                line: actual_line + 1,
                                column: None,
                                kind: EditKind::ReplaceLine,
                                new_text: new_line,
                            }],
                        }),
                    });
                }
            }
        }
    }

    // Fallback: unrecognized E0277 pattern
    Some(Diagnosis {
        language: "rust".to_string(),
        error_code: "E0277".to_string(),
        message: format!("Type mismatch: {}", msg),
        location: error.line.map(|l| FixLocation {
            file: error.file.clone().unwrap_or_default(),
            line: l,
            column: error.column,
        }),
        confidence: FixConfidence::Low,
        fix: None,
    })
}

// ============================================================================
// Analyzer 3: E0425 -- Not found in scope (inject known use)
// ============================================================================

/// Analyze E0425: cannot find value/type `X` in this scope.
///
/// Looks up the missing name in the KNOWN_ITEMS table and injects the
/// appropriate `use` statement.
fn analyze_e0425(error: &ParsedError, source: &str) -> Option<Diagnosis> {
    let msg = &error.message;

    // Extract the unresolved name
    let name = extract_unresolved_name(msg)?;

    // Look up in KNOWN_ITEMS table
    let use_stmt = KNOWN_ITEMS
        .iter()
        .find(|(item, _)| *item == name)
        .map(|(_, stmt)| *stmt)?;

    // Build the fix
    let (new_text, insert_line) = inject_use_statement(source, use_stmt)?;

    let edit_kind = if source.lines().any(|l| {
        let t = l.trim();
        t.starts_with("use ") || t.starts_with("pub use ")
    }) {
        EditKind::InsertAfter
    } else {
        EditKind::InsertBefore
    };

    Some(Diagnosis {
        language: "rust".to_string(),
        error_code: "E0425".to_string(),
        message: format!("`{}` not in scope -- add `{}`", name, use_stmt),
        location: error.line.map(|l| FixLocation {
            file: error.file.clone().unwrap_or_default(),
            line: l,
            column: error.column,
        }),
        confidence: FixConfidence::High,
        fix: Some(Fix {
            description: format!("Add `{}`", use_stmt),
            edits: vec![TextEdit {
                line: insert_line,
                column: None,
                kind: edit_kind,
                new_text,
            }],
        }),
    })
}

/// Extract the unresolved name from an E0425 error message.
///
/// Handles patterns:
/// - "cannot find type `HashMap` in this scope"
/// - "cannot find value `thread` in this scope"
/// - "`HashMap` not found"
/// - "not found in this scope...`HashMap`"
fn extract_unresolved_name(msg: &str) -> Option<String> {
    // Try "cannot find type/value `X` in this scope"
    if let Some(caps) = Regex::new(r"cannot find (?:type|value) `(\w+)`")
        .ok()
        .and_then(|re| re.captures(msg))
    {
        return Some(caps.get(1).unwrap().as_str().to_string());
    }

    // Try "not found in this scope...`X`" or "`X` not found"
    if let Some(caps) = Regex::new(r"`(\w+)` not found")
        .ok()
        .and_then(|re| re.captures(msg))
    {
        return Some(caps.get(1).unwrap().as_str().to_string());
    }

    // Try "not found in this scope.*`X`"
    if let Some(caps) = Regex::new(r"not found in this scope.*`(\w+)`")
        .ok()
        .and_then(|re| re.captures(msg))
    {
        return Some(caps.get(1).unwrap().as_str().to_string());
    }

    None
}

// ============================================================================
// Analyzer 4: E0433 -- Failed to resolve (type-position missing use)
// ============================================================================

/// Analyze E0433: failed to resolve: use of undeclared type `X`.
///
/// This is the type-position counterpart to E0425 (value-position). `rustc`
/// emits E0433 when a type name like `HashMap` appears in type position
/// (e.g., `let m: HashMap<...> = HashMap::new()`) without the corresponding
/// `use` statement. E0433 is the MOST COMMON error for missing `use`
/// statements in practice.
///
/// The fix is identical to E0425: look up the type in KNOWN_ITEMS and inject
/// the appropriate `use` statement.
fn analyze_e0433(error: &ParsedError, source: &str) -> Option<Diagnosis> {
    let msg = &error.message;

    // Extract the type name from the error message.
    // Patterns:
    //   "failed to resolve: use of undeclared type `HashMap`"
    //   "failed to resolve: use of undeclared crate or module `HashMap`"
    //   "could not find `HashMap` in `std`"
    let type_name = extract_failed_resolve_name(msg)?;

    // Look up in KNOWN_ITEMS table
    let use_stmt_opt = KNOWN_ITEMS
        .iter()
        .find(|(item, _)| *item == type_name)
        .map(|(_, stmt)| *stmt);

    let use_stmt = match use_stmt_opt {
        Some(stmt) => stmt,
        None => {
            // Unknown type: return diagnosis without fix
            return Some(Diagnosis {
                language: "rust".to_string(),
                error_code: "E0433".to_string(),
                message: format!(
                    "Failed to resolve `{}` -- not in known items table",
                    type_name
                ),
                location: error.line.map(|l| FixLocation {
                    file: error.file.clone().unwrap_or_default(),
                    line: l,
                    column: error.column,
                }),
                confidence: FixConfidence::Low,
                fix: None,
            });
        }
    };

    // Build the fix
    let (new_text, insert_line) = inject_use_statement(source, use_stmt)?;

    let edit_kind = if source.lines().any(|l| {
        let t = l.trim();
        t.starts_with("use ") || t.starts_with("pub use ")
    }) {
        EditKind::InsertAfter
    } else {
        EditKind::InsertBefore
    };

    Some(Diagnosis {
        language: "rust".to_string(),
        error_code: "E0433".to_string(),
        message: format!("`{}` not resolved -- add `{}`", type_name, use_stmt),
        location: error.line.map(|l| FixLocation {
            file: error.file.clone().unwrap_or_default(),
            line: l,
            column: error.column,
        }),
        confidence: FixConfidence::High,
        fix: Some(Fix {
            description: format!("Add `{}`", use_stmt),
            edits: vec![TextEdit {
                line: insert_line,
                column: None,
                kind: edit_kind,
                new_text,
            }],
        }),
    })
}

/// Extract the unresolved type name from an E0433 error message.
///
/// Handles patterns:
/// - "failed to resolve: use of undeclared type `HashMap`"
/// - "failed to resolve: use of undeclared crate or module `HashMap`"
/// - "could not find `HashMap` in `std`"
/// - "use of undeclared type `HashMap`"
fn extract_failed_resolve_name(msg: &str) -> Option<String> {
    // Try "use of undeclared type `X`"
    if let Some(caps) = Regex::new(r"use of undeclared (?:type|crate or module) `(\w+)`")
        .ok()
        .and_then(|re| re.captures(msg))
    {
        return Some(caps.get(1).unwrap().as_str().to_string());
    }

    // Try "could not find `X` in"
    if let Some(caps) = Regex::new(r"could not find `(\w+)` in")
        .ok()
        .and_then(|re| re.captures(msg))
    {
        return Some(caps.get(1).unwrap().as_str().to_string());
    }

    // Try "failed to resolve.*`X`"
    if let Some(caps) = Regex::new(r"failed to resolve.*`(\w+)`")
        .ok()
        .and_then(|re| re.captures(msg))
    {
        return Some(caps.get(1).unwrap().as_str().to_string());
    }

    None
}

// ============================================================================
// Analyzer 5: E0308 -- Mismatched types (&str vs String, Option, etc.)
// ============================================================================

/// Analyze E0308: mismatched types.
///
/// Common patterns:
/// - Pattern 1/2: `.cloned()` or `.ok_or()` with &str/String mismatch
/// - Pattern 3: expected `String`, found `&str` -> add `.to_string()`
/// - Pattern 4: expected `&str`, found `String` -> add `&` borrowing
/// - Pattern 5: expected `T`, found `&T` (or vice versa) -> add `*` or `&`
fn analyze_e0308(error: &ParsedError, source: &str) -> Option<Diagnosis> {
    let msg = &error.message;
    // Combine message and raw_text for broader pattern matching
    let full_text = format!("{} {}", msg, error.raw_text);

    // Extract expected/found types from error message for Pattern 4/5
    let type_mismatch_re = Regex::new(r"expected `([^`]+)`, found `([^`]+)`").ok();
    let (expected_type, found_type) = type_mismatch_re
        .as_ref()
        .and_then(|re| re.captures(&full_text))
        .map(|caps| {
            (
                caps.get(1).unwrap().as_str().to_string(),
                caps.get(2).unwrap().as_str().to_string(),
            )
        })
        .unwrap_or_default();

    // Pattern 1/2: &str vs String conversion (specific subpatterns first)
    if full_text.contains("String") && full_text.contains("&str") {
        if let Some(line_no) = error.line {
            let lines: Vec<&str> = source.lines().collect();
            if line_no > 0 && line_no <= lines.len() {
                let old_line = lines[line_no - 1];
                let new_line;

                // Subpattern 1: .cloned() present -> replace with .map(|s| s.to_string())
                if old_line.contains(".cloned()") {
                    new_line = old_line.replace(".cloned()", ".map(|s| s.to_string())");
                } else if old_line.contains(".ok_or(") && !old_line.contains(".map(") {
                    // Subpattern 2: .ok_or() without prior .map() -> insert .map(|s| s.to_string())
                    new_line = old_line.replace(".ok_or(", ".map(|s| s.to_string()).ok_or(");
                } else if expected_type == "String" && found_type == "&str" {
                    // Pattern 3: generic "expected String, found &str"
                    // Append .to_string() to the rightmost expression before ; or )
                    new_line = apply_to_string_coercion(old_line);
                } else if expected_type == "&str" && found_type == "String" {
                    // Pattern 4: "expected &str, found String"
                    // Prepend & to the rhs expression
                    new_line = apply_borrow_coercion(old_line);
                } else {
                    // Cannot determine specific fix
                    new_line = old_line.to_string();
                }

                if new_line != old_line {
                    let description = if expected_type == "&str" {
                        format!("Borrow `String` as `&str` at line {}", line_no)
                    } else {
                        format!("Convert `&str` to `String` at line {}", line_no)
                    };
                    return Some(Diagnosis {
                        language: "rust".to_string(),
                        error_code: "E0308".to_string(),
                        message: format!(
                            "Type mismatch: expected `{}`, found `{}` at line {}",
                            expected_type, found_type, line_no
                        ),
                        location: Some(FixLocation {
                            file: error.file.clone().unwrap_or_default(),
                            line: line_no,
                            column: error.column,
                        }),
                        confidence: FixConfidence::Medium,
                        fix: Some(Fix {
                            description,
                            edits: vec![TextEdit {
                                line: line_no,
                                column: None,
                                kind: EditKind::ReplaceLine,
                                new_text: new_line,
                            }],
                        }),
                    });
                }
            }
        }
    }

    // Pattern 5: Reference mismatch (expected T, found &T or expected &T, found T)
    // This handles non-String/&str cases like i32/&i32, u64/&u64, etc.
    if !expected_type.is_empty() && !found_type.is_empty() {
        // Check: expected `T`, found `&T` -> dereference with *
        let needs_deref =
            found_type.starts_with('&') && expected_type == found_type.trim_start_matches('&');
        // Check: expected `&T`, found `T` -> add &
        let needs_ref =
            expected_type.starts_with('&') && found_type == expected_type.trim_start_matches('&');

        if needs_deref || needs_ref {
            if let Some(line_no) = error.line {
                let lines: Vec<&str> = source.lines().collect();
                if line_no > 0 && line_no <= lines.len() {
                    let old_line = lines[line_no - 1];
                    let new_line = if needs_deref {
                        apply_deref_coercion(old_line)
                    } else {
                        apply_borrow_coercion(old_line)
                    };

                    if new_line != old_line {
                        let description = if needs_deref {
                            format!(
                                "Dereference `{}` to `{}` at line {}",
                                found_type, expected_type, line_no
                            )
                        } else {
                            format!(
                                "Borrow `{}` as `{}` at line {}",
                                found_type, expected_type, line_no
                            )
                        };
                        return Some(Diagnosis {
                            language: "rust".to_string(),
                            error_code: "E0308".to_string(),
                            message: format!(
                                "Type mismatch: expected `{}`, found `{}` at line {}",
                                expected_type, found_type, line_no
                            ),
                            location: Some(FixLocation {
                                file: error.file.clone().unwrap_or_default(),
                                line: line_no,
                                column: error.column,
                            }),
                            confidence: FixConfidence::Medium,
                            fix: Some(Fix {
                                description,
                                edits: vec![TextEdit {
                                    line: line_no,
                                    column: None,
                                    kind: EditKind::ReplaceLine,
                                    new_text: new_line,
                                }],
                            }),
                        });
                    }
                }
            }
        }
    }

    // Fallback: unrecognized E0308 pattern
    Some(Diagnosis {
        language: "rust".to_string(),
        error_code: "E0308".to_string(),
        message: format!("Mismatched types: {}", msg),
        location: error.line.map(|l| FixLocation {
            file: error.file.clone().unwrap_or_default(),
            line: l,
            column: error.column,
        }),
        confidence: FixConfidence::Low,
        fix: None,
    })
}

/// Apply `.to_string()` coercion to a source line.
///
/// Heuristic: find the rightmost expression before `;` or `)` and append
/// `.to_string()`. Handles assignment RHS (`= EXPR;`), function arguments
/// (`fn(EXPR)`), and string literals (`"..."`).
fn apply_to_string_coercion(line: &str) -> String {
    let trimmed = line.trim();

    // Case 1: Assignment `= EXPR;` -- append .to_string() to EXPR
    if let Some(caps) = Regex::new(r"^(.*=\s*)(.+?)\s*;\s*$")
        .ok()
        .and_then(|re| re.captures(trimmed))
    {
        let prefix = caps.get(1).unwrap().as_str();
        let expr = caps.get(2).unwrap().as_str();
        // Don't double-apply if already has .to_string()
        if expr.ends_with(".to_string()") || expr.ends_with(".to_owned()") {
            return line.to_string();
        }
        let indent = &line[..line.len() - line.trim_start().len()];
        return format!("{}{}{}.to_string();", indent, prefix, expr);
    }

    // Case 2: Function call `fn(EXPR)` at end of line (possibly with ;)
    // Find the last identifier or string literal before ) or );
    if let Some(caps) = Regex::new(r"^(.*\(\s*)(\w+)(\s*\)\s*;?\s*)$")
        .ok()
        .and_then(|re| re.captures(trimmed))
    {
        let prefix = caps.get(1).unwrap().as_str();
        let arg = caps.get(2).unwrap().as_str();
        let suffix = caps.get(3).unwrap().as_str();
        let indent = &line[..line.len() - line.trim_start().len()];
        return format!("{}{}{}.to_string(){}", indent, prefix, arg, suffix);
    }

    // No recognized pattern -- return unchanged
    line.to_string()
}

/// Apply `&` borrow coercion to a source line.
///
/// Heuristic: find the RHS expression in `= EXPR;` or the argument in
/// `fn(EXPR)` and prepend `&`.
fn apply_borrow_coercion(line: &str) -> String {
    let trimmed = line.trim();

    // Case 1: Assignment `= EXPR;` -- prepend & to EXPR
    if let Some(caps) = Regex::new(r"^(.*=\s*)(.+?)\s*;\s*$")
        .ok()
        .and_then(|re| re.captures(trimmed))
    {
        let prefix = caps.get(1).unwrap().as_str();
        let expr = caps.get(2).unwrap().as_str();
        // Don't double-borrow
        if expr.starts_with('&') {
            return line.to_string();
        }
        let indent = &line[..line.len() - line.trim_start().len()];
        return format!("{}{}&{};", indent, prefix, expr);
    }

    // Case 2: Function call `fn(EXPR)` -- prepend & to the last arg
    if let Some(caps) = Regex::new(r"^(.*\(\s*)(\w+)(\s*\)\s*;?\s*)$")
        .ok()
        .and_then(|re| re.captures(trimmed))
    {
        let prefix = caps.get(1).unwrap().as_str();
        let arg = caps.get(2).unwrap().as_str();
        let suffix = caps.get(3).unwrap().as_str();
        // Don't double-borrow
        if arg.starts_with('&') {
            return line.to_string();
        }
        let indent = &line[..line.len() - line.trim_start().len()];
        return format!("{}{}&{}{}", indent, prefix, arg, suffix);
    }

    // No recognized pattern -- return unchanged
    line.to_string()
}

/// Apply `*` dereference coercion to a source line.
///
/// Heuristic: find the RHS expression in `= EXPR;` or the argument in
/// `fn(EXPR)` and prepend `*`.
fn apply_deref_coercion(line: &str) -> String {
    let trimmed = line.trim();

    // Case 1: Assignment `= EXPR;` -- prepend * to EXPR
    if let Some(caps) = Regex::new(r"^(.*=\s*)(.+?)\s*;\s*$")
        .ok()
        .and_then(|re| re.captures(trimmed))
    {
        let prefix = caps.get(1).unwrap().as_str();
        let expr = caps.get(2).unwrap().as_str();
        // Don't double-deref
        if expr.starts_with('*') {
            return line.to_string();
        }
        let indent = &line[..line.len() - line.trim_start().len()];
        return format!("{}{}*{};", indent, prefix, expr);
    }

    // Case 2: Function call `fn(EXPR)` -- prepend * to the last arg
    if let Some(caps) = Regex::new(r"^(.*\(\s*)(\w+)(\s*\)\s*;?\s*)$")
        .ok()
        .and_then(|re| re.captures(trimmed))
    {
        let prefix = caps.get(1).unwrap().as_str();
        let arg = caps.get(2).unwrap().as_str();
        let suffix = caps.get(3).unwrap().as_str();
        let indent = &line[..line.len() - line.trim_start().len()];
        return format!("{}{}*{}{}", indent, prefix, arg, suffix);
    }

    // No recognized pattern -- return unchanged
    line.to_string()
}

// ============================================================================
// Tests
// ============================================================================

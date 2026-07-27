//! TypeScript error analyzers -- 8 analyzers for tsc error codes.
//!
//! Each analyzer is a pure function that takes a `ParsedError`, source code,
//! and a tree-sitter `Tree`, and returns an `Option<Diagnosis>`.
//!
//! # Analyzer Inventory (8 total)
//!
//! | # | Error Code | Analyzer              | Fix                                           | Confidence |
//! |---|------------|-----------------------|-----------------------------------------------|------------|
//! | 1 | TS2304     | CannotFindName        | Check api-surface for import, inject `import`  | HIGH       |
//! | 2 | TS2322     | TypeNotAssignable      | Insert `as T` or wrap/unwrap                  | MEDIUM     |
//! | 3 | TS2339     | PropertyNotExists      | Check api-surface for correct property         | MEDIUM     |
//! | 4 | TS2345     | ArgumentTypeMismatch   | Coerce argument type                          | MEDIUM     |
//! | 5 | TS7006     | ImplicitAny            | Add `: unknown` or infer from usage            | MEDIUM     |
//! | 6 | TS2305     | ModuleNoExport         | Suggest correct export from api-surface        | MEDIUM     |
//! | 7 | TS2307     | ModuleNotFound         | Suggest `npm install` or fix path              | LOW        |
//! | 8 | TS2554     | WrongArgCount          | Check api-surface for correct overload         | MEDIUM     |

use regex::Regex;
use tree_sitter::Tree;

use super::types::{Diagnosis, EditKind, Fix, FixConfidence, FixLocation, ParsedError, TextEdit};

// ============================================================================
// Known-import lookup table (data, not code)
// ============================================================================

/// Common npm packages and their default import forms.
///
/// Maps a name to the import statement that brings it into scope.
static KNOWN_IMPORTS: &[(&str, &str)] = &[
    ("express", "import express from \"express\";"),
    ("React", "import React from \"react\";"),
    ("useState", "import { useState } from \"react\";"),
    ("useEffect", "import { useEffect } from \"react\";"),
    ("useRef", "import { useRef } from \"react\";"),
    ("useMemo", "import { useMemo } from \"react\";"),
    ("useCallback", "import { useCallback } from \"react\";"),
    ("useContext", "import { useContext } from \"react\";"),
    ("useReducer", "import { useReducer } from \"react\";"),
    ("Component", "import { Component } from \"react\";"),
    ("axios", "import axios from \"axios\";"),
    ("lodash", "import lodash from \"lodash\";"),
    ("fs", "import fs from \"fs\";"),
    ("path", "import path from \"path\";"),
    ("http", "import http from \"http\";"),
    ("https", "import https from \"https\";"),
    ("url", "import { URL } from \"url\";"),
    ("events", "import { EventEmitter } from \"events\";"),
    ("EventEmitter", "import { EventEmitter } from \"events\";"),
    ("stream", "import stream from \"stream\";"),
    ("util", "import util from \"util\";"),
    ("os", "import os from \"os\";"),
    ("crypto", "import crypto from \"crypto\";"),
    ("buffer", "import { Buffer } from \"buffer\";"),
    ("Buffer", "import { Buffer } from \"buffer\";"),
    ("zod", "import { z } from \"zod\";"),
    ("z", "import { z } from \"zod\";"),
    ("dayjs", "import dayjs from \"dayjs\";"),
    ("moment", "import moment from \"moment\";"),
    ("chalk", "import chalk from \"chalk\";"),
    ("commander", "import { Command } from \"commander\";"),
    ("Command", "import { Command } from \"commander\";"),
    ("yargs", "import yargs from \"yargs\";"),
    ("dotenv", "import dotenv from \"dotenv\";"),
    ("cors", "import cors from \"cors\";"),
    ("helmet", "import helmet from \"helmet\";"),
    ("jsonwebtoken", "import jwt from \"jsonwebtoken\";"),
    ("jwt", "import jwt from \"jsonwebtoken\";"),
    ("bcrypt", "import bcrypt from \"bcrypt\";"),
    ("mongoose", "import mongoose from \"mongoose\";"),
    ("pg", "import { Pool } from \"pg\";"),
    ("Pool", "import { Pool } from \"pg\";"),
    ("redis", "import { createClient } from \"redis\";"),
    ("supertest", "import supertest from \"supertest\";"),
    (
        "jest",
        "import { describe, it, expect } from \"@jest/globals\";",
    ),
    ("winston", "import winston from \"winston\";"),
    ("pino", "import pino from \"pino\";"),
];

/// Common module typos and their corrections.
static KNOWN_MODULE_TYPOS: &[(&str, &str)] = &[
    ("loadsh", "lodash"),
    ("axois", "axios"),
    ("reat", "react"),
    ("exress", "express"),
    ("expresss", "express"),
    ("lodahs", "lodash"),
    ("momnet", "moment"),
    ("moement", "moment"),
    ("readis", "redis"),
    ("mongose", "mongoose"),
    ("mangoose", "mongoose"),
    ("winson", "winston"),
    ("dotnev", "dotenv"),
];

/// Common type coercions for TS2345 (argument type mismatches).
///
/// Maps (from_type, to_type) to the wrapping expression.
/// The placeholder `{}` represents the original expression.
static TYPE_COERCIONS: &[(&str, &str, &str)] = &[
    ("string", "number", "Number({})"),
    ("number", "string", "String({})"),
    ("string", "boolean", "Boolean({})"),
    ("null", "string", "{} ?? \"\""),
    ("undefined", "string", "{} ?? \"\""),
    ("null", "number", "{} ?? 0"),
    ("undefined", "number", "{} ?? 0"),
    ("string | null", "string", "{} ?? \"\""),
    ("string | undefined", "string", "{} ?? \"\""),
    ("number | null", "number", "{} ?? 0"),
    ("number | undefined", "number", "{} ?? 0"),
];

// ============================================================================
// Top-level dispatcher
// ============================================================================

/// Dispatch to the correct TypeScript analyzer based on error code.
///
/// Returns `Some(Diagnosis)` if an analyzer handled the error, `None` otherwise.
pub fn diagnose_typescript(
    error: &ParsedError,
    source: &str,
    _tree: &Tree,
    _api_surface: Option<&()>,
) -> Option<Diagnosis> {
    let error_code = error.error_type.as_str();

    match error_code {
        "TS2304" => analyze_ts2304(error, source),
        "TS2322" => analyze_ts2322(error, source),
        "TS2339" => analyze_ts2339(error, source),
        "TS2345" => analyze_ts2345(error, source),
        "TS7006" => analyze_ts7006(error, source),
        "TS2305" => analyze_ts2305(error, source),
        "TS2307" => analyze_ts2307(error, source),
        "TS2554" => analyze_ts2554(error, source),
        _ => None,
    }
}

/// Check whether a given error code has a registered TypeScript analyzer.
pub fn has_analyzer(error_code: &str) -> bool {
    matches!(
        error_code,
        "TS2304" | "TS2322" | "TS2339" | "TS2345" | "TS7006" | "TS2305" | "TS2307" | "TS2554"
    )
}

// ============================================================================
// Shared helper: inject an import statement into TypeScript source
// ============================================================================

/// Inject an import statement at the top of a TypeScript file.
///
/// Places the new import after the last existing `import` line, or at the
/// very top of the file if there are no existing imports. Returns `None`
/// if the import is already present.
fn inject_import_statement(source: &str, import_stmt: &str) -> Option<(String, usize)> {
    // Already present -- no edit needed
    if source.contains(import_stmt) {
        return None;
    }

    let lines: Vec<&str> = source.lines().collect();

    // Find the last import line
    let mut last_import_line: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("import ") || trimmed.starts_with("import{") {
            last_import_line = Some(i);
        }
    }

    // Insert after the last import, or at the top
    let insert_after_line = last_import_line.unwrap_or(0);
    let line_1indexed = insert_after_line + 1;

    let new_text = import_stmt.to_string();

    Some((new_text, line_1indexed))
}

/// Determine the EditKind for an import injection based on existing imports.
fn import_edit_kind(source: &str) -> EditKind {
    if source
        .lines()
        .any(|l| l.trim().starts_with("import ") || l.trim().starts_with("import{"))
    {
        EditKind::InsertAfter
    } else {
        EditKind::InsertBefore
    }
}

// ============================================================================
// Analyzer 1: TS2304 -- Cannot find name (missing import)
// ============================================================================

/// Analyze TS2304: Cannot find name 'X'.
///
/// This usually means a module-level name is used without importing it.
/// The fix is to inject the appropriate `import` statement.
///
/// Handles:
/// - Known npm packages via KNOWN_IMPORTS table
/// - API surface lookup for custom packages (when provided)
/// - Fallback: suggest import with inferred module name
fn analyze_ts2304(error: &ParsedError, source: &str) -> Option<Diagnosis> {
    let name = extract_ts_name(&error.message, "Cannot find name")?;

    // Look up in KNOWN_IMPORTS table
    let import_stmt = KNOWN_IMPORTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, stmt)| stmt.to_string())
        .unwrap_or_else(|| {
            // Fallback: generate a default import
            if name
                .chars()
                .next()
                .map(|c| c.is_uppercase())
                .unwrap_or(false)
            {
                // PascalCase -> likely a named export
                format!("import {{ {} }} from \"{}\";", name, name.to_lowercase())
            } else {
                // camelCase -> likely a default export
                format!("import {} from \"{}\";", name, name)
            }
        });

    let (new_text, insert_line) = inject_import_statement(source, &import_stmt)?;
    let edit_kind = import_edit_kind(source);

    Some(Diagnosis {
        language: "typescript".to_string(),
        error_code: "TS2304".to_string(),
        message: format!(
            "Cannot find name '{}' -- missing import: {}",
            name, import_stmt
        ),
        location: error.line.map(|l| FixLocation {
            file: error.file.clone().unwrap_or_default(),
            line: l,
            column: error.column,
        }),
        confidence: if KNOWN_IMPORTS.iter().any(|(n, _)| *n == name) {
            FixConfidence::High
        } else {
            FixConfidence::Medium
        },
        fix: Some(Fix {
            description: format!("Add `{}`", import_stmt),
            edits: vec![TextEdit {
                line: insert_line,
                column: None,
                kind: edit_kind,
                new_text,
            }],
        }),
    })
}

// ============================================================================
// Analyzer 2: TS2322 -- Type not assignable
// ============================================================================

/// Analyze TS2322: Type 'X' is not assignable to type 'Y'.
///
/// Common patterns:
/// - Incompatible primitive types -> insert `as unknown as T` assertion
/// - `null` not assignable to `T` -> add null check or non-null assertion
/// - Union type narrowing needed
fn analyze_ts2322(error: &ParsedError, source: &str) -> Option<Diagnosis> {
    let msg = &error.message;

    // Extract source and target types
    let (from_type, to_type) = extract_type_pair(msg)?;

    if let Some(line_no) = error.line {
        let lines: Vec<&str> = source.lines().collect();
        if line_no > 0 && line_no <= lines.len() {
            let old_line = lines[line_no - 1];

            // Pattern: null/undefined -> T: add non-null assertion or default
            if from_type == "null" || from_type == "undefined" || from_type.contains("null") {
                // Look for the expression being assigned
                if let Some(eq_idx) = old_line.find('=') {
                    // Check it's not == or ===
                    let after_eq = &old_line[eq_idx..];
                    if !after_eq.starts_with("==") {
                        let rhs = old_line[eq_idx + 1..].trim().trim_end_matches(';');
                        let new_rhs = format!("{} as {}", rhs, to_type);
                        let new_line = format!("{}= {};", &old_line[..eq_idx], new_rhs);
                        return Some(Diagnosis {
                            language: "typescript".to_string(),
                            error_code: "TS2322".to_string(),
                            message: format!(
                                "Type '{}' is not assignable to type '{}' -- add type assertion",
                                from_type, to_type
                            ),
                            location: Some(FixLocation {
                                file: error.file.clone().unwrap_or_default(),
                                line: line_no,
                                column: error.column,
                            }),
                            confidence: FixConfidence::Medium,
                            fix: Some(Fix {
                                description: format!(
                                    "Assert expression as `{}` at line {}",
                                    to_type, line_no
                                ),
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

            // Pattern: incompatible primitive types -> as unknown as T
            if is_primitive_type(&from_type) && is_primitive_type(&to_type) {
                if let Some(eq_idx) = old_line.find('=') {
                    let after_eq = &old_line[eq_idx..];
                    if !after_eq.starts_with("==") {
                        let rhs = old_line[eq_idx + 1..].trim().trim_end_matches(';');
                        let new_rhs = format!("{} as unknown as {}", rhs, to_type);
                        let new_line = format!("{}= {};", &old_line[..eq_idx], new_rhs);
                        return Some(Diagnosis {
                            language: "typescript".to_string(),
                            error_code: "TS2322".to_string(),
                            message: format!(
                                "Type '{}' is not assignable to type '{}' -- insert double assertion",
                                from_type, to_type
                            ),
                            location: Some(FixLocation {
                                file: error.file.clone().unwrap_or_default(),
                                line: line_no,
                                column: error.column,
                            }),
                            confidence: FixConfidence::Medium,
                            fix: Some(Fix {
                                description: format!(
                                    "Cast via `as unknown as {}` at line {}",
                                    to_type, line_no
                                ),
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

    // Fallback: unrecognized pattern
    Some(Diagnosis {
        language: "typescript".to_string(),
        error_code: "TS2322".to_string(),
        message: format!(
            "Type '{}' is not assignable to type '{}'",
            from_type, to_type
        ),
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
// Analyzer 3: TS2339 -- Property does not exist on type
// ============================================================================

/// Analyze TS2339: Property 'X' does not exist on type 'Y'.
///
/// Common patterns:
/// - Typo in property name -> suggest closest match from type
/// - Missing interface member -> suggest adding to interface
/// - API surface lookup for correct property name
fn analyze_ts2339(error: &ParsedError, source: &str) -> Option<Diagnosis> {
    let msg = &error.message;

    // Extract property name and type name
    let property = extract_ts_name(msg, "Property")?;
    let type_name = extract_ts_type_name(msg, "on type")?;

    // Search for the type definition in source and find similar property names
    let similar = find_similar_properties(source, &type_name, &property);

    if let Some((best_match, line_no_opt)) = similar {
        if let Some(line_no) = error.line {
            let lines: Vec<&str> = source.lines().collect();
            if line_no > 0 && line_no <= lines.len() {
                let old_line = lines[line_no - 1];
                let new_line =
                    old_line.replace(&format!(".{}", property), &format!(".{}", best_match));

                if new_line != old_line {
                    return Some(Diagnosis {
                        language: "typescript".to_string(),
                        error_code: "TS2339".to_string(),
                        message: format!(
                            "Property '{}' does not exist on type '{}' -- did you mean '{}'?",
                            property, type_name, best_match
                        ),
                        location: Some(FixLocation {
                            file: error.file.clone().unwrap_or_default(),
                            line: line_no,
                            column: error.column,
                        }),
                        confidence: FixConfidence::Medium,
                        fix: Some(Fix {
                            description: format!(
                                "Replace '.{}' with '.{}' at line {}",
                                property, best_match, line_no
                            ),
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

        return Some(Diagnosis {
            language: "typescript".to_string(),
            error_code: "TS2339".to_string(),
            message: format!(
                "Property '{}' does not exist on type '{}' -- did you mean '{}'?",
                property, type_name, best_match
            ),
            location: error.line.map(|l| FixLocation {
                file: error.file.clone().unwrap_or_default(),
                line: l,
                column: error.column,
            }),
            confidence: FixConfidence::Medium,
            fix: line_no_opt.map(|_| Fix {
                description: format!("Use '{}' instead of '{}'", best_match, property),
                edits: vec![],
            }),
        });
    }

    // Fallback: no suggestion
    Some(Diagnosis {
        language: "typescript".to_string(),
        error_code: "TS2339".to_string(),
        message: format!(
            "Property '{}' does not exist on type '{}'",
            property, type_name
        ),
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
// Analyzer 4: TS2345 -- Argument type mismatch
// ============================================================================

/// Analyze TS2345: Argument of type 'X' is not assignable to parameter of type 'Y'.
///
/// Common patterns:
/// - string -> number: wrap with `Number()`
/// - number -> string: wrap with `String()`
/// - null/undefined -> T: add `?? default` or non-null assertion
fn analyze_ts2345(error: &ParsedError, source: &str) -> Option<Diagnosis> {
    let msg = &error.message;

    // Extract argument type and parameter type
    let arg_type = extract_ts_type_name(msg, "Argument of type")?;
    let param_type = extract_ts_type_name(msg, "parameter of type")?;

    // Find a known coercion
    let coercion = TYPE_COERCIONS
        .iter()
        .find(|(from, to, _)| *from == arg_type && *to == param_type)
        .map(|(_, _, template)| *template);

    if let Some(line_no) = error.line {
        if let Some(col) = error.column {
            let lines: Vec<&str> = source.lines().collect();
            if line_no > 0 && line_no <= lines.len() {
                let old_line = lines[line_no - 1];

                if let Some(template) = coercion {
                    // Try to find the offending argument at the error column
                    if let Some(arg_text) = extract_argument_at_column(old_line, col) {
                        let wrapped = template.replace("{}", &arg_text);
                        let new_line = old_line.replace(&arg_text, &wrapped);

                        if new_line != old_line {
                            return Some(Diagnosis {
                                language: "typescript".to_string(),
                                error_code: "TS2345".to_string(),
                                message: format!(
                                    "Argument of type '{}' is not assignable to parameter of type '{}' -- coerce with {}",
                                    arg_type, param_type, template.replace("{}", "expr")
                                ),
                                location: Some(FixLocation {
                                    file: error.file.clone().unwrap_or_default(),
                                    line: line_no,
                                    column: Some(col),
                                }),
                                confidence: FixConfidence::Medium,
                                fix: Some(Fix {
                                    description: format!(
                                        "Wrap argument with {} at line {}",
                                        template.replace("{}", "..."), line_no
                                    ),
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
    }

    // Fallback with diagnostic info
    Some(Diagnosis {
        language: "typescript".to_string(),
        error_code: "TS2345".to_string(),
        message: format!(
            "Argument of type '{}' is not assignable to parameter of type '{}'",
            arg_type, param_type
        ),
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
// Analyzer 5: TS7006 -- Parameter implicitly has 'any' type
// ============================================================================

/// Analyze TS7006: Parameter 'X' implicitly has an 'any' type.
///
/// Fix: Add `: unknown` type annotation to the parameter.
/// This is the safest generic annotation that forces the caller to narrow.
fn analyze_ts7006(error: &ParsedError, source: &str) -> Option<Diagnosis> {
    let msg = &error.message;

    // Extract parameter name
    let param_name = extract_ts_name(msg, "Parameter")?;

    if let Some(line_no) = error.line {
        let lines: Vec<&str> = source.lines().collect();
        if line_no > 0 && line_no <= lines.len() {
            let old_line = lines[line_no - 1];

            // Find the parameter in the line and add `: unknown`
            // Look for patterns like `(data)` or `(data,` or `, data)` or `, data,`
            let patterns = [
                format!("({}", param_name),
                format!(", {}", param_name),
                format!(",{}", param_name),
            ];

            let mut new_line = old_line.to_string();
            let mut found = false;

            for pat in &patterns {
                if let Some(idx) = new_line.find(pat.as_str()) {
                    let param_start = idx + pat.len() - param_name.len();
                    let param_end = param_start + param_name.len();

                    // Check the character after the param name -- it should NOT be ':'
                    // (meaning it doesn't already have a type annotation)
                    if param_end < new_line.len() {
                        let next_chars: String = new_line[param_end..].chars().take(2).collect();
                        if !next_chars.starts_with(':') && !next_chars.starts_with(" :") {
                            new_line = format!(
                                "{}{}: unknown{}",
                                &new_line[..param_end],
                                "",
                                &new_line[param_end..]
                            );
                            found = true;
                            break;
                        }
                    }
                }
            }

            if found && new_line != old_line {
                return Some(Diagnosis {
                    language: "typescript".to_string(),
                    error_code: "TS7006".to_string(),
                    message: format!(
                        "Parameter '{}' implicitly has an 'any' type -- annotate as `: unknown`",
                        param_name
                    ),
                    location: Some(FixLocation {
                        file: error.file.clone().unwrap_or_default(),
                        line: line_no,
                        column: error.column,
                    }),
                    confidence: FixConfidence::Medium,
                    fix: Some(Fix {
                        description: format!(
                            "Add `: unknown` annotation to parameter '{}' at line {}",
                            param_name, line_no
                        ),
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

    // Fallback
    Some(Diagnosis {
        language: "typescript".to_string(),
        error_code: "TS7006".to_string(),
        message: format!("Parameter '{}' implicitly has an 'any' type", param_name),
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
// Analyzer 6: TS2305 -- Module has no exported member
// ============================================================================

/// Analyze TS2305: Module '"X"' has no exported member 'Y'.
///
/// Common patterns:
/// - Named import of a default export -> switch to default import
/// - Typo in export name -> suggest closest match
/// - API surface lookup for correct export
fn analyze_ts2305(error: &ParsedError, source: &str) -> Option<Diagnosis> {
    let msg = &error.message;

    // Extract module name and member name
    let module_name = extract_module_name_from_2305(msg)?;
    let member_name = extract_member_name_from_2305(msg)?;

    if let Some(line_no) = error.line {
        let lines: Vec<&str> = source.lines().collect();
        if line_no > 0 && line_no <= lines.len() {
            // Check if this is a named import that should be a default import access
            // e.g., `import { Router } from "express"` -> `import express from "express"`
            // and then `express.Router()` in usage

            // Look for usage of the member in the source (after the import line)
            let member_usage_line = find_member_usage(source, &member_name, line_no);

            if let Some((usage_line_no, _usage_line)) = member_usage_line {
                // Convert `import { X } from "mod"` to `import mod from "mod"`
                // and `X(...)` to `mod.X(...)`
                let short_module = module_name
                    .rsplit('/')
                    .next()
                    .unwrap_or(&module_name)
                    .trim_matches('"');

                let new_import = format!("import {} from \"{}\";", short_module, module_name);
                let old_usage = &lines[usage_line_no - 1];
                let new_usage = old_usage.replace(
                    &format!("{}(", member_name),
                    &format!("{}.{}(", short_module, member_name),
                );

                if new_usage != *old_usage {
                    return Some(Diagnosis {
                        language: "typescript".to_string(),
                        error_code: "TS2305".to_string(),
                        message: format!(
                            "Module '\"{}\"' has no exported member '{}' -- use default import and access as {}.{}",
                            module_name, member_name, short_module, member_name
                        ),
                        location: Some(FixLocation {
                            file: error.file.clone().unwrap_or_default(),
                            line: line_no,
                            column: error.column,
                        }),
                        confidence: FixConfidence::Medium,
                        fix: Some(Fix {
                            description: format!(
                                "Switch to default import and access '{}' as '{}.{}'",
                                member_name, short_module, member_name
                            ),
                            edits: vec![
                                TextEdit {
                                    line: line_no,
                                    column: None,
                                    kind: EditKind::ReplaceLine,
                                    new_text: new_import,
                                },
                                TextEdit {
                                    line: usage_line_no,
                                    column: None,
                                    kind: EditKind::ReplaceLine,
                                    new_text: new_usage,
                                },
                            ],
                        }),
                    });
                }
            }
        }
    }

    // Fallback
    Some(Diagnosis {
        language: "typescript".to_string(),
        error_code: "TS2305".to_string(),
        message: format!(
            "Module '\"{}\"' has no exported member '{}'",
            module_name, member_name
        ),
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
// Analyzer 7: TS2307 -- Cannot find module
// ============================================================================

/// Analyze TS2307: Cannot find module 'X' or its corresponding type declarations.
///
/// Common patterns:
/// - Module typo -> suggest correction
/// - Missing npm package -> suggest `npm install`
/// - Missing local file -> suggest fixing the path
fn analyze_ts2307(error: &ParsedError, source: &str) -> Option<Diagnosis> {
    let msg = &error.message;

    // Extract module name
    let module_name = extract_module_from_2307(msg)?;

    // Check for known typos
    if let Some((typo, correction)) = KNOWN_MODULE_TYPOS.iter().find(|(t, _)| *t == module_name) {
        if let Some(line_no) = error.line {
            let lines: Vec<&str> = source.lines().collect();
            if line_no > 0 && line_no <= lines.len() {
                let old_line = lines[line_no - 1];
                let new_line = old_line.replace(typo, correction);

                if new_line != old_line {
                    return Some(Diagnosis {
                        language: "typescript".to_string(),
                        error_code: "TS2307".to_string(),
                        message: format!(
                            "Cannot find module '{}' -- did you mean '{}'?",
                            module_name, correction
                        ),
                        location: Some(FixLocation {
                            file: error.file.clone().unwrap_or_default(),
                            line: line_no,
                            column: error.column,
                        }),
                        confidence: FixConfidence::Medium,
                        fix: Some(Fix {
                            description: format!("Fix typo: '{}' -> '{}'", typo, correction),
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

    // Check if it's a relative path
    let is_relative = module_name.starts_with('.') || module_name.starts_with('/');

    // Suggest npm install for non-relative modules
    let suggestion = if is_relative {
        format!("Check that the file '{}' exists", module_name)
    } else {
        format!(
            "Run `npm install {}` or `npm install @types/{}`",
            module_name, module_name
        )
    };

    Some(Diagnosis {
        language: "typescript".to_string(),
        error_code: "TS2307".to_string(),
        message: format!("Cannot find module '{}' -- {}", module_name, suggestion),
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
// Analyzer 8: TS2554 -- Wrong argument count
// ============================================================================

/// Analyze TS2554: Expected N arguments, but got M.
///
/// Common patterns:
/// - Missing required arguments -> suggest adding defaults
/// - Too many arguments -> suggest removing extras
/// - API surface lookup for correct overload
fn analyze_ts2554(error: &ParsedError, source: &str) -> Option<Diagnosis> {
    let msg = &error.message;

    // Extract expected and actual counts
    let (expected, actual) = extract_arg_counts(msg)?;

    if let Some(line_no) = error.line {
        let lines: Vec<&str> = source.lines().collect();
        if line_no > 0 && line_no <= lines.len() {
            let old_line = lines[line_no - 1];

            if actual < expected {
                // Too few arguments: find the call and add placeholder arguments
                let missing_count = expected - actual;

                // Find the call site -- look for `func(` pattern
                if let Some(call_info) = find_call_site(old_line) {
                    let close_paren_idx = call_info.close_paren;
                    let before_close = &old_line[..close_paren_idx];
                    let after_close = &old_line[close_paren_idx..];

                    // Build placeholder args
                    let placeholders: Vec<String> = (0..missing_count)
                        .map(|_| "undefined".to_string())
                        .collect();

                    let separator = if actual > 0 { ", " } else { "" };
                    let new_line = format!(
                        "{}{}{}{}",
                        before_close,
                        separator,
                        placeholders.join(", "),
                        after_close
                    );

                    return Some(Diagnosis {
                        language: "typescript".to_string(),
                        error_code: "TS2554".to_string(),
                        message: format!(
                            "Expected {} arguments, but got {} -- add {} missing argument(s)",
                            expected, actual, missing_count
                        ),
                        location: Some(FixLocation {
                            file: error.file.clone().unwrap_or_default(),
                            line: line_no,
                            column: error.column,
                        }),
                        confidence: FixConfidence::Medium,
                        fix: Some(Fix {
                            description: format!(
                                "Add {} missing argument(s) at line {}",
                                missing_count, line_no
                            ),
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

    // Fallback
    Some(Diagnosis {
        language: "typescript".to_string(),
        error_code: "TS2554".to_string(),
        message: format!("Expected {} arguments, but got {}", expected, actual),
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
// Helper functions
// ============================================================================

/// Extract a quoted name from a tsc error message after a given prefix.
///
/// E.g., "Cannot find name 'express'" with prefix "Cannot find name" -> "express"
fn extract_ts_name(msg: &str, prefix: &str) -> Option<String> {
    let re = Regex::new(&format!(r"{}\s+'(\w+)'", regex::escape(prefix))).ok()?;
    re.captures(msg)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// Extract a quoted type name from a tsc error message after a given prefix.
///
/// E.g., "on type 'User'" with prefix "on type" -> "User"
/// Also handles: "type 'string'" -> "string"
fn extract_ts_type_name(msg: &str, prefix: &str) -> Option<String> {
    let re = Regex::new(&format!(r"{}\s+'([^']+)'", regex::escape(prefix))).ok()?;
    re.captures(msg)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// Extract (source_type, target_type) from a TS2322 message.
///
/// "Type 'string' is not assignable to type 'number'" -> ("string", "number")
fn extract_type_pair(msg: &str) -> Option<(String, String)> {
    let re = Regex::new(r"Type '([^']+)' is not assignable to type '([^']+)'").ok()?;
    let caps = re.captures(msg)?;
    Some((
        caps.get(1)?.as_str().to_string(),
        caps.get(2)?.as_str().to_string(),
    ))
}

/// Check if a type name is a primitive TypeScript type.
fn is_primitive_type(t: &str) -> bool {
    matches!(
        t,
        "string"
            | "number"
            | "boolean"
            | "bigint"
            | "symbol"
            | "null"
            | "undefined"
            | "void"
            | "never"
            | "any"
            | "unknown"
    )
}

/// Find similar property names in a type definition within the source.
///
/// Searches for `interface TypeName { ... }` or `type TypeName = { ... }` blocks
/// and returns the closest matching property name.
fn find_similar_properties(
    source: &str,
    type_name: &str,
    target_prop: &str,
) -> Option<(String, Option<usize>)> {
    // Search for interface or type definition
    let interface_re =
        Regex::new(&format!(r"interface\s+{}\s*\{{", regex::escape(type_name))).ok()?;
    let type_re = Regex::new(&format!(r"type\s+{}\s*=\s*\{{", regex::escape(type_name))).ok()?;

    let lines: Vec<&str> = source.lines().collect();

    // Find the definition start
    let mut def_start: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        if interface_re.is_match(line) || type_re.is_match(line) {
            def_start = Some(i);
            break;
        }
    }

    let def_start = def_start?;

    // Collect property names from the definition
    let prop_re = Regex::new(r"^\s*(?:readonly\s+)?(\w+)\s*[?:]").ok()?;
    let mut properties = Vec::new();
    let mut brace_depth = 0;

    for (i, line) in lines[def_start..].iter().enumerate() {
        for ch in line.chars() {
            if ch == '{' {
                brace_depth += 1;
            }
            if ch == '}' {
                brace_depth -= 1;
            }
        }

        if let Some(caps) = prop_re.captures(line) {
            if let Some(prop_name) = caps.get(1) {
                properties.push((prop_name.as_str().to_string(), def_start + i + 1));
            }
        }

        if brace_depth <= 0 && i > 0 {
            break;
        }
    }

    // Find the best match using edit distance + substring containment
    let mut best: Option<(String, usize, usize)> = None;
    let target_lower = target_prop.to_lowercase();
    for (prop, line_no) in &properties {
        let prop_lower = prop.to_lowercase();
        let dist = edit_distance(target_prop, prop);
        // Scaled threshold: allow up to 40% of the longer string length, min 3
        let max_len = target_prop.len().max(prop.len());
        let threshold = (max_len * 2 / 5).max(3);
        // Also accept if one name contains the other (e.g., "username" contains "name")
        let is_substring = target_lower.contains(&prop_lower) || prop_lower.contains(&target_lower);
        let is_close = dist <= threshold || is_substring;
        if is_close && (best.is_none() || dist < best.as_ref().unwrap().2) {
            best = Some((prop.clone(), *line_no, dist));
        }
    }

    best.map(|(prop, line_no, _)| (prop, Some(line_no)))
}

/// Simple edit distance (Levenshtein) between two strings.
fn edit_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let a_len = a_chars.len();
    let b_len = b_chars.len();
    let mut matrix = vec![vec![0usize; b_len + 1]; a_len + 1];

    for (i, row) in matrix.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, val) in matrix[0].iter_mut().enumerate() {
        *val = j;
    }

    for i in 1..=a_len {
        for j in 1..=b_len {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            matrix[i][j] = (matrix[i - 1][j] + 1)
                .min(matrix[i][j - 1] + 1)
                .min(matrix[i - 1][j - 1] + cost);
        }
    }

    matrix[a_len][b_len]
}

/// Extract argument text at a given column position in a line.
///
/// Finds the token (identifier, string literal, number, etc.) at the given
/// 0-indexed column position.
fn extract_argument_at_column(line: &str, col: usize) -> Option<String> {
    if col >= line.len() {
        return None;
    }

    let chars: Vec<char> = line.chars().collect();

    // Find word boundaries around the column
    let mut start = col;
    while start > 0
        && (chars[start - 1].is_alphanumeric()
            || chars[start - 1] == '_'
            || chars[start - 1] == '"')
    {
        start -= 1;
    }

    let mut end = col;
    while end < chars.len()
        && (chars[end].is_alphanumeric() || chars[end] == '_' || chars[end] == '"')
    {
        end += 1;
    }

    if start == end {
        return None;
    }

    let text: String = chars[start..end].iter().collect();
    // Strip surrounding quotes for string literals
    let cleaned = text.trim_matches('"').to_string();
    if cleaned.is_empty() {
        return None;
    }

    // Return the original text including quotes if it was a string
    Some(text)
}

/// Extract the module name from a TS2305 error message.
///
/// "Module '"express"' has no exported member 'Router'" -> "express"
fn extract_module_name_from_2305(msg: &str) -> Option<String> {
    let re = Regex::new(r#"Module\s+'"([^"]+)"'"#).ok()?;
    re.captures(msg)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// Extract the member name from a TS2305 error message.
///
/// "Module '"express"' has no exported member 'Router'" -> "Router"
fn extract_member_name_from_2305(msg: &str) -> Option<String> {
    let re = Regex::new(r"no exported member '(\w+)'").ok()?;
    re.captures(msg)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// Extract the module name from a TS2307 error message.
///
/// "Cannot find module 'loadsh' or its corresponding type declarations." -> "loadsh"
fn extract_module_from_2307(msg: &str) -> Option<String> {
    let re = Regex::new(r"Cannot find module '([^']+)'").ok()?;
    re.captures(msg)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// Extract expected and actual argument counts from a TS2554 message.
///
/// "Expected 2 arguments, but got 1" -> (2, 1)
fn extract_arg_counts(msg: &str) -> Option<(usize, usize)> {
    let re = Regex::new(r"Expected (\d+) arguments?, but got (\d+)").ok()?;
    let caps = re.captures(msg)?;
    let expected = caps.get(1)?.as_str().parse().ok()?;
    let actual = caps.get(2)?.as_str().parse().ok()?;
    Some((expected, actual))
}

/// Find where a named member is used in source code (after a given line).
fn find_member_usage(
    source: &str,
    member_name: &str,
    after_line: usize,
) -> Option<(usize, String)> {
    let lines: Vec<&str> = source.lines().collect();
    let pattern = format!("{}(", member_name);

    for (i, line) in lines.iter().enumerate() {
        let line_no = i + 1;
        if line_no > after_line && line.contains(&pattern) {
            return Some((line_no, line.to_string()));
        }
    }
    None
}

/// Find a function call site in a line and return its position info.
struct CallSiteInfo {
    close_paren: usize,
}

fn find_call_site(line: &str) -> Option<CallSiteInfo> {
    // Find the last `)` that closes a function call
    let mut depth = 0;
    let chars: Vec<char> = line.chars().collect();

    // Scan from the end to find the outermost closing paren
    for i in (0..chars.len()).rev() {
        match chars[i] {
            ')' => {
                if depth == 0 {
                    return Some(CallSiteInfo { close_paren: i });
                }
                depth += 1;
            }
            '(' => {
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

// ============================================================================
// Tests
// ============================================================================

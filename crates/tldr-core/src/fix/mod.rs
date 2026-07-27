//! Diagnostic and auto-fix system for `tldr fix`.
//!
//! This module provides:
//! - Error parsing from compiler/runtime output (`error_parser`)
//! - Python error analysis with 22 analyzers (`python`)
//! - Rust error analysis with 5 analyzers (`rust_lang`)
//! - TypeScript error analysis with 8 analyzers (`typescript`)
//! - Go error analysis with 6 analyzers (`go`)
//! - JavaScript error analysis with 4 analyzers (`javascript`)
//! - Patch application for text edits (`patch`)
//! - Core types for the fix lifecycle (`types`)
//! - Check loop: run command, diagnose, fix, repeat (`check`)
//!
//! # Usage
//!
//! ```rust,ignore
//! use tldr_core::fix::{diagnose, apply_fix};
//!
//! let error_text = "UnboundLocalError: cannot access local variable 'counter'";
//! let source = std::fs::read_to_string("app.py")?;
//! let diagnosis = diagnose(error_text, &source, Some("python"), None)?;
//! if let Some(fix) = &diagnosis.fix {
//!     let patched = apply_fix(&source, fix);
//!     std::fs::write("app.py", patched)?;
//! }
//! ```

pub mod check;
pub mod error_parser;
pub mod go;
pub mod javascript;
pub mod patch;
pub mod python;
pub mod rust_lang;
pub mod types;
pub mod typescript;

pub use check::{run_check_loop, CheckConfig, CheckResult, FixAttempt};
pub use patch::apply_fix;
pub use types::{Diagnosis, EditKind, Fix, FixConfidence, FixLocation, ParsedError, TextEdit};

use crate::ast::parser;

/// Diagnose an error from raw error text and source code.
///
/// This is the main entry point for the fix system. It:
/// 1. Parses the raw error text into a structured `ParsedError`
/// 2. Dispatches to the correct language-specific analyzer
/// 3. Returns a `Diagnosis` with an optional fix
///
/// # Arguments
///
/// * `error_text` - Raw error output (traceback, compiler message, etc.)
/// * `source` - The source code of the file where the error occurred
/// * `lang` - Optional language hint (auto-detected if `None`)
/// * `api_surface` - Optional API surface for enhanced analysis (Phase 1 output)
///
/// # Returns
///
/// `Some(Diagnosis)` if the error was recognized, `None` if parsing failed.
pub fn diagnose(
    error_text: &str,
    source: &str,
    lang: Option<&str>,
    _api_surface: Option<&()>,
) -> Option<Diagnosis> {
    // Step 1: Parse the error text
    let parsed = error_parser::parse_error(error_text, lang)?;

    // Step 2: Dispatch to language-specific analyzer
    match parsed.language.as_str() {
        "python" => diagnose_python(&parsed, source, _api_surface),
        "rust" => diagnose_rust_lang(&parsed, source, _api_surface),
        "typescript" => diagnose_typescript(&parsed, source, _api_surface),
        "go" => diagnose_go(&parsed, source, _api_surface),
        "javascript" => diagnose_javascript(&parsed, source, _api_surface),
        _ => {
            // Try Python analyzer as fallback (handles generic single-line errors)
            diagnose_python(&parsed, source, _api_surface)
        }
    }
}

/// Diagnose a pre-parsed error against source code.
///
/// Use this when you already have a `ParsedError` (e.g., from a custom parser).
pub fn diagnose_parsed(
    error: &ParsedError,
    source: &str,
    _api_surface: Option<&()>,
) -> Option<Diagnosis> {
    match error.language.as_str() {
        "python" => diagnose_python(error, source, _api_surface),
        "rust" => diagnose_rust_lang(error, source, _api_surface),
        "typescript" => diagnose_typescript(error, source, _api_surface),
        "go" => diagnose_go(error, source, _api_surface),
        "javascript" => diagnose_javascript(error, source, _api_surface),
        _ => diagnose_python(error, source, _api_surface),
    }
}

/// Internal: run the Rust diagnostic pipeline.
fn diagnose_rust_lang(
    error: &ParsedError,
    source: &str,
    _api_surface: Option<&()>,
) -> Option<Diagnosis> {
    // Parse the source with tree-sitter
    let tree = parser::parse(source, crate::Language::Rust).ok()?;
    rust_lang::diagnose_rust(error, source, &tree, _api_surface)
}

/// Internal: run the TypeScript diagnostic pipeline.
fn diagnose_typescript(
    error: &ParsedError,
    source: &str,
    _api_surface: Option<&()>,
) -> Option<Diagnosis> {
    // Parse the source with tree-sitter
    let tree = parser::parse(source, crate::Language::TypeScript).ok()?;
    typescript::diagnose_typescript(error, source, &tree, _api_surface)
}

/// Internal: run the Go diagnostic pipeline.
fn diagnose_go(error: &ParsedError, source: &str, _api_surface: Option<&()>) -> Option<Diagnosis> {
    // Parse the source with tree-sitter
    let tree = parser::parse(source, crate::Language::Go).ok()?;
    go::diagnose_go(error, source, &tree, _api_surface)
}

/// Internal: run the JavaScript diagnostic pipeline.
fn diagnose_javascript(
    error: &ParsedError,
    source: &str,
    _api_surface: Option<&()>,
) -> Option<Diagnosis> {
    // Parse the source with tree-sitter (JavaScript uses the TypeScript grammar)
    let tree = parser::parse(source, crate::Language::JavaScript).ok()?;
    javascript::diagnose_javascript(error, source, &tree, _api_surface)
}

/// Internal: run the Python diagnostic pipeline.
fn diagnose_python(
    error: &ParsedError,
    source: &str,
    api_surface: Option<&()>,
) -> Option<Diagnosis> {
    // Parse the source with tree-sitter
    let tree = parser::parse(source, crate::Language::Python).ok()?;
    python::diagnose_python(error, source, &tree, api_surface)
}

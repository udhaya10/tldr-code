//! Error text parser -- converts raw error output into `ParsedError`.
//!
//! Supports:
//! - Python tracebacks (`Traceback (most recent call last):`)
//! - Language auto-detection from error format (spec section 9.7)
//!
//! The parser extracts the error type, message, file, line, function name,
//! and offending source line from the raw text.

use std::path::PathBuf;

use regex::Regex;

use super::types::ParsedError;

// ---------------------------------------------------------------------------
// pytest output pre-processing
// ---------------------------------------------------------------------------

/// Strip pytest decoration lines from raw output.
///
/// Removes:
/// - `====...====` separator lines (pytest section headers/footers)
/// - `____...____` test name separator lines
/// - `FAILED file::test - ErrorType: message` summary lines
/// - `N failed, M passed in X.XXs` timing lines
/// - `-- Captured stdout --` / `-- Captured stderr --` section headers
/// - Progress bar lines (`test_foo.py::test_bar PASSED  [XX%]`)
/// - Session info lines (`platform ...`, `rootdir: ...`, `collected N items`)
///
/// Preserves the actual traceback and error lines.
fn strip_pytest_decoration(raw: &str) -> String {
    let lines: Vec<&str> = raw.lines().collect();
    let mut cleaned: Vec<&str> = Vec::new();
    let mut in_captured_section = false;

    // Pre-compile regexes outside the loop to avoid re-creation per line
    let timing_re = Regex::new(r"^\d+\s+(failed|passed|error)").unwrap();
    let progress_re = Regex::new(r"::\w+\s+(PASSED|FAILED|ERROR|SKIPPED)\s+\[").unwrap();

    for line in &lines {
        let trimmed = line.trim();

        // Skip empty lines (they'll be preserved around traceback content)
        if trimmed.is_empty() {
            // End captured section on blank line
            if in_captured_section {
                in_captured_section = false;
            }
            cleaned.push(line);
            continue;
        }

        // Skip if we're inside a "-- Captured stdout/stderr --" section
        if in_captured_section {
            continue;
        }

        // Skip === separator lines (e.g., "===== FAILURES =====")
        if trimmed.starts_with("===") && trimmed.ends_with("===") {
            continue;
        }
        // Skip === lines that are just separators without text
        if trimmed.chars().all(|c| c == '=') && trimmed.len() > 3 {
            continue;
        }

        // Skip ___ test name separator lines (e.g., "_______ test_foo _______")
        if trimmed.starts_with("___") && trimmed.ends_with("___") {
            continue;
        }
        if trimmed.chars().all(|c| c == '_') && trimmed.len() > 3 {
            continue;
        }

        // Skip FAILED summary lines
        if trimmed.starts_with("FAILED ") && trimmed.contains(" - ") {
            continue;
        }

        // Skip timing summary lines (e.g., "1 failed in 0.12s", "1 failed, 2 passed in 0.45s")
        if timing_re.is_match(trimmed) {
            continue;
        }

        // Skip "-- Captured stdout --" / "-- Captured stderr --" section headers
        // and mark that we're in a captured section
        if trimmed.starts_with("-- Captured ") && trimmed.ends_with(" --") {
            in_captured_section = true;
            continue;
        }

        // Skip pytest session header lines
        if trimmed.starts_with("platform ") && trimmed.contains("pytest") {
            continue;
        }
        if trimmed.starts_with("rootdir:") {
            continue;
        }
        if trimmed.starts_with("collected ") && trimmed.contains("item") {
            continue;
        }

        // Skip progress bar lines (e.g., "test_app.py::test_alpha PASSED   [ 33%]")
        if progress_re.is_match(trimmed) {
            continue;
        }

        // Skip pytest header line (e.g., "===== test session starts =====")
        // Already handled by === detection above

        cleaned.push(line);
    }

    cleaned.join("\n")
}

/// Parse a pytest summary line into a `ParsedError`.
///
/// Handles the format:
/// `FAILED test_app.py::test_foo - NameError: name 'x' is not defined`
fn parse_pytest_summary_line(raw: &str) -> Option<ParsedError> {
    let summary_re =
        Regex::new(r"FAILED\s+([^:]+)::(\w+)\s+-\s+([A-Z]\w*(?:Error|Exception|Warning)):\s*(.*)")
            .ok()?;

    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(caps) = summary_re.captures(trimmed) {
            let file = caps.get(1).unwrap().as_str();
            let _test_name = caps.get(2).unwrap().as_str();
            let error_type = caps.get(3).unwrap().as_str();
            let message = caps.get(4).unwrap().as_str();

            return Some(ParsedError {
                error_type: error_type.to_string(),
                message: message.to_string(),
                file: Some(PathBuf::from(file)),
                line: None,
                column: None,
                language: "python".to_string(),
                raw_text: raw.to_string(),
                function_name: None,
                offending_line: None,
            });
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Python traceback parser
// ---------------------------------------------------------------------------

/// Parse a Python traceback or error message into a `ParsedError`.
///
/// Handles three formats:
/// 1. Full traceback with `Traceback (most recent call last):` header
///    (including when wrapped in pytest verbose output)
/// 2. Pytest summary line: `FAILED file::test - ErrorType: message`
/// 3. Single-line error: `ErrorType: message`
pub fn parse_python_error(raw: &str) -> Option<ParsedError> {
    // Pre-process: strip pytest decoration if present, then try full traceback
    let cleaned = strip_pytest_decoration(raw);
    if let Some(parsed) = parse_python_traceback(&cleaned) {
        return Some(parsed);
    }

    // Try pytest summary line fallback
    if let Some(parsed) = parse_pytest_summary_line(raw) {
        return Some(parsed);
    }

    // Fall back to single-line error
    parse_python_single_line(raw)
}

/// Parse a full Python traceback.
///
/// Format:
/// ```text
/// Traceback (most recent call last):
///   File "app.py", line 10, in some_function
///     counter += 1
/// UnboundLocalError: cannot access local variable 'counter'
/// ```
fn parse_python_traceback(raw: &str) -> Option<ParsedError> {
    if !raw.contains("Traceback (most recent call last)") {
        return None;
    }

    let lines: Vec<&str> = raw.lines().collect();

    // Extract file, line, function from the LAST `File "...", line N, in func` entry
    let file_line_re = Regex::new(r#"^\s*File "([^"]+)", line (\d+)(?:, in (\w+))?"#).ok()?;

    let mut file: Option<PathBuf> = None;
    let mut line_num: Option<usize> = None;
    let mut func_name: Option<String> = None;
    let mut offending_line: Option<String> = None;
    let mut last_file_idx: Option<usize> = None;

    for (idx, text) in lines.iter().enumerate() {
        if let Some(caps) = file_line_re.captures(text) {
            file = Some(PathBuf::from(caps.get(1).unwrap().as_str()));
            line_num = caps.get(2).and_then(|m| m.as_str().parse().ok());
            func_name = caps.get(3).map(|m| m.as_str().to_string());
            last_file_idx = Some(idx);
        }
    }

    // The offending line is the line immediately after the last File reference
    if let Some(idx) = last_file_idx {
        if idx + 1 < lines.len() {
            let candidate = lines[idx + 1].trim();
            // Skip if it looks like another File line or the error line
            if !candidate.starts_with("File ")
                && !candidate.is_empty()
                && !candidate.contains("Traceback")
            {
                offending_line = Some(candidate.to_string());
            }
        }
    }

    // The error line is the last non-empty line that matches `ErrorType: message`
    let error_line_re =
        Regex::new(r"^([A-Z]\w*(?:Error|Exception|Iteration|Warning))\s*:\s*(.*)$").ok()?;
    // Also handle bare error types like "KeyError: 'name'" (compiled outside the loop)
    let key_error_re = Regex::new(r"^(KeyError)\s*:\s*(.*)$").ok()?;

    let mut error_type = String::new();
    let mut message = String::new();

    for text in lines.iter().rev() {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(caps) = error_line_re.captures(trimmed) {
            error_type = caps.get(1).unwrap().as_str().to_string();
            message = caps.get(2).unwrap().as_str().to_string();
            break;
        }
        if let Some(caps) = key_error_re.captures(trimmed) {
            error_type = caps.get(1).unwrap().as_str().to_string();
            message = caps.get(2).unwrap().as_str().to_string();
            break;
        }
        // StopIteration has no suffix "Error"
        if trimmed == "StopIteration" {
            error_type = "StopIteration".to_string();
            break;
        }
        break;
    }

    if error_type.is_empty() {
        // Try inference from message patterns
        error_type = extract_error_type(raw);
        if error_type == "Unknown" {
            return None;
        }
        message = raw.to_string();
    }

    Some(ParsedError {
        error_type,
        message,
        file,
        line: line_num,
        column: None,
        language: "python".to_string(),
        raw_text: raw.to_string(),
        function_name: func_name,
        offending_line,
    })
}

/// Parse a single-line Python error: `ErrorType: message`
fn parse_python_single_line(raw: &str) -> Option<ParsedError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let error_type = extract_error_type(trimmed);
    if error_type == "Unknown" {
        return None;
    }

    // Extract message: everything after "ErrorType: "
    let message = if let Some(idx) = trimmed.find(": ") {
        trimmed[idx + 2..].to_string()
    } else {
        trimmed.to_string()
    };

    // Try to extract file/line if present in the message
    let file_re = Regex::new(r#"File "([^"]+)", line (\d+)"#).ok()?;
    let (file, line) = if let Some(caps) = file_re.captures(raw) {
        (
            Some(PathBuf::from(caps.get(1).unwrap().as_str())),
            caps.get(2).and_then(|m| m.as_str().parse().ok()),
        )
    } else {
        (None, None)
    };

    Some(ParsedError {
        error_type,
        message,
        file,
        line,
        column: None,
        language: "python".to_string(),
        raw_text: raw.to_string(),
        function_name: None,
        offending_line: None,
    })
}

// ---------------------------------------------------------------------------
// Language auto-detection
// ---------------------------------------------------------------------------

/// Auto-detect language from error text format.
///
/// Returns "python", "rust", "typescript", "go", "javascript", or "unknown".
///
/// Detection order matters -- JavaScript is checked before the generic Python
/// `XError:` pattern because JS runtime errors (ReferenceError, TypeError) would
/// otherwise match Python. Rust is detected via both JSON format (`"code":"E0..."`)
/// and rendered text format (`error[E0425]: ...`).
pub fn detect_language(error_text: &str) -> &'static str {
    // Python: Traceback header is unambiguous
    if error_text.contains("Traceback (most recent call last)") {
        return "python";
    }

    // JavaScript (Node.js): must be checked BEFORE the generic Python `XError:` pattern
    // since JS runtime errors (ReferenceError, TypeError, SyntaxError) also match that regex.
    // JS detection is based on Node.js stack trace patterns or .js file references.
    if error_text.contains("at Object.<anonymous>")
        || error_text.contains("at Module._compile")
        || error_text.contains("at Module._extensions")
        || error_text.contains("at node:internal")
        || Regex::new(r"[\w./]+\.js:\d+")
            .map(|re| re.is_match(error_text))
            .unwrap_or(false)
    {
        return "javascript";
    }

    // JavaScript (Node.js): TypeError patterns that are unique to the V8/Node.js runtime
    // and NEVER appear in Python. Must be checked before the generic `XError:` Python pattern.
    //
    // Patterns:
    // - "Cannot read properties of undefined (reading 'X')"
    // - "Cannot read properties of null (reading 'X')"
    // - "Cannot set properties of undefined (setting 'X')"
    // - "Cannot set properties of null (setting 'X')"
    // - "X is not a function" (when preceded by TypeError:)
    if error_text.contains("Cannot read properties of")
        || error_text.contains("Cannot set properties of")
        || Regex::new(r"TypeError:\s+.+\s+is not a function")
            .map(|re| re.is_match(error_text))
            .unwrap_or(false)
    {
        return "javascript";
    }

    // Python: known error types (after JS check to avoid false positives)
    if Regex::new(r"^[A-Z]\w*Error:\s")
        .map(|re| re.is_match(error_text.trim()))
        .unwrap_or(false)
    {
        return "python";
    }

    // Rust: JSON with "code" field starting with E
    if error_text.contains(r#""code""#)
        && (error_text.contains(r#""E0"#) || error_text.contains(r#""rendered""#))
    {
        return "rust";
    }

    // Rust: rendered text format — `error[E0425]: message`
    if Regex::new(r"error\[[A-Z]\d+\]:")
        .map(|re| re.is_match(error_text))
        .unwrap_or(false)
    {
        return "rust";
    }

    // TypeScript: file.ts(line,col): error TS
    if Regex::new(r"\w+\.tsx?\(\d+,\d+\):\s*error\s+TS\d+")
        .map(|re| re.is_match(error_text))
        .unwrap_or(false)
    {
        return "typescript";
    }

    // TypeScript simplified: bare `error TS2304:` without file prefix
    if Regex::new(r"error\s+TS\d+:")
        .map(|re| re.is_match(error_text))
        .unwrap_or(false)
    {
        return "typescript";
    }

    // Go: file.go:line:col: pattern (matches ./main.go:10:5: or ./pkg/handler.go:10:5:)
    if Regex::new(r"[\w./]+\.go:\d+:\d+:")
        .map(|re| re.is_match(error_text))
        .unwrap_or(false)
    {
        return "go";
    }

    // Go simplified: bare Go diagnostic keywords without file:line:col prefix.
    // These patterns are unique to Go compiler output and do not collide with
    // Python/JS/Rust/TS error formats.
    if error_text.contains("undefined:")
        && !error_text.contains("is not defined")
        && !error_text.contains("has no field or method")
    {
        // "undefined: X" is Go; "is not defined" is JS/Python; "has no field or method" uses
        // the field_not_found path below
        return "go";
    }
    if error_text.contains("declared but not used") || error_text.contains("declared and not used")
    {
        return "go";
    }
    if error_text.contains("imported and not used") || error_text.contains("imported but not used")
    {
        return "go";
    }
    if error_text.contains("missing return at end of function") {
        return "go";
    }
    if error_text.contains("has no field or method") {
        return "go";
    }
    if error_text.contains("cannot use") && error_text.contains("as type") {
        return "go";
    }

    "unknown"
}

/// Parse error text with language auto-detection.
///
/// If `lang` is provided, uses the specified language parser.
/// Otherwise, auto-detects from the error format.
pub fn parse_error(raw: &str, lang: Option<&str>) -> Option<ParsedError> {
    let detected = lang.unwrap_or_else(|| detect_language(raw));
    match detected {
        "python" => parse_python_error(raw),
        "rust" => parse_rustc_error(raw),
        "typescript" => parse_tsc_error(raw),
        "go" => parse_go_error(raw),
        "javascript" => parse_js_error(raw),
        _ => {
            // Try Python parser as fallback -- it handles single-line errors
            parse_python_error(raw)
        }
    }
}

// ---------------------------------------------------------------------------
// JavaScript (Node.js) error parser
// ---------------------------------------------------------------------------

/// Parse a Node.js runtime error into a `ParsedError`.
///
/// Handles the Node.js error format:
/// ```text
/// /path/to/file.js:10
///     undefinedVar.foo()
///     ^
/// ReferenceError: undefinedVar is not defined
///     at Object.<anonymous> (/path/to/file.js:10:1)
///     at Module._compile (node:internal/modules/cjs/loader:1234:14)
/// ```
///
/// Also handles single-line runtime errors like:
/// `TypeError: Cannot read properties of undefined (reading 'foo')`
pub fn parse_js_error(raw: &str) -> Option<ParsedError> {
    let lines: Vec<&str> = raw.lines().collect();

    // Strategy 1: Look for the error type line (ReferenceError, TypeError, SyntaxError)
    let error_line_re = Regex::new(
        r"^(ReferenceError|TypeError|SyntaxError|RangeError|URIError|EvalError):\s*(.+)$",
    )
    .ok()?;

    let mut error_type = String::new();
    let mut message = String::new();
    let mut file: Option<PathBuf> = None;
    let mut line_num: Option<usize> = None;
    let mut column: Option<usize> = None;
    let mut offending_line: Option<String> = None;
    let mut function_name: Option<String> = None;

    // Find the error type line
    for text in &lines {
        let trimmed = text.trim();
        if let Some(caps) = error_line_re.captures(trimmed) {
            error_type = caps.get(1).unwrap().as_str().to_string();
            message = caps.get(2).unwrap().as_str().to_string();
            break;
        }
    }

    if error_type.is_empty() {
        return None;
    }

    // Strategy 2: Extract file/line from the header line: `/path/to/file.js:10`
    let header_re = Regex::new(r"^([^\s:]+\.(?:js|mjs|cjs)):(\d+)$").ok()?;
    for text in &lines {
        let trimmed = text.trim();
        if let Some(caps) = header_re.captures(trimmed) {
            file = Some(PathBuf::from(caps.get(1).unwrap().as_str()));
            line_num = caps.get(2).and_then(|m| m.as_str().parse().ok());
            break;
        }
    }

    // Strategy 3: If no header line found, extract from the stack trace
    // `at Object.<anonymous> (/path/to/file.js:10:1)`
    if file.is_none() {
        let stack_re =
            Regex::new(r"at\s+(?:([^\s(]+)\s+)?\(?([^)(\s]+\.(?:js|mjs|cjs)):(\d+):(\d+)\)?")
                .ok()?;
        for text in &lines {
            let trimmed = text.trim();
            if let Some(caps) = stack_re.captures(trimmed) {
                function_name = caps.get(1).map(|m| m.as_str().to_string());
                file = Some(PathBuf::from(caps.get(2).unwrap().as_str()));
                line_num = caps.get(3).and_then(|m| m.as_str().parse().ok());
                column = caps.get(4).and_then(|m| m.as_str().parse().ok());
                break;
            }
        }
    }

    // Strategy 4: Extract the offending source line (line after the header, before `^`)
    if let Some(header_idx) = lines.iter().position(|l| header_re.is_match(l.trim())) {
        if header_idx + 1 < lines.len() {
            let candidate = lines[header_idx + 1].trim();
            if !candidate.is_empty() && !candidate.starts_with('^') && !candidate.starts_with("at ")
            {
                offending_line = Some(candidate.to_string());
            }
        }
    }

    Some(ParsedError {
        error_type,
        message,
        file,
        line: line_num,
        column,
        language: "javascript".to_string(),
        raw_text: raw.to_string(),
        function_name,
        offending_line,
    })
}

// ---------------------------------------------------------------------------
// Rust error parser (rustc --error-format=json)
// ---------------------------------------------------------------------------

/// Parse a Rust compiler error from JSON output.
///
/// Handles two formats:
/// 1. Direct error JSON: `{"code":"E0599","message":"...",...}`
/// 2. Cargo JSON output: `{"reason":"compiler-message","message":{...}}`
///
/// Also handles the rendered text format as a fallback.
pub fn parse_rustc_error(raw: &str) -> Option<ParsedError> {
    // Try parsing as JSON first
    if let Some(parsed) = parse_rustc_json(raw) {
        return Some(parsed);
    }

    // Try line-by-line for cargo's multi-JSON output
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(parsed) = parse_rustc_json(trimmed) {
            return Some(parsed);
        }
    }

    // Fallback: try to parse rendered text format
    parse_rustc_rendered(raw)
}

/// Parse a single JSON object from rustc output.
fn parse_rustc_json(json_str: &str) -> Option<ParsedError> {
    let value: serde_json::Value = serde_json::from_str(json_str).ok()?;

    // Handle cargo JSON wrapper: {"reason":"compiler-message","message":{...}}
    let msg = if value.get("reason").and_then(|r| r.as_str()) == Some("compiler-message") {
        value.get("message")?.clone()
    } else if value.get("code").is_some() || value.get("message").is_some() {
        // Direct error object
        value
    } else {
        return None;
    };

    // Only handle errors (not warnings)
    if let Some(level) = msg.get("level").and_then(|l| l.as_str()) {
        if level != "error" {
            return None;
        }
    }

    // Extract error code
    let error_code = msg
        .get("code")
        .and_then(|c| {
            // Code can be a string or an object with a "code" field
            if c.is_string() {
                c.as_str().map(|s| s.to_string())
            } else {
                c.get("code")
                    .and_then(|cc| cc.as_str())
                    .map(|s| s.to_string())
            }
        })
        .unwrap_or_default();

    let message = msg
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();

    // Extract primary span
    let spans = msg.get("spans").and_then(|s| s.as_array());
    let primary_span = spans.and_then(|spans| {
        spans
            .iter()
            .find(|s| s.get("is_primary").and_then(|p| p.as_bool()) == Some(true))
            .or_else(|| spans.first())
    });

    let file = primary_span
        .and_then(|s| s.get("file_name"))
        .and_then(|f| f.as_str())
        .map(PathBuf::from);

    let line = primary_span
        .and_then(|s| s.get("line_start"))
        .and_then(|l| l.as_u64())
        .map(|l| l as usize);

    let column = primary_span
        .and_then(|s| s.get("column_start"))
        .and_then(|c| c.as_u64())
        .map(|c| c as usize);

    let offending_line = primary_span
        .and_then(|s| s.get("text"))
        .and_then(|t| t.as_array())
        .and_then(|arr| arr.first())
        .and_then(|t| t.get("text"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string());

    // Build the raw_text including children for hint extraction
    let raw_text = json_str.to_string();

    Some(ParsedError {
        error_type: error_code,
        message,
        file,
        line,
        column,
        language: "rust".to_string(),
        raw_text,
        function_name: None,
        offending_line,
    })
}

/// Parse rendered rustc error text (non-JSON format).
///
/// Format:
/// ```text
/// error[E0599]: no method named `read_line` found for struct `Stdin`
///   --> src/main.rs:3:21
/// ```
fn parse_rustc_rendered(raw: &str) -> Option<ParsedError> {
    let error_re = Regex::new(r"error\[([A-Z]\d+)\]:\s*(.+)").ok()?;

    let caps = error_re.captures(raw)?;
    let error_code = caps.get(1).unwrap().as_str().to_string();
    let message = caps.get(2).unwrap().as_str().to_string();

    // Extract location: --> file.rs:line:col
    let loc_re = Regex::new(r"-->\s+([^:]+):(\d+):(\d+)").ok()?;
    let (file, line, column) = if let Some(loc_caps) = loc_re.captures(raw) {
        (
            Some(PathBuf::from(loc_caps.get(1).unwrap().as_str())),
            loc_caps.get(2).and_then(|m| m.as_str().parse().ok()),
            loc_caps.get(3).and_then(|m| m.as_str().parse().ok()),
        )
    } else {
        (None, None, None)
    };

    Some(ParsedError {
        error_type: error_code,
        message,
        file,
        line,
        column,
        language: "rust".to_string(),
        raw_text: raw.to_string(),
        function_name: None,
        offending_line: None,
    })
}

// ---------------------------------------------------------------------------
// TypeScript (tsc) error parser
// ---------------------------------------------------------------------------

/// Parse a TypeScript compiler error from tsc output.
///
/// Handles the standard tsc output format:
/// `file.ts(line,col): error TS2304: Cannot find name 'foo'.`
///
/// Also handles multi-line tsc output by extracting the first error line.
pub fn parse_tsc_error(raw: &str) -> Option<ParsedError> {
    // Try each line for a tsc error pattern
    let tsc_re = Regex::new(r"([^\s(]+\.tsx?)\((\d+),(\d+)\):\s*error\s+(TS\d+):\s*(.*)").ok()?;

    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(caps) = tsc_re.captures(trimmed) {
            let file = caps.get(1).unwrap().as_str();
            let line_no: usize = caps.get(2).unwrap().as_str().parse().ok()?;
            let col: usize = caps.get(3).unwrap().as_str().parse().ok()?;
            let error_code = caps.get(4).unwrap().as_str();
            let message = caps.get(5).unwrap().as_str().trim_end_matches('.');

            return Some(ParsedError {
                error_type: error_code.to_string(),
                message: message.to_string(),
                file: Some(PathBuf::from(file)),
                line: Some(line_no),
                column: Some(col),
                language: "typescript".to_string(),
                raw_text: raw.to_string(),
                function_name: None,
                offending_line: None,
            });
        }
    }

    // Fallback: bare `error TS2304: message` without file(line,col) prefix.
    // Matches simplified / hand-typed / truncated tsc output.
    let tsc_fallback_re = Regex::new(r"error\s+(TS\d+):\s*(.+)").ok()?;

    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(caps) = tsc_fallback_re.captures(trimmed) {
            let error_code = caps.get(1).unwrap().as_str();
            let message = caps.get(2).unwrap().as_str().trim_end_matches('.');

            return Some(ParsedError {
                error_type: error_code.to_string(),
                message: message.to_string(),
                file: None,
                line: None,
                column: None,
                language: "typescript".to_string(),
                raw_text: raw.to_string(),
                function_name: None,
                offending_line: None,
            });
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Go error parser (go build / go vet output)
// ---------------------------------------------------------------------------

/// Parse a Go compiler/vet error from `go build` or `go vet` output.
///
/// Handles the standard Go error format:
/// `./main.go:10:5: undefined: foo`
///
/// Also detects Go-specific error patterns to classify the error type.
pub fn parse_go_error(raw: &str) -> Option<ParsedError> {
    // Go error line pattern: file.go:line:col: message
    let go_re = Regex::new(r"([^\s:]+\.go):(\d+):(\d+):\s*(.+)").ok()?;

    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(caps) = go_re.captures(trimmed) {
            let file = caps.get(1).unwrap().as_str();
            let line_no: usize = caps.get(2).unwrap().as_str().parse().ok()?;
            let col: usize = caps.get(3).unwrap().as_str().parse().ok()?;
            let message = caps.get(4).unwrap().as_str();

            // Classify the error type based on message content
            let error_type = classify_go_error(message);

            return Some(ParsedError {
                error_type,
                message: message.to_string(),
                file: Some(PathBuf::from(file)),
                line: Some(line_no),
                column: Some(col),
                language: "go".to_string(),
                raw_text: raw.to_string(),
                function_name: None,
                offending_line: None,
            });
        }
    }

    // Fallback: bare Go error messages without file:line:col prefix.
    // Matches simplified / hand-typed / truncated go build output.
    // Try classify_go_error on each line; if it returns something other
    // than "go_error" (the catch-all), the line is a recognizable Go diagnostic.
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let error_type = classify_go_error(trimmed);
        if error_type != "go_error" {
            return Some(ParsedError {
                error_type,
                message: trimmed.to_string(),
                file: None,
                line: None,
                column: None,
                language: "go".to_string(),
                raw_text: raw.to_string(),
                function_name: None,
                offending_line: None,
            });
        }
    }

    None
}

/// Classify a Go error message into a canonical error type.
///
/// Maps Go error message patterns to the analyzer pattern names used
/// by the Go fix module.
///
/// Handles two alternate Go compiler phrasings for unused variables:
/// - `x declared but not used`  (older gc format)
/// - `declared and not used: x` (alternate gc format)
///
/// Handles multiple alternate phrasings for unused imports:
/// - `"os" imported and not used`
/// - `"os" imported but not used`
/// - `imported and not used: "os"` (alternate gc format)
/// - `imported but not used: "os"` (alternate gc format)
fn classify_go_error(message: &str) -> String {
    if message.contains("undefined:") && !message.contains("has no field or method") {
        "undefined".to_string()
    } else if message.contains("cannot use") && message.contains("as type") {
        "type_mismatch".to_string()
    } else if message.contains("has no field or method") {
        "field_not_found".to_string()
    } else if message.contains("imported and not used") || message.contains("imported but not used")
    {
        // Both "imported and not used" and "imported but not used"
        // are valid Go compiler phrasings for unused imports.
        "unused_import".to_string()
    } else if message.contains("declared but not used") || message.contains("declared and not used")
    {
        // Both "x declared but not used" and "declared and not used: x"
        // are valid Go compiler phrasings for the same diagnostic.
        "unused_var".to_string()
    } else if message.contains("missing return") {
        "missing_return".to_string()
    } else {
        // Use the raw message prefix as error type
        "go_error".to_string()
    }
}

// ---------------------------------------------------------------------------
// Error type extraction (ported from FastEdit base.py)
// ---------------------------------------------------------------------------

/// Inference table: message pattern -> inferred error type.
/// More specific patterns must come before broader ones.
static INFERENCE_TABLE: &[(&str, &str)] = &[
    // Tier 1 (scope / name)
    ("referenced before assignment", "UnboundLocalError"),
    ("cannot access local variable", "UnboundLocalError"),
    ("is not defined", "NameError"),
    // Tier 2 (types)
    ("object is not callable", "TypeError"),
    ("not JSON serializable", "TypeError"),
    ("unexpected keyword argument", "TypeError"),
    ("required positional argument", "TypeError"),
    ("has no attribute", "AttributeError"),
    ("object is not subscriptable", "TypeError"),
    ("object is not iterable", "TypeError"),
    ("unhashable type", "TypeError"),
    ("argument of type", "TypeError"),
    // Tier 0 (compile-time) -- must come after "has no attribute"
    ("inconsistent use of tabs and spaces", "IndentationError"),
    ("expected an indented block", "IndentationError"),
    ("unexpected indent", "IndentationError"),
    ("unindent does not match", "IndentationError"),
    ("expected ':'", "SyntaxError"),
    ("invalid syntax", "SyntaxError"),
    ("prior to global declaration", "SyntaxError"),
    // Tier 3 (imports)
    ("partially initialized module", "ImportError"),
    ("cannot import name", "ImportError"),
    ("No module named", "ImportError"),
    // Tier 4 (lookups)
    ("list index out of range", "IndexError"),
    ("string index out of range", "IndexError"),
    ("tuple index out of range", "IndexError"),
    // Tier 5 (value / unicode / arithmetic)
    ("invalid literal for int", "ValueError"),
    ("not enough values to unpack", "ValueError"),
    ("too many values to unpack", "ValueError"),
    ("substring not found", "ValueError"),
    ("codec can't decode", "UnicodeError"),
    ("codec can't encode", "UnicodeError"),
    ("float division by zero", "ZeroDivisionError"),
    ("integer division or modulo by zero", "ZeroDivisionError"),
    ("division by zero", "ZeroDivisionError"),
    // Tier 6 (runtime / control flow)
    ("maximum recursion depth exceeded", "RecursionError"),
    // Tier 7 (OS / resources)
    ("No such file or directory", "OSError"),
    ("Permission denied", "OSError"),
    ("Is a directory", "OSError"),
    ("File exists", "OSError"),
];

/// Extract the error type from an error string.
///
/// Handles two formats:
/// 1. Explicit prefix: `UnboundLocalError: message` -> `UnboundLocalError`
/// 2. Message inference: `"referenced before assignment"` -> `UnboundLocalError`
///
/// Returns `"Unknown"` if the error type cannot be determined.
pub fn extract_error_type(error_string: &str) -> String {
    if error_string.is_empty() {
        return "Unknown".to_string();
    }

    // Try explicit prefix: "ErrorType: message" or bare "ErrorType:" at end
    if let Some(caps) = Regex::new(r"^([A-Z]\w+):\s?")
        .ok()
        .and_then(|re| re.captures(error_string.trim()))
    {
        let name = caps.get(1).unwrap().as_str();
        if name.ends_with("Error")
            || name.ends_with("Exception")
            || name.ends_with("Iteration")
            || name == "KeyError"
            || name == "StopIteration"
            || name == "StopAsyncIteration"
        {
            return name.to_string();
        }
    }

    // Infer from message patterns
    for (pattern, error_type) in INFERENCE_TABLE {
        if error_string.contains(pattern) {
            return (*error_type).to_string();
        }
    }

    "Unknown".to_string()
}

/// Extract a variable name from common error message patterns.
///
/// Handles:
/// - `local variable 'x' referenced before assignment`
/// - `cannot access local variable 'x'`
/// - `name 'x' is not defined`
/// - `has no attribute 'x'`
pub fn extract_variable_name(error_message: &str) -> Option<String> {
    // Python <3.12: "local variable 'x' referenced before assignment"
    if let Some(caps) = Regex::new(r"local variable '(\w+)' referenced before assignment")
        .ok()
        .and_then(|re| re.captures(error_message))
    {
        return Some(caps.get(1).unwrap().as_str().to_string());
    }

    // Python 3.12+: "cannot access local variable 'x'"
    if let Some(caps) = Regex::new(r"cannot access local variable '(\w+)'")
        .ok()
        .and_then(|re| re.captures(error_message))
    {
        return Some(caps.get(1).unwrap().as_str().to_string());
    }

    // "name 'x' is not defined"
    if let Some(caps) = Regex::new(r"name '(\w+)' is not defined")
        .ok()
        .and_then(|re| re.captures(error_message))
    {
        return Some(caps.get(1).unwrap().as_str().to_string());
    }

    // "has no attribute 'x'"
    if let Some(caps) = Regex::new(r"has no attribute '(\w+)'")
        .ok()
        .and_then(|re| re.captures(error_message))
    {
        return Some(caps.get(1).unwrap().as_str().to_string());
    }

    None
}

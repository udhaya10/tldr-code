//! Search tools: search, bm25, semantic
//!
//! These tools provide various search capabilities over codebases.

use crate::protocol::ToolsCallResult;
use serde_json::Value;
use std::collections::HashSet;

use super::{
    get_optional_bool, get_optional_int, get_optional_string, get_optional_string_array,
    get_required_string, to_path,
};

/// Handle tldr_search tool call (regex search)
pub fn handle_search(args: Value) -> ToolsCallResult {
    let pattern = match get_required_string(&args, "pattern") {
        Ok(p) => p,
        Err(e) => return ToolsCallResult::error(e),
    };

    let path = match get_required_string(&args, "path") {
        Ok(p) => p,
        Err(e) => return ToolsCallResult::error(e),
    };

    let extensions = get_optional_string_array(&args, "extensions");
    let context_lines = get_optional_int(&args, "context_lines").unwrap_or(0) as usize;
    let max_results = get_optional_int(&args, "max_results").unwrap_or(100) as usize;
    let max_files = get_optional_int(&args, "max_files").unwrap_or(1000) as usize;

    let path = to_path(&path);
    if !path.exists() {
        return ToolsCallResult::error(format!("Path not found: {}", path.display()));
    }

    // Convert extensions to HashSet if provided
    let ext_set: Option<HashSet<String>> = extensions.map(|exts| {
        exts.into_iter()
            .map(|e| {
                if e.starts_with('.') {
                    e
                } else {
                    format!(".{}", e)
                }
            })
            .collect()
    });

    match tldr_core::search(
        &pattern,
        &path,
        ext_set.as_ref(),
        context_lines,
        max_results,
        max_files,
    ) {
        Ok(matches) => {
            match serde_json::to_string_pretty(&serde_json::json!({
                "pattern": pattern,
                "total_matches": matches.len(),
                "matches": matches
            })) {
                Ok(json) => ToolsCallResult::text(json),
                Err(e) => ToolsCallResult::error(format!("Serialization error: {}", e)),
            }
        }
        Err(e) => ToolsCallResult::error(format!("Error: {}", e)),
    }
}

/// Handle tldr_bm25 tool call (BM25 keyword search)
pub fn handle_bm25(args: Value) -> ToolsCallResult {
    let query = match get_required_string(&args, "query") {
        Ok(q) => q,
        Err(e) => return ToolsCallResult::error(e),
    };

    let path = match get_required_string(&args, "path") {
        Ok(p) => p,
        Err(e) => return ToolsCallResult::error(e),
    };

    let top_k = get_optional_int(&args, "top_k").unwrap_or(10) as usize;

    let path = to_path(&path);
    if !path.exists() {
        return ToolsCallResult::error(format!("Path not found: {}", path.display()));
    }

    // Auto-detect language from path or use provided
    let language = get_optional_string(&args, "language");
    let lang = if let Some(l) = language {
        match l.parse::<tldr_core::Language>() {
            Ok(lang) => lang,
            Err(e) => return ToolsCallResult::error(e),
        }
    } else {
        tldr_core::Language::from_directory(&path).unwrap_or(tldr_core::Language::Python)
    };

    let command = serde_json::json!({
        "cmd": "search",
        "query": query,
        "path": path,
        "language": lang,
        "top_k": top_k,
        "include_callgraph": true,
        "regex": false
    });
    let report = match tldr_core::daemon_client::request(&path, &command) {
        Ok(report) => report,
        Err(error) => {
            return ToolsCallResult::error(format!(
                "{error} — start and warm it with `tldr daemon start` then `tldr warm`"
            ))
        }
    };

    match serde_json::to_string_pretty(&report) {
        Ok(json) => ToolsCallResult::text(json),
        Err(e) => ToolsCallResult::error(format!("Serialization error: {}", e)),
    }
}

/// Handle semantic search through the same authoritative resident daemon.
pub fn handle_semantic(args: Value) -> ToolsCallResult {
    let query = match get_required_string(&args, "query") {
        Ok(query) => query,
        Err(error) => return ToolsCallResult::error(error),
    };
    let path = match get_required_string(&args, "path") {
        Ok(path) => to_path(&path),
        Err(error) => return ToolsCallResult::error(error),
    };
    if !path.exists() {
        return ToolsCallResult::error(format!("Path not found: {}", path.display()));
    }
    let top_k = get_optional_int(&args, "top_k").unwrap_or(10) as usize;
    let hybrid = get_optional_bool(&args, "hybrid").unwrap_or(false);
    let languages = get_optional_string(&args, "language")
        .map(|language| language.parse::<tldr_core::Language>())
        .transpose();
    let languages = match languages {
        Ok(Some(language)) => vec![language],
        Ok(None) => Vec::new(),
        Err(error) => return ToolsCallResult::error(error),
    };
    let command = serde_json::json!({
        "cmd": "semantic",
        "query": query,
        "top_k": top_k,
        "hybrid": hybrid,
        "languages": languages,
    });
    let report = match tldr_core::daemon_client::request(&path, &command) {
        Ok(report) => report,
        Err(error) => {
            return ToolsCallResult::error(format!(
                "{error} — start and warm it with `tldr daemon start` then `tldr warm`"
            ))
        }
    };
    match serde_json::to_string_pretty(&report) {
        Ok(json) => ToolsCallResult::text(json),
        Err(error) => ToolsCallResult::error(format!("Serialization error: {error}")),
    }
}

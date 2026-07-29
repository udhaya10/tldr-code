//! Quality tools: context, change_impact, smells, maintainability, diagnostics, health, todo, diff, debt
//!
//! These tools provide code quality analysis and metrics.

use crate::protocol::ToolsCallResult;
use serde_json::Value;

use super::{
    get_optional_bool, get_optional_int, get_optional_string, get_optional_string_array,
    get_required_string, to_path,
};

/// Read the atomically persisted hook ledger. The hook writes on every event,
/// so this remains useful even when the MCP server is a separate process.
pub fn handle_session_stats(args: Value) -> ToolsCallResult {
    let path = match get_required_string(&args, "path") {
        Ok(path) => to_path(&path),
        Err(error) => return ToolsCallResult::error(error),
    };
    let requested = get_optional_string(&args, "session");
    let ledger_path = path.join(".tldr").join("session-context.json");
    let bytes = match std::fs::read(&ledger_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return ToolsCallResult::error(format!(
                "Session ledger unavailable at {}: {}",
                ledger_path.display(),
                error
            ))
        }
    };
    let sessions: Vec<Value> = match serde_json::from_slice(&bytes) {
        Ok(sessions) => sessions,
        Err(error) => return ToolsCallResult::error(format!("Invalid session ledger: {}", error)),
    };
    let selected = requested.as_deref().map_or_else(
        || Value::Array(sessions.clone()),
        |session| {
            sessions
                .iter()
                .find(|entry| entry.get("session_id").and_then(Value::as_str) == Some(session))
                .cloned()
                .unwrap_or(Value::Null)
        },
    );
    let output = serde_json::json!({
        "source": "agent_hooks",
        "session": requested,
        "stats": selected,
        "caveat": "provider token/cost fields are exact only when the agent host includes usage telemetry; tldr injected_tokens are measured locally"
    });
    ToolsCallResult::text(
        serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string()),
    )
}

/// Handle tldr_context tool call
pub fn handle_context(args: Value) -> ToolsCallResult {
    let path = match get_required_string(&args, "path") {
        Ok(p) => p,
        Err(e) => return ToolsCallResult::error(e),
    };

    let entry_point = match get_required_string(&args, "entry_point") {
        Ok(e) => e,
        Err(e) => return ToolsCallResult::error(e),
    };

    let language = match get_required_string(&args, "language") {
        Ok(l) => l,
        Err(e) => return ToolsCallResult::error(e),
    };

    let depth = get_optional_int(&args, "depth").unwrap_or(2) as usize;
    let include_docstrings = get_optional_bool(&args, "include_docstrings").unwrap_or(false);

    let path = to_path(&path);
    if !path.exists() {
        return ToolsCallResult::error(format!("Path not found: {}", path.display()));
    }

    let lang = match language.parse::<tldr_core::Language>() {
        Ok(l) => l,
        Err(e) => return ToolsCallResult::error(e),
    };

    match tldr_core::get_relevant_context(
        &path,
        &entry_point,
        depth,
        lang,
        include_docstrings,
        None,
    ) {
        Ok(context) => {
            // Return the LLM-formatted string for maximum token efficiency
            ToolsCallResult::text(context.to_llm_string())
        }
        Err(e) => ToolsCallResult::error(format!("Error: {}", e)),
    }
}

/// Handle tldr_change_impact tool call
pub fn handle_change_impact(args: Value) -> ToolsCallResult {
    let path = match get_required_string(&args, "path") {
        Ok(p) => p,
        Err(e) => return ToolsCallResult::error(e),
    };

    let language = match get_required_string(&args, "language") {
        Ok(l) => l,
        Err(e) => return ToolsCallResult::error(e),
    };

    let changed_files = get_optional_string_array(&args, "changed_files");

    let path = to_path(&path);
    if !path.exists() {
        return ToolsCallResult::error(format!("Path not found: {}", path.display()));
    }

    let lang = match language.parse::<tldr_core::Language>() {
        Ok(l) => l,
        Err(e) => return ToolsCallResult::error(e),
    };

    let changed: Option<Vec<std::path::PathBuf>> =
        changed_files.map(|files| files.into_iter().map(|f| to_path(&f)).collect());

    match tldr_core::change_impact(&path, changed.as_deref(), lang) {
        Ok(report) => match serde_json::to_string_pretty(&report) {
            Ok(json) => ToolsCallResult::text(json),
            Err(e) => ToolsCallResult::error(format!("Serialization error: {}", e)),
        },
        Err(e) => ToolsCallResult::error(format!("Error: {}", e)),
    }
}

/// Handle tldr_smells tool call
pub fn handle_smells(args: Value) -> ToolsCallResult {
    let path = match get_required_string(&args, "path") {
        Ok(p) => p,
        Err(e) => return ToolsCallResult::error(e),
    };

    let threshold_str =
        get_optional_string(&args, "threshold").unwrap_or_else(|| "default".to_string());
    let smell_type_str = get_optional_string(&args, "smell_type");
    let suggest = get_optional_bool(&args, "suggest").unwrap_or(false);

    let path = to_path(&path);
    if !path.exists() {
        return ToolsCallResult::error(format!("Path not found: {}", path.display()));
    }

    let threshold = match threshold_str.to_lowercase().as_str() {
        "strict" => tldr_core::ThresholdPreset::Strict,
        "relaxed" => tldr_core::ThresholdPreset::Relaxed,
        _ => tldr_core::ThresholdPreset::Default,
    };

    let smell_type = smell_type_str.and_then(|s| match s.as_str() {
        "GodClass" => Some(tldr_core::SmellType::GodClass),
        "LongMethod" => Some(tldr_core::SmellType::LongMethod),
        "LongParameterList" => Some(tldr_core::SmellType::LongParameterList),
        "FeatureEnvy" => Some(tldr_core::SmellType::FeatureEnvy),
        "DataClumps" => Some(tldr_core::SmellType::DataClumps),
        _ => None,
    });

    match tldr_core::detect_smells(&path, threshold, smell_type, suggest) {
        Ok(report) => match serde_json::to_string_pretty(&report) {
            Ok(json) => ToolsCallResult::text(json),
            Err(e) => ToolsCallResult::error(format!("Serialization error: {}", e)),
        },
        Err(e) => ToolsCallResult::error(format!("Error: {}", e)),
    }
}

/// Handle tldr_maintainability tool call
pub fn handle_maintainability(args: Value) -> ToolsCallResult {
    let path = match get_required_string(&args, "path") {
        Ok(p) => p,
        Err(e) => return ToolsCallResult::error(e),
    };

    let include_halstead = get_optional_bool(&args, "include_halstead").unwrap_or(false);
    let language = get_optional_string(&args, "language");

    let path = to_path(&path);
    if !path.exists() {
        return ToolsCallResult::error(format!("Path not found: {}", path.display()));
    }

    let lang = language.and_then(|l| l.parse::<tldr_core::Language>().ok());

    match tldr_core::maintainability_index(&path, include_halstead, lang) {
        Ok(report) => match serde_json::to_string_pretty(&report) {
            Ok(json) => ToolsCallResult::text(json),
            Err(e) => ToolsCallResult::error(format!("Serialization error: {}", e)),
        },
        Err(e) => ToolsCallResult::error(format!("Error: {}", e)),
    }
}

/// Handle tldr_diagnostics tool call
pub fn handle_diagnostics(args: Value) -> ToolsCallResult {
    let path = match get_required_string(&args, "path") {
        Ok(p) => p,
        Err(e) => return ToolsCallResult::error(e),
    };

    let _language = get_optional_string(&args, "language");

    let path = to_path(&path);
    if !path.exists() {
        return ToolsCallResult::error(format!("Path not found: {}", path.display()));
    }

    // Diagnostics requires external tools (pyright, ruff, etc.)
    // For now, return a placeholder response indicating it needs external tooling
    ToolsCallResult::text(serde_json::json!({
        "status": "not_implemented",
        "message": "Diagnostics requires external tools (pyright, ruff). Run these directly for now.",
        "path": path.display().to_string()
    }).to_string())
}

/// Handle tldr_diff tool call
pub fn handle_diff(args: Value) -> ToolsCallResult {
    let old = match get_required_string(&args, "old") {
        Ok(o) => o,
        Err(e) => return ToolsCallResult::error(e),
    };

    let new = match get_required_string(&args, "new") {
        Ok(n) => n,
        Err(e) => return ToolsCallResult::error(e),
    };

    let _language = get_optional_string(&args, "language");

    let old_path = to_path(&old);
    let new_path = to_path(&new);

    if !old_path.exists() {
        return ToolsCallResult::error(format!("Old path not found: {}", old_path.display()));
    }
    if !new_path.exists() {
        return ToolsCallResult::error(format!("New path not found: {}", new_path.display()));
    }

    // Semantic diff is complex - return placeholder for now
    ToolsCallResult::text(
        serde_json::json!({
            "status": "not_implemented",
            "message": "Semantic diff is planned for future implementation",
            "old": old,
            "new": new
        })
        .to_string(),
    )
}

/// Handle tldr_debt tool call
pub fn handle_debt(args: Value) -> ToolsCallResult {
    let path = match get_required_string(&args, "path") {
        Ok(p) => p,
        Err(e) => return ToolsCallResult::error(e),
    };

    let _language = get_optional_string(&args, "language");

    let path = to_path(&path);
    if !path.exists() {
        return ToolsCallResult::error(format!("Path not found: {}", path.display()));
    }

    // Technical debt estimation combines multiple metrics
    // For now, return placeholder
    ToolsCallResult::text(
        serde_json::json!({
            "status": "not_implemented",
            "message": "Technical debt estimation is planned for future implementation",
            "path": path.display().to_string()
        })
        .to_string(),
    )
}

/// Handle tldr_health tool call (composite)
pub fn handle_health(args: Value) -> ToolsCallResult {
    let path = match get_required_string(&args, "path") {
        Ok(p) => p,
        Err(e) => return ToolsCallResult::error(e),
    };

    let _language = get_optional_string(&args, "language");

    let path = to_path(&path);
    if !path.exists() {
        return ToolsCallResult::error(format!("Path not found: {}", path.display()));
    }

    // Health dashboard combines smells, maintainability, and complexity
    // Run maintainability as a quick health indicator
    match tldr_core::maintainability_index(&path, false, None) {
        Ok(mi_report) => {
            match tldr_core::detect_smells(&path, tldr_core::ThresholdPreset::Default, None, false)
            {
                Ok(smells_report) => {
                    let health_score = if mi_report.summary.average_mi >= 80.0
                        && smells_report.summary.total_smells == 0
                    {
                        "excellent"
                    } else if mi_report.summary.average_mi >= 60.0
                        && smells_report.summary.total_smells < 5
                    {
                        "good"
                    } else if mi_report.summary.average_mi >= 40.0
                        && smells_report.summary.total_smells < 10
                    {
                        "fair"
                    } else {
                        "needs_attention"
                    };

                    ToolsCallResult::text(
                        serde_json::json!({
                            "health_score": health_score,
                            "maintainability": {
                                "average_mi": mi_report.summary.average_mi,
                                "min_mi": mi_report.summary.min_mi,
                                "max_mi": mi_report.summary.max_mi,
                                "files_analyzed": mi_report.summary.files_analyzed
                            },
                            "smells": {
                                "total": smells_report.summary.total_smells,
                                "files_with_smells": smells_report.smells.len()
                            }
                        })
                        .to_string(),
                    )
                }
                Err(e) => ToolsCallResult::error(format!("Error analyzing smells: {}", e)),
            }
        }
        Err(e) => ToolsCallResult::error(format!("Error calculating maintainability: {}", e)),
    }
}

/// Handle tldr_todo tool call (composite)
pub fn handle_todo(args: Value) -> ToolsCallResult {
    let path = match get_required_string(&args, "path") {
        Ok(p) => p,
        Err(e) => return ToolsCallResult::error(e),
    };

    let _language = get_optional_string(&args, "language");

    let path = to_path(&path);
    if !path.exists() {
        return ToolsCallResult::error(format!("Path not found: {}", path.display()));
    }

    // Generate action items from analysis
    let mut todos = Vec::new();

    // Check for code smells
    if let Ok(smells_report) =
        tldr_core::detect_smells(&path, tldr_core::ThresholdPreset::Default, None, true)
    {
        for smell in smells_report.smells.iter().take(10) {
            todos.push(serde_json::json!({
                "category": "smell",
                "priority": "medium",
                "file": smell.file.display().to_string(),
                "line": smell.line,
                "description": smell.reason,
                "suggestion": smell.suggestion
            }));
        }
    }

    // Check for secrets
    if let Ok(secrets_report) = tldr_core::scan_secrets(&path, 4.5, false, None) {
        for secret in secrets_report.findings.iter().take(5) {
            todos.push(serde_json::json!({
                "category": "security",
                "priority": "high",
                "file": secret.file.display().to_string(),
                "line": secret.line,
                "description": format!("Potential secret found: {}", secret.pattern),
                "suggestion": "Remove hardcoded secret and use environment variables"
            }));
        }
    }

    ToolsCallResult::text(
        serde_json::json!({
            "total_items": todos.len(),
            "action_items": todos
        })
        .to_string(),
    )
}

#[cfg(test)]
mod session_tests {
    use super::*;

    #[test]
    fn session_stats_selects_requested_persisted_session() {
        let project = tempfile::tempdir().expect("project");
        let directory = project.path().join(".tldr");
        std::fs::create_dir_all(&directory).expect("ledger directory");
        std::fs::write(
            directory.join("session-context.json"),
            r#"[{"session_id":"wanted","input_tokens":12},{"session_id":"other","input_tokens":99}]"#,
        )
        .expect("ledger");
        let result = handle_session_stats(serde_json::json!({
            "path": project.path(),
            "session": "wanted"
        }));
        assert_ne!(result.is_error, Some(true));
        let text = &result.content[0].text;
        assert!(text.contains("\"session_id\": \"wanted\""));
        assert!(!text.contains("\"session_id\": \"other\""));
    }
}

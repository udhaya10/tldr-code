//! Daemon Router - Auto-route CLI commands through daemon cache
//!
//! This module provides transparent routing of CLI commands through the daemon
//! when it's running, falling back to direct compute when the daemon is unavailable.
//!
//! # Design
//!
//! Each command can call `try_daemon_route()` before doing direct compute.
//! If the daemon is running and responds successfully, the cached result is returned.
//! Otherwise, the command falls back to computing the result directly.
//!
//! # Performance
//!
//! The daemon maintains Salsa-style query memoization, providing ~35x speedup
//! on cache hits compared to direct computation.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::commands::daemon::error::DaemonError;
use crate::commands::daemon::ipc::send_raw_command;

// =============================================================================
// Core Router Function
// =============================================================================

/// Try to route a command through the daemon.
///
/// Returns `Some(result)` if the daemon is running and responds successfully.
/// Returns `None` if the daemon is not running or an error occurs (caller should fallback).
///
/// # Arguments
///
/// * `project` - Project root directory (used to find the correct daemon)
/// * `endpoint` - Command name (e.g., "calls", "impact", "structure")
/// * `params` - Additional JSON parameters for the command
///
/// # Example
///
/// ```ignore
/// if let Some(result) = try_daemon_route::<CallGraphOutput>(
///     &self.path,
///     "calls",
///     json!({"language": language.to_string()})
/// ) {
///     return writer.write(&result);
/// }
/// // Fallback to direct compute...
/// ```
pub fn try_daemon_route<T: DeserializeOwned>(
    project: &Path,
    endpoint: &str,
    params: serde_json::Value,
) -> Option<T> {
    // Use blocking runtime for sync commands
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(_) => return None,
    };

    runtime.block_on(try_daemon_route_async(project, endpoint, params))
}

/// Async version of try_daemon_route.
///
/// Used internally and can be called directly from async contexts.
pub async fn try_daemon_route_async<T: DeserializeOwned>(
    project: &Path,
    endpoint: &str,
    params: serde_json::Value,
) -> Option<T> {
    // Resolve project path to absolute
    let project = project.canonicalize().unwrap_or_else(|_| {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(project)
    });

    // Build command JSON
    let mut cmd_obj = serde_json::json!({
        "cmd": endpoint.to_lowercase()
    });

    // Merge additional parameters
    if let serde_json::Value::Object(params_obj) = params {
        if let serde_json::Value::Object(ref mut cmd_map) = cmd_obj {
            for (key, value) in params_obj {
                cmd_map.insert(key, value);
            }
        }
    }

    let command_json = match serde_json::to_string(&cmd_obj) {
        Ok(json) => json,
        Err(_) => return None,
    };

    // Send to daemon
    let response = match send_raw_command(&project, &command_json).await {
        Ok(resp) => resp,
        Err(DaemonError::NotRunning) => return None,
        Err(DaemonError::ConnectionRefused) => return None,
        Err(_) => return None,
    };

    // Parse response - check for error response first
    let response_value: serde_json::Value = match serde_json::from_str(&response) {
        Ok(v) => v,
        Err(_) => return None,
    };

    // Check if response is an error
    if let Some(status) = response_value.get("status") {
        if status == "error" {
            return None;
        }
    }

    // If the response has a "result" field, extract it (daemon wraps results)
    let result_value = if response_value.get("result").is_some() {
        response_value
            .get("result")
            .cloned()
            .unwrap_or(response_value)
    } else {
        response_value
    };

    // Deserialize to target type
    serde_json::from_value(result_value).ok()
}

// =============================================================================
// Semantic Router (require-warm, TLDR-7xz.1)
// =============================================================================

/// Outcome of routing a semantic query to the daemon.
///
/// Unlike [`try_daemon_route`] (whose `None` means "fall back to cold
/// compute"), semantic has NO cold fallback: the caller must surface each
/// non-hit honestly. The four states are machine-distinguishable so the CLI
/// can print the right guidance for each.
#[derive(Debug)]
pub enum SemanticRoute<T> {
    /// Warm daemon served the query at full quality.
    Hit(T),
    /// No daemon is listening for this project.
    DaemonDown,
    /// Daemon is up but the resident store can't serve yet
    /// (`status: "not_ready"`): cold store or build in progress. The message
    /// carries the daemon's guidance (e.g. "index not built — run tldr warm").
    NotReady(String),
    /// Daemon returned a real error, or the response was malformed.
    Error(String),
}

/// Route a semantic query to the daemon — require-warm, never fall back.
///
/// TLDR-7xz.1: `tldr semantic` has exactly two modes — served warm at full
/// quality, or an honest explanation. This is the routing primitive for the
/// first mode; every non-`Hit` variant maps to an explanation, never to a
/// silent cold serve. Kept separate from [`try_daemon_route`] so the ~20
/// cheap AST commands keep their legitimate compute-cold-on-miss behavior.
pub fn route_semantic<T: DeserializeOwned>(
    project: &Path,
    params: serde_json::Value,
) -> SemanticRoute<T> {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return SemanticRoute::Error(format!("failed to start tokio runtime: {e}")),
    };
    runtime.block_on(route_semantic_async(project, params))
}

async fn route_semantic_async<T: DeserializeOwned>(
    project: &Path,
    params: serde_json::Value,
) -> SemanticRoute<T> {
    let project = project.canonicalize().unwrap_or_else(|_| {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(project)
    });

    let mut cmd_obj = serde_json::json!({ "cmd": "semantic" });
    if let (serde_json::Value::Object(params_obj), serde_json::Value::Object(cmd_map)) =
        (params, &mut cmd_obj)
    {
        for (key, value) in params_obj {
            cmd_map.insert(key, value);
        }
    }
    let command_json = match serde_json::to_string(&cmd_obj) {
        Ok(json) => json,
        Err(e) => return SemanticRoute::Error(format!("failed to encode request: {e}")),
    };

    let response = match send_raw_command(&project, &command_json).await {
        Ok(resp) => resp,
        Err(DaemonError::NotRunning) => return SemanticRoute::DaemonDown,
        Err(DaemonError::ConnectionRefused) => return SemanticRoute::DaemonDown,
        Err(e) => return SemanticRoute::Error(format!("daemon request failed: {e}")),
    };

    let response_value: serde_json::Value = match serde_json::from_str(&response) {
        Ok(v) => v,
        Err(e) => return SemanticRoute::Error(format!("malformed daemon response: {e}")),
    };

    match response_value.get("status").and_then(|s| s.as_str()) {
        Some("not_ready") => {
            let msg = response_value
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("index not built — run tldr warm")
                .to_string();
            return SemanticRoute::NotReady(msg);
        }
        Some("error") => {
            let msg = response_value
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("daemon error")
                .to_string();
            return SemanticRoute::Error(msg);
        }
        _ => {}
    }

    let result_value = response_value
        .get("result")
        .cloned()
        .unwrap_or(response_value);
    match serde_json::from_value(result_value) {
        Ok(v) => SemanticRoute::Hit(v),
        Err(e) => SemanticRoute::Error(format!("malformed daemon result: {e}")),
    }
}

/// Check if the daemon is running for a project.
///
/// This is a lightweight check that doesn't send a command.
pub fn is_daemon_running(project: &Path) -> bool {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(_) => return false,
    };

    runtime.block_on(is_daemon_running_async(project))
}

/// Async version of is_daemon_running.
pub async fn is_daemon_running_async(project: &Path) -> bool {
    use crate::commands::daemon::ipc::check_socket_alive;
    check_socket_alive(project).await
}

// =============================================================================
// DaemonRoute — the no-silent-fallback routing primitive (ADR-10 / TLDR-14i)
// =============================================================================
//
// Generalizes `SemanticRoute` to every daemon-capable command. Unlike
// `try_daemon_route` (whose `None` collapses "daemon down" and "daemon errored"
// into the same silent-fallback signal), `DaemonRoute` keeps the states
// machine-distinguishable so the CLI can fail LOUDLY and honestly — the daemon
// is the only serve path, with `--oneshot` as the sole explicit local escape.

/// Environment flag set by the global `--oneshot`/`--local` CLI flag. When set,
/// a command bypasses the daemon entirely and computes locally — the ONLY
/// sanctioned non-daemon path under ADR-10. Checked here (not threaded through
/// every command signature) mirroring the existing `TLDR_QUIET` convention.
pub fn is_oneshot() -> bool {
    std::env::var("TLDR_ONESHOT").is_ok_and(|v| v == "1")
}

/// Outcome of routing a command to the daemon.
///
/// `DaemonDown` and `Error` are deliberately separate (the whole point of this
/// type vs `try_daemon_route`): "no daemon" is an onboarding/UX state, while a
/// real daemon error is a failure to surface. `NotReady` is reserved for
/// index-backed commands (e.g. semantic) whose store must be warmed first;
/// compute-on-miss commands never produce it.
#[derive(Debug)]
pub enum DaemonRoute<T> {
    /// Daemon served the query.
    Hit(T),
    /// No daemon is listening for this project.
    DaemonDown,
    /// Daemon is up but cannot serve yet (e.g. index not built).
    NotReady(String),
    /// Daemon returned a real error, or the response was malformed.
    Error(String),
}

impl<T> DaemonRoute<T> {
    /// Collapse a route into `Ok(value)` on `Hit`, or an honest non-`Hit`
    /// error per ADR-10. `cmd` names the command for the daemon-down guidance.
    /// This is the single place the no-silent-fallback error text lives, so
    /// every converted command fails identically.
    pub fn into_hit_or_bail(self, cmd: &str) -> anyhow::Result<T> {
        match self {
            DaemonRoute::Hit(v) => Ok(v),
            DaemonRoute::DaemonDown => anyhow::bail!(
                "daemon not running — start it with: tldr daemon start  (or run `tldr {cmd} --oneshot` for a one-off local compute)"
            ),
            DaemonRoute::NotReady(msg) => anyhow::bail!("{msg}"),
            DaemonRoute::Error(e) => anyhow::bail!("daemon request failed: {e}"),
        }
    }
}

/// Route a command to the daemon, preserving the DaemonDown vs Error
/// distinction. Sync wrapper over [`route_async`].
pub fn route<T: DeserializeOwned>(
    project: &Path,
    endpoint: &str,
    params: serde_json::Value,
) -> DaemonRoute<T> {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return DaemonRoute::Error(format!("failed to start tokio runtime: {e}")),
    };
    runtime.block_on(route_async(project, endpoint, params))
}

/// Find the project root of a RUNNING daemon that covers `path` — the path
/// itself or any ancestor of it.
///
/// Registry-driven (robust): unlike marker-sniffing (`find_project_root`), this
/// never anchors to a stale `.tldr`/`.git` left in a subdirectory. A per-file
/// command (e.g. `complexity`) deep inside the tree resolves to the repo-root
/// daemon that actually watches it. When several daemons cover the path, the
/// most specific (deepest project) wins.
pub fn daemon_project_for(path: &Path) -> Option<PathBuf> {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    crate::commands::daemon::daemon_registry::live_entries()
        .into_iter()
        .filter(|e| canon == e.project || canon.starts_with(&e.project))
        .map(|e| e.project)
        .max_by_key(|p| p.components().count())
}

/// Resolve the covering daemon for `path` and route `endpoint` to it.
///
/// The per-file/per-path entry point for converted commands: it pairs
/// [`daemon_project_for`] with [`route`] so callers don't have to guess a
/// project root. No covering daemon => [`DaemonRoute::DaemonDown`] (honest,
/// never a silent local fallback).
pub fn route_for_path<T: DeserializeOwned>(
    path: &Path,
    endpoint: &str,
    params: serde_json::Value,
) -> DaemonRoute<T> {
    match daemon_project_for(path) {
        Some(project) => route(&project, endpoint, params),
        None => DaemonRoute::DaemonDown,
    }
}

/// Async version of [`route`].
pub async fn route_async<T: DeserializeOwned>(
    project: &Path,
    endpoint: &str,
    params: serde_json::Value,
) -> DaemonRoute<T> {
    let project = project.canonicalize().unwrap_or_else(|_| {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(project)
    });

    let mut cmd_obj = serde_json::json!({ "cmd": endpoint.to_lowercase() });
    if let (serde_json::Value::Object(params_obj), serde_json::Value::Object(cmd_map)) =
        (params, &mut cmd_obj)
    {
        for (key, value) in params_obj {
            cmd_map.insert(key, value);
        }
    }
    let command_json = match serde_json::to_string(&cmd_obj) {
        Ok(json) => json,
        Err(e) => return DaemonRoute::Error(format!("failed to encode request: {e}")),
    };

    let response = match send_raw_command(&project, &command_json).await {
        Ok(resp) => resp,
        Err(DaemonError::NotRunning) => return DaemonRoute::DaemonDown,
        Err(DaemonError::ConnectionRefused) => return DaemonRoute::DaemonDown,
        Err(e) => return DaemonRoute::Error(format!("daemon request failed: {e}")),
    };

    let response_value: serde_json::Value = match serde_json::from_str(&response) {
        Ok(v) => v,
        Err(e) => return DaemonRoute::Error(format!("malformed daemon response: {e}")),
    };

    match response_value.get("status").and_then(|s| s.as_str()) {
        Some("not_ready") => {
            let msg = response_value
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("daemon not ready — run tldr warm")
                .to_string();
            return DaemonRoute::NotReady(msg);
        }
        Some("error") => {
            let msg = response_value
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("daemon error")
                .to_string();
            return DaemonRoute::Error(msg);
        }
        _ => {}
    }

    let result_value = if response_value.get("result").is_some() {
        response_value
            .get("result")
            .cloned()
            .unwrap_or(response_value)
    } else {
        response_value
    };
    match serde_json::from_value(result_value) {
        Ok(v) => DaemonRoute::Hit(v),
        Err(e) => DaemonRoute::Error(format!("malformed daemon result: {e}")),
    }
}

// =============================================================================
// Convenience Builders
// =============================================================================

/// Build JSON params with optional path.
pub fn params_with_path(path: Option<&Path>) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    if let Some(p) = path {
        obj.insert("path".to_string(), serde_json::json!(p));
    }
    serde_json::Value::Object(obj)
}

/// Build JSON params with file path.
pub fn params_with_file(file: &Path) -> serde_json::Value {
    serde_json::json!({
        "file": file
    })
}

/// Build JSON params with file path and optional language hint.
///
/// Used by commands (e.g. `imports`) that accept `--lang` and route through the
/// daemon. JSON key is `"language"` to match the daemon handler's
/// `ImportsRequest.language` field (handlers/ast.rs:L164) — there is no
/// `#[serde(rename)]` on that field, so a `"lang"` key would be silently
/// ignored and the bug would still ship.
pub fn params_with_file_lang(file: &Path, lang: Option<&str>) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("file".to_string(), serde_json::json!(file));
    if let Some(l) = lang {
        obj.insert("language".to_string(), serde_json::json!(l));
    }
    serde_json::Value::Object(obj)
}

/// Build JSON params with file, function, and optional language hint.
///
/// Used by `complexity` which accepts `--lang`. JSON key is `language` to match
/// the daemon `Complexity` variant (alias `lang` also accepted).
pub fn params_with_file_function_lang(
    file: &Path,
    function: &str,
    lang: Option<&str>,
) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("file".to_string(), serde_json::json!(file));
    obj.insert("function".to_string(), serde_json::json!(function));
    if let Some(l) = lang {
        obj.insert("language".to_string(), serde_json::json!(l));
    }
    serde_json::Value::Object(obj)
}

/// Build JSON params with file and function.
pub fn params_with_file_function(file: &Path, function: &str) -> serde_json::Value {
    serde_json::json!({
        "file": file,
        "function": function
    })
}

/// Build JSON params with file, function, and line.
pub fn params_with_file_function_line(file: &Path, function: &str, line: u32) -> serde_json::Value {
    serde_json::json!({
        "file": file,
        "function": function,
        "line": line
    })
}

/// Build JSON params with function name and optional depth.
pub fn params_with_func_depth(func: &str, depth: Option<usize>) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("func".to_string(), serde_json::json!(func));
    if let Some(d) = depth {
        obj.insert("depth".to_string(), serde_json::json!(d));
    }
    serde_json::Value::Object(obj)
}

/// Build JSON params with module and optional path.
pub fn params_with_module(module: &str, path: Option<&Path>) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("module".to_string(), serde_json::json!(module));
    if let Some(p) = path {
        obj.insert("path".to_string(), serde_json::json!(p));
    }
    serde_json::Value::Object(obj)
}

/// Build JSON params with pattern and max_results.
pub fn params_with_pattern(pattern: &str, max_results: Option<usize>) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("pattern".to_string(), serde_json::json!(pattern));
    if let Some(m) = max_results {
        obj.insert("max_results".to_string(), serde_json::json!(m));
    }
    serde_json::Value::Object(obj)
}

/// Build JSON params with entry point and depth.
pub fn params_with_entry_depth(entry: &str, depth: Option<usize>) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("entry".to_string(), serde_json::json!(entry));
    if let Some(d) = depth {
        obj.insert("depth".to_string(), serde_json::json!(d));
    }
    serde_json::Value::Object(obj)
}

/// Build JSON params with path and lang.
pub fn params_with_path_lang(path: &Path, lang: Option<&str>) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("path".to_string(), serde_json::json!(path));
    if let Some(l) = lang {
        obj.insert("lang".to_string(), serde_json::json!(l));
    }
    serde_json::Value::Object(obj)
}

/// Build JSON params for the `smells` command.
///
/// v0.2.3 (#1.D): the smells command supports a repeatable `--files` flag and
/// an `--include-tests` flag. Both are forwarded to the daemon handler so the
/// daemon can produce identical output to direct-compute mode. `files` is only
/// emitted when non-empty so the daemon can detect "default" vs "scoped" mode.
pub fn params_for_smells(
    path: Option<&Path>,
    files: &[PathBuf],
    include_tests: bool,
) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    if let Some(p) = path {
        obj.insert("path".to_string(), serde_json::json!(p));
    }
    if !files.is_empty() {
        obj.insert("files".to_string(), serde_json::json!(files));
    }
    obj.insert(
        "include_tests".to_string(),
        serde_json::Value::Bool(include_tests),
    );
    serde_json::Value::Object(obj)
}

/// Build JSON params for dead code analysis.
pub fn params_for_dead(path: Option<&Path>, entry: Option<&[String]>) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    if let Some(p) = path {
        obj.insert("path".to_string(), serde_json::json!(p));
    }
    if let Some(e) = entry {
        obj.insert("entry".to_string(), serde_json::json!(e));
    }
    serde_json::Value::Object(obj)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_params_with_path() {
        let params = params_with_path(Some(Path::new("/test/path")));
        assert_eq!(params.get("path").unwrap(), "/test/path");
    }

    #[test]
    fn test_params_with_path_none() {
        let params = params_with_path(None);
        assert!(params.get("path").is_none());
    }

    #[test]
    fn test_params_with_file_function() {
        let params = params_with_file_function(Path::new("/test/file.py"), "my_func");
        assert_eq!(params.get("file").unwrap(), "/test/file.py");
        assert_eq!(params.get("function").unwrap(), "my_func");
    }

    #[test]
    fn test_params_with_file_function_line() {
        let params = params_with_file_function_line(Path::new("/test/file.py"), "my_func", 42);
        assert_eq!(params.get("file").unwrap(), "/test/file.py");
        assert_eq!(params.get("function").unwrap(), "my_func");
        assert_eq!(params.get("line").unwrap(), 42);
    }

    #[test]
    fn test_params_with_func_depth() {
        let params = params_with_func_depth("process_data", Some(5));
        assert_eq!(params.get("func").unwrap(), "process_data");
        assert_eq!(params.get("depth").unwrap(), 5);
    }

    #[test]
    fn test_params_with_pattern() {
        let params = params_with_pattern("fn main", Some(100));
        assert_eq!(params.get("pattern").unwrap(), "fn main");
        assert_eq!(params.get("max_results").unwrap(), 100);
    }

    #[test]
    fn test_is_daemon_running_no_daemon() {
        let temp = TempDir::new().unwrap();
        assert!(!is_daemon_running(temp.path()));
    }

    #[test]
    fn test_try_daemon_route_no_daemon() {
        let temp = TempDir::new().unwrap();
        let result: Option<serde_json::Value> =
            try_daemon_route(temp.path(), "ping", serde_json::json!({}));
        assert!(result.is_none());
    }

    #[test]
    fn test_params_with_file_lang_includes_language_key() {
        let params = params_with_file_lang(Path::new("/tmp/myscript"), Some("python"));
        assert_eq!(params["language"], "python");
        assert_eq!(params["file"], "/tmp/myscript");
    }

    #[test]
    fn test_params_with_file_lang_omits_language_when_none() {
        let params = params_with_file_lang(Path::new("/tmp/myscript"), None);
        assert!(params.get("language").is_none());
        assert_eq!(params["file"], "/tmp/myscript");
    }

    #[test]
    fn test_route_daemon_down_when_no_daemon() {
        // No daemon for a fresh tempdir => DaemonDown (NOT a silent None).
        let temp = TempDir::new().unwrap();
        let route: DaemonRoute<serde_json::Value> =
            route(temp.path(), "ping", serde_json::json!({}));
        assert!(matches!(route, DaemonRoute::DaemonDown));
    }

    #[test]
    fn test_into_hit_or_bail_hit_returns_value() {
        let route: DaemonRoute<u32> = DaemonRoute::Hit(42);
        assert_eq!(route.into_hit_or_bail("calls").unwrap(), 42);
    }

    #[test]
    fn test_into_hit_or_bail_daemon_down_names_start_and_oneshot() {
        let route: DaemonRoute<u32> = DaemonRoute::DaemonDown;
        let err = route.into_hit_or_bail("calls").unwrap_err().to_string();
        assert!(err.contains("tldr daemon start"), "got: {err}");
        assert!(err.contains("--oneshot"), "got: {err}");
    }

    #[test]
    fn test_into_hit_or_bail_error_surfaces_message() {
        let route: DaemonRoute<u32> = DaemonRoute::Error("boom".to_string());
        let err = route.into_hit_or_bail("calls").unwrap_err().to_string();
        assert!(err.contains("boom"), "got: {err}");
    }

    #[test]
    fn test_into_hit_or_bail_not_ready_surfaces_message() {
        let route: DaemonRoute<u32> = DaemonRoute::NotReady("index not built".to_string());
        let err = route.into_hit_or_bail("semantic").unwrap_err().to_string();
        assert!(err.contains("index not built"), "got: {err}");
    }

    #[test]
    fn test_is_oneshot_reads_env() {
        // Note: env is process-global; restore afterward to avoid cross-test leak.
        let prev = std::env::var("TLDR_ONESHOT").ok();
        std::env::set_var("TLDR_ONESHOT", "1");
        assert!(is_oneshot());
        std::env::remove_var("TLDR_ONESHOT");
        assert!(!is_oneshot());
        if let Some(p) = prev {
            std::env::set_var("TLDR_ONESHOT", p);
        }
    }

    #[test]
    fn test_daemon_project_for_resolves_ancestor_daemon() {
        // Registry-driven resolution must return the daemon whose project is an
        // ANCESTOR of a deep file — NOT anchor to a stale marker in a subdir
        // (the bug this fix targets, TLDR-7pp.1.3).
        use crate::commands::daemon::daemon_registry::{
            add_entry, test_support::with_registry_dir,
        };
        with_registry_dir(|dir| {
            let project = dir.join("proj");
            let sub = project.join("a/b");
            std::fs::create_dir_all(&sub).unwrap();
            let file = sub.join("f.rs");
            std::fs::write(&file, "x").unwrap();
            // Register a live daemon (this process's pid is alive) at the root.
            add_entry(&project, std::process::id(), &dir.join("p.sock")).unwrap();

            let got = daemon_project_for(&file);
            let canon_project = project.canonicalize().unwrap();
            assert_eq!(
                got,
                Some(canon_project),
                "deep file should resolve to the ancestor daemon"
            );
        });
    }

    #[test]
    fn test_daemon_project_for_none_when_no_daemon() {
        use crate::commands::daemon::daemon_registry::test_support::with_registry_dir;
        with_registry_dir(|dir| {
            let file = dir.join("x.rs");
            std::fs::write(&file, "x").unwrap();
            assert!(
                daemon_project_for(&file).is_none(),
                "no registered daemon => None (honest DaemonDown, never silent fallback)"
            );
        });
    }
}

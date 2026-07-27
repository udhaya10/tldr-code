//! Tool execution engine for L1 commodity diagnostics
//!
//! Spawns diagnostic tools as subprocesses, captures their output, parses it
//! through the appropriate parser, and handles timeouts and failures.
//!
//! Key behaviors:
//! - Binary not found: returns `ToolResult { success: false }` with spawn error
//! - Tool timeout: kills child process, returns timeout error
//! - Non-zero exit with parseable output: `success: true` (linters exit non-zero on findings)
//! - Parse error: `success: false` with parse error detail
//! - After parsing: injects `tool.name` into each `L1Finding.tool` [PM-6]
//!
//! Parallel execution uses `std::thread::scope` (Rust 1.63+) for safe scoped threads.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::parsers;
use super::tools::{L1Finding, ToolConfig, ToolResult};

/// Kill a process by its OS-level PID. Cross-platform (F3).
///
/// On Unix, sends SIGKILL via libc. On Windows, uses `TerminateProcess`
/// via the `windows-sys` crate (or raw WinAPI). This is needed because the
/// watchdog thread only has the PID, not the `Child` handle (which is
/// consumed by `wait_with_output`).
fn kill_process_by_id(pid: u32) {
    #[cfg(unix)]
    {
        // SAFETY: We are sending SIGKILL to a process we spawned.
        // The PID is valid because we obtained it from child.id() before
        // the watchdog thread was spawned.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        // On Windows, open the process handle and terminate it.
        // SAFETY: We spawned this process and hold a valid PID.
        unsafe {
            let handle = windows_sys::Win32::System::Threading::OpenProcess(
                windows_sys::Win32::System::Threading::PROCESS_TERMINATE,
                0, // bInheritHandle = FALSE
                pid,
            );
            if handle != 0 {
                windows_sys::Win32::System::Threading::TerminateProcess(handle, 1);
                windows_sys::Win32::Foundation::CloseHandle(handle);
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        // Unsupported platform: log a warning. The timeout flag is still
        // set, so the result will report a timeout even if the process
        // continues running.
        eprintln!("bugbot: cannot kill process {} on this platform", pid);
    }
}

/// Maximum bytes of stdout/stderr to retain from a tool subprocess.
///
/// This is a safety valve: clippy on a large project can produce megabytes
/// of JSON output. Beyond this limit, output is truncated to prevent
/// unbounded memory growth. 10 MB is generous for any reasonable project.
pub const MAX_OUTPUT_BYTES: usize = 10 * 1024 * 1024; // 10 MB

/// Executes diagnostic tools and captures their output.
///
/// Each tool is run as a subprocess with configurable timeout. Output is
/// captured and fed through the parser identified by `ToolConfig::parser`.
pub struct ToolRunner {
    /// Timeout per tool in seconds
    timeout_secs: u64,
}

impl ToolRunner {
    /// Create a new `ToolRunner` with the given per-tool timeout in seconds.
    pub fn new(timeout_secs: u64) -> Self {
        Self { timeout_secs }
    }

    /// Run a single tool and parse its output.
    ///
    /// # Contract
    /// - Binary not found: `ToolResult { success: false, error: "spawn" message }`
    /// - Tool timeout: kill child, `ToolResult { success: false, error: "Timeout" }`
    /// - Tool crashes (non-zero exit) with parseable output: `success: true`
    ///   (linters exit non-zero when findings exist)
    /// - Tool crashes with unparseable output: `success: false`
    /// - Parse error: `ToolResult { success: false, error: "Parse error: ..." }`
    /// - After parsing, injects `tool.name` into each `L1Finding.tool` [PM-6]
    pub fn run_tool(&self, tool: &ToolConfig, project_path: &Path) -> (ToolResult, Vec<L1Finding>) {
        let start = Instant::now();

        // Spawn the subprocess
        let child = Command::new(tool.binary)
            .args(tool.args)
            .current_dir(project_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        let child = match child {
            Ok(c) => c,
            Err(e) => {
                return (
                    ToolResult {
                        name: tool.name.to_string(),
                        category: tool.category,
                        success: false,
                        duration_ms: start.elapsed().as_millis() as u64,
                        finding_count: 0,
                        error: Some(format!("Failed to spawn '{}': {}", tool.binary, e)),
                        exit_code: None,
                    },
                    vec![],
                );
            }
        };

        // Set up timeout watchdog thread.
        // The watchdog sleeps for timeout_secs, then kills the process via SIGKILL.
        // Meanwhile the main thread calls wait_with_output() which blocks until
        // the child exits (either naturally or via the kill signal).
        let timeout = Duration::from_secs(self.timeout_secs);
        let child_id = child.id();
        let timed_out = Arc::new(AtomicBool::new(false));
        let timed_out_clone = timed_out.clone();

        let _watchdog = std::thread::spawn(move || {
            std::thread::sleep(timeout);
            timed_out_clone.store(true, Ordering::SeqCst);
            // Kill the child process. Platform-specific because we only have
            // the PID (the Child handle is consumed by wait_with_output).
            kill_process_by_id(child_id);
        });

        // Block until child exits (naturally or killed by watchdog)
        let output = child.wait_with_output();
        let duration_ms = start.elapsed().as_millis() as u64;

        // Check if watchdog triggered
        if timed_out.load(Ordering::SeqCst) {
            return (
                ToolResult {
                    name: tool.name.to_string(),
                    category: tool.category,
                    success: false,
                    duration_ms,
                    finding_count: 0,
                    error: Some(format!("Timeout after {}s", self.timeout_secs)),
                    exit_code: None,
                },
                vec![],
            );
        }

        // Read output and truncate to MAX_OUTPUT_BYTES to prevent unbounded
        // memory growth (F1 safety valve).
        let (stdout, stderr, exit_code) = match output {
            Ok(o) => {
                let raw_stdout = String::from_utf8_lossy(&o.stdout).to_string();
                let raw_stderr = String::from_utf8_lossy(&o.stderr).to_string();
                let stdout = if raw_stdout.len() > MAX_OUTPUT_BYTES {
                    let mut truncated = raw_stdout;
                    truncated.truncate(MAX_OUTPUT_BYTES);
                    // Trim to last complete line to avoid breaking JSON parsing
                    if let Some(last_newline) = truncated.rfind('\n') {
                        truncated.truncate(last_newline + 1);
                    }
                    truncated
                } else {
                    raw_stdout
                };
                let stderr = if raw_stderr.len() > MAX_OUTPUT_BYTES {
                    let mut truncated = raw_stderr;
                    truncated.truncate(MAX_OUTPUT_BYTES);
                    truncated
                } else {
                    raw_stderr
                };
                (stdout, stderr, o.status.code())
            }
            Err(e) => {
                return (
                    ToolResult {
                        name: tool.name.to_string(),
                        category: tool.category,
                        success: false,
                        duration_ms,
                        finding_count: 0,
                        error: Some(format!("Failed to read output: {}", e)),
                        exit_code: None,
                    },
                    vec![],
                );
            }
        };

        // Parse output through the tool's parser
        match parsers::parse_tool_output(tool.parser, &stdout) {
            Ok(mut findings) => {
                // PM-6: Inject tool name into each finding
                for f in &mut findings {
                    f.tool = tool.name.to_string();
                }
                let count = findings.len();
                (
                    ToolResult {
                        name: tool.name.to_string(),
                        category: tool.category,
                        success: true,
                        duration_ms,
                        finding_count: count,
                        error: None,
                        exit_code,
                    },
                    findings,
                )
            }
            Err(e) => {
                // If parse failed, include truncated stderr for diagnostics
                let error_msg = if stderr.is_empty() {
                    format!("Parse error: {}", e)
                } else {
                    let truncated = if stderr.len() > 200 {
                        &stderr[..200]
                    } else {
                        &stderr
                    };
                    format!("Parse error: {}. stderr: {}", e, truncated.trim())
                };
                (
                    ToolResult {
                        name: tool.name.to_string(),
                        category: tool.category,
                        success: false,
                        duration_ms,
                        finding_count: 0,
                        error: Some(error_msg),
                        exit_code,
                    },
                    vec![],
                )
            }
        }
    }

    /// Run multiple tools in parallel, collecting results.
    ///
    /// # Contract
    /// - One tool failure does not block others
    /// - Results are in deterministic order (same as input `tools` order)
    /// - All findings have tool name injected [PM-6]
    /// - Single tool or empty list: runs sequentially (no thread overhead)
    pub fn run_tools_parallel(
        &self,
        tools: &[&ToolConfig],
        project_path: &Path,
    ) -> (Vec<ToolResult>, Vec<L1Finding>) {
        if tools.len() <= 1 {
            return self.run_tools_sequential(tools, project_path);
        }

        // Parallel execution using scoped threads (Rust 1.63+)
        // Scoped threads allow borrowing from the enclosing scope safely.
        let results: Vec<(usize, ToolResult, Vec<L1Finding>)> = std::thread::scope(|s| {
            let handles: Vec<_> = tools
                .iter()
                .enumerate()
                .map(|(i, tool)| {
                    let tool_name = tool.name;
                    let tool_category = tool.category;
                    let path = project_path;
                    let handle = s.spawn(move || {
                        let (result, findings) = self.run_tool(tool, path);
                        (i, result, findings)
                    });
                    (handle, i, tool_name, tool_category)
                })
                .collect();

            // F4: Convert thread panics into ToolResult with success=false
            // instead of propagating the panic to the parent thread.
            handles
                .into_iter()
                .map(|(h, idx, name, category)| match h.join() {
                    Ok(result) => result,
                    Err(_) => {
                        eprintln!("bugbot: tool thread for '{}' panicked", name);
                        (
                            idx,
                            ToolResult {
                                name: name.to_string(),
                                category,
                                success: false,
                                duration_ms: 0,
                                finding_count: 0,
                                error: Some("Tool thread panicked".to_string()),
                                exit_code: None,
                            },
                            vec![],
                        )
                    }
                })
                .collect()
        });

        // Sort by original index to maintain deterministic order
        let mut sorted = results;
        sorted.sort_by_key(|(i, _, _)| *i);

        let mut all_results = Vec::new();
        let mut all_findings = Vec::new();
        for (_idx, result, findings) in sorted {
            all_results.push(result);
            all_findings.extend(findings);
        }

        (all_results, all_findings)
    }

    /// Run tools sequentially. Used when there is 0 or 1 tool.
    fn run_tools_sequential(
        &self,
        tools: &[&ToolConfig],
        project_path: &Path,
    ) -> (Vec<ToolResult>, Vec<L1Finding>) {
        let mut all_results = Vec::new();
        let mut all_findings = Vec::new();
        for tool in tools {
            let (result, findings) = self.run_tool(tool, project_path);
            all_results.push(result);
            all_findings.extend(findings);
        }
        (all_results, all_findings)
    }
}

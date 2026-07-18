//! `tldr init` / `tldr init --remove` — project lifecycle.
//!
//! Brings up (or tears down) per-project daemon supervision without requiring
//! the user to run `warm` or hand-author LaunchAgents.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args;
use serde::Serialize;

use crate::commands::daemon::start::DaemonStartArgs;
use crate::commands::daemon::stop::DaemonStopArgs;
use crate::commands::daemon::warm::WarmArgs;
use crate::commands::daemon::ipc::check_socket_alive;
use crate::lifecycle::launchd::{
    default_path_env, install_launch_agent, remove_launch_agent, resolve_tldr_bin, LaunchdVars,
};
use crate::lifecycle::{
    derive_label, ensure_project_files, log_paths_for, read_service_state, resolve_project_root,
    write_service_state, ServiceState, TEMPLATE_VERSION,
};
use crate::output::OutputFormat;

/// Initialize or remove tldr project lifecycle for a directory.
#[derive(Debug, Clone, Args)]
pub struct InitArgs {
    /// Project root (default: current directory).
    #[arg(long, short = 'p', default_value = ".")]
    pub project: PathBuf,

    /// Tear down LaunchAgent + stop daemon for this project.
    /// Keeps `.tldr/config.json`, caches, and log files.
    #[arg(long)]
    pub remove: bool,
}

#[derive(Debug, Clone, Serialize)]
struct InitOutput {
    status: String,
    action: String,
    project: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plist_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    warm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    logs: Option<InitLogsOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct InitLogsOut {
    stdout: PathBuf,
    stderr: PathBuf,
    daemon: PathBuf,
}

impl InitArgs {
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        if self.remove {
            self.run_remove(format, quiet)
        } else {
            self.run_init(format, quiet)
        }
    }

    fn run_init(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let project = resolve_project_root(&self.project)?;
        let files = ensure_project_files(&project)?;
        let (label, slug) = derive_label(&project);
        let logs = log_paths_for(&project, &slug);
        let tldr_bin = resolve_tldr_bin()?;

        if let Some(parent) = logs.stdout.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        if let Some(parent) = logs.daemon.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let mut plist_path = None;
        let daemon_status;
        let platform;

        if cfg!(target_os = "macos") {
            platform = "macos-launchd".to_string();
            let vars = LaunchdVars {
                label: label.clone(),
                tldr_bin: tldr_bin.clone(),
                project: project.clone(),
                stdout_log: logs.stdout.clone(),
                stderr_log: logs.stderr.clone(),
                path_env: default_path_env(),
            };
            let install = install_launch_agent(&vars, &logs)?;
            if !install.plist_path.as_os_str().is_empty() {
                plist_path = Some(install.plist_path);
            }

            std::thread::sleep(Duration::from_millis(800));
            let mut status = wait_for_daemon(&project, 15);
            if status != "running" {
                let _ = DaemonStartArgs {
                    project: project.clone(),
                    foreground: false,
                }
                .run(OutputFormat::Json, true);
                status = wait_for_daemon(&project, 15);
            }
            daemon_status = status;
        } else {
            platform = "daemon-only".to_string();
            match (DaemonStartArgs {
                project: project.clone(),
                foreground: false,
            })
            .run(OutputFormat::Json, true)
            {
                Ok(()) => {}
                Err(e) => {
                    let msg = e.to_string();
                    if !msg.contains("already running") {
                        return Err(e);
                    }
                }
            }
            daemon_status = wait_for_daemon(&project, 15);
        }

        let warm_status = ensure_warm(&project);

        let state = ServiceState {
            version: 1,
            template_version: TEMPLATE_VERSION,
            project: project.clone(),
            label: label.clone(),
            plist_path: plist_path.clone(),
            tldr_bin,
            platform,
            logs: logs.clone(),
            installed_at: Some(chrono::Utc::now().to_rfc3339()),
        };
        write_service_state(&project, &state)
            .with_context(|| "failed to write .tldr/service.json")?;

        let mut notes = Vec::new();
        if files.config_created {
            notes.push("wrote .tldr/config.json".to_string());
        }
        if files.ignore_created {
            notes.push("wrote .tldrignore".to_string());
        }

        let out = InitOutput {
            status: "ok".to_string(),
            action: "init".to_string(),
            project,
            label: Some(label),
            plist_path,
            daemon: Some(daemon_status),
            warm: Some(warm_status),
            logs: Some(InitLogsOut {
                stdout: logs.stdout,
                stderr: logs.stderr,
                daemon: logs.daemon,
            }),
            message: Some(if notes.is_empty() {
                "project lifecycle ready (idempotent refresh)".to_string()
            } else {
                notes.join("; ")
            }),
        };
        print_output(&out, format, quiet)
    }

    fn run_remove(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let project = match resolve_project_root(&self.project) {
            Ok(p) => p,
            Err(_) => {
                let p = if self.project.is_absolute() {
                    self.project.clone()
                } else {
                    std::env::current_dir()?.join(&self.project)
                };
                p.canonicalize().unwrap_or(p)
            }
        };

        let state = read_service_state(&project);
        let (label, plist_path) = if let Some(ref s) = state {
            (s.label.clone(), s.plist_path.clone())
        } else {
            let (label, _) = derive_label(&project);
            (label, None)
        };

        // Bootout first so KeepAlive cannot respawn during stop.
        remove_launch_agent(&label, plist_path.as_deref())?;

        let _ = DaemonStopArgs {
            project: project.clone(),
            all: false,
        }
        .run(OutputFormat::Json, true);

        remove_launch_agent(&label, plist_path.as_deref())?;

        let service_path = project.join(".tldr").join("service.json");
        if service_path.exists() {
            std::fs::remove_file(&service_path)
                .with_context(|| format!("failed to remove {}", service_path.display()))?;
        }

        let out = InitOutput {
            status: "ok".to_string(),
            action: "remove".to_string(),
            project,
            label: Some(label),
            plist_path,
            daemon: Some("stopped".to_string()),
            warm: None,
            logs: state.map(|s| InitLogsOut {
                stdout: s.logs.stdout,
                stderr: s.logs.stderr,
                daemon: s.logs.daemon,
            }),
            message: Some(
                "service removed; config/cache/logs retained (delete manually to purge)"
                    .to_string(),
            ),
        };
        print_output(&out, format, quiet)
    }
}

fn wait_for_daemon(project: &std::path::Path, timeout_secs: u64) -> String {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(_) => return "unknown".to_string(),
    };
    runtime.block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
        while tokio::time::Instant::now() < deadline {
            if check_socket_alive(project).await {
                return "running".to_string();
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        "not_running".to_string()
    })
}

fn ensure_warm(project: &std::path::Path) -> String {
    let args = WarmArgs {
        path: project.to_path_buf(),
        background: false,
    };
    match args.run(OutputFormat::Json, true) {
        Ok(()) => "queued_or_ok".to_string(),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not running") {
                "skipped_no_daemon".to_string()
            } else {
                format!("error: {msg}")
            }
        }
    }
}

fn print_output(out: &InitOutput, format: OutputFormat, quiet: bool) -> Result<()> {
    if quiet && matches!(format, OutputFormat::Text | OutputFormat::Dot) {
        return Ok(());
    }
    match format {
        OutputFormat::Json | OutputFormat::Compact => {
            println!("{}", serde_json::to_string_pretty(out)?);
        }
        OutputFormat::Text | OutputFormat::Sarif | OutputFormat::Dot => {
            println!("tldr init — {}", out.action);
            println!("  status:  {}", out.status);
            println!("  project: {}", out.project.display());
            if let Some(ref l) = out.label {
                println!("  label:   {l}");
            }
            if let Some(ref d) = out.daemon {
                println!("  daemon:  {d}");
            }
            if let Some(ref w) = out.warm {
                println!("  warm:    {w}");
            }
            if let Some(ref logs) = out.logs {
                println!("  logs:");
                println!("    out:    {}", logs.stdout.display());
                println!("    err:    {}", logs.stderr.display());
                println!("    daemon: {}", logs.daemon.display());
            }
            if let Some(ref m) = out.message {
                println!("  note:    {m}");
            }
        }
    }
    Ok(())
}

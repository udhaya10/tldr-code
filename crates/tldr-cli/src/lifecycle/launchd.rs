//! macOS LaunchAgent install/remove for per-project daemons.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::service_state::{ServiceLogs, TEMPLATE_VERSION};

/// Embedded LaunchAgent template (`templates/launchd/daemon.plist.template`).
pub const LAUNCHD_TEMPLATE: &str = include_str!("../../templates/launchd/daemon.plist.template");

#[derive(Debug, Clone)]
pub struct LaunchdInstall {
    pub label: String,
    pub plist_path: PathBuf,
    pub logs: ServiceLogs,
}

/// Variables substituted into the LaunchAgent template.
#[derive(Debug, Clone)]
pub struct LaunchdVars {
    pub label: String,
    pub tldr_bin: PathBuf,
    pub project: PathBuf,
    pub stdout_log: PathBuf,
    pub stderr_log: PathBuf,
    pub path_env: String,
}

/// Render the plist template. Pure function — unit-tested.
pub fn render_plist(vars: &LaunchdVars) -> String {
    LAUNCHD_TEMPLATE
        .replace("{{LABEL}}", &vars.label)
        .replace("{{TLDR_BIN}}", &vars.tldr_bin.to_string_lossy())
        .replace("{{PROJECT}}", &vars.project.to_string_lossy())
        .replace(
            "{{WORKING_DIRECTORY}}",
            &vars.project.to_string_lossy(),
        )
        .replace("{{STDOUT_LOG}}", &vars.stdout_log.to_string_lossy())
        .replace("{{STDERR_LOG}}", &vars.stderr_log.to_string_lossy())
        .replace("{{PATH_ENV}}", &vars.path_env)
}

pub fn default_path_env() -> String {
    let mut parts = vec![
        "/opt/homebrew/bin".to_string(),
        "/opt/homebrew/sbin".to_string(),
        "/usr/local/bin".to_string(),
        "/usr/bin".to_string(),
        "/bin".to_string(),
        "/usr/sbin".to_string(),
        "/sbin".to_string(),
    ];
    if let Some(home) = dirs::home_dir() {
        parts.insert(0, home.join(".cargo/bin").to_string_lossy().into_owned());
        parts.insert(0, home.join(".local/bin").to_string_lossy().into_owned());
    }
    if let Ok(path) = std::env::var("PATH") {
        for p in path.split(':') {
            if !p.is_empty() && !parts.iter().any(|x| x == p) {
                parts.push(p.to_string());
            }
        }
    }
    parts.join(":")
}

/// Resolve the tldr binary to embed in the LaunchAgent.
pub fn resolve_tldr_bin() -> Result<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(canon) = exe.canonicalize() {
            return Ok(canon);
        }
        return Ok(exe);
    }
    which::which("tldr").context("tldr binary not found on PATH and current_exe failed")
}

#[cfg(target_os = "macos")]
pub fn install_launch_agent(vars: &LaunchdVars, logs: &ServiceLogs) -> Result<LaunchdInstall> {
    let agents_dir = dirs::home_dir()
        .context("HOME not set")?
        .join("Library")
        .join("LaunchAgents");
    fs::create_dir_all(&agents_dir)
        .with_context(|| format!("failed to create {}", agents_dir.display()))?;

    if let Some(parent) = logs.stdout.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create log dir {}", parent.display()))?;
    }

    let plist_path = agents_dir.join(format!("{}.plist", vars.label));
    let body = render_plist(vars);
    fs::write(&plist_path, body)
        .with_context(|| format!("failed to write {}", plist_path.display()))?;

    let uid = get_uid();
    let domain_label = format!("gui/{uid}/{}", vars.label);

    // Best-effort unload of previous job.
    let _ = Command::new("launchctl")
        .args(["bootout", &domain_label])
        .output();

    let status = Command::new("launchctl")
        .args(["bootstrap", &format!("gui/{uid}"), plist_path.to_str().unwrap_or("")])
        .output()
        .context("failed to run launchctl bootstrap")?;
    if !status.status.success() {
        // Already bootstrapped: try kickstart only.
        let kick = Command::new("launchctl")
            .args(["kickstart", &domain_label])
            .output()
            .context("failed to run launchctl kickstart")?;
        if !kick.status.success() {
            let err = String::from_utf8_lossy(&status.stderr);
            let err2 = String::from_utf8_lossy(&kick.stderr);
            bail!(
                "launchctl bootstrap failed: {} / kickstart failed: {}",
                err.trim(),
                err2.trim()
            );
        }
    } else {
        // Ensure running.
        let _ = Command::new("launchctl")
            .args(["kickstart", &domain_label])
            .output();
    }

    let _ = TEMPLATE_VERSION; // documented coupling

    Ok(LaunchdInstall {
        label: vars.label.clone(),
        plist_path,
        logs: logs.clone(),
    })
}

#[cfg(not(target_os = "macos"))]
pub fn install_launch_agent(vars: &LaunchdVars, logs: &ServiceLogs) -> Result<LaunchdInstall> {
    // Non-macOS: no LaunchAgent; caller starts the daemon directly.
    Ok(LaunchdInstall {
        label: vars.label.clone(),
        plist_path: PathBuf::new(),
        logs: logs.clone(),
    })
}

#[cfg(target_os = "macos")]
pub fn remove_launch_agent(label: &str, plist_path: Option<&Path>) -> Result<()> {
    let uid = get_uid();
    let domain_label = format!("gui/{uid}/{label}");
    let _ = Command::new("launchctl")
        .args(["bootout", &domain_label])
        .output();
    if let Some(p) = plist_path {
        if p.exists() {
            fs::remove_file(p)
                .with_context(|| format!("failed to remove plist {}", p.display()))?;
        }
    } else if let Some(home) = dirs::home_dir() {
        let p = home
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{label}.plist"));
        if p.exists() {
            let _ = fs::remove_file(p);
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn remove_launch_agent(_label: &str, _plist_path: Option<&Path>) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn get_uid() -> u32 {
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn render_plist_substitutes_all_placeholders() {
        let vars = LaunchdVars {
            label: "com.parcadei.tldr-daemon.demo".into(),
            tldr_bin: PathBuf::from("/usr/local/bin/tldr"),
            project: PathBuf::from("/tmp/demo"),
            stdout_log: PathBuf::from("/tmp/demo.out.log"),
            stderr_log: PathBuf::from("/tmp/demo.err.log"),
            path_env: "/usr/bin:/bin".into(),
        };
        let out = render_plist(&vars);
        assert!(!out.contains("{{"));
        assert!(out.contains("com.parcadei.tldr-daemon.demo"));
        assert!(out.contains("/usr/local/bin/tldr"));
        assert!(out.contains("--project"));
        assert!(out.contains("/tmp/demo"));
        assert!(out.contains("/tmp/demo.out.log"));
        assert!(out.contains("/tmp/demo.err.log"));
        assert!(out.contains("<key>KeepAlive</key>"));
        assert!(out.contains("<string>--foreground</string>"));
    }

    #[test]
    fn template_version_constant_is_one() {
        assert_eq!(TEMPLATE_VERSION, 1);
    }
}

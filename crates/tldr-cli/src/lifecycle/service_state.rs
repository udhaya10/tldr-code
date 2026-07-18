//! Machine-local service binding recorded under `.tldr/service.json`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Bump when `templates/launchd/daemon.plist.template` changes shape.
pub const TEMPLATE_VERSION: u32 = 1;

/// Log paths owned by a project's lifecycle install.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceLogs {
    pub stdout: PathBuf,
    pub stderr: PathBuf,
    pub daemon: PathBuf,
}

/// Persisted binding between a project root and its LaunchAgent (if any).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceState {
    pub version: u32,
    pub template_version: u32,
    pub project: PathBuf,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plist_path: Option<PathBuf>,
    pub tldr_bin: PathBuf,
    pub platform: String,
    pub logs: ServiceLogs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_at: Option<String>,
}

impl ServiceState {
    pub fn path_in(project: &Path) -> PathBuf {
        project.join(".tldr").join("service.json")
    }
}

/// Build a stable launchd label + filesystem slug from a project path.
pub fn derive_label(project: &Path) -> (String, String) {
    let base = project
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("project");
    let mut slug: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        slug = "project".to_string();
    }
    // Disambiguate same basename at different paths.
    let hash = short_path_hash(project);
    let slug = format!("{slug}-{hash}");
    let label = format!("com.parcadei.tldr-daemon.{slug}");
    (label, slug)
}

fn short_path_hash(project: &Path) -> String {
    let s = project.to_string_lossy();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:08x}", (h & 0xffff_ffff) as u32)
}

/// Default log locations for a project slug.
pub fn log_paths_for(project: &Path, slug: &str) -> ServiceLogs {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    let log_dir = home.join("Library").join("Logs").join("tldr");
    ServiceLogs {
        stdout: log_dir.join(format!("{slug}.out.log")),
        stderr: log_dir.join(format!("{slug}.err.log")),
        daemon: project.join(".tldr").join("daemon.log"),
    }
}

pub fn read_service_state(project: &Path) -> Option<ServiceState> {
    let path = ServiceState::path_in(project);
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn write_service_state(project: &Path, state: &ServiceState) -> std::io::Result<()> {
    let path = ServiceState::path_in(project);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(path, text + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_label_is_stable_and_hashed() {
        let p = PathBuf::from("/tmp/My_App.Name");
        let (label, slug) = derive_label(&p);
        assert!(label.starts_with("com.parcadei.tldr-daemon."));
        assert!(slug.contains("my-app-name"));
        let (label2, _) = derive_label(&p);
        assert_eq!(label, label2);
    }

    #[test]
    fn different_paths_same_basename_differ() {
        let a = PathBuf::from("/tmp/a/proj");
        let b = PathBuf::from("/tmp/b/proj");
        let (la, _) = derive_label(&a);
        let (lb, _) = derive_label(&b);
        assert_ne!(la, lb);
    }
}

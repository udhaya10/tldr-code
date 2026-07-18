//! Project lifecycle helpers for `tldr init` / `tldr init --remove`.
//!
//! Keeps LaunchAgent templating, `.tldr/service.json`, and log path
//! conventions in one place so the CLI surface stays thin.

pub mod launchd;
pub mod project;
pub mod service_state;

pub use launchd::{
    default_path_env, install_launch_agent, remove_launch_agent, resolve_tldr_bin, LaunchdInstall,
    LaunchdVars,
};
pub use project::{ensure_project_files, resolve_project_root, DEFAULT_CONFIG_JSON};
pub use service_state::{
    derive_label, log_paths_for, read_service_state, write_service_state, ServiceLogs,
    ServiceState, TEMPLATE_VERSION,
};

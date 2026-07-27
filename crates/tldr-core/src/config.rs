//! Project configuration for tldr-code (.tldr/config.json)

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Project configuration loaded from `.tldr/config.json` (global then project).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TldrConfig {
    /// Config schema version (defaults to 1).
    #[serde(default = "default_version")]
    pub version: u32,

    /// Embedding-provider settings (model, endpoint, dimensions).
    #[serde(default)]
    pub embedding: EmbeddingConfig,

    /// Semantic-search settings (enabled, language filter).
    #[serde(default)]
    pub semantic: SemanticConfig,

    /// In-daemon filesystem watcher batching and burst-control settings.
    #[serde(default)]
    pub watcher: WatcherConfig,
}

fn default_version() -> u32 {
    1
}

impl Default for TldrConfig {
    fn default() -> Self {
        Self {
            version: default_version(),
            embedding: EmbeddingConfig::default(),
            semantic: SemanticConfig::default(),
            watcher: WatcherConfig::default(),
        }
    }
}

/// Embedding-provider configuration. Defaults to the local in-process model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// Provider id (`"local"` by default; a cloud seam is future work).
    #[serde(default = "default_provider")]
    pub provider: String,

    /// Override the embedding model name (None = the deployed default).
    #[serde(default)]
    pub model: Option<String>,

    /// Remote endpoint URL for non-local providers.
    #[serde(default)]
    pub endpoint: Option<String>,

    /// Environment-variable name holding the provider API key.
    #[serde(default)]
    pub api_key_env: Option<String>,

    /// Expected embedding dimensionality (provider/model specific).
    #[serde(default)]
    pub dimensions: Option<usize>,
}

fn default_provider() -> String {
    "local".to_string()
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            model: None,
            endpoint: None,
            api_key_env: None,
            dimensions: None,
        }
    }
}

/// Semantic-search configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticConfig {
    /// Whether semantic search is enabled (default true).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Restrict indexing/search to these languages (None = all detected).
    #[serde(default)]
    pub langs: Option<Vec<String>>,
}

fn default_true() -> bool {
    true
}

impl Default for SemanticConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            langs: None,
        }
    }
}

/// In-daemon filesystem watcher configuration.
///
/// Optional fields preserve deep-merge semantics: a project config only
/// overrides the global values it explicitly sets. Runtime defaults live in
/// `DaemonConfig`, where these values become concrete durations and caps.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WatcherConfig {
    /// Enable the in-daemon recursive watcher.
    #[serde(default)]
    pub enabled: Option<bool>,

    /// Quiet period after the most recent accepted event.
    #[serde(default)]
    pub debounce_ms: Option<u64>,

    /// Maximum time from the first event before a batch must flush.
    #[serde(default)]
    pub max_wait_ms: Option<u64>,

    /// Pending unique-file count above which deltas become a full rebuild.
    #[serde(default)]
    pub burst_file_cap: Option<usize>,

    /// Accepted-event count above which deltas become a full rebuild.
    #[serde(default)]
    pub burst_event_cap: Option<usize>,

    /// Rolling window used by `burst_event_cap`.
    #[serde(default)]
    pub burst_window_ms: Option<u64>,
}

impl TldrConfig {
    /// Parse a config from a JSON string.
    // Intentionally named `from_str` for the JSON-parsing API; not the
    // `std::str::FromStr` trait (the error type is `serde_json::Error`).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Load a config from `path`, falling back to defaults if the file is
    /// missing or unparseable.
    pub fn from_path(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(contents) => Self::from_str(&contents).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Deep-merge `other` on top of `self` (other wins for set fields).
    pub fn merge(&mut self, other: &TldrConfig) {
        if other.version != default_version() {
            self.version = other.version;
        }
        self.embedding.merge(&other.embedding);
        self.semantic.merge(&other.semantic);
        self.watcher.merge(&other.watcher);
    }

    /// Resolve config: global (~/.tldr/config.json) then project (.tldr/config.json).
    /// Missing files at any layer are no-ops.
    pub fn resolve(project_root: Option<&Path>) -> Self {
        let global_path = global_config_path();
        let mut config = match global_path {
            Some(p) => Self::from_path(&p),
            None => Self::default(),
        };

        if let Some(root) = project_root {
            let project_path = root.join(".tldr").join("config.json");
            let project_config = Self::from_path(&project_path);
            config.merge(&project_config);
        }

        config
    }
}

impl EmbeddingConfig {
    fn merge(&mut self, other: &EmbeddingConfig) {
        if other.provider != default_provider() {
            self.provider.clone_from(&other.provider);
        }
        if other.model.is_some() {
            self.model.clone_from(&other.model);
        }
        if other.endpoint.is_some() {
            self.endpoint.clone_from(&other.endpoint);
        }
        if other.api_key_env.is_some() {
            self.api_key_env.clone_from(&other.api_key_env);
        }
        if other.dimensions.is_some() {
            self.dimensions = other.dimensions;
        }
    }
}

impl SemanticConfig {
    fn merge(&mut self, other: &SemanticConfig) {
        if !other.enabled {
            self.enabled = false;
        }
        if other.langs.is_some() {
            self.langs.clone_from(&other.langs);
        }
    }
}

impl WatcherConfig {
    fn merge(&mut self, other: &WatcherConfig) {
        self.enabled = other.enabled.or(self.enabled);
        self.debounce_ms = other.debounce_ms.or(self.debounce_ms);
        self.max_wait_ms = other.max_wait_ms.or(self.max_wait_ms);
        self.burst_file_cap = other.burst_file_cap.or(self.burst_file_cap);
        self.burst_event_cap = other.burst_event_cap.or(self.burst_event_cap);
        self.burst_window_ms = other.burst_window_ms.or(self.burst_window_ms);
    }
}

/// Walk up from `start` looking for a directory containing `.tldr/` or `.git/`.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    let mut current = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start
    };
    loop {
        if current.join(".tldr").is_dir() || current.join(".git").is_dir() {
            return Some(current);
        }
        match current.parent() {
            Some(p) if p != current => current = p.to_path_buf(),
            _ => return None,
        }
    }
}

fn global_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".tldr").join("config.json"))
}

#[cfg(test)]
mod tests {
    use super::TldrConfig;

    #[test]
    fn watcher_config_parses_all_pipeline_knobs() {
        let config = TldrConfig::from_str(
            r#"{
                "watcher": {
                    "debounce_ms": 125,
                    "max_wait_ms": 900,
                    "burst_file_cap": 7,
                    "burst_event_cap": 11,
                    "burst_window_ms": 250
                }
            }"#,
        )
        .expect("watcher config should parse");

        assert_eq!(config.watcher.debounce_ms, Some(125));
        assert_eq!(config.watcher.max_wait_ms, Some(900));
        assert_eq!(config.watcher.burst_file_cap, Some(7));
        assert_eq!(config.watcher.burst_event_cap, Some(11));
        assert_eq!(config.watcher.burst_window_ms, Some(250));
    }

    #[test]
    fn watcher_config_merge_only_replaces_explicit_values() {
        let mut base = TldrConfig::from_str(
            r#"{
                "watcher": {
                    "debounce_ms": 125,
                    "max_wait_ms": 900,
                    "burst_file_cap": 7
                }
            }"#,
        )
        .expect("base config should parse");
        let overlay = TldrConfig::from_str(
            r#"{
                "watcher": {
                    "max_wait_ms": 1200,
                    "burst_event_cap": 11
                }
            }"#,
        )
        .expect("overlay config should parse");

        base.merge(&overlay);

        assert_eq!(base.watcher.debounce_ms, Some(125));
        assert_eq!(base.watcher.max_wait_ms, Some(1200));
        assert_eq!(base.watcher.burst_file_cap, Some(7));
        assert_eq!(base.watcher.burst_event_cap, Some(11));
        assert_eq!(base.watcher.burst_window_ms, None);
    }
}

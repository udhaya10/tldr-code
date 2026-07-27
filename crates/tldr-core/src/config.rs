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

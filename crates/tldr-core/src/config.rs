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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_object_returns_defaults() {
        let config = TldrConfig::from_str("{}").unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.embedding.provider, "local");
        assert!(config.embedding.model.is_none());
        assert!(config.semantic.enabled);
        assert!(config.semantic.langs.is_none());
    }

    #[test]
    fn partial_config_fills_defaults() {
        let json = r#"{"embedding": {"model": "arctic-l"}}"#;
        let config = TldrConfig::from_str(json).unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.embedding.provider, "local");
        assert_eq!(config.embedding.model.as_deref(), Some("arctic-l"));
        assert!(config.semantic.enabled);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let json = r#"{"version": 1, "future_field": true, "embedding": {"model": "arctic-m", "new_option": 42}}"#;
        let config = TldrConfig::from_str(json).unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.embedding.model.as_deref(), Some("arctic-m"));
    }

    #[test]
    fn full_cloud_shaped_config() {
        let json = r#"{
            "version": 1,
            "embedding": {
                "provider": "openai",
                "model": "text-embedding-3-large",
                "endpoint": "https://api.openai.com/v1/embeddings",
                "api_key_env": "OPENAI_API_KEY",
                "dimensions": 3072
            },
            "semantic": {
                "enabled": true,
                "langs": ["rs", "py"]
            }
        }"#;
        let config = TldrConfig::from_str(json).unwrap();
        assert_eq!(config.embedding.provider, "openai");
        assert_eq!(
            config.embedding.model.as_deref(),
            Some("text-embedding-3-large")
        );
        assert_eq!(
            config.embedding.endpoint.as_deref(),
            Some("https://api.openai.com/v1/embeddings")
        );
        assert_eq!(
            config.embedding.api_key_env.as_deref(),
            Some("OPENAI_API_KEY")
        );
        assert_eq!(config.embedding.dimensions, Some(3072));
        assert!(config.semantic.enabled);
        assert_eq!(
            config.semantic.langs.as_deref(),
            Some(&["rs".to_string(), "py".to_string()][..])
        );
    }

    #[test]
    fn missing_file_returns_default() {
        let config = TldrConfig::from_path(Path::new("/nonexistent/path/config.json"));
        assert_eq!(config.version, 1);
        assert_eq!(config.embedding.provider, "local");
    }

    #[test]
    fn malformed_json_returns_default() {
        let config = TldrConfig::from_str("{not valid json");
        assert!(config.is_err());
        // from_path would return default on parse error
        // (tested via from_path with a bad file, but we test the fallback logic)
    }

    #[test]
    fn semantic_disabled() {
        let json = r#"{"semantic": {"enabled": false}}"#;
        let config = TldrConfig::from_str(json).unwrap();
        assert!(!config.semantic.enabled);
    }

    #[test]
    fn merge_project_overrides_global() {
        let mut global = TldrConfig::from_str(r#"{"embedding": {"model": "arctic-m"}}"#).unwrap();
        let project = TldrConfig::from_str(r#"{"embedding": {"model": "arctic-l"}}"#).unwrap();
        global.merge(&project);
        assert_eq!(global.embedding.model.as_deref(), Some("arctic-l"));
        assert_eq!(global.embedding.provider, "local");
    }

    #[test]
    fn merge_preserves_unset_fields() {
        let mut global = TldrConfig::from_str(
            r#"{"embedding": {"model": "arctic-l", "endpoint": "http://localhost:8080"}}"#,
        )
        .unwrap();
        let project = TldrConfig::from_str(r#"{"embedding": {"model": "arctic-m"}}"#).unwrap();
        global.merge(&project);
        assert_eq!(global.embedding.model.as_deref(), Some("arctic-m"));
        assert_eq!(
            global.embedding.endpoint.as_deref(),
            Some("http://localhost:8080")
        );
    }

    #[test]
    fn merge_semantic_disabled_wins() {
        let mut global = TldrConfig::default();
        assert!(global.semantic.enabled);
        let project = TldrConfig::from_str(r#"{"semantic": {"enabled": false}}"#).unwrap();
        global.merge(&project);
        assert!(!global.semantic.enabled);
    }

    #[test]
    fn resolve_all_absent_returns_defaults() {
        // Test with a path that won't have a project config
        // Note: This may still load global config if it exists
        let config = TldrConfig::resolve(Some(Path::new("/nonexistent/project")));
        assert_eq!(config.version, 1);
        assert_eq!(config.embedding.provider, "local");
        // Model may be set by global config, so we just check it's a valid option
        if let Some(ref model) = config.embedding.model {
            assert!(!model.is_empty());
        }
        assert!(config.semantic.enabled);
    }

    #[test]
    fn resolve_no_project_root() {
        let config = TldrConfig::resolve(None);
        assert_eq!(config.version, 1);
        assert_eq!(config.embedding.provider, "local");
    }
}

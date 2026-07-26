//! FastEmbed-compatible model metadata and commit-pinned cache resolution.

use std::path::{Path, PathBuf};

use fastembed::{EmbeddingModel as FastEmbeddingModel, OutputKey, Pooling, TextEmbedding};
use hf_hub::{Cache, Repo};

use crate::semantic::types::EmbeddingModel;

const TOKENIZER_FILES: [&str; 4] = [
    "tokenizer.json",
    "config.json",
    "special_tokens_map.json",
    "tokenizer_config.json",
];

/// Pooling operation that must match the FastEmbed oracle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelPooling {
    /// Select the first token embedding.
    Cls,
    /// Attention-mask-aware mean pooling.
    Mean,
}

/// Preferred ONNX output selection copied from FastEmbed metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelOutput {
    /// Model exposes one relevant output.
    OnlyOne,
    /// Select an output by stable position.
    ByOrder(usize),
    /// Select an output by stable name.
    ByName(String),
}

/// Immutable metadata needed by either embedding executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelArtifactSpec {
    /// Hugging Face repository identifier.
    pub repository: String,
    /// Repository-relative ONNX model filename.
    pub model_file: String,
    /// Repository-relative external ONNX data files.
    pub additional_files: Vec<String>,
    /// Expected embedding dimensions.
    pub dimensions: usize,
    /// Oracle-compatible pooling operation.
    pub pooling: ModelPooling,
    /// Oracle-compatible output selection.
    pub output: ModelOutput,
}

impl ModelArtifactSpec {
    /// Derive the exact FastEmbed 5.8.1 metadata for a supported tldr model.
    pub fn for_model(model: EmbeddingModel) -> Result<Self, ModelArtifactError> {
        let fast_model = fastembed_model(model);
        let info = TextEmbedding::get_model_info(&fast_model)
            .map_err(|error| ModelArtifactError::Metadata(error.to_string()))?;
        let pooling = match TextEmbedding::get_default_pooling_method(&fast_model) {
            Some(Pooling::Cls) => ModelPooling::Cls,
            Some(Pooling::Mean) => ModelPooling::Mean,
            None => return Err(ModelArtifactError::MissingPooling),
        };
        let output = match info.output_key.as_ref().unwrap_or(&OutputKey::OnlyOne) {
            OutputKey::OnlyOne => ModelOutput::OnlyOne,
            OutputKey::ByOrder(index) => ModelOutput::ByOrder(*index),
            OutputKey::ByName(name) => ModelOutput::ByName((*name).to_string()),
        };
        Ok(Self {
            repository: info.model_code.clone(),
            model_file: info.model_file.clone(),
            additional_files: info.additional_files.clone(),
            dimensions: info.dim,
            pooling,
            output,
        })
    }
}

/// Exact local files from one immutable Hugging Face snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModelArtifacts {
    /// Metadata used to resolve this snapshot.
    pub spec: ModelArtifactSpec,
    /// Hugging Face commit hash naming the cached snapshot.
    pub revision: String,
    /// Commit-pinned ONNX model path.
    pub model_path: PathBuf,
    /// Commit-pinned external ONNX data paths.
    pub additional_paths: Vec<PathBuf>,
    /// Commit-pinned tokenizer and tokenizer-configuration paths.
    pub tokenizer_paths: Vec<PathBuf>,
}

impl ResolvedModelArtifacts {
    /// Resolve already-downloaded FastEmbed files without network access.
    ///
    /// FastEmbed uses `HF_HOME` verbatim when set, otherwise its configured
    /// cache directory. This mirrors that behavior and verifies every required
    /// file belongs to the same immutable snapshot revision.
    pub fn resolve(
        model: EmbeddingModel,
        fastembed_cache_dir: &Path,
    ) -> Result<Self, ModelArtifactError> {
        let cache_dir = std::env::var_os("HF_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| fastembed_cache_dir.to_path_buf());
        Self::resolve_in_cache(model, &cache_dir)
    }

    fn resolve_in_cache(
        model: EmbeddingModel,
        cache_dir: &Path,
    ) -> Result<Self, ModelArtifactError> {
        let spec = ModelArtifactSpec::for_model(model)?;
        let repo = Cache::new(cache_dir.to_path_buf()).repo(Repo::model(spec.repository.clone()));
        let model_path = cached_file(&repo, &spec.model_file)?;
        let revision = snapshot_revision(&model_path)?;

        let additional_paths = spec
            .additional_files
            .iter()
            .map(|file| cached_file(&repo, file))
            .collect::<Result<Vec<_>, _>>()?;
        let tokenizer_paths = TOKENIZER_FILES
            .iter()
            .map(|file| cached_file(&repo, file))
            .collect::<Result<Vec<_>, _>>()?;
        for path in additional_paths.iter().chain(&tokenizer_paths) {
            let found = snapshot_revision(path)?;
            if found != revision {
                return Err(ModelArtifactError::MixedRevisions {
                    expected: revision.clone(),
                    found,
                });
            }
        }

        Ok(Self {
            spec,
            revision,
            model_path,
            additional_paths,
            tokenizer_paths,
        })
    }
}

fn cached_file(repo: &hf_hub::CacheRepo, filename: &str) -> Result<PathBuf, ModelArtifactError> {
    repo.get(filename)
        .ok_or_else(|| ModelArtifactError::MissingFile(filename.to_string()))
}

fn snapshot_revision(path: &Path) -> Result<String, ModelArtifactError> {
    let mut components = path.components();
    while let Some(component) = components.next() {
        if component.as_os_str() == "snapshots" {
            return components
                .next()
                .map(|value| value.as_os_str().to_string_lossy().into_owned())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| ModelArtifactError::InvalidSnapshot(path.to_path_buf()));
        }
    }
    Err(ModelArtifactError::InvalidSnapshot(path.to_path_buf()))
}

pub(crate) fn fastembed_model(model: EmbeddingModel) -> FastEmbeddingModel {
    match model {
        EmbeddingModel::ArcticXS => FastEmbeddingModel::SnowflakeArcticEmbedXS,
        EmbeddingModel::ArcticS => FastEmbeddingModel::SnowflakeArcticEmbedS,
        EmbeddingModel::ArcticM => FastEmbeddingModel::SnowflakeArcticEmbedM,
        EmbeddingModel::ArcticMLong => FastEmbeddingModel::SnowflakeArcticEmbedMLong,
        EmbeddingModel::ArcticL => FastEmbeddingModel::SnowflakeArcticEmbedL,
    }
}

/// Failures resolving a model's exact cached artifact generation.
#[derive(Debug)]
pub enum ModelArtifactError {
    /// FastEmbed did not publish metadata for the selected model.
    Metadata(String),
    /// FastEmbed did not declare a pooling operation.
    MissingPooling,
    /// A required file is absent from the current cached snapshot.
    MissingFile(String),
    /// A cached path is not rooted in a commit-addressed snapshot.
    InvalidSnapshot(PathBuf),
    /// Required files unexpectedly resolve to different commits.
    MixedRevisions {
        /// Revision resolved for the ONNX model.
        expected: String,
        /// Revision resolved for another required file.
        found: String,
    },
}

impl std::fmt::Display for ModelArtifactError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Metadata(error) => write!(formatter, "FastEmbed model metadata failed: {error}"),
            Self::MissingPooling => write!(formatter, "FastEmbed model has no pooling method"),
            Self::MissingFile(file) => write!(formatter, "cached model file is missing: {file}"),
            Self::InvalidSnapshot(path) => {
                write!(
                    formatter,
                    "model path is not commit-pinned: {}",
                    path.display()
                )
            }
            Self::MixedRevisions { expected, found } => write!(
                formatter,
                "model artifacts span revisions {expected} and {found}"
            ),
        }
    }
}

impl std::error::Error for ModelArtifactError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arctic_specs_match_fastembed_oracle_metadata() {
        for model in [
            EmbeddingModel::ArcticXS,
            EmbeddingModel::ArcticS,
            EmbeddingModel::ArcticM,
            EmbeddingModel::ArcticMLong,
            EmbeddingModel::ArcticL,
        ] {
            let spec = ModelArtifactSpec::for_model(model).unwrap();
            assert_eq!(spec.dimensions, model.dimensions());
            assert_eq!(spec.pooling, ModelPooling::Cls);
            assert_eq!(spec.output, ModelOutput::OnlyOne);
            assert_eq!(spec.model_file, "onnx/model.onnx");
        }
    }

    #[test]
    fn resolves_every_required_file_from_one_commit_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let spec = ModelArtifactSpec::for_model(EmbeddingModel::ArcticM).unwrap();
        let revision = "0123456789abcdef";
        let repo = Repo::model(spec.repository.clone());
        let repo_root = root.path().join(repo.folder_name());
        std::fs::create_dir_all(repo_root.join("refs")).unwrap();
        std::fs::write(repo_root.join("refs/main"), revision).unwrap();
        for file in std::iter::once(spec.model_file.as_str())
            .chain(spec.additional_files.iter().map(String::as_str))
            .chain(TOKENIZER_FILES)
        {
            let path = repo_root.join("snapshots").join(revision).join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"fixture").unwrap();
        }

        let resolved =
            ResolvedModelArtifacts::resolve_in_cache(EmbeddingModel::ArcticM, root.path()).unwrap();
        assert_eq!(resolved.revision, revision);
        assert_eq!(resolved.tokenizer_paths.len(), TOKENIZER_FILES.len());
        assert!(resolved
            .model_path
            .ends_with("snapshots/0123456789abcdef/onnx/model.onnx"));
    }

    #[test]
    fn rejects_non_snapshot_paths() {
        assert!(matches!(
            snapshot_revision(Path::new("/tmp/model.onnx")),
            Err(ModelArtifactError::InvalidSnapshot(_))
        ));
    }
}

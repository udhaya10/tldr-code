//! Global document-embedding cache management.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use clap::Args;
use serde::Serialize;
use tldr_core::semantic::CacheConfig;

use crate::output::OutputFormat;

/// Remove globally reusable document embeddings.
///
/// This affects every project. Stop all tldr daemons and embedding builders
/// before clearing. Downloaded fastembed model weights are preserved.
#[derive(Debug, Clone, Args)]
pub struct EmbeddingsClearArgs {}

/// Structured result for `tldr embeddings clear`.
#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingsClearOutput {
    /// Operation status.
    pub status: String,
    /// Exact global document-cache directory targeted.
    pub cache_dir: PathBuf,
    /// Number of files and symlinks removed.
    pub files_removed: usize,
    /// Logical bytes freed according to symlink metadata.
    pub bytes_freed: u64,
    /// Human-readable logical size.
    pub size_freed_human: String,
    /// Downloaded model/tokenizer cache is deliberately outside the target.
    pub model_cache_preserved: bool,
    /// Human-readable outcome.
    pub message: String,
}

impl EmbeddingsClearArgs {
    /// Clear the fixed global document-embedding cache.
    pub fn run(&self, format: OutputFormat, quiet: bool) -> anyhow::Result<()> {
        let cache_dir = CacheConfig::default().cache_dir;
        let (files_removed, bytes_freed) = clear_embedding_cache_at(&cache_dir)?;
        let message = if files_removed == 0 {
            "No global embedding cache found; downloaded model weights were preserved".to_string()
        } else {
            format!(
                "Global embedding cache cleared: {files_removed} file(s) removed; downloaded model weights preserved"
            )
        };
        let output = EmbeddingsClearOutput {
            status: "ok".into(),
            cache_dir,
            files_removed,
            bytes_freed,
            size_freed_human: format_bytes(bytes_freed),
            model_cache_preserved: true,
            message,
        };
        print_output(&output, format, quiet)
    }
}

fn clear_embedding_cache_at(cache_dir: &Path) -> io::Result<(usize, u64)> {
    let metadata = match fs::symlink_metadata(cache_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "embedding cache root is not a directory: {}",
                cache_dir.display()
            ),
        ));
    }

    let mut files_removed = 0;
    let mut bytes_freed = 0;
    clear_directory_contents(cache_dir, &mut files_removed, &mut bytes_freed)?;
    fs::remove_dir(cache_dir)?;
    Ok((files_removed, bytes_freed))
}

fn clear_directory_contents(
    directory: &Path,
    files_removed: &mut usize,
    bytes_freed: &mut u64,
) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_dir() {
            clear_directory_contents(&path, files_removed, bytes_freed)?;
            fs::remove_dir(&path)?;
        } else {
            *bytes_freed = bytes_freed.saturating_add(metadata.len());
            fs::remove_file(&path)?;
            *files_removed += 1;
        }
    }
    Ok(())
}

fn print_output(
    output: &EmbeddingsClearOutput,
    format: OutputFormat,
    quiet: bool,
) -> anyhow::Result<()> {
    if quiet {
        return Ok(());
    }
    match format {
        OutputFormat::Json | OutputFormat::Compact => {
            println!("{}", serde_json::to_string_pretty(output)?);
        }
        OutputFormat::Text | OutputFormat::Sarif | OutputFormat::Dot => {
            println!("{} ({})", output.message, output.size_freed_human);
        }
    }
    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::clear_embedding_cache_at;

    #[test]
    fn embedding_cache_clear_preserves_sibling_runtime_state_and_is_idempotent() {
        let root = tempfile::tempdir().expect("cache parent");
        let embeddings = root.path().join("embeddings");
        let fastembed = root.path().join("fastembed");
        let stores = root.path().join("stores/other-project");
        std::fs::create_dir_all(embeddings.join("nested")).expect("embedding dirs");
        std::fs::create_dir_all(&fastembed).expect("model dir");
        std::fs::create_dir_all(&stores).expect("store dir");
        std::fs::write(embeddings.join("cache.redb"), [1_u8, 2, 3]).expect("cache");
        std::fs::write(embeddings.join("nested/recovered.redb"), [4_u8, 5]).expect("recovered");
        std::fs::write(fastembed.join("model.onnx"), [6_u8]).expect("model");
        std::fs::write(stores.join("index.usearch"), [7_u8]).expect("store");

        let (files, bytes) = clear_embedding_cache_at(&embeddings).expect("clear embeddings");

        assert_eq!(files, 2);
        assert_eq!(bytes, 5);
        assert!(!embeddings.exists());
        assert!(fastembed.join("model.onnx").exists());
        assert!(stores.join("index.usearch").exists());
        assert_eq!(
            clear_embedding_cache_at(&embeddings).expect("idempotent clear"),
            (0, 0)
        );
    }

    #[cfg(unix)]
    #[test]
    fn embedding_cache_clear_removes_symlink_without_following_it() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("cache parent");
        let external = tempfile::tempdir().expect("external");
        let embeddings = root.path().join("embeddings");
        std::fs::create_dir(&embeddings).expect("embeddings");
        let sentinel = external.path().join("must-survive");
        std::fs::write(&sentinel, "outside").expect("sentinel");
        symlink(external.path(), embeddings.join("external-link")).expect("symlink");

        let (files, _) = clear_embedding_cache_at(&embeddings).expect("clear embeddings");

        assert_eq!(files, 1);
        assert!(sentinel.exists());
        assert!(!embeddings.exists());
    }
}

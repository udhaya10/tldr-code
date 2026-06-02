//! Embed command - Build and persist the usearch vector store for a project.
//!
//! Replaces the legacy chunk→EmbeddingCache→JSON flow with VectorStore::build+save
//! (TLDR-zxb). The EmbeddingCache is still used internally by VectorStore::build
//! as the content-hash dedup layer (unchanged chunks are not re-embedded).

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use clap::Args;

use tldr_core::config::{find_project_root, TldrConfig};
use tldr_core::semantic::{
    load_or_build_store, store_dir_for, BuildOptions, CacheConfig, ChunkGranularity, EmbedReport,
    EmbeddingModel,
};

use crate::output::{OutputFormat, OutputWriter};

/// Generate embeddings for code
#[derive(Debug, Args)]
pub struct EmbedArgs {
    /// Path to file or directory to embed
    pub path: PathBuf,

    /// Output file (JSON). If not specified, prints to stdout
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Chunking granularity: "file" or "function"
    #[arg(short, long, default_value = "function")]
    pub granularity: String,

    /// Embedding model: arctic-xs, arctic-s, arctic-m, arctic-m-long, arctic-l
    #[arg(short, long)]
    pub model: Option<String>,

    /// Filter by language via file extensions (comma-separated, e.g., `--langs rs,py`).
    ///
    /// Values are parsed by `Language::from_extension`, which accepts file
    /// extensions such as `rs`, `py`, `ts`, `go`, `java`, `rb`, `kt`, `cpp`.
    /// Language names (`rust`, `python`) are NOT accepted here; use the
    /// global `--lang <LANG>` flag above for name-based single-language
    /// selection. Passing an unknown extension silently drops that entry
    /// from the filter.
    #[arg(long = "langs", value_delimiter = ',')]
    pub langs: Option<Vec<String>>,

    /// Disable embedding cache
    #[arg(long)]
    pub no_cache: bool,
}

impl EmbedArgs {
    /// Run the embed command
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let writer = OutputWriter::new(format, quiet);
        let start = Instant::now();

        // Resolve model: CLI flag > config > built-in default
        let project_root = find_project_root(&self.path);
        let config = TldrConfig::resolve(project_root.as_deref());
        let model = EmbeddingModel::resolve(self.model.as_deref(), &config)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        let granularity = match self.granularity.as_str() {
            "file" => ChunkGranularity::File,
            "function" => ChunkGranularity::Function,
            _ => {
                return Err(anyhow::anyhow!(
                    "Invalid granularity '{}'. Use 'file' or 'function'.",
                    self.granularity
                ))
            }
        };

        writer.progress(&format!(
            "Building vector store for {} ({:?} granularity, {:?} model)...",
            self.path.display(),
            granularity,
            model
        ));

        let build_opts = BuildOptions {
            model,
            granularity,
            languages: self.langs.clone(),
            show_progress: !quiet,
            use_cache: !self.no_cache,
        };

        let cache_config = if self.no_cache {
            None
        } else {
            Some(CacheConfig::default())
        };

        let store_dir = store_dir_for(&self.path);
        let store = load_or_build_store(&self.path, &store_dir, &build_opts, cache_config)?;

        let total_chunks = store.len();
        let latency_ms = start.elapsed().as_millis() as u64;

        let report = EmbedReport {
            path: self.path.clone(),
            model,
            granularity,
            chunks_embedded: total_chunks,
            chunks_cached: 0,
            chunks: None,
            latency_ms,
        };

        writer.progress(&format!(
            "Built store with {} chunks in {}ms (saved to {})",
            total_chunks,
            latency_ms,
            store_dir.display()
        ));

        if let Some(ref output_path) = self.output {
            let file = std::fs::File::create(output_path)?;
            serde_json::to_writer_pretty(file, &report)?;
            writer.progress(&format!("Output written to {}", output_path.display()));
        } else {
            writer.write(&report)?;
        }

        Ok(())
    }
}

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
    EmbeddingModel, GenerationManager, GenerationSelection,
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

    /// Write build instrumentation (per-batch shape, cache accounting, RSS
    /// timeline + peak, phase boundaries, throughput) as JSON to this path
    /// (TLDR-9bxa.1). Forces a fresh build (a loaded store carries no
    /// metrics), so a report is always emitted — `--no-cache` is not required
    /// for metrics (use it only to also bypass the dedup cache).
    #[arg(long)]
    pub metrics: Option<PathBuf>,

    /// Complete generation to serve after the build: active, previous, or a number.
    #[arg(long, default_value = "active")]
    pub generation: String,
}

impl EmbedArgs {
    /// Run the embed command
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        // ADR-10 / DaemonRoute discipline (TLDR-ami): `embed` builds the project's
        // usearch store at `store_dir_for(path)` — the SAME store the daemon's
        // IndexManager builds, saves, and serves. If a live daemon covers this
        // path, running a standalone build here makes TWO concurrent writers of
        // one store: rebuild livelock + cache flush race (observed 2026-06-28 as
        // a hung `embed` looping on "source changed; rebuilding" while the daemon
        // ballooned to 20GB). Refuse loudly and point at the daemon-owned build.
        //
        // No `--oneshot` override on purpose (unlike read-only query commands):
        // a local build WRITES the shared `store_dir`, so it cannot be made safe
        // while the daemon is live — the explicit escape is to stop the daemon.
        if let Some(project) = crate::commands::daemon_router::daemon_project_for(&self.path) {
            anyhow::bail!(
                "a tldr daemon owns this project's index ({}) and builds it for you — \
                 standalone `tldr embed` here would be a second writer of the same store \
                 (rebuild livelock / cache race).\n\
                 \n\
                 Instead:\n  \
                 • run `tldr warm {}` to (re)build the index via the daemon, or\n  \
                 • `tldr daemon stop` first if you really want a one-off local build.",
                project.display(),
                self.path.display(),
            );
        }

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
            collect_metrics: self.metrics.is_some(),
        };

        let cache_config = if self.no_cache {
            None
        } else {
            Some(CacheConfig::default())
        };

        let store_dir = store_dir_for(&self.path);
        let selection = GenerationSelection::parse(&self.generation).map_err(anyhow::Error::msg)?;
        if self.metrics.is_some() && selection != GenerationSelection::Active {
            anyhow::bail!("--metrics can only be combined with --generation active");
        }
        let mut store = load_or_build_store(&self.path, &store_dir, &build_opts, cache_config)?;
        let identity = tldr_core::semantic::store_search::manifest_id_for(&self.path, &build_opts);
        let generations = GenerationManager::open(&store_dir)?;
        store = match selection {
            GenerationSelection::Active => store,
            GenerationSelection::Previous => generations
                .select_previous(&identity)?
                .ok_or_else(|| anyhow::anyhow!("no previous complete generation is retained"))?,
            GenerationSelection::Number(generation) => generations.select(generation, &identity)?,
        };

        // TLDR-9bxa.1: emit the build-instrumentation report. `--metrics` sets
        // collect_metrics, which forces a fresh build, so a report is always
        // present here (a loaded store carries no metrics, but we never load
        // when collecting).
        if let Some(ref metrics_path) = self.metrics {
            let report = store.build_metrics().ok_or_else(|| {
                anyhow::anyhow!(
                    "--metrics was set but no build metrics were collected \
                     (expected a forced fresh build)"
                )
            })?;
            let file = std::fs::File::create(metrics_path)?;
            serde_json::to_writer_pretty(file, report)?;
            writer.progress(&format!("Metrics written to {}", metrics_path.display()));
        }

        let total_chunks = store.len();
        let latency_ms = start.elapsed().as_millis() as u64;

        let report = EmbedReport {
            path: self.path.clone(),
            model,
            granularity,
            chunks_embedded: total_chunks,
            chunks_cached: 0,
            files_indexed: store.build_stats().files_indexed,
            files_skipped: store.build_stats().files_skipped,
            files_unsupported: store.build_stats().files_unsupported,
            files_oversized: store.build_stats().files_oversized,
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

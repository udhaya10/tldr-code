//! Process-isolated full-corpus FastEmbed/fixed-shape cold-build comparison.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tldr_core::semantic::vector_store::VectorStore;
use tldr_core::semantic::{BuildOptions, DocumentEmbeddingBackend, EmbeddingModel};

const MAX_RSS_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_WALL_REGRESSION: f64 = 0.10;
const MAX_PLATEAU_SPREAD_BYTES: u64 = 64 * 1024 * 1024;
const PLATEAU_SAMPLES: usize = 10;

#[derive(Debug, Serialize, Deserialize)]
struct ColdBuildSummary {
    backend: DocumentEmbeddingBackend,
    model: EmbeddingModel,
    root: PathBuf,
    chunks: usize,
    duration_ms: u64,
    embed_latency_ms: u64,
    peak_rss_bytes: u64,
    final_rss_bytes: u64,
    final_window_spread_bytes: u64,
    exact_shapes: Vec<(usize, usize)>,
    embeddings_per_second: f64,
}

#[derive(Debug, Serialize)]
struct ColdBuildComparison {
    oracle: ColdBuildSummary,
    candidate: ColdBuildSummary,
    wall_regression_fraction: f64,
    rss_passed: bool,
    plateau_passed: bool,
    throughput_passed: bool,
    shape_set_passed: bool,
    passed: bool,
}

fn main() -> Result<()> {
    std::env::set_var("TLDR_QUIET", "1");
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments
        .first()
        .is_some_and(|argument| argument == "--worker")
    {
        let backend = parse_backend(
            arguments
                .get(1)
                .context("--worker requires fastembed or fixed-shape")?,
        )?;
        let root = PathBuf::from(
            arguments
                .get(2)
                .context("--worker requires a corpus path")?,
        );
        let model = arguments
            .get(3)
            .map(|value| EmbeddingModel::parse(value).map_err(anyhow::Error::msg))
            .transpose()?
            .unwrap_or_default();
        let summary = run_worker(&root, model, backend)?;
        println!("{}", serde_json::to_string(&summary)?);
        return Ok(());
    }

    let root = PathBuf::from(
        arguments
            .first()
            .context("usage: fixed_shape_cold_build <corpus-path> [model]")?,
    );
    let model = arguments
        .get(1)
        .map(|value| EmbeddingModel::parse(value).map_err(anyhow::Error::msg))
        .transpose()?
        .unwrap_or_default();
    let oracle = launch_worker(&root, model, DocumentEmbeddingBackend::FastEmbed)?;
    let candidate = launch_worker(&root, model, DocumentEmbeddingBackend::FixedShapeOrt)?;
    let wall_regression_fraction =
        (candidate.duration_ms as f64 / oracle.duration_ms.max(1) as f64 - 1.0).max(0.0);
    let rss_passed = candidate.peak_rss_bytes <= MAX_RSS_BYTES;
    let plateau_passed = candidate.final_window_spread_bytes <= MAX_PLATEAU_SPREAD_BYTES;
    let throughput_passed = wall_regression_fraction <= MAX_WALL_REGRESSION;
    let shape_set_passed =
        candidate.exact_shapes == vec![(8, 512), (14, 384), (32, 256), (64, 128)];
    let passed = rss_passed && plateau_passed && throughput_passed && shape_set_passed;
    let comparison = ColdBuildComparison {
        oracle,
        candidate,
        wall_regression_fraction,
        rss_passed,
        plateau_passed,
        throughput_passed,
        shape_set_passed,
        passed,
    };
    println!("{}", serde_json::to_string_pretty(&comparison)?);
    if !comparison.passed {
        bail!("fixed-shape full cold-build gate failed");
    }
    Ok(())
}

fn launch_worker(
    root: &Path,
    model: EmbeddingModel,
    backend: DocumentEmbeddingBackend,
) -> Result<ColdBuildSummary> {
    eprintln!("Running isolated {} cold build...", backend.as_str());
    let executable = std::env::current_exe().context("cannot resolve benchmark executable")?;
    let output = Command::new(executable)
        .arg("--worker")
        .arg(backend.as_str())
        .arg(root)
        .arg(model_alias(model))
        .env("TLDR_QUIET", "1")
        .output()
        .with_context(|| format!("failed to launch {} worker", backend.as_str()))?;
    if !output.status.success() {
        bail!(
            "{} worker failed:\n{}",
            backend.as_str(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "{} worker returned invalid JSON: {}",
            backend.as_str(),
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn run_worker(
    root: &Path,
    model: EmbeddingModel,
    backend: DocumentEmbeddingBackend,
) -> Result<ColdBuildSummary> {
    let options = BuildOptions {
        model,
        show_progress: false,
        use_cache: false,
        collect_metrics: true,
        ..BuildOptions::default()
    };
    let store = VectorStore::build_with_backend(root, &options, None, backend)?;
    let metrics = store
        .build_metrics()
        .context("instrumented build returned no metrics")?;
    let mut exact_shapes = metrics
        .batches
        .iter()
        .filter_map(|batch| Some((batch.tensor_batch_size?, batch.sequence_tokens?)))
        .collect::<Vec<_>>();
    exact_shapes.sort_unstable();
    exact_shapes.dedup();
    let tail_start = metrics.rss.timeline.len().saturating_sub(PLATEAU_SAMPLES);
    let final_window = &metrics.rss.timeline[tail_start..];
    let minimum = final_window
        .iter()
        .map(|sample| sample.rss_bytes)
        .min()
        .or(metrics.rss.final_bytes)
        .unwrap_or(0);
    let maximum = final_window
        .iter()
        .map(|sample| sample.rss_bytes)
        .max()
        .or(metrics.rss.final_bytes)
        .unwrap_or(0);
    Ok(ColdBuildSummary {
        backend,
        model,
        root: root.to_path_buf(),
        chunks: metrics.chunks_total,
        duration_ms: metrics.duration_ms,
        embed_latency_ms: metrics.embed_latency_ms,
        peak_rss_bytes: metrics.rss.peak_bytes.unwrap_or(0),
        final_rss_bytes: metrics.rss.final_bytes.unwrap_or(0),
        final_window_spread_bytes: maximum.saturating_sub(minimum),
        exact_shapes,
        embeddings_per_second: metrics.throughput.embeddings_per_second,
    })
}

fn parse_backend(value: &str) -> Result<DocumentEmbeddingBackend> {
    match value {
        "fastembed" => Ok(DocumentEmbeddingBackend::FastEmbed),
        "fixed-shape" | "fixed_shape_ort" => Ok(DocumentEmbeddingBackend::FixedShapeOrt),
        _ => bail!("unknown backend {value:?}"),
    }
}

fn model_alias(model: EmbeddingModel) -> &'static str {
    match model {
        EmbeddingModel::ArcticXS => "arctic-xs",
        EmbeddingModel::ArcticS => "arctic-s",
        EmbeddingModel::ArcticM => "arctic-m",
        EmbeddingModel::ArcticMLong => "arctic-m-long",
        EmbeddingModel::ArcticL => "arctic-l",
    }
}

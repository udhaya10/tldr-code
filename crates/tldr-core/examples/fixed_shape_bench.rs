//! Measure fixed-shape latency, throughput, and RSS against FastEmbed.
//!
//! Run with:
//! `cargo run --release -p tldr-core --example fixed_shape_bench -- --iterations 4`

use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use tldr_core::semantic::{
    Embedder, EmbeddingBackend, EmbeddingModel, FixedShapeBatch, FixedShapeOrtBackend,
    ModelPerformanceReport, OrtBackendConfig, PerformanceGate, PerformanceMatrixReport,
    RssPlateauReport, SequenceBucket, ShapePerformanceReport,
};
use tldr_core::util::current_rss_bytes;

const DEFAULT_ITERATIONS: usize = 4;
const RSS_SAMPLE_INTERVAL: Duration = Duration::from_millis(10);
const SHAPES: [SequenceBucket; 4] = [
    SequenceBucket::Tokens128,
    SequenceBucket::Tokens256,
    SequenceBucket::Tokens384,
    SequenceBucket::Tokens512,
];
const MODELS: [EmbeddingModel; 5] = [
    EmbeddingModel::ArcticXS,
    EmbeddingModel::ArcticS,
    EmbeddingModel::ArcticM,
    EmbeddingModel::ArcticMLong,
    EmbeddingModel::ArcticL,
];

fn main() -> Result<()> {
    std::env::set_var("TLDR_QUIET", "1");
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let iterations = parse_iterations(&args)?;
    if let Some(worker_index) = args.iter().position(|argument| argument == "--worker") {
        let model_name = args
            .get(worker_index + 1)
            .context("--worker requires a model name")?;
        let model = EmbeddingModel::parse(model_name).map_err(anyhow::Error::msg)?;
        let report = run_worker(model, iterations, PerformanceGate::default())?;
        println!("{}", serde_json::to_string(&report)?);
        return Ok(());
    }

    let gate = PerformanceGate::default();
    let executable = std::env::current_exe().context("cannot resolve benchmark executable")?;
    let mut reports = Vec::with_capacity(MODELS.len());
    for model in MODELS {
        eprintln!(
            "Benchmarking {} in an isolated worker...",
            model.model_name()
        );
        let output = Command::new(&executable)
            .arg("--worker")
            .arg(model_alias(model))
            .arg("--iterations")
            .arg(iterations.to_string())
            .env("TLDR_QUIET", "1")
            .output()
            .with_context(|| format!("failed to launch worker for {}", model.model_name()))?;
        if !output.status.success() {
            bail!(
                "{} worker failed:\n{}",
                model.model_name(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let report: ModelPerformanceReport =
            serde_json::from_slice(&output.stdout).with_context(|| {
                format!(
                    "{} worker returned invalid JSON: {}",
                    model.model_name(),
                    String::from_utf8_lossy(&output.stdout)
                )
            })?;
        reports.push(report);
    }
    let report = PerformanceMatrixReport {
        gate,
        models: reports,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.passed() {
        bail!("fixed-shape performance matrix failed");
    }
    Ok(())
}

fn parse_iterations(args: &[String]) -> Result<usize> {
    let iterations = args
        .iter()
        .position(|argument| argument == "--iterations")
        .map(|index| {
            args.get(index + 1)
                .context("--iterations requires a value")?
                .parse::<usize>()
                .context("--iterations must be a positive integer")
        })
        .transpose()?
        .unwrap_or(DEFAULT_ITERATIONS);
    if iterations < PerformanceGate::default().plateau_window {
        bail!(
            "--iterations must be at least {} for the RSS plateau gate",
            PerformanceGate::default().plateau_window
        );
    }
    Ok(iterations)
}

fn run_worker(
    model: EmbeddingModel,
    iterations: usize,
    gate: PerformanceGate,
) -> Result<ModelPerformanceReport> {
    let mut oracle = Embedder::new(model)
        .with_context(|| format!("failed to load FastEmbed oracle for {}", model.model_name()))?;
    let artifacts = oracle
        .model_artifacts()
        .with_context(|| format!("model artifacts unavailable for {}", model.model_name()))?
        .clone();
    let revision = artifacts.revision.clone();
    let tokenizer = oracle
        .token_budget()
        .with_context(|| format!("tokenizer unavailable for {}", model.model_name()))?;
    let planner = tokenizer
        .fixed_shape_planner("test")
        .map_err(anyhow::Error::msg)?;

    let mut work = Vec::with_capacity(SHAPES.len());
    for bucket in SHAPES {
        let text = exact_length_text(tokenizer, bucket.sequence_length())?;
        let template = tokenizer
            .tokenize_fixed_shape(0, &text)
            .map_err(anyhow::Error::msg)?;
        let inputs = (0..bucket.batch_size())
            .map(|request_index| {
                let mut input = template.clone();
                input.request_index = request_index;
                input
            })
            .collect::<Vec<_>>();
        let mut batches = planner.plan(inputs)?;
        if batches.len() != 1
            || batches[0].shape() != (bucket.batch_size(), bucket.sequence_length())
            || batches[0].real_rows() != bucket.batch_size()
        {
            bail!(
                "{} fixture produced unexpected {:?} batches",
                model.model_name(),
                batches
                    .iter()
                    .map(FixedShapeBatch::shape)
                    .collect::<Vec<_>>()
            );
        }
        work.push(ShapeWork {
            bucket,
            text,
            batch: batches.remove(0),
            oracle_ms: Vec::with_capacity(iterations),
            fixed_ms: Vec::with_capacity(iterations),
        });
    }

    for shape in &mut work {
        run_oracle(&mut oracle, shape)?;
        for _ in 0..iterations {
            shape.oracle_ms.push(time_oracle(&mut oracle, shape)?);
        }
    }
    drop(oracle);

    let baseline_rss_bytes =
        current_rss_bytes().context("current RSS is unavailable on this platform")?;
    let sampler = RssSampler::start(baseline_rss_bytes);
    let mut candidate = FixedShapeOrtBackend::new(artifacts, OrtBackendConfig::default())
        .map_err(anyhow::Error::msg)?;
    for shape in &work {
        run_fixed(&mut candidate, shape)?;
    }

    let mut cycle_end_rss_bytes = Vec::with_capacity(iterations);
    for cycle in 0..iterations {
        for offset in 0..work.len() {
            let index = (cycle + offset) % work.len();
            let elapsed_ms = time_fixed(&mut candidate, &work[index])?;
            work[index].fixed_ms.push(elapsed_ms);
        }
        cycle_end_rss_bytes
            .push(current_rss_bytes().context("current RSS disappeared during benchmark")?);
    }
    let sampled_peak_rss_bytes = sampler.stop()?;

    let observations = candidate.shape_observations();
    if observations.len() != SHAPES.len()
        || observations.iter().any(|observation| {
            observation.executions != (iterations + 1) as u64
                || !SHAPES.iter().any(|bucket| {
                    observation.batch == bucket.batch_size()
                        && observation.sequence == bucket.sequence_length()
                })
        })
    {
        bail!(
            "{} emitted an unexpected measured shape set: {observations:?}",
            model.model_name()
        );
    }

    let shapes = work
        .iter()
        .map(|shape| {
            ShapePerformanceReport::from_samples(
                shape.bucket.sequence_length(),
                shape.bucket.batch_size(),
                &shape.oracle_ms,
                &shape.fixed_ms,
                gate,
            )
            .map_err(anyhow::Error::msg)
        })
        .collect::<Result<Vec<_>>>()?;
    let rss = RssPlateauReport::from_samples(
        baseline_rss_bytes,
        sampled_peak_rss_bytes,
        cycle_end_rss_bytes,
        gate,
    )
    .map_err(anyhow::Error::msg)?;
    Ok(ModelPerformanceReport {
        model: model.model_name().to_string(),
        revision,
        iterations,
        shapes,
        rss,
    })
}

struct ShapeWork {
    bucket: SequenceBucket,
    text: String,
    batch: FixedShapeBatch,
    oracle_ms: Vec<f64>,
    fixed_ms: Vec<f64>,
}

fn exact_length_text(
    tokenizer: &tldr_core::semantic::TokenBudget,
    target: usize,
) -> Result<String> {
    let special_tokens = tokenizer.token_count("").map_err(anyhow::Error::msg)?;
    if special_tokens > target {
        bail!("tokenizer special tokens exceed target sequence length {target}");
    }
    let mut text = std::iter::repeat_n("a", target - special_tokens)
        .collect::<Vec<_>>()
        .join(" ");
    let mut actual = tokenizer.token_count(&text).map_err(anyhow::Error::msg)?;
    while actual < target {
        text.push_str(" a");
        actual = tokenizer.token_count(&text).map_err(anyhow::Error::msg)?;
    }
    if actual != target {
        bail!("could not synthesize exactly {target} tokens; produced {actual}");
    }
    Ok(text)
}

fn run_oracle(oracle: &mut Embedder, shape: &ShapeWork) -> Result<()> {
    let outputs = oracle.embed_batch_with_size(
        vec![shape.text.as_str(); shape.bucket.batch_size()],
        Some(shape.bucket.batch_size()),
    )?;
    if outputs.len() != shape.bucket.batch_size() {
        bail!("FastEmbed returned an unexpected output count");
    }
    Ok(())
}

fn time_oracle(oracle: &mut Embedder, shape: &ShapeWork) -> Result<f64> {
    let start = Instant::now();
    run_oracle(oracle, shape)?;
    Ok(start.elapsed().as_secs_f64() * 1_000.0)
}

fn run_fixed(candidate: &mut FixedShapeOrtBackend, shape: &ShapeWork) -> Result<()> {
    let outputs = candidate
        .embed_prepared(std::slice::from_ref(&shape.batch))
        .map_err(anyhow::Error::msg)?;
    if outputs.len() != shape.bucket.batch_size() {
        bail!("fixed-shape backend returned an unexpected output count");
    }
    Ok(())
}

fn time_fixed(candidate: &mut FixedShapeOrtBackend, shape: &ShapeWork) -> Result<f64> {
    let start = Instant::now();
    run_fixed(candidate, shape)?;
    Ok(start.elapsed().as_secs_f64() * 1_000.0)
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

struct RssSampler {
    running: Arc<AtomicBool>,
    maximum: Arc<AtomicU64>,
    handle: JoinHandle<()>,
}

impl RssSampler {
    fn start(initial_rss_bytes: u64) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let maximum = Arc::new(AtomicU64::new(initial_rss_bytes));
        let thread_running = Arc::clone(&running);
        let thread_maximum = Arc::clone(&maximum);
        let handle = thread::spawn(move || {
            while thread_running.load(Ordering::Relaxed) {
                if let Some(rss_bytes) = current_rss_bytes() {
                    thread_maximum.fetch_max(rss_bytes, Ordering::Relaxed);
                }
                thread::sleep(RSS_SAMPLE_INTERVAL);
            }
        });
        Self {
            running,
            maximum,
            handle,
        }
    }

    fn stop(self) -> Result<u64> {
        self.running.store(false, Ordering::Relaxed);
        self.handle
            .join()
            .map_err(|_| anyhow::anyhow!("RSS sampler thread panicked"))?;
        Ok(self.maximum.load(Ordering::Relaxed))
    }
}

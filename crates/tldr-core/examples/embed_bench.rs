//! Batch-size micro-benchmark (TLDR-blm / TLDR-3rh). Isolates the embed step:
//! chunks a dir, logs the chunk length distribution (the crux of fastembed's
//! BatchLongest padding behavior), then prints RSS + elapsed time at each
//! phase boundary (model load, warmup, batch 32, batch 256) so a memory jump
//! can be attributed to a specific phase instead of assumed.
//!
//! Run: cargo run -p tldr-core --release --features semantic --example embed_bench -- <dir>
//!
//! The 256/32 ratio is valid even under CPU contention (both halves run back to
//! back under the same load). Absolute times inflate under load; the ratio and
//! the length distribution do not.

use std::path::PathBuf;
use std::time::Instant;

use tldr_core::semantic::{chunk_code, ChunkGranularity, ChunkOptions, Embedder, EmbeddingModel};

/// Current process RSS in KB via `ps`. Good enough for phase checkpoints
/// (sampled synchronously at a call boundary, not polled externally), and
/// avoids adding a `libc`/mach dependency to this example. Production RSS
/// reporting lives in `tldr-cli/src/commands/daemon/rss.rs`.
fn rss_kb() -> Option<u64> {
    let pid = std::process::id().to_string();
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn checkpoint(label: &str, t0: Instant) {
    let rss = rss_kb()
        .map(|kb| format!("{:.2} GB", kb as f64 / 1_048_576.0))
        .unwrap_or_else(|| "?".into());
    println!(
        "[{:>7.1}s] {:<32} rss={}",
        t0.elapsed().as_secs_f64(),
        label,
        rss
    );
}

fn main() {
    let t0 = Instant::now();
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/semantic"));

    checkpoint("start", t0);
    eprintln!("chunking {} ...", root.display());
    let opts = ChunkOptions {
        granularity: ChunkGranularity::Function,
        ..Default::default()
    };
    let res = chunk_code(&root, &opts).expect("chunk_code failed");
    let texts: Vec<&str> = res.chunks.iter().map(|c| c.content.as_str()).collect();
    let n = texts.len();
    if n == 0 {
        println!("no chunks; pick a dir with source files");
        return;
    }
    checkpoint(&format!("chunked ({n} chunks)"), t0);

    // Char length as a token proxy. fastembed pads each batch to the LONGEST
    // member (capped at 512 tokens ~ a few thousand chars). If the distribution
    // is bimodal (small p50, large max), big batches waste compute padding short
    // chunks up to the longest one.
    let mut lens: Vec<usize> = texts.iter().map(|t| t.len()).collect();
    lens.sort_unstable();
    let pct = |p: f64| lens[(((n as f64) * p) as usize).min(n - 1)];
    println!("chunks:   {n}");
    println!(
        "char len: min={} p50={} p90={} p99={} max={}",
        lens[0],
        pct(0.50),
        pct(0.90),
        pct(0.99),
        lens[n - 1]
    );

    eprintln!("loading model (ArcticM) ...");
    let mut emb = Embedder::new(EmbeddingModel::ArcticM).expect("embedder init failed");
    checkpoint("model loaded", t0);

    // Warm up the model/session once so neither timed run pays first-call cost.
    let _ = emb.embed_batch_with_size(texts[..n.min(8)].to_vec(), Some(32));
    checkpoint("warmup (batch 32, 8 texts) done", t0);

    let t = Instant::now();
    emb.embed_batch_with_size(texts.clone(), Some(32))
        .expect("batch32 failed");
    let d32 = t.elapsed().as_secs_f64();
    checkpoint("batch 32 (full corpus) done", t0);

    let t = Instant::now();
    emb.embed_batch_with_size(texts.clone(), None)
        .expect("batch256 failed");
    let d256 = t.elapsed().as_secs_f64();
    checkpoint("batch 256 (fastembed default) done", t0);

    println!(
        "\nbatch  32: {:.1}s  ({:.1} ms/chunk)",
        d32,
        d32 * 1000.0 / n as f64
    );
    println!(
        "batch 256: {:.1}s  ({:.1} ms/chunk)",
        d256,
        d256 * 1000.0 / n as f64
    );
    println!(
        "ratio 256/32: {:.2}x  (>1 means 256 is slower)",
        d256 / d32.max(0.001)
    );
}

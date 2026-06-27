//! Batch-size micro-benchmark (TLDR-blm). Isolates the embed step: chunks a dir,
//! logs the chunk length distribution (the crux of fastembed's BatchLongest
//! padding behavior), then times embedding the SAME chunks at batch 32 vs 256.
//!
//! Run: cargo run -p tldr-core --release --example embed_bench -- crates/tldr-core/src/semantic
//!
//! The 256/32 ratio is valid even under CPU contention (both halves run back to
//! back under the same load). Absolute times inflate under load; the ratio and
//! the length distribution do not.

use std::path::PathBuf;
use std::time::Instant;

use tldr_core::semantic::{chunk_code, ChunkGranularity, ChunkOptions, Embedder, EmbeddingModel};

fn main() {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/semantic"));

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

    // Warm up the model/session once so neither timed run pays first-call cost.
    let _ = emb.embed_batch(texts[..n.min(8)].to_vec(), true);

    // batch 32  (embed_batch maps show_progress=true  -> Some(32))
    let t = Instant::now();
    emb.embed_batch(texts.clone(), true)
        .expect("batch32 failed");
    let d32 = t.elapsed().as_secs_f64();

    // batch 256 (embed_batch maps show_progress=false -> None -> 256)
    let t = Instant::now();
    emb.embed_batch(texts.clone(), false)
        .expect("batch256 failed");
    let d256 = t.elapsed().as_secs_f64();

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

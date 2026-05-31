//! Semantic search eval harness (TLDR-25p, epic TLDR-blm).
//!
//! Hand-authored gold set of (natural-language query -> expected file [+ function])
//! cases over THIS codebase, used to measure semantic-search quality as a single
//! number before/after the Phase 1 quality work (enrichment wiring, Arctic prefix,
//! hybrid). Reports recall@5, recall@10, and MRR.
//!
//! Run with:
//!   cargo run -p tldr-core --release --example semantic_eval
//!   cargo run -p tldr-core --release --example semantic_eval -- crates/tldr-core/src
//!
//! The first run downloads the embedding model (~110MB) to the fastembed cache.
//! Caching is left ON: the embedding cache key will be invalidated by the
//! recipe-version tag added in TLDR-lwg, so cross-version comparisons stay honest
//! while same-version re-runs stay fast.

use std::path::PathBuf;
use std::time::Instant;

use tldr_core::semantic::{BuildOptions, IndexSearchOptions, SemanticIndex};

/// (query, expected file path suffix, optional expected function name)
///
/// Targets are spread across the semantic/, search/, ast/, cfg/, dfg/ subsystems
/// and deliberately avoid code slated for deletion (e.g. embedding_client.rs).
const GOLD: &[(&str, &str, Option<&str>)] = &[
    (
        "reciprocal rank fusion to combine two rankings",
        "search/hybrid.rs",
        Some("fuse_rrf"),
    ),
    (
        "cosine similarity between two embedding vectors",
        "semantic/similarity.rs",
        Some("cosine_similarity"),
    ),
    (
        "find the top k most similar vectors to a query",
        "semantic/similarity.rs",
        Some("top_k_similar"),
    ),
    (
        "normalize a vector to unit length",
        "semantic/similarity.rs",
        Some("normalize"),
    ),
    (
        "build a semantic embedding index from a directory",
        "semantic/index.rs",
        Some("build"),
    ),
    ("bm25 keyword ranking score", "search/bm25.rs", None),
    (
        "generate embeddings for a batch of texts",
        "semantic/embedder.rs",
        Some("embed_batch"),
    ),
    (
        "split source code into function level chunks",
        "semantic/chunker.rs",
        None,
    ),
    (
        "persist embeddings to a cache with invalidation",
        "semantic/cache.rs",
        None,
    ),
    (
        "build the enriched text used for embedding a code unit",
        "semantic/enrichment.rs",
        Some("build_embedding_text"),
    ),
    (
        "tokenize code identifiers splitting camelCase and snake_case",
        "search/tokenizer.rs",
        None,
    ),
    (
        "parse import statements from a source file",
        "ast/imports.rs",
        None,
    ),
    (
        "extract a control flow graph from a function",
        "cfg/extractor.rs",
        None,
    ),
    (
        "compute reaching definitions in data flow analysis",
        "dfg/reaching.rs",
        None,
    ),
];

const TOP_K: usize = 10;

fn norm(p: &str) -> String {
    p.replace('\\', "/")
}

fn main() {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"));

    eprintln!("Building semantic index over {} ...", root.display());
    let build_start = Instant::now();
    let mut index = SemanticIndex::build(
        &root,
        BuildOptions {
            show_progress: false,
            ..Default::default()
        },
        Some(Default::default()),
    )
    .expect("index build failed");
    eprintln!("Index built in {:.1}s", build_start.elapsed().as_secs_f64());

    let opts = IndexSearchOptions {
        top_k: TOP_K,
        threshold: 0.0, // rank everything; we measure ranking, not a cutoff
        include_snippet: false,
        ..Default::default()
    };

    let mut hits_at_5 = 0usize;
    let mut hits_at_10 = 0usize;
    let mut mrr_sum = 0.0f64;

    println!(
        "\n{:<58} {:>5} {:>6} {}",
        "query", "rank", "fn?", "expected"
    );
    println!("{}", "-".repeat(96));

    for (query, want_file, want_fn) in GOLD {
        let report = index.search(query, &opts).expect("search failed");
        let want_file_n = norm(want_file);

        // First result whose file path matches the expected suffix.
        let mut rank: Option<usize> = None;
        let mut fn_matched = false;
        for (i, r) in report.results.iter().enumerate() {
            let fp = norm(&r.file_path.to_string_lossy());
            if fp.ends_with(&want_file_n) {
                rank = Some(i + 1);
                if let Some(wf) = want_fn {
                    fn_matched = r.function_name.as_deref() == Some(*wf);
                }
                break;
            }
        }

        let rank_str = rank.map(|r| r.to_string()).unwrap_or_else(|| "—".into());
        let fn_str = match (want_fn, rank.is_some(), fn_matched) {
            (None, _, _) => "n/a",
            (Some(_), true, true) => "yes",
            (Some(_), true, false) => "no",
            (Some(_), false, _) => "—",
        };
        println!(
            "{:<58} {:>5} {:>6} {}",
            &query[..query.len().min(58)],
            rank_str,
            fn_str,
            want_file
        );

        if let Some(r) = rank {
            if r <= 5 {
                hits_at_5 += 1;
            }
            if r <= 10 {
                hits_at_10 += 1;
            }
            mrr_sum += 1.0 / r as f64;
        }
    }

    let n = GOLD.len() as f64;
    println!("{}", "-".repeat(96));
    println!("\nGold cases:  {}", GOLD.len());
    println!("Recall@5:    {:.3}  ({}/{})", hits_at_5 as f64 / n, hits_at_5, GOLD.len());
    println!("Recall@10:   {:.3}  ({}/{})", hits_at_10 as f64 / n, hits_at_10, GOLD.len());
    println!("MRR:         {:.3}", mrr_sum / n);
}

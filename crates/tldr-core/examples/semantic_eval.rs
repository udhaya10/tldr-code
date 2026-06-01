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

use tldr_core::config::TldrConfig;
use tldr_core::semantic::{BuildOptions, EmbeddingModel, IndexSearchOptions, SemanticIndex};
use tldr_core::{hybrid_search_with_index, Language};

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
    // ---------------------------------------------------------------------
    // TLDR-6h3: expansion from 14 -> ~52 cases for statistically trustworthy
    // deltas. Targets are real, doc-verified public functions spread across the
    // analysis/, callgraph/, ast/, cfg/, dfg/, quality/, security/, context/,
    // git/, metrics/, alias/, inheritance/, patterns/, diagnostics/ subsystems.
    // Queries paraphrase each function's PURPOSE in user terms (never the symbol
    // name) to exercise the modality gap that the Arctic query prefix targets.
    // Scoring is file-level (ends_with); the Option<fn> is a diagnostic column.
    // ---------------------------------------------------------------------
    (
        "which tests are affected when these files change",
        "analysis/change_impact.rs",
        Some("change_impact"),
    ),
    (
        "find unreachable functions that are never called",
        "analysis/dead.rs",
        Some("dead_code_analysis"),
    ),
    (
        "detect cycles among mutually recursive functions",
        "analysis/tarjan.rs",
        Some("detect_cycles"),
    ),
    (
        "find groups of nodes all reachable from each other",
        "analysis/tarjan.rs",
        Some("find_sccs"),
    ),
    (
        "find copy pasted duplicate code blocks",
        "analysis/clones/detect.rs",
        None,
    ),
    (
        "which files import a given module",
        "analysis/importers.rs",
        Some("find_importers"),
    ),
    (
        "analyze the overall architecture of a codebase",
        "analysis/architecture.rs",
        Some("architecture_analysis"),
    ),
    (
        "build def use chains linking definitions to their uses",
        "dfg/reaching.rs",
        Some("build_def_use_chains"),
    ),
    (
        "which variable definitions reach a particular line",
        "dfg/reaching.rs",
        Some("definitions_reaching_line"),
    ),
    (
        "measure cyclomatic complexity across a codebase",
        "quality/complexity.rs",
        Some("analyze_complexity"),
    ),
    (
        "compute the maintainability index of a file",
        "quality/maintainability.rs",
        Some("maintainability_index"),
    ),
    (
        "estimate technical debt remediation time",
        "quality/debt.rs",
        None,
    ),
    (
        "scan source for hardcoded secrets and api keys",
        "security/secrets.rs",
        Some("scan_secrets"),
    ),
    (
        "check if a variable is tainted by untrusted input",
        "security/taint.rs",
        Some("is_tainted"),
    ),
    (
        "find sinks where tainted data causes a vulnerability",
        "security/taint.rs",
        Some("get_vulnerabilities"),
    ),
    (
        "detect security vulnerabilities using taint tracking",
        "security/vuln.rs",
        Some("scan_vulnerabilities"),
    ),
    (
        "build a project wide call graph",
        "callgraph/builder.rs",
        Some("build_project_call_graph"),
    ),
    (
        "decide whether a function is a program entry point",
        "callgraph/builder.rs",
        Some("is_entry_point"),
    ),
    (
        "convert a file path to a module name",
        "callgraph/module_path.rs",
        Some("path_to_module"),
    ),
    (
        "deduplicate repeated strings by interning them into ids",
        "callgraph/interner.rs",
        Some("intern"),
    ),
    (
        "resolve an import statement to its target module",
        "callgraph/import_resolver.rs",
        Some("resolve"),
    ),
    (
        "extract module information from a source file",
        "ast/extract.rs",
        Some("extract_file"),
    ),
    (
        "get the tree sitter grammar for a language",
        "ast/parser.rs",
        Some("get_ts_language"),
    ),
    (
        "locate a function node by name in the syntax tree",
        "ast/function_finder.rs",
        Some("find_function_node"),
    ),
    (
        "count the number of functions in a codebase",
        "ast/count.rs",
        Some("count_functions_canonical"),
    ),
    (
        "gather relevant code context for an llm from an entry point",
        "context/builder.rs",
        Some("get_relevant_context"),
    ),
    (
        "format extracted code context as a string for an llm",
        "context/builder.rs",
        Some("to_llm_string"),
    ),
    (
        "check whether a path is inside a git repository",
        "git/mod.rs",
        Some("is_git_repository"),
    ),
    (
        "get git history with lines added and deleted per file",
        "git/mod.rs",
        Some("git_log_numstat"),
    ),
    (
        "compute halstead software complexity metrics for a file",
        "metrics/halstead.rs",
        Some("analyze_halstead"),
    ),
    (
        "classify tokens into operators and operands",
        "metrics/halstead.rs",
        Some("classify_tokens"),
    ),
    (
        "analyze the class inheritance hierarchy",
        "inheritance/mod.rs",
        Some("extract_inheritance"),
    ),
    (
        "diagnose an error from raw compiler error text",
        "fix/mod.rs",
        Some("diagnose"),
    ),
    (
        "filter diagnostics by minimum severity level",
        "diagnostics/mod.rs",
        Some("filter_diagnostics_by_severity"),
    ),
    (
        "run fixed point iteration for pointer alias analysis",
        "alias/solver.rs",
        Some("solve"),
    ),
    (
        "mine recurring code patterns from a directory",
        "patterns/mod.rs",
        Some("mine_patterns"),
    ),
    (
        "build a bm25 keyword index from a project directory",
        "search/bm25.rs",
        Some("from_project"),
    ),
    (
        "control whether gitignore rules are honored when walking files",
        "walker.rs",
        Some("respect_gitignore"),
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
    // show_progress: true is REQUIRED for batched embedding — embedder.rs ties
    // batch_size to it (None when false), so false embeds the whole corpus in one
    // unbatched call (17GB, minutes). Cache ON: the schema tag is recipe-honest
    // (raw-v1/enriched-v1) and the query prefix never changes document vectors,
    // so prefix A/B re-runs reuse cached docs and only re-embed the query.
    // y0q: benchmark the DEPLOYED model (resolved from .tldr config exactly like
    // the CLI does), not the hardcoded enum default (ArcticM). Otherwise the eval
    // measures a model users may not run. Override with TLDR_EVAL_MODEL=arctic-l|...
    let cfg = TldrConfig::resolve(Some(&root));
    let model = EmbeddingModel::resolve(std::env::var("TLDR_EVAL_MODEL").ok().as_deref(), &cfg)
        .unwrap_or_else(|e| {
            eprintln!("model resolve failed ({e}); falling back to default");
            EmbeddingModel::default()
        });
    eprintln!("Eval embedding model: {:?}", model);
    // TLDR-4er: hybrid mode fuses dense (SemanticIndex) with BM25 via RRF at FILE
    // granularity. Off by default (dense-only). Enable with TLDR_EVAL_HYBRID=1.
    let hybrid = std::env::var("TLDR_EVAL_HYBRID")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    eprintln!(
        "Eval mode: {}",
        if hybrid {
            "HYBRID (BM25 + dense RRF, file-level)"
        } else {
            "dense-only"
        }
    );
    let mut index = SemanticIndex::build(
        &root,
        BuildOptions {
            model,
            show_progress: true,
            use_cache: true,
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

    // Only score gold cases whose expected file is actually in the indexed
    // corpus, so the same harness can be pointed at a small subtree for fast
    // iteration without counting out-of-scope targets as misses.
    let corpus: Vec<String> = walkdir::WalkDir::new(&root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
        .map(|e| norm(&e.path().to_string_lossy()))
        .collect();
    let in_corpus = |suffix: &str| corpus.iter().any(|p| p.ends_with(suffix));

    let gold: Vec<_> = GOLD
        .iter()
        .filter(|(_, want_file, _)| in_corpus(&norm(want_file)))
        .collect();

    let mut hits_at_5 = 0usize;
    let mut hits_at_10 = 0usize;
    let mut mrr_sum = 0.0f64;

    println!(
        "\n{:<58} {:>5} {:>6} {}",
        "query", "rank", "fn?", "expected"
    );
    println!("{}", "-".repeat(96));

    for (query, want_file, want_fn) in &gold {
        let want_file_n = norm(want_file);

        let mut rank: Option<usize> = None;
        let mut fn_matched = false;
        if hybrid {
            // Measure the EXACT production path (tldr semantic --hybrid / MCP):
            // build dense from the index, reduce to best-per-file, RRF-fuse with
            // BM25, then rank the gold FILE in the fused list.
            let fused = hybrid_search_with_index(&mut index, query, &root, Language::Rust, TOP_K)
                .expect("hybrid search failed");
            for (i, hr) in fused.results.iter().enumerate() {
                if norm(&hr.file_path.to_string_lossy()).ends_with(&want_file_n) {
                    rank = Some(i + 1);
                    break;
                }
            }
        } else {
            // Dense-only: first chunk whose file matches the expected suffix.
            let report = index.search(query, &opts).expect("search failed");
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
        }

        let rank_str = rank.map(|r| r.to_string()).unwrap_or_else(|| "—".into());
        let fn_str = if hybrid {
            "n/a" // file-level fusion; no function granularity
        } else {
            match (want_fn, rank.is_some(), fn_matched) {
                (None, _, _) => "n/a",
                (Some(_), true, true) => "yes",
                (Some(_), true, false) => "no",
                (Some(_), false, _) => "—",
            }
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

    let n = gold.len().max(1) as f64;
    println!("{}", "-".repeat(96));
    println!("\nScope:       {}", root.display());
    println!("Gold cases:  {} (of {} total; rest out of scope)", gold.len(), GOLD.len());
    println!("Recall@5:    {:.3}  ({}/{})", hits_at_5 as f64 / n, hits_at_5, gold.len());
    println!("Recall@10:   {:.3}  ({}/{})", hits_at_10 as f64 / n, hits_at_10, gold.len());
    println!("MRR:         {:.3}", mrr_sum / n);
}

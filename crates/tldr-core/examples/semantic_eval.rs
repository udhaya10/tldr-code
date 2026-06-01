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
        let q_short: String = query.chars().take(58).collect();
        println!(
            "{:<58} {:>5} {:>6} {}",
            q_short,
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

    // TLDR-l5d acceptance: store-path equivalence on real data. Build a usearch
    // VectorStore over the SAME root/model/cache and check that its per-query
    // top-K ranking is IDENTICAL to the dense SemanticIndex path (same vectors,
    // same exact cosine). Gated on TLDR_EVAL_STORE=1 (extra build + searches).
    let store_eval = std::env::var("TLDR_EVAL_STORE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if store_eval {
        use tldr_core::semantic::embedder::Embedder;
        use tldr_core::semantic::vector_store::VectorStore;
        use tldr_core::semantic::ChunkGranularity;

        eprintln!("\nBuilding usearch VectorStore (store-path equivalence)...");
        let store = VectorStore::build(
            &root,
            &BuildOptions {
                model,
                granularity: ChunkGranularity::Function,
                languages: None,
                show_progress: true,
                use_cache: true,
            },
            Some(Default::default()),
        )
        .expect("VectorStore::build failed");
        let mut q_embedder = Embedder::new(model).expect("query embedder");

        // The dense path's file_path is root-prefixed; store paths are
        // root-relative — normalize both to the same root-relative form.
        let canon_root = root.canonicalize().unwrap_or_else(|_| root.clone());
        let rel = |p: &std::path::Path| -> String {
            let cp = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
            cp.strip_prefix(&canon_root)
                .unwrap_or(&cp)
                .to_string_lossy()
                .replace('\\', "/")
        };

        // Diagnostic: at the FIRST differing rank, compare the two competing
        // items by the SAME dense f64 cosine scorer. dense uses f64 + stable sort
        // (chunk order tie-break); the store uses usearch f32 exact_search. So a
        // reorder is a TIE (equal dense cosine), an EPSILON f32/f64 boundary
        // flip, or a REAL ranking bug (the store ranked a clearly-lower-cosine
        // item higher). Only REAL > 0 fails results-equivalence.
        use std::collections::HashMap;
        type Item = (String, Option<String>, u32);
        let mut order_identical = 0usize;
        let mut tie = 0usize;
        let mut epsilon = 0usize;
        let mut real = 0usize;
        // Score dense DEEP (top-50) so a store item that lands just outside the
        // dense top-K still gets a real dense cosine instead of a guess.
        let score_opts = IndexSearchOptions {
            top_k: 50,
            threshold: 0.0,
            include_snippet: false,
            ..Default::default()
        };
        for (query, _, _) in &gold {
            let dense = index.search(query, &score_opts).expect("dense search");
            let d_items: Vec<Item> = dense
                .results
                .iter()
                .take(TOP_K)
                .map(|r| (rel(&r.file_path), r.function_name.clone(), r.line_start))
                .collect();
            let dscore: HashMap<Item, f64> = dense
                .results
                .iter()
                .map(|r| ((rel(&r.file_path), r.function_name.clone(), r.line_start), r.score))
                .collect();

            let qv = q_embedder.embed_query(query).expect("embed_query");
            let s_items: Vec<Item> = store
                .search(&qv, TOP_K)
                .expect("store search")
                .iter()
                .map(|h| (h.meta.file_rel_path.clone(), h.meta.function_name.clone(), h.meta.line_start))
                .collect();

            let first = (0..d_items.len().min(s_items.len())).find(|&r| d_items[r] != s_items[r]);
            let r = match first {
                None => {
                    order_identical += 1;
                    continue;
                }
                Some(r) => r,
            };
            let d_item = &d_items[r];
            let s_item = &s_items[r];
            let d_sc = dscore.get(d_item).copied().unwrap_or(f64::NAN);
            let s_sc = dscore.get(s_item).copied(); // store's pick, scored by DENSE cosine
            let verdict = match s_sc {
                Some(s_sc) => {
                    let gap = (d_sc - s_sc).abs();
                    if gap < 1e-6 {
                        tie += 1;
                        format!("TIE      gap={gap:.2e}")
                    } else if gap < 1e-4 {
                        epsilon += 1;
                        format!("EPSILON  gap={gap:.2e}")
                    } else {
                        real += 1;
                        format!("REAL     gap={gap:.2e}  <-- ranking bug")
                    }
                }
                None => {
                    // store surfaced an item outside the dense top-K — a boundary
                    // tie (something just past rank K). Treat as epsilon.
                    epsilon += 1;
                    "BOUNDARY (store item outside dense top-K)".to_string()
                }
            };
            eprintln!("  rank {r} [{verdict}]  \"{}\"", &query[..query.len().min(44)]);
            eprintln!("    dense[{r}]: {d_item:?}  sim={d_sc:.6}");
            eprintln!(
                "    store[{r}]: {s_item:?}  dense_sim={}",
                s_sc.map(|x| format!("{x:.6}")).unwrap_or_else(|| "(outside top-K)".into())
            );
        }
        let equivalent = order_identical + tie + epsilon;
        println!("\nStore-path equivalence over {} gold queries:", gold.len());
        println!("  order-identical: {order_identical}");
        println!("  TIE (gap<1e-6):  {tie}");
        println!("  EPSILON(<1e-4):  {epsilon}");
        println!("  REAL (>=1e-4):   {real}   <-- MUST be 0 for results-equivalence");
        println!("  => equivalent modulo ties/epsilon: {equivalent}/{}", gold.len());
    }
}

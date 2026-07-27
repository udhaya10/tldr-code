//! Run the labeled AST-binding call-edge precision/recall gate.
//!
//! Usage:
//! `cargo run -p tldr-core --example callgraph_binding_eval -- <project> <suite.json>`

use std::error::Error;
use std::path::PathBuf;

use serde::Deserialize;
use tldr_core::callgraph::{
    build_project_call_graph_v2, evaluate_binding_edges, BindingEdgeLabel, BuildConfig,
};

#[derive(Deserialize)]
struct EvalSuite {
    language: String,
    #[serde(default)]
    callee_suffix: Option<String>,
    expected: Vec<BindingEdgeLabel>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let project = PathBuf::from(arguments.next().ok_or("missing project path")?);
    let suite_path = PathBuf::from(arguments.next().ok_or("missing suite JSON path")?);
    if arguments.next().is_some() {
        return Err("expected exactly: <project> <suite.json>".into());
    }

    let suite: EvalSuite = serde_json::from_slice(&std::fs::read(suite_path)?)?;
    let graph = build_project_call_graph_v2(
        &project,
        BuildConfig {
            language: suite.language,
            use_type_resolution: true,
            ..Default::default()
        },
    )?;
    let selected = graph.edges.iter().filter(|edge| {
        suite
            .callee_suffix
            .as_deref()
            .is_none_or(|suffix| edge.dst_func.ends_with(suffix))
    });
    let report = evaluate_binding_edges(suite.expected, selected);
    println!("{}", serde_json::to_string_pretty(&report)?);
    if report.false_positives > 0 || report.false_negatives > 0 {
        std::process::exit(1);
    }
    Ok(())
}

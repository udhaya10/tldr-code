//! Calls command - Build call graph
//!
//! Builds and displays the cross-file call graph for a project.
//! Auto-routes through daemon when available for ~35x speedup.

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;
use serde::{Deserialize, Serialize};

use tldr_core::callgraph::cross_file_types::CallType;
use tldr_core::callgraph::{build_project_call_graph_v2, BuildConfig};
use tldr_core::Language;

use crate::commands::daemon_router::{is_oneshot, route_for_path};
use crate::output::{format_calls_dot, DotCallEdge, OutputFormat, OutputWriter};

/// Build and display cross-file call graph
#[derive(Debug, Args)]
pub struct CallsArgs {
    /// Project root directory (default: current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Programming language (auto-detected if not specified)
    #[arg(long, short = 'l')]
    pub lang: Option<Language>,

    /// Respect .gitignore and .tldrignore patterns
    #[arg(long, default_value = "true")]
    pub respect_ignore: bool,

    /// Maximum items (edges) to include in output (default: 200)
    #[arg(long, default_value = "200")]
    pub max_items: usize,
}

/// Call graph output format
///
/// med-low-schema-cleanup-v1 (N12): the redundant `edge_count` and
/// `node_count` keys were removed. `total_edges` + `shown_edges` +
/// `truncated` is the single canonical pair (mirrors what `references`
/// and `dead` use); `node_count` was always equal to `nodes.len()` so
/// consumers can derive it locally.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CallGraphOutput {
    root: PathBuf,
    /// Resolved language. `None` (serialized as JSON `null`) when the
    /// caller passed no `--lang` flag and `Language::from_directory`
    /// found no analyzable files (e.g. the path is an empty directory).
    ///
    /// schema-cleanup-v2 (P2.BUG-10): pre-fix the type was `Language`
    /// and the `unwrap_or(Language::Python)` autodetect fallback caused
    /// an empty directory to be reported as `language: "python"` —
    /// silently picking a default that misrepresented the input. Now
    /// the field is `Option<Language>` and the autodetect failure
    /// surfaces as JSON `null`, which downstream consumers can branch
    /// on without parsing English error strings.
    language: Option<Language>,
    nodes: Vec<String>,
    edges: Vec<EdgeOutput>,
    /// Whether the output was truncated due to max_items limit
    ///
    /// (path-and-schema-cleanup-v3 P3.BUG-N5) Always emitted — including
    /// when `false` — so schema consumers do not need to handle the
    /// absent-key case. Previously elided via `skip_serializing_if`, but
    /// downstream tooling (and `references`, `dead`, `dice`, etc.) all
    /// treat `truncated` as a stable boolean key.
    #[serde(default)]
    truncated: bool,
    /// Total number of edges before truncation
    total_edges: usize,
    /// Number of edges shown after truncation
    shown_edges: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct EdgeOutput {
    src_file: PathBuf,
    src_func: String,
    dst_file: PathBuf,
    dst_func: String,
    call_type: CallType,
}

/// Build the canonical [`CallGraphOutput`] for `path`.
///
/// TLDR-7pp.1.5: shared by the CLI `--oneshot` path (via
/// [`CallsArgs::compute_local`]) AND the daemon `Calls` handler so the two
/// produce byte-identical results. `building_language` is the language used to
/// construct the graph (autodetect falls back to Python); `detected_language`
/// is the value reported in the JSON `language` field (`None` => `null` when
/// autodetection found nothing). `respect_ignore` and `max_items` are the
/// behavior-affecting flags that MUST travel on the wire for parity.
pub(crate) fn build_call_graph_output(
    path: &Path,
    building_language: Language,
    detected_language: Option<Language>,
    respect_ignore: bool,
    max_items: usize,
) -> Result<CallGraphOutput> {
    // Build call graph (V2 canonical)
    let config = BuildConfig {
        language: building_language.as_str().to_string(),
        respect_ignore,
        use_type_resolution: true,
        ..Default::default()
    };
    let ir = build_project_call_graph_v2(path, config)?;
    // Bypass compat layer - output ir.edges directly with normalized paths
    let root = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let edges: Vec<EdgeOutput> = ir
        .edges
        .iter()
        .map(|e| {
            let src = e.src_file.strip_prefix(&root).unwrap_or(&e.src_file);
            let dst = e.dst_file.strip_prefix(&root).unwrap_or(&e.dst_file);
            EdgeOutput {
                src_file: src.to_path_buf(),
                src_func: e.src_func.clone(),
                dst_file: dst.to_path_buf(),
                dst_func: e.dst_func.clone(),
                call_type: e.call_type,
            }
        })
        .collect();

    // Sort and truncate edges by max_items
    let total_edges = edges.len();
    let truncated = total_edges > max_items;
    let mut edges = edges;
    if edges.len() > max_items {
        // Sort by source file + function as a simple importance metric
        edges.sort_by(|a, b| {
            let a_key = format!("{}:{}", a.src_file.display(), a.src_func);
            let b_key = format!("{}:{}", b.src_file.display(), b.src_func);
            a_key.cmp(&b_key)
        });
        edges.truncate(max_items);
    }
    let shown_edges = edges.len();

    // Build unique node set from truncated edges AND from every
    // defined function in the project. The original derivation was
    // edges-only, which under-reported the call graph for files like
    // OCaml functor bodies (`module Make (V) = struct ... end`)
    // whose let-bindings make external calls (`Format.fprintf`, …)
    // that don't resolve to in-project targets. Phase-12 audit
    // (BUG-AGG12-4) caught dag.ml reporting nodes=2 even though
    // `tldr structure dag.ml` finds 19 functions. Including defined
    // funcs as graph nodes (zero-out-degree where appropriate) gives
    // every language a faithful node count: the call graph now
    // exposes both call relationships AND the function inventory.
    let mut node_set = std::collections::BTreeSet::new();
    for edge in &edges {
        node_set.insert(format!("{}:{}", edge.src_file.display(), edge.src_func));
        node_set.insert(format!("{}:{}", edge.dst_file.display(), edge.dst_func));
    }
    for (file_path, file_ir) in &ir.files {
        // FileIR paths are already normalized to forward-slash
        // relative form; strip the canonicalized root just in case
        // the FileIR happens to be absolute (defensive).
        let rel = file_path.strip_prefix(&root).unwrap_or(file_path);
        for func in &file_ir.funcs {
            let qualified = if let Some(class) = &func.class_name {
                format!("{}.{}", class, func.name)
            } else {
                func.name.clone()
            };
            node_set.insert(format!("{}:{}", rel.display(), qualified));
        }
    }
    let nodes: Vec<String> = node_set.into_iter().collect();

    Ok(CallGraphOutput {
        root: path.to_path_buf(),
        language: detected_language,
        nodes,
        edges,
        truncated,
        total_edges,
        shown_edges,
    })
}

/// Render a [`CallGraphOutput`] to the requested format. Single renderer shared
/// by both the daemon and `--oneshot` paths.
fn render(writer: &OutputWriter, output: &CallGraphOutput) -> Result<()> {
    if writer.is_dot() {
        // surface-gaps-v1 (BUG-19): DOT path.
        let srcs: Vec<String> = output
            .edges
            .iter()
            .map(|e| format!("{}:{}", e.src_file.display(), e.src_func))
            .collect();
        let dsts: Vec<String> = output
            .edges
            .iter()
            .map(|e| format!("{}:{}", e.dst_file.display(), e.dst_func))
            .collect();
        let labels: Vec<String> = output
            .edges
            .iter()
            .map(|e| format!("{:?}", e.call_type))
            .collect();
        let dot_edges: Vec<DotCallEdge<'_>> = (0..output.edges.len())
            .map(|i| DotCallEdge {
                src: srcs[i].as_str(),
                dst: dsts[i].as_str(),
                label: Some(labels[i].as_str()),
            })
            .collect();
        let dot = format_calls_dot(&dot_edges);
        writer.write_text(&dot)?;
        return Ok(());
    }
    if writer.is_text() {
        let mut text = String::new();
        let lang_label = output
            .language
            .map(|l| l.as_str().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        text.push_str(&format!(
            "Call Graph for {} ({})\n",
            output.root.display(),
            lang_label,
        ));
        text.push_str(&format!("Edges: {}\n\n", output.total_edges));

        for edge in &output.edges {
            text.push_str(&format!(
                "{}:{} -> {}:{}\n",
                edge.src_file.display(),
                edge.src_func,
                edge.dst_file.display(),
                edge.dst_func
            ));
        }

        writer.write_text(&text)?;
    } else {
        writer.write(output)?;
    }
    Ok(())
}

impl CallsArgs {
    /// Run the calls command
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let writer = OutputWriter::new(format, quiet);

        // Validate path exists BEFORE language detection / progress banner
        // (lang-detect-default-v1)
        if !self.path.exists() {
            anyhow::bail!("Path not found: {}", self.path.display());
        }

        // Determine language. schema-cleanup-v2 (P2.BUG-10): when the
        // caller did not pass `--lang` AND `Language::from_directory`
        // detects nothing (e.g. empty directory), preserve the absence
        // as `None` rather than silently falling back to Python.
        let detected_language = self.lang.or_else(|| Language::from_directory(&self.path));

        // ADR-10 (TLDR-7pp.1.5): daemon is the only serve path; `--oneshot` is
        // the sole explicit local-compute escape. No silent fallback.
        let output: CallGraphOutput = if is_oneshot() {
            self.compute_local(detected_language, &writer)?
        } else {
            // Full flag envelope on the wire: detected language (the JSON
            // `language` field), respect_ignore, and max_items all travel so
            // the daemon produces byte-identical output to compute_local.
            let params = serde_json::json!({
                "path": self.path,
                "language": detected_language,
                "respect_ignore": self.respect_ignore,
                "max_items": self.max_items,
            });
            route_for_path::<CallGraphOutput>(&self.path, "calls", params)
                .into_hit_or_bail("calls")?
        };

        render(&writer, &output)
    }

    /// Local in-process call-graph build — reached only via `--oneshot`.
    fn compute_local(
        &self,
        detected_language: Option<Language>,
        writer: &OutputWriter,
    ) -> Result<CallGraphOutput> {
        // The build path treats `None` as Python for call-graph construction
        // (the builder requires a language) but the JSON `language` field
        // reflects the actual detection result.
        let building_language = detected_language.unwrap_or(Language::Python);

        writer.progress(&format!(
            "Building call graph for {} ({:?})...",
            self.path.display(),
            building_language
        ));

        build_call_graph_output(
            &self.path,
            building_language,
            detected_language,
            self.respect_ignore,
            self.max_items,
        )
    }
}

//! Context command - Build LLM context
//!
//! Generates token-efficient LLM context from an entry point.
//! Auto-routes through daemon when available for ~35x speedup.

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;

use tldr_core::types::RelevantContext;
use tldr_core::{get_relevant_context, Language};

use crate::commands::daemon_router::{params_with_entry_depth, try_daemon_route};
use crate::output::{OutputFormat, OutputWriter};

/// Build LLM-ready context from entry point
#[derive(Debug, Args)]
pub struct ContextArgs {
    /// Entry point function name
    pub entry: String,

    /// Project root directory as positional argument (mirrors sibling
    /// path-taking commands like `impact`, `whatbreaks`). When set, this
    /// takes precedence over `--project`. (med-cleanup-bundle-v1 / M1)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Project root directory (deprecated alias for the positional path
    /// argument; kept for back-compat). (med-cleanup-bundle-v1 / M1)
    #[arg(long, short = 'p')]
    pub project: Option<PathBuf>,

    /// Programming language
    #[arg(long, short = 'l')]
    pub lang: Option<Language>,

    /// Maximum traversal depth
    #[arg(long, short = 'd', default_value = "3")]
    pub depth: usize,

    /// Include function docstrings
    #[arg(long)]
    pub include_docstrings: bool,

    /// Filter to functions in this file (for disambiguating common names like "render")
    #[arg(long)]
    pub file: Option<PathBuf>,
}

impl ContextArgs {
    /// Resolve the effective project path. The positional `path` argument
    /// is the canonical input; `--project` is kept as a back-compat alias
    /// and only wins when the positional path is left at its default ".".
    /// (med-cleanup-bundle-v1 / M1)
    fn effective_project(&self) -> PathBuf {
        match &self.project {
            Some(p) if self.path == PathBuf::from(".") => p.clone(),
            _ => self.path.clone(),
        }
    }

    /// Run the context command
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let writer = OutputWriter::new(format, quiet);

        let mut project_path = self.effective_project();

        // language-adapter-fixes-v1 (P13.AGG13-5): accept the
        // `<file>:<func>` shorthand so users can disambiguate common
        // function names without typing `--file` separately. The shape
        // mirrors `tldr explain <file> <func>` and `tldr resources
        // <file> <func>`. We split on the LAST `:` so paths with
        // Windows drive letters (`C:\foo\bar.js:foo`) still resolve
        // file=`C:\foo\bar.js` / func=`foo`. If the file half does not
        // exist on disk, fall back to the legacy bare-name behaviour
        // so genuine names containing `:` (e.g. C++ `Class::method`,
        // Rust `mod::fn`) still parse.
        //
        // When the shorthand resolves and the user did not supply an
        // explicit project path (positional or `--project`), infer the
        // project root from the file's enclosing directory — walking up
        // until we find a likely repo marker (`.git`, package manifest,
        // etc.) or fall back to the file's parent. This mirrors what a
        // user manually types: `cd /tmp/repos/express && tldr context
        // render`.
        let (entry, derived_file): (String, Option<PathBuf>) = match self.entry.rfind(':') {
            Some(idx) if idx > 0 && idx + 1 < self.entry.len() => {
                let file_part = &self.entry[..idx];
                let func_part = &self.entry[idx + 1..];
                let candidate = PathBuf::from(file_part);
                if candidate.is_file() && !func_part.is_empty() {
                    (func_part.to_string(), Some(candidate))
                } else {
                    (self.entry.clone(), None)
                }
            }
            _ => (self.entry.clone(), None),
        };

        // The user-supplied --file (if any) wins over the derived form so
        // explicit flags always take precedence over inferred shorthands.
        let effective_file: Option<PathBuf> =
            self.file.clone().or_else(|| derived_file.clone());

        // Auto-derive project root from file when shorthand was used and
        // the user didn't supply an explicit one. Honour `.git` /
        // `package.json` / `Cargo.toml` markers; otherwise fall back to
        // the file's immediate parent directory. This keeps the
        // shorthand useful from any cwd.
        if derived_file.is_some()
            && self.path == PathBuf::from(".")
            && self.project.is_none()
        {
            if let Some(file) = effective_file.as_ref() {
                if let Some(root) = infer_project_root_from_file(file) {
                    project_path = root;
                }
            }
        }

        // Determine language (auto-detect from directory, default to Python)
        let language = self
            .lang
            .unwrap_or_else(|| Language::from_directory(&project_path).unwrap_or(Language::Python));

        // Try daemon first for cached result. Only route through the
        // daemon when there is no derived-file disambiguation, since the
        // daemon protocol does not currently propagate the `--file`
        // filter (would silently ignore the disambiguator).
        if effective_file.is_none() {
            if let Some(context) = try_daemon_route::<RelevantContext>(
                &project_path,
                "context",
                params_with_entry_depth(&entry, Some(self.depth)),
            ) {
                // Output based on format
                if writer.is_text() {
                    // Use the built-in LLM string format
                    let text = context.to_llm_string();
                    writer.write_text(&text)?;
                    return Ok(());
                } else {
                    writer.write(&context)?;
                    return Ok(());
                }
            }
        }

        // Fallback to direct compute
        writer.progress(&format!(
            "Building context for {} (depth={})...",
            entry, self.depth
        ));

        // Get relevant context
        let context = get_relevant_context(
            &project_path,
            &entry,
            self.depth,
            language,
            self.include_docstrings,
            effective_file.as_deref(),
        )?;

        // Output based on format
        if writer.is_text() {
            // Use the built-in LLM string format
            let text = context.to_llm_string();
            writer.write_text(&text)?;
        } else {
            writer.write(&context)?;
        }

        Ok(())
    }
}

/// Walk upward from `file`'s parent directory until we hit a directory
/// containing one of the common project-root markers (`.git`,
/// `package.json`, `Cargo.toml`, `go.mod`, `pyproject.toml`,
/// `pom.xml`, `build.gradle*`, `*.csproj`, `mix.exs`, `dune-project`).
/// Returns `Some(parent_dir)` as a fallback if no marker is found.
///
/// language-adapter-fixes-v1 (P13.AGG13-5): used by the context command
/// when the user invokes the `<file>:<func>` shorthand without an
/// explicit project path. Lets `tldr context /path/to/repo/src/x.js:foo`
/// resolve from any cwd, mirroring `cd /path/to/repo && tldr context foo`.
fn infer_project_root_from_file(file: &Path) -> Option<PathBuf> {
    let abs = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
    let parent = abs.parent()?;
    const MARKERS: &[&str] = &[
        ".git",
        "package.json",
        "Cargo.toml",
        "go.mod",
        "pyproject.toml",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "mix.exs",
        "dune-project",
        "Package.swift",
    ];
    let mut cursor: Option<&Path> = Some(parent);
    while let Some(dir) = cursor {
        for m in MARKERS {
            if dir.join(m).exists() {
                return Some(dir.to_path_buf());
            }
        }
        // Also accept any *.csproj sibling (C# projects).
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry
                    .path()
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e == "csproj" || e == "sln")
                    .unwrap_or(false)
                {
                    return Some(dir.to_path_buf());
                }
            }
        }
        cursor = dir.parent();
    }
    Some(parent.to_path_buf())
}

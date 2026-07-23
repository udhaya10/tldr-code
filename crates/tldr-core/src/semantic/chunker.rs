//! Code chunking using tree-sitter for function extraction
//!
//! This module provides code chunking functionality for the semantic search system.
//! It extracts discrete code units (files or functions) that can be individually
//! embedded for similarity search.
//!
//! # Architecture
//!
//! The chunker integrates with the existing AST infrastructure in `tldr_core::ast`:
//! - Uses `tldr_core::ast::parser` for tree-sitter parsing
//! - Leverages `tldr_core::ast::extractor` patterns for function extraction
//!
//! # P0 Mitigations (from phased-plan.yaml)
//!
//! - Extracts ALL function types (lambdas, closures, async functions)
//! - Reports skipped files with reasons (not silent failures)
//!
//! # Example
//!
//! ```rust,ignore
//! use std::path::Path;
//! use tldr_core::semantic::chunker::{chunk_code, ChunkOptions};
//!
//! let result = chunk_code(Path::new("src/"), &ChunkOptions::default())?;
//!
//! for chunk in &result.chunks {
//!     println!("{}: {} lines",
//!         chunk.file_path.display(),
//!         chunk.line_end - chunk.line_start + 1
//!     );
//! }
//!
//! if !result.skipped.is_empty() {
//!     eprintln!("Skipped {} files", result.skipped.len());
//! }
//! ```

use std::path::{Path, PathBuf};

use tree_sitter::{Node, Tree};

use crate::ast::parser::parse_file;
use crate::semantic::types::{ChunkGranularity, ChunkOptions, CodeChunk};
use crate::{Language, TldrError, TldrResult};

// =============================================================================
// Constants
// =============================================================================

/// Maximum chunk size in characters (default: ~4000 chars for ~1000 tokens)
pub const DEFAULT_MAX_CHUNK_SIZE: usize = 4000;

/// Binary file extensions to skip
const BINARY_EXTENSIONS: &[&str] = &[
    "exe", "dll", "so", "dylib", "a", "lib", "o", "obj", // Executables/libraries
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "svg", "webp", // Images
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", // Documents
    "zip", "tar", "gz", "rar", "7z", "bz2", // Archives
    "mp3", "mp4", "wav", "avi", "mov", "mkv", // Media
    "wasm", "pyc", "pyo", "class", // Compiled code
    "db", "sqlite", "sqlite3", // Databases
    "ttf", "otf", "woff", "woff2", "eot", // Fonts
];

/// Hidden directory/file prefixes to skip
const HIDDEN_PREFIXES: &[&str] = &[".", "_"];

// =============================================================================
// Result Types
// =============================================================================

/// Result of a chunking operation
///
/// Contains the extracted chunks and information about skipped files.
#[derive(Debug, Clone, Default)]
pub struct ChunkResult {
    /// Successfully extracted code chunks
    pub chunks: Vec<CodeChunk>,

    /// Files that were skipped during chunking
    pub skipped: Vec<SkippedFile>,

    /// Counts describing the chunking pass.
    pub stats: ChunkStats,
}

impl ChunkResult {
    fn from_parts(chunks: Vec<CodeChunk>, skipped: Vec<SkippedFile>) -> Self {
        let stats = ChunkStats::from_parts(&chunks, &skipped);
        Self {
            chunks,
            skipped,
            stats,
        }
    }
}

/// Counts describing source eligibility and chunk creation.
///
/// Every counter is derived from the same chunking pass via
/// [`ChunkStats::from_parts`], so consumers reading policy-aware counts do
/// not need to recompute them from [`ChunkResult::chunks`] and
/// [`ChunkResult::skipped`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChunkStats {
    /// Source files that produced at least one chunk.
    ///
    /// Deduped by [`CodeChunk::file_path`]; a file that yields N chunks
    /// contributes 1 here.
    pub files_indexed: usize,

    /// Total skipped files across every skip reason (autogen, oversized,
    /// unsupported language, ignore matchers, etc.).
    ///
    /// [`Self::files_unsupported`] and [`Self::files_oversized`] are
    /// subsets of this count.
    pub files_skipped: usize,

    /// Skipped files rejected by [`CorpusPolicy`] for language reasons.
    ///
    /// Matches skip reasons containing `Unknown language` or
    /// `Filtered out by language` — the corpus deemed the language
    /// ineligible for embedding.
    pub files_unsupported: usize,

    /// Skipped files rejected for exceeding the corpus size budget.
    ///
    /// Matches skip reasons containing `too large` or `exceeds` —
    /// the file is unreadable or too large to chunk within budget.
    pub files_oversized: usize,

    /// Total chunks emitted by the pass.
    ///
    /// May exceed [`Self::files_indexed`] when a single file yields
    /// multiple chunks (multi-function files, oversized splits).
    pub chunks_created: usize,
}

impl ChunkStats {
    fn from_parts(chunks: &[CodeChunk], skipped: &[SkippedFile]) -> Self {
        let files_indexed = chunks
            .iter()
            .map(|chunk| chunk.file_path.to_string_lossy().into_owned())
            .collect::<std::collections::HashSet<_>>()
            .len();
        let files_unsupported = skipped
            .iter()
            .filter(|file| {
                file.reason.contains("Unknown language")
                    || file.reason.contains("Filtered out by language")
            })
            .count();
        let files_oversized = skipped
            .iter()
            .filter(|file| {
                file.reason.contains("too large") || file.reason.contains("exceeds")
            })
            .count();
        Self {
            files_indexed,
            files_skipped: skipped.len(),
            files_unsupported,
            files_oversized,
            chunks_created: chunks.len(),
        }
    }
}

/// A file that was skipped during chunking
///
/// Provides transparency about why files were not processed,
/// implementing the P0 mitigation for "report skipped files with reasons".
#[derive(Debug, Clone)]
pub struct SkippedFile {
    /// Path to the skipped file
    pub path: String,

    /// Human-readable reason for skipping
    pub reason: String,
}

/// Shared file-level eligibility gate for the semantic corpus.
///
/// Directory traversal still owns ignore-file and generated-directory pruning,
/// while this policy owns the checks that must agree for builds, freshness
/// checks, and watcher updates.
#[derive(Debug, Clone, Copy, Default)]
pub struct CorpusPolicy;

impl CorpusPolicy {
    /// Return whether an existing path is a source-corpus candidate.
    pub fn accepts_file(path: &Path) -> bool {
        !is_binary_or_hidden(path) && Language::from_path(path).is_some()
    }

    /// Return whether an existing source file passes the size policy.
    pub fn within_size_limit(path: &Path) -> bool {
        matches!(
            Self::size_check(path),
            crate::fs::oversize::SizeCheck::WithinLimit { .. }
        )
    }

    /// Return the centralized size-policy result for a source file.
    pub fn size_check(path: &Path) -> crate::fs::oversize::SizeCheck {
        crate::fs::oversize::check_size(path)
    }

    /// Return the supported extension set used by project traversal.
    pub fn supported_extensions() -> Vec<&'static str> {
        crate::Language::all()
            .iter()
            .flat_map(|language| language.scan_extensions().iter().copied())
            .filter_map(|extension| extension.strip_prefix('.'))
            .collect()
    }

    /// Return whether an existing path survives the full corpus gate,
    /// including ignore files and generated-directory pruning.
    pub fn accepts_path(root: &Path, file: &Path) -> bool {
        is_corpus_file_impl(root, file)
    }
}

// =============================================================================
// Public API
// =============================================================================

/// Chunk a file or directory of code
///
/// This is the main entry point for code chunking. It handles both
/// single files and directories, recursively processing all supported
/// source files.
///
/// # Arguments
///
/// * `path` - File or directory path to chunk
/// * `options` - Chunking options (granularity, max size, etc.)
///
/// # Returns
///
/// * `Ok(ChunkResult)` - Chunks and skipped file information
/// * `Err(TldrError)` - If path doesn't exist
///
/// # Example
///
/// ```rust,ignore
/// let result = chunk_code(Path::new("src/"), &ChunkOptions::default())?;
/// println!("Extracted {} chunks from {} files",
///     result.chunks.len(),
///     result.chunks.iter()
///         .map(|c| &c.file_path)
///         .collect::<std::collections::HashSet<_>>()
///         .len()
/// );
/// ```
pub fn chunk_code<P: AsRef<Path>>(path: P, options: &ChunkOptions) -> TldrResult<ChunkResult> {
    let path = path.as_ref();

    if !path.exists() {
        return Err(TldrError::PathNotFound(path.to_path_buf()));
    }

    if path.is_file() {
        chunk_file(path, options)
    } else if path.is_dir() {
        chunk_directory(path, options)
    } else {
        Err(TldrError::PathNotFound(path.to_path_buf()))
    }
}

/// Chunk a single file
///
/// Extracts code chunks from a single source file based on the
/// specified granularity (file-level or function-level).
///
/// # Arguments
///
/// * `path` - Path to the source file
/// * `options` - Chunking options
///
/// # Returns
///
/// * `Ok(ChunkResult)` - Extracted chunks (or skipped info if file can't be processed)
///
/// # Example
///
/// ```rust,ignore
/// let result = chunk_file(
///     Path::new("src/main.rs"),
///     &ChunkOptions { granularity: ChunkGranularity::Function, ..Default::default() }
/// )?;
/// ```
pub fn chunk_file<P: AsRef<Path>>(path: P, options: &ChunkOptions) -> TldrResult<ChunkResult> {
    let path = path.as_ref();
    let mut chunks = Vec::new();
    let mut skipped = Vec::new();

    // Check if file should be skipped
    if is_binary_or_hidden(path) {
        skipped.push(SkippedFile {
            path: path.display().to_string(),
            reason: "Binary or hidden file".into(),
        });
        return Ok(ChunkResult::from_parts(chunks, skipped));
    }

    // Detect language from extension
    let language = match Language::from_path(path) {
        Some(lang) => lang,
        None => {
            skipped.push(SkippedFile {
                path: path.display().to_string(),
                reason: format!(
                    "Unknown language for extension: {}",
                    path.extension()
                        .map(|e| e.to_string_lossy().to_string())
                        .unwrap_or_else(|| "none".into())
                ),
            });
            return Ok(ChunkResult::from_parts(chunks, skipped));
        }
    };

    // Check language filter if specified
    if let Some(ref langs) = options.languages {
        if !langs.contains(&language) {
            skipped.push(SkippedFile {
                path: path.display().to_string(),
                reason: format!("Filtered out by language ({})", language),
            });
            return Ok(ChunkResult::from_parts(chunks, skipped));
        }
    }

    if let crate::fs::oversize::SizeCheck::Oversize {
        size_bytes,
        max_bytes,
        is_autogen,
    } = CorpusPolicy::size_check(path)
    {
        skipped.push(SkippedFile {
            path: path.display().to_string(),
            reason: crate::fs::oversize::format_oversize_warning(
                path,
                size_bytes,
                max_bytes,
                is_autogen,
            ),
        });
        return Ok(ChunkResult::from_parts(chunks, skipped));
    }

    // Read file content
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            skipped.push(SkippedFile {
                path: path.display().to_string(),
                reason: format!("Read error: {}", e),
            });
            return Ok(ChunkResult::from_parts(chunks, skipped));
        }
    };

    // Parse the file
    let parse_result = parse_file(path);

    match options.granularity {
        ChunkGranularity::File => {
            // One chunk for entire file
            chunks.push(create_file_chunk(path, &content, language, options));
        }
        ChunkGranularity::Function => {
            // Try to extract functions using tree-sitter
            match parse_result {
                Ok((tree, source, lang)) => {
                    let functions = extract_function_chunks(&tree, &source, path, lang, options);

                    if functions.is_empty() {
                        // Fallback to file-level chunk if no functions found
                        chunks.push(create_file_chunk(path, &content, language, options));
                    } else {
                        chunks.extend(functions);
                    }
                }
                Err(e) => {
                    // Parse failed - fallback to file-level chunk with warning
                    eprintln!(
                        "Warning: Parse failed for {}, using file-level chunk: {}",
                        path.display(),
                        e
                    );
                    chunks.push(create_file_chunk(path, &content, language, options));
                }
            }
        }
    }

    Ok(ChunkResult::from_parts(chunks, skipped))
}

// =============================================================================
// Internal Functions
// =============================================================================

/// Chunk all files in a directory recursively.
///
/// verification-and-metrics-completeness-v1 (P12.AGG12-12): switched from a
/// raw `walkdir::WalkDir` with a tiny built-in skip list to the shared
/// `ProjectWalker`, which honours `.gitignore`, the canonical
/// `DEFAULT_EXCLUDE_DIRS` list (covers `dox/`, `out/`, `obj/`, JVM build
/// dirs, Python venvs, etc.), and the `dir_has_generated_sentinel` check
/// (skips e.g. `docs/` directories that contain doxygen output identified
/// by `doxygen.css` / `doxygen.svg` siblings). Without these filters,
/// `tldr semantic` indexed minified vendor JS such as
/// `cpp-tinyxml2/docs/jquery.js` and `clipboard.js`, which then dominated
/// every search result.
fn chunk_directory<P: AsRef<Path>>(path: P, options: &ChunkOptions) -> TldrResult<ChunkResult> {
    let path = path.as_ref();
    let mut all_chunks = Vec::new();
    let mut all_skipped = Vec::new();

    for entry_path in enumerate_corpus_files(path) {
        match chunk_file(&entry_path, options) {
            Ok(result) => {
                all_chunks.extend(result.chunks);
                all_skipped.extend(result.skipped);
            }
            Err(e) => {
                all_skipped.push(SkippedFile {
                    path: entry_path.display().to_string(),
                    reason: format!("Error: {}", e),
                });
            }
        }
    }

    Ok(ChunkResult {
        stats: ChunkStats::from_parts(&all_chunks, &all_skipped),
        chunks: all_chunks,
        skipped: all_skipped,
    })
}

/// Check whether a single `file` would be included in the corpus enumerated by
/// [`enumerate_corpus_files`] — i.e., the same `ProjectWalker` filters
/// (`.gitignore`, `DEFAULT_EXCLUDE_DIRS`, generated-dir sentinels), the
/// `is_binary_or_hidden` gate, and the `Language::from_path` check all agree
/// the file is indexable. Used by the delta path (TLDR-ac0.6) as the first
/// gate before `chunk_file`, so a Notify for a non-corpus file is a no-op.
///
/// Implementation: builds a `WalkBuilder` rooted at `root` with the same
/// config as `ProjectWalker`, but prunes every directory that is NOT an
/// ancestor of `file` — so the walk visits only the O(depth) ancestor chain
/// plus the target file's siblings at the leaf level. The file is in the
/// corpus iff the walker yields it AND it passes `is_binary_or_hidden`.
pub fn is_corpus_file(root: &Path, file: &Path) -> bool {
    CorpusPolicy::accepts_path(root, file)
}

fn is_corpus_file_impl(root: &Path, file: &Path) -> bool {
    use crate::walker::{dir_has_generated_sentinel, DEFAULT_EXCLUDE_DIRS};
    use ignore::WalkBuilder;

    let canonical_file = match file.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let canonical_root = match root.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };

    let rel = match canonical_file.strip_prefix(&canonical_root) {
        Ok(r) => r,
        Err(_) => return false,
    };

    // Quick pre-checks before building the walker.
    if !CorpusPolicy::accepts_file(&canonical_file) {
        return false;
    }

    // Collect the ancestor chain from root to the file's parent so we can
    // prune the walk to only descend into these directories.
    let ancestors: std::collections::HashSet<std::path::PathBuf> = {
        let mut set = std::collections::HashSet::new();
        set.insert(canonical_root.clone());
        let mut cur = canonical_root.clone();
        for component in rel.parent().into_iter().flat_map(|p| p.components()) {
            cur = cur.join(component);
            set.insert(cur.clone());
        }
        set
    };

    let preserve_js_ts_dirs = crate::walker::root_is_js_ts_dominated(&canonical_root);
    let js_ts_preserved: &[&str] = if preserve_js_ts_dirs {
        crate::walker::JS_TS_PRESERVED_DIRS
    } else {
        &[]
    };

    let mut builder = WalkBuilder::new(&canonical_root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .follow_links(false)
        .max_depth(Some(rel.components().count()));

    // Honor `.tldrignore` (TLDR-1j2): the single-file corpus gate must agree
    // with `enumerate_corpus_files` (which walks via `ProjectWalker`, also
    // `.tldrignore`-aware), so the watcher delta path and the full warm build
    // make the same membership decision for an excluded dir.
    builder.add_custom_ignore_filename(crate::walker::TLDRIGNORE_FILE);

    builder.filter_entry(move |entry| {
        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
        if is_dir {
            // Only descend into ancestors of the target file.
            let entry_canon = entry
                .path()
                .canonicalize()
                .unwrap_or_else(|_| entry.path().to_path_buf());
            if !ancestors.contains(&entry_canon) {
                return false;
            }
            if let Some(name) = entry.file_name().to_str() {
                if js_ts_preserved.contains(&name) {
                    // preserved for JS/TS — defer to .gitignore
                } else if DEFAULT_EXCLUDE_DIRS.contains(&name) {
                    return false;
                }
            }
            if dir_has_generated_sentinel(entry.path()) {
                return false;
            }
        }
        true
    });

    for res in builder.build() {
        let Ok(entry) = res else { continue };
        let Some(ft) = entry.file_type() else {
            continue;
        };
        if !ft.is_file() {
            continue;
        }
        let entry_canon = match entry.path().canonicalize() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if entry_canon == canonical_file {
            return true;
        }
    }
    false
}

/// Enumerate the candidate source files under `root` — the **pre-parse** corpus
/// the chunker feeds to `chunk_file()`: `ProjectWalker` (honours `.gitignore`,
/// `.tldrignore`, `DEFAULT_EXCLUDE_DIRS`, generated-dir sentinels, and the
/// supported-language extension set) → regular files → not binary/hidden.
/// This is the SINGLE source of truth for "which files are in the corpus",
/// shared by `chunk_directory` and the store freshness gate (TLDR-kkt), so the
/// two can never drift. Membership is decided BEFORE parsing, so a supported
/// file that yields ZERO chunks (e.g. a `mod.rs` of only `pub mod`
/// declarations) still counts here — which is exactly what keeps the freshness
/// digest from spuriously flagging such files as additions.
pub(crate) fn enumerate_corpus_files(root: &Path) -> Vec<PathBuf> {
    let extensions = CorpusPolicy::supported_extensions();
    let mut files = Vec::new();
    for entry in crate::walker::ProjectWalker::new(root)
        .extensions(&extensions)
        .iter()
    {
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let entry_path = entry.path();
        if !CorpusPolicy::accepts_file(entry_path) {
            continue;
        }
        files.push(entry_path.to_path_buf());
    }
    files
}

/// Walk the corpus under `root` for a single [`Language`] and return
/// policy-aware [`ChunkStats`] without producing chunks or reading file
/// contents.
///
/// This is the count-only counterpart to [`chunk_code`]: it uses the same
/// `ProjectWalker` honoring `.gitignore`/`.tldrignore` and filters by
/// `language.extensions()`, but stops before parsing. Search-side code paths
/// (e.g. enriched/regex) that previously re-ran `ProjectWalker` to derive
/// `total_files_searched` should call this so the report's count agrees with
/// the same corpus policy that `chunk_code` honors. The returned stats have
/// `files_indexed` populated; everything else is zero because no file was
/// opened or skipped in this pass.
pub fn corpus_stats_for_language(root: &Path, language: Language) -> ChunkStats {
    let walker_extensions: Vec<&'static str> = language
        .extensions()
        .iter()
        .filter_map(|extension| extension.strip_prefix('.'))
        .collect();
    let mut files_indexed = 0usize;
    for entry in crate::walker::ProjectWalker::new(root)
        .extensions(&walker_extensions)
        .iter()
    {
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        files_indexed += 1;
    }
    ChunkStats {
        files_indexed,
        // No file was read or classified in this pass, so the per-file skip
        // buckets stay at 0; `files_skipped` likewise. `chunks_created` is the
        // honest nothing-emitted representation.
        files_skipped: 0,
        files_unsupported: 0,
        files_oversized: 0,
        chunks_created: 0,
    }
}

/// Check if a file is binary or hidden
fn is_binary_or_hidden(path: &Path) -> bool {
    // Check if hidden
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        for prefix in HIDDEN_PREFIXES {
            if name.starts_with(prefix) {
                return true;
            }
        }
    }

    // Check if binary extension
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ext_lower = ext.to_lowercase();
        for binary_ext in BINARY_EXTENSIONS {
            if ext_lower == *binary_ext {
                return true;
            }
        }
    }

    false
}

/// Create a file-level chunk
fn create_file_chunk(
    path: &Path,
    content: &str,
    language: Language,
    options: &ChunkOptions,
) -> CodeChunk {
    let max_size = if options.max_chunk_size > 0 {
        Some(options.max_chunk_size)
    } else {
        Some(DEFAULT_MAX_CHUNK_SIZE)
    };

    let (final_content, _truncated) = truncate_if_needed(content, max_size);
    let line_count = content.lines().count();

    CodeChunk {
        file_path: path.to_path_buf(),
        function_name: None,
        class_name: None,
        line_start: 1,
        line_end: line_count.max(1) as u32,
        content: final_content,
        content_hash: compute_hash(content),
        language,
    }
}

/// Truncate content if it exceeds max size
fn truncate_if_needed(content: &str, max_size: Option<usize>) -> (String, bool) {
    match max_size {
        Some(max) if content.len() > max => {
            // Truncate at character boundary
            let truncated = content
                .char_indices()
                .take_while(|(i, _)| *i < max)
                .map(|(_, c)| c)
                .collect::<String>();
            (truncated, true)
        }
        _ => (content.to_string(), false),
    }
}

/// Compute MD5 hash for content
fn compute_hash(content: &str) -> String {
    format!("{:x}", md5::compute(content.as_bytes()))
}

// =============================================================================
// Function Extraction
// =============================================================================

/// Internal struct for extracted function data
struct ExtractedFunction {
    name: String,
    class_name: Option<String>,
    line_start: u32,
    line_end: u32,
    content: String,
}

/// Extract function-level chunks from a parsed tree
fn extract_function_chunks(
    tree: &Tree,
    source: &str,
    path: &Path,
    language: Language,
    options: &ChunkOptions,
) -> Vec<CodeChunk> {
    let root = tree.root_node();
    let mut functions = Vec::new();

    // Extract functions based on language
    match language {
        Language::Python => extract_python_all_functions(&root, source, &mut functions),
        Language::TypeScript | Language::JavaScript => {
            extract_ts_all_functions(&root, source, &mut functions)
        }
        Language::Rust => extract_rust_all_functions(&root, source, &mut functions),
        Language::Go => extract_go_all_functions(&root, source, &mut functions),
        Language::Java => extract_java_all_functions(&root, source, &mut functions),
        _ => {}
    }

    // Convert to CodeChunks
    let max_size = if options.max_chunk_size > 0 {
        Some(options.max_chunk_size)
    } else {
        Some(DEFAULT_MAX_CHUNK_SIZE)
    };

    functions
        .into_iter()
        .map(|func| {
            let (final_content, _truncated) = truncate_if_needed(&func.content, max_size);

            CodeChunk {
                file_path: path.to_path_buf(),
                function_name: Some(func.name),
                class_name: func.class_name,
                line_start: func.line_start,
                line_end: func.line_end,
                content: final_content,
                content_hash: compute_hash(&func.content),
                language,
            }
        })
        .collect()
}

/// Get text content of a node
fn get_node_text(node: &Node, source: &str) -> String {
    source[node.byte_range()].to_string()
}

/// Get line numbers (1-indexed) for a node
fn get_line_range(node: &Node) -> (u32, u32) {
    let start = node.start_position().row + 1;
    let end = node.end_position().row + 1;
    (start as u32, end as u32)
}

// =============================================================================
// Python Function Extraction
// =============================================================================

/// Extract ALL Python functions (including methods, lambdas, nested)
fn extract_python_all_functions(node: &Node, source: &str, functions: &mut Vec<ExtractedFunction>) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_definition" => {
                // Regular function or method
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = get_node_text(&name_node, source);
                    let (line_start, line_end) = get_line_range(&child);
                    let content = get_node_text(&child, source);

                    // Check if it's a method (inside a class)
                    let class_name = get_enclosing_class_name(&child, source);

                    functions.push(ExtractedFunction {
                        name,
                        class_name,
                        line_start,
                        line_end,
                        content,
                    });
                }

                // Recurse to find nested functions
                if let Some(body) = child.child_by_field_name("body") {
                    extract_python_all_functions(&body, source, functions);
                }
            }
            "lambda" => {
                // Lambda functions - create a synthetic name
                let (line_start, line_end) = get_line_range(&child);
                let content = get_node_text(&child, source);

                // Try to get the variable name if assigned
                let name = get_lambda_name(&child, source).unwrap_or_else(|| {
                    format!("<lambda:{}:{}>", line_start, child.start_position().column)
                });

                functions.push(ExtractedFunction {
                    name,
                    class_name: None,
                    line_start,
                    line_end,
                    content,
                });
            }
            "class_definition" => {
                // Recurse into class body
                if let Some(body) = child.child_by_field_name("body") {
                    extract_python_all_functions(&body, source, functions);
                }
            }
            _ => {
                // Recurse into other nodes
                extract_python_all_functions(&child, source, functions);
            }
        }
    }
}

// =============================================================================
// TypeScript/JavaScript Function Extraction
// =============================================================================

/// Extract ALL TypeScript/JavaScript functions (including arrow, async, methods)
fn extract_ts_all_functions(node: &Node, source: &str, functions: &mut Vec<ExtractedFunction>) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_declaration" | "function" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = get_node_text(&name_node, source);
                    let (line_start, line_end) = get_line_range(&child);
                    let content = get_node_text(&child, source);

                    functions.push(ExtractedFunction {
                        name,
                        class_name: get_enclosing_class_name(&child, source),
                        line_start,
                        line_end,
                        content,
                    });
                }

                // Recurse into body for nested functions
                if let Some(body) = child.child_by_field_name("body") {
                    extract_ts_all_functions(&body, source, functions);
                }
            }
            "method_definition" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = get_node_text(&name_node, source);
                    let (line_start, line_end) = get_line_range(&child);
                    let content = get_node_text(&child, source);

                    functions.push(ExtractedFunction {
                        name,
                        class_name: get_enclosing_class_name(&child, source),
                        line_start,
                        line_end,
                        content,
                    });
                }
            }
            "arrow_function" => {
                // Arrow functions - get name from variable declarator
                let (line_start, line_end) = get_line_range(&child);
                let content = get_node_text(&child, source);

                let name = get_arrow_function_name(&child, source).unwrap_or_else(|| {
                    format!("<arrow:{}:{}>", line_start, child.start_position().column)
                });

                functions.push(ExtractedFunction {
                    name,
                    class_name: get_enclosing_class_name(&child, source),
                    line_start,
                    line_end,
                    content,
                });

                // Recurse into body
                if let Some(body) = child.child_by_field_name("body") {
                    extract_ts_all_functions(&body, source, functions);
                }
            }
            "class_declaration" | "class" => {
                if let Some(body) = child.child_by_field_name("body") {
                    extract_ts_all_functions(&body, source, functions);
                }
            }
            _ => {
                extract_ts_all_functions(&child, source, functions);
            }
        }
    }
}

/// Get arrow function name from parent variable declarator
fn get_arrow_function_name(node: &Node, source: &str) -> Option<String> {
    if let Some(parent) = node.parent() {
        if parent.kind() == "variable_declarator" {
            if let Some(name_node) = parent.child_by_field_name("name") {
                return Some(get_node_text(&name_node, source));
            }
        }
    }
    None
}

// =============================================================================
// Rust Function Extraction
// =============================================================================

/// Extract ALL Rust functions (including impl methods, closures, async)
fn extract_rust_all_functions(node: &Node, source: &str, functions: &mut Vec<ExtractedFunction>) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_item" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = get_node_text(&name_node, source);
                    let (line_start, line_end) = get_line_range(&child);
                    let content = get_node_text(&child, source);

                    // Check if inside impl block
                    let class_name = get_rust_impl_type(&child, source);

                    functions.push(ExtractedFunction {
                        name,
                        class_name,
                        line_start,
                        line_end,
                        content,
                    });
                }

                // Recurse into body for nested functions/closures
                if let Some(body) = child.child_by_field_name("body") {
                    extract_rust_all_functions(&body, source, functions);
                }
            }
            "closure_expression" => {
                let (line_start, line_end) = get_line_range(&child);
                let content = get_node_text(&child, source);

                // Try to get name from let binding
                let name = get_rust_closure_name(&child, source).unwrap_or_else(|| {
                    format!("<closure:{}:{}>", line_start, child.start_position().column)
                });

                functions.push(ExtractedFunction {
                    name,
                    class_name: None,
                    line_start,
                    line_end,
                    content,
                });
            }
            "impl_item" => {
                // Recurse into impl body
                if let Some(body) = child.child_by_field_name("body") {
                    extract_rust_all_functions(&body, source, functions);
                }
            }
            _ => {
                extract_rust_all_functions(&child, source, functions);
            }
        }
    }
}

/// Get the type name from an enclosing impl block
fn get_rust_impl_type(node: &Node, source: &str) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "impl_item" {
            // Try to get the type name
            if let Some(type_node) = parent.child_by_field_name("type") {
                return Some(get_node_text(&type_node, source));
            }
        }
        current = parent.parent();
    }
    None
}

/// Get closure name from let binding
fn get_rust_closure_name(node: &Node, source: &str) -> Option<String> {
    if let Some(parent) = node.parent() {
        if parent.kind() == "let_declaration" {
            if let Some(pattern) = parent.child_by_field_name("pattern") {
                return Some(get_node_text(&pattern, source));
            }
        }
    }
    None
}

// =============================================================================
// Go Function Extraction
// =============================================================================

/// Extract ALL Go functions (including methods)
fn extract_go_all_functions(node: &Node, source: &str, functions: &mut Vec<ExtractedFunction>) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_declaration" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = get_node_text(&name_node, source);
                    let (line_start, line_end) = get_line_range(&child);
                    let content = get_node_text(&child, source);

                    functions.push(ExtractedFunction {
                        name,
                        class_name: None,
                        line_start,
                        line_end,
                        content,
                    });
                }
            }
            "method_declaration" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = get_node_text(&name_node, source);
                    let (line_start, line_end) = get_line_range(&child);
                    let content = get_node_text(&child, source);

                    // Get receiver type as "class_name"
                    let class_name = child
                        .child_by_field_name("receiver")
                        .and_then(|r| get_go_receiver_type(&r, source));

                    functions.push(ExtractedFunction {
                        name,
                        class_name,
                        line_start,
                        line_end,
                        content,
                    });
                }
            }
            "func_literal" => {
                // Anonymous function literal
                let (line_start, line_end) = get_line_range(&child);
                let content = get_node_text(&child, source);

                let name = format!("<func:{}:{}>", line_start, child.start_position().column);

                functions.push(ExtractedFunction {
                    name,
                    class_name: None,
                    line_start,
                    line_end,
                    content,
                });
            }
            _ => {
                extract_go_all_functions(&child, source, functions);
            }
        }
    }
}

/// Get Go method receiver type
fn get_go_receiver_type(receiver: &Node, source: &str) -> Option<String> {
    let mut cursor = receiver.walk();
    for child in receiver.children(&mut cursor) {
        if child.kind() == "parameter_declaration" {
            if let Some(type_node) = child.child_by_field_name("type") {
                let type_text = get_node_text(&type_node, source);
                // Strip pointer if present
                return Some(type_text.trim_start_matches('*').to_string());
            }
        }
    }
    None
}

// =============================================================================
// Java Function Extraction
// =============================================================================

/// Extract ALL Java methods
fn extract_java_all_functions(node: &Node, source: &str, functions: &mut Vec<ExtractedFunction>) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "method_declaration" | "constructor_declaration" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = get_node_text(&name_node, source);
                    let (line_start, line_end) = get_line_range(&child);
                    let content = get_node_text(&child, source);

                    functions.push(ExtractedFunction {
                        name,
                        class_name: get_enclosing_class_name(&child, source),
                        line_start,
                        line_end,
                        content,
                    });
                }
            }
            "lambda_expression" => {
                let (line_start, line_end) = get_line_range(&child);
                let content = get_node_text(&child, source);

                let name = format!("<lambda:{}:{}>", line_start, child.start_position().column);

                functions.push(ExtractedFunction {
                    name,
                    class_name: get_enclosing_class_name(&child, source),
                    line_start,
                    line_end,
                    content,
                });
            }
            "class_declaration" | "interface_declaration" | "enum_declaration" => {
                // Recurse into class body
                if let Some(body) = child.child_by_field_name("body") {
                    extract_java_all_functions(&body, source, functions);
                }
            }
            _ => {
                extract_java_all_functions(&child, source, functions);
            }
        }
    }
}

// =============================================================================
// Helpers
// =============================================================================

/// Get the name of the enclosing class/struct
fn get_enclosing_class_name(node: &Node, source: &str) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "class_definition" | "class_declaration" | "class" => {
                if let Some(name_node) = parent.child_by_field_name("name") {
                    return Some(get_node_text(&name_node, source));
                }
            }
            "impl_item" => {
                if let Some(type_node) = parent.child_by_field_name("type") {
                    return Some(get_node_text(&type_node, source));
                }
            }
            _ => {}
        }
        current = parent.parent();
    }
    None
}

/// Get lambda variable name from assignment
fn get_lambda_name(node: &Node, source: &str) -> Option<String> {
    if let Some(parent) = node.parent() {
        // Check for assignment: x = lambda: ...
        if parent.kind() == "assignment" {
            if let Some(left) = parent.child_by_field_name("left") {
                return Some(get_node_text(&left, source));
            }
        }
        // Check for named expression: x := lambda: ...
        if parent.kind() == "named_expression" {
            if let Some(name) = parent.child_by_field_name("name") {
                return Some(get_node_text(&name, source));
            }
        }
    }
    None
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod chunker_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn chunk_options_default_values() {
        let options = ChunkOptions::default();
        assert_eq!(options.granularity, ChunkGranularity::Function);
        assert_eq!(options.max_chunk_size, 0); // 0 means use default
        assert!(!options.include_docs);
        assert!(options.languages.is_none());
    }

    #[test]
    fn chunk_file_rust_function_extraction() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");

        fs::write(
            &file_path,
            r#"
fn foo() {
    println!("foo");
}

fn bar(x: i32) -> i32 {
    x * 2
}

impl MyStruct {
    fn method(&self) {
        // method
    }
}
"#,
        )
        .unwrap();

        let result = chunk_file(&file_path, &ChunkOptions::default()).unwrap();

        assert!(result.skipped.is_empty());
        assert!(result.chunks.len() >= 3);

        let names: Vec<_> = result
            .chunks
            .iter()
            .filter_map(|c| c.function_name.as_ref())
            .collect();

        assert!(names.contains(&&"foo".to_string()));
        assert!(names.contains(&&"bar".to_string()));
        assert!(names.contains(&&"method".to_string()));
    }

    #[test]
    fn chunk_file_python_function_extraction() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.py");

        fs::write(
            &file_path,
            r#"
def foo():
    pass

def bar(x):
    return x * 2

class MyClass:
    def method(self):
        pass
"#,
        )
        .unwrap();

        let result = chunk_file(&file_path, &ChunkOptions::default()).unwrap();

        assert!(result.skipped.is_empty());
        assert!(result.chunks.len() >= 3);

        let names: Vec<_> = result
            .chunks
            .iter()
            .filter_map(|c| c.function_name.as_ref())
            .collect();

        assert!(names.contains(&&"foo".to_string()));
        assert!(names.contains(&&"bar".to_string()));
        assert!(names.contains(&&"method".to_string()));
    }

    #[test]
    fn chunk_file_file_level_granularity() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");

        fs::write(
            &file_path,
            r#"
fn foo() {}
fn bar() {}
"#,
        )
        .unwrap();

        let options = ChunkOptions {
            granularity: ChunkGranularity::File,
            ..Default::default()
        };

        let result = chunk_file(&file_path, &options).unwrap();

        // Should be exactly 1 chunk for the whole file
        assert_eq!(result.chunks.len(), 1);
        assert!(result.chunks[0].function_name.is_none());
        assert!(result.chunks[0].content.contains("fn foo()"));
        assert!(result.chunks[0].content.contains("fn bar()"));
    }

    #[test]
    fn chunk_code_directory_traversal() {
        let tmp = TempDir::new().unwrap();

        // Create multiple files
        fs::write(tmp.path().join("a.rs"), "fn a() {}").unwrap();
        fs::write(tmp.path().join("b.py"), "def b(): pass").unwrap();

        // Create a subdirectory with files
        let sub = tmp.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("c.rs"), "fn c() {}").unwrap();

        let result = chunk_code(tmp.path(), &ChunkOptions::default()).unwrap();

        // Should find functions from all files
        assert!(!result.chunks.is_empty(), "Should have found some chunks");

        let names: Vec<_> = result
            .chunks
            .iter()
            .filter_map(|c| c.function_name.as_ref())
            .collect();

        // Rust files should have function extraction
        assert!(
            names.contains(&&"a".to_string()),
            "Should find function 'a' from a.rs"
        );
        assert!(
            names.contains(&&"c".to_string()),
            "Should find function 'c' from sub/c.rs"
        );

        // Python may or may not extract 'b' depending on parser support
        // Either we get the function, or a file-level chunk
        let has_b = names.contains(&&"b".to_string())
            || result
                .chunks
                .iter()
                .any(|c| c.file_path.to_string_lossy().contains("b.py"));
        assert!(has_b, "Should have b.py in some form");
    }

    #[test]
    fn chunk_file_nonexistent_returns_error() {
        let result = chunk_code("/nonexistent/path/to/file.rs", &ChunkOptions::default());
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TldrError::PathNotFound(_)));
    }

    #[test]
    fn chunk_file_binary_file_skipped() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.exe");

        fs::write(&file_path, [0u8; 100]).unwrap();

        let result = chunk_file(&file_path, &ChunkOptions::default()).unwrap();

        assert!(result.chunks.is_empty());
        assert_eq!(result.skipped.len(), 1);
        assert!(result.skipped[0].reason.contains("Binary"));
    }

    #[test]
    fn chunk_file_includes_content_hash() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");

        fs::write(&file_path, "fn foo() {}").unwrap();

        let result = chunk_file(&file_path, &ChunkOptions::default()).unwrap();

        assert!(!result.chunks.is_empty());
        let chunk = &result.chunks[0];

        // Hash should be non-empty and valid hex
        assert!(!chunk.content_hash.is_empty());
        assert!(chunk.content_hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn chunk_file_consistent_hashing() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");

        fs::write(&file_path, "fn foo() {}").unwrap();

        let result1 = chunk_file(&file_path, &ChunkOptions::default()).unwrap();
        let result2 = chunk_file(&file_path, &ChunkOptions::default()).unwrap();

        // Same content should produce same hash
        assert_eq!(
            result1.chunks[0].content_hash,
            result2.chunks[0].content_hash
        );
    }

    #[test]
    fn chunk_file_hidden_file_skipped() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join(".hidden.rs");

        fs::write(&file_path, "fn foo() {}").unwrap();

        let result = chunk_file(&file_path, &ChunkOptions::default()).unwrap();

        assert!(result.chunks.is_empty());
        assert_eq!(result.skipped.len(), 1);
        assert!(result.skipped[0].reason.contains("hidden"));
    }

    #[test]
    fn chunk_file_language_filter() {
        let tmp = TempDir::new().unwrap();
        let rust_file = tmp.path().join("test.rs");
        let py_file = tmp.path().join("test.py");

        fs::write(&rust_file, "fn foo() {}").unwrap();
        fs::write(&py_file, "def bar(): pass").unwrap();

        // Filter to only Rust
        let options = ChunkOptions {
            languages: Some(vec![Language::Rust]),
            ..Default::default()
        };

        let result = chunk_code(tmp.path(), &options).unwrap();

        // Should only have Rust functions
        let names: Vec<_> = result
            .chunks
            .iter()
            .filter_map(|c| c.function_name.as_ref())
            .collect();

        assert!(names.contains(&&"foo".to_string()));
        assert!(!names.contains(&&"bar".to_string()));

        // Python file should be in skipped
        assert!(result.skipped.iter().any(|s| s.path.contains("test.py")));
    }

    #[test]
    fn chunk_file_truncation() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");

        // Create a file with content longer than max
        let long_content = format!("fn foo() {{\n{}\n}}", "    let x = 1;\n".repeat(500));
        fs::write(&file_path, &long_content).unwrap();

        let options = ChunkOptions {
            max_chunk_size: 100, // Very small limit
            ..Default::default()
        };

        let result = chunk_file(&file_path, &options).unwrap();

        assert!(!result.chunks.is_empty());
        // Content should be truncated
        assert!(result.chunks[0].content.len() <= 100);
    }

    #[test]
    fn chunk_file_unknown_language_skipped() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.xyz");

        fs::write(&file_path, "some content").unwrap();

        let result = chunk_file(&file_path, &ChunkOptions::default()).unwrap();

        assert!(result.chunks.is_empty());
        assert_eq!(result.skipped.len(), 1);
        assert!(result.skipped[0].reason.contains("Unknown language"));
        assert_eq!(result.stats.files_skipped, 1);
        assert_eq!(result.stats.files_unsupported, 1);
        assert_eq!(result.stats.chunks_created, 0);
    }

    #[test]
    fn corpus_policy_matches_supported_source_candidates() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("main.cpp");
        let data = tmp.path().join("events.csv");
        fs::write(&source, "int main() { return 0; }\n").unwrap();
        fs::write(&data, "id,value\n1,large\n").unwrap();

        assert!(CorpusPolicy::accepts_file(&source));
        assert!(!CorpusPolicy::accepts_file(&data));
        assert!(CorpusPolicy::supported_extensions().contains(&"cpp"));
    }

    #[test]
    fn chunk_directory_skips_node_modules() {
        let tmp = TempDir::new().unwrap();

        // Create a file in root
        fs::write(tmp.path().join("main.rs"), "fn main() {}").unwrap();

        // Create node_modules with a file
        let node_modules = tmp.path().join("node_modules");
        fs::create_dir(&node_modules).unwrap();
        fs::write(node_modules.join("dep.js"), "function dep() {}").unwrap();

        let result = chunk_code(tmp.path(), &ChunkOptions::default()).unwrap();

        // Should only find main, not dep
        let names: Vec<_> = result
            .chunks
            .iter()
            .filter_map(|c| c.function_name.as_ref())
            .collect();

        assert!(names.contains(&&"main".to_string()));
        assert!(!names.iter().any(|n| *n == "dep"));
    }

    // --- is_corpus_file (TLDR-ac0.6) ---
    // The single-file gate must agree with enumerate_corpus_files: same walker
    // rules (gitignore, DEFAULT_EXCLUDE_DIRS), is_binary_or_hidden, and the
    // Language::from_path check.

    #[test]
    fn is_corpus_file_accepts_recognized_source() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("src/lib.rs");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "fn f() {}\n").unwrap();
        assert!(is_corpus_file(tmp.path(), &file));
    }

    #[test]
    fn is_corpus_file_rejects_unknown_extension() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("src/data.xyz");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "not source\n").unwrap();
        assert!(!is_corpus_file(tmp.path(), &file));
    }

    #[test]
    fn is_corpus_file_rejects_excluded_dir() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("node_modules/foo/bar.js");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "function f() {}\n").unwrap();
        assert!(!is_corpus_file(tmp.path(), &file));
    }

    #[test]
    fn is_corpus_file_rejects_gitignored() {
        let tmp = TempDir::new().unwrap();
        // The ignore crate only honours .gitignore inside a git repo.
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        fs::write(tmp.path().join(".gitignore"), "generated/\n").unwrap();
        let file = tmp.path().join("generated/auto.py");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "def gen(): pass\n").unwrap();
        assert!(!is_corpus_file(tmp.path(), &file));
    }

    #[test]
    fn is_corpus_file_rejects_missing_file() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("src/gone.rs");
        // Never created — canonicalize fails, so it can't be in the corpus.
        assert!(!is_corpus_file(tmp.path(), &file));
    }

    #[test]
    fn is_corpus_file_rejects_tldrignored() {
        // TLDR-1j2: `.tldrignore` is a custom ignore filename, honored even
        // without a git repo. The single-file gate must drop excluded paths so
        // the watcher delta path agrees with `enumerate_corpus_files`.
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".tldrignore"), "vendored/\n").unwrap();
        let file = tmp.path().join("vendored/dep.py");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "def f(): pass\n").unwrap();
        assert!(!is_corpus_file(tmp.path(), &file));

        // A normal source file alongside it is still accepted.
        let ok = tmp.path().join("main.py");
        fs::write(&ok, "def g(): pass\n").unwrap();
        assert!(is_corpus_file(tmp.path(), &ok));
    }

    #[test]
    fn enumerate_corpus_files_excludes_tldrignored() {
        // TLDR-1qv (closed-as-bonus here): the full warm-build corpus must also
        // honor `.tldrignore`, or the build over-indexes excluded dirs and
        // disagrees with the single-file gate.
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".tldrignore"), "vendored/\n").unwrap();
        fs::write(tmp.path().join("main.py"), "def g(): pass\n").unwrap();
        let v = tmp.path().join("vendored/dep.py");
        fs::create_dir_all(v.parent().unwrap()).unwrap();
        fs::write(&v, "def f(): pass\n").unwrap();

        let files = enumerate_corpus_files(tmp.path());
        assert!(
            files.iter().any(|p| p.ends_with("main.py")),
            "main.py should be enumerated: {files:?}"
        );
        assert!(
            !files.iter().any(|p| p.ends_with("dep.py")),
            "tldrignored file must not be enumerated: {files:?}"
        );
    }

    #[test]
    fn enumerate_corpus_files_excludes_unsupported_extensions() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("main.cpp"), "int main() { return 0; }\n").unwrap();
        fs::write(tmp.path().join("events.csv"), "id,value\n1,large\n").unwrap();
        fs::write(tmp.path().join("server.log"), "request completed\n").unwrap();

        let files = enumerate_corpus_files(tmp.path());
        assert_eq!(
            files,
            vec![tmp.path().join("main.cpp")],
            "only supported source files belong to the semantic corpus: {files:?}"
        );
    }

    #[test]
    fn chunk_file_skips_oversized_generated_source_before_reading() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("generated.d.ts");
        fs::write(
            &path,
            vec![b'a'; crate::fs::oversize::MAX_AUTOGEN_FILE_SIZE_BYTES as usize + 1],
        )
        .unwrap();

        let result = chunk_file(&path, &ChunkOptions::default()).unwrap();
        assert!(result.chunks.is_empty());
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.stats.files_oversized, 1);
        assert_eq!(result.stats.files_skipped, 1);
        assert!(
            result.skipped[0].reason.contains("exceeds 512KB cap"),
            "unexpected oversize reason: {}",
            result.skipped[0].reason
        );
    }

    #[test]
    fn corpus_stats_for_language_counts_only_matching_extensions() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.rs"), "fn a() {}\n").unwrap();
        fs::write(tmp.path().join("b.rs"), "fn b() {}\n").unwrap();
        fs::write(tmp.path().join("c.py"), "def c(): pass\n").unwrap();

        let rust_stats = corpus_stats_for_language(tmp.path(), Language::Rust);
        assert_eq!(rust_stats.files_indexed, 2, "two .rs files in corpus");
        assert_eq!(rust_stats.chunks_created, 0);
        assert_eq!(rust_stats.files_skipped, 0);

        let py_stats = corpus_stats_for_language(tmp.path(), Language::Python);
        assert_eq!(py_stats.files_indexed, 1, "exactly one .py file");
    }

    #[test]
    fn corpus_stats_for_language_zero_when_no_matches() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("only.py"), "def x(): pass\n").unwrap();

        let stats = corpus_stats_for_language(tmp.path(), Language::Rust);
        assert_eq!(stats.files_indexed, 0);
        assert_eq!(stats.chunks_created, 0);
    }

    #[test]
    fn corpus_stats_for_language_honors_gitignore() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".gitignore"), "ignored.rs\n").unwrap();
        fs::write(tmp.path().join("kept.rs"), "fn k() {}\n").unwrap();
        fs::write(tmp.path().join("ignored.rs"), "fn i() {}\n").unwrap();

        let stats = corpus_stats_for_language(tmp.path(), Language::Rust);
        assert_eq!(
            stats.files_indexed, 1,
            "gitignored file must not be counted for the language"
        );
    }
}

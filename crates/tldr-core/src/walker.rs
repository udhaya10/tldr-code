//! Shared project walker built on `ignore::WalkBuilder`.
//!
//! Honors `.gitignore`, skips hidden dirs, skips vendor/build dirs by default,
//! does not follow symlinks. Every project-wide filesystem walk in tldr
//! should go through this module instead of using `walkdir::WalkDir` directly.
//!
//! # Why this exists
//!
//! Raw `walkdir::WalkDir` doesn't honor `.gitignore`, doesn't skip vendor dirs
//! (like `node_modules`, `target`, `dist`), and by default follows symlinks.
//! In pnpm monorepos `node_modules/.pnpm/` is a symlink forest that causes
//! infinite loops (`tldr smells` on a 2GB pnpm repo ran for 10+ minutes
//! before being killed) and produces false findings inside vendored code.
//!
//! # Typical usage
//!
//! ```rust,ignore
//! use tldr_core::walker::walk_project;
//!
//! for entry in walk_project("src") {
//!     // `entry` is an `ignore::DirEntry` yielded only for non-ignored files.
//! }
//! ```
//!
//! Or with more control:
//!
//! ```rust,ignore
//! use tldr_core::walker::ProjectWalker;
//!
//! let files: Vec<_> = ProjectWalker::new("src")
//!     .max_depth(10)
//!     .extensions(&["rs"])
//!     .iter()
//!     .collect();
//! ```

use std::path::{Path, PathBuf};

use ignore::{DirEntry, WalkBuilder};

/// The conventional tldr-specific ignore filename, honored alongside
/// `.gitignore` by every project walk (TLDR-1j2 / TLDR-vti, epic TLDR-bpf).
///
/// Registered on the [`ignore::WalkBuilder`] of each walker via
/// [`WalkBuilder::add_custom_ignore_filename`], so it gets full gitignore
/// semantics (per-directory scope, negation, parent matching) for free — the
/// same engine that already powers `.gitignore` support.
pub const TLDRIGNORE_FILE: &str = ".tldrignore";

/// An opaque, thread-safe `.tldrignore`/`.gitignore` matcher for per-path
/// ignore checks, returned by [`build_path_ignore_matcher`].
///
/// Wraps the `ignore` crate's [`ignore::gitignore::Gitignore`] so callers
/// (e.g. the `tldr-cli` daemon watcher) get the matching semantics without
/// taking a direct dependency on the `ignore` crate.
#[derive(Debug, Clone)]
pub struct PathIgnoreMatcher {
    gitignore: Option<ignore::gitignore::Gitignore>,
    tldrignore: Option<ignore::gitignore::Gitignore>,
}

impl PathIgnoreMatcher {
    /// Returns `true` when `path` is excluded by the loaded `.tldrignore` /
    /// `.gitignore` patterns. Uses `matched_path_or_any_parents` so a
    /// directory pattern (e.g. `vendored/`) also excludes files nested under
    /// it — essential for the watcher's vanished-path (delete) branch, where
    /// only the parent dir pattern can be consulted. `is_dir` should be
    /// `false` for a path that no longer exists.
    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        self.gitignore.as_ref().is_some_and(|matcher| {
            matcher
                .matched_path_or_any_parents(path, is_dir)
                .is_ignore()
        }) || self.tldrignore.as_ref().is_some_and(|matcher| {
            matcher
                .matched_path_or_any_parents(path, is_dir)
                .is_ignore()
        })
    }
}

/// Build a standalone gitignore-semantics matcher for per-path ignore checks
/// where a full [`WalkBuilder`] traversal isn't available — notably the
/// in-daemon watcher's vanished-path (delete) branch, which cannot walk a path
/// that no longer exists on disk (TLDR-1j2).
///
/// Honors `<root>/.tldrignore` and, when `include_gitignore` is set, also
/// `<root>/.gitignore` (to drop deleted gitignored files before a wasted
/// reindex hop). Returns `None` when neither file contributes a usable pattern,
/// in which case callers treat every path as non-ignored.
///
/// This is the single shared `.tldrignore` matcher loader for the matcher-only
/// callers (epic TLDR-bpf / TLDR-9w8 direction); the walker paths register the
/// filename on their `WalkBuilder` directly. Root-level only by design — nested
/// `.tldrignore`/`.gitignore` files are covered by the full walkers
/// ([`ProjectWalker`], `is_corpus_file`) for paths that still exist.
pub fn build_path_ignore_matcher(
    root: &Path,
    include_gitignore: bool,
) -> Option<PathIgnoreMatcher> {
    use ignore::gitignore::GitignoreBuilder;

    let mut gitignore = None;
    let mut tldrignore = None;

    let tldrignore_path = root.join(TLDRIGNORE_FILE);
    if tldrignore_path.is_file() {
        let mut builder = GitignoreBuilder::new(root);
        let mut valid = true;
        if let Ok(contents) = std::fs::read_to_string(&tldrignore_path) {
            for line in contents.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let deny_pattern = line.strip_prefix('!').unwrap_or(line);
                if builder.add_line(None, deny_pattern).is_err() {
                    valid = false;
                    break;
                }
            }
        } else {
            valid = false;
        }
        if valid {
            tldrignore = builder.build().ok();
        }
    }

    if include_gitignore {
        let gitignore_path = root.join(".gitignore");
        if gitignore_path.is_file() {
            let mut builder = GitignoreBuilder::new(root);
            if builder.add(&gitignore_path).is_none() {
                gitignore = builder.build().ok();
            }
        }
    }

    if gitignore.is_none() && tldrignore.is_none() {
        return None;
    }
    Some(PathIgnoreMatcher {
        gitignore,
        tldrignore,
    })
}

/// Directories skipped by default regardless of `.gitignore` presence.
///
/// Commands that explicitly need to scan vendored code (e.g. auditing
/// dependencies) can disable this list via
/// [`ProjectWalker::no_default_ignore`].
///
/// **api-check-and-patterns-accuracy-v1 (P11.BUG-AGG-7)**: extended this
/// list to include common generated/vendored artifact dirs that previously
/// polluted language autodetection (e.g. doxygen `dox/` output, sphinx
/// `_build/`, gradle/maven build sinks, Python venvs and caches). These
/// directories ship in many third-party repositories — without skipping
/// them, `tldr patterns /tmp/repos/cpp-tinyxml2` mis-classified the project
/// as JavaScript-majority because the `docs/` doxygen output contained 63
/// generated `.js` files vs 3 actual `.cpp` source files.
pub const DEFAULT_EXCLUDE_DIRS: &[&str] = &[
    // Vendored / package-manager output
    "node_modules",
    "vendor",
    // Build sinks (general)
    "target",
    "dist",
    "build",
    "out",
    "bin",
    "obj",
    // JavaScript framework caches
    ".next",
    ".nuxt",
    // Doxygen output (typical custom-config dir; the more common `docs/`
    // is detected via the `doxygen.css` sentinel below since `docs/` may
    // legitimately hold authored markdown).
    "dox",
    // Python tooling. `venv`/`env` are the non-dotfile virtualenv dir names
    // (`.venv`/`.env` are dotfiles and already caught by the hidden filter);
    // added in TLDR-boa.3 when `fs/tree.rs::DEFAULT_SKIP_DIRS` was collapsed
    // onto this list, preserving the old tree/text-skip behaviour for the two
    // entries that were stricter than the canonical walker.
    "__pycache__",
    "venv",
    "env",
    ".pytest_cache",
    ".tox",
    ".mypy_cache",
    ".ruff_cache",
    // Coverage artefacts
    "coverage",
    ".coverage",
    // JVM tooling
    ".gradle",
    // Version control
    ".git",
];

/// Files whose presence in a directory indicates it is generator output
/// rather than authored source. When a directory contains any of these
/// sentinels at its top level, the walker skips it (subject to
/// [`ProjectWalker::no_default_ignore`]).
///
/// This is the secondary mechanism used to detect generated docs whose
/// directory name is itself ambiguous (e.g. `docs/` may be authored
/// markdown OR doxygen html output). A name-only ignore list cannot
/// distinguish those without reading inside the dir.
///
/// Sentinels chosen here are unambiguous markers of *generated* output:
/// - `doxygen.css` / `doxygen.svg`: doxygen-emitted style/asset files
///   (placed alongside generated HTML+JS by `doxygen` in its target dir).
/// - `.doctrees/` is sphinx's internal cache, typically inside `_build/`.
const GENERATED_DIR_SENTINELS: &[&str] = &["doxygen.css", "doxygen.svg"];

/// JS/TS-friendly subset of [`DEFAULT_EXCLUDE_DIRS`]: directories that are
/// build sinks for some languages (Rust `build/`, Java `dist/`) but commonly
/// hold authored source for JS/TS (`src/build/emitter.ts` in ts-dom-gen,
/// monorepo `packages/x/dist/index.ts`). When a [`ProjectWalker`] is
/// configured with [`ProjectWalker::lang_hint`] set to JS or TS, these
/// names are NOT auto-excluded — the walker defers to `.gitignore` instead.
///
/// residual-bugs-v1 (P15.AGG14-7-cascade): mirrors the per-language gate
/// already in `crates/tldr-core/src/callgraph/scanner.rs`
/// (`should_skip_build_or_dist_for_lang`). Without this gate `tldr dead`
/// (which uses `ProjectWalker`) returned `functions_analyzed: 0` on
/// ts-dom-gen even though `tldr calls` (which uses the scanner) returned
/// 112 nodes / 200 edges from the same file (`src/build/emitter.ts`).
pub(crate) const JS_TS_PRESERVED_DIRS: &[&str] = &["build", "dist", "out", "bin", "obj"];

/// How a path is classified by the canonical walker.
///
/// Unifies the scattered eligibility signals — the implicit "did the walker
/// yield it?", the binary sniff, the oversize cap, the generated-dir
/// detection — into a single vocabulary. See [`ProjectWalker::classify_path`]
/// (cheap, pure-path) and [`classify_content`] (expensive, reads the file).
///
/// `Hidden`/`Ignored`/`Unsupported`/`Generated` are pure-path (no I/O);
/// `Binary`/`Oversized` require a `stat()` and, for binary, a byte sniff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathClass {
    /// Source file the walker would process: supported extension, within the
    /// size cap, valid UTF-8.
    Eligible,
    /// Matched `.gitignore` / `.tldrignore` / `.git/info/exclude`.
    Ignored,
    /// Dotfile/dotdir (the `ignore` crate's `hidden(true)` rule).
    Hidden,
    /// Extension not in the configured allow-list (or no recognisable language).
    Unsupported,
    /// Under a default-skip or sentinel-marked generated/vendored directory.
    Generated,
    /// Content is not valid UTF-8 (binary).
    Binary,
    /// Exceeds the size cap (`fs::oversize.rs` policy).
    Oversized,
}

/// Builder for project walks.
///
/// Produces an iterator of [`ignore::DirEntry`]s after applying:
/// - `.gitignore` / global gitignore / `.git/info/exclude` (default on)
/// - hidden-file filtering (always on)
/// - the [`DEFAULT_EXCLUDE_DIRS`] list (default on, disable via
///   [`ProjectWalker::no_default_ignore`])
/// - `follow_links(false)` (always — critical for pnpm symlink forests)
/// - optional max depth
/// - optional extension allow-list
/// - optional language hint that relaxes the JS/TS-friendly subset
///   ([`JS_TS_PRESERVED_DIRS`]) when set to `Language::JavaScript` or
///   `Language::TypeScript`
pub struct ProjectWalker {
    root: PathBuf,
    respect_gitignore: bool,
    default_ignore: bool,
    exclude_hidden: bool,
    max_depth: Option<usize>,
    extensions: Option<Vec<&'static str>>,
    lang_hint: Option<crate::types::Language>,
}

impl ProjectWalker {
    /// Create a walker rooted at `root` with all default filters on.
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            respect_gitignore: true,
            default_ignore: true,
            exclude_hidden: true,
            max_depth: None,
            extensions: None,
            lang_hint: None,
        }
    }

    /// Tell the walker which language it is being run for. When set to
    /// `Language::JavaScript` or `Language::TypeScript`, the walker stops
    /// auto-excluding [`JS_TS_PRESERVED_DIRS`] (`build`, `dist`, `out`,
    /// `bin`, `obj`) — JS/TS projects routinely keep authored source under
    /// these names. For all other languages the hint is a no-op (the
    /// default exclusion list applies).
    ///
    /// residual-bugs-v1 (P15.AGG14-7-cascade): without this hook, callers
    /// that already know the language (e.g. `tldr dead --lang typescript`)
    /// could not opt into the same per-language gate the call-graph
    /// scanner already implements, leading to 0-result outputs on repos
    /// like ts-dom-gen whose entire source surface lives at
    /// `src/build/emitter.ts`.
    pub fn lang_hint(mut self, lang: crate::types::Language) -> Self {
        self.lang_hint = Some(lang);
        self
    }

    /// Disable the [`DEFAULT_EXCLUDE_DIRS`] list.
    ///
    /// Use when a command explicitly needs to scan vendored code
    /// (e.g. `node_modules`, `target`). `.gitignore` is still honored
    /// unless [`ProjectWalker::respect_gitignore(false)`] is also set.
    pub fn no_default_ignore(mut self) -> Self {
        self.default_ignore = false;
        self
    }

    /// Control whether `.gitignore` rules are honored. Default: `true`.
    pub fn respect_gitignore(mut self, yes: bool) -> Self {
        self.respect_gitignore = yes;
        self
    }

    /// Include hidden (dotfile/dotdir) entries instead of skipping them.
    ///
    /// `ProjectWalker` skips hidden entries by default (the `ignore` crate's
    /// `hidden(true)`). Callers that need hidden entries — notably
    /// `get_file_tree`'s `exclude_hidden = false` path — opt in here. Added
    /// for TLDR-boa.2 so the canonical walker can faithfully replace the
    /// bespoke tree walker.
    pub fn include_hidden(mut self) -> Self {
        self.exclude_hidden = false;
        self
    }

    /// Limit recursion depth.
    pub fn max_depth(mut self, n: usize) -> Self {
        self.max_depth = Some(n);
        self
    }

    /// Only yield files with these extensions (e.g. `&["rs", "ts", "tsx"]`).
    ///
    /// Extensions should NOT include the leading dot. Matching is
    /// case-sensitive. Callers that want language-aware filtering should
    /// prefer `Language::from_path` after the walk.
    pub fn extensions(mut self, exts: &[&'static str]) -> Self {
        self.extensions = Some(exts.to_vec());
        self
    }

    /// Iterate yielded entries.
    ///
    /// Errors during traversal (permission denied, broken symlinks, etc.)
    /// are silently skipped — the caller gets only successful `DirEntry`s.
    pub fn iter(self) -> impl Iterator<Item = DirEntry> {
        let default_ignore = self.default_ignore;
        let extensions = self.extensions.clone();
        // residual-bugs-v1 (P15.AGG14-7-cascade): when the caller passes
        // a JS/TS language hint, the JS/TS-preserved subset of the
        // default exclude list is treated as opt-in (deferred to
        // `.gitignore`). Captured into a single bool so the closure
        // below stays cheap.
        //
        // cross-cutting-and-clear-fix-bugs-v1 (P18.X4): when no lang_hint
        // was supplied AND the root dir is dominated by JS/TS extensions
        // (counted permissively, ignoring the default skip list so the
        // count reflects actual content), opt into the same preservation
        // automatically. This fixes commands that don't explicitly set
        // `lang_hint` (patterns, deps, search, etc.) on JS/TS layouts
        // like `src/build/emitter.ts` where the only source is under a
        // name that's normally a build sink.
        let auto_js_ts = self.lang_hint.is_none() && root_is_js_ts_dominated(&self.root);
        let preserve_js_ts_dirs = auto_js_ts
            || matches!(
                self.lang_hint,
                Some(crate::types::Language::JavaScript) | Some(crate::types::Language::TypeScript)
            );

        let mut builder = WalkBuilder::new(&self.root);
        // Shared canonical walk config (hidden, gitignore family, `.tldrignore`,
        // follow_links). See [`apply_canonical_walk_config`].
        apply_canonical_walk_config(&mut builder, self.exclude_hidden, self.respect_gitignore);

        if let Some(depth) = self.max_depth {
            builder.max_depth(Some(depth));
        }

        let tldr_deny_matcher = if self.respect_gitignore {
            build_path_ignore_matcher(&self.root, true)
        } else {
            None
        };
        if default_ignore || tldr_deny_matcher.is_some() {
            builder.filter_entry(move |entry| {
                let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                let name = entry.file_name().to_str();
                // Shared with `ProjectWalker::classify_path`: the explicit
                // walk filters (root-level ignore deny-list + generated-dir
                // name/sentinel). `Some` => drop the entry, `None` => keep.
                classify_explicit_path(
                    entry.path(),
                    name,
                    is_dir,
                    tldr_deny_matcher.as_ref(),
                    preserve_js_ts_dirs,
                )
                .is_none()
            });
        }

        builder.build().filter_map(move |res| {
            let entry = res.ok()?;
            if let Some(ref allowed) = extensions {
                // Only apply extension filter to files; directories must
                // still pass through so we can descend into them.
                let is_file = entry.file_type().map(|ft| ft.is_file()).unwrap_or(false);
                if is_file {
                    let ext = entry.path().extension().and_then(|s| s.to_str());
                    match ext {
                        Some(e) if allowed.contains(&e) => Some(entry),
                        _ => None,
                    }
                } else {
                    Some(entry)
                }
            } else {
                Some(entry)
            }
        })
    }

    /// Classify a single path the way this walker would treat it during a
    /// walk — without performing one.
    ///
    /// Cheap (pure-path): no `stat()` or file read. Resolves `Hidden`,
    /// `Ignored` (root-level `.gitignore`/`.tldrignore` only), `Generated`,
    /// and `Unsupported` (extension). Use [`classify_content`] to resolve the
    /// remaining `Binary`/`Oversized`/`Eligible` cases on a file the caller
    /// actually intends to process.
    ///
    /// # Faithfulness notes
    ///
    /// - `Hidden`, `Generated` and `Unsupported` are authoritative here.
    /// - `Ignored` is a **best-effort under-approximation**: it reflects the
    ///   root-level ignore files only. The `ignore` crate's in-walk engine
    ///   (per-directory `.gitignore`, parent traversal) is richer and remains
    ///   authoritative during [`Self::iter`]. For the authoritative
    ///   high-throughput walk, use [`Self::iter`].
    /// - The auto JS/TS-dominance heuristic ([`root_is_js_ts_dominated`]) is
    ///   an `iter`-time optimisation and is NOT consulted here; only an
    ///   explicit [`Self::lang_hint`] relaxes the generated-dir list.
    pub fn classify_path(&self, path: &Path, is_dir: bool) -> PathClass {
        let name = path.file_name().and_then(|n| n.to_str());

        // Hidden: dotfile/dotdir. `iter` sets `hidden(self.exclude_hidden)`
        // on the ignore builder, so this check mirrors it.
        if self.exclude_hidden {
            if let Some(n) = name {
                if n.starts_with('.') && n != "." && n != ".." {
                    return PathClass::Hidden;
                }
            }
        }

        // Explicit walk filters — applied only when `iter` would install its
        // `filter_entry` (i.e. `default_ignore` OR a root-level ignore file).
        let deny = if self.respect_gitignore {
            build_path_ignore_matcher(&self.root, true)
        } else {
            None
        };
        if self.default_ignore || deny.is_some() {
            let preserve_js_ts = matches!(
                self.lang_hint,
                Some(crate::types::Language::JavaScript) | Some(crate::types::Language::TypeScript)
            );
            if let Some(class) =
                classify_explicit_path(path, name, is_dir, deny.as_ref(), preserve_js_ts)
            {
                return class;
            }
        }

        // Unsupported: extension not in the configured allow-list (files only).
        if !is_dir {
            if let Some(allowed) = &self.extensions {
                let ext = path.extension().and_then(|e| e.to_str());
                match ext {
                    Some(e) if allowed.contains(&e) => {}
                    _ => return PathClass::Unsupported,
                }
            }
        }

        PathClass::Eligible
    }
}

/// Whether a directory contains any [`GENERATED_DIR_SENTINELS`] at its
/// top level. Used by [`ProjectWalker::iter`]'s `filter_entry` to skip
/// generator output dirs whose name is ambiguous.
///
/// The check reads only the top-level entries of `dir`; nested matches
/// are not considered (a project that authors a `doxygen.css` deep inside
/// its source tree is legitimate). Errors during read (permission denied,
/// non-directory) are treated as "no sentinel found" — the walker then
/// falls through to the normal name-based exclusion logic.
pub(crate) fn dir_has_generated_sentinel(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            if GENERATED_DIR_SENTINELS.contains(&name) {
                return true;
            }
        }
    }
    false
}

/// Pure-path explicit-walk classification shared by [`ProjectWalker::iter`]'s
/// `filter_entry` and [`ProjectWalker::classify_path`].
///
/// Encodes ONLY the filters the walker applies explicitly (not the `ignore`
/// crate's in-walk `hidden(true)` / per-directory-`.gitignore` engine):
/// - root-level `.gitignore` + `.tldrignore` via `deny` (when present);
/// - generated/vendored directory names ([`DEFAULT_EXCLUDE_DIRS`], subject to
///   the JS/TS-preserved subset when `preserve_js_ts_dirs`);
/// - generator-output sentinel files ([`dir_has_generated_sentinel`]).
///
/// The directory-only checks mirror `iter`'s `filter_entry` exactly: files are
/// never matched against the exclude list (a file literally named
/// `node_modules` is yielded). Returns `Some(class)` when the entry should be
/// dropped, `None` when it passes the explicit filters.
fn classify_explicit_path(
    path: &Path,
    name: Option<&str>,
    is_dir: bool,
    deny: Option<&PathIgnoreMatcher>,
    preserve_js_ts_dirs: bool,
) -> Option<PathClass> {
    // Root-level ignore deny-list applies to files AND directories.
    if let Some(matcher) = deny {
        if matcher.is_ignored(path, is_dir) {
            return Some(PathClass::Ignored);
        }
    }

    // Name + sentinel checks are directory-only.
    if is_dir {
        if let Some(name) = name {
            let name_excluded = if preserve_js_ts_dirs && JS_TS_PRESERVED_DIRS.contains(&name) {
                // JS/TS callers commonly keep authored source under these
                // build-sink names — defer to `.gitignore`.
                false
            } else {
                DEFAULT_EXCLUDE_DIRS.contains(&name)
            };
            if name_excluded {
                return Some(PathClass::Generated);
            }
        }
        if dir_has_generated_sentinel(path) {
            return Some(PathClass::Generated);
        }
    }

    None
}

/// Apply the canonical project-walk **config** to a raw [`WalkBuilder`]:
/// hidden-file filtering, `.gitignore` / global gitignore / `.git/info/exclude`
/// / parent traversal, `.tldrignore` registration, and `follow_links(false)`.
///
/// This is the single config shared by [`ProjectWalker::iter`] and the
/// single-file corpus gate (`CorpusPolicy::accepts_path` →
/// `is_corpus_file_impl` in `semantic/chunker.rs`), so the two cannot drift on
/// *which* ignore sources they honour — only the *config* is shared.
///
/// Callers still own their own `filter_entry`: [`ProjectWalker::iter`] applies
/// [`DEFAULT_EXCLUDE_DIRS`] + generated-dir sentinels (via
/// [`classify_explicit_path`]); the single-file gate additionally prunes the
/// walk to the target file's ancestor chain.
///
/// # Why `.tldrignore` rides the `respect_gitignore` gate
///
/// `.tldrignore` is registered with full gitignore semantics at every
/// directory level (TLDR-1j2 / TLDR-vti). It is tied to the same
/// `respect_gitignore` gate as `.gitignore`: a caller that opts out of ignore
/// files (`--no-respect-ignore`) opts out of both. This is THE shared corpus
/// walk (`enumerate_corpus_files`), so honoring it here keeps the full warm
/// build consistent with the single-file gate (`is_corpus_file`).
pub(crate) fn apply_canonical_walk_config(
    builder: &mut WalkBuilder,
    exclude_hidden: bool,
    respect_gitignore: bool,
) {
    builder
        .hidden(exclude_hidden)
        .git_ignore(respect_gitignore)
        .git_global(respect_gitignore)
        .git_exclude(respect_gitignore)
        .parents(respect_gitignore)
        .follow_links(false); // CRITICAL: avoid pnpm symlink loops
    if respect_gitignore {
        builder.add_custom_ignore_filename(TLDRIGNORE_FILE);
    }
}

/// cross-cutting-and-clear-fix-bugs-v1 (P18.X4): permissive JS/TS
/// dominance check. Walks `dir` ignoring the default skip list (so the
/// count reflects what's REALLY there, not what's left after stripping
/// `build/`, `dist/`, etc.) and reports whether `.ts`/`.tsx`/`.js`/`.jsx`/
/// `.mjs`/`.cjs` files outnumber any other recognised language. Used to
/// opt ProjectWalker into JS/TS-preservation when the caller did not set
/// an explicit `lang_hint`.
///
/// To keep the cost bounded, the walk caps at 256 inspected files —
/// enough to disambiguate even small libraries without scanning a giant
/// monorepo every time.
pub(crate) fn root_is_js_ts_dominated(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let mut js_ts_count = 0usize;
    let mut other_count = 0usize;
    let mut inspected = 0usize;
    const CAP: usize = 256;
    let mut walker = WalkBuilder::new(dir);
    walker
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .follow_links(false);
    walker.add_custom_ignore_filename(TLDRIGNORE_FILE);
    for entry in walker.build().flatten() {
        if inspected >= CAP {
            break;
        }
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let p = entry.path();
        let Some(ext) = p.extension().and_then(|s| s.to_str()) else {
            continue;
        };
        match ext {
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
                js_ts_count += 1;
                inspected += 1;
            }
            "py" | "rs" | "go" | "java" | "c" | "cc" | "cpp" | "cxx" | "h" | "hpp" | "kt"
            | "swift" | "rb" | "php" | "scala" | "lua" | "luau" | "ex" | "exs" | "ml" | "mli"
            | "cs" => {
                other_count += 1;
                inspected += 1;
            }
            _ => {}
        }
    }
    js_ts_count > other_count && js_ts_count > 0
}

/// Content-based classification of a file: `Oversized` or `Binary`, else
/// `Eligible`.
///
/// Consolidates the two existing content policies onto the [`PathClass`]
/// vocabulary:
/// - [`PathClass::Oversized`] ← [`crate::fs::check_size`] (the autogen-aware
///   size cap from `fs::oversize.rs`);
/// - [`PathClass::Binary`] ← [`crate::fs::read_to_string_tolerant`] returning
///   [`crate::fs::ReadOutcome::NonUtf8`].
///
/// # Cost
///
/// This reads the whole file (to validate UTF-8). Production paths that already
/// read the file should detect binary/oversize from that single read rather
/// than calling this separately; this function exists to give the
/// classification a single, testable home.
///
/// Undeterminable files (un-stat-able or unreadable) return
/// [`PathClass::Eligible`] so genuine I/O failures are surfaced by the
/// caller's own read path rather than masked as `Binary`.
pub fn classify_content(path: &Path) -> PathClass {
    use crate::fs::{check_size, read_to_string_tolerant, ReadOutcome, SizeCheck};

    match check_size(path) {
        SizeCheck::Oversize { .. } => return PathClass::Oversized,
        SizeCheck::Unknown | SizeCheck::WithinLimit { .. } => {}
    }

    match read_to_string_tolerant(path) {
        Ok(ReadOutcome::Ok(_)) => PathClass::Eligible,
        Ok(ReadOutcome::NonUtf8 { .. }) => PathClass::Binary,
        Err(_) => PathClass::Eligible,
    }
}

/// Convenience free function: walk project with all defaults on.
///
/// Equivalent to `ProjectWalker::new(root).iter()`. Use [`ProjectWalker`]
/// directly for finer control (extension filters, max depth, opt-outs).
pub fn walk_project(root: impl AsRef<Path>) -> impl Iterator<Item = DirEntry> {
    ProjectWalker::new(root).iter()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    fn collect_rel_files(root: &Path, walker: impl Iterator<Item = DirEntry>) -> Vec<String> {
        let mut out: Vec<String> = walker
            .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
            .map(|e| {
                e.path()
                    .strip_prefix(root)
                    .unwrap_or(e.path())
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_string()
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn test_skips_node_modules_by_default() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write_file(&root.join("foo.rs"), "fn main() {}");
        write_file(&root.join("node_modules/bad.py"), "import os");

        let files = collect_rel_files(root, walk_project(root));
        assert_eq!(files, vec!["foo.rs".to_string()]);
    }

    #[test]
    fn test_skips_target_dist_build_cache() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write_file(&root.join("src/lib.rs"), "fn main() {}");
        write_file(&root.join("target/debug/x.rs"), "fn x() {}");
        write_file(&root.join("dist/bundle.js"), "// bundled");
        write_file(&root.join("build/out.o"), "binary");
        write_file(&root.join("__pycache__/cached.pyc"), "binary");
        write_file(&root.join(".next/cache.js"), "// cached");
        write_file(&root.join("vendor/dep.go"), "package v");

        let files = collect_rel_files(root, walk_project(root));
        assert_eq!(files, vec!["src/lib.rs".to_string()]);
    }

    #[test]
    fn test_respects_gitignore() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        // Gotcha: ignore crate only activates gitignore under a git repo or
        // if we register a custom ignore. Create a .git dir so it's treated
        // as a repo root.
        fs::create_dir_all(root.join(".git")).unwrap();
        write_file(&root.join(".gitignore"), "secret/\n");
        write_file(&root.join("foo.rs"), "fn main() {}");
        write_file(&root.join("secret/x.rs"), "fn x() {}");

        let files = collect_rel_files(root, walk_project(root));
        assert_eq!(files, vec!["foo.rs".to_string()]);
    }

    #[test]
    fn test_respects_tldrignore() {
        // TLDR-1j2/vti: `.tldrignore` is registered as a custom ignore filename,
        // so — unlike `.gitignore` — it's honored even WITHOUT a git repo. This
        // is the shared corpus walk (`enumerate_corpus_files`), so honoring it
        // here keeps the warm build consistent with the single-file gate.
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write_file(&root.join(".tldrignore"), "vendored/\n*.gen.rs\n");
        write_file(&root.join("foo.rs"), "fn main() {}");
        write_file(&root.join("model.gen.rs"), "fn g() {}");
        write_file(&root.join("vendored/x.rs"), "fn x() {}");

        let files = collect_rel_files(root, walk_project(root));
        assert_eq!(files, vec!["foo.rs".to_string()]);
    }

    #[test]
    fn test_path_matcher_combines_gitignore_and_tldrignore_as_denylists() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write_file(&root.join(".gitignore"), "git-only/\n");
        write_file(&root.join(".tldrignore"), "tldr-only/\n");

        let matcher = build_path_ignore_matcher(root, true).unwrap();
        assert!(matcher.is_ignored(&root.join("git-only/file.cpp"), false));
        assert!(matcher.is_ignored(&root.join("tldr-only/file.cpp"), false));
        assert!(!matcher.is_ignored(&root.join("src/file.cpp"), false));
    }

    #[test]
    fn test_tldrignore_negation_cannot_reinclude_a_denied_path() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write_file(&root.join(".gitignore"), "git-only/\n");
        write_file(
            &root.join(".tldrignore"),
            "tldr-only/\n!tldr-only/keep.cpp\n",
        );

        let matcher = build_path_ignore_matcher(root, true).unwrap();
        assert!(matcher.is_ignored(&root.join("tldr-only/keep.cpp"), false));

        let files = collect_rel_files(root, walk_project(root));
        assert!(
            !files.iter().any(|file| file == "tldr-only/keep.cpp"),
            "tldrignore negation must not re-include a denied file: {files:?}"
        );
    }

    #[test]
    fn test_no_respect_ignore_disables_tldrignore() {
        // Opting out of ignore files (`respect_gitignore(false)`) also opts out
        // of `.tldrignore` — the two share one gate.
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write_file(&root.join(".tldrignore"), "vendored/\n");
        write_file(&root.join("foo.rs"), "fn main() {}");
        write_file(&root.join("vendored/x.rs"), "fn x() {}");

        let mut files: Vec<String> = ProjectWalker::new(root)
            .respect_gitignore(false)
            .iter()
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .filter_map(|e| {
                e.path()
                    .strip_prefix(root)
                    .ok()
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
            })
            .collect();
        files.sort();
        assert!(
            files.iter().any(|f| f == "vendored/x.rs"),
            "with ignore disabled, tldrignored file should appear: {files:?}"
        );
    }

    #[test]
    fn test_hidden_dirs_skipped() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write_file(&root.join("visible.rs"), "fn main() {}");
        write_file(&root.join(".hidden/secret.rs"), "fn secret() {}");

        let files = collect_rel_files(root, walk_project(root));
        assert_eq!(files, vec!["visible.rs".to_string()]);
    }

    #[test]
    fn test_does_not_follow_symlinks_into_loop() {
        // Build root/a.rs plus root/loop -> root to exercise the symlink
        // guard. On systems where symlinks aren't supported the call errors
        // out; just skip those.
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write_file(&root.join("a.rs"), "fn a() {}");

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            // Point a child dir back to root -> would loop if followed.
            let loop_path = root.join("loop");
            symlink(root, &loop_path).unwrap();
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::symlink_dir;
            let loop_path = root.join("loop");
            // May fail without dev-mode; swallow the error so the rest of
            // the test still exercises normal traversal.
            let _ = symlink_dir(root, &loop_path);
        }

        // Traversal must terminate. Collect with a reasonable cap to
        // prevent a runaway test from hanging CI for infinity.
        let files: Vec<_> = walk_project(root).take(10_000).collect();
        // Must find a.rs exactly once; symlink target must not be
        // descended into.
        let count_a = files.iter().filter(|e| e.file_name() == "a.rs").count();
        assert_eq!(count_a, 1, "expected exactly one a.rs, got {}", count_a);
    }

    #[test]
    fn test_no_default_ignore_walks_node_modules() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write_file(&root.join("foo.rs"), "fn main() {}");
        write_file(&root.join("node_modules/bad.py"), "import os");

        let files = collect_rel_files(root, ProjectWalker::new(root).no_default_ignore().iter());
        assert!(
            files.contains(&"foo.rs".to_string()),
            "missing foo.rs: {files:?}"
        );
        assert!(
            files.contains(&"node_modules/bad.py".to_string()),
            "expected node_modules/bad.py to be walked with no_default_ignore: {files:?}"
        );
    }

    #[test]
    fn test_extensions_filter() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write_file(&root.join("a.rs"), "fn a() {}");
        write_file(&root.join("b.py"), "def b(): pass");
        write_file(&root.join("c.ts"), "function c() {}");

        let files = collect_rel_files(root, ProjectWalker::new(root).extensions(&["rs"]).iter());
        assert_eq!(files, vec!["a.rs".to_string()]);
    }

    #[test]
    fn test_max_depth_limits_recursion() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write_file(&root.join("top.rs"), "fn top() {}");
        write_file(&root.join("a/b/deep.rs"), "fn deep() {}");

        // max_depth(1) should include entries exactly one level deep, i.e.
        // files immediately under root (top.rs and the `a` directory) but
        // not a/b/deep.rs.
        let files = collect_rel_files(root, ProjectWalker::new(root).max_depth(1).iter());
        assert!(files.contains(&"top.rs".to_string()), "{files:?}");
        assert!(
            !files.contains(&"a/b/deep.rs".to_string()),
            "max_depth=1 should have excluded deep file: {files:?}"
        );
    }

    // --- PathClass / classify_path / classify_content (TLDR-boa.1) ---------

    fn classify(root: &Path, rel: &str, is_dir: bool) -> PathClass {
        ProjectWalker::new(root).classify_path(&root.join(rel), is_dir)
    }

    #[test]
    fn classify_path_hidden_dotfile() {
        let tmp = tempdir().unwrap();
        write_file(&tmp.path().join(".hidden.rs"), "fn a() {}");
        assert_eq!(classify(tmp.path(), ".hidden.rs", false), PathClass::Hidden);
    }

    #[test]
    fn classify_path_include_hidden_makes_dotfile_eligible() {
        let tmp = tempdir().unwrap();
        write_file(&tmp.path().join(".hidden.rs"), "fn a() {}");
        // Default: hidden.
        assert_eq!(classify(tmp.path(), ".hidden.rs", false), PathClass::Hidden);
        // Opt in: the dotfile is eligible.
        assert_eq!(
            ProjectWalker::new(tmp.path())
                .include_hidden()
                .classify_path(&tmp.path().join(".hidden.rs"), false),
            PathClass::Eligible
        );
    }

    #[test]
    fn classify_path_generated_default_skip_dir() {
        let tmp = tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("node_modules")).unwrap();
        assert_eq!(
            classify(tmp.path(), "node_modules", true),
            PathClass::Generated
        );
    }

    #[test]
    fn classify_path_generated_sentinel_dir() {
        let tmp = tempdir().unwrap();
        // `docs/` is NOT a name-excluded dir, but a doxygen sentinel marks it
        // generated (api-check-and-patterns-accuracy-v1 / P11.BUG-AGG-7).
        let docs = tmp.path().join("docs");
        fs::create_dir_all(&docs).unwrap();
        write_file(&docs.join("doxygen.css"), "/* generated */");
        assert_eq!(classify(tmp.path(), "docs", true), PathClass::Generated);
    }

    #[test]
    fn classify_path_generated_respects_js_ts_hint() {
        let tmp = tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("build")).unwrap();
        // No lang hint: `build/` is a default exclude -> Generated.
        assert_eq!(classify(tmp.path(), "build", true), PathClass::Generated);
        // TypeScript hint: `build/` is preserved -> Eligible.
        assert_eq!(
            ProjectWalker::new(tmp.path())
                .lang_hint(crate::types::Language::TypeScript)
                .classify_path(&tmp.path().join("build"), true),
            PathClass::Eligible
        );
    }

    #[test]
    fn classify_path_unsupported_extension() {
        let tmp = tempdir().unwrap();
        write_file(&tmp.path().join("foo.py"), "x = 1");
        assert_eq!(
            ProjectWalker::new(tmp.path())
                .extensions(&["rs"])
                .classify_path(&tmp.path().join("foo.py"), false),
            PathClass::Unsupported
        );
        // Allowed extension -> Eligible.
        write_file(&tmp.path().join("bar.rs"), "fn b() {}");
        assert_eq!(
            ProjectWalker::new(tmp.path())
                .extensions(&["rs"])
                .classify_path(&tmp.path().join("bar.rs"), false),
            PathClass::Eligible
        );
    }

    #[test]
    fn classify_path_eligible_source() {
        let tmp = tempdir().unwrap();
        write_file(&tmp.path().join("foo.rs"), "fn main() {}");
        assert_eq!(classify(tmp.path(), "foo.rs", false), PathClass::Eligible);
    }

    #[test]
    fn classify_path_ignored_by_tldrignore() {
        let tmp = tempdir().unwrap();
        write_file(&tmp.path().join(".tldrignore"), "*.gen.rs\n");
        write_file(&tmp.path().join("model.gen.rs"), "fn g() {}");
        assert_eq!(
            classify(tmp.path(), "model.gen.rs", false),
            PathClass::Ignored
        );
    }

    #[test]
    fn classify_content_oversized_autogen() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("huge.d.ts");
        let bytes = vec![b'a'; (crate::fs::MAX_AUTOGEN_FILE_SIZE_BYTES as usize) + 1];
        fs::write(&path, &bytes).unwrap();
        assert_eq!(classify_content(&path), PathClass::Oversized);
    }

    #[test]
    fn classify_content_binary() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("blob.bin");
        // 0xFF is never a valid UTF-8 leading byte.
        fs::write(&path, b"valid prefix \xFF\xFE invalid").unwrap();
        assert_eq!(classify_content(&path), PathClass::Binary);
    }

    #[test]
    fn classify_content_eligible() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("ok.rs");
        fs::write(&path, b"fn main() {}\n").unwrap();
        assert_eq!(classify_content(&path), PathClass::Eligible);
    }
}

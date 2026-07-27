//! Common types for TLDR operations
//!
//! This module defines all shared types used across the TLDR codebase.
//! All types derive Serialize/Deserialize with consistent field ordering
//! to address M5 (JSON Serialization Consistency).
//!
//! ## Submodules
//!
//! - `inheritance` - Types for class hierarchy extraction (Phase 7-9, A9)
//! - `patterns` - Types for design pattern mining (Phase 4-6, A10)
//! - `arch_rules` - Types for architecture rules and violations (Phase 3, A11)

// =============================================================================
// Submodules for Architecture Commands (Phase 1: Types Foundation)
// =============================================================================

pub mod arch_rules;
pub mod inheritance;
pub mod patterns;

// Re-export submodule types for convenience
pub use arch_rules::*;
pub use inheritance::*;
pub use patterns::*;

use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// =============================================================================
// Language Support
// =============================================================================

/// Supported programming languages (17 variants as per spec Section 1.2)
///
/// Priority levels:
/// - P0: Python, TypeScript, JavaScript, Go (full support)
/// - P1: Rust, Java (full support)
/// - P2: C, C++, Ruby, Kotlin, Swift, C#, Scala, PHP, Lua, Luau, Elixir (basic support)
#[derive(
    Archive,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    RkyvDeserialize,
    RkyvSerialize,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// Python (.py)
    Python,
    /// TypeScript (.ts, .tsx)
    TypeScript,
    /// JavaScript (.js, .jsx, .mjs, .cjs)
    JavaScript,
    /// Go (.go)
    Go,
    /// Rust (.rs)
    Rust,
    /// Java (.java)
    Java,
    /// C (.c, .h)
    C,
    /// C++ (.cpp, .cc, .cxx, .hpp)
    Cpp,
    /// Ruby (.rb)
    Ruby,
    /// Kotlin (.kt, .kts)
    Kotlin,
    /// Swift (.swift)
    Swift,
    /// C# (.cs)
    CSharp,
    /// Scala (.scala)
    Scala,
    /// PHP (.php)
    Php,
    /// Lua (.lua)
    Lua,
    /// Luau (.luau)
    Luau,
    /// Elixir (.ex, .exs)
    Elixir,
    /// OCaml (.ml, .mli)
    Ocaml,
}

impl Language {
    /// Get file extensions for this language
    pub fn extensions(&self) -> &'static [&'static str] {
        match self {
            Language::Python => &[".py"],
            Language::TypeScript => &[".ts", ".tsx"],
            Language::JavaScript => &[".js", ".jsx", ".mjs", ".cjs"],
            Language::Go => &[".go"],
            Language::Rust => &[".rs"],
            Language::Java => &[".java"],
            Language::C => &[".c", ".h"],
            // kotlin-extract-and-cpp-extensions-v1 (P6.BUG-N2): include
            // the rare-but-valid `.c++`, `.hh`, `.hxx`, `.h++` Cpp
            // spellings so the single-bucket classifier (used by
            // `parse_file_with_lang` autodetection and by per-file
            // `tldr extract`) doesn't reject them as `Unsupported
            // language`. `.h` stays in the C bucket — sibling-aware
            // widening (`from_path_with_siblings`) flips it to Cpp
            // when there is positive evidence in the parent dir.
            Language::Cpp => &[".cpp", ".cc", ".cxx", ".c++", ".hpp", ".hh", ".hxx", ".h++"],
            Language::Ruby => &[".rb"],
            Language::Kotlin => &[".kt", ".kts"],
            Language::Swift => &[".swift"],
            Language::CSharp => &[".cs"],
            Language::Scala => &[".scala"],
            Language::Php => &[".php"],
            Language::Lua => &[".lua"],
            Language::Luau => &[".luau"],
            Language::Elixir => &[".ex", ".exs"],
            Language::Ocaml => &[".ml", ".mli"],
        }
    }

    /// Get file extensions for **directory scanning** when this language is
    /// the requested / autodetected target.
    ///
    /// Distinct from [`Self::extensions`] (which returns the canonical
    /// extensions for **classification**). Two language families need a
    /// broader scan list so directory walks don't silently drop files that
    /// belong to the project but live in a sibling extension:
    ///
    /// - **C++** scans must include `.h` (and the rare `.h++` / `.c++`
    ///   spellings). The header extension is technically ambiguous between
    ///   C and C++, but `tinyxml2.h` next to `tinyxml2.cpp` is
    ///   unambiguously C++. When the user (or autodetect) selects `Cpp`,
    ///   include all C-style header extensions; tree-sitter will parse them
    ///   with the C++ grammar (`Language::Cpp` is passed to the parser),
    ///   which is a strict superset of C declarations.
    ///
    /// - **JavaScript / TypeScript** are sibling families. Real React /
    ///   Node monorepos ship `.tsx`, `.jsx`, `.cjs`, `.mjs` side-by-side;
    ///   when autodetect picks one, the other's extensions must still
    ///   participate. `parse_with_path` already routes `.tsx` / `.jsx`
    ///   through the TSX grammar regardless of the requested language, so
    ///   parsing remains correct.
    ///
    /// All other languages return their canonical [`Self::extensions`]
    /// list — this method is purely a widening for the JS/TS and C++
    /// families.
    ///
    /// # Why a separate method
    ///
    /// `from_extension` / `from_path` keep their single-bucket semantics
    /// (every extension maps to exactly one canonical language) so the
    /// dozens of call sites that use them for classification don't change
    /// shape. Only the directory walker needs the broader list.
    pub fn scan_extensions(&self) -> &'static [&'static str] {
        match self {
            // C++ family: include all C-style header extensions so a
            // `tinyxml2.h` sitting next to `tinyxml2.cpp` is included.
            Language::Cpp => &[
                ".cpp", ".cc", ".cxx", ".c++", ".hpp", ".hh", ".hxx", ".h++", ".h",
            ],
            // C family: same as canonical (`.c` + `.h`).
            // JS/TS sibling family: include each other's extensions so a
            // mixed `.ts/.tsx/.js/.jsx/.mjs/.cjs` directory is fully
            // walked regardless of which sibling autodetect picks.
            Language::JavaScript => &[".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx"],
            Language::TypeScript => &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"],
            // All other languages: canonical extensions.
            _ => self.extensions(),
        }
    }

    /// Detect language from file extension
    pub fn from_extension(ext: &str) -> Option<Self> {
        // Normalize extension to lowercase with leading dot
        let ext = if ext.starts_with('.') {
            ext.to_lowercase()
        } else {
            format!(".{}", ext.to_lowercase())
        };

        match ext.as_str() {
            ".py" => Some(Language::Python),
            ".ts" | ".tsx" => Some(Language::TypeScript),
            ".js" | ".jsx" | ".mjs" | ".cjs" => Some(Language::JavaScript),
            ".go" => Some(Language::Go),
            ".rs" => Some(Language::Rust),
            ".java" => Some(Language::Java),
            ".c" | ".h" => Some(Language::C),
            // kotlin-extract-and-cpp-extensions-v1 (P6.BUG-N2): mirror
            // the canonical `extensions()` widening so per-file
            // classification (used by `parse_file_with_lang` when no
            // hint is supplied, and by `tldr extract` autodetect)
            // accepts the rare Cpp spellings. The walker's
            // `scan_extensions()` already handled them.
            ".cpp" | ".cc" | ".cxx" | ".c++" | ".hpp" | ".hh" | ".hxx" | ".h++" => {
                Some(Language::Cpp)
            }
            ".rb" => Some(Language::Ruby),
            ".kt" | ".kts" => Some(Language::Kotlin),
            ".swift" => Some(Language::Swift),
            ".cs" => Some(Language::CSharp),
            ".scala" => Some(Language::Scala),
            ".php" => Some(Language::Php),
            ".lua" => Some(Language::Lua),
            ".luau" => Some(Language::Luau),
            ".ex" | ".exs" => Some(Language::Elixir),
            ".ml" | ".mli" => Some(Language::Ocaml),
            _ => None,
        }
    }

    /// Detect language from file path
    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        path.extension()
            .and_then(|ext| ext.to_str())
            .and_then(|ext| Self::from_extension(&format!(".{}", ext)))
    }

    /// Detect language from file path with sibling-aware widening for the
    /// C/C++ `.h` header ambiguity (cross-command-consistency-v3 P5.BUG-N1).
    ///
    /// `from_path` uses a single-bucket classifier where `.h` always maps to
    /// `Language::C`. That's correct for headers in pure C projects but wrong
    /// for the (much more common) case of a C++ project keeping its public
    /// headers as `.h` next to `.cpp` translation units (e.g. `tinyxml2.h` /
    /// `tinyxml2.cpp`). Without widening, the C tree-sitter grammar parses
    /// the file and returns `class Foo {…}` declarations as zero-classes
    /// plus a function with `return_type: "class"` — silent garbage.
    ///
    /// This method:
    ///
    /// 1. For `.h` files only, scans the file's parent directory for any C++
    ///    source (`.cpp`/`.cc`/`.cxx`/`.c++`) or richer header extension
    ///    (`.hpp`/`.hh`/`.hxx`/`.h++`). When at least one such sibling
    ///    exists, returns `Language::Cpp`.
    /// 2. For every other extension (including `.h` with no C++ siblings),
    ///    falls back to [`Self::from_path`] verbatim.
    ///
    /// The widening is intentionally narrow: it only flips `.h` → C++ when
    /// the directory provides positive evidence. Mixed projects that keep
    /// pure-C `.h` files in their own directories continue to be treated as
    /// C, preserving backwards behaviour.
    ///
    /// Used by `tldr extract` when no explicit `--lang` is supplied. Other
    /// commands (`structure`, `dead`, etc.) walk directories and rely on
    /// [`Self::matches_for_scan`] / [`Self::scan_extensions`] for the same
    /// widening.
    pub fn from_path_with_siblings(path: &std::path::Path) -> Option<Self> {
        // Only the C/C++ `.h` header is ambiguous. Everything else: defer
        // to the canonical single-bucket classifier.
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        if ext.as_deref() != Some("h") {
            return Self::from_path(path);
        }

        // `.h` next to any C++ sibling → Cpp; otherwise C.
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => return Self::from_path(path),
        };

        // Read up to a bounded number of entries to keep this cheap on
        // pathological directories. The decision only needs *one* positive
        // C++ sibling, so we early-return on the first hit.
        let read_dir = match std::fs::read_dir(parent) {
            Ok(rd) => rd,
            Err(_) => return Self::from_path(path),
        };

        const CPP_SIBLING_EXTS: &[&str] = &["cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx", "h++"];

        for entry in read_dir.flatten() {
            let p = entry.path();
            let Some(sib_ext) = p.extension().and_then(|e| e.to_str()) else {
                continue;
            };
            let sib_ext_lc = sib_ext.to_ascii_lowercase();
            if CPP_SIBLING_EXTS.contains(&sib_ext_lc.as_str()) {
                return Some(Language::Cpp);
            }
        }

        // No C++ sibling found → canonical (C).
        Self::from_path(path)
    }

    /// Returns `true` if `path`'s extension belongs to the broader scan
    /// family of this language.
    ///
    /// This is the predicate version of [`Self::scan_extensions`]. Unlike
    /// [`Self::from_path`] (which always returns the canonical language for
    /// an extension), this method handles the C/C++ header ambiguity and
    /// the JS/TS sibling-family widening:
    ///
    /// - `Language::Cpp.matches_for_scan("foo.h")` → `true` (canonical
    ///   `from_path` would tag it as C).
    /// - `Language::JavaScript.matches_for_scan("foo.tsx")` → `true`
    ///   (canonical `from_path` would tag it as TypeScript).
    /// - `Language::Cpp.matches_for_scan("foo.rs")` → `false`.
    ///
    /// Used by directory-scanning commands (smells, dead, calls,
    /// structure, …) to keep mixed-extension projects working when the
    /// user (or autodetect) picks a single language.
    pub fn matches_for_scan(self, path: &std::path::Path) -> bool {
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            return false;
        };
        let dotted = format!(".{}", ext.to_ascii_lowercase());
        self.scan_extensions()
            .iter()
            .any(|e| e.eq_ignore_ascii_case(&dotted))
    }

    /// Detect dominant language from files in a directory.
    ///
    /// # Detection strategy (autodetect-dominant-language-v1)
    ///
    /// **Strict extension-majority is the primary signal**, with manifest
    /// detection serving only as a tiebreaker for close calls. Earlier
    /// versions ran manifest detection first and unconditionally won, which
    /// produced confidently-wrong results on real repositories:
    ///
    /// - `scala-cats-effect` (457 `.scala` + `package.json` for tooling) →
    ///   used to return `JavaScript`; now returns `Scala`.
    /// - `ocaml-dune` (1794 `.ml` + a stray `doc/requirements.txt`) →
    ///   used to return `Python`; now returns `OCaml`.
    ///
    /// # Algorithm
    ///
    /// 1. Walk the directory via [`crate::walker::walk_project`] (which
    ///    already skips `node_modules`/`target`/`build`/`dist`/`.git` and
    ///    other vendored trees, hidden dirs, and `.gitignored` paths,
    ///    without following symlinks).
    /// 2. Count files per language extension.
    /// 3. Identify the dominant language (maximum count) and the
    ///    second-place language.
    /// 4. **Strict majority:** when the second-place count is below 80% of
    ///    the dominant count, return the dominant language. Manifests are
    ///    ignored: hundreds of `.scala` files beat a tooling
    ///    `package.json`.
    /// 5. **Tiebreaker:** when the second-place count is within 20% of the
    ///    dominant (i.e. ≥ 80% of it), or when the dominant has only a
    ///    handful of files, run manifest detection. If a manifest is
    ///    found, prefer its language **only if** that language has at
    ///    least one source file in the walk; otherwise fall back to the
    ///    dominant extension.
    /// 6. **Empty / unrecognised:** when the walk yields zero recognised
    ///    source files, return `None` regardless of manifests — a project
    ///    with no source code should not be silently labelled.
    ///
    /// # Manifest detection (depth ≤ 2)
    ///
    /// When the tiebreaker triggers, [`detect_from_manifests`] scans the
    /// root, each immediate subdirectory, and each grandchild (covers
    /// `apps/*/` and `packages/*/` monorepo layouts). Among all manifests
    /// found, the one with the highest precedence wins; ties at the same
    /// precedence are broken by shallowest path. Precedence table:
    ///
    /// | Precedence | Manifest(s)                                      | Language                          |
    /// |-----------:|--------------------------------------------------|-----------------------------------|
    /// |          1 | `tsconfig.json`                                  | TypeScript                        |
    /// |          2 | `package.json`                                   | TypeScript (with TS dep) or JS    |
    /// |          3 | `Cargo.toml`                                     | Rust                              |
    /// |          4 | `go.mod`                                         | Go                                |
    /// |        5–7 | `pyproject.toml`, `setup.py`, `requirements.txt` | Python                            |
    /// |          8 | `pom.xml`                                        | Java                              |
    /// |       9–10 | `build.gradle.kts`, `build.gradle`               | Kotlin or Java (tie-break below)  |
    /// |      11–14 | `CMakeLists.txt`, `meson.build`, `configure.ac`/`configure.in`, `Makefile.am`/`Makefile.in` | C or C++ (tie-break below) |
    /// |      15–17 | `*.csproj`, `*.sln`, `global.json` (with `sdk`)  | C#                                |
    /// |      18–19 | `build.sbt`, `project/build.properties`          | Scala                             |
    /// |      20–21 | `dune-project`, `*.opam`                         | OCaml                             |
    /// |         22 | `Gemfile`                                        | Ruby                              |
    /// |         23 | `composer.json`                                  | PHP                               |
    /// |         24 | `mix.exs`                                        | Elixir                            |
    /// |         25 | `Package.swift`                                  | Swift                             |
    /// |      26–27 | `*.rockspec`, `.luarc.json`                      | Lua                               |
    /// |      28–29 | `default.project.json` (Rojo), `.luaurc`         | Luau                              |
    ///
    /// Gradle tie-break: when `build.gradle.kts` is the winning manifest,
    /// count `.kt` vs `.java` files across the walk.
    ///
    /// C/C++ tie-break: when a shared build-system manifest (CMake, Meson,
    /// Autotools) wins, count cpp-family vs c-family files; the
    /// autodetect-correctness-v1 Swift / Rust extension-majority override
    /// inside [`c_vs_cpp_tie_break`] is preserved.
    pub fn from_directory(path: &std::path::Path) -> Option<Self> {
        use std::collections::HashMap;

        // --- Stage 1: walk and tally extension counts ----------------------
        //
        // For language *identification* we additionally filter out files
        // under documentation / example trees: Doxygen's `docs/` ships
        // dozens of `.js` files that would otherwise drown out a small
        // C++ project's actual source. These paths still belong to the
        // project — they are just useless for *deciding what language
        // the project is*. The walker itself does not exclude them
        // (other commands do want to see them).
        const NOISE_DIRS: &[&str] = &["docs", "doc", "documentation", "site-docs"];
        let mut counts: HashMap<Language, usize> = HashMap::new();
        for entry in crate::walker::walk_project(path) {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            // Skip files whose relative path traverses a noise dir.
            if let Ok(rel) = p.strip_prefix(path) {
                if rel.components().any(|c| match c {
                    std::path::Component::Normal(s) => {
                        s.to_str().map(|n| NOISE_DIRS.contains(&n)).unwrap_or(false)
                    }
                    _ => false,
                }) {
                    continue;
                }
            }
            if let Some(lang) = Self::from_path(p) {
                *counts.entry(lang).or_insert(0) += 1;
            }
        }

        // cross-cutting-and-clear-fix-bugs-v1 (P18.X4): when the default
        // walker pass returned no recognised files, retry with the default
        // skip list disabled. This handles JS/TS projects whose only
        // sources live under `src/build/`, `src/dist/`, etc — names that
        // are otherwise pre-emptively skipped before the JS/TS hint can
        // be derived. If retry still yields no files, we honour the
        // original "unrecognised directory" None behaviour.
        if counts.is_empty() {
            for entry in crate::walker::ProjectWalker::new(path)
                .no_default_ignore()
                .iter()
            {
                let p = entry.path();
                if !p.is_file() {
                    continue;
                }
                if let Ok(rel) = p.strip_prefix(path) {
                    if rel.components().any(|c| match c {
                        std::path::Component::Normal(s) => {
                            s.to_str().map(|n| NOISE_DIRS.contains(&n)).unwrap_or(false)
                        }
                        _ => false,
                    }) {
                        continue;
                    }
                }
                if let Some(lang) = Self::from_path(p) {
                    *counts.entry(lang).or_insert(0) += 1;
                }
            }
            if counts.is_empty() {
                return None;
            }
        }

        // --- Stage 2: rank languages by file count -------------------------
        let mut ranked: Vec<(Language, usize)> = counts.iter().map(|(l, c)| (*l, *c)).collect();
        // Sort descending by count, stable on language enum order for
        // deterministic tie-breaks (avoids HashMap-iteration nondeterminism
        // when two langs are exactly tied).
        ranked.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then(format!("{:?}", a.0).cmp(&format!("{:?}", b.0)))
        });
        let (dominant_lang_raw, _dominant_count_raw) = ranked[0];

        // --- Stage 3: C-vs-Cpp disambiguation ------------------------------
        // `.h` is ambiguous between C and C++ but `from_path` always tags
        // it as C. When the dominant pick is C or Cpp, defer to the
        // `c_vs_cpp_tie_break` source-file counter (which ignores `.h`) so
        // a `.cpp + .h`-heavy project isn't misdetected as C. This call
        // also runs the autodetect-correctness-v1 Swift / Rust
        // extension-majority override embedded inside `c_vs_cpp_tie_break`.
        let dominant_lang = if matches!(dominant_lang_raw, Language::C | Language::Cpp) {
            c_vs_cpp_tie_break(path)
        } else {
            dominant_lang_raw
        };

        // --- Stage 4: combine C-family for the dominance comparison --------
        // After disambiguation, treat C and Cpp as the same family for the
        // close-call computation: when 295 .cpp + 261 (.c+.h) collapse to
        // Cpp, the "runner-up" should not be the inflated C count — it's
        // the next genuinely-different language. Otherwise a Cpp project
        // dominated by .h headers would always trigger the close-call
        // tiebreaker and let a stray `tools/fuzz/requirements.txt` flip
        // the answer to Python (the luau-luau bug).
        let consolidated: Vec<(Language, usize)> =
            if matches!(dominant_lang, Language::C | Language::Cpp) {
                let c_total = counts.get(&Language::C).copied().unwrap_or(0)
                    + counts.get(&Language::Cpp).copied().unwrap_or(0);
                let mut v: Vec<(Language, usize)> = counts
                    .iter()
                    .filter(|(l, _)| !matches!(l, Language::C | Language::Cpp))
                    .map(|(l, c)| (*l, *c))
                    .collect();
                v.push((dominant_lang, c_total));
                v.sort_by(|a, b| {
                    b.1.cmp(&a.1)
                        .then(format!("{:?}", a.0).cmp(&format!("{:?}", b.0)))
                });
                v
            } else {
                ranked.clone()
            };
        let dominant_count = consolidated[0].1;
        let runner_up_count = consolidated.get(1).map(|(_, c)| *c).unwrap_or(0);

        // --- Stage 5: strict majority OR manifest tiebreaker ---------------
        // "Within 20%" means runner_up_count >= 0.8 * dominant_count.
        // Use integer math: 5 * runner_up >= 4 * dominant  <==> ratio >= 0.8.
        let close_call = 5 * runner_up_count >= 4 * dominant_count;

        if !close_call {
            // Strict majority — extension count alone decides. Manifests
            // CANNOT override this: a tooling `package.json` next to 457
            // `.scala` files must not flip the answer to JavaScript.
            return Some(dominant_lang);
        }

        // Close call: ask the manifest detector for an opinion. We only
        // honour the manifest's choice when the implied language actually
        // has source files in the walk — otherwise it would be guessing.
        if let Some(manifest_lang) = detect_from_manifests(path) {
            if counts.get(&manifest_lang).copied().unwrap_or(0) > 0 {
                return Some(manifest_lang);
            }
        }
        Some(dominant_lang)
    }

    /// Get the language name as it appears in JSON output
    pub fn as_str(&self) -> &'static str {
        match self {
            Language::Python => "python",
            Language::TypeScript => "typescript",
            Language::JavaScript => "javascript",
            Language::Go => "go",
            Language::Rust => "rust",
            Language::Java => "java",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::Ruby => "ruby",
            Language::Kotlin => "kotlin",
            Language::Swift => "swift",
            Language::CSharp => "csharp",
            Language::Scala => "scala",
            Language::Php => "php",
            Language::Lua => "lua",
            Language::Luau => "luau",
            Language::Elixir => "elixir",
            Language::Ocaml => "ocaml",
        }
    }

    /// Check if this is a P0 (highest priority) language
    pub fn is_p0(&self) -> bool {
        matches!(
            self,
            Language::Python | Language::TypeScript | Language::JavaScript | Language::Go
        )
    }

    /// Check if this is a P1 (high priority) language
    pub fn is_p1(&self) -> bool {
        matches!(self, Language::Rust | Language::Java)
    }

    /// Get all supported languages
    pub fn all() -> &'static [Language] {
        &[
            Language::Python,
            Language::TypeScript,
            Language::JavaScript,
            Language::Go,
            Language::Rust,
            Language::Java,
            Language::C,
            Language::Cpp,
            Language::Ruby,
            Language::Kotlin,
            Language::Swift,
            Language::CSharp,
            Language::Scala,
            Language::Php,
            Language::Lua,
            Language::Luau,
            Language::Elixir,
            Language::Ocaml,
        ]
    }
}

// =============================================================================
// Manifest-based language detection (VAL-002)
//
// `Language::from_directory` uses these helpers as its first stage before
// falling back to extension majority. The goal is to beat false positives on
// pnpm/npm monorepos where `node_modules/.pnpm/**` ships thousands of `.py`
// files from node-gyp and wins a naive extension vote on TypeScript projects.
// =============================================================================

/// Manifest-file precedence when multiple candidates exist at the same depth.
///
/// Ordered from highest to lowest precedence. Earlier entries win ties in
/// `detect_from_manifests`; see `Language::from_directory` docs for the full
/// rationale.
///
/// VAL-008 expanded this list from 14 to 29 entries, adding manifest support
/// for the 7 previously extension-only languages (C, Cpp, CSharp, Scala,
/// OCaml, Lua, Luau). C and C++ share the same manifest families (CMake,
/// Meson, Autotools) and are disambiguated via a source-file-count tie-break
/// in `language_from_manifest_set`.
const MANIFEST_PRECEDENCE: &[ManifestKind] = &[
    // --- TS/JS (highest: most specific signal for the largest lang family)
    ManifestKind::TsConfig,
    ManifestKind::PackageJson,
    // --- Rust, Go
    ManifestKind::CargoToml,
    ManifestKind::GoMod,
    // --- Python
    ManifestKind::PyProject,
    ManifestKind::SetupPy,
    ManifestKind::RequirementsTxt,
    // --- JVM (Java/Kotlin)
    ManifestKind::PomXml,
    ManifestKind::BuildGradleKts,
    ManifestKind::BuildGradle,
    // --- C / C++ build systems (shared manifests, tie-break by file count).
    // Placed high because CMake/Meson/Autotools are unambiguous C-family signals.
    ManifestKind::CmakeLists,
    ManifestKind::MesonBuild,
    ManifestKind::ConfigureAc,
    ManifestKind::MakefileAm,
    // --- CSharp (high-signal, specific)
    ManifestKind::CsProj,
    ManifestKind::SlnFile,
    ManifestKind::GlobalJson,
    // --- Scala
    ManifestKind::BuildSbt,
    ManifestKind::ScalaBuildProperties,
    // --- OCaml
    ManifestKind::DuneProject,
    ManifestKind::OpamFile,
    // --- Ruby, PHP, Elixir, Swift (pre-VAL-008 order, preserved)
    ManifestKind::Gemfile,
    ManifestKind::ComposerJson,
    ManifestKind::MixExs,
    ManifestKind::PackageSwift,
    // --- Lua / Luau (weakest signals — lua projects often lack formal manifests)
    ManifestKind::Rockspec,
    ManifestKind::Luarc,
    ManifestKind::RojoProject,
    ManifestKind::LuauRc,
];

/// How a `ManifestKind` matches a directory entry.
///
/// Most manifests are identified by a fixed filename (`Cargo.toml`, `go.mod`);
/// a few are identified by extension (`*.csproj`, `*.opam`, `*.rockspec`); and
/// one — `ScalaBuildProperties` — is a nested fixed filename.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestMatcher {
    /// Exact filename match at the directory root (e.g. `Cargo.toml`).
    Exact(&'static str),
    /// File extension match, e.g. `"csproj"` matches `MyApp.csproj`.
    /// Matching is case-insensitive.
    Extension(&'static str),
    /// Nested fixed path under the directory (e.g. `project/build.properties`).
    Nested(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestKind {
    // Pre-VAL-008 (14 entries)
    TsConfig,
    PackageJson,
    CargoToml,
    GoMod,
    PyProject,
    SetupPy,
    RequirementsTxt,
    PomXml,
    BuildGradle,
    BuildGradleKts,
    Gemfile,
    ComposerJson,
    MixExs,
    PackageSwift,
    // VAL-008: C / C++ (shared; language chosen by extension tie-break)
    CmakeLists,
    MesonBuild,
    ConfigureAc, // matches both `configure.ac` and `configure.in`
    MakefileAm,  // matches both `Makefile.am` and `Makefile.in`
    // VAL-008: CSharp
    CsProj,     // *.csproj (extension match)
    SlnFile,    // *.sln (extension match)
    GlobalJson, // global.json — only counts when `"sdk"` key is present
    // VAL-008: Scala
    BuildSbt,
    ScalaBuildProperties, // nested: project/build.properties
    // VAL-008: OCaml
    DuneProject,
    OpamFile, // *.opam (extension match)
    // VAL-008: Lua
    Rockspec, // *.rockspec (extension match)
    Luarc,    // .luarc.json
    // VAL-008: Luau
    RojoProject, // default.project.json
    LuauRc,      // .luaurc
}

impl ManifestKind {
    /// The matcher used to locate this manifest in a directory.
    fn matcher(self) -> ManifestMatcher {
        match self {
            // Fixed-filename manifests (pre-VAL-008)
            ManifestKind::TsConfig => ManifestMatcher::Exact("tsconfig.json"),
            ManifestKind::PackageJson => ManifestMatcher::Exact("package.json"),
            ManifestKind::CargoToml => ManifestMatcher::Exact("Cargo.toml"),
            ManifestKind::GoMod => ManifestMatcher::Exact("go.mod"),
            ManifestKind::PyProject => ManifestMatcher::Exact("pyproject.toml"),
            ManifestKind::SetupPy => ManifestMatcher::Exact("setup.py"),
            ManifestKind::RequirementsTxt => ManifestMatcher::Exact("requirements.txt"),
            ManifestKind::PomXml => ManifestMatcher::Exact("pom.xml"),
            ManifestKind::BuildGradle => ManifestMatcher::Exact("build.gradle"),
            ManifestKind::BuildGradleKts => ManifestMatcher::Exact("build.gradle.kts"),
            ManifestKind::Gemfile => ManifestMatcher::Exact("Gemfile"),
            ManifestKind::ComposerJson => ManifestMatcher::Exact("composer.json"),
            ManifestKind::MixExs => ManifestMatcher::Exact("mix.exs"),
            ManifestKind::PackageSwift => ManifestMatcher::Exact("Package.swift"),
            // VAL-008: C / C++ build systems
            ManifestKind::CmakeLists => ManifestMatcher::Exact("CMakeLists.txt"),
            ManifestKind::MesonBuild => ManifestMatcher::Exact("meson.build"),
            // configure.ac / configure.in: handled as a special case in
            // `matches_in` because it's a two-filename disjunction, not a
            // true extension match.
            ManifestKind::ConfigureAc => ManifestMatcher::Exact("configure.ac"),
            ManifestKind::MakefileAm => ManifestMatcher::Exact("Makefile.am"),
            // VAL-008: CSharp
            ManifestKind::CsProj => ManifestMatcher::Extension("csproj"),
            ManifestKind::SlnFile => ManifestMatcher::Extension("sln"),
            ManifestKind::GlobalJson => ManifestMatcher::Exact("global.json"),
            // VAL-008: Scala
            ManifestKind::BuildSbt => ManifestMatcher::Exact("build.sbt"),
            ManifestKind::ScalaBuildProperties => {
                ManifestMatcher::Nested("project/build.properties")
            }
            // VAL-008: OCaml
            ManifestKind::DuneProject => ManifestMatcher::Exact("dune-project"),
            ManifestKind::OpamFile => ManifestMatcher::Extension("opam"),
            // VAL-008: Lua
            ManifestKind::Rockspec => ManifestMatcher::Extension("rockspec"),
            ManifestKind::Luarc => ManifestMatcher::Exact(".luarc.json"),
            // VAL-008: Luau
            ManifestKind::RojoProject => ManifestMatcher::Exact("default.project.json"),
            ManifestKind::LuauRc => ManifestMatcher::Exact(".luaurc"),
        }
    }

    /// True when a directory contains a file matching this manifest kind.
    ///
    /// Handles the three matcher flavours:
    /// - `Exact`: single `path.join(name).is_file()` check.
    /// - `Extension`: scan `read_dir` entries for any file with that extension.
    /// - `Nested`: single `path.join(rel).is_file()` check.
    ///
    /// Also handles the special-case disjunctions:
    /// - `ConfigureAc` matches either `configure.ac` or `configure.in`.
    /// - `MakefileAm` matches either `Makefile.am` or `Makefile.in`.
    /// - `GlobalJson` additionally requires `"sdk"` to appear as a JSON key,
    ///   to avoid false-positives on unrelated `global.json` files shipped
    ///   by tools like `expo-cli` or `firebase-tools`.
    fn matches_in(self, dir: &std::path::Path) -> bool {
        match self {
            ManifestKind::ConfigureAc => {
                dir.join("configure.ac").is_file() || dir.join("configure.in").is_file()
            }
            ManifestKind::MakefileAm => {
                dir.join("Makefile.am").is_file() || dir.join("Makefile.in").is_file()
            }
            ManifestKind::GlobalJson => {
                let p = dir.join("global.json");
                if !p.is_file() {
                    return false;
                }
                // Require an "sdk" key: that's the unambiguous .NET marker.
                // Readers that fail for any reason fall through to false so
                // we don't mis-tag unrelated global.json files as CSharp.
                match std::fs::read_to_string(&p) {
                    Ok(contents) => global_json_has_sdk_key(&contents),
                    Err(_) => false,
                }
            }
            _ => match self.matcher() {
                ManifestMatcher::Exact(name) => dir.join(name).is_file(),
                ManifestMatcher::Nested(rel) => dir.join(rel).is_file(),
                ManifestMatcher::Extension(ext) => dir_has_file_with_extension(dir, ext),
            },
        }
    }
}

/// Scan `dir` (non-recursive) for any file whose extension matches `ext`
/// (case-insensitive, without a leading dot). Used by extension-matching
/// manifests such as `*.csproj`, `*.opam`, `*.rockspec`.
fn dir_has_file_with_extension(dir: &std::path::Path, ext: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let target = ext.to_ascii_lowercase();
    for entry in entries.flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        if let Some(e) = p.extension().and_then(|e| e.to_str()) {
            if e.to_ascii_lowercase() == target {
                return true;
            }
        }
    }
    false
}

/// Check whether a `global.json` contents string has an `"sdk"` key. The
/// test is conservative: the file must parse as a JSON object AND that
/// object must contain `sdk` as a top-level key. This avoids false positives
/// on unrelated `global.json` files shipped by other tooling.
fn global_json_has_sdk_key(contents: &str) -> bool {
    match serde_json::from_str::<serde_json::Value>(contents) {
        Ok(serde_json::Value::Object(map)) => map.contains_key("sdk"),
        _ => false,
    }
}

/// Decide the language for a project whose winning manifest is one of the
/// shared build-system families (CMake, Meson, Autotools, Makefile.am).
///
/// These manifests are not language-specific — they can build C, C++, Swift,
/// Rust, Fortran, etc. CMake in particular is widely used by Swift packages
/// (see e.g. swift-collections/Sources/CMakeLists.txt) and can mislead a
/// pure-manifest detector into reporting C for a Swift codebase.
///
/// Strategy: walk the project counting source files per language family.
/// If a non-C/C++ language family has strictly more source files than the
/// combined C+C++ count, return that language. Otherwise, fall back to the
/// classic C-vs-C++ tie-break:
/// - C++ family: `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx`.
/// - C family:   `.c` (NOT `.h` — ambiguous with C++).
///
/// If the C++ count strictly exceeds the C count, return `Cpp`; otherwise
/// default to `C` (the older, simpler language wins on ties or empty counts).
fn c_vs_cpp_tie_break(root: &std::path::Path) -> Language {
    let mut c_family = 0usize;
    let mut cpp_family = 0usize;
    // Track other languages that commonly use shared build-system manifests
    // (CMake, Meson, Autotools). Swift is the canonical case (Apple ships
    // CMakeLists.txt alongside Package.swift in many official repos).
    let mut swift_count = 0usize;
    let mut rust_count = 0usize;
    for entry in crate::walker::walk_project(root) {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        match p.extension().and_then(|e| e.to_str()) {
            Some("cpp") | Some("cc") | Some("cxx") | Some("hpp") | Some("hh") | Some("hxx") => {
                cpp_family += 1
            }
            Some("c") => c_family += 1,
            Some("swift") => swift_count += 1,
            Some("rs") => rust_count += 1,
            _ => {}
        }
    }
    let c_total = c_family + cpp_family;
    // Extension-majority override: if a non-C/C++ language family strictly
    // dominates, prefer it over the manifest-implied C/C++ default.
    if swift_count > c_total && swift_count >= rust_count {
        return Language::Swift;
    }
    if rust_count > c_total && rust_count > swift_count {
        return Language::Rust;
    }
    if cpp_family > c_family {
        Language::Cpp
    } else {
        Language::C
    }
}

/// Collect the immediate-child directories of `parent`, skipping hidden
/// directories and the well-known vendor list so monorepo sub-manifests
/// buried in `node_modules` / `target` can't mask the real project.
fn collect_child_dirs(parent: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(parent) else {
        return out;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') || crate::walker::DEFAULT_EXCLUDE_DIRS.contains(&name) {
                continue;
            }
        }
        out.push(p);
    }
    out
}

/// Look for a project manifest at `root` and at immediate subdirectories
/// (depth <= 2 to cover pnpm/Yarn/Turbo monorepos that keep manifests in
/// `packages/*/` and `apps/*/`).
///
/// Returns `None` when no manifest is found; callers fall back to
/// extension-majority detection. Precedence works as follows (VAL-002):
///
/// 1. Collect every manifest at every scanned depth.
/// 2. Pick the one with the highest slot in [`MANIFEST_PRECEDENCE`]
///    (TsConfig > PackageJson > CargoToml > ...). This means a
///    `tsconfig.json` nested in `packages/ui/` beats a bare
///    `package.json` at the root — the correct outcome for monorepos
///    where the root package.json holds only tooling (prettier, turbo,
///    eslint) and the language lives in subpackages.
/// 3. If multiple manifests share the top precedence, pick the shallowest
///    path. (Purely cosmetic — the same manifest at any depth resolves
///    to the same language.)
fn detect_from_manifests(root: &std::path::Path) -> Option<Language> {
    // Collect candidate directories at depth 0 (root), depth 1 (immediate
    // subdirs), and depth 2 (grandchildren — needed for `packages/*/`).
    let mut dirs: Vec<(usize, std::path::PathBuf)> = Vec::new();
    dirs.push((0, root.to_path_buf()));

    let depth1 = collect_child_dirs(root);
    for d1 in &depth1 {
        dirs.push((1, d1.clone()));
    }
    for d1 in &depth1 {
        for d2 in collect_child_dirs(d1) {
            dirs.push((2, d2));
        }
    }

    // Collect every (precedence_index, depth, dir, manifest) tuple, then
    // pick the entry with the smallest precedence_index, breaking ties by
    // shallowest depth.
    let mut best: Option<(usize, usize, std::path::PathBuf, ManifestKind)> = None;
    for (depth, dir) in &dirs {
        for (idx, m) in MANIFEST_PRECEDENCE.iter().copied().enumerate() {
            if m.matches_in(dir) {
                let candidate = (idx, *depth, dir.clone(), m);
                best = match best {
                    None => Some(candidate),
                    Some(ref existing) => {
                        // Pick the lower precedence index, then shallower depth.
                        if candidate.0 < existing.0
                            || (candidate.0 == existing.0 && candidate.1 < existing.1)
                        {
                            Some(candidate)
                        } else {
                            Some(existing.clone())
                        }
                    }
                };
            }
        }
    }

    best.and_then(|(_, _, dir, m)| language_from_manifest_set(&dir, &[m], root))
}

/// Convert a sorted-by-precedence set of manifests (all at the same depth)
/// into a [`Language`], applying the per-manifest heuristics.
///
/// The `project_root` is the original path passed to `from_directory` and is
/// used to count `.kt` vs `.java` files when resolving Gradle ambiguity.
///
/// `present` is assumed non-empty; the first manifest wins since it has
/// highest precedence per [`MANIFEST_PRECEDENCE`].
fn language_from_manifest_set(
    dir: &std::path::Path,
    present: &[ManifestKind],
    project_root: &std::path::Path,
) -> Option<Language> {
    let m = *present.first()?;
    let lang = match m {
        ManifestKind::TsConfig => Language::TypeScript,
        ManifestKind::PackageJson => {
            // TypeScript when a typescript dep is declared, else JavaScript.
            // If we can't read the file, assume JavaScript.
            let p = dir.join("package.json");
            match std::fs::read_to_string(&p) {
                Ok(contents) if package_json_has_typescript_dep(&contents) => Language::TypeScript,
                _ => Language::JavaScript,
            }
        }
        ManifestKind::CargoToml => Language::Rust,
        ManifestKind::GoMod => Language::Go,
        ManifestKind::PyProject | ManifestKind::SetupPy | ManifestKind::RequirementsTxt => {
            Language::Python
        }
        ManifestKind::PomXml => Language::Java,
        ManifestKind::BuildGradleKts => {
            // Kotlin DSL Gradle file; could be either Kotlin or Java.
            // Tie-break by counting .kt vs .java across the project.
            gradle_kotlin_vs_java(project_root)
        }
        ManifestKind::BuildGradle => Language::Java,
        ManifestKind::Gemfile => Language::Ruby,
        ManifestKind::ComposerJson => Language::Php,
        ManifestKind::MixExs => Language::Elixir,
        ManifestKind::PackageSwift => Language::Swift,
        // VAL-008: C / C++ shared build-system manifests. Dispatch via
        // file-count tie-break (`.cpp`/`.cc`/`.cxx`/`.hpp`/`.hh`/`.hxx` vs
        // `.c`). On ties or empty counts we fall back to C.
        ManifestKind::CmakeLists
        | ManifestKind::MesonBuild
        | ManifestKind::ConfigureAc
        | ManifestKind::MakefileAm => c_vs_cpp_tie_break(project_root),
        // VAL-008: CSharp
        ManifestKind::CsProj | ManifestKind::SlnFile | ManifestKind::GlobalJson => Language::CSharp,
        // VAL-008: Scala
        ManifestKind::BuildSbt | ManifestKind::ScalaBuildProperties => Language::Scala,
        // VAL-008: OCaml
        ManifestKind::DuneProject | ManifestKind::OpamFile => Language::Ocaml,
        // VAL-008: Lua
        ManifestKind::Rockspec | ManifestKind::Luarc => Language::Lua,
        // VAL-008: Luau
        ManifestKind::RojoProject | ManifestKind::LuauRc => Language::Luau,
    };
    Some(lang)
}

/// Lightweight check for "typescript" as a dep / devDep / peerDep in a
/// `package.json`. We avoid pulling in a JSON parser for this — a substring
/// check inside the dependency-section braces is enough to separate the
/// common TS project case from a pure-JS project.
fn package_json_has_typescript_dep(contents: &str) -> bool {
    // Fast reject: the word "typescript" must appear somewhere.
    if !contents.contains("typescript") {
        return false;
    }
    // Simple heuristic: look for `"typescript"` as a JSON key (followed by
    // a colon with optional whitespace). Matches:
    //   "typescript": "5.0.0"
    //   "typescript" : "^5"
    // Won't match a dep whose *value* contains the word (e.g. a package
    // called "my-typescript-helper" would only trip the fast-reject, not
    // this check).
    let mut rest = contents;
    while let Some(idx) = rest.find("\"typescript\"") {
        let after = &rest[idx + "\"typescript\"".len()..];
        let trimmed = after.trim_start();
        if trimmed.starts_with(':') {
            return true;
        }
        rest = after;
    }
    false
}

/// Resolve Gradle ambiguity: return Kotlin when `.kt` file count across the
/// project walk exceeds `.java`, otherwise Java.
fn gradle_kotlin_vs_java(root: &std::path::Path) -> Language {
    let mut kt = 0usize;
    let mut java = 0usize;
    for entry in crate::walker::walk_project(root) {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        match p.extension().and_then(|e| e.to_str()) {
            Some("kt") | Some("kts") => kt += 1,
            Some("java") => java += 1,
            _ => {}
        }
    }
    if kt > java {
        Language::Kotlin
    } else {
        Language::Java
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for Language {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "python" | "py" => Ok(Language::Python),
            "typescript" | "ts" => Ok(Language::TypeScript),
            "javascript" | "js" => Ok(Language::JavaScript),
            "go" | "golang" => Ok(Language::Go),
            "rust" | "rs" => Ok(Language::Rust),
            "java" => Ok(Language::Java),
            "c" => Ok(Language::C),
            "cpp" | "c++" | "cxx" => Ok(Language::Cpp),
            "ruby" | "rb" => Ok(Language::Ruby),
            "kotlin" | "kt" => Ok(Language::Kotlin),
            "swift" => Ok(Language::Swift),
            "csharp" | "c#" | "cs" => Ok(Language::CSharp),
            "scala" => Ok(Language::Scala),
            "php" => Ok(Language::Php),
            "lua" => Ok(Language::Lua),
            "luau" => Ok(Language::Luau),
            "elixir" | "ex" => Ok(Language::Elixir),
            "ocaml" | "ml" => Ok(Language::Ocaml),
            _ => Err(format!("Unknown language: {}", s)),
        }
    }
}

// =============================================================================
// File System Types
// =============================================================================

/// File tree node type
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NodeType {
    /// Directory node
    Dir,
    /// File node
    File,
}

/// File tree structure (spec Section 2.1.1)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTree {
    /// Display name of the file or directory
    pub name: String,
    /// Whether this node is a file or directory
    #[serde(rename = "type")]
    pub node_type: NodeType,
    /// Absolute path to the file (None for directory nodes)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Child nodes (only populated for directory nodes)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<FileTree>,
}

impl FileTree {
    /// Create a new file node
    pub fn file(name: impl Into<String>, path: PathBuf) -> Self {
        Self {
            name: name.into(),
            node_type: NodeType::File,
            path: Some(path),
            children: Vec::new(),
        }
    }

    /// Create a new directory node
    pub fn dir(name: impl Into<String>, children: Vec<FileTree>) -> Self {
        Self {
            name: name.into(),
            node_type: NodeType::Dir,
            path: None,
            children,
        }
    }
}

/// File entry for flat file lists
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// Path to the file
    pub path: PathBuf,
    /// Detected programming language, if any
    pub language: Option<Language>,
    /// File size in bytes
    pub size_bytes: u64,
}

// IgnoreSpec (gitignore-style caller patterns) was removed in TLDR-boa.4. The
// canonical `crate::walker::ProjectWalker` honors `.gitignore`/`.tldrignore`
// directly; no production caller ever passed a populated spec (all used
// `IgnoreSpec::default()`/`None`), so the parameter was dead weight.

// =============================================================================
// AST Types (Layer 1)
// =============================================================================

/// Code structure for a project (spec Section 2.1.2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeStructure {
    /// Root directory of the analyzed project
    pub root: PathBuf,
    /// Primary programming language of the project.
    ///
    /// med-low-schema-cleanup-v1 (N7): emitted as `null` when the
    /// directory contains zero source files (instead of silently
    /// defaulting to `"python"`). When `language` is `None` the
    /// `warnings` field carries a `"No source files found in
    /// directory"` entry, mirroring the M-X5/M-Y2/M-Z8 warnings
    /// pattern.
    #[serde(default)]
    pub language: Option<Language>,
    /// Structural information for each source file
    pub files: Vec<FileStructure>,
    /// Files that were skipped during the scan (with reason).
    ///
    /// typescript-large-file-perf-v1: populated when a file exceeded
    /// the size policy (`crate::fs::oversize`). Omitted from the
    /// JSON output when zero, so existing snapshot consumers stay
    /// unchanged on clean inputs.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub files_skipped: u32,
    /// Per-file skip warnings (one entry per skipped file).
    ///
    /// Format: `Skipped <path>: <size>MB exceeds <cap>MB cap for
    /// <category>`. Omitted from the JSON output when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Definition-level information with line ranges and signatures.
/// Extracted from tree-sitter AST, suitable for caching.
#[derive(
    Archive, Debug, Clone, PartialEq, RkyvDeserialize, RkyvSerialize, Serialize, Deserialize,
)]
pub struct DefinitionInfo {
    /// Symbol name
    pub name: String,
    /// Kind: "function", "method", "class", "struct"
    pub kind: String,
    /// Start line (1-indexed)
    pub line_start: u32,
    /// End line (1-indexed, inclusive)
    pub line_end: u32,
    /// Signature line (e.g., "pub fn foo(x: i32) -> bool")
    pub signature: String,
}

/// Structure of a single file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileStructure {
    /// Path to the source file
    pub path: PathBuf,
    /// Number of tree-sitter `ERROR` or missing recovery nodes.
    #[serde(default)]
    pub parse_errors: usize,
    /// Names of top-level functions defined in this file.
    ///
    /// schema-cleanup-v1 BUG-13: kept on the in-memory struct for
    /// internal consumers but `#[serde(skip_serializing)]` so JSON
    /// output never carries this redundant string list. New consumers
    /// should read `definitions[]` (which carries name + line ranges +
    /// signatures + kind).
    #[serde(skip_serializing)]
    #[serde(default)]
    pub functions: Vec<String>,
    /// Names of classes or structs defined in this file
    pub classes: Vec<String>,
    /// Names of methods (functions inside classes) in this file.
    ///
    /// schema-unification-v1 BUG-21: this flat string list collapses
    /// overloads with the same name (e.g. three `getPet(...)` overloads in
    /// Java). Kept for backward compatibility; new code should consume
    /// `method_infos` (or `definitions`, which already carries line ranges
    /// + signatures).
    ///
    /// schema-cleanup-v1 BUG-13: now `#[serde(skip_serializing)]` —
    /// JSON output emits `method_infos` (objects) and `definitions`
    /// instead. Internal consumers may still build/read this field.
    #[serde(skip_serializing)]
    #[serde(default)]
    pub methods: Vec<String>,
    /// Detailed method information that distinguishes overloads by line
    /// number and signature. Each element carries `(name, signature, line)`
    /// so consumers can disambiguate same-name methods (e.g. three
    /// `getPet(...)` overloads in Java/Kotlin/Scala/C++).
    ///
    /// structure-method-infos-all-langs-v1: ALWAYS emitted in JSON output
    /// (as `[]` for languages whose file contains no methods, e.g. C / OCaml
    /// modules / Lua / shell scripts) so consumers can rely on the field
    /// being present across all 17 supported languages. Without this, code
    /// that does `files[0].method_infos` would error on languages where
    /// the file has no class scope.
    ///
    /// schema-unification-v1 BUG-21: ADDITIVE companion to `methods`.
    #[serde(default)]
    pub method_infos: Vec<MethodInfo>,
    /// Import statements found in this file
    pub imports: Vec<ImportInfo>,
    /// Detailed definition information with line ranges and signatures
    ///
    /// (path-and-schema-cleanup-v3 P3.BUG-N4) Always emitted, even if
    /// empty. Previously elided via `skip_serializing_if = "Vec::is_empty"`,
    /// which forced consumers to handle the absent-key case. Schema
    /// consumers expect a stable shape — `definitions: []` is the
    /// canonical empty value.
    #[serde(default)]
    pub definitions: Vec<DefinitionInfo>,
}

/// Method information that preserves overload distinguishability.
///
/// schema-unification-v1 BUG-21: parallels each entry of
/// [`FileStructure::methods`] with line + signature so consumers can
/// distinguish same-name overloads.
#[derive(
    Archive, Debug, Clone, RkyvDeserialize, RkyvSerialize, Serialize, Deserialize, PartialEq, Eq,
)]
pub struct MethodInfo {
    /// Method name (matches the corresponding `methods[i]` entry).
    pub name: String,
    /// Signature line (e.g., `public Pet getPet(Integer id, boolean ignoreNew)`),
    /// or empty string if not extractable.
    #[serde(default)]
    pub signature: String,
    /// 1-indexed line number of the method definition.
    pub line: u32,
    /// 1-indexed end line (inclusive) of the method body.
    ///
    /// schema-cleanup-v1 BUG-13: added to align with
    /// `DefinitionInfo.line_end`, so consumers can compute method
    /// length without additional AST queries. Defaults to `0` for
    /// legacy data; new entries are populated from `DefinitionInfo`.
    #[serde(default)]
    pub line_end: u32,
}

/// Import statement information (spec Section 2.1.4)
#[derive(Archive, Debug, Clone, RkyvDeserialize, RkyvSerialize, Serialize, Deserialize)]
pub struct ImportInfo {
    /// Module or package being imported
    pub module: String,
    /// Specific names imported from the module (e.g., `from X import a, b`)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
    /// Whether this is a `from` import (e.g., `from module import name`)
    #[serde(default)]
    pub is_from: bool,
    /// Import alias (e.g., `import X as Y`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

/// Complete module information (spec Section 2.1.3)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleInfo {
    /// Path to the source file for this module
    pub file_path: PathBuf,
    /// Programming language of the module
    pub language: Language,
    /// Module-level docstring, if present
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    /// Import statements in this module
    pub imports: Vec<ImportInfo>,
    /// Top-level functions defined in this module
    pub functions: Vec<FunctionInfo>,
    /// Classes or structs defined in this module
    pub classes: Vec<ClassInfo>,
    /// Module-level constants (Gap 3)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constants: Vec<FieldInfo>,
    /// Intra-file call graph showing function call relationships within this module
    pub call_graph: IntraFileCallGraph,
}

/// Function information with full details
///
/// schema-unification-v1 BUG-17: serializes both `line_number` (legacy
/// canonical name) and `line` (additive alias matching `vuln`/`dead`/etc.)
/// so consumers can use a single field name across all commands. The
/// `Deserialize` impl accepts either name (`#[serde(alias = "line")]`).
///
/// schema-cleanup-v1 BUG-23: the `line_number` field is no longer
/// emitted in JSON (the duplicate of `line` was dead surface) and a
/// new `line_end` field is emitted, mirroring
/// `DefinitionInfo.line_end` and `MethodInfo.line_end`. The struct
/// field is still named `line_number` for source compatibility with
/// the many call-sites that build it, but JSON output is `line` +
/// `line_end`.
#[derive(Archive, Debug, Clone, RkyvDeserialize, RkyvSerialize, Deserialize)]
pub struct FunctionInfo {
    /// Name of the function
    pub name: String,
    /// Parameter names (and optional type annotations)
    pub params: Vec<String>,
    /// Return type annotation, if present
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_type: Option<String>,
    /// Docstring or doc comment for this function
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    /// Whether this function is a method (defined inside a class/struct)
    #[serde(default)]
    pub is_method: bool,
    /// Whether this function is declared as async
    #[serde(default)]
    pub is_async: bool,
    /// Decorator or annotation names applied to this function
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decorators: Vec<String>,
    /// Line number where this function is defined (1-indexed).
    ///
    /// schema-cleanup-v1 BUG-23: deserialized from `line`
    /// (canonical) or `line_number` (legacy alias).
    #[serde(alias = "line")]
    pub line_number: u32,
    /// 1-indexed end line of the function body (inclusive).
    ///
    /// schema-cleanup-v1 BUG-23: 0 if not extracted (legacy
    /// constructions). Populated by the canonical extractors that
    /// have access to the AST node range.
    #[serde(default)]
    pub line_end: u32,
}

// schema-unification-v1 BUG-17 / schema-cleanup-v1 BUG-23: manual
// Serialize impl emits `line` and `line_end` (the canonical schema)
// and intentionally OMITS `line_number` (which was a redundant alias
// of `line` from BUG-17 — keeping both was dead surface).
impl Serialize for FunctionInfo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        // Field count: name, params, is_method, is_async, line, line_end
        // + optional fields.
        let mut count = 6;
        if self.return_type.is_some() {
            count += 1;
        }
        if self.docstring.is_some() {
            count += 1;
        }
        if !self.decorators.is_empty() {
            count += 1;
        }
        let mut s = serializer.serialize_struct("FunctionInfo", count)?;
        s.serialize_field("name", &self.name)?;
        s.serialize_field("params", &self.params)?;
        if let Some(rt) = &self.return_type {
            s.serialize_field("return_type", rt)?;
        }
        if let Some(ds) = &self.docstring {
            s.serialize_field("docstring", ds)?;
        }
        s.serialize_field("is_method", &self.is_method)?;
        s.serialize_field("is_async", &self.is_async)?;
        if !self.decorators.is_empty() {
            s.serialize_field("decorators", &self.decorators)?;
        }
        s.serialize_field("line", &self.line_number)?;
        s.serialize_field("line_end", &self.line_end)?;
        s.end()
    }
}

/// Class information with full details
///
/// schema-unification-v1 BUG-17: emits both `line_number` and `line` —
/// see `FunctionInfo` doc for rationale.
#[derive(Archive, Debug, Clone, RkyvDeserialize, RkyvSerialize, Deserialize)]
pub struct ClassInfo {
    /// Name of the class or struct
    pub name: String,
    /// Base classes or parent types this class extends
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bases: Vec<String>,
    /// Docstring or doc comment for this class
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    /// Methods defined in this class
    pub methods: Vec<FunctionInfo>,
    /// Fields/properties of the class (Gap 3)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldInfo>,
    /// Decorator or annotation names applied to this class
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decorators: Vec<String>,
    /// Line number where this class is defined (1-indexed).
    ///
    /// schema-cleanup-v1 BUG-23: deserialized from `line`
    /// (canonical) or `line_number` (legacy alias).
    #[serde(alias = "line")]
    pub line_number: u32,
    /// 1-indexed end line of the class body (inclusive).
    ///
    /// schema-cleanup-v1 BUG-23: 0 if not extracted (legacy
    /// constructions). Populated by the canonical extractors that
    /// have access to the AST node range.
    #[serde(default)]
    pub line_end: u32,
}

// schema-unification-v1 BUG-17 / schema-cleanup-v1 BUG-23: emits
// `line` + `line_end`; intentionally OMITS `line_number` (which was a
// redundant alias for `line`).
impl Serialize for ClassInfo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut count = 4; // name + methods + line + line_end
        if !self.bases.is_empty() {
            count += 1;
        }
        if self.docstring.is_some() {
            count += 1;
        }
        if !self.fields.is_empty() {
            count += 1;
        }
        if !self.decorators.is_empty() {
            count += 1;
        }
        let mut s = serializer.serialize_struct("ClassInfo", count)?;
        s.serialize_field("name", &self.name)?;
        if !self.bases.is_empty() {
            s.serialize_field("bases", &self.bases)?;
        }
        if let Some(ds) = &self.docstring {
            s.serialize_field("docstring", ds)?;
        }
        s.serialize_field("methods", &self.methods)?;
        if !self.fields.is_empty() {
            s.serialize_field("fields", &self.fields)?;
        }
        if !self.decorators.is_empty() {
            s.serialize_field("decorators", &self.decorators)?;
        }
        s.serialize_field("line", &self.line_number)?;
        s.serialize_field("line_end", &self.line_end)?;
        s.end()
    }
}

/// Field or constant information (Gap 3)
///
/// Represents:
/// - Class/struct fields (instance variables, properties)
/// - Module-level constants
/// - Static class variables
///
/// schema-unification-v1 BUG-17: emits both `line_number` and `line`.
#[derive(Archive, Debug, Clone, RkyvDeserialize, RkyvSerialize, Deserialize)]
pub struct FieldInfo {
    /// Field name
    pub name: String,
    /// Field type annotation (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field_type: Option<String>,
    /// Default value (if present)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,
    /// Whether this is a static/class variable
    #[serde(default)]
    pub is_static: bool,
    /// Whether this is a constant (immutable, UPPER_CASE by convention)
    #[serde(default)]
    pub is_constant: bool,
    /// Visibility modifier
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    /// Line number where field is defined (1-indexed).
    ///
    /// schema-cleanup-v1 BUG-23: deserialized from `line`
    /// (canonical) or `line_number` (legacy alias).
    #[serde(alias = "line")]
    pub line_number: u32,
    /// 1-indexed end line of the field declaration. For most languages
    /// this is the same as `line_number` (single-line declarations);
    /// surfaces multi-line forms (e.g. lambda-bodied class fields).
    ///
    /// schema-cleanup-v1 BUG-23: added for parity with FunctionInfo /
    /// ClassInfo / MethodInfo / DefinitionInfo.
    #[serde(default)]
    pub line_end: u32,
}

// schema-unification-v1 BUG-17 / schema-cleanup-v1 BUG-23: emits
// `line` + `line_end`; intentionally OMITS `line_number` (which was a
// redundant alias of `line` from BUG-17 — kept on the in-memory
// struct for source compatibility but no longer serialized).
impl Serialize for FieldInfo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut count = 5; // name, is_static, is_constant, line, line_end
        if self.field_type.is_some() {
            count += 1;
        }
        if self.default_value.is_some() {
            count += 1;
        }
        if self.visibility.is_some() {
            count += 1;
        }
        let mut s = serializer.serialize_struct("FieldInfo", count)?;
        s.serialize_field("name", &self.name)?;
        if let Some(ft) = &self.field_type {
            s.serialize_field("field_type", ft)?;
        }
        if let Some(dv) = &self.default_value {
            s.serialize_field("default_value", dv)?;
        }
        s.serialize_field("is_static", &self.is_static)?;
        s.serialize_field("is_constant", &self.is_constant)?;
        if let Some(vis) = &self.visibility {
            s.serialize_field("visibility", vis)?;
        }
        s.serialize_field("line", &self.line_number)?;
        s.serialize_field("line_end", &self.line_end)?;
        s.end()
    }
}

/// Intra-file call graph
#[derive(
    Archive, Debug, Clone, RkyvDeserialize, RkyvSerialize, Serialize, Deserialize, Default,
)]
pub struct IntraFileCallGraph {
    /// Map from function name to the list of functions it calls.
    ///
    /// TLDR-7pp.1.5: `BTreeMap` (not `HashMap`) so serialization is
    /// deterministic across processes. The daemon and a cold `--oneshot` run
    /// are separate processes with different `HashMap` hash seeds; a `HashMap`
    /// here serialized its keys in per-process-random order, making `tldr
    /// extract` output non-byte-identical between the two paths (a parity
    /// break the daemon-vs-oneshot differential exposed once `extract` began
    /// honestly routing through the daemon).
    pub calls: std::collections::BTreeMap<String, Vec<String>>,
    /// Reverse map from function name to the list of functions that call it.
    /// `BTreeMap` for the same determinism reason as `calls`.
    pub called_by: std::collections::BTreeMap<String, Vec<String>>,
}

// =============================================================================
// Call Graph Types (Layer 2)
// =============================================================================

/// Helper for serde skip_serializing_if on u32 fields.
fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

/// Reference to a function in the codebase, used in call graphs and dead code analysis.
///
/// Equality and hashing are based only on `file` and `name`, so metadata
/// fields do not affect `HashSet`/`HashMap` lookups.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionRef {
    /// Path to the file containing this function
    pub file: PathBuf,
    /// Name of the function
    pub name: String,
    /// Line number where the function starts (1-based, 0 = unknown)
    #[serde(default)]
    pub line: u32,
    /// Function signature (e.g. "def my_func(x, y) -> int")
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub signature: String,
    /// Reference count: how many times this identifier appears across the codebase.
    /// 1 = only the definition, 0 = unknown/not computed.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub ref_count: u32,
    /// Whether this function is public/exported (pub, export, uppercase Go, etc.)
    #[serde(default)]
    pub is_public: bool,
    /// Whether this function is a test function (in test file or test function)
    #[serde(default)]
    pub is_test: bool,
    /// Whether this function is inside a trait/interface/protocol/abstract class
    #[serde(default)]
    pub is_trait_method: bool,
    /// Whether this function has any decorator/annotation
    #[serde(default)]
    pub has_decorator: bool,
    /// Names of decorators/annotations on this function
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decorator_names: Vec<String>,
}

// Equality based on file + name only (metadata is for analysis, not identity)
impl PartialEq for FunctionRef {
    fn eq(&self, other: &Self) -> bool {
        self.file == other.file && self.name == other.name
    }
}

impl Eq for FunctionRef {}

// Hash based on file + name only (must match PartialEq)
impl std::hash::Hash for FunctionRef {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.file.hash(state);
        self.name.hash(state);
    }
}

impl FunctionRef {
    /// Create a new function reference with default (unenriched) metadata.
    ///
    /// All metadata fields default to false/empty, meaning the function
    /// is treated as private with no special attributes. This preserves
    /// backward compatibility with existing call sites.
    pub fn new(file: PathBuf, name: impl Into<String>) -> Self {
        Self {
            file,
            name: name.into(),
            line: 0,
            signature: String::new(),
            ref_count: 0,
            is_public: false,
            is_test: false,
            is_trait_method: false,
            has_decorator: false,
            decorator_names: Vec::new(),
        }
    }
}

impl std::fmt::Display for FunctionRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.file.display(), self.name)
    }
}

/// Workspace configuration for multi-root projects
#[derive(Debug, Clone, Default)]
pub struct WorkspaceConfig {
    /// Root directories of the workspace
    pub roots: Vec<PathBuf>,
}

/// Upper bound on workspace members to guard against pathological configs
/// (VAL-007). Matches the cost budget documented on `WorkspaceConfig::discover`.
const MAX_WORKSPACE_MEMBERS: usize = 256;

/// Directories we refuse to expand globs into during workspace discovery
/// (VAL-007). Mirrors [`crate::walker::DEFAULT_EXCLUDE_DIRS`] but is kept
/// duplicated here to avoid a circular module reference (`walker` depends
/// on nothing from `types`, and we want to keep it that way).
const WORKSPACE_EXPANSION_EXCLUDED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".nuxt",
    "__pycache__",
    "vendor",
    ".git",
    ".venv",
    "venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
];

impl WorkspaceConfig {
    /// Discover workspace roots from common manifest files at or near `root`.
    ///
    /// Returns `Some(WorkspaceConfig { roots: [...] })` when a known workspace
    /// manifest is found and enumerates at least one member; `None` otherwise
    /// (so callers can preserve existing single-root behavior).
    ///
    /// Probed markers, in order:
    /// - `pnpm-workspace.yaml` at `root` (parses `packages:` glob list)
    /// - `package.json` at `root` with `"workspaces": [...]` (npm/yarn/pnpm)
    /// - `Cargo.toml` at `root` with `[workspace] members = [...]`
    /// - `go.work` at `root` with `use <path>` directives
    ///
    /// All returned roots are absolute paths (canonicalized when possible,
    /// falling back to `root.join(member)` when canonicalization fails).
    /// The returned list always contains `root` itself as the first entry,
    /// followed by each discovered member directory.
    ///
    /// To prevent pathological configurations, the returned list is capped
    /// at [`MAX_WORKSPACE_MEMBERS`] entries (including the root).
    pub fn discover(root: &Path) -> Option<Self> {
        // Deliberately do NOT canonicalize the root here. Downstream code
        // (e.g. `callgraph::scanner::resolve_scan_roots`) verifies that
        // every workspace root `starts_with(root)` using the path shape
        // the caller provided — canonicalizing to `/private/var/...` on
        // macOS when the caller passed `/var/...` would break that check.
        // The paths we return are always `root.join(member)` in the same
        // shape as the caller's root.
        let probe_root = root;

        // Probe markers in priority order.
        let members = probe_pnpm_workspace(probe_root)
            .or_else(|| probe_package_json_workspaces(probe_root))
            .or_else(|| probe_cargo_workspace(probe_root))
            .or_else(|| probe_go_work(probe_root))?;

        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut roots = Vec::with_capacity(members.len() + 1);

        // Always include the root itself so siblings can be scanned together.
        let root_key = root.to_path_buf();
        seen.insert(root_key.clone());
        roots.push(root_key);

        let cap = MAX_WORKSPACE_MEMBERS;
        let mut truncated = false;
        for member in members {
            if roots.len() >= cap {
                truncated = true;
                break;
            }
            if !member.exists() || !member.is_dir() {
                continue;
            }
            // Keep paths in the same shape as the caller's root so
            // downstream starts_with() checks pass (no canonicalization).
            if seen.insert(member.clone()) {
                roots.push(member);
            }
        }

        if truncated {
            eprintln!(
                "[tldr] WorkspaceConfig::discover: workspace member count exceeded {} — truncating; some roots not scanned.",
                cap
            );
        }

        // If only the root itself made it into the list (no real members
        // found), tell the caller there's no workspace.
        if roots.len() <= 1 {
            return None;
        }

        Some(Self { roots })
    }
}

// =============================================================================
// Workspace discovery probes (VAL-007)
// =============================================================================

/// Probe for a pnpm workspace at `root`. Returns discovered member directories
/// (NOT including the root itself) on success.
fn probe_pnpm_workspace(root: &Path) -> Option<Vec<PathBuf>> {
    let path = root.join("pnpm-workspace.yaml");
    let content = std::fs::read_to_string(&path).ok()?;

    // Parse with serde_yaml first; fall back to a minimal regex extractor
    // if the YAML is unparseable (pnpm sometimes accepts slightly sloppy
    // YAML that real-world repos have on disk).
    let packages: Vec<String> = serde_yaml::from_str::<serde_yaml::Value>(&content)
        .ok()
        .and_then(|v| {
            v.get("packages").and_then(|p| p.as_sequence()).map(|seq| {
                seq.iter()
                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
        })
        .unwrap_or_else(|| fallback_extract_yaml_list(&content, "packages"));

    if packages.is_empty() {
        return None;
    }

    Some(expand_workspace_patterns(root, &packages))
}

/// Probe for an npm/yarn `package.json` with a `workspaces` array.
fn probe_package_json_workspaces(root: &Path) -> Option<Vec<PathBuf>> {
    let path = root.join("package.json");
    let content = std::fs::read_to_string(&path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let ws = json.get("workspaces")?;

    // `workspaces` may be an array of strings OR an object with `packages`.
    let patterns: Vec<String> = if let Some(arr) = ws.as_array() {
        arr.iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect()
    } else if let Some(obj) = ws.as_object() {
        obj.get("packages")
            .and_then(|p| p.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        return None;
    };

    if patterns.is_empty() {
        return None;
    }

    Some(expand_workspace_patterns(root, &patterns))
}

/// Probe for a Cargo workspace at `root`. Parses the `[workspace]` section
/// manually to avoid pulling in the `toml` crate (the codebase already
/// reads Cargo.toml this way — see `detect_rust_crate_name` in
/// `callgraph/module_index.rs`).
fn probe_cargo_workspace(root: &Path) -> Option<Vec<PathBuf>> {
    let path = root.join("Cargo.toml");
    let content = std::fs::read_to_string(&path).ok()?;

    let mut in_workspace = false;
    let mut members_block: Option<String> = None;
    let mut buffer = String::new();
    let mut collecting = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if collecting {
                // A new section started before the array closed — bail.
                break;
            }
            in_workspace = trimmed == "[workspace]";
            continue;
        }
        if !in_workspace {
            continue;
        }

        if !collecting {
            // Look for `members = [...]` (possibly on a single line or multi-line).
            if let Some(rest) = trimmed.strip_prefix("members") {
                let after_eq = rest.trim_start().strip_prefix('=')?.trim_start();
                if let Some(after_open) = after_eq.strip_prefix('[') {
                    // Check if the array closes on the same line.
                    if let Some(end) = after_open.find(']') {
                        members_block = Some(after_open[..end].to_string());
                        break;
                    } else {
                        buffer.push_str(after_open);
                        buffer.push('\n');
                        collecting = true;
                    }
                }
            }
        } else if let Some(end) = trimmed.find(']') {
            buffer.push_str(&trimmed[..end]);
            members_block = Some(std::mem::take(&mut buffer));
            break;
        } else {
            buffer.push_str(trimmed);
            buffer.push('\n');
        }
    }

    let block = members_block?;
    let patterns: Vec<String> = block
        .split(',')
        .map(|p| {
            p.trim()
                .trim_matches('"')
                .trim_matches('\'')
                .trim()
                .to_string()
        })
        .filter(|p| !p.is_empty() && !p.starts_with('#'))
        .collect();

    if patterns.is_empty() {
        return None;
    }

    Some(expand_workspace_patterns(root, &patterns))
}

/// Probe for a Go workspace at `root` (`go.work`).
fn probe_go_work(root: &Path) -> Option<Vec<PathBuf>> {
    let path = root.join("go.work");
    let content = std::fs::read_to_string(&path).ok()?;

    let mut patterns: Vec<String> = Vec::new();
    let mut in_use_block = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }

        // Multi-line form: `use (\n\t./a\n\t./b\n)`
        if in_use_block {
            if trimmed == ")" {
                in_use_block = false;
                continue;
            }
            let p = trimmed.trim_matches('"').trim();
            if !p.is_empty() {
                patterns.push(p.to_string());
            }
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("use") {
            let rest = rest.trim_start();
            if rest == "(" || rest.is_empty() {
                in_use_block = rest == "(";
                continue;
            }
            // Single-line form: `use ./foo`
            let p = rest.trim_matches('"').trim();
            if !p.is_empty() {
                patterns.push(p.to_string());
            }
        }
    }

    if patterns.is_empty() {
        return None;
    }

    Some(expand_workspace_patterns(root, &patterns))
}

/// Expand a list of workspace patterns (possibly containing `*` globs or
/// `**` recursive globs) relative to `root` into a concrete list of
/// directory paths. Vendored / build directories are skipped.
fn expand_workspace_patterns(root: &Path, patterns: &[String]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();

    for pat in patterns {
        let cleaned = pat
            .trim()
            .trim_start_matches("./")
            .trim_end_matches('/')
            .to_string();
        if cleaned.is_empty() {
            continue;
        }

        if contains_glob_char(&cleaned) {
            let full = root.join(&cleaned);
            let full_str = full.to_string_lossy();
            if let Ok(paths) = glob::glob(&full_str) {
                for entry in paths.flatten() {
                    if entry.is_dir() && !path_contains_excluded_dir(root, &entry) {
                        out.push(entry);
                    }
                }
            }
        } else {
            let full = root.join(&cleaned);
            if full.is_dir() && !path_contains_excluded_dir(root, &full) {
                out.push(full);
            }
        }
    }

    out
}

fn contains_glob_char(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[')
}

/// Return true if any path component between `root` and `path`
/// matches a vendored / build-output directory name. The root itself
/// is NOT checked (the root can legitimately live under a dir named
/// `vendor/` on disk).
fn path_contains_excluded_dir(root: &Path, path: &Path) -> bool {
    let rel = match path.strip_prefix(root) {
        Ok(r) => r,
        Err(_) => return false,
    };
    rel.components().any(|c| {
        if let std::path::Component::Normal(name) = c {
            if let Some(s) = name.to_str() {
                return WORKSPACE_EXPANSION_EXCLUDED_DIRS.contains(&s);
            }
        }
        false
    })
}

/// Last-resort regex extractor for `key: [ "./a", "./b" ]` / block-style
/// YAML lists when `serde_yaml` rejects the document. Matches only the
/// shape real-world `pnpm-workspace.yaml` files take.
fn fallback_extract_yaml_list(content: &str, key: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_block = false;
    let prefix = format!("{}:", key);

    for line in content.lines() {
        let raw = line;
        let trimmed = line.trim_start();

        if !in_block {
            if trimmed.starts_with(&prefix) {
                // Flow-style single-line list: `packages: ["./a", "./b"]`
                if let Some(rest) = trimmed.strip_prefix(&prefix) {
                    let rest = rest.trim();
                    if let Some(inner) = rest.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                        for piece in inner.split(',') {
                            let cleaned = piece
                                .trim()
                                .trim_matches('"')
                                .trim_matches('\'')
                                .to_string();
                            if !cleaned.is_empty() {
                                out.push(cleaned);
                            }
                        }
                        return out;
                    }
                    if rest.is_empty() {
                        in_block = true;
                    }
                }
            }
            continue;
        }

        // In the block: accept `  - "./foo"` until a less-indented line.
        if raw.trim().is_empty() {
            continue;
        }
        if !raw.starts_with(' ') && !raw.starts_with('\t') {
            // Dedented past our key — block ended.
            break;
        }
        let t = raw.trim();
        if let Some(item) = t.strip_prefix("- ") {
            let cleaned = item.trim().trim_matches('"').trim_matches('\'').to_string();
            if !cleaned.is_empty() {
                out.push(cleaned);
            }
        }
    }

    out
}

/// Project-wide call graph (spec Section 2.2.1)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectCallGraph {
    edges: HashSet<CallEdge>,
}

/// Edge in the call graph
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallEdge {
    /// Path to the file containing the calling function
    pub src_file: PathBuf,
    /// Name of the calling function
    pub src_func: String,
    /// Path to the file containing the called function
    pub dst_file: PathBuf,
    /// Name of the called function
    pub dst_func: String,
}

// =============================================================================
// Type-Aware Call Graph Types (Phase 7-8: Type Resolution)
// =============================================================================

/// Confidence level for type resolution
///
/// Indicates how confident we are in the type resolution:
/// - High: Explicit annotation or constructor call
/// - Medium: Return type inference or union type
/// - Low: No type info available (fallback to variable name)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    /// Explicit annotation, constructor, or self/this reference
    High,
    /// Return type inference, union type, or interface
    Medium,
    /// Unknown type, fallback to variable name
    #[default]
    Low,
}

impl std::fmt::Display for Confidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Confidence::High => write!(f, "HIGH"),
            Confidence::Medium => write!(f, "MEDIUM"),
            Confidence::Low => write!(f, "LOW"),
        }
    }
}

/// Extended call edge with type resolution metadata
///
/// Used when --type-aware flag is enabled to track:
/// - The resolved receiver type (e.g., "User" instead of "user")
/// - Confidence level of the resolution
/// - Line number of the call site
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TypedCallEdge {
    /// Path to the file containing the calling function
    pub src_file: PathBuf,
    /// Name of the calling function
    pub src_func: String,
    /// Path to the file containing the called function
    pub dst_file: PathBuf,
    /// Name of the called function
    pub dst_func: String,
    /// Resolved receiver type (e.g., "User" for user.save())
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver_type: Option<String>,
    /// Confidence level of the type resolution
    pub confidence: Confidence,
    /// Line number of the call site
    pub call_site_line: u32,
}

impl TypedCallEdge {
    /// Create a new typed call edge from a basic CallEdge
    pub fn from_call_edge(edge: &CallEdge, line: u32) -> Self {
        Self {
            src_file: edge.src_file.clone(),
            src_func: edge.src_func.clone(),
            dst_file: edge.dst_file.clone(),
            dst_func: edge.dst_func.clone(),
            receiver_type: None,
            confidence: Confidence::Low,
            call_site_line: line,
        }
    }

    /// Create a high-confidence typed call edge
    pub fn high_confidence(
        src_file: PathBuf,
        src_func: String,
        dst_file: PathBuf,
        dst_func: String,
        receiver_type: String,
        line: u32,
    ) -> Self {
        Self {
            src_file,
            src_func,
            dst_file,
            dst_func,
            receiver_type: Some(receiver_type),
            confidence: Confidence::High,
            call_site_line: line,
        }
    }

    /// Create a medium-confidence typed call edge
    pub fn medium_confidence(
        src_file: PathBuf,
        src_func: String,
        dst_file: PathBuf,
        dst_func: String,
        receiver_type: String,
        line: u32,
    ) -> Self {
        Self {
            src_file,
            src_func,
            dst_file,
            dst_func,
            receiver_type: Some(receiver_type),
            confidence: Confidence::Medium,
            call_site_line: line,
        }
    }

    /// Convert to basic CallEdge (loses type info)
    pub fn to_call_edge(&self) -> CallEdge {
        CallEdge {
            src_file: self.src_file.clone(),
            src_func: self.src_func.clone(),
            dst_file: self.dst_file.clone(),
            dst_func: self.dst_func.clone(),
        }
    }
}

/// Statistics on type resolution (T17 mitigation)
///
/// Provides observability into how well type resolution worked,
/// helping users understand if --type-aware is useful for their codebase.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TypeResolutionStats {
    /// Whether type-aware analysis was enabled
    pub enabled: bool,
    /// Number of calls resolved with HIGH confidence
    pub resolved_high_confidence: usize,
    /// Number of calls resolved with MEDIUM confidence
    pub resolved_medium_confidence: usize,
    /// Number of calls that fell back to variable name (LOW confidence)
    pub fallback_used: usize,
    /// Total number of call sites analyzed
    pub total_call_sites: usize,
}

impl TypeResolutionStats {
    /// Create stats with type-aware enabled
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            ..Default::default()
        }
    }

    /// Record a high-confidence resolution
    pub fn record_high(&mut self) {
        self.resolved_high_confidence += 1;
        self.total_call_sites += 1;
    }

    /// Record a medium-confidence resolution
    pub fn record_medium(&mut self) {
        self.resolved_medium_confidence += 1;
        self.total_call_sites += 1;
    }

    /// Record a fallback (low confidence)
    pub fn record_fallback(&mut self) {
        self.fallback_used += 1;
        self.total_call_sites += 1;
    }

    /// Get the percentage of successfully resolved calls (HIGH + MEDIUM)
    pub fn resolution_rate(&self) -> f64 {
        if self.total_call_sites == 0 {
            return 0.0;
        }
        let resolved = self.resolved_high_confidence + self.resolved_medium_confidence;
        (resolved as f64 / self.total_call_sites as f64) * 100.0
    }

    /// Format as human-readable summary
    pub fn summary(&self) -> String {
        if !self.enabled {
            return "Type resolution: disabled".to_string();
        }
        let resolved = self.resolved_high_confidence + self.resolved_medium_confidence;
        format!(
            "Type-aware resolution: {}/{} calls resolved ({} high, {} medium confidence)",
            resolved,
            self.total_call_sites,
            self.resolved_high_confidence,
            self.resolved_medium_confidence
        )
    }
}

impl ProjectCallGraph {
    /// Create a new empty call graph
    pub fn new() -> Self {
        Self {
            edges: HashSet::new(),
        }
    }

    /// Iterate over all edges
    pub fn edges(&self) -> impl Iterator<Item = &CallEdge> {
        self.edges.iter()
    }

    /// Add an edge to the graph
    pub fn add_edge(&mut self, edge: CallEdge) {
        self.edges.insert(edge);
    }

    /// Check if the graph contains an edge
    pub fn contains(&self, edge: &CallEdge) -> bool {
        self.edges.contains(edge)
    }

    /// Get the number of edges
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Check if graph is empty
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }
}

// =============================================================================
// Impact Analysis Types (spec Section 2.2.2)
// =============================================================================

/// Impact analysis report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactReport {
    /// Map from target function name to its caller tree
    pub targets: HashMap<String, CallerTree>,
    /// Total number of target functions analyzed
    pub total_targets: usize,
    /// Type resolution statistics (when --type-aware is enabled)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_resolution: Option<TypeResolutionStats>,
}

/// Tree of callers for impact analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallerTree {
    /// Name of the function at this node
    pub function: String,
    /// Path to the file containing this function
    pub file: PathBuf,
    /// Number of direct callers of this function
    pub caller_count: usize,
    /// Recursive tree of callers (callers of callers)
    pub callers: Vec<CallerTree>,
    /// Whether the caller tree was truncated due to depth limits
    #[serde(default)]
    pub truncated: bool,
    /// Optional note about this node (e.g., truncation reason)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Confidence of type resolution for this caller (when --type-aware is enabled)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,
    /// Resolved receiver type (when --type-aware is enabled)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver_type: Option<String>,
}

// =============================================================================
// Dead Code Types (spec Section 2.2.3)
// =============================================================================

/// Dead code analysis report
///
/// Functions are classified into two tiers:
/// - `dead_functions`: Definitely dead (private/unenriched + uncalled + no special metadata)
/// - `possibly_dead`: Public/exported but uncalled (may be API surface)
///
/// The `dead_percentage` is calculated from `dead_functions` only (definitely dead).
///
/// med-low-schema-cleanup-v1 (N13): JSON serialization emits both
/// `functions_analyzed` (canonical, matches `health.summary.functions_analyzed`
/// from the M-B2 canonical-function-enumerator-v1 vocabulary) and the legacy
/// `total_functions` key (deprecated alias) for back-compat.
#[derive(Debug, Clone)]
pub struct DeadCodeReport {
    /// Functions that are definitely dead (private and uncalled)
    pub dead_functions: Vec<FunctionRef>,
    /// Public/exported functions that are uncalled (may be intentional API surface)
    pub possibly_dead: Vec<FunctionRef>,
    /// Map from file path to names of dead functions in that file
    pub by_file: HashMap<PathBuf, Vec<String>>,
    /// Count of definitely-dead functions
    pub total_dead: usize,
    /// Number of possibly-dead (public but uncalled) functions
    pub total_possibly_dead: usize,
    /// Total number of functions in the analyzed codebase.
    pub total_functions: usize,
    /// Percentage of definitely-dead functions (excludes possibly_dead)
    pub dead_percentage: f64,
}

// TLDR-7pp.1.5: hand-rolled `Deserialize` so DeadCodeReport ROUND-TRIPS
// through its own `Serialize`. The serializer below emits BOTH the canonical
// `functions_analyzed` key and the legacy `total_functions` key (N13
// back-compat). A `#[serde(alias = "functions_analyzed")]` on a single
// `total_functions` field made the derive reject that payload with "duplicate
// field `total_functions`" — harmless until `tldr dead` began honestly routing
// through the daemon, whose JSON transport round-trips the report (the daemon
// serializes, the CLI deserializes). The helper struct keeps the two keys as
// SEPARATE optional fields (no duplicate-field error) and coalesces them,
// preferring the canonical `functions_analyzed`.
impl<'de> serde::Deserialize<'de> for DeadCodeReport {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct DeadCodeReportDe {
            dead_functions: Vec<FunctionRef>,
            #[serde(default)]
            possibly_dead: Vec<FunctionRef>,
            by_file: HashMap<PathBuf, Vec<String>>,
            total_dead: usize,
            #[serde(default)]
            total_possibly_dead: usize,
            #[serde(default)]
            functions_analyzed: Option<usize>,
            #[serde(default)]
            total_functions: Option<usize>,
            dead_percentage: f64,
        }
        let de = DeadCodeReportDe::deserialize(deserializer)?;
        Ok(DeadCodeReport {
            dead_functions: de.dead_functions,
            possibly_dead: de.possibly_dead,
            by_file: de.by_file,
            total_dead: de.total_dead,
            total_possibly_dead: de.total_possibly_dead,
            // Prefer the canonical key; fall back to the legacy one.
            total_functions: de.functions_analyzed.or(de.total_functions).unwrap_or(0),
            dead_percentage: de.dead_percentage,
        })
    }
}

// med-low-schema-cleanup-v1 (N13): hand-rolled `Serialize` so we can emit
// the canonical `functions_analyzed` key AND keep the legacy
// `total_functions` key in JSON for back-compat. Field order matches the
// previous struct shape so existing snapshot tests keep diffing cleanly.
impl serde::Serialize for DeadCodeReport {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("DeadCodeReport", 8)?;
        s.serialize_field("dead_functions", &self.dead_functions)?;
        s.serialize_field("possibly_dead", &self.possibly_dead)?;
        s.serialize_field("by_file", &self.by_file)?;
        s.serialize_field("total_dead", &self.total_dead)?;
        s.serialize_field("total_possibly_dead", &self.total_possibly_dead)?;
        // Canonical key (N13).
        s.serialize_field("functions_analyzed", &self.total_functions)?;
        // Deprecated alias - kept for back-compat with consumers that
        // were reading the old key. Will be removed in a future major
        // release; new code should read `functions_analyzed`.
        s.serialize_field("total_functions", &self.total_functions)?;
        s.serialize_field("dead_percentage", &self.dead_percentage)?;
        s.end()
    }
}

// =============================================================================
// Importers Types (spec Section 2.2.4)
// =============================================================================

/// Report of files importing a module
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportersReport {
    /// Name of the module being queried
    pub module: String,
    /// Files that import this module
    pub importers: Vec<ImporterInfo>,
    /// Total number of importers found
    pub total: usize,
}

/// Information about a file that imports a module
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImporterInfo {
    /// Path to the file that contains the import
    pub file: PathBuf,
    /// Line number of the import statement (1-indexed)
    pub line: u32,
    /// Full text of the import statement
    pub import_statement: String,
}

// =============================================================================
// Architecture Types (spec Section 2.2.5)
// =============================================================================

/// Architecture analysis report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureReport {
    /// Functions in the entry layer (called by external consumers, call others)
    pub entry_layer: Vec<FunctionRef>,
    /// Functions in the middle/service layer (called by entry, call leaf)
    pub middle_layer: Vec<FunctionRef>,
    /// Functions in the leaf/utility layer (called by others, call nothing external)
    pub leaf_layer: Vec<FunctionRef>,
    /// Per-directory statistics (function counts, call directions)
    pub directories: HashMap<PathBuf, DirStats>,
    /// Detected circular dependencies between directories
    pub circular_dependencies: Vec<CircularDep>,
    /// Inferred architectural layer for each directory
    pub inferred_layers: HashMap<PathBuf, LayerType>,
}

/// Directory statistics for architecture analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirStats {
    /// Names of functions defined in this directory
    pub functions: Vec<String>,
    /// Number of outgoing calls from this directory to other directories
    pub calls_out: usize,
    /// Number of incoming calls from other directories into this directory
    pub calls_in: usize,
}

/// Circular dependency between directories
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircularDep {
    /// First directory in the circular dependency
    pub a: PathBuf,
    /// Second directory in the circular dependency
    pub b: PathBuf,
}

/// Inferred layer type for a directory
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum LayerType {
    /// Entry point layer (API handlers, CLI commands, main functions)
    Entry,
    /// Service/business logic layer (orchestrates utilities)
    Service,
    /// Utility/leaf layer (pure helpers, no external dependencies)
    Utility,
    /// Dynamic dispatch layer (virtual calls, trait objects, callbacks)
    DynamicDispatch,
}

// =============================================================================
// CFG Types (Layer 3, spec Section 2.3)
// =============================================================================

/// Control flow graph information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CfgInfo {
    /// Name of the function this CFG represents
    pub function: String,
    /// Basic blocks in the control flow graph
    pub blocks: Vec<CfgBlock>,
    /// Edges connecting basic blocks
    pub edges: Vec<CfgEdge>,
    /// ID of the entry basic block
    pub entry_block: usize,
    /// IDs of exit basic blocks (return/end points)
    pub exit_blocks: Vec<usize>,
    /// Cyclomatic complexity of this function
    pub cyclomatic_complexity: u32,
    /// CFGs for nested/inner functions defined within this function
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub nested_functions: HashMap<String, CfgInfo>,
}

/// Basic block in CFG
#[derive(Archive, Debug, Clone, RkyvDeserialize, RkyvSerialize, Serialize, Deserialize)]
pub struct CfgBlock {
    /// Unique identifier for this basic block
    pub id: usize,
    /// Classification of this basic block (entry, branch, loop, etc.)
    pub block_type: BlockType,
    /// Line range covered by this block (start_line, end_line), 1-indexed
    pub lines: (u32, u32),
    /// Function calls made within this basic block
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<String>,
}

/// Type of basic block
#[derive(
    Archive,
    Debug,
    Clone,
    Copy,
    RkyvDeserialize,
    RkyvSerialize,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
)]
#[serde(rename_all = "snake_case")]
pub enum BlockType {
    /// Function entry point
    Entry,
    /// Conditional branch (if/else, match)
    Branch,
    /// Loop condition check (for, while header)
    LoopHeader,
    /// Loop body statements
    LoopBody,
    /// Return statement
    Return,
    /// Function exit point
    Exit,
    /// Sequential statement block
    Body,
}

/// Edge in CFG
#[derive(Archive, Debug, Clone, RkyvDeserialize, RkyvSerialize, Serialize, Deserialize)]
pub struct CfgEdge {
    /// ID of the source basic block
    pub from: usize,
    /// ID of the target basic block
    pub to: usize,
    /// Classification of this edge (true branch, false branch, unconditional, etc.)
    pub edge_type: EdgeType,
    /// Condition expression for conditional edges (e.g., `x > 0`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

/// Type of CFG edge
#[derive(
    Archive,
    Debug,
    Clone,
    Copy,
    RkyvDeserialize,
    RkyvSerialize,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
)]
#[serde(rename_all = "snake_case")]
pub enum EdgeType {
    /// True branch of a conditional
    True,
    /// False branch of a conditional
    False,
    /// Unconditional flow (fallthrough, goto)
    Unconditional,
    /// Back edge to a loop header
    BackEdge,
    /// Break out of a loop
    Break,
    /// Continue to next loop iteration
    Continue,
}

/// Complexity metrics (spec Section 2.3.2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityMetrics {
    /// Name of the function being measured
    pub function: String,
    /// Cyclomatic complexity (number of independent paths)
    pub cyclomatic: u32,
    /// Cognitive complexity (how hard the function is to understand)
    pub cognitive: u32,
    /// Maximum nesting depth of control structures.
    ///
    /// cross-command-consistency-v1 (BUG-7): renamed from `nesting_depth` to
    /// `max_nesting` so this value uses the same field name that
    /// `tldr cognitive` already exposes.  The serde alias keeps deserialisation
    /// of older JSON bodies (`{"nesting_depth": ...}`) working.
    #[serde(alias = "nesting_depth")]
    pub max_nesting: u32,
    /// Number of lines of code in the function
    pub lines_of_code: u32,
}

// =============================================================================
// DFG Types (Layer 4, spec Section 2.4)
// =============================================================================

/// Data flow graph information
#[derive(Archive, Debug, Clone, RkyvDeserialize, RkyvSerialize, Serialize, Deserialize)]
pub struct DfgInfo {
    /// Name of the function this data flow graph represents
    pub function: String,
    /// All variable references (definitions, updates, uses) in the function
    pub refs: Vec<VarRef>,
    /// Data flow edges (def-use chains) connecting definitions to their uses
    pub edges: Vec<DataflowEdge>,
    /// Names of all variables tracked in this function
    pub variables: Vec<String>,
}

/// Variable reference in DFG
#[derive(Archive, Debug, Clone, RkyvDeserialize, RkyvSerialize, Serialize, Deserialize)]
pub struct VarRef {
    /// Name of the variable being referenced
    pub name: String,
    /// Whether this is a definition, update, or use of the variable
    pub ref_type: RefType,
    /// Line number of this reference (1-indexed)
    pub line: u32,
    /// Column number of this reference (0-indexed)
    pub column: u32,
    /// Language-specific construct context (e.g., "augmented_assignment", "destructuring")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<VarRefContext>,
    /// Statement group ID for parallel assignments (e.g., a, b = b, a)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<u32>,
}

/// Context for language-specific variable reference patterns
#[derive(
    Archive, Debug, Clone, RkyvDeserialize, RkyvSerialize, Serialize, Deserialize, PartialEq, Eq,
)]
#[serde(rename_all = "snake_case")]
pub enum VarRefContext {
    // Python-specific
    /// x += 1: both use and def in same statement
    AugmentedAssignment,
    /// a, b = b, a: parallel semantics (RHS evaluated before LHS)
    MultipleAssignment,
    /// n := expr: walrus operator, def in expression context
    WalrusOperator,
    /// [x for x in ...]: x is scoped to comprehension
    ComprehensionScope,
    /// match case (x, y): pattern binding
    MatchBinding,
    /// global x / nonlocal x: external scope reference
    GlobalNonlocal,

    // TypeScript/JavaScript-specific
    /// const {a, b} = obj: destructuring creates multiple defs
    Destructuring,
    /// Closure captures variable by reference
    ClosureCapture,
    /// Optional chaining (?.) short-circuit
    OptionalChain,

    // Go-specific
    /// x := 1: short declaration (may be new var or redefinition)
    ShortDeclaration,
    /// a, b := f(): multiple return values
    MultipleReturn,
    /// _ = x: blank identifier (not a real definition)
    BlankIdentifier,
    /// defer log(x): captured at defer point
    DeferCapture,

    // Rust-specific
    /// let x = 1; let x = 2: shadowing creates NEW variable
    Shadowing,
    /// let (a, b) = tuple: pattern binding
    PatternBinding,
    /// let b = a: ownership move ends a's liveness
    OwnershipMove,
    /// match x { Some(v) => ... }: binding scoped to arm
    MatchArmBinding,
}

/// Type of variable reference
#[derive(
    Archive,
    Debug,
    Clone,
    Copy,
    RkyvDeserialize,
    RkyvSerialize,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
)]
#[serde(rename_all = "lowercase")]
pub enum RefType {
    /// Variable definition (first assignment)
    Definition,
    /// Variable update (reassignment or mutation)
    Update,
    /// Variable use (read)
    Use,
}

/// Data flow edge (def-use chain)
#[derive(Archive, Debug, Clone, RkyvDeserialize, RkyvSerialize, Serialize, Deserialize)]
pub struct DataflowEdge {
    /// Name of the variable flowing from definition to use
    pub var: String,
    /// Line number where the variable is defined (1-indexed)
    pub def_line: u32,
    /// Line number where the variable is used (1-indexed)
    pub use_line: u32,
    /// Full variable reference at the definition site
    pub def_ref: VarRef,
    /// Full variable reference at the use site
    pub use_ref: VarRef,
}

// =============================================================================
// PDG Types (Layer 5, spec Section 2.5)
// =============================================================================

/// Program dependence graph information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PdgInfo {
    /// Name of the function this PDG represents
    pub function: String,
    /// Control flow graph for this function
    pub cfg: CfgInfo,
    /// Data flow graph for this function
    pub dfg: DfgInfo,
    /// Nodes in the program dependence graph
    pub nodes: Vec<PdgNode>,
    /// Dependence edges (control and data) between PDG nodes
    pub edges: Vec<PdgEdge>,
}

/// Node in PDG
#[derive(Archive, Debug, Clone, RkyvDeserialize, RkyvSerialize, Serialize, Deserialize)]
pub struct PdgNode {
    /// Unique identifier for this PDG node
    pub id: usize,
    /// Type of statement at this node (e.g., "assignment", "branch", "call")
    pub node_type: String,
    /// Line range covered by this node (start_line, end_line), 1-indexed
    pub lines: (u32, u32),
    /// Variables defined at this node
    pub definitions: Vec<String>,
    /// Variables used at this node
    pub uses: Vec<String>,
}

/// Edge in PDG
#[derive(Archive, Debug, Clone, RkyvDeserialize, RkyvSerialize, Serialize, Deserialize)]
pub struct PdgEdge {
    /// ID of the source PDG node
    pub source_id: usize,
    /// ID of the target PDG node
    pub target_id: usize,
    /// Whether this is a control or data dependence
    pub dep_type: DependenceType,
    /// Human-readable label describing the dependence (e.g., variable name)
    pub label: String,
}

/// Type of dependence in PDG
#[derive(
    Archive,
    Debug,
    Clone,
    Copy,
    RkyvDeserialize,
    RkyvSerialize,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
)]
#[serde(rename_all = "lowercase")]
pub enum DependenceType {
    /// Control dependence (execution of target depends on a branch decision)
    Control,
    /// Data dependence (target uses a value defined by source)
    Data,
}

/// Slice direction for program slicing (spec Section 2.5.2)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SliceDirection {
    /// Backward slice: find all statements that affect the slicing criterion
    Backward,
    /// Forward slice: find all statements affected by the slicing criterion
    Forward,
}

impl std::str::FromStr for SliceDirection {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "backward" | "back" | "b" => Ok(SliceDirection::Backward),
            "forward" | "fwd" | "f" => Ok(SliceDirection::Forward),
            _ => Err(format!(
                "Invalid direction: {}. Expected 'backward' or 'forward'",
                s
            )),
        }
    }
}

/// Thin slice result (spec Section 2.5.3)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinSliceResult {
    /// Line numbers in the thin (data-only) slice
    pub lines: HashSet<u32>,
    /// Line numbers in the full (data + control) slice for comparison
    pub full_slice_lines: HashSet<u32>,
    /// Percentage reduction from full slice to thin slice
    pub reduction_pct: f64,
}

// =============================================================================
// Search Types (spec Section 2.6)
// =============================================================================

/// Search match result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchMatch {
    /// Path to the file containing the match
    pub file: PathBuf,
    /// Line number of the match (1-indexed)
    pub line: u32,
    /// Content of the matching line
    pub content: String,
    /// Surrounding context lines (before and after the match)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Vec<String>>,
}

/// BM25 search result (spec Section 2.6.2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bm25Result {
    /// Path to the file containing the result
    pub file_path: PathBuf,
    /// BM25 relevance score
    pub score: f64,
    /// Start line of the matching snippet (1-indexed)
    pub line_start: u32,
    /// End line of the matching snippet (1-indexed)
    pub line_end: u32,
    /// Text snippet containing the match
    pub snippet: String,
    /// Query terms that matched in this result
    pub matched_terms: Vec<String>,
}

/// Hybrid search result (spec Section 2.6.3)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridResult {
    /// Path to the file containing the result
    pub file_path: PathBuf,
    /// Reciprocal Rank Fusion score combining BM25 and dense retrieval
    pub rrf_score: f64,
    /// Rank from the BM25 retriever, if this result appeared in BM25 results
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bm25_rank: Option<usize>,
    /// Rank from the dense (embedding) retriever, if applicable
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dense_rank: Option<usize>,
    /// Raw BM25 score, if this result appeared in BM25 results
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bm25_score: Option<f64>,
    /// Raw dense (cosine similarity) score, if applicable
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dense_score: Option<f64>,
    /// Text snippet containing the match
    pub snippet: String,
    /// Query terms that matched in this result
    pub matched_terms: Vec<String>,
}

/// Hybrid search report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridSearchReport {
    /// Ranked search results after reciprocal rank fusion
    pub results: Vec<HybridResult>,
    /// Original search query string
    pub query: String,
    /// Total number of candidate results before ranking
    pub total_candidates: usize,
    /// Number of results found only by BM25 (not dense retrieval)
    pub bm25_only: usize,
    /// Number of results found only by dense retrieval (not BM25)
    pub dense_only: usize,
    /// Number of results found by both retrievers
    pub overlap: usize,
    /// Fallback mode used when dense retrieval is unavailable (e.g., "bm25_only")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_mode: Option<String>,
}

// =============================================================================
// Context Types (spec Section 2.7)
// =============================================================================

/// Relevant context for LLM (spec Section 2.7.1)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelevantContext {
    /// Name of the entry point function for context gathering
    pub entry_point: String,
    /// Maximum call depth traversed to gather context
    pub depth: usize,
    /// Functions reachable from the entry point within the specified depth
    pub functions: Vec<FunctionContext>,
}

/// Function context for LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionContext {
    /// Name of the function
    pub name: String,
    /// Path to the file containing this function
    pub file: PathBuf,
    /// Line number where the function is defined (1-indexed)
    pub line: u32,
    /// Full function signature
    pub signature: String,
    /// Docstring or doc comment, if present
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    /// Names of functions called by this function
    pub calls: Vec<String>,
    /// Number of basic blocks in the function's CFG
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocks: Option<usize>,
    /// Cyclomatic complexity of the function
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cyclomatic: Option<u32>,
}

impl RelevantContext {
    /// Format for LLM consumption
    pub fn to_llm_string(&self) -> String {
        let mut output = String::new();
        output.push_str(&format!("# Context for: {}\n\n", self.entry_point));
        for func in &self.functions {
            output.push_str(&format!("## {}\n", func.name));
            output.push_str(&format!("File: {}:{}\n", func.file.display(), func.line));
            output.push_str(&format!("Signature: {}\n", func.signature));
            if let Some(doc) = &func.docstring {
                output.push_str(&format!("Doc: {}\n", doc));
            }
            if !func.calls.is_empty() {
                output.push_str(&format!("Calls: {}\n", func.calls.join(", ")));
            }
            output.push('\n');
        }
        output
    }
}

// =============================================================================
// Change Impact Types (spec Section 2.7.2)
// =============================================================================

/// Change impact report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeImpactReport {
    /// Files that were changed (from git diff or explicit input)
    pub changed_files: Vec<PathBuf>,
    /// Test files potentially affected by the changes
    pub affected_tests: Vec<PathBuf>,
    /// Functions transitively affected by the changes
    pub affected_functions: Vec<FunctionRef>,
    /// Method used to detect impacts (e.g., "call_graph", "import_graph")
    pub detection_method: String,
}

// =============================================================================
// Quality Types (spec Section 2.8)
// =============================================================================

/// Threshold preset for quality checks
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ThresholdPreset {
    /// Strict thresholds (lower tolerance for smells and complexity)
    Strict,
    /// Default thresholds (balanced tolerance)
    #[default]
    Default,
    /// Relaxed thresholds (higher tolerance, fewer warnings)
    Relaxed,
}

/// Code smell type
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SmellType {
    /// Class that does too much (high number of methods, fields, or responsibilities)
    GodClass,
    /// Method with too many lines of code or excessive complexity
    LongMethod,
    /// Method that uses another class's data more than its own
    FeatureEnvy,
    /// Groups of fields that frequently appear together across classes
    DataClumps,
    /// Function with too many parameters
    LongParameterList,
}

/// Code smells report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmellsReport {
    /// Individual code smell findings
    pub smells: Vec<SmellFinding>,
    /// Number of files analyzed for code smells
    pub files_analyzed: usize,
    /// Total number of code smells found
    pub total_smells: usize,
}

/// Individual smell finding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmellFinding {
    /// Path to the file containing the smell
    pub file: PathBuf,
    /// Line number where the smell occurs (1-indexed)
    pub line: u32,
    /// Classification of the code smell
    pub smell_type: SmellType,
    /// Human-readable description of the smell
    pub description: String,
    /// Suggested fix or refactoring
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

/// Maintainability report (spec Section 2.8.2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintainabilityReport {
    /// Per-file maintainability index results
    pub files: Vec<FileMI>,
    /// Aggregate summary of maintainability across all files
    pub summary: MISummary,
}

/// File maintainability index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMI {
    /// Path to the source file
    pub path: PathBuf,
    /// Maintainability Index score (0-100, higher is better)
    pub mi: f64,
    /// Letter grade (A, B, or C) derived from the MI score
    pub grade: char,
    /// Halstead metrics used in MI calculation, if computed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub halstead: Option<HalsteadMetrics>,
}

/// MI summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MISummary {
    /// Average Maintainability Index across all files
    pub average_mi: f64,
    /// Lowest Maintainability Index (worst file)
    pub min_mi: f64,
    /// Highest Maintainability Index (best file)
    pub max_mi: f64,
    /// Number of files included in the summary
    pub files_analyzed: usize,
}

/// Halstead metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HalsteadMetrics {
    /// Number of distinct operators and operands (n = n1 + n2)
    pub vocabulary: u32,
    /// Total number of operators and operands (N = N1 + N2)
    pub length: u32,
    /// Volume: N * log2(n), measures information content
    pub volume: f64,
    /// Difficulty: (n1/2) * (N2/n2), measures error-proneness
    pub difficulty: f64,
    /// Effort: D * V, measures cognitive effort to understand
    pub effort: f64,
}

// =============================================================================
// Security Types (spec Section 2.9)
// =============================================================================

/// Severity level for security findings
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Low severity (informational, minor risk)
    Low,
    /// Medium severity (moderate risk, should be addressed)
    Medium,
    /// High severity (significant risk, needs prompt attention)
    High,
    /// Critical severity (immediate risk, must be fixed urgently)
    Critical,
}

/// Secrets report (spec Section 2.9.1)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretsReport {
    /// Individual secret findings (hardcoded keys, tokens, passwords)
    pub findings: Vec<SecretFinding>,
    /// Number of files scanned for secrets
    pub files_scanned: usize,
    /// Number of secret patterns checked
    pub patterns_checked: usize,
    /// Aggregate summary of findings by severity
    pub summary: SecretsSummary,
}

/// Secret finding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretFinding {
    /// Path to the file containing the secret
    pub file: PathBuf,
    /// Line number where the secret was found (1-indexed)
    pub line: u32,
    /// Name of the pattern that matched (e.g., "AWS_ACCESS_KEY")
    pub pattern: String,
    /// Severity of the finding
    pub severity: Severity,
    /// Partially masked value showing the secret type without exposing it
    pub masked_value: String,
}

/// Secrets summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretsSummary {
    /// Total number of secret findings
    pub total_findings: usize,
    /// Breakdown of findings by severity level
    pub by_severity: HashMap<String, usize>,
}

/// Vulnerability type (spec Section 2.9.2)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum VulnType {
    /// SQL injection via unsanitized user input in queries
    SqlInjection,
    /// Cross-site scripting via unescaped output
    Xss,
    /// OS command injection via unsanitized shell arguments
    CommandInjection,
    /// Path traversal via unvalidated file paths
    PathTraversal,
    /// Server-side request forgery via user-controlled URLs
    Ssrf,
    /// Unsafe deserialization of untrusted data
    Deserialization,
}

/// Vulnerability report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VulnReport {
    /// Individual vulnerability findings
    pub findings: Vec<VulnFinding>,
    /// Number of files scanned for vulnerabilities
    pub files_scanned: usize,
    /// Aggregate summary by type and severity
    pub summary: VulnSummary,
}

/// Vulnerability finding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VulnFinding {
    /// Path to the file containing the vulnerability
    pub file: PathBuf,
    /// Line number where the vulnerability occurs (1-indexed)
    pub line: u32,
    /// Classification of the vulnerability
    pub vuln_type: VulnType,
    /// Severity of the vulnerability
    pub severity: Severity,
    /// Human-readable description of the vulnerability
    pub description: String,
    /// Taint source (where untrusted data enters), if identified
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Taint sink (where untrusted data is consumed unsafely), if identified
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sink: Option<String>,
}

/// Vulnerability summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VulnSummary {
    /// Total number of vulnerability findings
    pub total_findings: usize,
    /// Breakdown of findings by vulnerability type
    pub by_type: HashMap<String, usize>,
    /// Breakdown of findings by severity level
    pub by_severity: HashMap<String, usize>,
}

// =============================================================================
// Tests
// =============================================================================

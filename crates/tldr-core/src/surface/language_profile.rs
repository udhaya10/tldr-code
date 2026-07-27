//! Per-language static surface extraction profiles.
//!
//! This module isolates repository-shape and entrypoint heuristics for static
//! API surface extraction. The profiles are intentionally separate from the
//! extractors themselves so each language can evolve its own surface policy
//! without cross-language coupling.

use std::path::{Component, Path};

use crate::types::Language;

#[path = "profiles/mod.rs"]
mod profiles;

/// Static surface policy for one language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceLanguageProfile {
    /// Canonical language enum used by `surface`.
    pub language: Language,
    /// Exact directory names that should usually be ignored for API discovery.
    pub noise_dirs: &'static [&'static str],
    /// File suffixes that usually indicate tests, fixtures, or benchmarks.
    pub noise_file_suffixes: &'static [&'static str],
    /// Leading scaffolding segments to trim after prefix stripping.
    pub drop_segments: &'static [&'static str],
    /// Layout-specific multi-segment prefixes to trim from the start of a path.
    pub drop_prefixes: &'static [&'static [&'static str]],
    /// Preferred roots for package-facing API discovery.
    pub preferred_roots: &'static [&'static str],
    /// Files or roots that commonly define the package entrypoint.
    pub entrypoints: &'static [&'static str],
}

#[doc(hidden)]
pub trait SurfaceProfileLanguage {
    fn into_language(self) -> Option<Language>;
}

impl SurfaceProfileLanguage for Language {
    fn into_language(self) -> Option<Language> {
        Some(self)
    }
}

impl SurfaceProfileLanguage for &str {
    fn into_language(self) -> Option<Language> {
        match self {
            "c" => Some(Language::C),
            "cpp" => Some(Language::Cpp),
            "csharp" => Some(Language::CSharp),
            "elixir" => Some(Language::Elixir),
            "go" => Some(Language::Go),
            "java" => Some(Language::Java),
            "javascript" | "js" => Some(Language::JavaScript),
            "kotlin" => Some(Language::Kotlin),
            "lua" => Some(Language::Lua),
            "php" => Some(Language::Php),
            "python" => Some(Language::Python),
            "ruby" => Some(Language::Ruby),
            "rust" => Some(Language::Rust),
            "scala" => Some(Language::Scala),
            "swift" => Some(Language::Swift),
            "typescript" | "ts" => Some(Language::TypeScript),
            "luau" => Some(Language::Luau),
            "ocaml" => Some(Language::Ocaml),
            _ => None,
        }
    }
}

#[doc(hidden)]
pub trait IntoLayoutSegments {
    fn into_layout_segments(self) -> Vec<String>;
}

impl IntoLayoutSegments for &Path {
    fn into_layout_segments(self) -> Vec<String> {
        path_segments(self)
    }
}

impl IntoLayoutSegments for Vec<String> {
    fn into_layout_segments(self) -> Vec<String> {
        self
    }
}

/// Look up the static surface profile for one supported language.
#[must_use]
pub fn language_profile<L>(language: L) -> Option<&'static SurfaceLanguageProfile>
where
    L: SurfaceProfileLanguage,
{
    profiles::profile_for(language.into_language()?)
}

/// Return every language with a dedicated static surface profile.
#[must_use]
pub fn supported_surface_languages() -> &'static [Language] {
    profiles::SUPPORTED_SURFACE_LANGUAGES
}

/// Return `true` if a directory should usually be ignored for a language.
#[must_use]
pub fn is_noise_dir<L>(language: L, dir_name: &str) -> bool
where
    L: SurfaceProfileLanguage,
{
    let Some(profile) = language_profile(language) else {
        return false;
    };

    let candidate = dir_name.to_ascii_lowercase();
    profile.noise_dirs.iter().any(|entry| *entry == candidate)
}

/// Return `true` if a file name matches one of the language's noise suffixes.
#[must_use]
pub fn is_noise_file<L>(language: L, file_name: &str) -> bool
where
    L: SurfaceProfileLanguage,
{
    let Some(profile) = language_profile(language) else {
        return false;
    };

    let candidate = file_name.to_ascii_lowercase();
    profile
        .noise_file_suffixes
        .iter()
        .any(|suffix| candidate.ends_with(suffix))
}

/// Strip language-specific layout segments from a path.
///
/// This removes configured layout prefixes such as `src/main/java`,
/// `src/commonMain/kotlin`, or `Sources`, then trims any remaining leading
/// scaffolding segments like `src` or `lib`.
#[must_use]
pub fn strip_layout_segments<L, P>(language: L, path: P) -> Vec<String>
where
    L: SurfaceProfileLanguage,
    P: IntoLayoutSegments,
{
    let mut segments = path.into_layout_segments();
    let Some(profile) = language_profile(language) else {
        return segments;
    };
    if let Some((start, len)) = matching_prefix_len(&segments, profile.drop_prefixes) {
        segments.drain(start..start + len);
    }
    while segments
        .first()
        .is_some_and(|segment| contains_ignore_case(profile.drop_segments, segment))
    {
        segments.remove(0);
    }
    segments
}

/// Return the canonical entrypoint candidates for a language profile.
#[must_use]
pub fn entrypoint_candidates<L>(language: L) -> &'static [&'static str]
where
    L: SurfaceProfileLanguage,
{
    language_profile(language)
        .map(|profile| profile.entrypoints)
        .unwrap_or(&[])
}

/// Compute a static preference score for a relative path.
///
/// This is a small shared ranking scaffold for future extractor integration.
/// Higher scores indicate paths that look more package-facing according to the
/// language profile:
///
/// - entrypoints score highest
/// - preferred roots receive a smaller positive boost
/// - neutral paths stay at `0`
/// - paths containing known noise directories are demoted
#[must_use]
pub fn static_preference_score<L>(language: L, relative_path: &Path) -> i32
where
    L: SurfaceProfileLanguage,
{
    let Some(profile) = language_profile(language) else {
        return 0;
    };

    let segments = path_segments(relative_path);
    if segments.is_empty() {
        return 0;
    }

    if segments
        .iter()
        .any(|segment| contains_ignore_case(profile.noise_dirs, segment))
    {
        return -100;
    }

    let mut score = 0;

    if profile
        .entrypoints
        .iter()
        .any(|rule| path_matches_rule(&segments, rule))
    {
        score += 100;
    }

    if profile
        .preferred_roots
        .iter()
        .any(|rule| path_matches_rule(&segments, rule))
    {
        score += 25;
    }

    score
}

fn path_segments(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(segment) => Some(segment.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

fn matching_prefix_len(
    segments: &[String],
    prefixes: &'static [&'static [&'static str]],
) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None; // (start, prefix_len)
    for prefix in prefixes {
        for start in 0..segments.len() {
            if starts_with_ignore_case(&segments[start..], prefix)
                && best.is_none_or(|(_, prev_len)| prefix.len() > prev_len)
            {
                best = Some((start, prefix.len()));
            }
        }
    }
    best
}

fn starts_with_ignore_case(segments: &[String], prefix: &[&str]) -> bool {
    segments.len() >= prefix.len()
        && segments
            .iter()
            .take(prefix.len())
            .zip(prefix.iter())
            .all(|(segment, expected)| segment.eq_ignore_ascii_case(expected))
}

fn contains_ignore_case(values: &[&str], candidate: &str) -> bool {
    values
        .iter()
        .any(|value| candidate.eq_ignore_ascii_case(value))
}

fn path_matches_rule(segments: &[String], rule: &str) -> bool {
    if rule == "." {
        return true;
    }

    let rule_segments: Vec<&str> = rule
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect();

    if rule_segments.is_empty() {
        return false;
    }

    if starts_with_ignore_case(segments, &rule_segments) {
        return true;
    }

    rule_segments.len() == 1
        && segments
            .last()
            .is_some_and(|segment| segment.eq_ignore_ascii_case(rule_segments[0]))
}

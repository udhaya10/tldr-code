//! Pattern detection module for design pattern mining
//!
//! This module provides single-pass pattern extraction across codebases.
//! Addresses blockers: A5 (multi-pass overhead), A23 (parse error handling)
//!
//! # Architecture
//!
//! The pattern detection framework uses a single-pass approach:
//! 1. Parse each file once into AST
//! 2. Walk AST once, collecting signals for ALL patterns
//! 3. Convert signals to patterns after walk
//! 4. Aggregate patterns across files
//!
//! # Example
//!
//! ```rust,ignore
//! use tldr_core::patterns::{PatternMiner, PatternConfig};
//!
//! let miner = PatternMiner::new(PatternConfig::default());
//! let report = miner.mine_patterns(Path::new("src"), None)?;
//! ```

pub mod api_conventions;
pub mod async_patterns;
pub mod constraints;
pub mod detector;
pub mod error_handling;
pub mod format;
pub mod import_patterns;
pub mod language_profile;
pub mod languages;
pub mod naming;
pub mod resource_mgmt;
pub mod signals;
pub mod soft_delete;
pub mod test_idioms;
pub mod type_coverage;
pub mod validation;

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use crate::ast::parser::ParserPool;
use crate::error::TldrError;
use crate::fs::tree::{collect_files, get_file_tree};
use crate::types::{
    ApiConventionPattern, AsyncPattern, ErrorHandlingPattern, ImportPattern, Language,
    LanguageDistribution, NamingPattern, PatternCategory, PatternMetadata, PatternReport,
    ResourceManagementPattern, SoftDeletePattern, TestIdiomPattern, TypeCoveragePattern,
    ValidationPattern,
};
use crate::TldrResult;

pub use constraints::{generate_constraints, DetectedPatterns};
pub use detector::PatternDetector;
pub use signals::PatternSignals;

/// Configuration for pattern mining
#[derive(Debug, Clone)]
pub struct PatternConfig {
    /// Minimum confidence threshold for patterns (0.0-1.0)
    pub min_confidence: f64,
    /// Maximum files to analyze (0 = unlimited)
    pub max_files: usize,
    /// Number of evidence examples per pattern
    pub evidence_limit: usize,
    /// Categories to detect (empty = all)
    pub categories: Vec<PatternCategory>,
    /// Whether to generate LLM constraints
    pub generate_constraints: bool,
}

impl Default for PatternConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.5,
            max_files: 1000,
            evidence_limit: 3,
            categories: Vec::new(), // All categories
            generate_constraints: true,
        }
    }
}

/// Pattern miner that performs single-pass extraction across codebases
pub struct PatternMiner {
    config: PatternConfig,
    parser_pool: ParserPool,
}

impl PatternMiner {
    /// Create a new pattern miner with the given configuration
    pub fn new(config: PatternConfig) -> Self {
        Self {
            config,
            parser_pool: ParserPool::new(),
        }
    }

    /// Mine patterns from a path (file or directory)
    ///
    /// # Arguments
    /// * `path` - Path to file or directory to analyze
    /// * `lang` - Optional language filter (auto-detect if None)
    ///
    /// # Returns
    /// * `Ok(PatternReport)` - Complete pattern analysis report
    /// * `Err(TldrError)` - If analysis fails
    pub fn mine_patterns(&self, path: &Path, lang: Option<Language>) -> TldrResult<PatternReport> {
        let start = Instant::now();

        // Collect files to analyze
        let files = self.collect_files(path, lang)?;

        let mut files_analyzed = 0;
        let mut files_skipped = 0;
        let mut files_partial = 0;
        let mut files_by_language: HashMap<String, usize> = HashMap::new();
        let mut patterns_by_language: HashMap<String, usize> = HashMap::new();

        // Aggregate signals across all files
        let mut aggregated_signals = PatternSignals::default();

        for (file_path, file_lang) in files.iter().take(self.config.max_files) {
            // Read file content
            let content = match std::fs::read_to_string(file_path) {
                Ok(c) => c,
                Err(_) => {
                    files_skipped += 1;
                    continue;
                }
            };

            // Parse and extract signals
            match self.extract_file_signals(&content, *file_lang, file_path) {
                Ok(signals) => {
                    aggregated_signals.merge(&signals);
                    files_analyzed += 1;
                    *files_by_language.entry(file_lang.to_string()).or_insert(0) += 1;
                }
                Err(TldrError::ParseError { .. }) => {
                    // Try partial extraction for parse errors (A23 mitigation)
                    if let Ok(partial) =
                        self.extract_partial_signals(&content, *file_lang, file_path)
                    {
                        aggregated_signals.merge(&partial);
                        files_partial += 1;
                        *files_by_language.entry(file_lang.to_string()).or_insert(0) += 1;
                    } else {
                        files_skipped += 1;
                    }
                }
                Err(_) => {
                    files_skipped += 1;
                }
            }
        }

        let duration_ms = start.elapsed().as_millis() as u64;

        // Convert signals to patterns
        let soft_delete = self.signals_to_soft_delete(&aggregated_signals);
        let error_handling = self.signals_to_error_handling(&aggregated_signals);
        let naming = self.signals_to_naming(&aggregated_signals);
        let resource_management = self.signals_to_resource_mgmt(&aggregated_signals);
        let validation = self.signals_to_validation(&aggregated_signals);
        let test_idioms = self.signals_to_test_idioms(&aggregated_signals);
        let import_patterns = self.signals_to_import_patterns(&aggregated_signals);
        let type_coverage = self.signals_to_type_coverage(&aggregated_signals);
        let api_conventions = self.signals_to_api_conventions(&aggregated_signals);
        let async_patterns = self.signals_to_async_patterns(&aggregated_signals);

        // Count patterns before/after filter
        let patterns_before = self.count_patterns_before_filter(&DetectedPatterns {
            soft_delete: &soft_delete,
            error_handling: &error_handling,
            naming: &naming,
            resource_management: &resource_management,
            validation: &validation,
            test_idioms: &test_idioms,
            import_patterns: &import_patterns,
            type_coverage: &type_coverage,
            api_conventions: &api_conventions,
            async_patterns: &async_patterns,
        });

        // Apply confidence filter to all pattern types.
        // Note: ImportPattern has hardcoded confidence 1.0, so filtering is a no-op
        // by design (presence = confidence). Included for consistency.
        // Note: NamingPattern uses consistency_score as confidence. This means
        // inconsistent naming (low score) gets filtered out. This is a known
        // limitation — low consistency IS a valid finding worth reporting.
        // TODO: Add separate detection_confidence field to NamingPattern.
        // Note: TypeCoveragePattern uses coverage_overall as confidence. Low
        // coverage gets filtered, which may hide useful "low coverage" findings.
        let soft_delete = self.filter_by_confidence(soft_delete);
        let error_handling = self.filter_by_confidence(error_handling);
        let naming = self.filter_by_confidence(naming);
        let resource_management = self.filter_by_confidence(resource_management);
        let validation = self.filter_by_confidence(validation);
        let test_idioms = self.filter_by_confidence(test_idioms);
        let import_patterns = self.filter_by_confidence(import_patterns);
        let type_coverage = self.filter_by_confidence(type_coverage);
        let api_conventions = self.filter_by_confidence(api_conventions);
        let async_patterns = self.filter_by_confidence(async_patterns);

        let patterns_after = self.count_patterns_before_filter(&DetectedPatterns {
            soft_delete: &soft_delete,
            error_handling: &error_handling,
            naming: &naming,
            resource_management: &resource_management,
            validation: &validation,
            test_idioms: &test_idioms,
            import_patterns: &import_patterns,
            type_coverage: &type_coverage,
            api_conventions: &api_conventions,
            async_patterns: &async_patterns,
        });

        // Update patterns_by_language.
        // Languages without AST pattern handlers (detector.rs) genuinely detect 0 patterns.
        // For supported languages, use the global patterns_after count since signals are
        // aggregated globally and cannot be attributed to individual languages.
        // TODO: per-language pattern detection requires running the pipeline per language.
        let supported_pattern_languages: &[&str] =
            &["python", "typescript", "javascript", "go", "rust", "java"];
        for lang in files_by_language.keys() {
            let count = if supported_pattern_languages.contains(&lang.as_str()) {
                patterns_after
            } else {
                0
            };
            patterns_by_language.insert(lang.clone(), count);
        }

        // Build metadata
        let metadata = PatternMetadata {
            files_analyzed,
            files_skipped,
            files_partial,
            duration_ms,
            language_distribution: LanguageDistribution {
                files_by_language,
                patterns_by_language,
            },
            patterns_before_filter: patterns_before,
            patterns_after_filter: patterns_after,
            confidence_threshold: self.config.min_confidence,
        };

        // Generate constraints if enabled
        let constraints = if self.config.generate_constraints {
            generate_constraints(&DetectedPatterns {
                soft_delete: &soft_delete,
                error_handling: &error_handling,
                naming: &naming,
                resource_management: &resource_management,
                validation: &validation,
                test_idioms: &test_idioms,
                import_patterns: &import_patterns,
                type_coverage: &type_coverage,
                api_conventions: &api_conventions,
                async_patterns: &async_patterns,
            })
        } else {
            Vec::new()
        };

        // Detect conflicts
        let conflicts = self.detect_conflicts(&DetectedPatterns {
            soft_delete: &soft_delete,
            error_handling: &error_handling,
            naming: &naming,
            resource_management: &resource_management,
            validation: &validation,
            test_idioms: &test_idioms,
            import_patterns: &import_patterns,
            type_coverage: &type_coverage,
            api_conventions: &api_conventions,
            async_patterns: &async_patterns,
        });

        Ok(PatternReport {
            metadata,
            soft_delete,
            error_handling,
            naming,
            resource_management,
            validation,
            test_idioms,
            import_patterns,
            type_coverage,
            api_conventions,
            async_patterns,
            constraints,
            conflicts,
        })
    }

    /// Collect source files to analyze
    fn collect_files(
        &self,
        path: &Path,
        lang: Option<Language>,
    ) -> TldrResult<Vec<(std::path::PathBuf, Language)>> {
        if path.is_file() {
            let file_lang = lang.or_else(|| Language::from_path(path)).ok_or_else(|| {
                TldrError::UnsupportedLanguage(
                    path.extension()
                        .map(|e| e.to_string_lossy().to_string())
                        .unwrap_or_else(|| "unknown".to_string()),
                )
            })?;
            return Ok(vec![(path.to_path_buf(), file_lang)]);
        }

        let mut files = Vec::new();

        // Use get_file_tree to collect files with ignore support
        let tree = get_file_tree(path, None, true)?;
        let source_files = collect_files(&tree, path);

        for file_path in source_files {
            // fastpath-extend-non-vuln-v1: when `lang` is provided, restrict
            // to files whose extension actually matches that language (mirrors
            // the BUG-java-debt-stackoverflow-v1 fix in `quality/debt.rs`).
            //
            // Pre-fix, `lang = Some(luau)` against a C++ codebase like
            // `luau-luau` would force-parse every `.cpp`/`.h` file as luau,
            // producing pathological tree-sitter ASTs and pushing
            // `tldr patterns` past 60 s on a 122-luau-file repo (BEFORE
            // measurement: timeout >60s; AFTER: <2s).
            let detected = Language::from_path(&file_path);
            let file_lang = match (lang, detected) {
                // User specified language and file matches: use it.
                (Some(forced), Some(d)) if d == forced => forced,
                // User specified language but extension is a different known
                // language: skip (don't force-parse a `.cpp` as luau).
                (Some(_), Some(_)) => continue,
                // User specified language and extension is unknown
                // (e.g. `dictionary.txt`, `lua_dict.txt`): SKIP. The
                // alternative would be to force-parse the file under the
                // override grammar, which on real-world Lua repos
                // (`lua-lsp/meta/spell/dictionary.txt`, ~1.3 MB) hung
                // tree-sitter for >5 minutes. Restricting `--lang` to
                // matching extensions is the semantically correct
                // interpretation ("analyze <language> files only") and
                // avoids the pathological-AST timeout.
                (Some(_), None) => continue,
                // No override: only include files we can detect.
                (None, Some(d)) => d,
                (None, None) => continue,
            };

            // fastpath-extend-non-vuln-v1: enforce the central oversize
            // policy at collection time. The pattern miner reads files via
            // `std::fs::read_to_string` and dispatches them through
            // `ParserPool::parse(content, …)` (NOT through the path-based
            // `parse_file_with_lang` chokepoint that already enforces the
            // policy), so an extra check here is required for
            // auto-generated / minified artefacts to be skipped uniformly.
            if let crate::fs::oversize::SizeCheck::Oversize { .. } =
                crate::fs::oversize::check_size(&file_path)
            {
                continue;
            }

            files.push((file_path, file_lang));
        }

        Ok(files)
    }

    /// Extract pattern signals from a single file (single-pass)
    fn extract_file_signals(
        &self,
        content: &str,
        lang: Language,
        file_path: &Path,
    ) -> TldrResult<PatternSignals> {
        let tree = self.parser_pool.parse(content, lang)?;
        let detector = PatternDetector::new(lang, file_path.to_path_buf());
        Ok(detector.detect_all(&tree, content))
    }

    /// Extract partial signals from a file with parse errors (A23 mitigation)
    fn extract_partial_signals(
        &self,
        content: &str,
        lang: Language,
        file_path: &Path,
    ) -> TldrResult<PatternSignals> {
        // Use regex-based fallback detection for partially parseable files
        let detector = PatternDetector::new(lang, file_path.to_path_buf());
        Ok(detector.detect_fallback(content))
    }

    // Signal to pattern conversion methods
    fn signals_to_soft_delete(&self, signals: &PatternSignals) -> Option<SoftDeletePattern> {
        soft_delete::signals_to_pattern(signals, self.config.evidence_limit)
    }

    fn signals_to_error_handling(&self, signals: &PatternSignals) -> Option<ErrorHandlingPattern> {
        error_handling::signals_to_pattern(signals, self.config.evidence_limit)
    }

    fn signals_to_naming(&self, signals: &PatternSignals) -> Option<NamingPattern> {
        naming::signals_to_pattern(signals)
    }

    fn signals_to_resource_mgmt(
        &self,
        signals: &PatternSignals,
    ) -> Option<ResourceManagementPattern> {
        resource_mgmt::signals_to_pattern(signals, self.config.evidence_limit)
    }

    fn signals_to_validation(&self, signals: &PatternSignals) -> Option<ValidationPattern> {
        validation::signals_to_pattern(signals, self.config.evidence_limit)
    }

    fn signals_to_test_idioms(&self, signals: &PatternSignals) -> Option<TestIdiomPattern> {
        test_idioms::signals_to_pattern(signals, self.config.evidence_limit)
    }

    fn signals_to_import_patterns(&self, signals: &PatternSignals) -> Option<ImportPattern> {
        import_patterns::signals_to_pattern(signals, self.config.evidence_limit)
    }

    fn signals_to_type_coverage(&self, signals: &PatternSignals) -> Option<TypeCoveragePattern> {
        type_coverage::signals_to_pattern(signals, self.config.evidence_limit)
    }

    fn signals_to_api_conventions(&self, signals: &PatternSignals) -> Option<ApiConventionPattern> {
        api_conventions::signals_to_pattern(signals, self.config.evidence_limit)
    }

    fn signals_to_async_patterns(&self, signals: &PatternSignals) -> Option<AsyncPattern> {
        async_patterns::signals_to_pattern(signals, self.config.evidence_limit)
    }

    // Helper to filter patterns by confidence threshold
    fn filter_by_confidence<T: HasConfidence>(&self, pattern: Option<T>) -> Option<T> {
        pattern.filter(|p| p.confidence() >= self.config.min_confidence)
    }

    // Count total patterns before filter
    fn count_patterns_before_filter(&self, patterns: &DetectedPatterns<'_>) -> usize {
        let mut count = 0;
        if patterns.soft_delete.is_some() {
            count += 1;
        }
        if patterns.error_handling.is_some() {
            count += 1;
        }
        if patterns.naming.is_some() {
            count += 1;
        }
        if patterns.resource_management.is_some() {
            count += 1;
        }
        if patterns.validation.is_some() {
            count += 1;
        }
        if patterns.test_idioms.is_some() {
            count += 1;
        }
        if patterns.import_patterns.is_some() {
            count += 1;
        }
        if patterns.type_coverage.is_some() {
            count += 1;
        }
        if patterns.api_conventions.is_some() {
            count += 1;
        }
        if patterns.async_patterns.is_some() {
            count += 1;
        }
        count
    }

    // Detect conflicts between patterns
    fn detect_conflicts(&self, patterns: &DetectedPatterns<'_>) -> Vec<String> {
        let mut conflicts = Vec::new();

        // Check for import pattern conflicts
        if let Some(imports) = patterns.import_patterns {
            if imports.grouping_style == crate::types::ImportGrouping::Ungrouped {
                conflicts.push(
                    "Inconsistent import grouping: no clear ordering pattern detected".to_string(),
                );
            }
            if imports.absolute_vs_relative == crate::types::ImportStyle::Mixed {
                conflicts.push(
                    "Mixed import styles: some files use absolute imports, others use relative"
                        .to_string(),
                );
            }
        }

        conflicts
    }
}

/// Trait for patterns with a confidence score
pub trait HasConfidence {
    /// Returns the confidence score for this pattern in the range [0.0, 1.0].
    fn confidence(&self) -> f64;
}

impl HasConfidence for SoftDeletePattern {
    fn confidence(&self) -> f64 {
        self.confidence
    }
}

impl HasConfidence for ErrorHandlingPattern {
    fn confidence(&self) -> f64 {
        self.confidence
    }
}

impl HasConfidence for NamingPattern {
    fn confidence(&self) -> f64 {
        self.consistency_score
    }
}

impl HasConfidence for ResourceManagementPattern {
    fn confidence(&self) -> f64 {
        self.confidence
    }
}

impl HasConfidence for ValidationPattern {
    fn confidence(&self) -> f64 {
        self.confidence
    }
}

impl HasConfidence for TestIdiomPattern {
    fn confidence(&self) -> f64 {
        self.confidence
    }
}

impl HasConfidence for ImportPattern {
    fn confidence(&self) -> f64 {
        1.0 // Import patterns always have full confidence once detected
    }
}

impl HasConfidence for TypeCoveragePattern {
    fn confidence(&self) -> f64 {
        self.coverage_overall
    }
}

impl HasConfidence for ApiConventionPattern {
    fn confidence(&self) -> f64 {
        self.confidence
    }
}

impl HasConfidence for AsyncPattern {
    fn confidence(&self) -> f64 {
        self.concurrency_confidence
    }
}

/// Detect patterns from a path (convenience function)
pub fn detect_patterns(path: &Path, lang: Option<Language>) -> TldrResult<PatternReport> {
    let miner = PatternMiner::new(PatternConfig::default());
    miner.mine_patterns(path, lang)
}

/// Detect patterns with custom configuration
pub fn detect_patterns_with_config(
    path: &Path,
    lang: Option<Language>,
    config: PatternConfig,
) -> TldrResult<PatternReport> {
    let miner = PatternMiner::new(config);
    miner.mine_patterns(path, lang)
}

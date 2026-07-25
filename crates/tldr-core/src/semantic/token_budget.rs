//! Token-accurate input budgets for the embedding pipeline (TLDR-9bxa.2).
//!
//! Replaces the silent 4000-character truncation with real model-tokenizer
//! accounting. Every embedding input is checked against the model's true token
//! budget (`max_context` minus special tokens); oversized inputs are REPORTED
//! explicitly (no silent shortening) and left for fastembed to truncate at the
//! token level — which, for the 512-context models, is byte-for-byte what it
//! already did, so in-budget vectors are unchanged.
//!
//! The tokenizer is loaded from **fastembed's on-disk cache** (`<cache>/
//! models--<repo>/snapshots/<sha>/tokenizer.json`) — i.e. the exact tokenizer,
//! at the exact revision, that fastembed uses for the model. This avoids any
//! drift between our token counts and fastembed's tokenization (review #4) and
//! needs no network (`http`) feature.
//!
//! Non-goals (separate epics): AST recursive splitting (TLDR-9bxa.3) and
//! fixed-shape ONNX execution (TLDR-9bxa.5).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokenizers::Tokenizer;

use crate::semantic::types::EmbeddingModel;

/// Input-budget schema version. Bumped whenever the budgeting recipe changes in
/// a way that alters which tokens get embedded, so content-addressed caches and
/// persisted stores rebuild. (`2` = the move from char-truncation to tokenizer
/// budgets in TLDR-9bxa.2.)
pub const INPUT_BUDGET_SCHEMA_VERSION: u32 = 2;

/// Tokens reserved for model special tokens (CLS/SEP) within the context window,
/// subtracted from `max_context` to get the usable input budget.
const RESERVED_SPECIAL_TOKENS: usize = 2;

/// Outcome of checking one embedding input against the model token budget.
/// Serialized into the build-metrics report (TLDR-9bxa.1) for diagnostics.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TokenCheck {
    /// Token count of the original (pre-truncation) input.
    pub original_tokens: usize,
    /// Usable token budget for this model (`max_context` − special tokens).
    pub budget: usize,
    /// `true` if `original_tokens > budget` — fastembed will truncate this input
    /// to the model context. Reported, never silent.
    pub truncated: bool,
    /// Tokens effectively embedded (`min(original_tokens, budget)`).
    pub embedded_tokens: usize,
}

/// A loaded model tokenizer + its usable token budget. Build one per `Embedder`
/// and reuse across all inputs.
pub struct TokenBudget {
    tokenizer: Tokenizer,
    budget: usize,
}

impl TokenBudget {
    /// Construct from an already-loaded tokenizer (e.g., in tests).
    pub fn from_tokenizer(tokenizer: Tokenizer, model: EmbeddingModel) -> Self {
        let budget = model.max_context().saturating_sub(RESERVED_SPECIAL_TOKENS);
        Self { tokenizer, budget }
    }

    /// Load the tokenizer fastembed downloaded for `model` from `cache_dir`
    /// (the exact revision fastembed uses) and compute the budget.
    pub fn for_model_in_cache(
        model: EmbeddingModel,
        cache_dir: &Path,
    ) -> Result<Self, TokenBudgetError> {
        let tokenizer = load_cached_tokenizer(model.model_name(), cache_dir)
            .map_err(TokenBudgetError::TokenizerLoad)?;
        Ok(Self::from_tokenizer(tokenizer, model))
    }

    /// Exact token count of `text` under this model's tokenizer.
    pub fn token_count(&self, text: &str) -> usize {
        // `add_special_tokens = false`: count the text's own tokens, matching
        // how we reason about the input budget (special tokens are reserved
        // separately via RESERVED_SPECIAL_TOKENS).
        self.tokenizer
            .encode(text, false)
            .map(|enc| enc.get_ids().len())
            .unwrap_or(0)
    }

    /// Usable token budget (`max_context` − reserved special tokens).
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Check `text` against the budget (report only — does not mutate the text;
    /// fastembed performs the actual token-level truncation for oversized
    /// inputs). Deterministic: same input + model → same `TokenCheck`.
    pub fn check(&self, text: &str) -> TokenCheck {
        let original_tokens = self.token_count(text);
        let truncated = original_tokens > self.budget;
        TokenCheck {
            original_tokens,
            budget: self.budget,
            truncated,
            embedded_tokens: original_tokens.min(self.budget),
        }
    }
}

/// Locate `<cache_dir>/models--<repo>/snapshots/<sha>/tokenizer.json` — the
/// exact tokenizer fastembed downloaded for `repo` at its pinned revision — and
/// load it. Case-insensitive on the model dir to tolerate HF-hub casing.
fn load_cached_tokenizer(repo: &str, cache_dir: &Path) -> Result<Tokenizer, String> {
    let repo_dashed = format!("models--{}", repo.replace('/', "--"));
    let repo_lc = repo_dashed.to_lowercase();

    let model_dir: PathBuf = std::fs::read_dir(cache_dir)
        .map_err(|e| format!("read cache dir {}: {e}", cache_dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_lowercase() == repo_lc)
                .unwrap_or(false)
        })
        .ok_or_else(|| {
            format!(
                "no cached model dir matching {repo_dashed} in {} (run an embed once to download the model)",
                cache_dir.display()
            )
        })?;

    let snapshots = model_dir.join("snapshots");
    let tokenizer_path: PathBuf = std::fs::read_dir(&snapshots)
        .map_err(|e| format!("read snapshots {}: {e}", snapshots.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path().join("tokenizer.json"))
        .find(|p| p.is_file())
        .ok_or_else(|| format!("no tokenizer.json under {}", snapshots.display()))?;

    Tokenizer::from_file(&tokenizer_path)
        .map_err(|e| format!("load {}: {e}", tokenizer_path.display()))
}

/// Corpus-wide token-budget statistics, accumulated across all inputs an
/// `Embedder` embeds and surfaced via the TLDR-9bxa.1 metrics report.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenStats {
    /// Number of inputs checked.
    pub inputs_checked: usize,
    /// Number of inputs that exceeded the budget (will be truncated by fastembed).
    pub oversized: usize,
    /// Sum of original token counts across all inputs.
    pub total_original_tokens: usize,
    /// Sum of embedded token counts (post-budget) across all inputs.
    pub total_embedded_tokens: usize,
    /// Largest original token count observed (the worst single oversize).
    pub max_original_tokens: usize,
    /// The model's usable budget these checks ran against.
    pub budget: usize,
    /// `true` if the tokenizer could not be loaded, so checks were SKIPPED
    /// (distinguishes "0 oversized" from "couldn't check"). Review #3.
    pub unavailable: bool,
}

impl TokenStats {
    /// Record one input's check.
    pub fn record(&mut self, c: TokenCheck) {
        self.inputs_checked += 1;
        self.total_original_tokens += c.original_tokens;
        self.total_embedded_tokens += c.embedded_tokens;
        if c.original_tokens > self.max_original_tokens {
            self.max_original_tokens = c.original_tokens;
        }
        if c.truncated {
            self.oversized += 1;
        }
        self.budget = c.budget;
    }

    /// Whether any input was oversized.
    pub fn had_oversized(&self) -> bool {
        self.oversized > 0
    }

    /// Mark the tokenizer as unavailable (checks skipped) — review #3.
    pub fn mark_unavailable(&mut self) {
        self.unavailable = true;
    }
}

/// Errors from loading/using a token budget.
#[derive(Debug)]
pub enum TokenBudgetError {
    /// Tokenizer could not be loaded from fastembed's cache.
    TokenizerLoad(String),
}

impl std::fmt::Display for TokenBudgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TokenizerLoad(e) => write!(f, "tokenizer load failed: {e}"),
        }
    }
}

impl std::error::Error for TokenBudgetError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// fastembed's cache root as tldr configures it (where models + tokenizers
    /// land after `Embedder::new`).
    fn fastembed_cache() -> PathBuf {
        dirs::cache_dir()
            .expect("cache dir")
            .join("tldr")
            .join("fastembed")
    }

    #[test]
    fn token_stats_record_accumulates() {
        let mut s = TokenStats::default();
        s.record(TokenCheck { original_tokens: 10, budget: 512, truncated: false, embedded_tokens: 10 });
        s.record(TokenCheck { original_tokens: 600, budget: 512, truncated: true,  embedded_tokens: 512 });
        s.record(TokenCheck { original_tokens: 5,   budget: 512, truncated: false, embedded_tokens: 5 });
        assert_eq!(s.inputs_checked, 3);
        assert_eq!(s.oversized, 1);
        assert_eq!(s.total_original_tokens, 615);
        assert_eq!(s.total_embedded_tokens, 527);
        assert_eq!(s.max_original_tokens, 600);
        assert!(s.had_oversized());
        assert_eq!(s.budget, 512);
        assert!(!s.unavailable);
    }

    #[test]
    fn token_stats_unavailable_is_distinct_from_empty() {
        let mut s = TokenStats::default();
        s.mark_unavailable();
        assert!(s.unavailable);
        assert_eq!(s.inputs_checked, 0); // distinct from "checked, 0 oversized"
    }

    #[test]
    fn schema_version_bumped_from_one() {
        assert_eq!(INPUT_BUDGET_SCHEMA_VERSION, 2);
    }

    /// All five models' budgets are derivable from `max_context` without a
    /// tokenizer; the budget arithmetic is what matters here.
    #[test]
    fn per_model_budget_arithmetic() {
        let cases = [
            (EmbeddingModel::ArcticXS, 512 - RESERVED_SPECIAL_TOKENS),
            (EmbeddingModel::ArcticS, 512 - RESERVED_SPECIAL_TOKENS),
            (EmbeddingModel::ArcticM, 512 - RESERVED_SPECIAL_TOKENS),
            (EmbeddingModel::ArcticMLong, 8192 - RESERVED_SPECIAL_TOKENS),
            (EmbeddingModel::ArcticL, 512 - RESERVED_SPECIAL_TOKENS),
        ];
        for (model, expected) in cases {
            assert_eq!(
                model.max_context().saturating_sub(RESERVED_SPECIAL_TOKENS),
                expected,
                "budget mismatch for {model:?}"
            );
        }
    }

    #[test]
    #[ignore = "needs the Arctic-XS model downloaded (run an embed once)"]
    fn token_count_and_check_against_cached_tokenizer() {
        let tb = TokenBudget::for_model_in_cache(EmbeddingModel::ArcticXS, &fastembed_cache())
            .expect("Arctic-XS tokenizer is in fastembed's cache");
        assert_eq!(tb.budget(), 510);

        let short = "fn add(a: i32, b: i32) -> i32 { a + b }";
        let c = tb.check(short);
        assert!(c.original_tokens > 0);
        assert!(!c.truncated);
        assert_eq!(c.embedded_tokens, c.original_tokens);

        // Oversized: a token-dense input well over 510 tokens.
        let huge = "very_long_identifier_name ".repeat(4000);
        let c2 = tb.check(&huge);
        assert!(c2.original_tokens > tb.budget());
        assert!(c2.truncated);
        assert_eq!(c2.embedded_tokens, tb.budget());
    }

    #[test]
    #[ignore = "needs at least one Arctic model downloaded (run an embed once)"]
    fn cached_models_load_with_correct_budget() {
        // Verify the cache loader works across models; skip any not downloaded
        // on this machine (e.g. ArcticMLong if never embedded). At least one
        // must load so the test isn't a no-op.
        let cache = fastembed_cache();
        let models = [
            EmbeddingModel::ArcticXS,
            EmbeddingModel::ArcticS,
            EmbeddingModel::ArcticM,
            EmbeddingModel::ArcticL,
            EmbeddingModel::ArcticMLong,
        ];
        let mut loaded = 0;
        for model in models {
            let Ok(tb) = TokenBudget::for_model_in_cache(model, &cache) else {
                continue; // not downloaded here — skip, don't fail
            };
            loaded += 1;
            assert_eq!(
                tb.budget(),
                model.max_context() - RESERVED_SPECIAL_TOKENS,
                "budget for {model:?}"
            );
        }
        assert!(loaded > 0, "no Arctic model found in fastembed cache {cache:?}");
    }

    /// Unicode safety: the chunker no longer slices strings at byte offsets
    /// (the old `[..N]` panicked on multi-byte chars). Token counting must
    /// handle multi-byte input without panic.
    #[test]
    #[ignore = "needs the Arctic-XS model downloaded"]
    fn unicode_input_does_not_panic() {
        let tb = TokenBudget::for_model_in_cache(EmbeddingModel::ArcticXS, &fastembed_cache())
            .expect("tokenizer cached");
        // CJK + emoji + accented Latin — all multi-byte.
        let uni = "世界 🌍 café naïve\nfn 你好() -> 日本語 { \"üñîçödé\" }";
        let c = tb.check(uni);
        assert!(c.original_tokens > 0);
        assert!(!c.truncated);
    }

    /// Load failure is surfaced (not silent): a non-existent cache dir errors.
    #[test]
    fn missing_cache_dir_errors_clearly() {
        let err = TokenBudget::for_model_in_cache(
            EmbeddingModel::ArcticXS,
            Path::new("/nonexistent/tldr-fastembed-cache-9bxa2"),
        )
        .err()
        .expect("missing cache dir should error");
        let msg = format!("{err}");
        assert!(msg.contains("read cache dir") || msg.contains("no cached model dir"));
    }
}

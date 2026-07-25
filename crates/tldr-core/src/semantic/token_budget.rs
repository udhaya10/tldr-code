//! Token-accurate input budgets for the embedding pipeline (TLDR-9bxa.2).
//!
//! Replaces the silent 4000-character truncation with real model-tokenizer
//! accounting. Every embedding input is checked against the model's true token
//! budget (FastEmbed's configured truncation limit); oversized inputs are REPORTED
//! explicitly (no silent shortening) and left for fastembed to truncate at the
//! token level — which, for the 512-context models, is byte-for-byte what it
//! already did, so in-budget vectors are unchanged.
//!
//! Accounting clones the tokenizer configured inside FastEmbed itself, retaining
//! its effective truncation limit while disabling truncation and padding on the
//! clone only. This observes original lengths without changing inference.
//!
//! Non-goals (separate epics): AST recursive splitting (TLDR-9bxa.3) and
//! fixed-shape ONNX execution (TLDR-9bxa.5).

use serde::{Deserialize, Serialize};
use tokenizers::Tokenizer;

/// Input-budget diagnostic schema version. Bump whenever accounting semantics
/// change. Embedding-content cache invalidation is controlled separately by the
/// raw/enriched recipe tag because these checks are report-only and do not alter
/// vectors. (`3` = FastEmbed's configured tokenizer and effective limit.)
pub const INPUT_BUDGET_SCHEMA_VERSION: u32 = 3;

/// Outcome of checking one embedding input against the model token budget.
/// Serialized into the build-metrics report (TLDR-9bxa.1) for diagnostics.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TokenCheck {
    /// Token count of the original (pre-truncation) input.
    pub original_tokens: usize,
    /// FastEmbed's configured maximum encoded sequence length.
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
    /// Clone FastEmbed's fully configured tokenizer. The clone has truncation
    /// and padding disabled so encoding observes the original length, while the
    /// effective FastEmbed truncation limit is retained as the budget.
    pub fn from_configured_tokenizer(tokenizer: &Tokenizer) -> Result<Self, TokenBudgetError> {
        let budget = tokenizer
            .get_truncation()
            .map(|params| params.max_length)
            .ok_or(TokenBudgetError::MissingTruncation)?;
        let mut tokenizer = tokenizer.clone();
        tokenizer
            .with_truncation(None)
            .map_err(|e| TokenBudgetError::TokenizerConfig(e.to_string()))?;
        tokenizer.with_padding(None);
        Ok(Self { tokenizer, budget })
    }

    /// Exact token count of `text` under this model's tokenizer.
    pub fn token_count(&self, text: &str) -> Result<usize, TokenBudgetError> {
        self.tokenizer
            .encode(text, true)
            .map(|enc| enc.get_ids().len())
            .map_err(|e| TokenBudgetError::Encode(e.to_string()))
    }

    /// FastEmbed's configured maximum encoded sequence length.
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Check `text` against the budget (report only — does not mutate the text;
    /// fastembed performs the actual token-level truncation for oversized
    /// inputs). Deterministic: same input + model → same `TokenCheck`.
    pub fn check(&self, text: &str) -> Result<TokenCheck, TokenBudgetError> {
        let original_tokens = self.token_count(text)?;
        let truncated = original_tokens > self.budget;
        Ok(TokenCheck {
            original_tokens,
            budget: self.budget,
            truncated,
            embedded_tokens: original_tokens.min(self.budget),
        })
    }
}

/// Corpus-wide token-budget statistics, accumulated across all inputs an
/// `Embedder` embeds and surfaced via the TLDR-9bxa.1 metrics report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenCheckStatus {
    /// No inputs existed or accounting has not run.
    #[default]
    NotChecked,
    /// At least one input was successfully checked.
    Checked,
    /// Tokenizer configuration or encoding failed.
    Unavailable,
}

/// Aggregate token accounting and its explicit availability state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TokenStats {
    /// Whether accounting ran successfully, did not run, or failed.
    #[serde(default)]
    pub status: TokenCheckStatus,
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
}

impl TokenStats {
    /// Record one input's check.
    pub fn record(&mut self, c: TokenCheck) {
        // Once any input could not be checked, the aggregate is unavailable even
        // if a later input succeeds; partial statistics must not masquerade as a
        // complete corpus result.
        if self.status == TokenCheckStatus::NotChecked {
            self.status = TokenCheckStatus::Checked;
        }
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
        self.status = TokenCheckStatus::Unavailable;
    }
}

/// Errors from loading/using a token budget.
#[derive(Debug)]
pub enum TokenBudgetError {
    /// FastEmbed did not configure truncation, so no effective limit is known.
    MissingTruncation,
    /// The tokenizer clone could not be reconfigured for accounting.
    TokenizerConfig(String),
    /// Encoding an input failed.
    Encode(String),
}

impl std::fmt::Display for TokenBudgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingTruncation => write!(f, "FastEmbed tokenizer has no truncation limit"),
            Self::TokenizerConfig(e) => write!(f, "tokenizer configuration failed: {e}"),
            Self::Encode(e) => write!(f, "tokenizer encode failed: {e}"),
        }
    }
}

impl std::error::Error for TokenBudgetError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_stats_record_accumulates() {
        let mut s = TokenStats::default();
        s.record(TokenCheck {
            original_tokens: 10,
            budget: 512,
            truncated: false,
            embedded_tokens: 10,
        });
        s.record(TokenCheck {
            original_tokens: 600,
            budget: 512,
            truncated: true,
            embedded_tokens: 512,
        });
        s.record(TokenCheck {
            original_tokens: 5,
            budget: 512,
            truncated: false,
            embedded_tokens: 5,
        });
        assert_eq!(s.inputs_checked, 3);
        assert_eq!(s.oversized, 1);
        assert_eq!(s.total_original_tokens, 615);
        assert_eq!(s.total_embedded_tokens, 527);
        assert_eq!(s.max_original_tokens, 600);
        assert!(s.had_oversized());
        assert_eq!(s.budget, 512);
        assert_eq!(s.status, TokenCheckStatus::Checked);
    }

    #[test]
    fn token_stats_unavailable_is_distinct_from_empty() {
        let mut s = TokenStats::default();
        s.mark_unavailable();
        assert_eq!(s.status, TokenCheckStatus::Unavailable);
        assert_eq!(s.inputs_checked, 0); // distinct from "checked, 0 oversized"
    }

    #[test]
    fn unavailable_status_is_sticky_after_later_success() {
        let mut stats = TokenStats::default();
        stats.mark_unavailable();
        stats.record(TokenCheck {
            original_tokens: 3,
            budget: 512,
            truncated: false,
            embedded_tokens: 3,
        });
        assert_eq!(stats.status, TokenCheckStatus::Unavailable);
    }

    #[test]
    fn schema_version_tracks_configured_fastembed_tokenizer() {
        assert_eq!(INPUT_BUDGET_SCHEMA_VERSION, 3);
    }

    #[test]
    fn status_defaults_for_old_reports_and_empty_corpus() {
        let stats: TokenStats = serde_json::from_str(r#"{"inputs_checked":0}"#).unwrap();
        assert_eq!(stats.status, TokenCheckStatus::NotChecked);
    }

    #[test]
    fn missing_effective_limit_is_explicitly_unavailable() {
        let tokenizer = Tokenizer::new(tokenizers::models::bpe::BPE::default());
        assert!(matches!(
            TokenBudget::from_configured_tokenizer(&tokenizer),
            Err(TokenBudgetError::MissingTruncation)
        ));
    }

    #[test]
    fn configured_limit_is_copied_without_mutating_inference_tokenizer() {
        let mut tokenizer = Tokenizer::new(tokenizers::models::bpe::BPE::default());
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: 37,
                ..Default::default()
            }))
            .unwrap();

        let budget = TokenBudget::from_configured_tokenizer(&tokenizer).unwrap();
        assert_eq!(budget.budget(), 37);
        assert_eq!(tokenizer.get_truncation().unwrap().max_length, 37);
    }
}

//! Token-accurate input budgets for the embedding pipeline (TLDR-9bxa.2).
//!
//! Replaces the silent 4000-character truncation with real model-tokenizer
//! accounting. Every embedding input is checked against the model's true token
//! budget (`max_context` minus special tokens); oversized inputs are REPORTED
//! explicitly (no silent shortening) and left for fastembed to truncate at the
//! token level — which, for the 512-context models, is byte-for-byte what it
//! already did, so in-budget vectors are unchanged.
//!
//! Non-goals (separate epics): AST recursive splitting (TLDR-9bxa.3) and
//! fixed-shape ONNX execution (TLDR-9bxa.5).

use serde::{Deserialize, Serialize};
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
    /// Load the tokenizer for `model` (HuggingFace, cached after first download)
    /// and compute the usable budget.
    pub fn for_model(model: EmbeddingModel) -> Result<Self, TokenBudgetError> {
        let tokenizer = Tokenizer::from_pretrained(model.model_name(), None)
            .map_err(TokenBudgetError::TokenizerLoad)?;
        let budget = model.max_context().saturating_sub(RESERVED_SPECIAL_TOKENS);
        Ok(Self { tokenizer, budget })
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

/// Corpus-wide token-budget statistics, accumulated across all inputs of a
/// build and surfaced via the TLDR-9bxa.1 metrics report.
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
}

/// Errors from loading/using a token budget.
#[derive(Debug)]
pub enum TokenBudgetError {
    /// Tokenizer could not be loaded (e.g., network/HF hub failure).
    TokenizerLoad(tokenizers::Error),
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

    /// Token-counting and budget math are pure and need no model; only
    /// `TokenBudget::for_model` downloads a tokenizer, so the heavy tests are
    /// `#[ignore]` (see the workspace's other model-gated embedder tests).

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
    }

    #[test]
    fn token_stats_no_oversized() {
        let mut s = TokenStats::default();
        s.record(TokenCheck { original_tokens: 100, budget: 512, truncated: false, embedded_tokens: 100 });
        assert!(!s.had_oversized());
        assert_eq!(s.oversized, 0);
    }

    #[test]
    fn schema_version_bumped_from_one() {
        // .2 moved from char-truncation to tokenizer budgets -> version 2.
        assert_eq!(INPUT_BUDGET_SCHEMA_VERSION, 2);
    }

    #[test]
    #[ignore = "downloads the Arctic-XS tokenizer (~small) from HF; run on demand"]
    fn token_count_and_check_against_real_tokenizer() {
        let tb = TokenBudget::for_model(EmbeddingModel::ArcticXS).expect("tokenizer loads");
        // ArcticXS context is 512 -> budget = 512 - 2 = 510.
        assert_eq!(tb.budget(), 510);
        let short = "fn add(a: i32, b: i32) -> i32 { a + b }";
        let c = tb.check(short);
        assert!(c.original_tokens > 0);
        assert!(!c.truncated);
        assert_eq!(c.embedded_tokens, c.original_tokens);
        // An obviously-oversized input: repeat a long identifier many times.
        let huge = "very_long_identifier_name ".repeat(4000);
        let c2 = tb.check(&huge);
        assert!(c2.original_tokens > tb.budget());
        assert!(c2.truncated);
        assert_eq!(c2.embedded_tokens, tb.budget());
    }

    #[test]
    #[ignore = "downloads the Arctic-M-Long tokenizer; run on demand"]
    fn long_model_has_larger_budget() {
        let tb = TokenBudget::for_model(EmbeddingModel::ArcticMLong).expect("tokenizer loads");
        // ArcticMLong context is 8192 -> budget = 8190.
        assert_eq!(tb.budget(), 8190);
    }
}

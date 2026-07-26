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
//! AST recursive splitting is implemented separately by TLDR-9bxa.3. The exact
//! token tensors exposed here are also the tokenizer boundary for the
//! fixed-shape ONNX work in TLDR-9bxa.5.

use serde::{Deserialize, Serialize};
use tokenizers::Tokenizer;

use super::fixed_shape::{FixedShapePlanner, TokenizedInput};

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
    pad_token_id: Option<u32>,
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
        let pad_token_id = tokenizer.get_padding().map(|params| params.pad_id);
        let mut tokenizer = tokenizer.clone();
        tokenizer
            .with_truncation(None)
            .map_err(|e| TokenBudgetError::TokenizerConfig(e.to_string()))?;
        tokenizer.with_padding(None);
        Ok(Self {
            tokenizer,
            budget,
            pad_token_id,
        })
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

    /// Numeric pad token configured by FastEmbed before padding was disabled on
    /// the accounting clone.
    pub fn pad_token_id(&self) -> Result<i64, TokenBudgetError> {
        self.pad_token_id
            .map(i64::from)
            .ok_or(TokenBudgetError::MissingPadding)
    }

    /// Encode one input exactly once into the token tensors consumed by the
    /// fixed-shape planner. Oversized inputs are rejected instead of silently
    /// truncating; structural splitting must make them fit first.
    pub fn tokenize_fixed_shape(
        &self,
        request_index: usize,
        text: &str,
    ) -> Result<TokenizedInput, TokenBudgetError> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| TokenBudgetError::Encode(e.to_string()))?;
        self.fixed_shape_input(request_index, encoding)
    }

    /// Encode a document set once using the tokenizer's parallel batch path.
    ///
    /// This preserves the exact input order and numeric tensors returned by
    /// [`Self::tokenize_fixed_shape`] while avoiding one serial tokenizer call
    /// per document during a cold bulk build.
    pub fn tokenize_fixed_shape_batch(
        &self,
        indexed: &[(usize, &str)],
    ) -> Result<Vec<TokenizedInput>, TokenBudgetError> {
        // `encode_batch` parallelizes within each window. Keeping the window
        // bounded avoids temporarily retaining a second corpus-sized set of
        // tokenizer encodings before the fixed tensor inputs are constructed.
        const TOKENIZER_WINDOW: usize = 1_024;
        let mut tokenized = Vec::with_capacity(indexed.len());
        for window in indexed.chunks(TOKENIZER_WINDOW) {
            let encodings = self
                .tokenizer
                .encode_batch(
                    window.iter().map(|(_, text)| *text).collect::<Vec<_>>(),
                    true,
                )
                .map_err(|error| TokenBudgetError::Encode(error.to_string()))?;
            for (&(request_index, _), encoding) in window.iter().zip(encodings) {
                tokenized.push(self.fixed_shape_input(request_index, encoding)?);
            }
        }
        Ok(tokenized)
    }

    fn fixed_shape_input(
        &self,
        request_index: usize,
        encoding: tokenizers::Encoding,
    ) -> Result<TokenizedInput, TokenBudgetError> {
        let tokens = encoding.get_ids().len();
        if tokens > self.budget {
            return Err(TokenBudgetError::InputExceedsBudget {
                tokens,
                budget: self.budget,
            });
        }
        Ok(TokenizedInput {
            request_index,
            input_ids: encoding.get_ids().iter().copied().map(i64::from).collect(),
            attention_mask: encoding
                .get_attention_mask()
                .iter()
                .copied()
                .map(i64::from)
                .collect(),
            token_type_ids: Some(
                encoding
                    .get_type_ids()
                    .iter()
                    .copied()
                    .map(i64::from)
                    .collect(),
            ),
        })
    }

    /// Build a fixed-shape planner from this exact configured tokenizer.
    ///
    /// `dummy_text` is tokenized normally so partial batches contain valid,
    /// attended model inputs rather than fully masked synthetic rows.
    pub fn fixed_shape_planner(
        &self,
        dummy_text: &str,
    ) -> Result<FixedShapePlanner, TokenBudgetError> {
        let dummy = self.tokenize_fixed_shape(usize::MAX, dummy_text)?;
        FixedShapePlanner::new(self.pad_token_id()?, dummy)
            .map_err(|error| TokenBudgetError::FixedShape(error.to_string()))
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

    /// Return gap-free UTF-8-safe byte windows whose exact encoded count,
    /// including special tokens, is at most `max_tokens`.
    pub fn byte_windows(
        &self,
        text: &str,
        max_tokens: usize,
        overlap_tokens: usize,
    ) -> Result<Vec<(usize, usize)>, TokenBudgetError> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        if max_tokens == 0 {
            return Err(TokenBudgetError::ImpossibleBudget(max_tokens));
        }
        if self.token_count("")? > max_tokens {
            return Err(TokenBudgetError::ImpossibleBudget(max_tokens));
        }
        let mut windows = Vec::new();
        let boundaries: Vec<usize> = text
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
            .collect();
        let mut start_index = 0;
        while boundaries[start_index] < text.len() {
            let start = boundaries[start_index];
            let suffix = &text[start..];
            if self.token_count(suffix)? <= max_tokens {
                windows.push((start, text.len()));
                break;
            }

            // Derive candidates from the tokenizer's own offsets rather than
            // binary-searching character prefixes. BPE/WordPiece token counts
            // are not monotonic as a prefix grows, so binary search can reject
            // a later merged token that fits. Validate candidate prefixes with
            // an exact re-encode because cutting can retokenize the last token.
            let encoding = self
                .tokenizer
                .encode(suffix, true)
                .map_err(|error| TokenBudgetError::Encode(error.to_string()))?;
            let mut candidate_ends: Vec<usize> = encoding
                .get_offsets()
                .iter()
                .filter_map(|&(_, end)| (end > 0).then_some(start + end))
                .collect();
            candidate_ends.sort_unstable();
            candidate_ends.dedup();
            let content_slots = max_tokens.saturating_sub(self.token_count("")?);
            candidate_ends.truncate(content_slots.max(1));
            let mut end_index = candidate_ends
                .into_iter()
                .rev()
                .find(|&end| {
                    self.token_count(&text[start..end])
                        .is_ok_and(|n| n <= max_tokens)
                })
                .and_then(|end| boundaries.binary_search(&end).ok())
                .unwrap_or(start_index);

            // A tokenizer may expose no useful offset. Try exactly one UTF-8
            // scalar as the bounded progress fallback. Any longer tokenizer
            // merge that can fit is already represented by an encoding offset
            // above; scanning every remaining scalar here turns giant literals
            // into quadratic repeated tokenization.
            if end_index <= start_index {
                let next = start_index + 1;
                if next < boundaries.len()
                    && self.token_count(&text[start..boundaries[next]])? <= max_tokens
                {
                    end_index = next;
                }
            }
            if end_index <= start_index {
                return Err(TokenBudgetError::ImpossibleBudget(max_tokens));
            }
            let end = boundaries[end_index];
            windows.push((start, end));
            if end == text.len() {
                break;
            }
            // Derive overlap from one exact encoding of the selected prefix.
            // Re-encoding every preceding character is quadratic for giant
            // literals that tokenize as one unit. Clamp below the window size
            // so even pathological callers always advance.
            let overlap = overlap_tokens.min(max_tokens.saturating_sub(1));
            let next = if overlap == 0 {
                end_index
            } else {
                let prefix = self
                    .tokenizer
                    .encode(&text[start..end], true)
                    .map_err(|error| TokenBudgetError::Encode(error.to_string()))?;
                prefix
                    .get_offsets()
                    .iter()
                    .filter(|&&(offset_start, offset_end)| {
                        offset_end > offset_start && offset_end <= end - start
                    })
                    .rev()
                    .take(overlap)
                    .last()
                    .and_then(|&(offset_start, _)| {
                        boundaries.binary_search(&(start + offset_start)).ok()
                    })
                    .unwrap_or(end_index)
            };
            start_index = next.max(start_index + 1).min(end_index);
        }
        Ok(windows)
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

    /// Merge one bounded window into the build-wide aggregate.
    pub fn merge(&mut self, other: &Self) {
        self.status = match (self.status, other.status) {
            (TokenCheckStatus::Unavailable, _) | (_, TokenCheckStatus::Unavailable) => {
                TokenCheckStatus::Unavailable
            }
            (TokenCheckStatus::Checked, _) | (_, TokenCheckStatus::Checked) => {
                TokenCheckStatus::Checked
            }
            _ => TokenCheckStatus::NotChecked,
        };
        self.inputs_checked += other.inputs_checked;
        self.oversized += other.oversized;
        self.total_original_tokens += other.total_original_tokens;
        self.total_embedded_tokens += other.total_embedded_tokens;
        self.max_original_tokens = self.max_original_tokens.max(other.max_original_tokens);
        if other.budget != 0 {
            self.budget = other.budget;
        }
    }
}

/// Errors from loading/using a token budget.
#[derive(Debug)]
pub enum TokenBudgetError {
    /// FastEmbed did not configure truncation, so no effective limit is known.
    MissingTruncation,
    /// FastEmbed did not configure a numeric pad token.
    MissingPadding,
    /// The tokenizer clone could not be reconfigured for accounting.
    TokenizerConfig(String),
    /// Encoding an input failed.
    Encode(String),
    /// Structural splitting failed to fit an input into the model context.
    InputExceedsBudget {
        /// Exact encoded input length.
        tokens: usize,
        /// Configured model context limit.
        budget: usize,
    },
    /// The exact tokenizer output could not satisfy the fixed-shape contract.
    FixedShape(String),
    /// Even one non-empty UTF-8 scalar plus tokenizer special tokens cannot fit.
    ImpossibleBudget(usize),
}

impl std::fmt::Display for TokenBudgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingTruncation => write!(f, "FastEmbed tokenizer has no truncation limit"),
            Self::MissingPadding => write!(f, "FastEmbed tokenizer has no padding configuration"),
            Self::TokenizerConfig(e) => write!(f, "tokenizer configuration failed: {e}"),
            Self::Encode(e) => write!(f, "tokenizer encode failed: {e}"),
            Self::InputExceedsBudget { tokens, budget } => {
                write!(
                    f,
                    "input has {tokens} tokens, exceeding model budget {budget}"
                )
            }
            Self::FixedShape(e) => write!(f, "fixed-shape planner configuration failed: {e}"),
            Self::ImpossibleBudget(n) => write!(f, "token budget {n} cannot fit one input unit"),
        }
    }
}

impl std::error::Error for TokenBudgetError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn word_budget(limit: usize) -> TokenBudget {
        use tokenizers::models::wordlevel::WordLevel;
        use tokenizers::pre_tokenizers::whitespace::Whitespace;

        let vocab = ["[UNK]", "alpha", "beta", "世界", "🌍"]
            .into_iter()
            .enumerate()
            .map(|(index, token)| (token.to_string(), index as u32))
            .collect();
        let model = WordLevel::builder()
            .vocab(vocab)
            .unk_token("[UNK]".to_string())
            .build()
            .unwrap();
        let mut tokenizer = Tokenizer::new(model);
        tokenizer.with_pre_tokenizer(Some(Whitespace));
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: limit,
                ..Default::default()
            }))
            .unwrap();
        tokenizer.with_padding(Some(tokenizers::PaddingParams::default()));
        TokenBudget::from_configured_tokenizer(&tokenizer).unwrap()
    }

    #[test]
    fn byte_windows_are_exact_gap_free_and_unicode_safe() {
        let budget = word_budget(3);
        let text = "alpha   beta 世界 🌍 alpha";
        let windows = budget.byte_windows(text, 3, 1).unwrap();
        assert_eq!(windows.first().unwrap().0, 0);
        assert_eq!(windows.last().unwrap().1, text.len());
        for (index, &(start, end)) in windows.iter().enumerate() {
            assert!(text.is_char_boundary(start));
            assert!(text.is_char_boundary(end));
            assert!(budget.token_count(&text[start..end]).unwrap() <= 3);
            if index > 0 {
                assert!(start <= windows[index - 1].1, "windows left a byte gap");
            }
        }
    }

    #[test]
    fn byte_windows_clamp_pathological_overlap_and_advance() {
        let budget = word_budget(3);
        let text = "alpha beta 世界 🌍 alpha beta 世界 🌍";
        let windows = budget.byte_windows(text, 3, usize::MAX).unwrap();

        assert!(windows.len() > 1);
        for pair in windows.windows(2) {
            assert!(pair[1].0 > pair[0].0, "every window must advance");
            assert!(pair[1].0 <= pair[0].1, "windows must remain gap-free");
        }
    }

    #[test]
    fn byte_windows_reject_an_impossible_nonempty_budget() {
        let budget = word_budget(3);
        assert!(matches!(
            budget.byte_windows("alpha", 0, 0),
            Err(TokenBudgetError::ImpossibleBudget(0))
        ));
    }

    #[test]
    fn byte_windows_handle_non_monotonic_wordpiece_prefix_counts() {
        use tokenizers::models::wordpiece::WordPiece;

        let vocab = [
            ("[UNK]".to_string(), 0),
            ("a".to_string(), 1),
            ("##b".to_string(), 2),
            ("abc".to_string(), 3),
        ];
        let model = WordPiece::builder()
            .vocab(vocab)
            .unk_token("[UNK]".to_string())
            .build()
            .unwrap();
        let mut tokenizer = Tokenizer::new(model);
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: 1,
                ..Default::default()
            }))
            .unwrap();
        let budget = TokenBudget::from_configured_tokenizer(&tokenizer).unwrap();

        assert_eq!(budget.token_count("ab").unwrap(), 2);
        assert_eq!(budget.token_count("abc").unwrap(), 1);
        assert_eq!(budget.byte_windows("abc", 1, 0).unwrap(), [(0, 3)]);
    }

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
    fn token_stats_merge_preserves_totals_and_unavailable_status() {
        let mut left = TokenStats::default();
        left.record(TokenCheck {
            original_tokens: 4,
            budget: 3,
            truncated: true,
            embedded_tokens: 3,
        });
        let mut right = TokenStats::default();
        right.record(TokenCheck {
            original_tokens: 2,
            budget: 3,
            truncated: false,
            embedded_tokens: 2,
        });
        left.merge(&right);
        assert_eq!(left.inputs_checked, 2);
        assert_eq!(left.total_original_tokens, 6);
        assert_eq!(left.oversized, 1);
        right.mark_unavailable();
        left.merge(&right);
        assert_eq!(left.status, TokenCheckStatus::Unavailable);
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

    #[test]
    fn fixed_shape_encoding_preserves_exact_numeric_tokenizer_outputs() {
        let budget = word_budget(3);
        let input = budget.tokenize_fixed_shape(42, "alpha beta").unwrap();

        assert_eq!(budget.pad_token_id().unwrap(), 0);
        assert_eq!(input.request_index, 42);
        assert_eq!(input.input_ids, [1, 2]);
        assert_eq!(input.attention_mask, [1, 1]);
        assert_eq!(input.token_type_ids.as_deref(), Some([0, 0].as_slice()));
    }

    #[test]
    fn fixed_shape_batch_encoding_matches_single_encoding_and_preserves_order() {
        let budget = word_budget(3);
        let indexed = [(9, "alpha beta"), (3, "gamma")];
        let batch = budget.tokenize_fixed_shape_batch(&indexed).unwrap();

        assert_eq!(
            batch[0],
            budget.tokenize_fixed_shape(9, "alpha beta").unwrap()
        );
        assert_eq!(batch[1], budget.tokenize_fixed_shape(3, "gamma").unwrap());
    }

    #[test]
    fn configured_tokenizer_outputs_feed_the_fixed_shape_planner() {
        let budget = word_budget(3);
        let planner = budget.fixed_shape_planner("alpha").unwrap();
        let input = budget.tokenize_fixed_shape(7, "alpha beta").unwrap();
        let batch = planner.plan(vec![input]).unwrap().remove(0);

        assert_eq!(batch.request_indices, [7]);
        assert_eq!(&batch.input_ids[..2], [1, 2]);
        assert_eq!(batch.input_ids[2], budget.pad_token_id().unwrap());
        assert_eq!(&batch.attention_mask[..3], [1, 1, 0]);
        assert!(batch.validate().is_ok());
    }

    #[test]
    fn fixed_shape_encoding_rejects_oversized_inputs_without_truncating() {
        let budget = word_budget(1);
        assert!(matches!(
            budget.tokenize_fixed_shape(0, "alpha beta"),
            Err(TokenBudgetError::InputExceedsBudget {
                tokens: 2,
                budget: 1
            })
        ));
    }

    #[test]
    fn missing_padding_is_reported_only_when_fixed_shape_backend_needs_it() {
        let mut tokenizer = Tokenizer::new(tokenizers::models::bpe::BPE::default());
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: 37,
                ..Default::default()
            }))
            .unwrap();

        let budget = TokenBudget::from_configured_tokenizer(&tokenizer).unwrap();
        assert!(matches!(
            budget.pad_token_id(),
            Err(TokenBudgetError::MissingPadding)
        ));
    }
}

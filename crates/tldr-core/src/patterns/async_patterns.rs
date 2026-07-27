//! Async/concurrency pattern detection
//!
//! Detects async patterns:
//! - async/await keywords
//! - Go goroutines (go keyword)
//! - Tokio runtime usage
//! - Sync primitives (mutex, channel, semaphore)

use super::signals::PatternSignals;
use crate::types::AsyncPattern;

/// Convert signals to async pattern
pub fn signals_to_pattern(signals: &PatternSignals, evidence_limit: usize) -> Option<AsyncPattern> {
    let async_patterns = &signals.async_patterns;

    if !async_patterns.has_signals() {
        return None;
    }

    let concurrency_confidence = async_patterns.calculate_confidence();

    // Detect patterns
    let mut patterns = Vec::new();

    if !async_patterns.async_await.is_empty() {
        patterns.push("async_await".to_string());
    }

    if !async_patterns.goroutines.is_empty() {
        patterns.push("goroutines".to_string());
    }

    if !async_patterns.tokio_usage.is_empty() {
        patterns.push("tokio".to_string());
    }

    if !async_patterns.thread_spawns.is_empty() {
        patterns.push("thread_spawn".to_string());
    }

    // Collect sync primitives
    let mut sync_primitives: Vec<String> = async_patterns
        .sync_primitives
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    sync_primitives.sort();
    sync_primitives.dedup();

    // Collect evidence (limited)
    let mut evidence = Vec::new();
    evidence.extend(
        async_patterns
            .async_await
            .iter()
            .take(evidence_limit)
            .cloned(),
    );
    evidence.extend(
        async_patterns
            .goroutines
            .iter()
            .take(evidence_limit)
            .cloned(),
    );
    evidence.extend(
        async_patterns
            .tokio_usage
            .iter()
            .take(evidence_limit)
            .cloned(),
    );
    evidence.extend(
        async_patterns
            .sync_primitives
            .iter()
            .take(evidence_limit)
            .map(|(_, e)| e.clone()),
    );
    evidence.truncate(evidence_limit);

    Some(AsyncPattern {
        concurrency_confidence,
        patterns,
        sync_primitives,
        evidence,
    })
}

//! Own-process memory readout for `daemon status` (TLDR-yll part 2).
//!
//! Presence-based liveness (epic TLDR-cxa) means a busy machine holds its
//! resident daemon (usearch store + ONNX embedder) indefinitely — the
//! accepted trade-off. The counterweight is OBSERVABILITY: `daemon status`
//! reports current and peak RSS so a 22.7 GB build (observed live,
//! 2026-06-04) is a visible number, not a surprise in Activity Monitor.
//! The characterization of that figure and any opt-in max-RSS policy remain
//! tracked under TLDR-yll.
//!
//! TLDR-9bxa.1: the canonical, cross-platform impl now lives in
//! `tldr_core::util` (so the semantic build-metrics collector and the daemon
//! share one source of truth). These are thin delegating wrappers retained for
//! the existing call sites and tests.

/// Current resident set size of THIS process, in bytes.
pub(crate) fn current_rss_bytes() -> Option<u64> {
    tldr_core::util::current_rss_bytes()
}

/// Peak (high-water) resident set size of THIS process, in bytes.
pub(crate) fn peak_rss_bytes() -> Option<u64> {
    tldr_core::util::peak_rss_bytes()
}

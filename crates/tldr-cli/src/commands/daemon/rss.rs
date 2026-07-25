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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn current_rss_is_sane() {
        let rss = current_rss_bytes().expect("RSS readable on this platform");
        // A running test binary occupies between 1 MB and 1 TB.
        assert!(rss > 1 << 20, "RSS implausibly small: {rss}");
        assert!(rss < 1 << 40, "RSS implausibly large: {rss}");
    }

    #[cfg(unix)]
    #[test]
    fn peak_rss_at_least_current() {
        let peak = peak_rss_bytes().expect("peak RSS readable");
        if let Some(current) = current_rss_bytes() {
            // Allow slack: current is sampled after peak and pages can be
            // reclaimed, but peak must be in the same order of magnitude
            // and never absurdly below current.
            assert!(
                peak * 4 >= current,
                "peak {peak} implausibly below current {current}"
            );
        }
    }
}

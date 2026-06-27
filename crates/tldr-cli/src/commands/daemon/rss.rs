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
//! Best-effort by design: every reader returns `Option` and a failure is
//! reported as absent, never an error.

/// Current resident set size of THIS process, in bytes.
#[cfg(target_os = "macos")]
pub(crate) fn current_rss_bytes() -> Option<u64> {
    // mach task_info(MACH_TASK_BASIC_INFO) — there is no procfs on macOS.
    // libc exposes the task port as the `mach_task_self_` static.
    use libc::{mach_task_basic_info, mach_task_self_, natural_t, task_info, KERN_SUCCESS,
        MACH_TASK_BASIC_INFO};
    unsafe {
        let mut info: mach_task_basic_info = std::mem::zeroed();
        let mut count = (std::mem::size_of::<mach_task_basic_info>()
            / std::mem::size_of::<natural_t>()) as u32;
        let kr = task_info(
            mach_task_self_,
            MACH_TASK_BASIC_INFO as u32,
            &mut info as *mut _ as *mut _,
            &mut count,
        );
        (kr == KERN_SUCCESS).then(|| info.resident_size)
    }
}

/// Current resident set size of THIS process, in bytes.
#[cfg(target_os = "linux")]
pub(crate) fn current_rss_bytes() -> Option<u64> {
    // /proc/self/statm field 2 is RSS in pages.
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let rss_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    (page > 0).then(|| rss_pages * page as u64)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn current_rss_bytes() -> Option<u64> {
    None
}

/// Peak (high-water) resident set size of THIS process, in bytes.
#[cfg(unix)]
pub(crate) fn peak_rss_bytes() -> Option<u64> {
    unsafe {
        let mut ru: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut ru) != 0 {
            return None;
        }
        let raw = ru.ru_maxrss as u64;
        // ru_maxrss unit differs: bytes on macOS, kilobytes on Linux/BSD.
        Some(if cfg!(target_os = "macos") { raw } else { raw * 1024 })
    }
}

#[cfg(not(unix))]
pub(crate) fn peak_rss_bytes() -> Option<u64> {
    None
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

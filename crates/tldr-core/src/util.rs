//! Generic utility helpers shared across `tldr-core`.

/// Truncate `s` so that the returned slice contains at most `max_bytes` bytes
/// and always ends on a UTF-8 character boundary.
///
/// If `s.len() <= max_bytes`, the full string is returned unchanged. Otherwise
/// the slice is shrunk down (never up) to the largest valid char-boundary
/// position `<= max_bytes`. This is the building block for all docstring /
/// label truncation paths and replaces the historical `&s[..N]` pattern that
/// panicked on multi-byte input (e.g. CJK, accented Latin, emoji).
///
/// The `is_char_boundary` walk runs at most 3 iterations (UTF-8 sequences are
/// 1-4 bytes), so the cost is `O(1)` regardless of string length.
///
/// # Examples
///
/// ```
/// use tldr_core::util::truncate_at_char_boundary;
///
/// // ASCII fast path: clean cut.
/// assert_eq!(truncate_at_char_boundary("hello world", 5), "hello");
///
/// // Multi-byte safe: 3-byte char (U+4E16) repeated 4 times = 12 bytes.
/// // Asking for 7 bytes shrinks to 6 (two whole chars), never panics.
/// let cjk = "\u{4e16}\u{4e16}\u{4e16}\u{4e16}";
/// assert_eq!(truncate_at_char_boundary(cjk, 7), "\u{4e16}\u{4e16}");
///
/// // Slice longer than input is a no-op.
/// assert_eq!(truncate_at_char_boundary("abc", 100), "abc");
/// ```
pub fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Truncate `s` to the last `max_bytes` bytes, snapping the start index up to
/// the next valid UTF-8 character boundary.
///
/// Counterpart to [`truncate_at_char_boundary`] for the `&s[s.len() - N..]`
/// "show tail" pattern used by output formatters that want to elide the front
/// of a long file path. If `s.len() <= max_bytes`, the full string is returned.
///
/// # Examples
///
/// ```
/// use tldr_core::util::truncate_at_char_boundary_from_end;
///
/// assert_eq!(truncate_at_char_boundary_from_end("abcdef", 3), "def");
///
/// // Multi-byte: 3-byte char × 4 = 12 bytes; ask for last 7 -> 6 bytes (2 chars).
/// let cjk = "\u{4e16}\u{4e16}\u{4e16}\u{4e16}";
/// assert_eq!(truncate_at_char_boundary_from_end(cjk, 7), "\u{4e16}\u{4e16}");
/// ```
pub fn truncate_at_char_boundary_from_end(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut start = s.len() - max_bytes;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

// =========================================================================
// Process RSS readout (TLDR-9bxa.1).
//
// Canonical, cross-platform impl. `tldr-cli`'s `commands::daemon::rss` delegates
// here so there is a single source of truth. Best-effort by design: every reader
// returns `Option` and a failure is reported as `None`, never an error. Kept out
// of the `semantic` module (which is `--features semantic`-gated) so the always-on
// daemon can use it without that feature.
// =========================================================================

/// Current resident set size of THIS process, in bytes.
#[cfg(target_os = "macos")]
#[allow(deprecated)] // mach_task_self_ deprecated in libc in favor of the mach2 crate
pub fn current_rss_bytes() -> Option<u64> {
    // mach task_info(MACH_TASK_BASIC_INFO) — there is no procfs on macOS.
    use libc::{
        mach_task_basic_info, mach_task_self_, natural_t, task_info, KERN_SUCCESS,
        MACH_TASK_BASIC_INFO,
    };
    unsafe {
        let mut info: mach_task_basic_info = std::mem::zeroed();
        let mut count =
            (std::mem::size_of::<mach_task_basic_info>() / std::mem::size_of::<natural_t>())
                as u32;
        let kr = task_info(
            mach_task_self_,
            MACH_TASK_BASIC_INFO,
            &mut info as *mut _ as *mut _,
            &mut count,
        );
        (kr == KERN_SUCCESS).then_some(info.resident_size)
    }
}

/// Current resident set size of THIS process, in bytes.
#[cfg(target_os = "linux")]
pub fn current_rss_bytes() -> Option<u64> {
    // /proc/self/statm field 2 is RSS in pages.
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let rss_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    (page > 0).then(|| rss_pages * page as u64)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn current_rss_bytes() -> Option<u64> {
    None
}

/// Peak (high-water) resident set size of THIS process, in bytes.
#[cfg(unix)]
pub fn peak_rss_bytes() -> Option<u64> {
    unsafe {
        let mut ru: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut ru) != 0 {
            return None;
        }
        let raw = ru.ru_maxrss as u64;
        // ru_maxrss unit differs: bytes on macOS, kilobytes on Linux/BSD.
        Some(if cfg!(target_os = "macos") {
            raw
        } else {
            raw * 1024
        })
    }
}

#[cfg(not(unix))]
pub fn peak_rss_bytes() -> Option<u64> {
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
    fn peak_rss_at_least_current_order_of_magnitude() {
        let peak = peak_rss_bytes().expect("peak RSS readable");
        if let Some(current) = current_rss_bytes() {
            // Allow slack: current is sampled after peak and pages can be
            // reclaimed, but peak must be in the same order of magnitude.
            assert!(
                peak * 4 >= current,
                "peak {peak} implausibly below current {current}"
            );
        }
    }

    #[test]
    fn truncate_ascii_short_passthrough() {
        assert_eq!(truncate_at_char_boundary("abc", 10), "abc");
    }

    #[test]
    fn truncate_ascii_exact_cut() {
        assert_eq!(truncate_at_char_boundary("abcdef", 3), "abc");
    }

    #[test]
    fn truncate_at_char_boundary_zero_yields_empty() {
        assert_eq!(truncate_at_char_boundary("anything", 0), "");
    }

    #[test]
    fn truncate_three_byte_char_does_not_panic() {
        // 67 × U+4E16 = 201 bytes. Cutting at 197 lands inside the 66th char.
        let s = "\u{4e16}".repeat(67);
        let out = truncate_at_char_boundary(&s, 197);
        // Should snap down from 197 to 195 (= 65 whole chars × 3 bytes).
        assert_eq!(out.len(), 195);
        assert_eq!(out, "\u{4e16}".repeat(65));
        // Sanity: real UTF-8.
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn truncate_four_byte_emoji_does_not_panic() {
        // U+1F600 GRINNING FACE is 4 bytes. 51 × 4 = 204 bytes.
        let s = "\u{1f600}".repeat(51);
        let out = truncate_at_char_boundary(&s, 197);
        // 197 % 4 = 1, so snap from 197 down to 196 (49 chars).
        assert_eq!(out.len(), 196);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn truncate_mid_two_byte_sequence() {
        // U+00E9 (é) is 2 bytes. "café" is 1+1+1+2 = 5 bytes. Cut at 4.
        let out = truncate_at_char_boundary("café", 4);
        // Position 4 is mid-é; snap to 3 ("caf").
        assert_eq!(out, "caf");
    }

    #[test]
    fn truncate_from_end_ascii() {
        assert_eq!(truncate_at_char_boundary_from_end("abcdef", 3), "def");
    }

    #[test]
    fn truncate_from_end_short_passthrough() {
        assert_eq!(truncate_at_char_boundary_from_end("abc", 10), "abc");
    }

    #[test]
    fn truncate_from_end_three_byte_char() {
        // 13 × U+4E16 = 39 bytes. Ask for last 27 bytes.
        let s = "\u{4e16}".repeat(13);
        let out = truncate_at_char_boundary_from_end(&s, 27);
        // 39 - 27 = start 12; 12 is divisible by 3 so it's a boundary -> 27 bytes (9 chars).
        assert_eq!(out.len(), 27);
        assert_eq!(out, "\u{4e16}".repeat(9));
    }

    #[test]
    fn truncate_from_end_snaps_up_inside_codepoint() {
        // 13 × U+4E16 = 39 bytes. Ask for last 28 bytes.
        let s = "\u{4e16}".repeat(13);
        let out = truncate_at_char_boundary_from_end(&s, 28);
        // 39 - 28 = start 11; 11 is mid-char; snap up to 12 -> 27 bytes (9 chars).
        assert_eq!(out.len(), 27);
        assert_eq!(out, "\u{4e16}".repeat(9));
    }
}

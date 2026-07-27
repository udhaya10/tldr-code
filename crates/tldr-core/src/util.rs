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
            (std::mem::size_of::<mach_task_basic_info>() / std::mem::size_of::<natural_t>()) as u32;
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

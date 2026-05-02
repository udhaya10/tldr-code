//! File system operations for TLDR
//!
//! This module provides file tree traversal and ignore pattern handling.

pub mod oversize;
pub mod tree;

use std::io;
use std::path::Path;

pub use oversize::{
    check_size, format_oversize_warning, is_autogen_file, max_size_for, SizeCheck,
    MAX_AUTOGEN_FILE_SIZE_BYTES, MAX_FILE_SIZE_BYTES,
};
pub use tree::get_file_tree;

/// Outcome of a tolerant UTF-8 file read.
///
/// Distinguishes three cases that surface scanners need to handle differently:
///
/// - [`ReadOutcome::Ok`] - the file was readable and decoded as valid UTF-8.
/// - [`ReadOutcome::NonUtf8`] - the file exists and is readable, but contains
///   bytes that are not valid UTF-8 (e.g. a Lua/Luau parser-test fixture
///   with raw `0xFF` bytes). Callers should skip the file and emit a warning,
///   not abort the whole scan. The first invalid byte offset is included so
///   the warning can pinpoint the exact location.
/// - The error case (`Err(io::Error)`) is reserved for genuine I/O failures
///   (permission denied, file vanished, etc.) which still propagate.
#[derive(Debug)]
pub enum ReadOutcome {
    /// Successful read with valid UTF-8 content.
    Ok(String),
    /// File exists but is not valid UTF-8. `byte_offset` is the index of the
    /// first invalid byte sequence (matches `std::str::Utf8Error::valid_up_to`).
    NonUtf8 {
        /// Byte offset of the first invalid UTF-8 sequence.
        byte_offset: usize,
    },
}

/// Read a source file as UTF-8 text, tolerantly classifying non-UTF-8 content
/// as a skippable condition rather than a hard error.
///
/// Many parser-test corpora (notably the Luau `tests/conformance/literals.luau`
/// and `pm.luau` files) intentionally contain raw non-UTF-8 bytes. When such a
/// file appears under a directory being scanned (e.g. `tldr surface /repo`),
/// we want to skip it with a warning and continue, not abort the scan.
///
/// # Returns
///
/// - `Ok(ReadOutcome::Ok(source))` for valid UTF-8 files.
/// - `Ok(ReadOutcome::NonUtf8 { byte_offset })` for files whose bytes are not
///   valid UTF-8.
/// - `Err(io::Error)` for genuine I/O failures (file missing, permission
///   denied, etc.).
///
/// # Why not `from_utf8_lossy`?
///
/// Replacing invalid bytes with U+FFFD would produce gibberish strings that
/// parsers can choke on, surface garbage symbols, or yield misleading taint
/// analysis. Skipping with a warning is the safer policy.
pub fn read_to_string_tolerant(path: &Path) -> io::Result<ReadOutcome> {
    let bytes = std::fs::read(path)?;
    match String::from_utf8(bytes) {
        Ok(source) => Ok(ReadOutcome::Ok(source)),
        Err(err) => {
            let byte_offset = err.utf8_error().valid_up_to();
            Ok(ReadOutcome::NonUtf8 { byte_offset })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn read_to_string_tolerant_returns_ok_for_valid_utf8() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello world\n").unwrap();
        let outcome = read_to_string_tolerant(f.path()).unwrap();
        match outcome {
            ReadOutcome::Ok(s) => assert_eq!(s, "hello world\n"),
            other => panic!("expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn read_to_string_tolerant_returns_nonutf8_for_invalid_bytes() {
        let mut f = NamedTempFile::new().unwrap();
        // 0xFF is never valid as a leading byte in UTF-8.
        f.write_all(b"valid prefix \xFF\xFE invalid").unwrap();
        let outcome = read_to_string_tolerant(f.path()).unwrap();
        match outcome {
            ReadOutcome::NonUtf8 { byte_offset } => {
                assert_eq!(byte_offset, "valid prefix ".len());
            }
            other => panic!("expected NonUtf8, got {:?}", other),
        }
    }

    #[test]
    fn read_to_string_tolerant_returns_err_for_missing_file() {
        let outcome = read_to_string_tolerant(Path::new("/nonexistent/path/xyz.txt"));
        assert!(outcome.is_err());
    }
}

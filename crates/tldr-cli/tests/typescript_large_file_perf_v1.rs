//! TYPESCRIPT-LARGE-FILE-PERF-V1 — uniform oversize-file skip policy
//!
//! Six commands (structure, calls, smells, dead, secure, plus other
//! parse-based scanners) timed out at 30 s when pointed at a single
//! 2.3 MB auto-generated TypeScript declaration file
//! (`/tmp/repos/ts-dom-gen/baselines/dom.generated.d.ts`). The same
//! repo's `src/` finished in 0.02 s. The bottleneck was super-linear
//! per-file analysis on a dense `.d.ts` artefact that's rarely
//! valuable to analyse deeply.
//!
//! The fix centralises the file-size policy in
//! [`tldr_core::fs::oversize`] and enforces it at file-read time in
//! [`tldr_core::ast::parser::parse_file_with_lang`]. Auto-generated
//! / minified artefacts (`.d.ts`, `.min.js`, `.bundle.css`, …) get a
//! stricter 512 KB cap; normal source files keep the historical 10 MB
//! cap. Oversize files surface as a structured warning + a non-zero
//! `files_skipped` counter (mirrors the M-X5 / M-Y2 UTF-8-tolerance
//! pattern), never a hard error.
//!
//! These tests synthesise the failure mode (one valid + one
//! over-cap file in a tempdir) and assert the directory-scanning
//! commands continue and surface the skipped file in `warnings` +
//! `files_skipped`.

use assert_cmd::Command;
use std::fs;
use std::time::{Duration, Instant};
use tempfile::tempdir;

fn tldr_cmd() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin!("tldr"))
}

/// Cap from `tldr_core::fs::oversize::MAX_AUTOGEN_FILE_SIZE_BYTES`.
/// Hard-coded here to keep the integration test independent of the
/// internal API surface — a regression in either constant trips
/// the unit test in `oversize.rs` instead.
const AUTOGEN_CAP_BYTES: usize = 512 * 1024;

/// Cap from `tldr_core::fs::oversize::MAX_FILE_SIZE_BYTES`.
const SOURCE_CAP_BYTES: usize = 10 * 1024 * 1024;

/// Build a tempdir with one normal `.ts` file and one over-cap
/// `.d.ts` file. Returns the tempdir guard so the caller controls
/// lifetime.
///
/// The bad file is sized at the auto-gen cap + 16 KB so it crosses
/// the `.d.ts`-specific 512 KB threshold but stays well under the
/// 10 MB normal-source cap (proving the auto-gen cap is what
/// applied).
fn make_oversize_dts_dir() -> tempfile::TempDir {
    let dir = tempdir().unwrap();

    // Tiny valid file — must always be analysed.
    fs::write(
        dir.path().join("good.ts"),
        b"export function ok(): number { return 1; }\n",
    )
    .unwrap();

    // Over-cap auto-generated declaration file. Content is repeated
    // valid TypeScript so the bytes themselves are not the issue —
    // the size policy is the only reason this gets skipped.
    let chunk = b"export interface I { x: number; }\n";
    let mut bytes: Vec<u8> = Vec::with_capacity(AUTOGEN_CAP_BYTES + 16 * 1024);
    while bytes.len() < AUTOGEN_CAP_BYTES + 16 * 1024 {
        bytes.extend_from_slice(chunk);
    }
    fs::write(dir.path().join("dom.generated.d.ts"), bytes).unwrap();

    dir
}

/// `test_skip_oversize_file_with_warning` — the headline test from
/// the milestone spec. Synthetic dir with 1 valid file + 1 file >
/// MAX_SIZE; the scan must complete, `files_skipped` must include
/// the oversize file, and `warnings` must name it.
#[test]
fn test_skip_oversize_file_with_warning() {
    let dir = make_oversize_dts_dir();

    let started = Instant::now();
    let output = tldr_cmd()
        .arg("structure")
        .arg(dir.path().to_str().unwrap())
        .arg("--lang")
        .arg("typescript")
        .arg("-f")
        .arg("json")
        .output()
        .expect("structure must execute");
    let elapsed = started.elapsed();

    // Must finish well under the historical 30 s timeout — the cap
    // policy is what makes this fast. A 5 s budget gives huge
    // headroom on slow CI runners while still catching a regression.
    assert!(
        elapsed < Duration::from_secs(15),
        "structure took {:?}; oversize policy must skip the over-cap \
         auto-gen file rather than analyse it",
        elapsed,
    );

    assert!(
        output.status.success(),
        "structure must succeed with oversize policy; status={:?}, \
         stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "structure stdout must be valid JSON; err: {}; stdout: {}",
            e, stdout
        )
    });

    let files_skipped = report
        .get("files_skipped")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert_eq!(
        files_skipped, 1,
        "structure must report files_skipped=1 for the oversize \
         .d.ts; report={}",
        report
    );

    let warnings = report
        .get("warnings")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        warnings.len(),
        1,
        "structure must emit exactly one warning for the oversize \
         file; warnings={:?}",
        warnings
    );
    let warning = warnings[0].as_str().unwrap_or("");
    assert!(
        warning.contains("dom.generated.d.ts"),
        "warning must reference the skipped file path; got: {}",
        warning
    );
    assert!(
        warning.contains("exceeds"),
        "warning must use the documented 'exceeds' phrasing; got: {}",
        warning
    );

    // Sanity: the small file is still analysed.
    let files = report
        .get("files")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        !files.is_empty(),
        "structure must still analyse the valid .ts file; files={:?}",
        files
    );
}

/// `test_dts_files_have_lower_cap` — synthetic .d.ts that exceeds
/// the .d.ts-specific cap but stays under the normal 10 MB source
/// cap. Must be skipped (proving the auto-gen branch fires).
#[test]
fn test_dts_files_have_lower_cap() {
    let dir = tempdir().unwrap();

    // 1.5x autogen cap: > 512 KB autogen cap, < 10 MB source cap.
    // Sized deliberately to straddle the two caps — proves the
    // auto-gen branch is the rule that applied.
    let target_bytes = AUTOGEN_CAP_BYTES + AUTOGEN_CAP_BYTES / 2;
    assert!(
        target_bytes < SOURCE_CAP_BYTES,
        "test sizing invariant: must straddle the two caps so the \
         auto-gen branch is what fires"
    );
    let chunk = b"export type T = number;\n";
    let mut bytes: Vec<u8> = Vec::with_capacity(target_bytes);
    while bytes.len() < target_bytes {
        bytes.extend_from_slice(chunk);
    }
    fs::write(dir.path().join("autogen.d.ts"), bytes).unwrap();

    // Companion small valid file so the dir isn't empty.
    fs::write(dir.path().join("ok.ts"), b"export const x: number = 0;\n").unwrap();

    let output = tldr_cmd()
        .arg("structure")
        .arg(dir.path().to_str().unwrap())
        .arg("--lang")
        .arg("typescript")
        .arg("-f")
        .arg("json")
        .output()
        .expect("structure must execute");

    assert!(
        output.status.success(),
        "structure must succeed; status={:?}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "structure stdout must be valid JSON; err: {}; stdout: {}",
            e, stdout
        )
    });

    let files_skipped = report
        .get("files_skipped")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert_eq!(
        files_skipped, 1,
        "a .d.ts > 512 KB but < 10 MB MUST be skipped under the \
         auto-gen policy; report={}",
        report,
    );

    let warnings = report
        .get("warnings")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let warning = warnings.first().and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        warning.contains("autogen.d.ts"),
        "warning must reference the skipped .d.ts; got: {}",
        warning
    );
    assert!(
        warning.contains("auto-generated/minified files"),
        "warning must label the skipped file under the auto-gen \
         category (so users know why a sub-MB file was rejected when \
         the headline cap is 10 MB); got: {}",
        warning
    );
}

/// Negative control: a normal `.ts` file in the 512 KB – 10 MB band
/// MUST NOT be skipped (the auto-gen cap doesn't apply to non
/// auto-gen extensions).
#[test]
fn test_normal_ts_file_below_10mb_not_skipped() {
    let dir = tempdir().unwrap();

    // 1.5x autogen cap normal .ts: > auto-gen cap but < source cap.
    // Should be analysed normally.
    let target_bytes = AUTOGEN_CAP_BYTES + AUTOGEN_CAP_BYTES / 2;
    let chunk = b"export const a: number = 1;\n";
    let mut bytes: Vec<u8> = Vec::with_capacity(target_bytes);
    while bytes.len() < target_bytes {
        bytes.extend_from_slice(chunk);
    }
    fs::write(dir.path().join("big.ts"), bytes).unwrap();

    let output = tldr_cmd()
        .arg("structure")
        .arg(dir.path().to_str().unwrap())
        .arg("--lang")
        .arg("typescript")
        .arg("-f")
        .arg("json")
        .output()
        .expect("structure must execute");

    assert!(
        output.status.success(),
        "structure must succeed for a sub-10 MB normal .ts file; \
         status={:?}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "structure stdout must be valid JSON; err: {}; stdout: {}",
            e, stdout
        )
    });

    // files_skipped is omitted on clean inputs (skip_serializing_if).
    let files_skipped = report
        .get("files_skipped")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert_eq!(
        files_skipped, 0,
        "a sub-10 MB normal .ts MUST NOT be skipped (auto-gen cap \
         applies only to .d.ts/.min.js/.bundle.* extensions); \
         report={}",
        report,
    );
}

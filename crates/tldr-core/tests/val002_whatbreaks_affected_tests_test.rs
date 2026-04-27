//! M2 VAL-002 — whatbreaks affected_test_count populates from caller-tree (#1.E)
//!
//! Cross-file Python call graph requires the FLAT layout pattern that v2 builder
//! supports (see `crates/tldr-core/src/callgraph/cross_file_test.rs::create_simple_cross_file_project`).
//! `is_test_file` (clones/filter.rs:20) matches via filename prefix `test_*.py`,
//! so `test_service.py` at the project root is correctly classified as a test file.

use std::fs;
use tempfile::TempDir;

use tldr_core::analysis::whatbreaks::{whatbreaks_analysis, TargetType, WhatbreaksOptions};
use tldr_core::Language;

#[test]
fn whatbreaks_function_target_counts_test_callers() {
    let dir = TempDir::new().unwrap();

    // Production source: defines process()
    fs::write(
        dir.path().join("service.py"),
        "def process():\n    return \"processed\"\n",
    )
    .unwrap();

    // Test caller (filename matches `test_*.py` -> is_test_file == true).
    fs::write(
        dir.path().join("test_service.py"),
        "from service import process\n\ndef test_process():\n    assert process() == \"processed\"\n",
    )
    .unwrap();

    let opts = WhatbreaksOptions {
        depth: 3,
        quick: false,
        language: Some(Language::Python),
        force_type: None,
    };
    let report = whatbreaks_analysis("process", dir.path(), &opts).unwrap();

    // Sanity: target should be detected as Function.
    assert_eq!(
        report.target_type,
        TargetType::Function,
        "target should be detected as Function"
    );

    // Precondition: impact analysis must have found at least one caller.
    // If this fails, the bug is deeper than VAL-002 (impact analysis broken).
    assert!(
        report.summary.transitive_caller_count >= 1,
        "PRECONDITION: impact must find caller — got {}",
        report.summary.transitive_caller_count
    );

    // The actual VAL-002 assertion (FAILS on HEAD 69fe94c with affected_test_count = 0):
    assert!(
        report.summary.affected_test_count >= 1,
        "BUG: affected_test_count = {} but test_service.py is in caller tree",
        report.summary.affected_test_count
    );
}

#[test]
fn whatbreaks_function_target_zero_when_no_test_callers() {
    // Sanity: function with only production callers should report 0.
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("service.py"),
        "def process():\n    return \"processed\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("caller.py"),
        "from service import process\ndef invoke():\n    return process()\n",
    )
    .unwrap();

    let opts = WhatbreaksOptions {
        depth: 3,
        quick: false,
        language: Some(Language::Python),
        force_type: None,
    };
    let report = whatbreaks_analysis("process", dir.path(), &opts).unwrap();

    assert_eq!(
        report.summary.affected_test_count, 0,
        "no test callers; expected affected_test_count == 0"
    );
}

#[test]
fn whatbreaks_function_target_dedup_by_file() {
    // Two test functions in the SAME test file calling process() -> count = 1, not 2.
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("service.py"),
        "def process():\n    return \"processed\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("test_service.py"),
        "from service import process\n\
         def test_a():\n    assert process() == \"processed\"\n\
         def test_b():\n    assert process() == \"processed\"\n",
    )
    .unwrap();

    let opts = WhatbreaksOptions {
        depth: 3,
        quick: false,
        language: Some(Language::Python),
        force_type: None,
    };
    let report = whatbreaks_analysis("process", dir.path(), &opts).unwrap();

    assert_eq!(
        report.summary.affected_test_count, 1,
        "two test fns in same file should count as 1 affected file (HashSet dedup)"
    );
}

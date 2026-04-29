//! regex-removal-v1 — Wave 1 standard-library integration tests.
//!
//! These tests exercise the structural AST taint pipeline end-to-end via
//! `compute_taint_with_tree(...)` for stdlib patterns currently covered only
//! by regex banks. As parity-add milestones (W1-M4 and W1.5-M6) land, RED
//! tests in this file transition to GREEN; once Wave 2 deletes the regex
//! banks, this file becomes the primary safety net for stdlib coverage.
//!
//! Helper shape mirrored from
//! `val002_member_access_structural_test.rs::analyze` /
//! `analyze_with_ssa` (same shape as `rr_framework_integ_test.rs`).
//!
//! Milestone scope:
//! * W1-M4 — Python `os.spawn*` family AST sinks (this file's first 6 tests).
//! * W1.5-M6 — Ruby `IO.popen` + Elixir `System.cmd` module-call coverage
//!   (will append 2 more tests below).

use std::collections::HashMap;

use tldr_core::ast::parser::parse;
use tldr_core::cfg::get_cfg_context;
use tldr_core::dfg::get_dfg_context;
use tldr_core::security::taint::compute_taint_with_tree;
use tldr_core::ssa::construct::construct_minimal_ssa;
use tldr_core::{Language, TaintInfo, TaintSinkType};

fn statements_from(src: &str) -> HashMap<u32, String> {
    src.lines()
        .enumerate()
        .map(|(i, text)| ((i + 1) as u32, text.to_string()))
        .collect()
}

#[allow(dead_code)]
fn analyze(src: &str, lang: Language, fn_name: &str) -> TaintInfo {
    analyze_with_ssa(src, lang, fn_name, /* use_ssa */ true)
}

fn analyze_with_ssa(src: &str, lang: Language, fn_name: &str, use_ssa: bool) -> TaintInfo {
    let cfg = get_cfg_context(src, fn_name, lang).expect("CFG must succeed");
    let dfg = get_dfg_context(src, fn_name, lang).expect("DFG must succeed");
    let ssa = if use_ssa {
        construct_minimal_ssa(&cfg, &dfg).ok()
    } else {
        None
    };
    let tree = parse(src, lang).expect("parse must succeed");

    compute_taint_with_tree(
        &cfg,
        &dfg.refs,
        &statements_from(src),
        Some(&tree),
        Some(src.as_bytes()),
        lang,
        ssa.as_ref(),
    )
    .expect("taint analysis must succeed")
}

// ---------- W1-M4: Python os.spawn* family sinks ----------

/// W1-M4 #1 — `os.spawnl(MODE, path, '-c', cmd)` where `cmd` is `input()`.
///
/// Pre-W1-M4: AST bank lacks `('os','spawnl')`; this test currently passes
/// only because the parallel regex bank `os\.(system|popen|spawn\w*)\s*\(`
/// fires (additive mode). Post-W1-M4: AST member_pattern is the load-bearing
/// path; once Wave 2 deletes the regex bank, this test still passes.
#[test]
fn python_os_spawnl_to_user_input_via_compute_taint() {
    let src = "\
import os
def handler():
    cmd = input()
    os.spawnl(os.P_WAIT, '/bin/sh', '-c', cmd)
";
    let result = analyze_with_ssa(src, Language::Python, "handler", /* use_ssa */ false);
    let sink_lines: Vec<_> = result
        .sinks
        .iter()
        .filter(|s| matches!(s.sink_type, TaintSinkType::ShellExec))
        .map(|s| s.line)
        .collect();
    assert!(
        !sink_lines.is_empty(),
        "expected at least one ShellExec sink for os.spawnl; got sinks={:?}",
        result.sinks
    );
}

/// W1-M4 #2 — `os.spawnle(MODE, path, cmd, env)` (env-passing form).
#[test]
fn python_os_spawnle_via_compute_taint() {
    let src = "\
import os
def handler():
    cmd = input()
    os.spawnle(os.P_WAIT, '/bin/sh', cmd, {})
";
    let result = analyze_with_ssa(src, Language::Python, "handler", /* use_ssa */ false);
    let sink_lines: Vec<_> = result
        .sinks
        .iter()
        .filter(|s| matches!(s.sink_type, TaintSinkType::ShellExec))
        .map(|s| s.line)
        .collect();
    assert!(
        !sink_lines.is_empty(),
        "expected at least one ShellExec sink for os.spawnle; got sinks={:?}",
        result.sinks
    );
}

/// W1-M4 #3 — `os.spawnlp(MODE, name, args...)` (PATH-search form).
#[test]
fn python_os_spawnlp_via_compute_taint() {
    let src = "\
import os
def handler():
    cmd = input()
    os.spawnlp(os.P_WAIT, 'sh', '-c', cmd)
";
    let result = analyze_with_ssa(src, Language::Python, "handler", /* use_ssa */ false);
    let sink_lines: Vec<_> = result
        .sinks
        .iter()
        .filter(|s| matches!(s.sink_type, TaintSinkType::ShellExec))
        .map(|s| s.line)
        .collect();
    assert!(
        !sink_lines.is_empty(),
        "expected at least one ShellExec sink for os.spawnlp; got sinks={:?}",
        result.sinks
    );
}

/// W1-M4 #4 — `os.spawnv(MODE, path, args)` (vector-args form).
#[test]
fn python_os_spawnv_via_compute_taint() {
    let src = "\
import os
def handler():
    cmd = input()
    os.spawnv(os.P_WAIT, '/bin/sh', ['-c', cmd])
";
    let result = analyze_with_ssa(src, Language::Python, "handler", /* use_ssa */ false);
    let sink_lines: Vec<_> = result
        .sinks
        .iter()
        .filter(|s| matches!(s.sink_type, TaintSinkType::ShellExec))
        .map(|s| s.line)
        .collect();
    assert!(
        !sink_lines.is_empty(),
        "expected at least one ShellExec sink for os.spawnv; got sinks={:?}",
        result.sinks
    );
}

/// W1-M4 #5 — `os.spawnvp(MODE, name, args)` (PATH-search vector form).
#[test]
fn python_os_spawnvp_via_compute_taint() {
    let src = "\
import os
def handler():
    cmd = input()
    os.spawnvp(os.P_WAIT, 'sh', ['sh', '-c', cmd])
";
    let result = analyze_with_ssa(src, Language::Python, "handler", /* use_ssa */ false);
    let sink_lines: Vec<_> = result
        .sinks
        .iter()
        .filter(|s| matches!(s.sink_type, TaintSinkType::ShellExec))
        .map(|s| s.line)
        .collect();
    assert!(
        !sink_lines.is_empty(),
        "expected at least one ShellExec sink for os.spawnvp; got sinks={:?}",
        result.sinks
    );
}

/// W1-M4 #6 — `os.spawnvpe(MODE, name, args, env)` (PATH+env vector form).
#[test]
fn python_os_spawnvpe_via_compute_taint() {
    let src = "\
import os
def handler():
    cmd = input()
    os.spawnvpe(os.P_WAIT, 'sh', ['sh', '-c', cmd], {})
";
    let result = analyze_with_ssa(src, Language::Python, "handler", /* use_ssa */ false);
    let sink_lines: Vec<_> = result
        .sinks
        .iter()
        .filter(|s| matches!(s.sink_type, TaintSinkType::ShellExec))
        .map(|s| s.line)
        .collect();
    assert!(
        !sink_lines.is_empty(),
        "expected at least one ShellExec sink for os.spawnvpe; got sinks={:?}",
        result.sinks
    );
}

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
//! `analyze_with_ssa`. As of W1.5-M6 the helpers live in
//! `tests/common/integ_helpers.rs` and are shared by all Wave-1.5+
//! integration tests.
//!
//! Milestone scope:
//! * W1-M4 — Python `os.spawn*` family AST sinks (this file's first 6 tests).
//! * W1.5-M6 — Ruby `IO.popen` + Elixir `System.cmd` module-call partial-
//!   coverage probes (the 2 tests appended below). Both are documented as
//!   PARTIAL per worker-2-integ-tests.json: they currently pass via the
//!   regex bank (Ruby/Elixir are HOLD languages whose substring fallback
//!   stays through Wave 2). Wave 2 deletion does NOT remove the Ruby/
//!   Elixir regex banks, so these tests continue to pass post-deletion.

use tldr_core::{Language, TaintSinkType};

#[path = "common/integ_helpers.rs"]
mod common;

use common::analyze_with_ssa;

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

// ---------- W1.5-M6: Ruby + Elixir module-call partial-coverage probes ----------

/// W1.5-M6 #1 — Ruby `IO.popen(cmd)` where `cmd` came from `gets`. Module-
/// method-call form (qualified receiver = constant module name).
///
/// Per worker-2-integ-tests.json (B-#7): Ruby `field_access_info` covers
/// only `@ivar` (instance_variable nodes), NOT module calls. The flow can
/// land via either (a) `extract_call_name_ruby` returning the qualified
/// `'IO.popen'` form so that `RUBY_AST_SINKS` call_names matches, or
/// (b) the substring fallback in the retained Ruby regex bank (Ruby is a
/// HOLD language; its regex bank STAYS through Wave 2).
///
/// In additive dispatch (Wave 1.5) at least one of those paths fires —
/// the test is GREEN. Post-Wave-2 the Ruby regex bank is preserved
/// verbatim, so this test stays GREEN even after the TS/Python regex
/// banks are deleted.
#[test]
fn ruby_io_popen_module_call_via_compute_taint() {
    let src = "\
def handler
    cmd = gets
    IO.popen(cmd)
end
";
    let result = analyze_with_ssa(src, Language::Ruby, "handler", /* use_ssa */ false);
    let sink_lines: Vec<_> = result
        .sinks
        .iter()
        .filter(|s| matches!(s.sink_type, TaintSinkType::ShellExec))
        .map(|s| s.line)
        .collect();
    assert!(
        !sink_lines.is_empty(),
        "expected at least one ShellExec sink for IO.popen; got sinks={:?}",
        result.sinks
    );
}

/// W1.5-M6 #2 — Elixir `System.cmd(cmd, [])` where `cmd` came from
/// `IO.gets`. Module-method-call form mirroring the Ruby probe.
///
/// Per worker-2-integ-tests.json (B-#8): Elixir `field_access_info`
/// covers only `@attr` unary_operator nodes; module calls go through the
/// call_names path or the substring fallback in the retained Elixir
/// regex bank (also a HOLD language). Wave 2 preserves the Elixir regex
/// bank, so this probe stays GREEN through deletion.
#[test]
fn elixir_system_cmd_module_call_via_compute_taint() {
    let src = "\
def handler do
  cmd = IO.gets(\"$ \")
  System.cmd(cmd, [])
end
";
    let result = analyze_with_ssa(src, Language::Elixir, "handler", /* use_ssa */ false);
    let sink_lines: Vec<_> = result
        .sinks
        .iter()
        .filter(|s| matches!(s.sink_type, TaintSinkType::ShellExec))
        .map(|s| s.line)
        .collect();
    assert!(
        !sink_lines.is_empty(),
        "expected at least one ShellExec sink for System.cmd; got sinks={:?}",
        result.sinks
    );
}

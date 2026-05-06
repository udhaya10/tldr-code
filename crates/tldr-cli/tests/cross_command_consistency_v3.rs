//! cross-command-consistency-v3 — regression tests for 3 phase-5 audit bugs:
//!
//! - **P5.BUG-N1 (HIGH)**: `tldr extract` on cpp `.h` files extracted classes
//!   as functions with `return_type: "class"` and `classes: []` because the
//!   CLI dropped `--lang` on the floor and the autodetect always classified
//!   `.h` as C. Real C++ projects keep public headers as `.h` next to
//!   `.cpp` translation units (e.g. `tinyxml2.h` / `tinyxml2.cpp`); the C
//!   tree-sitter grammar then mis-parsed `class Foo {…}` declarations and
//!   the entire class enumeration was missed. The fix forwards the
//!   resolved language hint to `extract_file_with_lang` and adds
//!   `Language::from_path_with_siblings` so headers next to C++ sources
//!   are auto-classified as C++.
//!
//! - **P5.BUG-N2 (MED)**: `tldr complexity` and `tldr explain` reported
//!   different cyclomatic numbers for the same function (e.g. Flask.run:
//!   13 vs 12, Flask.full_dispatch_request: 6 vs 5). Two implementations
//!   existed: the canonical `tldr_core::calculate_complexity` (used by
//!   `tldr complexity`) and a private `compute_complexity` walker in
//!   `commands/remaining/explain.rs` that under-counted boolean operator
//!   decision points. The fix has explain delegate the cyclomatic value to
//!   `calculate_complexity`, keeping the local walker only for fields
//!   unique to `ComplexityInfo` (`num_blocks`, `num_edges`, `has_loops`).
//!
//! - **P5.BUG-N3 (MED)**: `tldr impact Class.method` errored "Function not
//!   found" while `tldr whatbreaks Class.method` accepted the same name
//!   (whatbreaks just hid the underlying error inside a sub-result).
//!   `impact_analysis` matched a candidate against the target only one
//!   way (strip the qualifier on the candidate); a user-typed
//!   `Flask.run` against a graph emitting bare `run` therefore failed.
//!   The fix introduces `names_match` which accepts both directions and
//!   tail-on-tail when the user explicitly qualified the target.

use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;

fn tldr_cmd() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin!("tldr"))
}

/// Run `tldr <args>` and parse stdout as JSON. Panics on non-zero exit.
fn run_json(args: &[&str]) -> Value {
    let out = tldr_cmd()
        .args(args)
        .args(["--format", "json", "-q"])
        .output()
        .unwrap_or_else(|e| panic!("spawn {:?}: {}", args, e));
    assert!(
        out.status.success(),
        "tldr {:?} failed: stderr={}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "tldr {:?} JSON parse failed: {}\nstdout={}",
            args,
            e,
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// Run `tldr <args>` and return (status, stdout, stderr).
fn run_raw(args: &[&str]) -> (bool, String, String) {
    let out = tldr_cmd()
        .args(args)
        .args(["--format", "json", "-q"])
        .output()
        .unwrap_or_else(|e| panic!("spawn {:?}: {}", args, e));
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

// =============================================================================
// P5.BUG-N1: extract on cpp .h files honors --lang and sibling autodetect
// =============================================================================

/// `tldr extract` on a C++ `.h` header next to `.cpp` siblings must use the
/// C++ grammar — language must be `cpp`, classes must be enumerated, and no
/// function entries must leak with `return_type == "class"`.
#[test]
fn test_n1_extract_cpp_h_uses_cpp_parser() {
    if !Path::new("/tmp/repos/cpp-tinyxml2/tinyxml2.h").exists() {
        return;
    }
    let v = run_json(&["extract", "/tmp/repos/cpp-tinyxml2/tinyxml2.h"]);

    let lang = v
        .get("language")
        .and_then(|l| l.as_str())
        .expect("extract: missing /language");
    assert_eq!(
        lang, "cpp",
        "expected language=cpp for tinyxml2.h (sibling .cpp present), got {:?}; \
         the C grammar mis-parses C++ headers and produces zero classes plus \
         class-as-function leakage",
        lang
    );

    let class_count = v
        .get("classes")
        .and_then(|c| c.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    assert!(
        class_count >= 6,
        "expected at least 6 classes in tinyxml2.h (real count is much higher), \
         got {}; classes array: {:?}",
        class_count,
        v.get("classes")
    );

    // No `functions[]` entry should have `return_type == "class"`.
    let class_as_fn: Vec<&Value> = v
        .get("functions")
        .and_then(|f| f.as_array())
        .map(|a| {
            a.iter()
                .filter(|f| {
                    f.get("return_type")
                        .and_then(|r| r.as_str())
                        .map(|s| s == "class")
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    assert!(
        class_as_fn.is_empty(),
        "expected zero functions with return_type=class (the C-grammar leakage), \
         found {} such entries: {:?}",
        class_as_fn.len(),
        class_as_fn
    );
}

/// `tldr extract --lang cpp` must honor the explicit hint regardless of the
/// file extension's canonical mapping (`.h` -> C). Direct test of the CLI
/// flag forwarding that was previously dropped on the floor.
#[test]
fn test_n1_extract_lang_flag_honored() {
    if !Path::new("/tmp/repos/cpp-tinyxml2/tinyxml2.h").exists() {
        return;
    }
    let v = run_json(&["extract", "--lang", "cpp", "/tmp/repos/cpp-tinyxml2/tinyxml2.h"]);
    let lang = v
        .get("language")
        .and_then(|l| l.as_str())
        .expect("extract: missing /language");
    assert_eq!(
        lang, "cpp",
        "explicit --lang cpp must override extension-based detection; got {:?}",
        lang
    );
}

// =============================================================================
// P5.BUG-N2: complexity and explain agree on cyclomatic
// =============================================================================

/// For multiple Flask methods, `tldr complexity` and `tldr explain` must
/// report the same cyclomatic number. The audit observed disagreement on
/// at least 3 of 4 methods (only Flask.__init__ matched accidentally).
#[test]
fn test_n2_cyclomatic_complexity_explain_agree() {
    let app_path = "/tmp/repos/flask/src/flask/app.py";
    if !Path::new(app_path).exists() {
        return;
    }

    // Cover the audit's full disagreement table plus __init__ as a control.
    let methods = [
        "Flask.__init__",
        "Flask.dispatch_request",
        "Flask.full_dispatch_request",
        "Flask.run",
    ];

    for method in &methods {
        let cmplx = run_json(&["complexity", app_path, method]);
        let cmplx_cyc = cmplx
            .get("cyclomatic")
            .and_then(|v| v.as_u64())
            .unwrap_or_else(|| panic!("complexity {}: missing cyclomatic", method));

        let expl = run_json(&["explain", app_path, method]);
        let expl_cyc = expl
            .pointer("/complexity/cyclomatic")
            .and_then(|v| v.as_u64())
            .unwrap_or_else(|| panic!("explain {}: missing /complexity/cyclomatic", method));

        assert_eq!(
            cmplx_cyc, expl_cyc,
            "cyclomatic mismatch for {}: complexity={} explain={} \
             (the two commands must share a single source of truth)",
            method, cmplx_cyc, expl_cyc
        );
    }
}

// =============================================================================
// P5.BUG-N3: impact accepts qualified Class.method names
// =============================================================================

/// `tldr impact Flask.run /tmp/repos/flask` must succeed (no
/// "Function not found"). The previous one-direction matcher rejected
/// every user-typed `Class.method` query against a graph emitting bare
/// method names.
#[test]
fn test_n3_impact_accepts_qualified_names() {
    if !Path::new("/tmp/repos/flask/src/flask/app.py").exists() {
        return;
    }

    let v = run_json(&["impact", "Flask.run", "/tmp/repos/flask"]);

    // The report's `targets` field is a map keyed by `<file>:<func>`. At
    // least one entry must identify a `run` method in the flask source
    // tree (we don't pin the exact key shape — the contract is "the
    // command no longer errors on Class.method").
    let targets = v
        .get("targets")
        .and_then(|t| t.as_object())
        .expect("impact: missing /targets");

    assert!(
        !targets.is_empty(),
        "impact Flask.run returned zero targets; report: {:?}",
        v
    );

    let any_run = targets.iter().any(|(key, val)| {
        let key_has_run = key.contains("run");
        let func_field = val
            .get("function")
            .and_then(|f| f.as_str())
            .unwrap_or("");
        key_has_run
            && (func_field == "run" || func_field == "Flask.run" || func_field.ends_with(".run"))
    });
    assert!(
        any_run,
        "expected a target identifying Flask.run (key contains 'run' and \
         function field is `run` / `Flask.run` / `*.run`); got: {:?}",
        targets
    );
}

/// Symmetry check: every name accepted by `whatbreaks` (whose Function-
/// target detection has historically tolerated more shapes) must also be
/// accepted by `impact`. We assert both commands return a non-zero exit
/// status for the same target.
#[test]
fn test_n3_impact_whatbreaks_name_parity() {
    if !Path::new("/tmp/repos/flask/src/flask/app.py").exists() {
        return;
    }

    // Names typed by users when copy-pasting from `tldr structure`,
    // `tldr complexity`, `tldr explain` output. All four are expected to
    // resolve consistently across both commands.
    let names = [
        "Flask.run",
        "run",
        "Flask.dispatch_request",
        "dispatch_request",
    ];

    for name in &names {
        let (wb_ok, _, wb_err) = run_raw(&["whatbreaks", name, "/tmp/repos/flask"]);
        let (imp_ok, _, imp_err) = run_raw(&["impact", name, "/tmp/repos/flask"]);

        // Whatever shapes whatbreaks accepts, impact must accept too.
        // We only enforce parity when whatbreaks accepts the name —
        // otherwise the input is genuinely unresolvable and impact is
        // free to error.
        if wb_ok {
            assert!(
                imp_ok,
                "name parity broken for {:?}: whatbreaks succeeded but impact failed.\n\
                 whatbreaks stderr: {}\n\
                 impact stderr: {}",
                name, wb_err, imp_err
            );
        }
    }
}

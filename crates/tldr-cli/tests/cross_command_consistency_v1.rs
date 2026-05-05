//! cross-command-consistency-v1 — regression tests for 4 bugs:
//!
//! - BUG-5: `tldr impact` missed callers that `tldr references` found.
//!   Specifically, function-as-value uses (e.g. `kw=_helper`,
//!   `return _helper`, `fn = _helper`) produced no edges in the call
//!   graph because the v2 builder's Python extractor only collected
//!   `call` nodes — never identifiers used as values. As a result,
//!   `impact` reported `caller_count: 0` for any function whose only
//!   uses were higher-order, and "exported but no callers" advice
//!   misled users.
//!
//! - BUG-7: `tldr complexity` and `tldr cognitive` reported different
//!   cognitive numbers for the same function. Two separate calculators
//!   existed (`metrics::complexity::ComplexityCalculator` and
//!   `metrics::cognitive::CognitiveCalculator`), each with its own
//!   nesting / else-if / logical-operator rules. After this fix,
//!   `complexity` delegates the cognitive number to the canonical
//!   SonarSource calculator that powers `cognitive`.
//!
//! - BUG-8: path canonicalization drift on macOS — `halstead`,
//!   `cognitive`, and `dead-stores` emitted `/private/tmp/...` while
//!   `reaching-defs` emitted `/tmp/...` for the same input. The fix:
//!   commands keep `validate_file_path` for existence/traversal checks
//!   but emit the user-supplied path in the JSON `file` field.
//!
//! - BUG-14: project-root and function-name field drift — `health`
//!   used `path`, `inheritance` used `project_path`, others used
//!   `root`; `taint` and `explain` used `function_name` while
//!   slice/dead-stores/resources/reaching-defs used `function`. The
//!   fix renames in JSON to the canonical `root` / `function` (with
//!   serde aliases for backward-compat deserialise).
//!
//! All tests build a minimal Python (and JS where relevant) project in
//! a tempdir so they don't depend on `/tmp/repos/<x>` being present.

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn tldr_cmd() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin!("tldr"))
}

fn write(p: &Path, body: &str) {
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).expect("mkdir -p");
    }
    fs::write(p, body).expect("write fixture");
}

/// Build a small Python project with a function that is used both as
/// a direct call AND as a value (return / assignment / kwarg / class
/// body). BUG-5 should make `impact` see all of these.
fn build_python_project_with_value_uses() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    write(
        &root.join("app.py"),
        r#"
def helper(x):
    return x * 2

def use_direct():
    return helper(3)

def use_return():
    return helper

def use_assign():
    fn = helper
    return fn

def use_kwarg(callback=helper):
    return callback(1)

def use_positional():
    return list(map(helper, [1, 2, 3]))


class Config:
    transformer = some_factory(callback=helper)
"#,
    );

    dir
}

/// Build a Python project with three functions of known cognitive
/// complexity that we test for parity between `tldr complexity` and
/// `tldr cognitive`.
fn build_python_project_for_cognitive_parity() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    write(
        &root.join("complex.py"),
        r#"
def trivial(x):
    return x


def with_nested(x, y):
    if x > 0:
        if y > 0:
            return x + y
    return 0


def with_logic(a, b, c):
    if a and b:
        return 1
    if a or c:
        return 2
    if b and c:
        return 3
    return 0
"#,
    );

    dir
}

/// Build a small JS project for cognitive parity (covers the
/// "and at least 2 languages" requirement of the spec).
fn build_js_project_for_cognitive_parity() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    write(
        &root.join("svc.js"),
        r#"
function plain(x) {
    return x;
}

function nested(a, b) {
    if (a > 0) {
        if (b > 0) {
            return a + b;
        }
    }
    return 0;
}

function logical(a, b, c) {
    if (a && b) return 1;
    if (a || c) return 2;
    return 0;
}
"#,
    );

    dir
}

/// Build a multi-file Python project the BUG-14 root-field tests can
/// run against. We need at least two files so `clones`/`deps`
/// produce non-empty results.
fn build_python_project_multi_file() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    write(
        &root.join("a.py"),
        r#"
import b

def hello():
    return b.world()

def long_fn():
    if True:
        for i in range(10):
            if i > 5:
                print(i)
    return 0
"#,
    );
    write(
        &root.join("b.py"),
        r#"
def world():
    return 'world'

def long_fn():
    if True:
        for i in range(10):
            if i > 5:
                print(i)
    return 0
"#,
    );
    dir
}

// =============================================================================
// BUG-5: impact finds function-as-value callers
// =============================================================================

#[test]
fn impact_finds_function_as_value_callers() {
    let dir = build_python_project_with_value_uses();
    let root = dir.path();

    let out = tldr_cmd()
        .args([
            "impact",
            "helper",
            root.to_str().unwrap(),
            "--format",
            "json",
            "-q",
        ])
        .output()
        .expect("tldr impact failed");
    assert!(out.status.success(), "tldr impact must exit 0");
    let body: Value = serde_json::from_slice(&out.stdout).expect("impact JSON");

    let targets = body["targets"]
        .as_object()
        .expect("targets must be an object");
    assert!(!targets.is_empty(), "must find at least one target");

    // The target's caller_count must include the value uses.
    // Direct call (use_direct) + 5 value uses (use_return, use_assign,
    // use_kwarg, use_positional, Config). Tolerate >= 4 (if some
    // value-uses dedupe, we still want substantially more than 1).
    let mut max_callers = 0u64;
    for (_k, v) in targets {
        let cc = v["caller_count"].as_u64().unwrap_or(0);
        if cc > max_callers {
            max_callers = cc;
        }
    }
    assert!(
        max_callers >= 4,
        "BUG-5: helper should have >= 4 callers (1 direct + value uses), got {}. \
         body={}",
        max_callers,
        serde_json::to_string_pretty(&body).unwrap()
    );

    // The note must NOT claim "no callers found" when we found callers.
    for (_k, v) in targets {
        if let Some(note) = v["note"].as_str() {
            if v["caller_count"].as_u64().unwrap_or(0) > 0 {
                assert!(
                    !note.contains("no callers found"),
                    "BUG-5: target with caller_count > 0 must not carry \
                     'no callers found' note. Got: {}",
                    note
                );
            }
        }
    }
}

// =============================================================================
// BUG-7: complexity and cognitive agree on the same function
// =============================================================================

fn cognitive_via_complexity(file: &Path, func: &str) -> u32 {
    let out = tldr_cmd()
        .args([
            "complexity",
            file.to_str().unwrap(),
            func,
            "--format",
            "json",
            "-q",
        ])
        .output()
        .expect("tldr complexity failed");
    assert!(
        out.status.success(),
        "tldr complexity must exit 0, got {:?} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let body: Value = serde_json::from_slice(&out.stdout).expect("complexity JSON");
    body["cognitive"]
        .as_u64()
        .expect("complexity.cognitive must be u64") as u32
}

fn cognitive_via_cognitive(file: &Path, func: &str) -> u32 {
    let out = tldr_cmd()
        .args([
            "cognitive",
            file.to_str().unwrap(),
            "--format",
            "json",
            "-q",
        ])
        .output()
        .expect("tldr cognitive failed");
    assert!(out.status.success(), "tldr cognitive must exit 0");
    let body: Value = serde_json::from_slice(&out.stdout).expect("cognitive JSON");
    let funcs = body["functions"].as_array().expect("functions array");
    for f in funcs {
        if f["name"].as_str() == Some(func) {
            return f["cognitive"]
                .as_u64()
                .expect("cognitive.cognitive must be u64") as u32;
        }
    }
    panic!(
        "function {} not found in `tldr cognitive` output: {}",
        func,
        serde_json::to_string_pretty(&body).unwrap()
    );
}

#[test]
fn complexity_and_cognitive_agree_on_same_function_python() {
    let dir = build_python_project_for_cognitive_parity();
    let file = dir.path().join("complex.py");
    for func in ["trivial", "with_nested", "with_logic"] {
        let a = cognitive_via_complexity(&file, func);
        let b = cognitive_via_cognitive(&file, func);
        assert_eq!(
            a, b,
            "BUG-7: tldr complexity and tldr cognitive disagree on python `{}`: \
             complexity={} cognitive={}",
            func, a, b
        );
    }
}

#[test]
fn complexity_and_cognitive_agree_on_same_function_js() {
    let dir = build_js_project_for_cognitive_parity();
    let file = dir.path().join("svc.js");
    for func in ["plain", "nested", "logical"] {
        let a = cognitive_via_complexity(&file, func);
        let b = cognitive_via_cognitive(&file, func);
        assert_eq!(
            a, b,
            "BUG-7: tldr complexity and tldr cognitive disagree on js `{}`: \
             complexity={} cognitive={}",
            func, a, b
        );
    }
}

#[test]
fn complexity_emits_max_nesting_field_renamed_from_nesting_depth() {
    // BUG-7 paperwork: ComplexityMetrics now exposes `max_nesting`
    // (renamed from `nesting_depth`) so it matches the cognitive
    // command's field name.
    let dir = build_python_project_for_cognitive_parity();
    let file = dir.path().join("complex.py");
    let out = tldr_cmd()
        .args([
            "complexity",
            file.to_str().unwrap(),
            "with_nested",
            "--format",
            "json",
            "-q",
        ])
        .output()
        .expect("tldr complexity failed");
    assert!(out.status.success());
    let body: Value = serde_json::from_slice(&out.stdout).expect("complexity JSON");
    assert!(
        body.get("max_nesting").is_some(),
        "BUG-7: complexity output must expose `max_nesting`, got keys={:?}",
        body.as_object().map(|o| o.keys().collect::<Vec<_>>())
    );
    assert!(
        body.get("nesting_depth").is_none(),
        "BUG-7: complexity output must NOT expose the old `nesting_depth` \
         alongside `max_nesting` (would create ambiguity)"
    );
}

// =============================================================================
// BUG-8: path canonicalization is consistent across commands
// =============================================================================

#[test]
fn path_canonicalization_consistent_across_commands() {
    // BUG-8: every command that emits a `file` field in JSON for a
    // single-file analysis must echo back the user-supplied path,
    // not a canonicalised one. On macOS this means `/tmp/...` must
    // NOT be rewritten to `/private/tmp/...`.
    //
    // We use tempfile (which lives under `/var/folders/...` on macOS,
    // and is itself canonical) — so the spec we assert is exact
    // string equality with the input we passed.
    let dir = build_python_project_for_cognitive_parity();
    let file = dir.path().join("complex.py");
    let file_str = file.to_str().unwrap().to_string();

    fn extract_file(value: &Value) -> Option<String> {
        if let Some(s) = value.get("file").and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
        if let Some(arr) = value.get("functions").and_then(|v| v.as_array()) {
            if let Some(first) = arr.first() {
                if let Some(s) = first.get("file").and_then(|v| v.as_str()) {
                    return Some(s.to_string());
                }
            }
        }
        None
    }

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
        serde_json::from_slice(&out.stdout).expect("JSON")
    }

    // halstead (whole-file) — emits .functions[].file
    let h = run_json(&["halstead", &file_str]);
    let hf = extract_file(&h).expect("halstead must emit file");
    assert_eq!(
        hf, file_str,
        "BUG-8: halstead must emit the user-supplied path, got {}",
        hf
    );

    // cognitive (whole-file) — emits .functions[].file
    let c = run_json(&["cognitive", &file_str]);
    let cf = extract_file(&c).expect("cognitive must emit file");
    assert_eq!(
        cf, file_str,
        "BUG-8: cognitive must emit the user-supplied path, got {}",
        cf
    );

    // reaching-defs (per-function) — emits .file
    let r = run_json(&["reaching-defs", &file_str, "with_nested"]);
    let rf = extract_file(&r).expect("reaching-defs must emit file");
    assert_eq!(
        rf, file_str,
        "BUG-8: reaching-defs must emit the user-supplied path, got {}",
        rf
    );

    // dead-stores (per-function) — emits .file
    let d = run_json(&["dead-stores", &file_str, "with_nested"]);
    let df = extract_file(&d).expect("dead-stores must emit file");
    assert_eq!(
        df, file_str,
        "BUG-8: dead-stores must emit the user-supplied path, got {}",
        df
    );

    // resources (per-function) — emits .file
    let res = run_json(&["resources", &file_str, "with_nested"]);
    let resf = extract_file(&res).expect("resources must emit file");
    assert_eq!(
        resf, file_str,
        "BUG-8: resources must emit the user-supplied path, got {}",
        resf
    );
}

// =============================================================================
// BUG-14: project-root field is canonical (`root`) across commands
// =============================================================================

#[test]
fn project_root_field_name_canonical() {
    let dir = build_python_project_multi_file();
    let root_path = dir.path().to_str().unwrap().to_string();

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
        serde_json::from_slice(&out.stdout).expect("JSON")
    }

    // Each whole-project command must expose a top-level `root` key.
    let cmds: &[&str] = &[
        "structure",
        "deps",
        "clones",
        "health",
        "secure",
        "inheritance",
    ];
    for cmd in cmds {
        let v = run_json(&[cmd, &root_path]);
        let root_val = v.get("root").and_then(|x| x.as_str());
        assert!(
            root_val.is_some(),
            "BUG-14: `{}` must expose `root` field, got keys={:?}",
            cmd,
            v.as_object().map(|o| o.keys().collect::<Vec<_>>())
        );
        // Legacy field names must not appear alongside (would create
        // ambiguity for downstream consumers).
        assert!(
            v.get("path").is_none() || cmd == &"deps",
            "BUG-14: `{}` must not expose legacy `path` alongside `root`",
            cmd
        );
        assert!(
            v.get("project_path").is_none(),
            "BUG-14: `{}` must not expose legacy `project_path` alongside `root`",
            cmd
        );
    }
}

#[test]
fn function_name_field_canonical() {
    // Build a project with one function we can address from every
    // function-scoped command.
    let dir = build_python_project_for_cognitive_parity();
    let file = dir.path().join("complex.py");
    let file_str = file.to_str().unwrap().to_string();

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
        serde_json::from_slice(&out.stdout).expect("JSON")
    }

    // Per-function commands. `slice` needs a line; we use the first
    // line of `with_nested` (the function we care about).
    let line = "5"; // approximate — slice tolerates any line inside
    let cmds: Vec<(&str, Vec<String>)> = vec![
        (
            "slice",
            vec![file_str.clone(), "with_nested".into(), line.into()],
        ),
        ("dead-stores", vec![file_str.clone(), "with_nested".into()]),
        ("resources", vec![file_str.clone(), "with_nested".into()]),
        (
            "reaching-defs",
            vec![file_str.clone(), "with_nested".into()],
        ),
        ("taint", vec![file_str.clone(), "with_nested".into()]),
        ("explain", vec![file_str.clone(), "with_nested".into()]),
    ];

    for (cmd, args) in cmds {
        let mut all: Vec<&str> = vec![cmd];
        for a in &args {
            all.push(a);
        }
        let v = run_json(&all);
        let func_val = v.get("function").and_then(|x| x.as_str());
        assert!(
            func_val.is_some(),
            "BUG-14: `{}` must expose `function` field, got keys={:?}",
            cmd,
            v.as_object().map(|o| o.keys().collect::<Vec<_>>())
        );
        // Legacy `function_name` must not be present alongside `function`.
        assert!(
            v.get("function_name").is_none(),
            "BUG-14: `{}` must not expose legacy `function_name` alongside `function`",
            cmd
        );
    }
}

//! M1a VAL-001a — process_block must NOT propagate taint via substring matching
//! on unrelated tokens (e.g., a tainted variable name appearing inside a string
//! literal, comment, or as a substring of an identifier).
//!
//! Pre-fix RED: `stmt.contains(tv.as_str())` at taint.rs:3761 (Definition arm)
//! and :3780 (Update arm) matches `x` anywhere in the statement text — including
//! inside string literals — and incorrectly propagates taint from `x = input()`
//! to `foo = "context for x is logged"`.
//!
//! Post-fix GREEN: VarRef-based per-line use lookup consults the DFG's emitted
//! `RefType::Use` refs at the line. The DFG correctly does NOT emit a Use for
//! a name that appears only inside a string literal, so `foo` stays untainted.
//!
//! Note on fixture choice: the original triage brief proposed `foo = bar.x()`
//! as the RED fixture, but the Python DFG over-emits a `Use("x", line)` for
//! the bare method-name token in `bar.x()` — that is a separate DFG bug
//! (out-of-scope per STOP #3) which would mask the M1a fix. The string-literal
//! fixture below isolates the substring-shadowing bug cleanly: the DFG omits
//! Use refs for tokens inside string literals, so the VarRef-based check
//! correctly finds nothing tainted on the RHS.
//!
//! This file also covers Q-M1-D — pre-v0.3.0 there was NO manually-constructed
//! test exercising the `RefType::Update` arm of `process_block`. The augmented
//! assignment fixture (`x += user_input`) closes that coverage gap.

use std::collections::HashMap;

use tldr_core::{compute_taint, get_cfg_context, get_dfg_context, Language};

fn statements_from(src: &str) -> HashMap<u32, String> {
    src.lines()
        .enumerate()
        .map(|(i, text)| ((i + 1) as u32, text.to_string()))
        .collect()
}

/// Negative case (RED before fix): substring shadowing must NOT propagate taint
/// when the tainted variable's name appears only inside a string literal.
///
/// Pre-fix, `stmt.contains("x")` matches inside the string `"context for x is
/// logged"` on line 3 and the engine taints `foo`. The eval(foo) sink at line
/// 4 then yields a flow. Post-fix, the DFG emits no `RefType::Use` for `x` at
/// line 3 (the only `x` token is inside a string literal), so the VarRef-based
/// check returns false and `foo` is correctly untainted.
#[test]
fn taint_does_not_propagate_via_substring_in_string_literal() {
    let src = "\
def handler():
    x = input()
    foo = \"context for x is logged\"
    eval(foo)
";
    let cfg = get_cfg_context(src, "handler", Language::Python).expect("cfg");
    let dfg = get_dfg_context(src, "handler", Language::Python).expect("dfg");
    let result = compute_taint(&cfg, &dfg.refs, &statements_from(src), Language::Python)
        .expect("taint analysis must succeed");

    // Any flow whose sink variable is `foo` indicates the substring shadowing bug.
    let foo_flows: Vec<_> = result
        .flows
        .iter()
        .filter(|f| f.sink.var == "foo")
        .collect();

    assert!(
        foo_flows.is_empty(),
        "expected foo NOT in tainted set, but found {} flow(s) — substring shadowing bug. \
         Flows: {:?}",
        foo_flows.len(),
        foo_flows
    );
}

/// Negative case (variant): substring shadowing inside a longer identifier
/// (`compute_x_value`) must not propagate taint. Pre-fix, `stmt.contains("x")`
/// matches the `x` substring in `compute_x_value`, tainting `foo`. Post-fix,
/// the DFG emits `Use(compute_x_value)` and `Use(bar)` but no `Use(x)` on
/// line 3, so `foo` stays untainted.
#[test]
fn taint_does_not_propagate_via_substring_in_identifier() {
    let src = "\
def handler():
    x = input()
    foo = bar.compute_x_value()
    eval(foo)
";
    let cfg = get_cfg_context(src, "handler", Language::Python).expect("cfg");
    let dfg = get_dfg_context(src, "handler", Language::Python).expect("dfg");
    let result = compute_taint(&cfg, &dfg.refs, &statements_from(src), Language::Python)
        .expect("taint analysis must succeed");

    let foo_flows: Vec<_> = result
        .flows
        .iter()
        .filter(|f| f.sink.var == "foo")
        .collect();

    assert!(
        foo_flows.is_empty(),
        "expected foo NOT in tainted set, but found {} flow(s) — substring shadowing bug \
         (x matched as substring of compute_x_value). Flows: {:?}",
        foo_flows.len(),
        foo_flows
    );
}

/// Positive regression guard: real assignment propagation must still work
/// after the substring engine is replaced.
#[test]
fn taint_still_propagates_through_real_assignment() {
    let src = "\
def handler():
    x = input()
    y = x
    eval(y)
";
    let cfg = get_cfg_context(src, "handler", Language::Python).expect("cfg");
    let dfg = get_dfg_context(src, "handler", Language::Python).expect("dfg");
    let result = compute_taint(&cfg, &dfg.refs, &statements_from(src), Language::Python)
        .expect("taint analysis must succeed");

    assert!(
        !result.flows.is_empty(),
        "regression: real x -> y -> eval(y) propagation lost; got 0 flows"
    );
}

/// Q-M1-D: Update-arm coverage. Pre-v0.3.0 no manually-constructed test
/// exercised `RefType::Update`. Augmented-assignment `x += user_input` emits
/// a VarRef with `ref_type = Update`. Post-fix, the same `rhs_uses_tainted`
/// helper is invoked from both the Definition and Update arms; the test must
/// pass under VarRef semantics (DFG emits a Use for `user_input` on the line).
#[test]
fn taint_propagates_through_augmented_assignment() {
    let src = "\
def handler():
    user_input = input()
    x = 0
    x += user_input
    eval(x)
";
    let cfg = get_cfg_context(src, "handler", Language::Python).expect("cfg");
    let dfg = get_dfg_context(src, "handler", Language::Python).expect("dfg");
    let result = compute_taint(&cfg, &dfg.refs, &statements_from(src), Language::Python)
        .expect("taint analysis must succeed");

    assert!(
        !result.flows.is_empty(),
        "regression: x += user_input (Update arm) must propagate taint to eval(x); got {} flows",
        result.flows.len()
    );
}

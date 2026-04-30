//! sanitizer-removal-v1 — M1 RED integration tests (regular dispatch path).
//!
//! Per-language sanitize-breaks-flow regression coverage for all 16 language
//! banks (Lua/Luau share LUA_PATTERNS so Lua covers both). Three categories:
//!
//! 1. **`<lang>_<sanitizer>_sanitizer_truncates_flow_via_compute_taint`** —
//!    one per language (19 total: 16 languages + Ruby `Rack::Utils.escape_html`
//!    parity-gap regression guard + PHP `(int)` cast parity-gap regression
//!    guard + PHP `intval(...)` call form). Each fixture introduces a
//!    sanitized intermediate (`safe = sanitize(raw)`) and asserts
//!    `result.sanitized_vars.contains("safe")` — the canonical signal that
//!    the sanitizer dispatch fired. At HEAD (regex bank still active) all 19
//!    PASS as additive coverage proof.
//!
//! 2. **`<lang>_<sanitizer>_in_string_literal_does_not_sanitize`** — 16
//!    string-literal regression guards (closes-#24-shaped FP class extended
//!    to sanitizers). Source code contains the sanitizer call name INSIDE a
//!    string literal — a sanitizer dispatch must NOT fire. Asserts
//!    `result.sanitized_vars.is_empty()`. At HEAD (regex bank active) some
//!    PASS and some FAIL deterministically (regex bank does NOT respect
//!    string-literal boundaries). Documented in `reports/M1-red-capture.txt`.
//!    ALL must PASS post-M4.
//!
//! Total: 19 sanitize-breaks-flow + 16 string-literal regression guards = 35.
//!
//! Mirrored under the AST-only harness in
//! `sanitize_breaks_flow_ast_only_harness.rs` (16 tests; RED gate for M2).
//!
//! Test names use mixed case (e.g. `parseInt`, `Number`, `Int`, `toInt`,
//! `String_to_integer`) to mirror the canonical sanitizer call shape per
//! language exactly as enumerated in the dispatch-contract M1 names list
//! (`continuum/autonomous/sanitizer-removal-v1-plan/dispatch-contract.json`
//! lines 47-81). The file-level allow keeps clippy quiet on the
//! contract-mandated names.
//!
//! ## Assertion strategy: `sanitized_vars` instead of `flows.is_empty()`
//!
//! `result.flows.is_empty()` is an indirect proxy for sanitizer detection
//! and depends on the source→sink flow-construction step succeeding. Some
//! per-language source/sink AST shapes detect both endpoints but do not yet
//! materialize a `TaintFlow` (e.g., Swift Process.launchPath assignment;
//! C# Process.Start receiver chain). Those flow-construction gaps are
//! ORTHOGONAL to the sanitizer dispatch under test. We assert directly on
//! `sanitized_vars` — the HashSet that both the regex and AST sanitizer
//! dispatches write into when they detect a sanitizer call. This is the
//! tightest, most discriminative property that proves sanitizer dispatch
//! fired without entanglement with flow-construction precision.
//!
//! Pre-M2 with regex bank active: `sanitized_vars` contains the safe-var
//! name across all 16 languages (regex catches each canonical sanitizer).
//! Pre-M2 with AST_ONLY_TEST_MODE: `sanitized_vars.is_empty()` because
//! detect_sanitizer_ast is dead code at the worklist call sites — the M2
//! wiring resolves this. (See `sanitize_breaks_flow_ast_only_harness.rs`.)

#![allow(non_snake_case)]

use tldr_core::Language;

#[path = "common/integ_helpers.rs"]
mod common;

use common::analyze_with_ssa;

/// Per-language sanitizer integration tests run SSA-off to match the
/// `rr_baseline_per_language_test` convention (proven working source/sink
/// fixture shapes). SSA-on has per-language gaps that are orthogonal to
/// the sanitizer dispatch path under test.
fn analyze(src: &str, lang: Language, fn_name: &str) -> tldr_core::TaintInfo {
    analyze_with_ssa(src, lang, fn_name, /* use_ssa */ false)
}

// ============================================================================
// Section 1: <lang>_<sanitizer>_sanitizer_truncates_flow_via_compute_taint
// 19 tests (one per language + 2 PHP shapes + 1 Ruby Rack::Utils gap guard)
// Each asserts `sanitized_vars.contains("safe")`.
// ============================================================================

#[test]
fn python_int_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
def f():
    raw = input(\"> \")
    safe = int(raw)
    eval(str(safe))
";
    let result = analyze(src, Language::Python, "f");
    assert!(
        result.sanitized_vars.contains("safe"),
        "int(raw) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn typescript_parseInt_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
function handler(req, res) {
    const x = req.body;
    const safe = parseInt(x);
    eval(String(safe));
}
";
    let result = analyze(src, Language::TypeScript, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "parseInt(x) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn javascript_Number_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
function handler(req, res) {
    var x = req.body;
    var safe = Number(x);
    eval(String(safe));
}
";
    let result = analyze(src, Language::JavaScript, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "Number(x) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn go_strconv_atoi_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
package main
import (
    \"os\"
    \"os/exec\"
    \"strconv\"
)
func f() {
    raw := os.Getenv(\"X\")
    safe, _ := strconv.Atoi(raw)
    exec.Command(\"sh\", \"-c\", strconv.Itoa(safe)).Run()
}
";
    let result = analyze(src, Language::Go, "f");
    assert!(
        result.sanitized_vars.contains("safe"),
        "strconv.Atoi(raw) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn java_integer_parseInt_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
public class H {
    public void handle(HttpServletRequest request) throws Exception {
        String cmd = request.getParameter(\"c\");
        int safe = Integer.parseInt(cmd);
        Runtime.getRuntime().exec(Integer.toString(safe));
    }
}
";
    let result = analyze(src, Language::Java, "handle");
    assert!(
        result.sanitized_vars.contains("safe"),
        "Integer.parseInt(cmd) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn rust_parse_turbofish_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
fn f() {
    let raw = std::env::var(\"X\").unwrap();
    let safe: i32 = raw.parse::<i32>().unwrap();
    std::process::Command::new(safe.to_string()).output().unwrap();
}
";
    let result = analyze(src, Language::Rust, "f");
    assert!(
        result.sanitized_vars.contains("safe"),
        "parse::<i32>() should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn c_atoi_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
#include <stdio.h>
#include <stdlib.h>
void handler(void) {
    char buf[256];
    fgets(buf, sizeof(buf), stdin);
    int safe = atoi(buf);
    char out[32];
    sprintf(out, \"%d\", safe);
    system(out);
}
";
    let result = analyze(src, Language::C, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "atoi(buf) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn cpp_std_stoi_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
#include <iostream>
#include <cstdlib>
void handler() {
    std::string s;
    std::cin >> s;
    int safe = std::stoi(s);
    system(std::to_string(safe).c_str());
}
";
    let result = analyze(src, Language::Cpp, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "std::stoi(s) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn ruby_to_i_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
def handler
    cmd = gets
    safe = cmd.to_i
    system(safe.to_s)
end
";
    let result = analyze(src, Language::Ruby, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "cmd.to_i should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

/// Ruby `Rack::Utils.escape_html` parity-gap regression guard. Per premortem
/// A1, `RUBY_AST_SANITIZERS` already has the raw-fallback at taint.rs:2416 —
/// so the AST-only mirror in `sanitize_breaks_flow_ast_only_harness.rs`
/// transitions RED → GREEN at M2 wiring (NOT at M3). Regular `analyze`
/// PASSES at HEAD (regex bank active).
#[test]
fn ruby_rack_utils_escape_html_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
def handler
    cmd = gets
    safe = Rack::Utils.escape_html(cmd)
    system(safe)
end
";
    let result = analyze(src, Language::Ruby, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "Rack::Utils.escape_html(cmd) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn kotlin_toInt_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
fun handler() {
    val line = readLine() ?: return
    val safe = line.toInt()
    Runtime.getRuntime().exec(safe.toString())
}
";
    let result = analyze(src, Language::Kotlin, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "line.toInt() should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn swift_Int_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
import Foundation
func handler() {
    guard let line = readLine() else { return }
    let safe = Int(line) ?? 0
    let p = Process()
    p.launchPath = String(safe)
    try? p.run()
}
";
    let result = analyze(src, Language::Swift, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "Int(line) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn csharp_int_parse_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
using System.Diagnostics;
public class H {
    public void Handle() {
        var s = System.Console.ReadLine();
        int safe = int.Parse(s);
        Process.Start(safe.ToString());
    }
}
";
    let result = analyze(src, Language::CSharp, "Handle");
    assert!(
        result.sanitized_vars.contains("safe"),
        "int.Parse(s) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn scala_toInt_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
object H {
  def handler(): Unit = {
    val cmd = scala.io.StdIn.readLine()
    val safe = cmd.toInt
    Runtime.getRuntime.exec(safe.toString)
  }
}
";
    let result = analyze(src, Language::Scala, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "cmd.toInt should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn php_intval_call_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
<?php
function handler() {
    $cmd = $_GET['c'];
    $safe = intval($cmd);
    system(strval($safe));
}
";
    let result = analyze(src, Language::Php, "handler");
    assert!(
        result.sanitized_vars.contains("safe") || result.sanitized_vars.contains("$safe"),
        "intval($cmd) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

/// PHP `(int)` cast_expression parity-gap regression guard. Per premortem A1,
/// `PHP_AST_SANITIZERS` already has the raw-fallback at taint.rs:2717 — so
/// the AST-only mirror transitions RED → GREEN at M2 wiring (NOT at M3).
/// Regular `analyze` PASSES at HEAD (regex bank active).
#[test]
fn php_int_cast_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
<?php
function handler() {
    $cmd = $_GET['c'];
    $safe = (int)$cmd;
    system(strval($safe));
}
";
    let result = analyze(src, Language::Php, "handler");
    assert!(
        result.sanitized_vars.contains("safe") || result.sanitized_vars.contains("$safe"),
        "(int)$cmd should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn lua_tonumber_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
function handler()
    local cmd = io.read()
    local safe = tonumber(cmd)
    os.execute(tostring(safe))
end
";
    let result = analyze(src, Language::Lua, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "tonumber(cmd) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn elixir_String_to_integer_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
defmodule F do
    def handler do
        cmd = IO.gets(\"> \")
        safe = String.to_integer(cmd)
        System.cmd(Integer.to_string(safe), [])
    end
end
";
    let result = analyze(src, Language::Elixir, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "String.to_integer(cmd) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn ocaml_int_of_string_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
let handler () =
  let cmd = read_line () in
  let safe = int_of_string cmd in
  ignore (Sys.command (string_of_int safe))
";
    let result = analyze(src, Language::Ocaml, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "int_of_string cmd should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

// ============================================================================
// Section 2: <lang>_<sanitizer>_in_string_literal_does_not_sanitize
// 16 tests (closes-#24-shaped FP class extended to sanitizers).
// Asserts `sanitized_vars.is_empty()` — sanitizer text appears INSIDE a
// string literal and must NOT trigger sanitization. At HEAD (regex bank
// active) some FAIL deterministically because the regex matches the
// sanitizer name inside a string literal. Post-M4 ALL PASS.
// ============================================================================

#[test]
fn python_int_in_string_literal_does_not_sanitize() {
    let src = "\
def f():
    raw = input(\"> \")
    msg = \"use int(x) to convert\"
    eval(raw)
";
    let result = analyze(src, Language::Python, "f");
    assert!(
        result.sanitized_vars.is_empty(),
        "String-literal containing 'int(x)' must NOT trigger sanitization; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn typescript_parseInt_in_string_literal_does_not_sanitize() {
    let src = "\
function handler(req, res) {
    const x = req.body;
    const msg = \"call parseInt(x) to convert\";
    eval(x);
}
";
    let result = analyze(src, Language::TypeScript, "handler");
    assert!(
        result.sanitized_vars.is_empty(),
        "String-literal containing 'parseInt(x)' must NOT trigger sanitization; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn go_strconv_atoi_in_string_literal_does_not_sanitize() {
    let src = "\
package main
import (
    \"os\"
    \"os/exec\"
)
func f() {
    raw := os.Getenv(\"X\")
    msg := \"use strconv.Atoi(x) to convert\"
    _ = msg
    exec.Command(\"sh\", \"-c\", raw).Run()
}
";
    let result = analyze(src, Language::Go, "f");
    assert!(
        result.sanitized_vars.is_empty(),
        "String-literal containing 'strconv.Atoi(x)' must NOT trigger sanitization; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn java_integer_parseInt_in_string_literal_does_not_sanitize() {
    let src = "\
public class H {
    public void handle(HttpServletRequest request) throws Exception {
        String cmd = request.getParameter(\"c\");
        String msg = \"call Integer.parseInt(cmd)\";
        Runtime.getRuntime().exec(cmd);
    }
}
";
    let result = analyze(src, Language::Java, "handle");
    assert!(
        result.sanitized_vars.is_empty(),
        "String-literal containing 'Integer.parseInt' must NOT trigger sanitization; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn rust_parse_in_string_literal_does_not_sanitize() {
    let src = "\
fn f() {
    let raw = std::env::var(\"X\").unwrap();
    let msg = \"call .parse::<i32>() on raw\";
    let _ = msg;
    std::process::Command::new(raw).output().unwrap();
}
";
    let result = analyze(src, Language::Rust, "f");
    assert!(
        result.sanitized_vars.is_empty(),
        "String-literal containing 'parse::<i32>' must NOT trigger sanitization; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn c_atoi_in_string_literal_does_not_sanitize() {
    let src = "\
#include <stdio.h>
#include <stdlib.h>
void handler(void) {
    char buf[256];
    fgets(buf, sizeof(buf), stdin);
    char* msg = \"use atoi(buf) to convert\";
    (void)msg;
    system(buf);
}
";
    let result = analyze(src, Language::C, "handler");
    assert!(
        result.sanitized_vars.is_empty(),
        "String-literal containing 'atoi(buf)' must NOT trigger sanitization; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn cpp_std_stoi_in_string_literal_does_not_sanitize() {
    let src = "\
#include <iostream>
#include <cstdlib>
void handler() {
    std::string s;
    std::cin >> s;
    std::string msg = \"use std::stoi to convert\";
    system(s.c_str());
}
";
    let result = analyze(src, Language::Cpp, "handler");
    assert!(
        result.sanitized_vars.is_empty(),
        "String-literal containing 'std::stoi' must NOT trigger sanitization; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn ruby_to_i_in_string_literal_does_not_sanitize() {
    let src = "\
def handler
    cmd = gets
    msg = \"call cmd.to_i to convert\"
    system(cmd)
end
";
    let result = analyze(src, Language::Ruby, "handler");
    assert!(
        result.sanitized_vars.is_empty(),
        "String-literal containing '.to_i' must NOT trigger sanitization; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn kotlin_toInt_in_string_literal_does_not_sanitize() {
    let src = "\
fun handler() {
    val line = readLine() ?: return
    val msg = \"call line.toInt() to convert\"
    Runtime.getRuntime().exec(line)
}
";
    let result = analyze(src, Language::Kotlin, "handler");
    assert!(
        result.sanitized_vars.is_empty(),
        "String-literal containing '.toInt()' must NOT trigger sanitization; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn swift_Int_in_string_literal_does_not_sanitize() {
    let src = "\
import Foundation
func handler() {
    guard let line = readLine() else { return }
    let msg = \"call Int(line) to convert\"
    let p = Process()
    p.launchPath = line
    try? p.run()
}
";
    let result = analyze(src, Language::Swift, "handler");
    assert!(
        result.sanitized_vars.is_empty(),
        "String-literal containing 'Int(line)' must NOT trigger sanitization; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn csharp_int_parse_in_string_literal_does_not_sanitize() {
    let src = "\
using System.Diagnostics;
public class H {
    public void Handle() {
        var s = System.Console.ReadLine();
        string msg = \"call int.Parse(s)\";
        Process.Start(s);
    }
}
";
    let result = analyze(src, Language::CSharp, "Handle");
    assert!(
        result.sanitized_vars.is_empty(),
        "String-literal containing 'int.Parse' must NOT trigger sanitization; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn scala_toInt_in_string_literal_does_not_sanitize() {
    let src = "\
object H {
  def handler(): Unit = {
    val cmd = scala.io.StdIn.readLine()
    val msg = \"call cmd.toInt to convert\"
    Runtime.getRuntime.exec(cmd)
  }
}
";
    let result = analyze(src, Language::Scala, "handler");
    assert!(
        result.sanitized_vars.is_empty(),
        "String-literal containing '.toInt' must NOT trigger sanitization; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn php_intval_in_string_literal_does_not_sanitize() {
    let src = "\
<?php
function handler() {
    $cmd = $_GET['c'];
    $msg = \"call intval to convert\";
    system($cmd);
}
";
    let result = analyze(src, Language::Php, "handler");
    assert!(
        result.sanitized_vars.is_empty(),
        "String-literal containing 'intval' must NOT trigger sanitization; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn lua_tonumber_in_string_literal_does_not_sanitize() {
    let src = "\
function handler()
    local cmd = io.read()
    local msg = \"call tonumber(cmd) to convert\"
    os.execute(cmd)
end
";
    let result = analyze(src, Language::Lua, "handler");
    assert!(
        result.sanitized_vars.is_empty(),
        "String-literal containing 'tonumber' must NOT trigger sanitization; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn elixir_String_to_integer_in_string_literal_does_not_sanitize() {
    let src = "\
defmodule F do
    def handler do
        cmd = IO.gets(\"> \")
        _msg = \"call String.to_integer(cmd)\"
        System.cmd(cmd, [])
    end
end
";
    let result = analyze(src, Language::Elixir, "handler");
    assert!(
        result.sanitized_vars.is_empty(),
        "String-literal containing 'String.to_integer' must NOT trigger sanitization; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn ocaml_int_of_string_in_string_literal_does_not_sanitize() {
    let src = "\
let handler () =
  let cmd = read_line () in
  let _msg = \"call int_of_string cmd\" in
  ignore (Sys.command cmd)
";
    let result = analyze(src, Language::Ocaml, "handler");
    assert!(
        result.sanitized_vars.is_empty(),
        "String-literal containing 'int_of_string' must NOT trigger sanitization; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

// ---------------------------------------------------------------------------
// sanitizer-removal-v1 M3 (Gap 2): Zod-style validator parity tests.
//
// The TYPESCRIPT_PATTERNS regex bank has `\.(parse|safeParse)\s*\(` (Numeric).
// Pre-M3, TYPESCRIPT_AST_SANITIZERS lacked an equivalent; with M2 wiring in
// place and M4 about to delete the regex bank, the AST bank now carries the
// `("*", "parse")` / `("*", "safeParse")` wildcard entries. These tests
// exercise the AST detection path on the regular `compute_taint_with_tree`
// pipeline (the ast_only mirrors live in `sanitize_breaks_flow_ast_only_harness.rs`).
// ---------------------------------------------------------------------------

#[test]
fn typescript_zod_parse_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
function handler(req, res) {
    const tainted = req.body;
    const safe = schema.parse(tainted);
    eval(String(safe));
}
";
    let result = analyze(src, Language::TypeScript, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "schema.parse(tainted) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn typescript_zod_safeParse_sanitizer_truncates_flow_via_compute_taint() {
    let src = "\
function handler(req, res) {
    const tainted = req.body;
    const safe = schema.safeParse(tainted);
    eval(String(safe));
}
";
    let result = analyze(src, Language::TypeScript, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "schema.safeParse(tainted) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

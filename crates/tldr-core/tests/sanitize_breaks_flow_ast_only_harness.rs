//! sanitizer-removal-v1 — M1 RED integration tests (AST-only harness).
//!
//! Sixteen mirror tests that exercise the same per-language sanitize-breaks-
//! flow fixtures as `sanitize_breaks_flow_per_language.rs` but route through
//! the `analyze_ast_only` helper at `common/integ_helpers.rs:94`. That helper
//! activates `AstOnlyTestModeGuard` (taint.rs:63) which short-circuits BOTH
//! sources/sinks AND — post-M1 3-LOC extension at taint.rs:1096 — sanitizers
//! to bank-empty mode. The only detection paths that can produce sanitizers
//! while the guard is alive are the AST sanitizer dispatch paths inside
//! `compute_taint_with_tree` (`detect_sanitizer_ast` and the M2-incoming
//! `build_sanitizer_ast_index` walk-once helper).
//!
//! ## RED gate
//!
//! At HEAD pre-M2 these tests FAIL deterministically:
//! - `analyze_ast_only` activates the guard → regex sanitizer bank short-
//!   circuited (post-M1 3-LOC extension at taint.rs:1096 returns None).
//! - AST sanitizer dispatch is NOT YET wired into compute_taint_with_tree —
//!   `detect_sanitizer_ast` exists at taint.rs:3490 but has zero call sites
//!   (verified in investigation.json).
//! - Net effect: NO sanitizer is detected, `result.sanitized_vars.is_empty()`,
//!   the assertion `result.sanitized_vars.contains("safe")` fails.
//!
//! M2 wires `build_sanitizer_ast_index` into `compute_taint_with_tree` and 14
//! of these 16 tests transition RED → GREEN. The remaining 2 (Ruby
//! Rack::Utils.escape_html and PHP `(int)` cast) are expected to ALSO
//! transition at M2 (premortem A1: both raw-fallback entries already present
//! at HEAD: `RUBY_AST_SANITIZERS` taint.rs:2416, `PHP_AST_SANITIZERS`
//! taint.rs:2717), so M3 is parity-AUDIT-only with zero additions expected.
//!
//! ## Why the assertion is `sanitized_vars.contains("safe")`
//!
//! See `sanitize_breaks_flow_per_language.rs` doc comment — same rationale:
//! `sanitized_vars` is the canonical signal that a sanitizer dispatch fired,
//! independent of whether per-language source/sink AST shapes manage to
//! materialize a `TaintFlow`. This decouples the sanitizer dispatch test
//! (the property under test) from the orthogonal flow-construction precision.
//!
//! Test names use mixed case to mirror the canonical sanitizer call shape
//! per language exactly (per dispatch-contract M1 names list).

#![allow(non_snake_case)]

use tldr_core::Language;

#[path = "common/integ_helpers.rs"]
mod common;

use common::analyze_ast_only;

#[test]
fn python_int_sanitizer_truncates_flow_ast_only() {
    let src = "\
def f():
    raw = input(\"> \")
    safe = int(raw)
    eval(str(safe))
";
    let result = analyze_ast_only(src, Language::Python, "f");
    assert!(
        result.sanitized_vars.contains("safe"),
        "AST-only: int(raw) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn typescript_parseInt_sanitizer_truncates_flow_ast_only() {
    let src = "\
function handler(req, res) {
    const x = req.body;
    const safe = parseInt(x);
    eval(String(safe));
}
";
    let result = analyze_ast_only(src, Language::TypeScript, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "AST-only: parseInt(x) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn javascript_Number_sanitizer_truncates_flow_ast_only() {
    let src = "\
function handler(req, res) {
    var x = req.body;
    var safe = Number(x);
    eval(String(safe));
}
";
    let result = analyze_ast_only(src, Language::JavaScript, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "AST-only: Number(x) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn go_strconv_atoi_sanitizer_truncates_flow_ast_only() {
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
    let result = analyze_ast_only(src, Language::Go, "f");
    assert!(
        result.sanitized_vars.contains("safe"),
        "AST-only: strconv.Atoi(raw) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn java_integer_parseInt_sanitizer_truncates_flow_ast_only() {
    let src = "\
public class H {
    public void handle(HttpServletRequest request) throws Exception {
        String cmd = request.getParameter(\"c\");
        int safe = Integer.parseInt(cmd);
        Runtime.getRuntime().exec(Integer.toString(safe));
    }
}
";
    let result = analyze_ast_only(src, Language::Java, "handle");
    assert!(
        result.sanitized_vars.contains("safe"),
        "AST-only: Integer.parseInt(cmd) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn rust_parse_turbofish_sanitizer_truncates_flow_ast_only() {
    let src = "\
fn f() {
    let raw = std::env::var(\"X\").unwrap();
    let safe: i32 = raw.parse::<i32>().unwrap();
    std::process::Command::new(safe.to_string()).output().unwrap();
}
";
    let result = analyze_ast_only(src, Language::Rust, "f");
    assert!(
        result.sanitized_vars.contains("safe"),
        "AST-only: parse::<i32>() should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn c_atoi_sanitizer_truncates_flow_ast_only() {
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
    let result = analyze_ast_only(src, Language::C, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "AST-only: atoi(buf) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn cpp_std_stoi_sanitizer_truncates_flow_ast_only() {
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
    let result = analyze_ast_only(src, Language::Cpp, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "AST-only: std::stoi(s) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn ruby_to_i_sanitizer_truncates_flow_ast_only() {
    let src = "\
def handler
    cmd = gets
    safe = cmd.to_i
    system(safe.to_s)
end
";
    let result = analyze_ast_only(src, Language::Ruby, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "AST-only: cmd.to_i should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

/// Ruby `Rack::Utils.escape_html` — premortem A1 expects RED→GREEN at M2
/// (NOT M3) because the raw-fallback is already at taint.rs:2416.
#[test]
fn ruby_rack_utils_escape_html_sanitizer_truncates_flow_ast_only() {
    let src = "\
def handler
    cmd = gets
    safe = Rack::Utils.escape_html(cmd)
    system(safe)
end
";
    let result = analyze_ast_only(src, Language::Ruby, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "AST-only: Rack::Utils.escape_html(cmd) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn kotlin_toInt_sanitizer_truncates_flow_ast_only() {
    let src = "\
fun handler() {
    val line = readLine() ?: return
    val safe = line.toInt()
    Runtime.getRuntime().exec(safe.toString())
}
";
    let result = analyze_ast_only(src, Language::Kotlin, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "AST-only: line.toInt() should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn swift_Int_sanitizer_truncates_flow_ast_only() {
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
    let result = analyze_ast_only(src, Language::Swift, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "AST-only: Int(line) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn csharp_int_parse_sanitizer_truncates_flow_ast_only() {
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
    let result = analyze_ast_only(src, Language::CSharp, "Handle");
    assert!(
        result.sanitized_vars.contains("safe"),
        "AST-only: int.Parse(s) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn scala_toInt_sanitizer_truncates_flow_ast_only() {
    let src = "\
object H {
  def handler(): Unit = {
    val cmd = scala.io.StdIn.readLine()
    val safe = cmd.toInt
    Runtime.getRuntime.exec(safe.toString)
  }
}
";
    let result = analyze_ast_only(src, Language::Scala, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "AST-only: cmd.toInt should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

/// PHP `(int)` cast_expression — premortem A1 expects RED→GREEN at M2
/// (NOT M3) because the raw-fallback is already at taint.rs:2717.
#[test]
fn php_int_cast_sanitizer_truncates_flow_ast_only() {
    let src = "\
<?php
function handler() {
    $cmd = $_GET['c'];
    $safe = (int)$cmd;
    system(strval($safe));
}
";
    let result = analyze_ast_only(src, Language::Php, "handler");
    assert!(
        result.sanitized_vars.contains("safe") || result.sanitized_vars.contains("$safe"),
        "AST-only: (int)$cmd should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn lua_tonumber_sanitizer_truncates_flow_ast_only() {
    let src = "\
function handler()
    local cmd = io.read()
    local safe = tonumber(cmd)
    os.execute(tostring(safe))
end
";
    let result = analyze_ast_only(src, Language::Lua, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "AST-only: tonumber(cmd) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn elixir_String_to_integer_sanitizer_truncates_flow_ast_only() {
    let src = "\
defmodule F do
    def handler do
        cmd = IO.gets(\"> \")
        safe = String.to_integer(cmd)
        System.cmd(Integer.to_string(safe), [])
    end
end
";
    let result = analyze_ast_only(src, Language::Elixir, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "AST-only: String.to_integer(cmd) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn ocaml_int_of_string_sanitizer_truncates_flow_ast_only() {
    let src = "\
let handler () =
  let cmd = read_line () in
  let safe = int_of_string cmd in
  ignore (Sys.command (string_of_int safe))
";
    let result = analyze_ast_only(src, Language::Ocaml, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "AST-only: int_of_string cmd should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

// ---------------------------------------------------------------------------
// sanitizer-removal-v1 M3 (Gap 2): Zod-style validator AST-only mirrors.
//
// Mirror of `typescript_zod_parse_*` / `typescript_zod_safeParse_*` in
// sanitize_breaks_flow_per_language.rs, but exercised through the
// AST_ONLY_TEST_MODE path (regex bank disabled). These prove the new
// `("*", "parse")` / `("*", "safeParse")` AST sanitizer entries fire on the
// pure-AST dispatch that M4 will make canonical.
// ---------------------------------------------------------------------------

#[test]
fn typescript_zod_parse_sanitizer_truncates_flow_ast_only() {
    let src = "\
function handler(req, res) {
    const tainted = req.body;
    const safe = schema.parse(tainted);
    eval(String(safe));
}
";
    let result = analyze_ast_only(src, Language::TypeScript, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "AST-only: schema.parse(tainted) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

#[test]
fn typescript_zod_safeParse_sanitizer_truncates_flow_ast_only() {
    let src = "\
function handler(req, res) {
    const tainted = req.body;
    const safe = schema.safeParse(tainted);
    eval(String(safe));
}
";
    let result = analyze_ast_only(src, Language::TypeScript, "handler");
    assert!(
        result.sanitized_vars.contains("safe"),
        "AST-only: schema.safeParse(tainted) should mark safe as sanitized; sanitized_vars={:?}",
        result.sanitized_vars
    );
}

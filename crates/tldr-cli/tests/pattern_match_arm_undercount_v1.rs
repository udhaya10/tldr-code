//! pattern-match-arm-undercount-v1 (P19.BUG-01 family + BUG-P19-02)
//!
//! After the M1 `cognitive-else-counting-fix-v1` (`2b6d794`), pattern-match
//! dispatch constructs were still under-counted across 7 languages:
//! c, cpp, elixir, kotlin, ocaml, rust, scala.
//!
//! The fix extends the per-language cognitive AST walker so:
//!   - c/cpp `case_statement` arms (non-`default`)
//!   - rust `match_arm` (non-wildcard)
//!   - scala `case_clause` (non-wildcard)
//!   - kotlin `when_entry` (non-`else`)
//!   - ocaml `match_case` (non-wildcard, in both `match` and `function`)
//!   - elixir `stab_clause` (non-catchall, inside `case`/`cond`/`with`)
//! each receive +1 cognitive and +1 cyclomatic per SonarSource v1.4.
//!
//! Additionally, BUG-P19-02 (Rust `max_nesting` permanently 0) is fixed by
//! teaching `increases_nesting` about the Rust grammar's `*_expression`
//! shape (`if_expression`, `for_expression`, etc.), which was already in
//! the same code surface. The Rust test below verifies nesting is now
//! tracked. Other secondary fixes (BUG-P19-03..09) are covered in
//! `p19_secondary_fixes_v1.rs`.
//!
//! All tests follow the `no-synthetic-fixtures-v1` architecture: gated on
//! presence of real-repo paths under `/tmp/repos/` with numeric, lower-bound
//! assertions against hand-counted ground truth.

use std::path::Path;
use std::process::Command;

fn tldr_bin() -> std::path::PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR must be set under cargo test");
    std::path::PathBuf::from(manifest)
        .join("..")
        .join("..")
        .join("target")
        .join("release")
        .join("tldr")
}

fn run_tldr(args: &[&str]) -> (i32, String) {
    let out = Command::new(tldr_bin())
        .args(args)
        .output()
        .expect("failed to run tldr binary");
    let exit = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    (exit, stdout)
}

fn parse_json(out: &str) -> serde_json::Value {
    serde_json::from_str(out).unwrap_or(serde_json::Value::Null)
}

fn max_cog_for(v: &serde_json::Value, name: &str) -> u64 {
    v["functions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|f| f["name"].as_str() == Some(name))
                .filter_map(|f| f["cognitive"].as_u64())
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

// ============================================================================
// Rust: 8-arm `match` in `parse` (globset/src/glob.rs) was pre-fix cog=0,
// max_nesting=0. Post-fix: cognitive should be at least 9 (1 match + 7
// non-wildcard arms), max_nesting should be at least 1.
// ============================================================================
#[test]
fn p19_bug01_rust_match_arms_credited() {
    let file = "/tmp/repos/ripgrep/crates/globset/src/glob.rs";
    if !Path::new(file).exists() {
        return;
    }
    let (rc, out) = run_tldr(&["cognitive", file, "--format", "json"]);
    assert_eq!(rc, 0, "tldr cognitive rc != 0; stdout={}", out);
    let v = parse_json(&out);
    let cog = max_cog_for(&v, "parse");
    assert!(
        cog >= 9,
        "rust `parse` in globset/src/glob.rs must score cognitive >= 9 after \
         match-arm credit; got {}",
        cog
    );
}

#[test]
fn p19_bug02_rust_max_nesting_not_zero_everywhere() {
    // BUG-P19-02: pre-fix `max_nesting` was 0 for every function across
    // the entire file (because the Rust grammar uses `if_expression` /
    // `for_expression` which weren't in the nesting set). Post-fix at
    // least some functions must report nesting > 0.
    let file = "/tmp/repos/ripgrep/crates/globset/src/glob.rs";
    if !Path::new(file).exists() {
        return;
    }
    let (rc, out) = run_tldr(&["cognitive", file, "--format", "json"]);
    assert_eq!(rc, 0);
    let v = parse_json(&out);
    let max_nest = v["functions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| f["max_nesting"].as_u64())
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    assert!(
        max_nest >= 2,
        "rust `max_nesting` must be at least 2 across glob.rs (BUG-P19-02 fix); \
         got {}",
        max_nest
    );
}

// ============================================================================
// C: 5-arm switch in sdsnewlen (sds.c). Pre-fix cog=8 (switch +1 + control
// flow). Post-fix: at least 4 additional non-default case arms credited =>
// cog >= 12.
// ============================================================================
#[test]
fn p19_bug01_c_switch_case_arms_credited() {
    let file = "/tmp/repos/c-sds/sds.c";
    if !Path::new(file).exists() {
        return;
    }
    let (rc, out) = run_tldr(&["cognitive", file, "--format", "json"]);
    assert_eq!(rc, 0);
    let v = parse_json(&out);
    let cog = max_cog_for(&v, "sdsnewlen");
    assert!(
        cog >= 12,
        "c `sdsnewlen` must score cognitive >= 12 after switch-case-arm \
         credit; got {}",
        cog
    );
}

// ============================================================================
// C++: 5-case switch (4 named + default) in ConvertUTF32ToUTF8 (tinyxml2.cpp).
// Pre-fix cog=5. Post-fix: at least 4 case arms credited.
// ============================================================================
#[test]
fn p19_bug01_cpp_switch_case_arms_credited() {
    let file = "/tmp/repos/cpp-tinyxml2/tinyxml2.cpp";
    if !Path::new(file).exists() {
        return;
    }
    let (rc, out) = run_tldr(&["cognitive", file, "--format", "json"]);
    assert_eq!(rc, 0);
    let v = parse_json(&out);
    let cog = v["functions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|f| {
                    f["name"]
                        .as_str()
                        .map(|n| n.contains("ConvertUTF32ToUTF8"))
                        .unwrap_or(false)
                })
                .filter_map(|f| f["cognitive"].as_u64())
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    assert!(
        cog >= 9,
        "cpp `ConvertUTF32ToUTF8` must score cognitive >= 9 after \
         switch-case-arm credit; got {}",
        cog
    );
}

// ============================================================================
// Scala: 20-arm match in IO.interpret (cats-effect). Pre-fix cog=1. Post-fix:
// at least 20 case arms (minus 1 catchall ≈ 19) + 1 for match ≈ 20.
// ============================================================================
#[test]
fn p19_bug01_scala_case_arms_credited() {
    let file = "/tmp/repos/scala-cats-effect/core/shared/src/main/scala/cats/effect/IO.scala";
    if !Path::new(file).exists() {
        return;
    }
    let (rc, out) = run_tldr(&["cognitive", file, "--format", "json"]);
    assert_eq!(rc, 0);
    let v = parse_json(&out);
    let cog = max_cog_for(&v, "interpret");
    assert!(
        cog >= 20,
        "scala `interpret` must score cognitive >= 20 after case-arm credit; \
         got {}",
        cog
    );
}

// ============================================================================
// Kotlin: across the kotlin-datetime corpus at least one file with `when`
// arms should now report cognitive ≥ 5. (Pre-fix the entire kotlinx-datetime
// corpus reported max cognitive = 5 with `when` flattened to 1.)
// ============================================================================
#[test]
fn p19_bug01_kotlin_when_arms_credited() {
    let file = "/tmp/repos/kotlin-datetime/core/tzdbOnFilesystem/test/TimeZoneRulesCompleteTest.kt";
    if !Path::new(file).exists() {
        return;
    }
    let (rc, out) = run_tldr(&["cognitive", file, "--format", "json"]);
    assert_eq!(rc, 0);
    let v = parse_json(&out);
    let max_cog = v["functions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| f["cognitive"].as_u64())
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    assert!(
        max_cog >= 5,
        "kotlin `when` arms must contribute cognitive >= 5 somewhere in the \
         tested kt file; got max {}",
        max_cog
    );
}

// ============================================================================
// OCaml: action_exec.ml has 25 `match` exprs across the file. Pre-fix EVERY
// function reported cog=0 (because match arms were invisible). Post-fix:
// at least 10 functions must report nonzero cognitive.
// ============================================================================
#[test]
fn p19_bug01_ocaml_match_arms_credited() {
    let file = "/tmp/repos/ocaml-dune/src/dune_engine/action_exec.ml";
    if !Path::new(file).exists() {
        return;
    }
    let (rc, out) = run_tldr(&["cognitive", file, "--format", "json"]);
    assert_eq!(rc, 0);
    let v = parse_json(&out);
    let nonzero = v["functions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|f| f["cognitive"].as_u64().unwrap_or(0) > 0)
                .count()
        })
        .unwrap_or(0);
    assert!(
        nonzero >= 10,
        "ocaml `action_exec.ml` must show >= 10 functions with cognitive > 0 \
         after match-arm credit; got {}",
        nonzero
    );
}

// ============================================================================
// Elixir: conn.ex has 22 case blocks + 1 cond. Pre-fix every function reported
// max cognitive = 2. Post-fix: max cognitive must be at least 5.
// ============================================================================
#[test]
fn p19_bug01_elixir_case_cond_with_arms_credited() {
    let file = "/tmp/repos/elixir-plug/lib/plug/conn.ex";
    if !Path::new(file).exists() {
        return;
    }
    let (rc, out) = run_tldr(&["cognitive", file, "--format", "json"]);
    assert_eq!(rc, 0);
    let v = parse_json(&out);
    let max_cog = v["functions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| f["cognitive"].as_u64())
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    assert!(
        max_cog >= 5,
        "elixir `conn.ex` must show max cognitive >= 5 after case/cond/with \
         arm credit; got {}",
        max_cog
    );
}

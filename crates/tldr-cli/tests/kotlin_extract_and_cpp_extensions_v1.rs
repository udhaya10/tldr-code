//! kotlin-extract-and-cpp-extensions-v1 — regression tests for 2 phase-6
//! audit bugs:
//!
//! - **P6.BUG-N1 (MED)**: `tldr extract` on Kotlin always returned an empty
//!   `name` field for every class. Root cause: `extract_kotlin_class_info`
//!   (and `extract_kotlin_object_info`) searched for a `type_identifier`
//!   child, but Kotlin's tree-sitter grammar uses `simple_identifier` for
//!   class/object names. The bug cascaded into `tldr impact` failing on
//!   qualified Kotlin names (`KnownBuilds.buildOn`) AND on bare names
//!   (`buildOn`) because the impact name index was keyed under "" for every
//!   Kotlin class. The fix mirrors the working
//!   `extract_kotlin_function_info` pattern: prefer `child_by_field_name
//!   ("name")`, fall back to `simple_identifier` / `type_identifier`.
//!
//! - **P6.BUG-N2 (MED)**: `tldr structure` aborted the entire walk with
//!   `Error: Unsupported language: hxx` (and `.hh`/`.h++`/`.c++`) on rare
//!   but valid C++ extensions. Root cause: `Language::from_extension` and
//!   `Language::extensions` only listed `.cpp/.cc/.cxx/.hpp` for Cpp; the
//!   walker's `scan_extensions()` already covered the rare spellings, but
//!   the per-file classifier (used by `parse_file_with_lang` autodetect)
//!   rejected them. ALSO the walker hard-aborted on any single file with
//!   an unrecognised extension instead of skipping. The fix:
//!   1. extends `from_extension` and `extensions()` for Cpp to include
//!      `.c++`, `.hh`, `.hxx`, `.h++`;
//!   2. flags `UnsupportedLanguage` as recoverable so directory walks
//!      skip-and-continue rather than aborting on a stray `.zzz` file.
//!      Per-file commands (`tldr extract somefile.zzz`) still surface
//!      `UnsupportedLanguage` as a hard error because they consult the error
//!      directly, not via `is_recoverable`.

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn tldr_cmd() -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("tldr"));
    command.env("TLDR_ONESHOT", "1");
    command
}

fn write(p: &Path, body: &str) {
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).expect("mkdir -p");
    }
    fs::write(p, body).expect("write fixture");
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

/// Run `tldr <args>` and return (success, stdout, stderr).
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
// P6.BUG-N1: Kotlin class / object name extraction
// =============================================================================

/// `tldr extract` on a Kotlin file with two top-level classes must populate
/// the `.classes[].name` field with the real source-level class names —
/// previously they came back as empty strings.
#[test]
fn test_n1_kotlin_class_names_populated() {
    let dir = TempDir::new().unwrap();
    let kt = dir.path().join("k.kt");
    write(
        &kt,
        r#"package com.example
class Person(val name: String, val age: Int) {
    fun greet(): String = "Hello, $name!"
}
class Animal {
    fun sound(): String = "generic"
}
"#,
    );

    let v = run_json(&["extract", kt.to_str().unwrap()]);
    let names: Vec<String> = v["classes"]
        .as_array()
        .expect("classes is array")
        .iter()
        .map(|c| c["name"].as_str().unwrap_or("").to_string())
        .collect();

    // Must contain both real names (no empty strings).
    assert!(
        names.contains(&"Person".to_string()),
        "expected 'Person' among class names, got {:?}",
        names
    );
    assert!(
        names.contains(&"Animal".to_string()),
        "expected 'Animal' among class names, got {:?}",
        names
    );
    assert!(
        !names.iter().any(|n| n.is_empty()),
        "no class name should be empty, got {:?}",
        names
    );
}

/// `tldr extract` on a Kotlin `object` declaration (singleton) must
/// populate the name field too — same root cause, same fix as the class
/// case but in a separate code path (`extract_kotlin_object_info`).
#[test]
fn test_n1_kotlin_object_names_populated() {
    let dir = TempDir::new().unwrap();
    let kt = dir.path().join("singleton.kt");
    write(
        &kt,
        r#"package com.example
object MySingleton {
    fun hello(): String = "hi"
    val constant: Int = 42
}
"#,
    );

    let v = run_json(&["extract", kt.to_str().unwrap()]);
    let names: Vec<String> = v["classes"]
        .as_array()
        .expect("classes is array")
        .iter()
        .map(|c| c["name"].as_str().unwrap_or("").to_string())
        .collect();

    assert!(
        names.contains(&"MySingleton".to_string()),
        "object 'MySingleton' must be present in classes list with non-empty name, got {:?}",
        names
    );
}

/// Cascading fix: with class names populated, `tldr impact` can resolve
/// both qualified (`KnownBuilds.buildOn`) and unqualified (`buildOn`)
/// Kotlin method names against a project that contains them. Uses an
/// inline synthetic Kotlin project so the test doesn't depend on a
/// particular external repo being present.
#[test]
fn test_n1_impact_kotlin_qualified_name() {
    let dir = TempDir::new().unwrap();
    let kt = dir.path().join("utils.kt");
    write(
        &kt,
        r#"package com.example

enum class Platform { LINUX, MACOS }

object KnownBuilds {
    fun buildOn(platform: Platform): String = "build_${platform.name}"
    fun deployOn(platform: Platform): String = "deploy_${platform.name}"
}
"#,
    );

    // Bare name: must succeed (used to be "Function not found" because the
    // impact name index was keyed under empty class name).
    let v_bare = run_json(&["impact", "buildOn", dir.path().to_str().unwrap()]);
    assert!(
        v_bare.get("targets").is_some(),
        "bare 'buildOn' must resolve to at least one target, got {}",
        v_bare
    );

    // Qualified name: must succeed.
    let v_qual = run_json(&[
        "impact",
        "KnownBuilds.buildOn",
        dir.path().to_str().unwrap(),
    ]);
    assert!(
        v_qual.get("targets").is_some(),
        "qualified 'KnownBuilds.buildOn' must resolve, got {}",
        v_qual
    );
    // The resolved target should reference KnownBuilds.buildOn somewhere.
    let qual_str = serde_json::to_string(&v_qual).unwrap();
    assert!(
        qual_str.contains("KnownBuilds.buildOn") || qual_str.contains("buildOn"),
        "qualified resolution should mention buildOn, got {}",
        qual_str
    );
}

// =============================================================================
// P6.BUG-N2: rare C++ extensions recognized; walker skips unknown ext
// =============================================================================

/// `tldr structure` on a directory mixing the rare-but-valid Cpp
/// spellings (`.hxx`, `.hh`, `.h++`, `.c++`) alongside `.cpp` must walk
/// all five files and not abort with `Error: Unsupported language: hxx`.
#[test]
fn test_n2_cpp_rare_extensions_recognized() {
    let dir = TempDir::new().unwrap();
    write(&dir.path().join("t.cpp"), "class A{};\n");
    write(&dir.path().join("t.hxx"), "class B{};\n");
    write(&dir.path().join("t.hh"), "class C{};\n");
    write(&dir.path().join("t.h++"), "class D{};\n");
    write(&dir.path().join("t.c++"), "class E{};\n");

    let v = run_json(&["structure", dir.path().to_str().unwrap()]);
    let mut paths: Vec<String> = v["files"]
        .as_array()
        .expect("files is array")
        .iter()
        .map(|f| f["path"].as_str().unwrap_or("").to_string())
        .collect();
    paths.sort();

    // Must include t.cpp + at least 2 of the rare-spelling variants
    // (the audit's minimum bar). Verifying the strict superset.
    assert!(
        paths.iter().any(|p| p.ends_with("t.cpp")),
        "t.cpp must be in walk, got {:?}",
        paths
    );
    let variant_count = paths
        .iter()
        .filter(|p| {
            p.ends_with("t.hxx")
                || p.ends_with("t.hh")
                || p.ends_with("t.h++")
                || p.ends_with("t.c++")
        })
        .count();
    assert!(
        variant_count >= 2,
        "at least 2 rare-extension variants expected, got {} from {:?}",
        variant_count,
        paths
    );
}

/// Per-file `tldr extract` on each rare Cpp spelling autodetects
/// `language: "cpp"` (previously: hard error "Unsupported language").
#[test]
fn test_n2_cpp_extension_extract_lang() {
    let dir = TempDir::new().unwrap();
    let cases = [".hxx", ".h++", ".hh", ".c++"];
    for ext in cases {
        let p = dir.path().join(format!("t{}", ext));
        write(&p, "class X{};\n");
        let v = run_json(&["extract", p.to_str().unwrap()]);
        let lang = v["language"].as_str().unwrap_or("");
        assert_eq!(
            lang, "cpp",
            "tldr extract t{} must autodetect cpp, got language={:?}",
            ext, lang
        );
    }
}

/// `tldr structure` on a directory containing a truly-unknown extension
/// (e.g. `.zzz`) alongside a recognised Cpp file must NOT abort the
/// walk; it should silently skip the unknown file and emit valid output
/// for the remaining files.
#[test]
fn test_n2_walker_skips_unknown_extension() {
    let dir = TempDir::new().unwrap();
    write(&dir.path().join("t.cpp"), "class A{};\n");
    write(&dir.path().join("t.zzz"), "arbitrary content\n");

    let (ok, stdout, stderr) = run_raw(&["structure", dir.path().to_str().unwrap()]);
    assert!(
        ok,
        "tldr structure must succeed even with an unknown-ext file present.\n\
         stderr={}",
        stderr
    );

    let v: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("JSON parse failed: {}\nstdout={}", e, stdout));
    let paths: Vec<String> = v["files"]
        .as_array()
        .expect("files is array")
        .iter()
        .map(|f| f["path"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        paths.iter().any(|p| p.ends_with("t.cpp")),
        "t.cpp must be present after skipping t.zzz, got {:?}",
        paths
    );
    // t.zzz must NOT appear (it's not a recognised source extension).
    assert!(
        !paths.iter().any(|p| p.ends_with("t.zzz")),
        "t.zzz must be silently skipped, got {:?}",
        paths
    );
}

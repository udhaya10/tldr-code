//! Canonical traversal-policy regression fixture (TLDR-boa.5).
//!
//! One fixture exercises every category the consolidated `ProjectWalker`
//! policy must handle, then asserts the walk surfaces AGREE on membership:
//!
//! - the canonical walk (`ProjectWalker`),
//! - the tree adapter (`get_file_tree`), and
//! - the single-file corpus gate (`is_corpus_file` / `CorpusPolicy::accepts_path`).
//!
//! `semantic` indexing, freshness, the watcher delta path, enriched search and
//! the build counters all walk via the same `ProjectWalker` (Phase 3 unified
//! their `WalkBuilder` config through `apply_canonical_walk_config`); their
//! agreement with `enumerate_corpus_files` is already pinned by the
//! corpus-contract unit tests in `semantic/chunker.rs`. This fixture extends
//! coverage to the full category matrix + the `get_file_tree` layer + the
//! `.tldrignore` negation invariant (a negation cannot re-include a path the
//! default policy denies).
//!
//! Categories: valid C++/C source; CSV + log (unsupported ext); `.gitignore`
//! exclusions; `.tldrignore` exclusions; nested ignore; generated directory
//! (sentinel); oversized source; negation (positive + invariant).

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use tldr_core::fs::tree::get_file_tree;
use tldr_core::semantic::chunker::{is_corpus_file, CorpusPolicy};
use tldr_core::types::{FileTree, NodeType};
use tldr_core::walker::ProjectWalker;

/// Build the regression fixture under a fresh tempdir and return its path.
///
/// `git init` makes the dir a repository root so the `ignore` crate's
/// `.gitignore` handling is deterministic (it bounds `parents()` traversal).
fn build_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    // Root ignore files.
    fs::write(
        root.join(".gitignore"),
        // `git_ignored.cpp` + the whole `build_output/` dir are git-ignored.
        "git_ignored.cpp\nbuild_output/\n",
    )
    .unwrap();
    fs::write(
        root.join(".tldrignore"),
        // `tldr_ignored.py` (exact) and `tmp_*.py` (glob) are ignored. `vendor/`
        // is a DEFAULT-excluded dir; the `!vendor/keep.cpp` negation must NOT
        // re-include it — the walker.rs:623 invariant (asserted in a separate
        // test below). Positive negation re-include is intentionally NOT
        // exercised here: the depth-limited single-file gate and the bulk walk
        // disagree on it (a pre-existing quirk, filed as a follow-up bead).
        "tldr_ignored.py\ntmp_*.py\n!vendor/keep.cpp\n",
    )
    .unwrap();

    // Eligible C++ / C source.
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.cpp"), "int main() { return 0; }\n").unwrap();
    fs::write(root.join("src/util.h"), "#pragma once\nint util();\n").unwrap();

    // Unsupported extensions — present in the tree, NOT in the corpus.
    fs::write(root.join("data.csv"), "a,b,c\n1,2,3\n").unwrap();
    fs::write(root.join("app.log"), "2026-07-25 started\n").unwrap();

    // `.gitignore`-denied.
    fs::write(root.join("git_ignored.cpp"), "int ghost() { return 1; }\n").unwrap();
    fs::create_dir_all(root.join("build_output")).unwrap();
    fs::write(
        root.join("build_output/x.cpp"),
        "int built() { return 2; }\n",
    )
    .unwrap();

    // `.tldrignore`-denied (exact name + glob).
    fs::write(root.join("tldr_ignored.py"), "def denied():\n    pass\n").unwrap();
    fs::write(root.join("tmp_skip.py"), "def skip():\n    pass\n").unwrap();

    // Nested `.tldrignore` (honored at any level, like `.gitignore`).
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("sub/.tldrignore"), "nested_skip.py\n").unwrap();
    fs::write(root.join("sub/keep.py"), "def nested_keep():\n    pass\n").unwrap();
    fs::write(
        root.join("sub/nested_skip.py"),
        "def nested_skip():\n    pass\n",
    )
    .unwrap();

    // Generated directory: `doxygen.css` sentinel marks `gen/` as generated.
    fs::create_dir_all(root.join("gen")).unwrap();
    fs::write(root.join("gen/doxygen.css"), "/* generated doxygen */\n").unwrap();
    fs::write(root.join("gen/out.cpp"), "int generated() { return 3; }\n").unwrap();

    // Oversized valid source: 10 MiB + 1 (one byte over `MAX_FILE_SIZE_BYTES`).
    // Oversize is a DOWNSTREAM concern (chunking/embedding), not a membership
    // one — so the file IS a corpus member and the walkers must AGREE on that.
    let big = format!("# {}", "x".repeat(10 * 1024 * 1024));
    fs::write(root.join("big.py"), big).unwrap();

    // DEFAULT-excluded dir (`vendor`) with a negation attempt that must fail.
    fs::create_dir_all(root.join("vendor")).unwrap();
    fs::write(
        root.join("vendor/keep.cpp"),
        "int vendored() { return 4; }\n",
    )
    .unwrap();

    // Make it a git repo root so `.gitignore` is honored deterministically.
    std::process::Command::new("git")
        .arg("init")
        .arg("-q")
        .current_dir(root)
        .status()
        .expect("git init");

    dir
}

/// Collect every file's project-relative path from a `FileTree`.
fn tree_rel_files(tree: &FileTree, out: &mut HashSet<String>) {
    match tree.node_type {
        NodeType::File => {
            if let Some(p) = &tree.path {
                out.insert(p.to_string_lossy().replace('\\', "/"));
            }
        }
        NodeType::Dir => {
            for child in &tree.children {
                tree_rel_files(child, out);
            }
        }
    }
}

/// Collect every file's project-relative path from a `ProjectWalker` walk.
fn walk_rel_files(root: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    for entry in ProjectWalker::new(root).iter() {
        if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            if let Ok(rel) = entry.path().strip_prefix(root) {
                out.insert(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out
}

#[test]
fn fixture_categories_agree_across_walk_tree_and_gate() {
    let dir = build_fixture();
    let root = dir.path();

    let tree = get_file_tree(root, None, true).expect("tree builds");
    let mut tree_files = HashSet::new();
    tree_rel_files(&tree, &mut tree_files);
    let walk_files = walk_rel_files(root);

    // Helper oracles against the three surfaces.
    let rel = |p: &str| root.join(p);
    let gate = |p: &str| is_corpus_file(root, &rel(p));
    let in_tree = |p: &str| tree_files.contains(p);
    let in_walk = |p: &str| walk_files.contains(p);

    // ---- Eligible source: IN everywhere. ----
    for eligible in ["src/main.cpp", "src/util.h", "sub/keep.py"] {
        assert!(gate(eligible), "{eligible} should be a corpus member");
        assert!(in_tree(eligible), "{eligible} should be in the tree");
        assert!(in_walk(eligible), "{eligible} should be in the walk");
    }

    // ---- Unsupported extensions: corpus NO (language filter), tree/walk YES. ----
    for unsupported in ["data.csv", "app.log"] {
        assert!(
            !gate(unsupported),
            "{unsupported} must fail the corpus gate"
        );
        assert!(in_tree(unsupported), "{unsupported} stays in the tree");
        assert!(in_walk(unsupported), "{unsupported} stays in the walk");
    }

    // ---- `.gitignore`-denied: OUT everywhere. ----
    assert!(!gate("git_ignored.cpp"), "gitignored file must be excluded");
    assert!(!in_tree("git_ignored.cpp"));
    assert!(!in_walk("git_ignored.cpp"));
    assert!(
        !gate("build_output/x.cpp"),
        "gitignored dir contents excluded"
    );
    assert!(!in_tree("build_output/x.cpp"));
    assert!(!in_walk("build_output/x.cpp"));

    // ---- `.tldrignore`-denied: OUT everywhere. ----
    assert!(!gate("tldr_ignored.py"));
    assert!(!in_tree("tldr_ignored.py"));
    assert!(!in_walk("tldr_ignored.py"));

    // ---- Nested `.tldrignore`: honored at depth. ----
    assert!(
        !gate("sub/nested_skip.py"),
        "nested .tldrignore must exclude"
    );
    assert!(!in_tree("sub/nested_skip.py"));
    assert!(!in_walk("sub/nested_skip.py"));

    // ---- Generated directory (sentinel): OUT everywhere. ----
    assert!(!gate("gen/out.cpp"), "generated-dir contents excluded");
    assert!(!in_tree("gen/out.cpp"));
    assert!(!in_walk("gen/out.cpp"));

    // ---- Oversized source: membership YES (downstream concern) — AGREEMENT. ----
    assert!(gate("big.py"), "oversized source is still a corpus member");
    assert!(in_tree("big.py"));
    assert!(in_walk("big.py"));

    // ---- `.tldrignore` glob: excluded everywhere. ----
    assert!(!gate("tmp_skip.py"), "glob-denied file stays excluded");
    assert!(!in_tree("tmp_skip.py"));
    assert!(!in_walk("tmp_skip.py"));
}

#[test]
fn tldrignore_negation_cannot_reinclude_default_denied_path() {
    // Invariant (walker.rs:623): a `.tldrignore` negation cannot re-include a
    // path the DEFAULT policy already denies. `vendor/` is in
    // `DEFAULT_EXCLUDE_DIRS`; the fixture's `!vendor/keep.cpp` must NOT rescue it.
    let dir = build_fixture();
    let root = dir.path();
    let target = root.join("vendor/keep.cpp");

    assert!(
        !is_corpus_file(root, &target),
        "negation must not re-include a default-excluded dir's file"
    );
    assert!(
        !CorpusPolicy::accepts_path(root, &target),
        "accepts_path must agree with is_corpus_file"
    );

    let tree = get_file_tree(root, None, true).unwrap();
    let mut tree_files = HashSet::new();
    tree_rel_files(&tree, &mut tree_files);
    assert!(
        !tree_files.contains("vendor/keep.cpp"),
        "default-excluded dir must be absent from the tree despite the negation"
    );
}

#[test]
fn single_file_gate_agrees_with_bulk_walk() {
    // The keystone invariant the refactor preserves: the single-file gate
    // (`is_corpus_file`, used by the watcher delta path + freshness) makes the
    // SAME membership decision as the bulk `ProjectWalker` walk, for every
    // supported source file.
    let dir = build_fixture();
    let root = dir.path();
    let walk_files = walk_rel_files(root);

    for rel_str in &walk_files {
        let abs = root.join(rel_str);
        // Only supported source files are gated `true`; the gate adds the
        // language/binary dimension on top of the walk. Skip unsupported exts
        // (csv/log) where the walk includes them but the gate rejects them by
        // language — that divergence is by design, not a drift.
        if tldr_core::Language::from_path(&abs).is_none() {
            continue;
        }
        let gate = is_corpus_file(root, &abs);
        assert!(
            gate,
            "supported source {rel_str} is in the walk but the gate rejected it — drift"
        );
    }

    // And the converse: every supported file that the gate REJECTS for
    // ignore/generated reasons is absent from the bulk walk.
    for denied in [
        "git_ignored.cpp",
        "build_output/x.cpp",
        "tldr_ignored.py",
        "sub/nested_skip.py",
        "gen/out.cpp",
        "tmp_skip.py",
    ] {
        assert!(
            !walk_files.contains(denied),
            "{denied} should not be walked"
        );
    }
}

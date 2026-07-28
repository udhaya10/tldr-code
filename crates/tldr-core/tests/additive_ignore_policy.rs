use std::fs;
use std::path::{Path, PathBuf};

use tldr_core::semantic::chunker::is_corpus_file;
use tldr_core::walker::{build_path_ignore_matcher, PathClass, ProjectWalker};

fn write(root: &Path, relative: &str, contents: &str) -> PathBuf {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create test directory");
    }
    fs::write(&path, contents).expect("write test file");
    path
}

fn walked_files(root: &Path) -> Vec<PathBuf> {
    ProjectWalker::new(root)
        .extensions(&["py"])
        .iter()
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(|entry| {
            entry
                .path()
                .strip_prefix(root)
                .expect("walked path is below root")
                .to_path_buf()
        })
        .collect()
}

#[test]
fn tldrignore_cannot_reinclude_a_gitignored_file() {
    let temp = tempfile::tempdir().expect("create temp project");
    let root = temp.path();
    fs::create_dir(root.join(".git")).expect("create git metadata directory");
    write(root, "nested/.gitignore", "secret.py\n");
    write(root, "nested/.tldrignore", "!secret.py\n");
    let secret = write(root, "nested/secret.py", "SECRET = True\n");
    let visible = write(root, "nested/visible.py", "VISIBLE = True\n");

    let files = walked_files(root);
    assert!(!files.contains(&PathBuf::from("nested/secret.py")));
    assert!(files.contains(&PathBuf::from("nested/visible.py")));
    assert_eq!(
        ProjectWalker::new(root).classify_path(&secret, false),
        PathClass::Ignored
    );
    assert!(!is_corpus_file(root, &secret));
    assert!(is_corpus_file(root, &visible));
}

#[test]
fn tldrignore_may_reopen_only_its_own_prior_exclusion() {
    let temp = tempfile::tempdir().expect("create temp project");
    let root = temp.path();
    write(root, "nested/.tldrignore", "*.py\n!keep.py\n");
    let dropped = write(root, "nested/drop.py", "DROP = True\n");
    let kept = write(root, "nested/keep.py", "KEEP = True\n");

    let files = walked_files(root);
    assert!(!files.contains(&PathBuf::from("nested/drop.py")));
    assert!(files.contains(&PathBuf::from("nested/keep.py")));
    assert_eq!(
        ProjectWalker::new(root).classify_path(&dropped, false),
        PathClass::Ignored
    );
    assert_eq!(
        ProjectWalker::new(root).classify_path(&kept, false),
        PathClass::Eligible
    );
    assert!(!is_corpus_file(root, &dropped));
    assert!(is_corpus_file(root, &kept));
}

#[test]
fn matcher_classifies_deleted_paths_using_nested_ignore_files() {
    let temp = tempfile::tempdir().expect("create temp project");
    let root = temp.path();
    fs::create_dir(root.join(".git")).expect("create git metadata directory");
    write(root, "nested/.gitignore", "git-only.py\n");
    write(root, "nested/.tldrignore", "tldr-only.py\n");

    let matcher = build_path_ignore_matcher(root, true).expect("build ignore matcher");
    let git_only = root.join("nested/git-only.py");
    let tldr_only = root.join("nested/tldr-only.py");

    assert!(matcher.is_ignored(&git_only, false));
    assert!(matcher.is_ignored(&tldr_only, false));
    assert!(!matcher.is_ignored(&root.join("nested/visible.py"), false));
}

#[test]
fn git_info_exclude_is_part_of_the_immutable_deny_floor() {
    let temp = tempfile::tempdir().expect("create temp project");
    let root = temp.path();
    fs::create_dir(root.join(".git")).expect("create git metadata directory");
    write(root, ".git/info/exclude", "private.py\n");
    write(root, ".tldrignore", "!private.py\n");
    let private = write(root, "private.py", "PRIVATE = True\n");

    assert!(!walked_files(root).contains(&PathBuf::from("private.py")));
    assert!(!is_corpus_file(root, &private));

    let matcher = build_path_ignore_matcher(root, true).expect("build ignore matcher");
    assert!(matcher.is_ignored(&private, false));
}

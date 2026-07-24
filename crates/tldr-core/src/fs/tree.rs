//! File tree traversal with ignore support.
//!
//! Implements the `tree` command functionality (spec Section 2.1.1).
//!
//! As of TLDR-boa.2 the bespoke `walkdir` traversal has been replaced by the
//! canonical [`crate::walker::ProjectWalker`]. `get_file_tree` is now a thin
//! adapter: it walks once via `ProjectWalker` — which honors `.gitignore`,
//! `.tldrignore`, hidden files, vendor/build dirs, generated-dir sentinels and
//! `follow_links(false)` — and reconstructs the nested [`FileTree`] from the
//! flat walk. This closes the gap where the tree walker honored only
//! `.tldrignore` while every other walk honored `.gitignore` too.
//!
//! # Mitigations Addressed
//! - M6: Large file memory (oversize is enforced centrally via
//!   [`crate::fs::check_size`], not by the tree walker).
//! - M9: Path handling platform (use PathBuf, dunce for normalization).
//! - M12: Gitignore pattern edge cases (use the `ignore` crate via
//!   [`crate::walker::ProjectWalker`]).
//! - M13: Symlink cycle detection ([`crate::walker::ProjectWalker`] sets
//!   `follow_links(false)`).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use ignore::gitignore::GitignoreBuilder;

use crate::error::TldrError;
use crate::types::{FileTree, IgnoreSpec, NodeType};
use crate::walker::ProjectWalker;
use crate::TldrResult;

/// Maximum file size to process (5MB) - M6 mitigation
pub const MAX_FILE_SIZE: u64 = 5 * 1024 * 1024;

/// Default directories to skip during traversal.
///
/// **api-check-and-patterns-accuracy-v1 (P11.BUG-AGG-7)**: extended this
/// list to include common generated artifact dirs (e.g. `out`, `bin`,
/// `obj`, `.gradle`, `dox` for doxygen, `.pytest_cache`, `.mypy_cache`,
/// `.ruff_cache`) so `tldr patterns` and other tree-driven commands
/// don't mis-classify projects whose generated docs/build output happens
/// to outnumber authored sources.
pub const DEFAULT_SKIP_DIRS: &[&str] = &[
    // Vendored / package-manager output
    "node_modules",
    "vendor",
    // Build sinks (general)
    "target",
    "dist",
    "build",
    "out",
    "bin",
    "obj",
    // JavaScript framework caches
    ".next",
    ".nuxt",
    // Doxygen output (typical custom-config dir; see GENERATED_DIR_SENTINELS
    // below for the `docs/` doxygen-output detection).
    "dox",
    // Python tooling
    "__pycache__",
    "venv",
    ".venv",
    "env",
    ".env",
    ".tox",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    // Coverage artefacts
    "coverage",
    ".coverage",
    // JVM tooling
    ".gradle",
    // Version control
    ".git",
    ".svn",
    ".hg",
    // Editor caches
    ".idea",
    ".vscode",
    ".cache",
];

/// Get file tree structure with optional extension filtering.
///
/// # Arguments
/// * `root` - Root directory to scan
/// * `extensions` - Optional set of extensions to include (e.g., `{".py", ".ts"}`)
/// * `exclude_hidden` - Skip hidden files/directories (default: true)
/// * `ignore_spec` - Optional gitignore-style patterns
///
/// # Returns
/// * `Ok(FileTree)` - Tree structure with files and directories
/// * `Err(TldrError::PathNotFound)` - Root directory doesn't exist
/// * `Err(TldrError::PathTraversal)` - Path contains directory traversal
///
/// # Example
/// ```ignore
/// use std::collections::HashSet;
/// use tldr_core::fs::tree::get_file_tree;
///
/// let extensions: HashSet<String> = [".py".to_string()].into_iter().collect();
/// let tree = get_file_tree(Path::new("src"), Some(&extensions), true, None)?;
/// ```
pub fn get_file_tree(
    root: &Path,
    extensions: Option<&HashSet<String>>,
    exclude_hidden: bool,
    ignore_spec: Option<&IgnoreSpec>,
) -> TldrResult<FileTree> {
    // Validate root path exists
    if !root.exists() {
        return Err(TldrError::PathNotFound(root.to_path_buf()));
    }

    // Check for path traversal attempts - M9 mitigation
    let canonical =
        dunce::canonicalize(root).map_err(|_| TldrError::PathNotFound(root.to_path_buf()))?;

    // Detect path traversal by checking if the path contains ".."
    let path_str = root.to_string_lossy();
    if path_str.contains("..") {
        // Verify it actually escapes by comparing canonical with expected
        if let Ok(parent) = std::env::current_dir() {
            let joined = parent.join(root);
            if let Ok(joined_canonical) = dunce::canonicalize(&joined) {
                // If the canonical path doesn't start with parent, it's traversal
                if !joined_canonical.starts_with(&parent)
                    && !joined_canonical.starts_with(&canonical)
                {
                    return Err(TldrError::PathTraversal(root.to_path_buf()));
                }
            }
        }
    }

    // Caller-supplied ignore patterns (compat shim). `.gitignore` and
    // `.tldrignore` are honored by ProjectWalker; only the extra patterns
    // carried by `IgnoreSpec` are applied here. Retired in TLDR-boa.4.
    let caller_ignore = build_caller_ignore_matcher(&canonical, ignore_spec);

    // Get root directory name
    let root_name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());

    // Single canonical walk. ProjectWalker honors `.gitignore` + `.tldrignore`,
    // skips hidden (unless `exclude_hidden` is false), skips vendor/build and
    // generated dirs, and does not follow symlinks.
    let mut walker = ProjectWalker::new(canonical.as_path());
    if !exclude_hidden {
        walker = walker.include_hidden();
    }

    // Collect (relative path, is_dir) for every surviving entry.
    let mut entries: Vec<(PathBuf, bool)> = Vec::new();
    for entry in walker.iter() {
        // Skip the root entry itself (rel == "").
        let rel = match entry.path().strip_prefix(&canonical) {
            Ok(r) if !r.as_os_str().is_empty() => r.to_path_buf(),
            _ => continue,
        };
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);

        // Caller-supplied ignore patterns (gitignore semantics; directory
        // patterns exclude contents via matched_path_or_any_parents).
        if let Some(gi) = &caller_ignore {
            if gi.matched_path_or_any_parents(&rel, is_dir).is_ignore() {
                continue;
            }
        }

        // Extension allow-list applies to files only; directories always pass
        // (a directory is kept iff it has surviving children — see below).
        if let Some(exts) = extensions {
            if !is_dir {
                let ext = rel
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default();
                if !exts.contains(&ext) {
                    continue;
                }
            }
        }

        entries.push((rel, is_dir));
    }

    // Reconstruct the nested FileTree. When an extension filter is active,
    // directories with no surviving files are pruned; otherwise every walked
    // directory is kept (matching the prior walkdir behavior).
    let children = build_tree_from_entries(&entries, extensions.is_some());

    Ok(FileTree::dir(root_name, children))
}

/// Build a gitignore matcher from the caller-supplied [`IgnoreSpec`] patterns
/// only, rooted at `root`.
///
/// Unlike the retired `build_gitignore`, this does NOT load the project's
/// `.tldrignore` or `.gitignore` — those are honored by [`ProjectWalker`].
/// Only the extra patterns a caller passes via `IgnoreSpec` reach this matcher,
/// which keeps the `Option<&IgnoreSpec>` parameter of [`get_file_tree`]
/// behavior-compatible while the canonical walker owns the on-disk ignore
/// files. The whole shim is removed when `IgnoreSpec` is retired (TLDR-boa.4).
fn build_caller_ignore_matcher(
    root: &Path,
    ignore_spec: Option<&IgnoreSpec>,
) -> Option<ignore::gitignore::Gitignore> {
    let spec = ignore_spec?;
    if spec.patterns.is_empty() {
        return None;
    }
    let mut builder = GitignoreBuilder::new(root);
    let mut added = false;
    for pattern in &spec.patterns {
        if builder.add_line(None, pattern).is_ok() {
            added = true;
        }
    }
    added.then(|| builder.build().ok()).flatten()
}

/// Reconstruct a nested [`FileTree`] from a flat list of `(relative_path,
/// is_dir)` entries.
///
/// `prune_empty_dirs` controls the directory-pruning rule the prior recursive
/// walker applied: when an extension filter is active, directories with no
/// surviving files are dropped; when there is no filter, every walked directory
/// is kept (including empty ones).
fn build_tree_from_entries(entries: &[(PathBuf, bool)], prune_empty_dirs: bool) -> Vec<FileTree> {
    let mut root = TreeNode::default();
    for (rel, is_dir) in entries {
        insert_entry(&mut root, rel, *is_dir);
    }
    convert_children(&root.children, prune_empty_dirs)
}

#[derive(Default)]
struct TreeNode {
    children: BTreeMap<String, TreeNode>,
    is_file: bool,
    file_rel: Option<PathBuf>,
}

/// Insert one walked entry into the tree by splitting its relative path into
/// components. Intermediate components become directory nodes; the final
/// component is marked as a file (with its relative path) or a directory.
fn insert_entry(root: &mut TreeNode, rel: &Path, is_dir: bool) {
    let comps: Vec<&str> = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    let mut node = root;
    for (i, comp) in comps.iter().enumerate() {
        let is_last = i + 1 == comps.len();
        let child = node.children.entry((*comp).to_string()).or_default();
        if is_last {
            child.is_file = !is_dir;
            if !is_dir {
                child.file_rel = Some(rel.to_path_buf());
            }
        }
        node = child;
    }
}

/// Convert a tree node's children into sorted [`FileTree`] nodes.
fn convert_children(
    children: &BTreeMap<String, TreeNode>,
    prune_empty_dirs: bool,
) -> Vec<FileTree> {
    let mut out: Vec<FileTree> = children
        .iter()
        .filter_map(|(name, node)| convert_node(name, node, prune_empty_dirs))
        .collect();
    sort_children(&mut out);
    out
}

fn convert_node(name: &str, node: &TreeNode, prune_empty_dirs: bool) -> Option<FileTree> {
    if node.is_file {
        Some(FileTree::file(
            name.to_string(),
            node.file_rel.clone().unwrap_or_default(),
        ))
    } else {
        let children = convert_children(&node.children, prune_empty_dirs);
        if prune_empty_dirs && children.is_empty() {
            None
        } else {
            Some(FileTree::dir(name.to_string(), children))
        }
    }
}

/// Sort children: directories first, then files, alphabetically within each
/// group — matching the prior recursive walker's ordering.
fn sort_children(children: &mut [FileTree]) {
    children.sort_by(|a, b| match (&a.node_type, &b.node_type) {
        (NodeType::Dir, NodeType::File) => std::cmp::Ordering::Less,
        (NodeType::File, NodeType::Dir) => std::cmp::Ordering::Greater,
        _ => a.name.cmp(&b.name),
    });
}

/// Collect all files from tree as flat list
pub fn collect_files(tree: &FileTree, root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_files_recursive(tree, root, &mut files);
    files
}

fn collect_files_recursive(tree: &FileTree, root: &Path, files: &mut Vec<PathBuf>) {
    match tree.node_type {
        NodeType::File => {
            if let Some(ref path) = tree.path {
                files.push(root.join(path));
            }
        }
        NodeType::Dir => {
            for child in &tree.children {
                collect_files_recursive(child, root, files);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_dir() -> TempDir {
        let dir = TempDir::new().unwrap();

        // Create some test files
        fs::write(dir.path().join("main.py"), "# Python file").unwrap();
        fs::write(dir.path().join("utils.py"), "# Utils").unwrap();
        fs::write(dir.path().join("config.json"), "{}").unwrap();

        // Create subdirectory
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/module.py"), "# Module").unwrap();

        // Create hidden file
        fs::write(dir.path().join(".hidden"), "hidden").unwrap();

        dir
    }

    #[test]
    fn test_get_file_tree_basic() {
        let dir = create_test_dir();
        let tree = get_file_tree(dir.path(), None, true, None).unwrap();

        assert_eq!(tree.node_type, NodeType::Dir);
        assert!(!tree.children.is_empty());
    }

    #[test]
    fn test_get_file_tree_extension_filter() {
        let dir = create_test_dir();
        let extensions: HashSet<String> = [".py".to_string()].into_iter().collect();
        let tree = get_file_tree(dir.path(), Some(&extensions), true, None).unwrap();

        // All files should be .py
        fn check_extensions(node: &FileTree) {
            if node.node_type == NodeType::File {
                assert!(
                    node.name.ends_with(".py"),
                    "Found non-py file: {}",
                    node.name
                );
            }
            for child in &node.children {
                check_extensions(child);
            }
        }
        check_extensions(&tree);
    }

    #[test]
    fn test_get_file_tree_excludes_hidden() {
        let dir = create_test_dir();
        let tree = get_file_tree(dir.path(), None, true, None).unwrap();

        // No hidden files in children (root can be hidden like .tmp...)
        fn check_no_hidden(node: &FileTree) {
            assert!(
                !node.name.starts_with('.') || node.name == ".",
                "Hidden file found: {}",
                node.name
            );
            for child in &node.children {
                check_no_hidden(child);
            }
        }
        // Check only children, not the root (which can have .tmp prefix from tempfile)
        for child in &tree.children {
            check_no_hidden(child);
        }
    }

    #[test]
    fn test_get_file_tree_includes_hidden() {
        let dir = create_test_dir();
        let tree = get_file_tree(dir.path(), None, false, None).unwrap();

        // Should have hidden file
        fn has_hidden(node: &FileTree) -> bool {
            if node.name.starts_with('.') && node.name != "." {
                return true;
            }
            node.children.iter().any(has_hidden)
        }
        assert!(has_hidden(&tree), "No hidden files found");
    }

    #[test]
    fn test_get_file_tree_nonexistent() {
        let result = get_file_tree(Path::new("/nonexistent/path"), None, true, None);
        assert!(matches!(result, Err(TldrError::PathNotFound(_))));
    }

    #[test]
    fn test_get_file_tree_ignore_patterns() {
        let dir = create_test_dir();
        let ignore = IgnoreSpec::new(vec!["*.json".to_string()]);
        let tree = get_file_tree(dir.path(), None, true, Some(&ignore)).unwrap();

        // No .json files
        fn check_no_json(node: &FileTree) {
            assert!(
                !node.name.ends_with(".json"),
                "JSON file found: {}",
                node.name
            );
            for child in &node.children {
                check_no_json(child);
            }
        }
        check_no_json(&tree);
    }

    #[test]
    fn test_collect_files() {
        let dir = create_test_dir();
        let tree = get_file_tree(dir.path(), None, true, None).unwrap();
        let files = collect_files(&tree, dir.path());

        assert!(!files.is_empty());
        assert!(files.iter().any(|f| f.ends_with("main.py")));
    }

    /// Regression test for issue #15 — tree builder must NOT report
    /// SymlinkCycle when traversing a directory containing hardlinked files.
    /// WalkDir is configured with follow_links(false), so no real symlink
    /// cycle can occur; the previous inode-tracking heuristic incorrectly
    /// flagged hardlinks as cycles.
    #[test]
    fn test_get_file_tree_hardlinks_no_symlink_cycle() {
        let dir = TempDir::new().unwrap();
        let original = dir.path().join("original.txt");
        let hard = dir.path().join("hardlink.txt");

        fs::write(&original, "shared content").unwrap();
        fs::hard_link(&original, &hard).expect("hardlink creation failed");

        // Sanity: both paths exist and reference the same inode on Unix.
        assert!(original.exists());
        assert!(hard.exists());

        let result = get_file_tree(dir.path(), None, true, None);

        // Pre-fix: returns Err(TldrError::SymlinkCycle(...)).
        // Post-fix: returns Ok with both files listed.
        assert!(
            result.is_ok(),
            "tree builder must not report SymlinkCycle on hardlinks; got: {:?}",
            result.err()
        );
        let tree = result.unwrap();
        let names: Vec<&str> = tree.children.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"original.txt") && names.contains(&"hardlink.txt"),
            "expected both hardlinked files in tree; got: {:?}",
            names
        );
    }
}

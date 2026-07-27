# Artifact Containment and Deletion Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close TLDR-wa2g by enforcing canonical source containment, preserving safe deleted-file deltas, removing stale deleted-file artifacts from full generations, and restoring the watcher max-wait acceptance proof.

**Architecture:** Source reads cross one canonical containment boundary in `FileFactsParser`; daemon deltas use a deletion-safe resolver that canonicalizes the nearest existing ancestor; full ingestion computes removed subjects from the previous manifest and excludes them from the next generation. The existing shared contract harness receives focused lifecycle/security scenarios, while the watcher correction remains a narrow unit-test change.

**Tech Stack:** Rust 2021, dunce canonicalization, redb artifact generations, tempfile contract fixtures, Cargo workspace gates.

---

## File Structure

- Modify `crates/tldr-core/src/artifact_store/file_facts.rs`: validate and normalize source paths before any read or parse.
- Modify `crates/tldr-cli/src/commands/daemon/artifact_manager.rs`: resolve existing and deleted delta paths without allowing symlink or `..` escape.
- Modify `crates/tldr-core/src/artifact_store/ingestion.rs`: treat subjects missing from the new source manifest as regenerated/removed.
- Modify `crates/tldr-contract-tests/src/main.rs`: add containment and full-deletion certification scenarios.
- Modify `crates/tldr-cli/src/commands/daemon/watcher.rs`: make the steady-stream test prove the hard max-wait boundary.

### Task 1: Add failing certification scenarios

**Files:**
- Modify: `crates/tldr-contract-tests/src/main.rs`

- [x] **Step 1: Register source-containment and deletion scenarios**

Add certification-only entries:

```rust
Scenario {
    name: "source_path_containment",
    smoke: false,
    run: source_path_containment,
},
Scenario {
    name: "full_project_deletion",
    smoke: false,
    run: full_project_deletion,
},
```

- [x] **Step 2: Prove parser and daemon containment**

On Unix, create an external Python file and an in-project symlink to it. Assert that `FileFactsParser::parse` rejects it before incrementing its invocation counter and that `ArtifactManager::apply_delta` rejects it. Also delete a legitimate in-project file and assert that the same delta API accepts the deletion:

```rust
let parser = FileFactsParser::default();
ensure(parser.parse(project.path(), &link).is_err(), "parser followed external symlink")?;
ensure(parser.invocations() == 0, "parser read before containment validation")?;

let manager = ArtifactManager::open(project.path()).map_err(display)?;
manager.warm().map_err(display)?;
ensure(manager.apply_delta(&link).is_err(), "delta followed external symlink")?;
fs::remove_file(&inside).map_err(display)?;
manager.apply_delta(&inside).map_err(display)?;
```

- [x] **Step 3: Prove full ingestion removes deleted subjects**

Build a two-file project, delete one source, perform another project ingestion, and assert that the active snapshot and manifest contain neither file nor symbol subjects for the deleted path:

```rust
engine.ingest(IngestionScope::Project).map_err(display)?;
fs::remove_file(project.path().join("helper.py")).map_err(display)?;
engine.ingest(IngestionScope::Project).map_err(display)?;
let snapshot = GenerationSnapshot::active(store.as_ref())
    .map_err(display)?
    .ok_or("deletion generation missing")?;
ensure(snapshot.file("helper.py").is_none(), "deleted file remained queryable")?;
ensure(
    !store.generation(snapshot.generation()).map_err(display)?
        .ok_or("deletion manifest missing")?
        .artifacts.iter().any(|key| match &key.subject {
            ArtifactSubject::File(path) => path == "helper.py",
            ArtifactSubject::Symbol(anchor) => anchor.starts_with("helper.py::"),
            ArtifactSubject::Project => false,
        }),
    "deleted subject remained in manifest",
)?;
```

- [x] **Step 4: Run certification and verify the new cases fail**

Run:

```bash
cargo tldr-certification
```

Expected: `source_path_containment` and `full_project_deletion` fail against the pre-fix implementation.

### Task 2: Enforce containment before parsing

**Files:**
- Modify: `crates/tldr-core/src/artifact_store/file_facts.rs:197-213`

- [x] **Step 1: Canonicalize and validate before language detection**

At the start of `FileFactsParser::parse`, canonicalize both inputs, compute the validated relative path, and use the canonical values for every downstream parser/extractor:

```rust
let root = dunce::canonicalize(root)?;
let path = dunce::canonicalize(path)?;
let relative = root_relative(&root, &path)?;
let language = Language::from_path_with_siblings(&path)
    .or_else(|| Language::from_path(&path))
    .ok_or_else(|| crate::TldrError::UnsupportedLanguage(path.display().to_string()))?;
let (tree, source, language) = parse_file_with_lang(&path, Some(language))?;
```

Persist `relative` directly in `FileFacts.path` and pass `&root`/`&path` to extraction, structure, callgraph, and semantic chunking.

- [x] **Step 2: Run the containment scenario**

Run:

```bash
cargo tldr-certification
```

Expected: parser containment and zero-invocation assertions pass; daemon and full-deletion assertions may still fail.

### Task 3: Add deletion-safe daemon delta containment

**Files:**
- Modify: `crates/tldr-cli/src/commands/daemon/artifact_manager.rs:161-176`

- [x] **Step 1: Add a normalized relative-path helper**

Add a helper that canonicalizes an existing target, or for a missing target walks to the nearest existing ancestor, canonicalizes that ancestor, rejects any non-normal suffix component, reconstructs the resolved path, and strips it against the already-canonical project root:

```rust
fn delta_relative(project: &Path, file: &Path) -> tldr_core::TldrResult<String> {
    let resolved = if file.exists() {
        dunce::canonicalize(file)?
    } else {
        let mut ancestor = file;
        while !ancestor.exists() {
            ancestor = ancestor.parent().ok_or_else(|| {
                TldrError::DaemonError(format!("delta path {} has no existing ancestor", file.display()))
            })?;
        }
        let canonical_ancestor = dunce::canonicalize(ancestor)?;
        let suffix = file.strip_prefix(ancestor).map_err(|_| {
            TldrError::DaemonError(format!("delta path {} cannot be normalized", file.display()))
        })?;
        if suffix.components().any(|component| !matches!(component, std::path::Component::Normal(_))) {
            return Err(TldrError::DaemonError(format!(
                "delta path {} contains non-normal components",
                file.display()
            )));
        }
        canonical_ancestor.join(suffix)
    };
    let relative = resolved.strip_prefix(project).map_err(|_| {
        TldrError::DaemonError(format!(
            "delta path {} is outside project root {}",
            file.display(),
            project.display()
        ))
    })?;
    relative
        .to_str()
        .map(|value| value.replace('\\', "/"))
        .ok_or_else(|| TldrError::DaemonError("delta path is not valid UTF-8".into()))
}
```

- [x] **Step 2: Route `apply_delta` through the helper**

Replace its lexical `strip_prefix` block with:

```rust
let relative = delta_relative(&self.project, file)?;
self.ingest(IngestionScope::Files(vec![relative]))
```

- [x] **Step 3: Run certification**

Run:

```bash
cargo tldr-certification
```

Expected: source-path containment passes for the parser, symlinked delta, and legitimate deleted delta.

### Task 4: Remove stale deleted subjects from full generations

**Files:**
- Modify: `crates/tldr-core/src/artifact_store/ingestion.rs:89-182`

- [x] **Step 1: Derive removed paths from the previous manifest**

Before consuming `previous`, collect every prior file/symbol subject whose source path is absent from the new `revisions` map:

```rust
let removed_files = previous_keys
    .iter()
    .filter_map(|key| subject_file(&key.subject))
    .filter(|path| !revisions.contains_key(*path))
    .map(ToOwned::to_owned)
    .collect::<HashSet<_>>();
```

- [x] **Step 2: Include removed paths in project regeneration**

Build the project-scope set from changed current files plus removed previous files:

```rust
IngestionScope::Project => selected
    .iter()
    .map(|path| relative(&self.root, path))
    .chain(removed_files.iter().cloned())
    .collect::<HashSet<_>>(),
```

This causes the existing `subject_changed` filter to remove file and symbol keys for deleted subjects before the new manifest is published.

- [x] **Step 3: Run certification**

Run:

```bash
cargo tldr-certification
```

Expected: all certification scenarios pass and the deleted file is absent from both snapshot and manifest.

### Task 5: Restore the watcher max-wait acceptance proof

**Files:**
- Modify: `crates/tldr-cli/src/commands/daemon/watcher.rs:478-494`

- [x] **Step 1: Keep the stream inside quiet debounce**

Emit events every 500 ms through 4.5 seconds, assert no flush at 4.999 seconds, and assert the delta at 5 seconds:

```rust
for half_second in 0..10 {
    assert_eq!(
        pipeline.accept(
            file.clone(),
            started + Duration::from_millis(half_second * 500),
        ),
        None
    );
}
assert_eq!(
    pipeline.flush_due(started + Duration::from_millis(4_999)),
    None
);
assert_eq!(
    pipeline.flush_due(started + Duration::from_secs(5)),
    Some(Flush::Delta(vec![file]))
);
```

- [x] **Step 2: Run the focused watcher test**

Run:

```bash
cargo test -p tldr-cli steady_stream_flushes_at_first_event_max_wait --all-features
```

Expected: one test passes.

### Task 6: Validate, record, and deliver

**Files:**
- Modify: Beads status/notes only after gates pass.

- [x] **Step 1: Run formatting**

Run:

```bash
cargo fmt --all --check
```

Expected: exit 0.

- [x] **Step 2: Run workspace lint and tests**

Run:

```bash
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --all-features -- -D warnings
CARGO_INCREMENTAL=0 cargo test --workspace --all-targets --all-features
```

Expected: both exit 0.

- [x] **Step 3: Run contract profiles**

Run:

```bash
cargo tldr-smoke
cargo tldr-certification
```

Expected: both profiles report zero failures.

- [x] **Step 4: Update and close Beads work**

Record direct evidence on `TLDR-wa2g.2` and `TLDR-ac0.7`, close both, then close `TLDR-wa2g`. CodeRabbit is omitted by explicit user direction.

- [x] **Step 5: Commit and push**

Run:

```bash
git add crates/tldr-core/src/artifact_store/file_facts.rs \
  crates/tldr-core/src/artifact_store/ingestion.rs \
  crates/tldr-cli/src/commands/daemon/artifact_manager.rs \
  crates/tldr-cli/src/commands/daemon/watcher.rs \
  crates/tldr-contract-tests/src/main.rs \
  docs/superpowers/plans/2026-07-27-artifact-containment-and-deletion-hardening.md \
  .beads/interactions.jsonl
git commit -m "fix(artifact-store): enforce containment and deletion cleanup"
git pull --rebase
bd dolt push
git push
git status --short --branch
```

Expected: branch is up to date with its remote and the worktree is clean.

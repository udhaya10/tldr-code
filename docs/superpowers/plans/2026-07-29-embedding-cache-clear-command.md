# Embedding Cache Clear Command Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `tldr embeddings clear` as the safe, explicit way to remove reusable document embeddings without deleting downloaded model weights.

**Architecture:** Register a top-level `embeddings` command group with a `clear` subcommand. The command resolves one fixed target from `CacheConfig::default().cache_dir`, never accepts an arbitrary path, recursively counts and removes entries without following symlinks, and leaves sibling `fastembed` and `stores` directories untouched. It emits the same structured file/byte accounting style as `tldr cache clear`.

**Tech Stack:** Rust, clap subcommands, serde output, filesystem metadata, tempfile unit tests.

---

### Task 1: Implement bounded embedding-cache deletion

**Files:**
- Create: `crates/tldr-cli/src/commands/embeddings.rs`
- Modify: `crates/tldr-cli/src/commands/mod.rs`
- Test: `crates/tldr-cli/src/commands/embeddings.rs`

- [ ] **Step 1: Write failing filesystem tests**

Create a temporary cache parent containing:

```text
embeddings/cache.redb
embeddings/nested/recovered.redb
embeddings/external-link -> external sentinel directory
fastembed/model.onnx
stores/other-project/index.usearch
```

Call an internal `clear_embedding_cache_at(&embeddings)` seam and assert that
the embeddings directory and symlink are gone, the external sentinel survives,
and both sibling directories survive.

- [ ] **Step 2: Verify the helper is missing**

Run:

```bash
cargo test -p tldr-cli embedding_cache_clear --lib
```

Expected: compilation failure because the command/helper does not exist.

- [ ] **Step 3: Implement the command**

Define:

```rust
#[derive(Debug, Clone, Args)]
pub struct EmbeddingsClearArgs {}

#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingsClearOutput {
    pub status: String,
    pub cache_dir: PathBuf,
    pub files_removed: usize,
    pub bytes_freed: u64,
    pub size_freed_human: String,
    pub model_cache_preserved: bool,
    pub message: String,
}
```

`run` resolves only `CacheConfig::default().cache_dir`. The recursive helper
uses `symlink_metadata`, treats symlinks as files, removes children before
directories, and returns `(files_removed, bytes_freed)`. Missing cache is a
successful idempotent no-op.

- [ ] **Step 4: Export the argument type**

Add `pub mod embeddings;` and `pub use embeddings::EmbeddingsClearArgs;` in
`crates/tldr-cli/src/commands/mod.rs`.

- [ ] **Step 5: Run focused tests**

Run:

```bash
cargo test -p tldr-cli embedding_cache_clear --lib
```

Expected: containment, sibling-preservation, byte-accounting, and idempotency
tests pass.

### Task 2: Register `tldr embeddings clear`

**Files:**
- Modify: `crates/tldr-cli/src/main.rs`
- Test: `crates/tldr-cli/src/main.rs`

- [ ] **Step 1: Write a failing clap parse test**

Assert:

```rust
let cli = Cli::try_parse_from(["tldr", "embeddings", "clear"]).unwrap();
assert!(matches!(
    cli.command,
    Command::Embeddings(EmbeddingsCommand::Clear(_))
));
```

- [ ] **Step 2: Add the command group**

Define `EmbeddingsCommand` with `Clear(EmbeddingsClearArgs)`, add
`Command::Embeddings`, include `"embeddings clear"` in the stable command-name
mapping, and dispatch to `args.run(cli.format, cli.quiet)`.

- [ ] **Step 3: Validate CLI output**

Run:

```bash
cargo run -p tldr-cli -- embeddings clear --help
```

Expected: help states that the cache is global, model weights are preserved,
and all daemons/builders should be stopped first.

### Task 3: Document, install, and use the command

**Files:**
- Modify: `docs/TROUBLESHOOTING.md`
- Modify: `docs/commands/daemon.md`
- Modify: Beads issues `TLDR-by82.1` and `TLDR-by82`

- [ ] **Step 1: Document cache boundaries**

Add a table distinguishing:

```text
tldr cache clear --project PATH  -> project artifacts/vector generations
tldr embeddings clear           -> global reusable document vectors
preserved automatically         -> downloaded fastembed models/tokenizer
```

Warn that `embeddings clear` affects every project and should run only after
stopping daemons/builders.

- [ ] **Step 2: Run quality gates**

Run:

```bash
cargo fmt --check
cargo test -p tldr-cli --all-features --lib
cargo clippy -p tldr-cli --all-targets --all-features -- -D warnings
```

- [ ] **Step 3: Install and verify**

Install the release binary, stop all active tldr builders/daemons, run
`tldr embeddings clear`, and verify the JSON reports nonzero files/bytes while
`~/Library/Caches/tldr/fastembed` remains present.

- [ ] **Step 4: Continue the cold benchmark**

Run `tldr cache clear --project <project>`, prove both cache classes absent,
then execute the correlated fully cold benchmark in
`docs/superpowers/plans/2026-07-29-fully-cold-runtime-benchmark.md`.

- [ ] **Step 5: Close and synchronize**

Close `TLDR-by82.1`, update the parent benchmark issue, commit command/tests/docs,
push Beads, and push `main` to `fork` before publishing benchmark artifacts.

# Semantic Context-Root Coalescing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve all indexed source text while preventing comments, attributes, and imports from becoming thousands of standalone semantic embedding documents.

**Architecture:** Keep the existing deterministic `split_node` planner and token-budget merge loop. Add a small syntax-kind classification that treats trivia/context roots as attachable input, then preserve a boundary only after the current group owns a retrieval-bearing declaration. Bump the persisted chunk-pipeline identities so stores and resumable workers cannot mix old and new boundaries.

**Tech Stack:** Rust, Tree-sitter, Hugging Face `tokenizers`, Redb/usearch semantic store, Cargo tests.

---

### Task 1: Lock the intended grouping behavior with planner tests

**Files:**
- Modify: `crates/tldr-core/src/semantic/structural_planner.rs`

- [ ] **Step 1: Add a deterministic test tokenizer and Rust source fixture**

```rust
#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use tokenizers::models::wordlevel::WordLevel;
    use tokenizers::{Tokenizer, TruncationParams};

    use super::*;
    use crate::Language;

    fn budget() -> TokenBudget {
        let model = WordLevel::builder()
            .vocab(HashMap::from([("[UNK]".to_string(), 0)]))
            .unk_token("[UNK]".to_string())
            .build()
            .expect("word-level tokenizer");
        let mut tokenizer = Tokenizer::new(model);
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: 512,
                ..Default::default()
            }))
            .expect("truncation config");
        TokenBudget::from_configured_tokenizer(&tokenizer).expect("token budget")
    }

    fn rust_file(source: &str) -> CodeChunk {
        CodeChunk {
            file_path: PathBuf::from("src/lib.rs"),
            function_name: None,
            class_name: None,
            line_start: 1,
            line_end: source.lines().count() as u32,
            content: source.to_string(),
            content_hash: format!("{:x}", md5::compute(source.as_bytes())),
            language: Language::Rust,
            structure: Default::default(),
        }
    }
}
```

- [ ] **Step 2: Add a failing regression test for comments, attributes, and imports**

```rust
#[test]
fn context_roots_attach_to_the_following_semantic_owner() {
    let source = concat!(
        "/// first docs\n",
        "#[inline]\n",
        "use std::fmt;\n",
        "fn first() {}\n",
        "// second docs\n",
        "fn second() {}\n",
    );

    let chunks = plan_chunks(
        &[rust_file(source)],
        &budget(),
        ChunkGranularity::Function,
    )
    .expect("planning succeeds");

    assert_eq!(chunks.len(), 2);
    assert!(chunks[0].content.contains("/// first docs"));
    assert!(chunks[0].content.contains("#[inline]"));
    assert!(chunks[0].content.contains("use std::fmt"));
    assert!(chunks[0].content.contains("fn first"));
    assert!(!chunks[0].content.contains("// second docs"));
    assert!(chunks[1].content.contains("// second docs"));
    assert!(chunks[1].content.contains("fn second"));
    assert_eq!(
        chunks.iter().map(|chunk| chunk.content.as_str()).collect::<String>(),
        source
    );
    assert_eq!(
        chunks
            .iter()
            .map(|chunk| chunk.structure.qualified_symbol.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("first"), Some("second")]
    );
}
```

- [ ] **Step 3: Run the focused test and verify the old planner fails**

Run:

```bash
cargo test -p tldr-core semantic::structural_planner::tests::context_roots_attach_to_the_following_semantic_owner --lib -- --exact
```

Expected: `FAILED`; the old planner emits each named comment, attribute, and use declaration as an independent chunk.

### Task 2: Implement owner-aware root coalescing

**Files:**
- Modify: `crates/tldr-core/src/semantic/structural_planner.rs`

- [ ] **Step 1: Add the minimal context-root classifier**

```rust
fn is_context_root_kind(kind: &str) -> bool {
    kind.contains("comment")
        || matches!(
            kind,
            "attribute_item"
                | "inner_attribute_item"
                | "use_declaration"
                | "import_statement"
                | "import_from_statement"
                | "future_import_statement"
                | "import_declaration"
                | "import_header"
        )
}
```

- [ ] **Step 2: Add a helper that detects whether the current group already owns a semantic declaration**

```rust
fn segment_is_semantic_owner(segment: &Segment<'_>) -> bool {
    segment
        .node
        .is_some_and(|node| !is_context_root_kind(node.kind()))
}
```

- [ ] **Step 3: Replace the unconditional named-root break**

Replace:

```rust
if preserve_named_roots && next.node.is_some() {
    break;
}
```

with:

```rust
if preserve_named_roots
    && next.node.is_some()
    && segments[index..=last].iter().any(segment_is_semantic_owner)
{
    break;
}
```

This permits leading comments/attributes/imports to merge forward into the next owner, but prevents comments belonging to the next declaration from merging backward into the current owner.

- [ ] **Step 4: Run the focused regression tests**

Run:

```bash
cargo test -p tldr-core semantic::structural_planner::tests --lib
```

Expected: all structural planner tests pass, including exact source reconstruction and symbol ownership.

### Task 3: Invalidate stores built with old chunk boundaries

**Files:**
- Modify: `crates/tldr-core/src/semantic/store_search.rs`
- Modify: `crates/tldr-core/src/semantic/worker_protocol.rs`

- [ ] **Step 1: Bump the store chunk-pipeline identity**

```rust
pub(crate) const CHUNK_WALKER_VERSION: &str = "w2";
```

- [ ] **Step 2: Bump the worker chunking identity**

```rust
pub const WORKER_PIPELINE_VERSION: &str = "structural-embedding-v2";
```

- [ ] **Step 3: Run protocol and manifest tests**

Run:

```bash
cargo test -p tldr-core semantic::worker_protocol --lib
cargo test -p tldr-core semantic::store_search --lib
```

Expected: all compatibility tests pass with the new identities.

### Task 4: Validate the production contract and record measured impact

**Files:**
- Modify: `docs/benchmarks/2026-07-28-clean-mixed-batch/SUMMARY.md`
- Update: Beads issues `TLDR-1hld.3`, `TLDR-1hld.12`, and parent `TLDR-1hld`

- [ ] **Step 1: Run formatting and focused quality gates**

Run:

```bash
cargo fmt --all -- --check
cargo test -p tldr-core semantic::structural_planner::tests --lib
cargo test -p tldr-core semantic::worker_protocol --lib
cargo check --workspace --all-targets
git diff --check
```

Expected: every command exits successfully.

- [ ] **Step 2: Re-run the no-inference planner audit**

Use the GenerationSnapshot replay described in `TLDR-1hld.1` and record:

```text
files
documents
tokens
roles
AST root kinds
qualified-symbol count
token buckets
source bytes
planned bytes
overlap bytes
top 20 files by document count
```

Expected: 511 files remain covered; standalone comment/import/attribute document counts fall sharply; planned source bytes still equal source bytes plus explicitly reported bounded overlap; no document exceeds 512 tokens.

- [ ] **Step 3: Verify the same-file bulk/delta contract**

Run the planner fixture through both `plan_chunks` and `plan_structural_delta_from_artifact` and assert identical content, structural roles, AST paths, source ranges, symbols, and signatures.

- [ ] **Step 4: Commit the implementation**

```bash
git add crates/tldr-core/src/semantic/structural_planner.rs \
  crates/tldr-core/src/semantic/store_search.rs \
  crates/tldr-core/src/semantic/worker_protocol.rs \
  crates/tldr-core/examples/context_root_audit.rs \
  docs/benchmarks/2026-07-28-clean-mixed-batch/SUMMARY.md \
  docs/superpowers/plans/2026-07-28-semantic-context-root-coalescing.md
git commit -m "perf(semantic): coalesce context roots into owners"
```

- [ ] **Step 5: Update Beads with final versus intermediate evidence**

Close `TLDR-1hld.12` only after source coverage, bulk/delta parity, retrieval checks, and measured document reduction pass. Keep `TLDR-1hld.3` open if normalized-duplicate or retrieval-oracle acceptance remains incomplete.

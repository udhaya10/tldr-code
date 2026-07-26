# Fixed-Shape Parity Matrix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Execute this plan inline in the current session. Beads `TLDR-9bxa.5` remains the authoritative task tracker; the checkboxes below describe implementation order only.

**Goal:** Prove numerical parity, padding invariance, and batch-composition invariance for every supported Arctic model across the 128/256/384/512 fixed-shape buckets.

**Architecture:** Add reusable, serializable vector-comparison and parity-report types to `tldr-core`, then add a diagnostic example that loads one model at a time, runs the FastEmbed oracle and direct ORT candidate against the same commit-pinned artifacts, and emits a JSON report. The public FastEmbed API owns its dynamic padding and cannot accept prepared rows, so parity is established by triangulation: compare every fixed candidate to FastEmbed's final normalized vector, then separately prove the candidate vector is identical across all four zero-attention padding lengths. Batch-composition invariance is a third, separately labeled case in which the planner fills unused rows with valid attended dummy inputs and discards their outputs.

**Tech Stack:** Rust, FastEmbed 5.8.1, tokenizers 0.22, ONNX Runtime 2.0.0-rc.11, serde/serde_json, Cargo examples.

---

### Task 1: Reusable parity metrics and declared tolerances

**Files:**
- Create: `crates/tldr-core/src/semantic/fixed_shape_parity.rs`
- Modify: `crates/tldr-core/src/semantic/mod.rs`

- [x] **Step 1: Write failing unit tests**

Add tests proving identical normalized vectors pass, a vector exceeding `max_absolute_difference` fails, and a vector below `minimum_cosine_similarity` fails.

- [x] **Step 2: Run the focused test**

Run:

```bash
cargo test -p tldr-core semantic::fixed_shape_parity --lib
```

Expected before implementation: compilation fails because the parity types do not exist.

- [x] **Step 3: Implement the metric types**

Define serializable `ParityTolerance`, `VectorParity`, `ParityCase`, `ModelParityReport`, and `ParityMatrixReport`. Use declared defaults of `max_absolute_difference = 1e-5` and `minimum_cosine_similarity = 0.99999`. Reject length mismatches and non-finite vector elements explicitly.

- [x] **Step 4: Re-run focused tests**

Expected: all `fixed_shape_parity` tests pass.

### Task 2: All-model/all-bucket diagnostic harness

**Files:**
- Create: `crates/tldr-core/examples/fixed_shape_parity.rs`

- [x] **Step 1: Implement one-model matrix execution**

For one `EmbeddingModel`, load `Embedder` as the FastEmbed oracle, clone its `ResolvedModelArtifacts`, construct `FixedShapeOrtBackend`, and tokenize a stable source snippet once. Keep the original token count separate, then produce four prepared rows with exact tensor sequence lengths `128`, `256`, `384`, and `512` using the real pad token, zero attention mask, and zero token-type IDs. Fail if the stable fixture does not fit the smallest declared shape.

- [x] **Step 2: Add padding and batch-composition cases**

For each bucket, record three independent comparisons: candidate versus FastEmbed's final normalized vector (`oracle_parity`), the 128-bucket candidate versus the current zero-attention padded bucket (`padding_parity`), and the target embedded alone versus in a two-real-row batch with valid attended dummy rows filling the remainder (`batch_composition_parity`). Record the exact `(batch, sequence)` shape. Because padding parity is exactly checked separately, any residual candidate/oracle difference is attributable to backend execution rather than padded token positions.

- [x] **Step 3: Run models sequentially**

Run `ArcticXS`, `ArcticS`, `ArcticM`, `ArcticMLong`, and `ArcticL` one at a time so only one oracle/candidate pair is resident. Emit a pretty JSON `ParityMatrixReport` to stdout and exit non-zero if any case fails.

- [x] **Step 4: Validate the harness**

Run:

```bash
cargo run --release -p tldr-core --example fixed_shape_parity
```

Expected: 20 model/bucket cases, exactly four declared shapes per model, every parity/invariance comparison passing, and process exit code 0.

### Task 3: Quality gates and checkpoint

**Files:**
- Modify only files from Tasks 1–2 if validation exposes a defect.

- [x] **Step 1: Run formatting and focused tests**

```bash
rustfmt --edition 2021 --check crates/tldr-core/src/semantic/fixed_shape_parity.rs crates/tldr-core/examples/fixed_shape_parity.rs
cargo test -p tldr-core semantic::fixed_shape_parity --lib
cargo test -p tldr-core semantic::fixed_shape_ort --lib
```

- [x] **Step 2: Run strict lint**

```bash
cargo clippy -p tldr-core --lib --examples -- -D warnings
```

- [x] **Step 3: Review the staged diff**

Run CodeRabbit against staged changes. Fix valid major/warning findings and rerun focused validation.

- [x] **Step 4: Record and push**

Commit the parity matrix separately, add the exact matrix results and any unavailable model/cache failures to `TLDR-9bxa.5`, then run `git pull --rebase`, `bd dolt push`, and `git push`.

## Self-review

- Spec coverage: all five Arctic models, all four finite buckets, numerical parity, padding invariance, batch-composition invariance, exact-shape reporting, and valid dummy-row execution are covered.
- Deliberate exclusions: RSS plateau and throughput measurement are the following empirical-shape phase; no runtime default switch occurs here.
- Type consistency: the harness consumes existing `TokenBudget`, `FixedShapePlanner`, `FixedShapeOrtBackend`, and `ResolvedModelArtifacts` public APIs without adding a second tokenizer path.

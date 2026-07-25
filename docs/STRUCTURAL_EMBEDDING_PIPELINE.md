# Structural Embedding Pipeline

Status: Proposed  
Date: 2026-07-25  
Beads: `TLDR-vbw0`, `TLDR-vbw0.1`, `TLDR-3rh`, `TLDR-k8s`

## Purpose

Replace the current character-truncated, dynamically shaped embedding build with
a structure-aware pipeline whose peak inference memory is bounded independently
of corpus size.

The pipeline treats embedding as a compiler stage:

```text
source
  -> AST structural units
  -> token-budgeted embedding documents
  -> exact finite-shape batches
  -> isolated ONNX worker
  -> redb authoritative records
  -> usearch derived search index
```

This design addresses three separate concerns:

1. Retrieval quality: chunks preserve syntactic and semantic boundaries.
2. Memory safety: ONNX receives a finite set of tensor shapes.
3. Operational safety: bulk inference is resumable and process-isolated.

## Current Problem

The current semantic build:

1. Creates function-level `CodeChunk` values.
2. Truncates chunks at a default 4,000-character boundary.
3. Optionally composes enriched text capped at 2,000 characters.
4. Passes all cache misses to FastEmbed.
5. FastEmbed pads each batch to its longest sequence.

On the measured `tldr-code` corpus, the ONNX Runtime CPU arena retained 195
uniform 128 MiB regions and reached a 25.75 GiB peak. ONNX Runtime documents
that its arenas do not return unused regions to the system by default. Dynamic
batch shapes therefore accumulate retained arena capacity over a long build.

The current length-sorting change in `TLDR-vbw0.1` is a useful optimization, but
it is not a memory-bound guarantee. Sorting by bytes can still produce hundreds
of distinct token lengths and tensor shapes.

## Design Principles

1. AST structure determines semantic boundaries.
2. The model tokenizer determines size and bucket assignment.
3. The shape planner determines exact tensor dimensions.
4. Bounded queues prevent whole-corpus materialization between stages.
5. Query, delta, and bulk workloads use separate inference sessions.
6. redb is the durable source of truth; usearch is a derived search structure.
7. Bulk ONNX inference runs in a child process so process exit guarantees
   allocator reclamation.
8. No stage silently truncates source or model input.

## 1. AST Structural Chunk Planner

Replace `truncate_if_needed(..., 4000 chars)` with deterministic recursive AST
planning.

### Planning algorithm

1. Parse the file using the existing pinned tree-sitter grammar.
2. Select semantic roots such as declarations, functions, methods, classes, and
   modules.
3. Keep a root whole when its final composed model input fits the token budget.
4. Recursively split an oversized root through named AST children.
5. Greedily merge adjacent sibling nodes while the composed input still fits.
6. If an indivisible leaf remains oversized, split it into tokenizer windows
   with explicit, recorded overlap.
7. Fall back deterministically for parse-error regions.

An oversized function can produce:

```text
function summary
  + setup statements
  + loop/body block
  + error-handling block
  + return/finalization block
```

Every child carries minimal ancestor context:

- repository-relative file path
- language
- qualified class/module and symbol
- function or method signature
- structural role and AST path
- selected callers and callees
- selected CFG/DFG facts

The parent summary supports symbol-level retrieval while child chunks preserve
implementation detail.

### Required properties

- Identical source and configuration produce an identical chunk plan.
- Intended source bytes are covered completely.
- Duplication occurs only through explicitly recorded overlap.
- Chunk boundaries do not depend on line-number shifts.
- Oversized code is split, never silently discarded.

## 2. Embedding Document Composer

Use one versioned recipe to turn a structural chunk into model input:

```text
Symbol: VectorStore::build
Kind: method
File: semantic/vector_store.rs
Signature: pub fn build(...)
Calls: chunk_code, Embedder::new
Control flow: complexity=...
Data flow: ...
Code:
<complete AST-aligned source>
```

When the token budget is constrained, content is prioritized in this order:

1. symbol identity and signature
2. source code
3. caller/callee context
4. CFG/DFG summary
5. documentation and dependency context

The budget includes model special tokens and any query/document prefix.
Character counts may guide initial partitioning, but the final composed input
must be tokenized and checked against the actual model limit.

## 3. Identity and Revision Model

Logical identity and content revision are distinct:

```rust
struct ChunkId(u128);
struct ChunkRevision([u8; 32]);
```

`ChunkId` represents lineage across localized edits. `ChunkRevision` hashes the
exact composed embedding document.

When a file changes, reconcile its new structural chunks against prior redb
records using:

1. qualified symbol plus structural role/path
2. unique structural anchor
3. exact content match under the same enclosing symbol
4. ordered structural matching
5. a new ID for unmatched chunks

Embedding reuse uses a separate cache key:

```text
pipeline schema version
+ model ID and revision
+ tokenizer ID and revision
+ document/query mode
+ pooling and normalization version
+ composed input hash
```

Parser, chunker, or enrichment changes increment the pipeline schema whenever
they can change the composed model input.

## 4. Tokenization and Exact Shape Planning

Tokenize each final document once and place it into a finite sequence bucket:

```text
128, 256, 384, or 512 tokens
```

Padding is applied to token tensors using the tokenizer's pad token and
attention mask. Padding source strings with spaces is incorrect because the
model would attend to those tokens.

Batch size is constrained by both token and attention budgets:

```text
batch_size = min(
    max_requests,
    token_budget / sequence_length,
    attention_budget / sequence_length^2
)
```

Initial bulk candidates:

| Sequence length | Batch size | Batch x length^2 |
| ---: | ---: | ---: |
| 128 | 64 | 1,048,576 |
| 256 | 16 | 1,048,576 |
| 384 | 7 | 1,032,192 |
| 512 | 4 | 1,048,576 |

These values are starting points. Final values are selected from measured RSS
and throughput for every supported model.

Partial batches are filled with valid dummy inputs and their outputs discarded,
so ONNX still sees an exact declared shape. Fully masked dummy rows are not used
because mean pooling may divide by a zero attention-mask sum.

## 5. Path-Specific Inference

### Query path

- Dedicated resident query session.
- Arctic query prefix applied before tokenization.
- One or a small finite set of `batch=1` query shapes.
- Never shares an arena with document indexing.
- Optimized for latency and a small resident footprint.

### File-delta path

- Rechunk only the changed file.
- Reconcile stable chunk identities.
- Reuse unchanged revisions from redb.
- Embed misses with a small fixed-shape runner.
- Apply additions and removals to resident usearch.
- Corpus-global enrichment requires explicit dependency invalidation; otherwise
  it is excluded from the incremental recipe.

### Bulk-build path

- AST planning and durable job state remain outside the ONNX worker.
- A child process owns the bulk model session.
- Bounded IPC carries only a few tokenized batches at a time.
- Results are checkpointed incrementally.
- The worker exits after completion, guaranteeing arena reclamation.
- An RSS watermark restarts the worker from its latest checkpoint if needed.
- The live `VectorStore` write lock is not held during parsing or embedding.

## 6. redb Storage Model

redb is used without rkyv in the initial design.

Suggested tables:

```text
metadata
  active_generation
  pipeline schema
  model and tokenizer revision

files
  path -> content hash, stat signal, chunk-plan version

chunks
  ChunkId -> file, anchor, range, revision, metadata

file_chunks
  file path -> ChunkId multimap

embeddings
  cache key -> little-endian f32 bytes

jobs
  generation/chunk -> pending, running, complete, failed
```

The redb cache size must be configured explicitly. redb 4.1.0 defaults to a
1 GiB cache, which is too large to accept without measurement.

### redb and usearch publication

redb and usearch do not share a transaction. Use a generation protocol:

1. Write completed chunk and embedding records into a staged redb generation.
2. Build and sync the staged usearch index.
3. Rename the staged usearch file into its final generation path.
4. Switch `active_generation` in an immediate redb transaction.

On startup:

```text
if usearch generation == redb active generation:
    load usearch
else:
    rebuild usearch from redb embedding records
```

A crash therefore leaves the previous complete generation active or leaves an
unreferenced staged artifact. It never publishes a mixed generation.

## 7. Embedding Backend Boundary

Introduce a backend interface:

```rust
trait EmbeddingBackend {
    fn tokenize(&self, text: &str, mode: InputMode) -> TokenizedInput;
    fn embed_fixed(&mut self, batch: FixedShapeBatch)
        -> Result<Vec<Vec<f32>>>;
}
```

Initial implementations:

- `FastEmbedBackend`: current behavior and numerical oracle.
- `FixedShapeOrtBackend`: explicit token IDs, attention masks, tensor shapes,
  pooling, and normalization.

The fixed backend must reproduce FastEmbed's tokenizer, pooling, and
normalization. It is not enabled by default until numerical-parity and
padding-invariance tests pass.

Where the ORT binding permits it, evaluate:

- `arena_extend_strategy = kSameAsRequested`
- an explicit arena `max_mem`
- arena shrinkage after bulk runs
- memory-pattern enabled versus disabled

These controls are defense in depth. Process isolation remains the hard memory
reclamation guarantee.

## 8. Bounded Pipeline

The bulk pipeline uses bounded producer/consumer stages:

```text
file enumeration
  -> parse and structural planning
  -> document composition
  -> cache lookup
  -> tokenization and bucket queue
  -> fixed-shape inference
  -> redb checkpoint
  -> staged usearch construction
```

No stage retains the entire corpus of source, enriched text, token tensors, and
vectors simultaneously. Queue capacities are part of the memory budget.

## 9. Delivery Plan

### Phase 0: Instrumentation

- Record token counts and exact ONNX tensor shapes.
- Sample current RSS over the entire cold build.
- Record per-batch latency, padding ratio, cache behavior, and throughput.
- Preserve the 25.75 GiB reproduction as the baseline.

Gate: measurements are reproducible on a fixed corpus.

### Phase 1: Token-budgeted AST chunking

- Add recursive split-and-merge planning.
- Add deterministic fallback and source-coverage tests.
- Keep the current FastEmbed backend.

Gate: no silent truncation, deterministic plans, complete intended coverage,
and no retrieval regression.

### Phase 2: Fixed-shape backend

- Add explicit tokenizer and ONNX tensor construction.
- Match FastEmbed pooling and normalization.
- Keep FastEmbed as the comparison oracle.

Gate: numerical parity, padding invariance, and batch-composition invariance.

### Phase 3: Separate sessions and empirical shape plan

- Separate query, delta, and bulk inference.
- Measure candidate batch sizes for each model and bucket.

Gate: RSS plateaus, peak is within budget, and query latency remains stable.

### Phase 4: redb cache and checkpoints

- Replace whole-map cache generations with per-record redb tables.
- Add file-level invalidation and resumable job state.

Gate: unchanged chunks retain identities and cache hits; crash tests cannot
publish mixed generations.

### Phase 5: Bulk worker isolation

- Move bulk ONNX inference to a child process.
- Add bounded IPC, checkpoints, worker watermark, and restart behavior.

Gate: worker memory disappears after exit and a killed build resumes safely.

### Phase 6: Default rollout

- Build a fresh incompatible generation.
- Compare retrieval quality and performance with the current implementation.
- Retain the previous complete generation as rollback.

## 10. Acceptance Criteria

- Cold-build peak RSS is at most 4 GiB and visibly plateaus.
- RSS does not grow monotonically across repeated heterogeneous batches.
- Every model input respects the true token limit.
- No arbitrary source truncation remains.
- All intended source is represented, with explicit overlap accounting.
- Unchanged chunks retain IDs after localized edits.
- One-file edits re-embed only changed chunks and declared dependents.
- Fixed-shape vectors meet the agreed numerical tolerance against FastEmbed.
- Retrieval quality does not regress on the existing gold set.
- Wall time remains within 10% unless a measured quality improvement justifies
  the cost.
- Bulk indexing does not materially regress query p95 latency.
- Injected crashes recover to either the prior complete generation or the new
  complete generation, never a mixture.
- Bulk-only ONNX memory is returned when the worker exits.

## Research Basis

- ONNX Runtime documents that arena regions are retained by default, supports
  `kSameAsRequested`, arena size controls, shared allocators, and end-of-run
  arena shrinkage.
- Hugging Face Text Embeddings Inference uses token-based dynamic batching and
  exposes a total token budget as a primary capacity control.
- The cAST paper uses recursive AST splitting plus greedy sibling merging and
  reports improvements of up to 4.3 Recall points and 2.67 Pass@1 points over
  fixed-size chunking.

Primary sources:

- [ONNX Runtime C API and arena configuration](https://onnxruntime.ai/docs/get-started/with-c.html)
- [ONNX Runtime memory guidance](https://onnxruntime.ai/docs/performance/tune-performance/memory.html)
- [Hugging Face Text Embeddings Inference](https://github.com/huggingface/text-embeddings-inference)
- [cAST: Enhancing Code Retrieval-Augmented Generation with Structural Chunking via Abstract Syntax Tree](https://arxiv.org/html/2506.15655v1)
- [FastEmbed Rust](https://github.com/Anush008/fastembed-rs)

Saved Firecrawl evidence is under `.firecrawl/onnx-*`,
`.firecrawl/embedding-batching-*`, `.firecrawl/tei-*`,
`.firecrawl/cast-*`, and `.firecrawl/fastembed-*`.

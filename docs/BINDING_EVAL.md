# AST Binding Evaluation

Call-graph variable bindings are extracted from tree-sitter nodes during the
shared file parse and stored in `FileIR.var_types`. Compose performs scoped
joins over those facts; it does not scan source text for receiver types.

Because this migration intentionally improves output, its rollout gate is
labeled edge precision/recall rather than byte parity with the retired text
oracle. Run a suite with:

```bash
cargo run -p tldr-core --example callgraph_binding_eval -- \
  crates/tldr-core/fixtures/binding_eval/python \
  crates/tldr-core/fixtures/binding_eval/python/labels.json
```

The suite schema contains:

- `language`: builder language;
- optional `callee_suffix`: the binding-sensitive edge slice to evaluate;
- `expected`: reviewed source-file/caller/destination-file/callee identities.

The command prints deterministic JSON with true positives, false positives,
false negatives, precision, recall, F1, unexpected edges, and missing edges. It
exits nonzero on any mismatch.

The initial adversarial fixture distinguishes `x: User` from both `max:
Maximum` and a comment containing `x: Maximum`. Its reviewed `User.save` edge
scores precision 1.0, recall 1.0, and F1 1.0. New binding behavior must add
reviewed fixtures before changing the extractor.

This output-changing cutover uses call-graph IR schema `2.0` and FileFacts
producer version `7`, forcing an artifact rebuild instead of decoding old
binding semantics as current facts.

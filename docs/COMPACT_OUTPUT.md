# Compact output for agents

Use `--format compact` (`-f compact`) for graph-shaped answers consumed by an
LLM or MCP adapter:

```bash
tldr -f compact calls src/
tldr -f compact impact parse_config src/
tldr -f compact dead src/
tldr -f compact hubs src/
tldr -f compact structure src/
```

JSON remains the compatibility format when a consumer needs every serialized
field. Compact is the primary agent surface: it removes repeated object keys,
redundant graph inventories, and repeated path prefixes before the answer
enters the model context.

## Compact-v1 contract

Compact-v1 is escaped TSV:

- The first row starts with `@<command>`, then format version `1`, then
  `key=value` metadata cells.
- The second row starts with `@columns` and names subsequent row fields.
- Data rows begin with a row kind such as `edge`, `caller`, `dead`, `hub`, or
  `function`.
- Backslash, tab, carriage return, and newline inside a cell are encoded as
  `\\`, `\t`, `\r`, and `\n`.
- Rows and target groups are deterministic. Impact targets are sorted.
- Counts in the metadata row describe the complete analysis even when compact
  deliberately omits redundant inventory, such as the full `calls` node list.

Consumers should reject an unknown version rather than guessing at its shape.
Format changes require a new version and updated golden tests.

## Token measurement

Measured on 2026-07-27 against this repository with the same analysis limits
for each JSON/compact pair. Token counts use `tiktoken 0.12.0` and
`cl100k_base`; model-specific counts vary, but the relative reduction is the
product gate.

| Command | JSON tokens | Compact tokens | Reduction |
|---|---:|---:|---:|
| `calls --max-items 200` | 155,430 | 7,771 | 20.00x |
| `impact compact_cell` | 1,255 | 572 | 2.19x |
| `dead --max-items 200` | 30,351 | 8,416 | 3.61x |
| `hubs --algorithm indegree --top 50` | 9,315 | 2,225 | 4.19x |
| `structure --max-results 100` | 161,449 | 74,675 | 2.16x |

Every measured command clears the 2x floor. The unusually large `calls`
improvement comes from omitting the full node inventory: edge rows are the
answer, while the complete node count remains in metadata. Use JSON if the
individual isolated node names are required.

The measurements can be reproduced by saving paired outputs as
`<command>.json` and `<command>.compact`, then running:

```python
import tiktoken

enc = tiktoken.get_encoding("cl100k_base")
json_tokens = len(enc.encode(open("calls.json").read()))
compact_tokens = len(enc.encode(open("calls.compact").read()))
print(json_tokens / compact_tokens)
```


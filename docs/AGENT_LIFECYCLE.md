# Agent lifecycle integration

The resident daemon can push a bounded, factual code-context pack into an agent
before the model handles a prompt. This complements MCP: hooks provide cheap
turn-start orientation, while MCP remains available for deeper pull queries.

## Setup

Initialize the project daemon once, then configure one or all supported agents:

```console
tldr init
tldr setup claude
tldr setup cursor
tldr setup codex
tldr setup all
```

`tldr setup` is idempotent and preserves unrelated configuration. Claude and
Codex receive prompt/session/tool lifecycle hooks plus the `tldr-mcp` server;
Cursor receives the MCP server. `--remove` removes only tldr-owned entries.

The installed hook invokes the hidden, fail-open bridge:

```console
tldr hook --project .
```

It reads the host hook event from stdin, allows at most 350 ms for daemon IPC,
prints only hook JSON on stdout, and emits `{}` if the daemon is unavailable,
unindexed, slow, or returns invalid data. A hook cannot prevent a prompt.

## Delivery and continuity

- `UserPromptSubmit` ranks resident definitions and call edges against the
  prompt, current-session hot files/symbols, and prior project hot files.
- `PostToolUse` records Read/Edit/Write file activity without injecting text.
- Daemon filesystem events also increase active-session file weights.
- `SessionStart` injects a project orientation pack. A `source=compact` event
  uses the same packer with the conversation hot set, restoring context after
  compaction.
- The bounded ledger is atomically persisted in
  `.tldr/session-context.json`. At most 32 recent sessions and 64 hot items per
  session are retained. It contains code-context continuity, not task or
  decision-tracker memory.

Generated context is capped by the configured token budget and by the host-safe
9,500-character ceiling. It contains tagged signatures and graph relationships,
not generated claims.

## Usage and cost

```console
tldr session stats
tldr session stats --session <host-session-id>
```

The same data is exposed as the MCP tool `tldr_session_stats`. TLDR measures its
own injected context tokens locally. Provider input/output tokens and cost are
reported only when the host hook payload supplies usage fields; zero therefore
means “not reported” as well as “none.”

## Supported event schema

The bridge consumes the common Claude/Codex hook fields `session_id`,
`hook_event_name`, `cwd`, `prompt`, `source`, and `tool_input`. It recognizes
file paths at `tool_input.file_path`, `tool_input.path`, and
`tool_input.notebook_path`, and optional usage at `usage.input_tokens`,
`usage.output_tokens`, and `usage.cost_usd` (top-level equivalents are also
accepted).

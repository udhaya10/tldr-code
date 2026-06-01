# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:7510c1e2 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.

## Session Completion

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd dolt push
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds
<!-- END BEADS INTEGRATION -->


## Build & Test

```bash
cargo build                          # Default build (no semantic search)
cargo build --features semantic      # Build with embedding/semantic search
cargo test -p tldr-core              # Core tests
cargo test -p tldr-cli               # CLI tests
cargo test --features semantic       # Full test suite including semantic
```

## Optional: Diagnostic Tools

Some commands (`doctor`, `diagnostics`) require external language tools.
After installing tldr, run `tldr doctor` to check which tools are available,
then install for the languages you work with:

```bash
tldr doctor                    # Check current tool availability
tldr doctor --install rust     # Install Rust diagnostic tools (rustc, cargo)
tldr doctor --install python   # Install Python tools (pyright, ruff, mypy)
tldr doctor --install typescript  # Install TS tools (typescript-language-server, tsc)
tldr doctor --install go       # Install Go tools (gopls, golangci-lint)
tldr doctor --install java     # Install Java tools (checkstyle, spotbugs)
```

## Architecture Overview

Cargo workspace with four crates:
- `tldr-core` — core analysis engine (AST, call graph, CFG, DFG, search, semantic)
- `tldr-cli` — CLI binary (clap-based, ~50 subcommands)
- `tldr-daemon` — background daemon (axum HTTP over Unix/TCP sockets)
- `tldr-mcp` — MCP server (JSON-RPC 2.0 over stdio)

Semantic search (`tldr-core/src/semantic/`) is behind `--features semantic` (pulls fastembed + ONNX Runtime).

## Conventions & Patterns

- Tree-sitter grammars are pinned to exact versions (`=X.Y.Z`)
- Embedding model enum: `EmbeddingModel` in `tldr-core/src/semantic/types.rs`
- Output formats: json (default), text, sarif, dot (command-specific)
- `#[cfg(feature = "semantic")]` gates all embedding code

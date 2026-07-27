# Grammar Compatibility and Upgrade Policy

tldr pins tree-sitter core and every grammar exactly. Grammar upgrades are
behavioral changes: a compatible ABI does not guarantee stable node names,
definition extraction, or recovery behavior.

## Current frontier matrix

| Language | Package pin | Bundled frontier |
|---|---:|---|
| Python | `tree-sitter-python=0.23.6` | PEP 695 aliases/generics, match, PEP 750 t-strings |
| TypeScript | `tree-sitter-typescript=0.23.2` | `satisfies` operator |
| Rust | `tree-sitter-rust=0.23.3` | let-else |

The workspace pins tree-sitter core at `0.25.0`. All other exact grammar pins
remain authoritative in the workspace `Cargo.toml`; none may use a caret or
tilde requirement.

Run the bundled probes with:

```bash
tldr doctor --grammar-frontier python -f json
tldr doctor --grammar-frontier typescript -f json
tldr doctor --grammar-frontier rust -f json
```

Each feature reports:

- `pass`: every expected definition was extracted and the tree contains no
  ERROR or missing recovery nodes.
- `recovered`: every expected definition was extracted, but tree-sitter used
  recovery nodes. The output is usable but the grammar does not fully support
  the syntax.
- `fail`: one or more expected definitions were lost. Doctor exits non-zero.

Python PEP 750 t-strings are intentionally `recovered` on the current 0.23.6
pin. A Python grammar upgrade containing native template-string support should
change this row to `pass`.

## Native-AST differential

The Python extraction gate uses Stock-Monitor-Django at commit
`1e56243d5258e0b310a45ca3cfae60c455328b76`. It compares every tldr-enumerated
Python file against CPython 3.14 `ast`, counting functions, async functions,
methods, and classes per file:

```bash
python3 scripts/grammar_differential.py /path/to/Stock-Monitor-Django \
  --tldr target/debug/tldr \
  --expected-commit 1e56243d5258e0b310a45ca3cfae60c455328b76
```

The recorded baseline is 217 files and zero mismatches.

## Required upgrade procedure

1. Change only the intended exact grammar pin and its lockfile entries.
2. Update the package version reported by the matching doctor frontier.
3. Run all three bundled frontiers. No `fail` verdict is acceptable; every new
   `recovered` verdict requires an explicit issue and release note.
4. Run the pinned Python differential and require zero mismatches.
5. Run smoke, certification, workspace tests, Clippy with warnings denied, and
   formatting checks.
6. Record before/after frontier JSON in the grammar-upgrade issue.
7. If any definition disappears or an existing `pass` becomes `recovered`,
   revert the grammar and lockfile together.

The `Grammar assurance` GitHub Actions workflow enforces the bundled frontiers
and pinned Python differential whenever grammar pins, parser/extractor code, or
the harness changes.

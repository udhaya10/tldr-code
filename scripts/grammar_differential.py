#!/usr/bin/env python3
"""Compare tldr Python extraction counts with CPython's native AST."""

from __future__ import annotations

import argparse
import ast
import json
import subprocess
import sys
from pathlib import Path


def command(*args: str, cwd: Path | None = None) -> str:
    result = subprocess.run(
        args,
        cwd=cwd,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode:
        raise RuntimeError(
            f"command failed ({result.returncode}): {' '.join(args)}\n{result.stderr}"
        )
    return result.stdout


def native_counts(path: Path) -> tuple[int, int]:
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    functions = sum(
        isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
        for node in ast.walk(tree)
    )
    classes = sum(isinstance(node, ast.ClassDef) for node in ast.walk(tree))
    return functions, classes


def tldr_counts(file_result: dict[str, object]) -> tuple[int, int]:
    definitions = file_result.get("definitions", [])
    if not isinstance(definitions, list):
        raise TypeError("tldr file result has no definitions list")
    functions = sum(
        definition.get("kind") in {"function", "method"}
        for definition in definitions
        if isinstance(definition, dict)
    )
    classes = sum(
        definition.get("kind") in {"class", "struct"}
        for definition in definitions
        if isinstance(definition, dict)
    )
    return functions, classes


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("corpus", type=Path)
    parser.add_argument("--tldr", type=Path, required=True)
    parser.add_argument("--expected-commit", required=True)
    args = parser.parse_args()

    corpus = args.corpus.resolve()
    actual_commit = command("git", "rev-parse", "HEAD", cwd=corpus).strip()
    if actual_commit != args.expected_commit:
        raise RuntimeError(
            f"corpus commit mismatch: expected {args.expected_commit}, got {actual_commit}"
        )

    raw = command(
        str(args.tldr.resolve()),
        "structure",
        str(corpus),
        "--lang",
        "python",
        "--oneshot",
        "-f",
        "json",
        "-q",
    )
    report = json.loads(raw)
    files = report.get("files", [])
    extracted_by_path = {
        Path(file_result["path"]): file_result for file_result in files
    }
    native_files = {
        path.relative_to(corpus)
        for path in corpus.rglob("*.py")
        if ".git" not in path.relative_to(corpus).parts
    }
    mismatches: list[str] = []
    for relative in sorted(native_files - extracted_by_path.keys()):
        mismatches.append(f"{relative}: present in corpus but missing from tldr output")
    for relative in sorted(extracted_by_path.keys() - native_files):
        mismatches.append(f"{relative}: present in tldr output but missing from corpus")
    for relative in sorted(native_files & extracted_by_path.keys()):
        native = native_counts(corpus / relative)
        extracted = tldr_counts(extracted_by_path[relative])
        if native != extracted:
            mismatches.append(
                f"{relative}: native functions/classes={native}, "
                f"tldr functions/classes={extracted}"
            )

    summary = {
        "commit": actual_commit,
        "files": len(files),
        "mismatches": len(mismatches),
    }
    print(json.dumps(summary, sort_keys=True))
    if mismatches:
        print("\n".join(mismatches), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Assert tldr's pinned Stock-Monitor-Django compatibility contract."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from collections import Counter
from pathlib import Path
from typing import Any

PINNED_COMMIT = "1e56243d5258e0b310a45ca3cfae60c455328b76"


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


def tldr_json(tldr: Path, *args: str) -> dict[str, Any]:
    raw = command(
        str(tldr),
        *args,
        "--oneshot",
        "--format",
        "json",
        "--quiet",
    )
    value = json.loads(raw)
    if not isinstance(value, dict):
        raise TypeError(f"expected object from {' '.join(args)}")
    return value


def relative(corpus: Path, value: object) -> str:
    path = Path(str(value))
    try:
        return path.resolve().relative_to(corpus).as_posix()
    except ValueError:
        return path.as_posix()


def reference_contract(
    failures: list[str], corpus: Path, tldr: Path
) -> dict[str, int]:
    report = tldr_json(
        tldr,
        "references",
        "upsert_ohlcv_bars",
        str(corpus),
        "--lang",
        "python",
        "--limit",
        "1000",
    )
    references = report.get("references", [])
    kinds = Counter(reference.get("kind") for reference in references)
    expected_kinds = {
        "definition": 1,
        "call": 6,
        "import": 2,
        "other": 1,
    }
    if len(references) != 10 or dict(kinds) != expected_kinds:
        failures.append(
            "upsert_ohlcv_bars reference kinds changed: "
            f"total={len(references)} kinds={dict(kinds)}"
        )

    expected_sites = {
        ("stocks/services/ohlcv_service.py", 92, "definition"),
        ("stocks/services/dhan_client.py", 8, "other"),
        ("stocks/services/dhan_client.py", 19, "import"),
        ("stocks/services/dhan_client.py", 92, "call"),
        ("stocks/tests/test_market_data.py", 40, "import"),
        ("stocks/tests/test_market_data.py", 336, "call"),
        ("stocks/tests/test_market_data.py", 354, "call"),
        ("stocks/tests/test_market_data.py", 361, "call"),
        ("stocks/tests/test_market_data.py", 373, "call"),
        ("stocks/tests/test_market_data.py", 387, "call"),
    }
    actual_sites = {
        (
            relative(corpus, reference.get("file")),
            reference.get("line"),
            reference.get("kind"),
        )
        for reference in references
    }
    if actual_sites != expected_sites:
        failures.append(
            "upsert_ohlcv_bars reference sites changed: "
            f"missing={sorted(expected_sites - actual_sites)} "
            f"extra={sorted(actual_sites - expected_sites)}"
        )

    symbol_map = tldr_json(
        tldr,
        "references",
        "SymbolMap",
        str(corpus),
        "--lang",
        "python",
        "--limit",
        "1000",
    )
    symbol_refs = symbol_map.get("references", [])
    if len(symbol_refs) != 106:
        failures.append(f"SymbolMap reference count changed: {len(symbol_refs)} != 106")
    required_reads = {
        ("stocks/tasks/dhan.py", 126, "read"),
        ("stocks/tasks/dhan.py", 424, "read"),
    }
    actual_symbol_sites = {
        (
            relative(corpus, reference.get("file")),
            reference.get("line"),
            reference.get("kind"),
        )
        for reference in symbol_refs
    }
    if not required_reads.issubset(actual_symbol_sites):
        failures.append(
            "SymbolMap ORM-chain reads missing: "
            f"{sorted(required_reads - actual_symbol_sites)}"
        )

    formatter = tldr_json(
        tldr,
        "references",
        "build_console_formatter",
        str(corpus),
        "--lang",
        "python",
        "--limit",
        "1000",
        "--kinds",
        "string-ref",
    )
    formatter_refs = formatter.get("references", [])
    formatter_sites = {
        (
            relative(corpus, reference.get("file")),
            reference.get("line"),
            reference.get("kind"),
        )
        for reference in formatter_refs
    }
    expected_formatter = {("config/settings.py", 245, "string-ref")}
    if formatter_sites != expected_formatter:
        failures.append(
            f"Django dotted-string reference changed: {sorted(formatter_sites)}"
        )

    return {
        "upsert_references": len(references),
        "symbol_map_references": len(symbol_refs),
        "formatter_string_references": len(formatter_refs),
    }


def candidate_names(report: dict[str, Any]) -> set[str]:
    return {
        str(candidate.get("name"))
        for key in ("dead_functions", "possibly_dead")
        for candidate in report.get(key, [])
    }


def dead_contract(
    failures: list[str], corpus: Path, tldr: Path
) -> dict[str, int]:
    base_args = (
        "dead",
        str(corpus),
        "--lang",
        "python",
        "--max-items",
        "1000",
    )
    baseline = tldr_json(tldr, *base_args)
    django = tldr_json(tldr, *base_args, "--entry-points", "django")
    baseline_names = candidate_names(baseline)
    django_names = candidate_names(django)

    required_dead = {
        "_calculate_q4fy26_earnings_metric",
        "NSEProvider.fetch_corporate_results",
        "NSEProvider.fetch_earnings_calendar",
    }
    missing_dead = required_dead - baseline_names
    if missing_dead:
        failures.append(f"known dead candidates disappeared: {sorted(missing_dead)}")

    never_dead = {
        "build_console_formatter",
        "Command.add_arguments",
        "Command.handle",
        "AppConfig.ready",
        "_live_intraday_sweep_impl",
        "_eod_pipeline_impl",
    }
    unexpected = never_dead & django_names
    if unexpected:
        failures.append(f"framework-wired functions reported dead: {sorted(unexpected)}")

    permission = "LegacyApiPermission.has_permission"
    if permission not in baseline_names:
        failures.append(f"corpus no longer exercises Django preset via {permission}")
    if permission in django_names:
        failures.append(f"Django preset failed to suppress {permission}")

    return {
        "baseline_candidates": len(baseline_names),
        "django_candidates": len(django_names),
    }


def call_and_impact_contract(
    failures: list[str], corpus: Path, tldr: Path
) -> dict[str, int]:
    calls = tldr_json(
        tldr,
        "calls",
        str(corpus),
        "--lang",
        "python",
        "--max-items",
        "100000",
    )
    edges = {
        (
            edge.get("src_file"),
            edge.get("src_func"),
            edge.get("dst_file"),
            edge.get("dst_func"),
            edge.get("call_type"),
        )
        for edge in calls.get("edges", [])
    }
    required_edges = {
        (
            "stocks/services/live_collector.py",
            "sweep_intraday_bars",
            "stocks/services/providers/registry.py",
            "get_provider",
            "local-import",
        ),
        (
            "stocks/services/live_collector.py",
            "sweep_intraday_bars",
            "stocks/symbols.py",
            "Symbol.parse",
            "local-import",
        ),
        (
            "stocks/services/stock_onboarding.py",
            "_run_full_history_backfill",
            "stocks/services/dhan_client.py",
            "fetch_and_store_historical",
            "local-import",
        ),
    }
    missing_edges = required_edges - edges
    if missing_edges:
        failures.append(f"function-local import edges missing: {sorted(missing_edges)}")

    impact = tldr_json(
        tldr,
        "impact",
        "upsert_ohlcv_bars",
        str(corpus),
        "--lang",
        "python",
        "--depth",
        "2",
    )
    target_values = list(impact.get("targets", {}).values())
    if not target_values:
        failures.append("impact returned no upsert_ohlcv_bars target")
        return {"call_edges": len(edges), "impact_level_two_callers": 0}

    fetch = next(
        (
            caller
            for caller in target_values[0].get("callers", [])
            if caller.get("function") == "fetch_and_store_historical"
        ),
        None,
    )
    if fetch is None:
        failures.append("impact depth 1 missing fetch_and_store_historical")
        level_two: set[str] = set()
    else:
        level_two = {
            str(caller.get("function")) for caller in fetch.get("callers", [])
        }
        required_level_two = {
            "_run_full_history_backfill",
            "_fetch_eod_all_active_impl",
            "_backfill_stock_impl",
        }
        if not required_level_two.issubset(level_two):
            failures.append(
                "impact depth 2 callers missing: "
                f"{sorted(required_level_two - level_two)}"
            )

    return {
        "call_edges": len(edges),
        "impact_level_two_callers": len(level_two),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("corpus", type=Path)
    parser.add_argument("--tldr", type=Path, required=True)
    parser.add_argument("--expected-commit", default=PINNED_COMMIT)
    args = parser.parse_args()

    corpus = args.corpus.resolve()
    tldr = args.tldr.resolve()
    actual_commit = command("git", "rev-parse", "HEAD", cwd=corpus).strip()
    if actual_commit != args.expected_commit:
        raise RuntimeError(
            f"corpus commit mismatch: expected {args.expected_commit}, got {actual_commit}"
        )

    failures: list[str] = []
    summary: dict[str, object] = {
        "commit": actual_commit,
        **reference_contract(failures, corpus, tldr),
        **dead_contract(failures, corpus, tldr),
        **call_and_impact_contract(failures, corpus, tldr),
    }
    summary["failures"] = len(failures)
    print(json.dumps(summary, sort_keys=True))
    if failures:
        print("\n".join(f"- {failure}" for failure in failures), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

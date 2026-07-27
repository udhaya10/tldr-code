# Django compatibility contract

tldr's Django behavior is gated against
[`udhaya10/Stock-Monitor-Django`](https://github.com/udhaya10/Stock-Monitor-Django)
at commit `1e56243d5258e0b310a45ca3cfae60c455328b76`. The pin is intentional:
line numbers and exact reference counts are part of the golden contract.

The gate protects:

- reference classification for `upsert_ohlcv_bars`;
- Django ORM-chain reads for `SymbolMap`;
- resolved dotted-string configuration references;
- dead-code precision and the opt-in `--entry-points django` preset;
- function-local import edges with `local-import` provenance;
- depth-two Python impact traversal.

Run it locally after checking out the pinned corpus:

```bash
cargo build -p tldr-cli --bin tldr --no-default-features
python3 scripts/django_compat_corpus.py \
  /path/to/Stock-Monitor-Django \
  --tldr target/debug/tldr \
  --expected-commit 1e56243d5258e0b310a45ca3cfae60c455328b76
```

The expected summary at this pin is:

```json
{"baseline_candidates":8,"call_edges":2426,"commit":"1e56243d5258e0b310a45ca3cfae60c455328b76","django_candidates":7,"failures":0,"formatter_string_references":1,"impact_level_two_callers":8,"symbol_map_references":106,"upsert_references":10}
```

Earlier evaluation notes referred to 42 `SymbolMap` references, four test calls
to `upsert_ohlcv_bars`, `_to_float`, and a
`sweep_intraday_bars -> create_dhan_client_from_env` edge. The pinned commit's
current truth is 106 `SymbolMap` references, five test calls, no `_to_float`
definition, and local-import edges from `sweep_intraday_bars` to `get_provider`
and `Symbol.parse`. The stable cross-file local-import example is
`_run_full_history_backfill -> fetch_and_store_historical`.

Update the pin and all goldens together only after manually validating the new
corpus state. A count change by itself is not evidence that behavior improved.

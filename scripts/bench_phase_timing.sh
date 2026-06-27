#!/usr/bin/env bash
# Phase-timing benchmark harness (Gate-1 campaign follow-up, ADR-8 era).
#
# Runs the call-graph build N times on a corpus with TLDR_PHASE_TIMING=1 and
# reports min/median/max for parse, compose, and total, plus the corpus
# counts (files/funcs/edges) so drift is visible.
#
# Usage:   scripts/bench_phase_timing.sh <path> <lang> [n-runs]
# Env:     TLDR_BIN  binary to bench (default: ./target/release/tldr)
#
# Baselines (2026-06-05, post round-5, Apple Silicon — see TLDR-olg):
#   tldr-code rust    compose ~0.56s   total ~3.1s
#   django    python  compose ~23s     (defect: per-call-site scans, unfixed)
#   zod       ts      compose ~4.7s    (same defect)
#   caddy     go      compose ~1.9s    (same defect)
#   junit5    java    compose ~1.1s    (mild)
set -euo pipefail

CORPUS=${1:?usage: bench_phase_timing.sh <path> <lang> [n-runs]}
LANG_ARG=${2:?usage: bench_phase_timing.sh <path> <lang> [n-runs]}
RUNS=${3:-5}
BIN=${TLDR_BIN:-./target/release/tldr}

declare -a PARSE COMPOSE TOTAL
COUNTS=""
for i in $(seq 1 "$RUNS"); do
  LINE=$(TLDR_PHASE_TIMING=1 "$BIN" calls "$CORPUS" -l "$LANG_ARG" \
    --respect-ignore -f json 2>&1 >/dev/null | grep '\[phase-timing\]') || {
    echo "run $i: no phase-timing line (build failed?)" >&2; exit 1; }
  COUNTS=$(sed -E 's/.*(files=[0-9]+ funcs=[0-9]+ edges=[0-9]+).*/\1/' <<<"$LINE")
  PARSE+=("$(sed -E 's/.*parse=([0-9]+)ms.*/\1/' <<<"$LINE")")
  COMPOSE+=("$(sed -E 's/.*compose=([0-9]+)ms.*/\1/' <<<"$LINE")")
  TOTAL+=("$(sed -E 's/.*total=([0-9]+)ms.*/\1/' <<<"$LINE")")
  echo "run $i: $LINE" >&2
done

stats() { # name values...
  local name=$1; shift
  local sorted; sorted=$(printf '%s\n' "$@" | sort -n)
  local n=$#
  local min med max
  min=$(head -1 <<<"$sorted")
  max=$(tail -1 <<<"$sorted")
  med=$(sed -n "$(((n + 1) / 2))p" <<<"$sorted")
  printf '%-8s min=%sms median=%sms max=%sms\n' "$name" "$min" "$med" "$max"
}

echo "== $CORPUS ($LANG_ARG, $RUNS runs) $COUNTS"
stats parse "${PARSE[@]}"
stats compose "${COMPOSE[@]}"
stats total "${TOTAL[@]}"

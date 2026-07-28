#!/usr/bin/env bash
set -u

benchmark_dir="docs/benchmarks/2026-07-29-fully-cold-runtime"
results_dir="$benchmark_dir/query-results"
metrics_file="$benchmark_dir/query-metrics.jsonl"

mkdir -p "$results_dir"
: > "$metrics_file"

run_case() {
  local label="$1"
  local iteration="$2"
  shift 2

  local stdout_file="$results_dir/${label}-${iteration}.stdout"
  local stderr_file="$results_dir/${label}-${iteration}.stderr"
  local started_ms
  local finished_ms
  local status
  local wall_ms
  local stdout_bytes
  local stderr_bytes

  started_ms=$(perl -MTime::HiRes=time -e 'printf "%.0f", time()*1000')
  "$@" >"$stdout_file" 2>"$stderr_file"
  status=$?
  finished_ms=$(perl -MTime::HiRes=time -e 'printf "%.0f", time()*1000')
  wall_ms=$((finished_ms - started_ms))
  stdout_bytes=$(wc -c <"$stdout_file" | tr -d ' ')
  stderr_bytes=$(wc -c <"$stderr_file" | tr -d ' ')

  jq -cn \
    --arg label "$label" \
    --argjson iteration "$iteration" \
    --argjson wall_ms "$wall_ms" \
    --argjson status "$status" \
    --argjson stdout_bytes "$stdout_bytes" \
    --argjson stderr_bytes "$stderr_bytes" \
    --arg stdout_file "$stdout_file" \
    --arg stderr_file "$stderr_file" \
    '{
      label: $label,
      iteration: $iteration,
      wall_ms: $wall_ms,
      status: $status,
      stdout_bytes: $stdout_bytes,
      stderr_bytes: $stderr_bytes,
      stdout_file: $stdout_file,
      stderr_file: $stderr_file
    }' >>"$metrics_file"
}

for iteration in 1 2 3 4 5; do
  run_case tree "$iteration" \
    tldr tree . -f compact
  run_case structure "$iteration" \
    tldr structure . -f compact
  run_case extract_watcher "$iteration" \
    tldr extract crates/tldr-cli/src/commands/daemon/watcher.rs -f compact
  run_case dead "$iteration" \
    tldr dead . -f compact
  run_case search_lexical "$iteration" \
    tldr search "fixed five second watcher batch queue" . --no-callgraph -f compact
  run_case search_enriched "$iteration" \
    tldr search "semantic delta source chunks" . -f compact
  run_case semantic_watcher "$iteration" \
    tldr semantic "where watcher events are collected into fixed batches" . -f compact
  run_case semantic_delta "$iteration" \
    tldr semantic "avoid full corpus work when one source file changes" . -f compact
done

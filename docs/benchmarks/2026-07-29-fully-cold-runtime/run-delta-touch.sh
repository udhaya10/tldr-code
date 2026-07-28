#!/usr/bin/env bash
set -u

benchmark_dir="docs/benchmarks/2026-07-29-fully-cold-runtime"
poll_file="$benchmark_dir/delta-touch-poll.jsonl"
target="crates/tldr-cli/src/commands/embeddings.rs"

tldr daemon status -f json >"$benchmark_dir/delta-touch-before.json"
baseline_generation=$(jq -r '.artifact_store.active_generation' \
  "$benchmark_dir/delta-touch-before.json")
started_ms=$(perl -MTime::HiRes=time -e 'printf "%.0f", time()*1000')
touch "$target"
: >"$poll_file"

seen_busy=0
seen_generation=0
quiet_after_activity=0

for poll_index in $(seq 1 240); do
  now_ms=$(perl -MTime::HiRes=time -e 'printf "%.0f", time()*1000')
  elapsed_ms=$((now_ms - started_ms))
  status_json=$(tldr daemon status -f json)
  busy_count=$(printf '%s\n' "$status_json" | jq '.liveness.busy | length')
  generation=$(printf '%s\n' "$status_json" |
    jq -r '.artifact_store.active_generation')

  printf '%s\n' "$status_json" |
    jq -c \
      --argjson elapsed_ms "$elapsed_ms" \
      --argjson poll_index "$poll_index" \
      '. + {elapsed_ms:$elapsed_ms,poll_index:$poll_index}' >>"$poll_file"

  if [ "$busy_count" -gt 0 ]; then
    seen_busy=1
  fi
  if [ "$generation" -gt "$baseline_generation" ]; then
    seen_generation=1
  fi
  if { [ "$seen_busy" -eq 1 ] || [ "$seen_generation" -eq 1 ]; } &&
    [ "$busy_count" -eq 0 ]; then
    quiet_after_activity=$((quiet_after_activity + 1))
  else
    quiet_after_activity=0
  fi
  if [ "$quiet_after_activity" -ge 3 ]; then
    break
  fi
  sleep 0.2
done

tldr daemon status -f json >"$benchmark_dir/delta-touch-after.json"

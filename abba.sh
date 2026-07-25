#!/usr/bin/env bash
# TLDR-9bxa.1 real-corpus gate: ABBA overhead + RSS-10% agreement.
# Run from the tldr-code repo root. Lines kept short to survive paste.
set -euo pipefail

cargo build --release -p tldr-cli
TLDR="$(pwd)/target/release/tldr"
# Match dirs::cache_dir() so we wipe where the binary actually stores:
# macOS -> ~/Library/Caches, Linux -> $XDG_CACHE_HOME or ~/.cache.
if [ -n "${XDG_CACHE_HOME:-}" ]; then
  CACHE="$XDG_CACHE_HOME/tldr"
elif [ "$(uname)" = "Darwin" ]; then
  CACHE="$HOME/Library/Caches/tldr"
else
  CACHE="$HOME/.cache/tldr"
fi
ROOT="$(pwd)"
OUT=/tmp/9bxa1_abba
mkdir -p "$OUT"

# arctic-xs: ~4x faster than the default, still real-corpus, and a STRICTER
# overhead test (smaller inference time). Override: MODEL="-m arctic-m".
MODEL="${MODEL:--m arctic-xs}"

"$TLDR" daemon stop 2>/dev/null || true

# One fresh cold build. Writes /usr/bin/time 'real' to <tag>.time and the
# command log to <tag>.log; prints real seconds.
cold_run() {
  tag="$1"; shift
  rm -rf "$CACHE/embeddings" "$CACHE/stores"
  /usr/bin/time -p -o "$OUT/$tag.time" \
    "$TLDR" embed "$ROOT" --no-cache $MODEL "$@" \
    > "$OUT/$tag.log" 2>&1
  awk '/^real/{print $2}' "$OUT/$tag.time"
}

echo "ABBA: OFF ON ON OFF (cold builds); MODEL='$MODEL'"
OFF1=$(cold_run off1)
ON1=$(cold_run on1 --metrics "$OUT/on1.metrics.json")
ON2=$(cold_run on2 --metrics "$OUT/on2.metrics.json")
OFF2=$(cold_run off2)
echo "raw seconds: OFF1=$OFF1 ON1=$ON1 ON2=$ON2 OFF2=$OFF2"

python3 - "$OFF1" "$OFF2" "$ON1" "$ON2" "$OUT" <<'PY'
import json, pathlib, sys
off = sorted(float(x) for x in sys.argv[1:3])
on = sorted(float(x) for x in sys.argv[3:5])
out = pathlib.Path(sys.argv[5])
moff = sum(off) / 2
mon = sum(on) / 2
pct = (mon - moff) / moff * 100
v1 = "PASS" if pct < 3 else "FAIL"
print(f"median_OFF={moff:.1f}s median_ON={mon:.1f}s")
print(f"overhead% = {pct:+.2f}% (gate <3%) -> {v1}")
for tag in ("on1", "on2"):
    m = json.load(open(out / f"{tag}.metrics.json"))
    p = m["rss"]["peak_bytes"]
    pp = m["rss"]["process_peak_bytes"]
    diff = abs(p - pp) * 100 / pp
    v2 = "PASS" if diff < 10 else "FAIL"
    print(f"{tag}: peak={p/1e9:.2f}GiB proc={pp/1e9:.2f}GiB")
    print(f"  diff={diff:.2f}% (gate <10%) -> {v2}")
PY

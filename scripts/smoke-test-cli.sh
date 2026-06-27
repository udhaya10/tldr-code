#!/usr/bin/env bash
# smoke-test-cli.sh — exercise every tldr CLI surface against a generated
# fixture project and produce a classified report.
#
# Outcome classes (per-command expectation table, not a bare exit-code loop):
#   PASS    exit 0 + valid JSON output; scanners (expect=findings) also PASS on
#           a nonzero grep-style findings exit (diagnostics=1, resources=3, ...)
#           as long as stdout is valid JSON
#   WARN    exit 0 but stdout is not valid JSON (surface works, output suspect)
#   HONEST  recognized not-ready / parked message (correct two-modes behavior:
#           "works beautifully or says why" — NOT a failure)
#   SKIP    not run (heavy/destructive without opt-in flag, or --filter miss)
#   FAIL    panic, timeout, unrecognized error, or unexpected exit code
#
# Usage:
#   scripts/smoke-test-cli.sh [options]
#     --include-heavy       also run `embed` / `warm` (cold embed can take ~hours)
#     --include-lifecycle   also run daemon start/stop + cache clear
#                           (DISRUPTIVE: restarts your live daemon, clears caches)
#     --filter <regex>      only run cases whose name matches <regex>
#     --out <dir>           report directory (default: ./tldr-smoke-report)
#     --bin <path>          tldr binary to test (default: tldr on PATH)
#     --timeout <secs>      per-command timeout (default: 60)
#     --keep-fixture        do not delete the generated fixture dir
#     -h | --help           this text
#
# Exit code: 0 if no FAILs, 1 otherwise.
#
# Compatible with macOS stock bash 3.2 (no associative arrays, no mapfile).

set -o pipefail

# ---------------------------------------------------------------- options ---
TLDR_BIN="tldr"
OUT_DIR="./tldr-smoke-report"
TIMEOUT_SECS=60
INCLUDE_HEAVY=0
INCLUDE_LIFECYCLE=0
FILTER=""
KEEP_FIXTURE=0

while [ $# -gt 0 ]; do
  case "$1" in
    --include-heavy)     INCLUDE_HEAVY=1 ;;
    --include-lifecycle) INCLUDE_LIFECYCLE=1 ;;
    --filter)            FILTER="$2"; shift ;;
    --out)               OUT_DIR="$2"; shift ;;
    --bin)               TLDR_BIN="$2"; shift ;;
    --timeout)           TIMEOUT_SECS="$2"; shift ;;
    --keep-fixture)      KEEP_FIXTURE=1 ;;
    -h|--help)           sed -n '2,28p' "$0"; exit 0 ;;
    *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
  esac
  shift
done

command -v "$TLDR_BIN" >/dev/null 2>&1 || { echo "error: '$TLDR_BIN' not found on PATH" >&2; exit 2; }

# ---------------------------------------------------------------- helpers ---
# Per-command timeout: GNU timeout / gtimeout if present, else perl alarm.
TIMEOUT_BIN=""
command -v timeout  >/dev/null 2>&1 && TIMEOUT_BIN="timeout"
[ -z "$TIMEOUT_BIN" ] && command -v gtimeout >/dev/null 2>&1 && TIMEOUT_BIN="gtimeout"

run_with_timeout() { # secs cmd args...
  local secs="$1"; shift
  if [ -n "$TIMEOUT_BIN" ]; then
    "$TIMEOUT_BIN" "$secs" "$@"
  else
    perl -e '
      my $t = shift @ARGV;
      my $pid = fork;
      if (!$pid) { exec @ARGV or exit 127; }
      $SIG{ALRM} = sub { kill "KILL", $pid; waitpid $pid, 0; exit 124; };
      alarm $t;
      waitpid $pid, 0;
      exit(($? & 127) ? 128 + ($? & 127) : $? >> 8);
    ' "$secs" "$@"
  fi
}

json_valid() { # file -> 0 if valid JSON
  if command -v jq >/dev/null 2>&1; then
    jq -e . "$1" >/dev/null 2>&1
  else
    python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$1" >/dev/null 2>&1
  fi
}

# Recognized "honest" messages: two-modes contract (Phase 1) + environment gaps.
HONEST_RE='daemon not started|index not built|index build in progress|not available in this version|daemon is not running|no daemon|not running|No supported diagnostic tools|no diagnostic tools|semantic feature|not compiled'

is_honest() { grep -Eiq "$HONEST_RE" "$1" 2>/dev/null; }

# tty colors
if [ -t 1 ]; then
  C_G=$'\033[32m'; C_R=$'\033[31m'; C_Y=$'\033[33m'; C_B=$'\033[34m'; C_D=$'\033[2m'; C_0=$'\033[0m'
else
  C_G=""; C_R=""; C_Y=""; C_B=""; C_D=""; C_0=""
fi

# ---------------------------------------------------------------- fixture ---
FIX="$(mktemp -d "${TMPDIR:-/tmp}/tldr-smoke.XXXXXX")"
cleanup() { [ "$KEEP_FIXTURE" -eq 1 ] || rm -rf "$FIX"; }
trap cleanup EXIT

cat > "$FIX/app.py" <<'EOF'
"""Fixture module: deliberate patterns for smoke-testing tldr surfaces."""
import os
import subprocess


def add(a, b):
    """Add two numbers."""
    result = a + b
    return result


def greet(name):
    """Greet a user."""
    message = "Hello, " + name
    unused = name.upper()  # dead store on purpose
    return message


def run_user_command(cmd):
    """Deliberate taint sink: user input reaches a shell."""
    subprocess.run(cmd, shell=True)


def read_config(path):
    """Deliberate resource leak: file opened, never closed."""
    f = open(path)
    data = f.read()
    return data


def orchestrate(name):
    """Entry point calling the others."""
    g = greet(name)
    s = add(1, 2)
    run_user_command("echo " + name)
    c = read_config(os.devnull)
    return g, s, c


def never_called():
    """Deliberate dead code."""
    return 42


class Shape:
    def __init__(self, name):
        self.name = name

    def area(self):
        return 0


class Circle(Shape):
    def __init__(self, r):
        super().__init__("circle")
        self.r = r

    def area(self):
        return 3.14159 * self.r * self.r
EOF

cat > "$FIX/util.py" <<'EOF'
"""Second module so importers/deps/coupling have an edge to find."""
import app


def double(x):
    return app.add(x, x)
EOF

# Slightly modified copy of app.py for the structural diff surface.
sed 's/Hello, /Hi, /' "$FIX/app.py" > "$FIX/app_v2.py"

cat > "$FIX/test_app.py" <<'EOF'
import app


def test_add():
    assert app.add(1, 2) == 3


def test_greet():
    assert app.greet("bob") == "Hello, bob"
EOF

cat > "$FIX/cov.lcov" <<'EOF'
TN:
SF:app.py
DA:6,1
DA:8,1
DA:9,1
DA:40,0
LF:4
LH:3
end_of_record
EOF

# Git history: churn/hotspots/change-impact/bugbot are git-dependent.
(
  cd "$FIX"
  git init -q
  git -c user.name=smoke -c user.email=smoke@test commit -q --allow-empty -m init 2>/dev/null || true
  git add -A
  git -c user.name=smoke -c user.email=smoke@test commit -qm "add fixture"
  printf '\n# churn marker\n' >> app.py
  git add app.py
  git -c user.name=smoke -c user.email=smoke@test commit -qm "touch app.py"
  # leave an uncommitted edit so bugbot/change-impact have a working-tree delta
  printf '\n# uncommitted marker\n' >> util.py
)

# Line coordinates computed from the file (never hardcode — they drift).
SLICE_LINE=$(grep -n 'return message' "$FIX/app.py" | head -1 | cut -d: -f1)
CHOP_SRC=$(grep -n 'message = ' "$FIX/app.py" | head -1 | cut -d: -f1)
CHOP_TGT="$SLICE_LINE"

# ----------------------------------------------------------------- runner ---
mkdir -p "$OUT_DIR/logs"
RESULTS_TSV="$OUT_DIR/results.tsv"
REPORT_MD="$OUT_DIR/report.md"
printf 'case\tstatus\texit\tsecs\tnote\tcommand\n' > "$RESULTS_TSV"

N_PASS=0; N_WARN=0; N_HONEST=0; N_SKIP=0; N_FAIL=0; N_TOTAL=0
FAILED_CASES=""

record() { # name status rc secs note cmdline
  N_TOTAL=$((N_TOTAL + 1))
  case "$2" in
    PASS)   N_PASS=$((N_PASS + 1));     local c="$C_G" ;;
    WARN)   N_WARN=$((N_WARN + 1));     local c="$C_Y" ;;
    HONEST) N_HONEST=$((N_HONEST + 1)); local c="$C_B" ;;
    SKIP)   N_SKIP=$((N_SKIP + 1));     local c="$C_D" ;;
    *)      N_FAIL=$((N_FAIL + 1));     local c="$C_R"; FAILED_CASES="$FAILED_CASES $1" ;;
  esac
  printf '%s%-8s%s %-28s %3ss  exit=%-3s %s\n' "$c" "$2" "$C_0" "$1" "$4" "$3" "$5"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" "$5" "$6" >> "$RESULTS_TSV"
}

run_case() { # name expect timeout_secs -- tldr-args...
  local name="$1" expect="$2" tmo="$3"; shift 3
  [ "$1" = "--" ] && shift
  local cmdline="$TLDR_BIN $*"

  if [ -n "$FILTER" ] && ! printf '%s' "$name" | grep -Eq "$FILTER"; then
    return 0  # filtered out: not reported
  fi
  if [ "${expect#skip:}" != "$expect" ]; then
    record "$name" SKIP "-" "-" "${expect#skip:}" "$cmdline"
    return 0
  fi

  local safe; safe=$(printf '%s' "$name" | tr ' /' '__')
  local out="$OUT_DIR/logs/$safe.out" err="$OUT_DIR/logs/$safe.err"
  local t0 t1 rc secs
  t0=$(date +%s)
  run_with_timeout "$tmo" "$TLDR_BIN" "$@" >"$out" 2>"$err"
  rc=$?
  t1=$(date +%s); secs=$((t1 - t0))

  # combined output for message matching
  local both="$OUT_DIR/logs/$safe.all"
  cat "$out" "$err" > "$both" 2>/dev/null

  if [ "$rc" -eq 124 ] || [ "$rc" -eq 137 ] || [ "$rc" -eq 142 ]; then
    record "$name" FAIL "$rc" "$secs" "TIMEOUT after ${tmo}s" "$cmdline"
    return 0
  fi
  if grep -q 'panicked at' "$err" 2>/dev/null; then
    record "$name" FAIL "$rc" "$secs" "PANIC (see logs/$safe.err)" "$cmdline"
    return 0
  fi

  if [ "$rc" -eq 0 ]; then
    if [ -s "$out" ] && json_valid "$out"; then
      record "$name" PASS "$rc" "$secs" "ok" "$cmdline"
    elif [ -s "$out" ]; then
      record "$name" WARN "$rc" "$secs" "exit 0 but stdout is not valid JSON" "$cmdline"
    else
      record "$name" WARN "$rc" "$secs" "exit 0 with empty stdout" "$cmdline"
    fi
    return 0
  fi

  # non-zero exit
  # Scanners use grep-style findings exit codes (e.g. diagnostics=1,
  # resources=3) — nonzero + valid JSON findings is correct behavior.
  if [ "$expect" = "findings" ] && [ -s "$out" ] && json_valid "$out"; then
    record "$name" PASS "$rc" "$secs" "ok (findings exit code)" "$cmdline"
    return 0
  fi
  if { [ "$expect" = "okor" ] || [ "$expect" = "findings" ]; } && is_honest "$both"; then
    local msg; msg=$(grep -Eio "$HONEST_RE" "$both" | head -1)
    record "$name" HONEST "$rc" "$secs" "honest: \"$msg\"" "$cmdline"
  else
    local first; first=$(head -c 120 "$err" | tr '\n\t' '  ')
    [ -z "$first" ] && first=$(head -c 120 "$out" | tr '\n\t' '  ')
    record "$name" FAIL "$rc" "$secs" "${first:-nonzero exit, no output}" "$cmdline"
  fi
}

# gate helpers: emit either the real expectation or a skip reason
heavy()     { [ "$INCLUDE_HEAVY" -eq 1 ]     && echo "$1" || echo "skip:heavy (use --include-heavy)"; }
lifecycle() { [ "$INCLUDE_LIFECYCLE" -eq 1 ] && echo "$1" || echo "skip:disruptive (use --include-lifecycle)"; }

# ------------------------------------------------------------------ cases ---
echo "${C_D}tldr binary : $(command -v "$TLDR_BIN") ($("$TLDR_BIN" --version 2>/dev/null | head -1))${C_0}"
echo "${C_D}fixture     : $FIX${C_0}"
echo "${C_D}report dir  : $OUT_DIR${C_0}"
echo

T="$TIMEOUT_SECS"

# --- L1 AST / structure
run_case "tree"              ok   "$T" -- -q tree "$FIX"
run_case "structure"         ok   "$T" -- -q structure "$FIX"
run_case "extract"           ok   "$T" -- -q extract "$FIX/app.py"
run_case "imports"           ok   "$T" -- -q imports "$FIX/app.py"
run_case "importers"         ok   "$T" -- -q importers app "$FIX"
run_case "loc"               ok   "$T" -- -q loc "$FIX"

# --- L2 call graph
run_case "calls"             ok   "$T" -- -q calls "$FIX"
run_case "impact"            ok   "$T" -- -q impact add "$FIX"
run_case "dead"              findings "$T" -- -q dead "$FIX"
run_case "whatbreaks"        ok   "$T" -- -q whatbreaks add "$FIX"
run_case "references"        ok   "$T" -- -q references add "$FIX"
run_case "hubs"              ok   "$T" -- -q hubs "$FIX"
run_case "definition"        ok   "$T" -- -q definition --symbol add --file "$FIX/app.py"

# --- L3/L4/L5 dataflow
run_case "reaching-defs"     ok   "$T" -- -q reaching-defs "$FIX/app.py" greet
run_case "available"         ok   "$T" -- -q available "$FIX/app.py" greet
run_case "dead-stores"       ok   "$T" -- -q dead-stores "$FIX/app.py" greet
run_case "slice"             ok   "$T" -- -q slice "$FIX/app.py" greet "$SLICE_LINE"
run_case "chop"              ok   "$T" -- -q chop "$FIX/app.py" greet "$CHOP_SRC" "$CHOP_TGT"

# --- search / context
run_case "search"            ok   "$T" -- -q search greet "$FIX"
run_case "context"           ok   "$T" -- -q context orchestrate "$FIX"

# --- quality / metrics
run_case "smells"            findings "$T" -- -q smells "$FIX"
run_case "complexity"        ok   "$T" -- -q complexity "$FIX/app.py" greet
run_case "cognitive"         ok   "$T" -- -q cognitive "$FIX"
run_case "halstead"          ok   "$T" -- -q halstead "$FIX"
run_case "debt"              ok   "$T" -- -q debt "$FIX"
run_case "health"            ok   "$T" -- -q health "$FIX"
run_case "cohesion"          ok   "$T" -- -q cohesion "$FIX"
run_case "todo"              ok   "$T" -- -q todo "$FIX"
run_case "verify"            ok   "$T" -- -q verify "$FIX"

# --- git-dependent
run_case "churn"             ok   "$T" -- -q churn "$FIX"
run_case "hotspots"          ok   "$T" -- -q hotspots "$FIX"
run_case "change-impact"     ok   "$T" -- -q change-impact "$FIX"
run_case "bugbot check"      findings "$T" -- -q bugbot check "$FIX"

# --- architecture / API
run_case "deps"              ok   "$T" -- -q deps "$FIX"
run_case "coupling"          ok   "$T" -- -q coupling "$FIX"
run_case "inheritance"       ok   "$T" -- -q inheritance "$FIX"
run_case "patterns"          ok   "$T" -- -q patterns "$FIX"
run_case "interface"         ok   "$T" -- -q interface "$FIX"
run_case "surface"           ok   "$T" -- -q surface "$FIX"
run_case "api-check"         findings "$T" -- -q api-check "$FIX"
run_case "explain"           ok   "$T" -- -q explain "$FIX/app.py" greet

# --- similarity / clones / diff
run_case "clones"            findings "$T" -- -q clones "$FIX"
run_case "dice"              ok   "$T" -- -q dice "$FIX/app.py::add" "$FIX/app.py::greet"
run_case "diff"              ok   "$T" -- -q diff "$FIX/app.py" "$FIX/app_v2.py"

# --- security (planted findings → nonzero findings exit codes are correct)
run_case "taint"             findings "$T" -- -q taint "$FIX/app.py" run_user_command
run_case "vuln"              findings "$T" -- -q vuln "$FIX"
run_case "secure"            findings "$T" -- -q secure "$FIX"
run_case "resources"         findings "$T" -- -q resources "$FIX/app.py" read_config

# --- tests / specs / coverage
run_case "specs"             ok   "$T" -- -q specs --from-tests "$FIX/test_app.py"
run_case "invariants"        okor "$T" -- -q invariants --from-tests "$FIX/test_app.py" "$FIX/app.py"
run_case "contracts"         ok   "$T" -- -q contracts "$FIX/app.py" greet
run_case "temporal"          ok   "$T" -- -q temporal "$FIX"
run_case "coverage"          ok   "$T" -- -q coverage "$FIX/cov.lcov"

# --- diagnostics / fixing (external-tool dependent → okor)
run_case "doctor"            okor "$T" -- -q doctor
run_case "diagnostics"       findings "$T" -- -q diagnostics "$FIX"
run_case "fix diagnose"      okor "$T" -- -q fix diagnose --source "$FIX/app.py" \
                                       --error "NameError: name 'mesage' is not defined"

# --- semantic surfaces (daemon/index dependent → environment-aware okor;
#     HONEST = the Phase-1 two-modes contract working as designed)
run_case "semantic"          okor "$T" -- -q semantic "add two numbers" "$FIX"
run_case "similar"           okor "$T" -- -q similar "$FIX/app.py"
run_case "embed"             "$(heavy ok)" 3600 -- -q embed "$FIX"
run_case "warm"              "$(heavy okor)" 3600 -- -q warm "$FIX"

# --- runtime / daemon (read-only surfaces always; lifecycle gated)
# NB: no -q here — `-q` currently suppresses the entire JSON result for
# `cache stats` / `daemon status|list|notify` (CLI inconsistency; `stats` is fine).
run_case "stats"             ok   "$T" -- -q stats
run_case "cache stats"       ok   "$T" -- cache stats
run_case "daemon status"     okor "$T" -- daemon status
run_case "daemon list"       okor "$T" -- daemon list
run_case "daemon query ping" okor "$T" -- daemon query ping --project "$FIX"
run_case "daemon notify"     okor "$T" -- daemon notify "$FIX/app.py" --project "$FIX"
run_case "daemon start"      "$(lifecycle okor)" "$T" -- daemon start --project "$FIX"
run_case "daemon stop"       "$(lifecycle okor)" "$T" -- daemon stop --project "$FIX"
run_case "cache clear"       "$(lifecycle ok)"   "$T" -- cache clear

# ------------------------------------------------------------------ report --
echo
SUMMARY="total=$N_TOTAL pass=$N_PASS warn=$N_WARN honest=$N_HONEST skip=$N_SKIP fail=$N_FAIL"
echo "${C_G}PASS=$N_PASS${C_0} ${C_Y}WARN=$N_WARN${C_0} ${C_B}HONEST=$N_HONEST${C_0} ${C_D}SKIP=$N_SKIP${C_0} ${C_R}FAIL=$N_FAIL${C_0}  (total $N_TOTAL)"
[ -n "$FAILED_CASES" ] && echo "${C_R}failed:${C_0}$FAILED_CASES"

{
  echo "# tldr CLI smoke report"
  echo
  echo "- date: $(date '+%Y-%m-%d %H:%M:%S')"
  echo "- binary: \`$(command -v "$TLDR_BIN")\` — $("$TLDR_BIN" --version 2>/dev/null | head -1)"
  echo "- fixture: \`$FIX\`$( [ "$KEEP_FIXTURE" -eq 1 ] && echo ' (kept)' || echo ' (deleted)')"
  echo "- summary: **$SUMMARY**"
  echo
  echo "| case | status | exit | secs | note |"
  echo "|---|---|---|---|---|"
  tail -n +2 "$RESULTS_TSV" | awk -F'\t' '{printf "| %s | %s | %s | %s | %s |\n", $1, $2, $3, $4, $5}'
  echo
  echo "Status legend: PASS = exit 0 + valid JSON · WARN = exit 0, non-JSON stdout ·"
  echo "HONEST = recognized not-ready/parked message (two-modes contract working) ·"
  echo "SKIP = gated (heavy/disruptive) · FAIL = panic/timeout/unrecognized error."
  echo
  echo "Per-case stdout/stderr captured under \`logs/\`."
} > "$REPORT_MD"

echo "${C_D}report: $REPORT_MD  ·  raw: $RESULTS_TSV  ·  logs: $OUT_DIR/logs/${C_0}"

[ "$N_FAIL" -eq 0 ]

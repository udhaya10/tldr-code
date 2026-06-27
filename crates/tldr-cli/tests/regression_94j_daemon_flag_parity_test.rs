//! TLDR-94j contract pins: slice direction/--variable and impact parity must
//! survive a running daemon.
//!
//! The daemon's `Slice` arm hardcoded `SliceDirection::Backward` and ignored
//! `--variable`, with a cache key of (file, function, line) only. Empirical
//! finding during the fix (2026-06-06): that wrong answer never actually
//! reached a CLI user — the arm returns a bare `HashSet<u32>` JSON array,
//! which can never deserialize into the client's `LegacySliceOutput` object,
//! so `try_daemon_route` always returned `None` and the CLI silently fell
//! back to the correct local compute (at the cost of wasted daemon-side
//! work and a poisoned-by-design cache entry). The Impact arm DID serve live
//! answers on language-matching (default: Python) projects, and those were
//! weaker: no AST fallback, no `enrich_impact_with_references`.
//!
//! The interim fix (TLDR-94j) removes the daemon route from `slice` and
//! `impact` so they always compute locally; the n74 rebuild may restore
//! routing ONLY with full flag parity.
//!
//! These tests are forward-looking CONTRACT pins (they pass both pre- and
//! post-fix, because the pre-fix wrong answer was masked by the serialization
//! mismatch). What each pin proves (Codex ② R5: scoped per test, not file-wide):
//! - The two SLICE tests seed the daemon cache with the default (backward)
//!   query, then assert a forward / variable-filtered query still gets the
//!   right answer — if anyone rewires slice through a daemon route that drops
//!   flags AND fixes the serialization shape, the seeded backward cache entry
//!   makes them fail loudly.
//! - The IMPACT test is a daemon-up vs daemon-down EQUIVALENCE check only, on
//!   a small two-file corpus; it pins the parity contract but does not
//!   reproduce the historical weaker-answer divergence (which needs a corpus
//!   where the call graph misses what AST fallback finds — n74-era coverage).
//!
//! Isolation (Codex ② R2): every spawned command gets a per-test HOME so the
//! daemon registry (`$HOME/.tldr/registry.json`) never bleeds across tests,
//! and daemon start is followed by a status poll, not a fixed sleep.
//!
//! Spawns a real daemon (writes `~/.tldr/registry.json`), so `#[ignore]` by
//! default — run via:
//! `cargo test -p tldr-cli --test regression_94j_daemon_flag_parity_test -- --ignored`
//! (same gating pattern as daemon_test.rs / daemon_observability_test.rs).

use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tempfile::TempDir;

fn tldr_cmd(home: &Path) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("tldr"));
    // Per-test HOME: isolates ~/.tldr/registry.json from other tests and from
    // the developer's real registry (Codex ② R2).
    cmd.env("HOME", home);
    cmd
}

fn stop_daemon(home: &Path, project: &Path) {
    let _ = tldr_cmd(home)
        .args(["daemon", "stop", "--project"])
        .arg(project)
        .output();
}

fn start_daemon(home: &Path, project: &Path) {
    let start = tldr_cmd(home)
        .args(["daemon", "start", "--project"])
        .arg(project)
        .output()
        .expect("spawn daemon start");
    assert!(
        start.status.success(),
        "daemon start failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&start.stdout),
        String::from_utf8_lossy(&start.stderr),
    );
    // Readiness poll instead of a fixed sleep (Codex ② R2): wait until
    // `daemon status` reports the daemon as reachable, up to 10s.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let out = tldr_cmd(home)
            .args(["daemon", "status", "--project"])
            .arg(project)
            .args(["-f", "json"])
            .output()
            .expect("run daemon status");
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.contains("\"running\"") && !stdout.contains("not_running") {
            return;
        }
        if std::time::Instant::now() > deadline {
            stop_daemon(home, project);
            panic!("daemon did not become ready within 10s; last status: {stdout}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

const SAMPLE: &str = "def compute(a, b):\n    x = a + 1\n    y = b + 2\n    z = x + y\n    print(z)\n    return y\n";

fn write_sample(project: &Path) -> std::path::PathBuf {
    let file = project.join("sample.py");
    std::fs::write(&file, SAMPLE).expect("write sample.py");
    file
}

fn slice_json(home: &Path, file: &Path, extra: &[&str]) -> serde_json::Value {
    let mut cmd = tldr_cmd(home);
    cmd.arg("slice")
        .arg(file)
        .args(["compute", "2", "-f", "json"])
        .args(extra);
    let out = cmd.output().expect("run tldr slice");
    assert!(
        out.status.success(),
        "slice {:?} failed: stderr={:?}",
        extra,
        String::from_utf8_lossy(&out.stderr),
    );
    serde_json::from_slice(&out.stdout).expect("slice output must be JSON")
}

/// Contract: with a daemon up AND a backward result already cached for the
/// same (file, function, line), `slice -d forward` must still return a
/// FORWARD slice. Pre-fix this returned the cached backward slice.
#[test]
#[ignore = "spawns a real daemon and writes to ~/.tldr/registry.json — run manually with `cargo test -- --ignored`"]
fn slice_direction_survives_running_daemon() {
    let home = TempDir::new().expect("home dir");
    let temp = TempDir::new().expect("temp dir");
    let (home, project) = (home.path(), temp.path());
    let file = write_sample(project);

    start_daemon(home, project);
    // Seed: default (backward) query first, so a flag-dropping daemon route
    // would have a poisoned cache entry keyed only by (file, function, line).
    let backward = slice_json(home, &file, &[]);
    let forward = slice_json(home, &file, &["-d", "forward"]);
    stop_daemon(home, project);

    assert_eq!(
        backward["direction"], "backward",
        "default slice must report backward, got: {backward}"
    );
    assert_eq!(
        forward["direction"], "forward",
        "TLDR-94j regression: `-d forward` answered with direction={} — \
         a daemon path is dropping the direction flag",
        forward["direction"]
    );
    // Substantive check, not just the echoed field: forward from `x = a + 1`
    // must reach the lines x flows into (z and print), which the backward
    // slice from the same criterion cannot contain.
    let lines = |v: &serde_json::Value| -> Vec<u64> {
        v["lines"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_u64()).collect())
            .unwrap_or_default()
    };
    let (fwd, bwd) = (lines(&forward), lines(&backward));
    assert!(
        fwd.contains(&4),
        "forward slice from line 2 must include line 4 (z = x + y), got {fwd:?}"
    );
    assert_ne!(
        fwd, bwd,
        "forward and backward slices are identical — daemon likely served one for the other"
    );
}

/// Contract: `--variable` must be honored with a daemon up (pre-fix the
/// daemon arm passed `None` and the output's `variable` field came back null).
#[test]
#[ignore = "spawns a real daemon and writes to ~/.tldr/registry.json — run manually with `cargo test -- --ignored`"]
fn slice_variable_survives_running_daemon() {
    let home = TempDir::new().expect("home dir");
    let temp = TempDir::new().expect("temp dir");
    let (home, project) = (home.path(), temp.path());
    let file = write_sample(project);

    start_daemon(home, project);
    let _seed = slice_json(home, &file, &[]);
    let filtered = slice_json(home, &file, &["--variable", "x"]);
    stop_daemon(home, project);

    assert_eq!(
        filtered["variable"], "x",
        "TLDR-94j regression: `--variable x` came back as {} — \
         a daemon path is dropping the variable flag",
        filtered["variable"]
    );
}

/// Contract: `impact` with a daemon up must equal `impact` with the daemon
/// stopped (pre-fix the daemon arm skipped AST fallback + reference
/// enrichment and resolved language differently — weaker answers).
#[test]
#[ignore = "spawns a real daemon and writes to ~/.tldr/registry.json — run manually with `cargo test -- --ignored`"]
fn impact_daemon_equals_local() {
    let home = TempDir::new().expect("home dir");
    let temp = TempDir::new().expect("temp dir");
    let (home, project) = (home.path(), temp.path());
    write_sample(project);
    let caller = "import sample\n\ndef caller():\n    return sample.compute(1, 2)\n";
    std::fs::write(project.join("caller.py"), caller).expect("write caller.py");
    // Second-level caller so depth/enrichment have something to act on.
    let caller2 = "import caller\n\ndef caller2():\n    return caller.caller()\n";
    std::fs::write(project.join("caller2.py"), caller2).expect("write caller2.py");

    let run_impact = || -> serde_json::Value {
        let out = tldr_cmd(home)
            .args(["impact", "compute"])
            .arg(project)
            .args(["-f", "json"])
            .output()
            .expect("run tldr impact");
        assert!(
            out.status.success(),
            "impact failed: stderr={:?}",
            String::from_utf8_lossy(&out.stderr),
        );
        serde_json::from_slice(&out.stdout).expect("impact output must be JSON")
    };

    start_daemon(home, project);
    let with_daemon = run_impact();
    stop_daemon(home, project);
    let local = run_impact();

    assert_eq!(
        with_daemon, local,
        "TLDR-94j regression: impact answer changes when a daemon is up — \
         daemon arm is serving a weaker/divergent report"
    );
}

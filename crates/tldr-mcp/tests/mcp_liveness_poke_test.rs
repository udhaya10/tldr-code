//! TLDR-axz — MCP liveness parity integration test.
//!
//! `tldr_mcp` is a SEPARATE binary from the CLI, so the CLI's
//! per-invocation liveness poke (TLDR-nke) never fires for MCP traffic.
//! Without its own poke, an agent using only MCP tools reproduces the
//! original TLDR-3w5 bug: the project's daemon idles out underneath a
//! working agent.
//!
//! This test drives a real `tools/call` frame through `process_request`
//! against a registry (redirected via `TLDR_DAEMON_REGISTRY_DIR`) whose
//! entry's project contains the test cwd, with a bound datagram socket
//! standing in for the daemon's poke receiver — and asserts the datagram
//! arrives.

#![cfg(unix)]

use std::os::unix::net::UnixDatagram;
use std::sync::Mutex;

use serde_json::json;
use tldr_mcp::server::process_request;
use tldr_mcp::tools::ToolRegistry;

/// TLDR-rml: `TLDR_DAEMON_REGISTRY_DIR` is process-global. Serialize any test
/// in this binary that mutates it (and any future test that pokes via
/// `process_request`) so they cannot observe each other's registry path,
/// mirroring the `ENV_LOCK` convention in `tldr-core/src/liveness.rs`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn tools_call_pokes_registered_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();

    // Stand-in daemon poke receiver.
    let stream_sock = dir.path().join("tldr-mcees.sock");
    let poke_path = stream_sock.with_extension("poke");
    let rx = UnixDatagram::bind(&poke_path).unwrap();
    rx.set_nonblocking(true).unwrap();

    // Registry entry whose project contains the test cwd.
    let registry_doc = json!({
        "daemons": [
            { "project": cwd, "pid": 1, "socket": stream_sock, "started_at": "t" }
        ]
    });
    std::fs::write(
        dir.path().join("daemon-registry.json"),
        serde_json::to_string(&registry_doc).unwrap(),
    )
    .unwrap();

    // The poke fires BEFORE tool dispatch, so even an unknown tool name
    // exercises it — keeping this test independent of the tool inventory.
    let frame = json!({
        "jsonrpc": "2.0",
        "method": "tools/call",
        "id": 1,
        "params": { "name": "nonexistent_tool_for_poke_test", "arguments": {} }
    })
    .to_string();

    let registry = ToolRegistry::new();
    // Hold the lock across the whole set/read/remove window so a parallel test
    // cannot see (or clobber) this registry path.
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("TLDR_DAEMON_REGISTRY_DIR", dir.path());
    let response = process_request(&frame, &registry);
    std::env::remove_var("TLDR_DAEMON_REGISTRY_DIR");

    assert!(response.is_some(), "tools/call must produce a response");

    let mut buf = [0u8; 8];
    assert!(
        rx.recv_from(&mut buf).is_ok(),
        "tools/call must poke the registered daemon (TLDR-axz)"
    );
}

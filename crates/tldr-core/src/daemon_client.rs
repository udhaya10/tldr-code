//! Read-only client for the authoritative local tldr daemon.
//!
//! This deliberately lives below both `tldr-cli` and `tldr-mcp`, avoiding a
//! dependency cycle while keeping non-CLI consumers off cold query pipelines.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use thiserror::Error;

const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Error)]
/// Failures produced by the shared daemon query client.
pub enum DaemonClientError {
    /// No registry entry covers the requested path.
    #[error("no running daemon is registered for {0}")]
    NotRunning(String),
    /// The registry endpoint does not match the expected project identity.
    #[error("daemon endpoint failed validation: {0}")]
    InvalidEndpoint(String),
    /// The local socket or TCP transport failed.
    #[error("daemon I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// A request or response exceeded the protocol size bound.
    #[error("daemon response exceeded {MAX_MESSAGE_SIZE} bytes")]
    ResponseTooLarge,
    /// The daemon returned invalid JSON.
    #[error("daemon returned malformed JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The daemon is running but its resident generation is cold/building.
    #[error("daemon is not ready: {0}")]
    NotReady(String),
    /// The daemon rejected the typed request.
    #[error("daemon request failed: {0}")]
    Remote(String),
}

/// Send one newline-delimited JSON request through the daemon covering `path`.
pub fn request(path: &Path, command: &Value) -> Result<Value, DaemonClientError> {
    let endpoint = crate::liveness::daemon_endpoint_for(path)
        .ok_or_else(|| DaemonClientError::NotRunning(path.display().to_string()))?;
    request_endpoint(&endpoint, command)
}

fn request_endpoint(
    endpoint: &crate::liveness::DaemonEndpoint,
    command: &Value,
) -> Result<Value, DaemonClientError> {
    let payload = serde_json::to_vec(command)?;
    if payload.len() > MAX_MESSAGE_SIZE {
        return Err(DaemonClientError::ResponseTooLarge);
    }

    #[cfg(unix)]
    let response = {
        validate_unix_endpoint(&endpoint.project, &endpoint.socket)?;
        let mut stream = std::os::unix::net::UnixStream::connect(&endpoint.socket)?;
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        stream.set_write_timeout(Some(CONNECT_TIMEOUT))?;
        exchange(&mut stream, &payload)?
    };

    #[cfg(windows)]
    let response = {
        let port = daemon_tcp_port(&endpoint.project);
        let mut stream =
            std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), CONNECT_TIMEOUT)?;
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        stream.set_write_timeout(Some(CONNECT_TIMEOUT))?;
        exchange(&mut stream, &payload)?
    };

    #[cfg(not(any(unix, windows)))]
    return Err(DaemonClientError::NotRunning(
        endpoint.project.display().to_string(),
    ));

    decode_response(&response)
}

fn exchange<S: std::io::Read + Write>(
    stream: &mut S,
    payload: &[u8],
) -> Result<Vec<u8>, DaemonClientError> {
    stream.write_all(payload)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut response = Vec::new();
    BufReader::new(stream)
        .take((MAX_MESSAGE_SIZE + 1) as u64)
        .read_until(b'\n', &mut response)?;
    if response.last() != Some(&b'\n') {
        return Err(DaemonClientError::ResponseTooLarge);
    }
    response.pop();
    Ok(response)
}

fn decode_response(response: &[u8]) -> Result<Value, DaemonClientError> {
    let value: Value = serde_json::from_slice(response)?;
    match value.get("status").and_then(Value::as_str) {
        Some("not_ready") => Err(DaemonClientError::NotReady(
            value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("run tldr warm")
                .to_string(),
        )),
        Some("error") => Err(DaemonClientError::Remote(
            value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown daemon error")
                .to_string(),
        )),
        _ => Ok(value.get("result").cloned().unwrap_or(value)),
    }
}

#[cfg(unix)]
fn validate_unix_endpoint(project: &Path, socket: &Path) -> Result<(), DaemonClientError> {
    let canonical = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());
    let expected_hash = format!("{:x}", md5::compute(canonical.to_string_lossy().as_bytes()));
    let expected_name = format!("tldr-{}.sock", &expected_hash[..8]);
    if socket.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str()) {
        return Err(DaemonClientError::InvalidEndpoint(
            socket.display().to_string(),
        ));
    }
    if std::fs::symlink_metadata(socket)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(DaemonClientError::InvalidEndpoint(
            socket.display().to_string(),
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn daemon_tcp_port(project: &Path) -> u16 {
    let canonical = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());
    let hash = format!("{:x}", md5::compute(canonical.to_string_lossy().as_bytes()));
    let hash_int = u64::from_str_radix(&hash[..8], 16).unwrap_or(0);
    49152 + (hash_int % 10000) as u16
}

#[cfg(all(test, unix))]
mod tests {
    use super::request_endpoint;
    use crate::liveness::DaemonEndpoint;
    use std::io::{BufRead, BufReader, Write};

    #[test]
    fn shared_client_exchanges_bounded_newline_json() {
        let project = tempfile::tempdir().expect("temp project");
        let canonical = project.path().canonicalize().expect("canonical project");
        let digest = format!("{:x}", md5::compute(canonical.to_string_lossy().as_bytes()));
        let socket = project.path().join(format!("tldr-{}.sock", &digest[..8]));
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind socket");
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut request = String::new();
            BufReader::new(stream.try_clone().expect("clone stream"))
                .read_line(&mut request)
                .expect("read request");
            assert!(request.contains("\"cmd\":\"search\""));
            stream
                .write_all(b"{\"status\":\"ok\",\"result\":{\"query\":\"risk\"}}\n")
                .expect("write response");
        });

        let endpoint = DaemonEndpoint {
            project: canonical,
            socket,
        };
        let response = request_endpoint(
            &endpoint,
            &serde_json::json!({"cmd": "search", "query": "risk"}),
        )
        .expect("query fake daemon");
        worker.join().expect("join fake daemon");
        assert_eq!(response["query"], "risk");
    }
}

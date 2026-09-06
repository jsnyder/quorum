mod support;

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener};
use std::thread;
use tempfile::TempDir;

#[test]
fn unhealthy_daemon_is_reported_without_false_fallback_message() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request);
        let body = "unhealthy";
        write!(
            stream,
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
        stream.flush().unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
    });

    let home = TempDir::new().unwrap();
    let file = home.path().join("input.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();
    let output = support::quorum_with_quorum_home(home.path())
        .args([
            "review",
            "--daemon",
            "--json",
            "--daemon-port",
            &port.to_string(),
            file.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    server.join().unwrap();

    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Daemon health check returned 503"),
        "health status should be reported; got:\n{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "Daemon is running on port {port} but reported an unhealthy status"
        )),
        "a response proves the daemon is running; got:\n{stderr}"
    );
    assert!(
        !stderr.contains("Daemon not running") && !stderr.contains("Falling back to local review"),
        "health failure must not claim the daemon is stopped or that local review ran; got:\n{stderr}"
    );
}

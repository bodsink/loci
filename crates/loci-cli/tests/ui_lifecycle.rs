//! `loci ui stop` must free a port the same way a user would after
//! `cannot bind 127.0.0.1:7420: Address already in use`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn loci_bin() -> &'static str {
    env!("CARGO_BIN_EXE_loci")
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("pick port");
    listener.local_addr().expect("addr").port()
}

fn health(port: u16) -> bool {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(400)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(400)));
    let request = format!(
        "GET /api/health HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut buf = String::new();
    if stream.read_to_string(&mut buf).is_err() {
        return false;
    }
    buf.contains("\"server\":\"loci-ui\"") || buf.contains("\"server\": \"loci-ui\"")
}

fn wait_health(port: u16, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if health(port) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    health(port)
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_ui(data: &Path, port: u16) -> ChildGuard {
    Command::new(loci_bin())
        .args([
            "ui",
            "--bind",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--no-open",
        ])
        .env("LOCI_DATA_DIR", data)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map(ChildGuard)
        .expect("spawn loci ui")
}

fn stop_ui(data: &Path, port: u16) -> std::process::Output {
    Command::new(loci_bin())
        .args([
            "ui",
            "stop",
            "--bind",
            "127.0.0.1",
            "--port",
            &port.to_string(),
        ])
        .env("LOCI_DATA_DIR", data)
        .output()
        .expect("loci ui stop")
}

fn wait_exit(child: &mut ChildGuard, timeout: Duration) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Some(status) = child.0.try_wait().expect("try_wait") {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.0.try_wait().expect("try_wait")
}

#[test]
fn loci_ui_stop_ends_the_process_holding_the_port() {
    let data = tempfile::tempdir().expect("data dir");
    let port = free_port();
    let mut child = spawn_ui(data.path(), port);
    assert!(
        wait_health(port, Duration::from_secs(5)),
        "loci ui did not become healthy on 127.0.0.1:{port}"
    );

    let output = stop_ui(data.path(), port);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "loci ui stop failed: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("Stopped loci ui"),
        "stop must report the process it ended: {stdout}"
    );
    assert!(
        wait_exit(&mut child, Duration::from_secs(3)).is_some(),
        "the ui process must exit after stop"
    );
    assert!(
        !health(port),
        "127.0.0.1:{port} must be free after loci ui stop"
    );
}

#[test]
fn loci_ui_stop_finds_a_loci_ui_that_has_no_pid_file() {
    let data = tempfile::tempdir().expect("data dir");
    let port = free_port();
    let mut child = spawn_ui(data.path(), port);
    assert!(
        wait_health(port, Duration::from_secs(5)),
        "loci ui did not become healthy on 127.0.0.1:{port}"
    );
    std::fs::remove_file(data.path().join("ui.pid")).expect("drop pid file");

    let output = stop_ui(data.path(), port);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stop without a pid file failed: stdout={stdout} stderr={stderr}"
    );
    assert!(
        wait_exit(&mut child, Duration::from_secs(3)).is_some(),
        "the ui process must exit after stop even without ui.pid"
    );
    assert!(
        !health(port),
        "127.0.0.1:{port} must be free after loci ui stop"
    );
}

#[test]
fn loci_ui_stop_is_ok_when_nothing_is_running() {
    let data = tempfile::tempdir().expect("data dir");
    let port = free_port();
    let output = stop_ui(data.path(), port);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(
        stdout.contains("is not running"),
        "idle stop must say so: {stdout}"
    );
}

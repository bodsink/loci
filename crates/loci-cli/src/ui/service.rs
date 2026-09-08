//! Start/stop bookkeeping for the loopback web UI.
//!
//! `loci ui stop` must free the port a previous `loci ui` is holding. The
//! recorded pid is the first look-up; a loci UI that pre-dates the pid file
//! is found via `/proc` after `/api/health` identifies it. A foreign
//! listener is never killed.

use super::probe_loci_ui;
use loci_core::{paths, LociError, Result};
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
use std::path::Path;
use std::time::{Duration, Instant};

const STOP_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiPid {
    pub pid: u32,
    pub bind: String,
    pub port: u16,
    pub url: String,
}

#[derive(Debug)]
pub enum StopOutcome {
    Stopped { pid: u32, url: String },
    NotRunning { bind: String, port: u16 },
}

pub fn write_pid(record: &UiPid) -> Result<()> {
    let dir = paths::data_dir()?;
    paths::ensure_dir(&dir)?;
    let path = paths::ui_pid_path()?;
    let bytes = serde_json::to_vec_pretty(record)?;
    std::fs::write(&path, bytes).map_err(|source| LociError::io(&path, source))
}

pub fn read_pid() -> Result<Option<UiPid>> {
    let path = paths::ui_pid_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).map_err(|source| LociError::io(&path, source))?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

pub fn remove_pid() -> Result<()> {
    let path = paths::ui_pid_path()?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|source| LociError::io(&path, source))?;
    }
    Ok(())
}

pub fn stop(bind: &str, port: u16) -> Result<StopOutcome> {
    let url = format!("http://{bind}:{port}");
    match resolve_pid(bind, port)? {
        StopTarget::Pid(pid) => {
            terminate(pid)?;
            if let Ok(Some(record)) = read_pid() {
                if record.pid == pid {
                    remove_pid()?;
                }
            }
            Ok(StopOutcome::Stopped { pid, url })
        }
        StopTarget::NotRunning => {
            if let Ok(Some(record)) = read_pid() {
                if record.port == port && !process_alive(record.pid) {
                    remove_pid()?;
                }
            }
            Ok(StopOutcome::NotRunning {
                bind: bind.to_string(),
                port,
            })
        }
    }
}

enum StopTarget {
    Pid(u32),
    NotRunning,
}

fn resolve_pid(bind: &str, port: u16) -> Result<StopTarget> {
    if let Some(record) = read_pid()? {
        if record.port == port
            && bind_matches(&record.bind, bind)
            && process_alive(record.pid)
            && is_loci_ui_process(record.pid)
        {
            return Ok(StopTarget::Pid(record.pid));
        }
    }

    let listener = find_listener_pid(bind, port);
    if probe_loci_ui(bind, port) {
        return match listener {
            Some(pid) => Ok(StopTarget::Pid(pid)),
            None => Err(LociError::InvalidArgument(format!(
                "loci ui is listening on {bind}:{port} but its process could not be identified"
            ))),
        };
    }

    if let Some(pid) = listener {
        return Err(LociError::InvalidArgument(format!(
            "{bind}:{port} is in use by another process (pid {pid}); not a loci UI. \
             loci ui stop will not kill it"
        )));
    }

    if port_in_use(bind, port) {
        return Err(LociError::InvalidArgument(format!(
            "{bind}:{port} is in use by another process; not a loci UI. \
             loci ui stop will not kill it"
        )));
    }

    Ok(StopTarget::NotRunning)
}

fn port_in_use(bind: &str, port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::new(
            bind.parse().unwrap_or(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST)),
            port,
        ),
        Duration::from_millis(150),
    )
    .is_ok()
}

fn bind_matches(recorded: &str, requested: &str) -> bool {
    recorded == requested
}

fn process_alive(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // `stat` is `pid (comm) state ...`; comm may contain spaces and ')'.
    let Some((_, after_comm)) = stat.rsplit_once(')') else {
        return false;
    };
    !after_comm.trim_start().starts_with('Z')
}

fn is_loci_ui_process(pid: u32) -> bool {
    let Ok(bytes) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let parts: Vec<String> = bytes
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    let Some(argv0) = parts.first() else {
        return false;
    };
    let file = Path::new(argv0)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    file == "loci" && parts.iter().any(|arg| arg == "ui")
}

fn terminate(pid: u32) -> Result<()> {
    if pid == std::process::id() {
        return Err(LociError::InvalidArgument(
            "refusing to stop the current process".to_string(),
        ));
    }
    send_signal(pid, "TERM")?;
    if wait_until_dead(pid, STOP_WAIT) {
        return Ok(());
    }
    send_signal(pid, "KILL")?;
    if wait_until_dead(pid, Duration::from_secs(1)) {
        return Ok(());
    }
    Err(LociError::InvalidArgument(format!(
        "loci ui process {pid} did not exit after SIGTERM/SIGKILL"
    )))
}

fn send_signal(pid: u32, signal: &str) -> Result<()> {
    let output = std::process::Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .output()
        .map_err(|source| LociError::io("<kill>", source))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !process_alive(pid) {
        return Ok(());
    }
    Err(LociError::InvalidArgument(format!(
        "kill -{signal} {pid} failed: {stderr}"
    )))
}

fn wait_until_dead(pid: u32, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if !process_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !process_alive(pid)
}

/// Linux: inode of a LISTEN socket on `bind:port`, then the `/proc` pid that
/// holds that socket.
pub fn find_listener_pid(bind: &str, port: u16) -> Option<u32> {
    if let Some(inode) = listen_inode(bind, port) {
        if let Some(pid) = pid_holding_socket(inode) {
            return Some(pid);
        }
    }
    find_listener_via_ss(bind, port)
}

fn listen_inode(bind: &str, port: u16) -> Option<u64> {
    let ip: Ipv4Addr = bind.parse().ok()?;
    let needle = ipv4_listen_key(ip, port);
    listen_inode_in("/proc/net/tcp", &needle)
        .or_else(|| listen_inode_in("/proc/self/net/tcp", &needle))
}

fn ipv4_listen_key(ip: Ipv4Addr, port: u16) -> String {
    let o = ip.octets();
    format!("{:02X}{:02X}{:02X}{:02X}:{port:04X}", o[3], o[2], o[1], o[0])
}

fn listen_inode_in(path: &str, local: &str) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 10 {
            continue;
        }
        if cols[1].eq_ignore_ascii_case(local) && cols[3].eq_ignore_ascii_case("0A") {
            return cols[9].parse().ok();
        }
    }
    None
}

fn pid_holding_socket(inode: u64) -> Option<u32> {
    let needle = format!("socket:[{inode}]");
    if fd_dir_has_socket(Path::new("/proc/self/fd"), &needle) {
        return Some(std::process::id());
    }
    let proc = std::fs::read_dir("/proc").ok()?;
    for entry in proc.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if fd_dir_has_socket(&entry.path().join("fd"), &needle) {
            return Some(pid);
        }
    }
    None
}

fn fd_dir_has_socket(fd_dir: &Path, needle: &str) -> bool {
    let Ok(fds) = std::fs::read_dir(fd_dir) else {
        return false;
    };
    fds.flatten().any(|fd| {
        std::fs::read_link(fd.path()).is_ok_and(|target| target == Path::new(needle))
    })
}

fn find_listener_via_ss(bind: &str, port: u16) -> Option<u32> {
    let output = std::process::Command::new("ss")
        .args(["-lptn", &format!("sport = :{port}")])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let addr = format!("{bind}:{port}");
    for line in text.lines() {
        if !line.contains(&addr) {
            continue;
        }
        if let Some(pid) = line
            .split("pid=")
            .nth(1)
            .and_then(|rest| rest.split([',', ')']).next())
            .and_then(|s| s.parse().ok())
        {
            return Some(pid);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn ipv4_listen_key_matches_proc_net_tcp() {
        assert_eq!(
            ipv4_listen_key(Ipv4Addr::new(127, 0, 0, 1), 7420),
            "0100007F:1CFC"
        );
    }

    #[test]
    fn find_listener_pid_returns_this_process_for_its_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let found = find_listener_pid("127.0.0.1", port);
        assert_eq!(
            found,
            Some(std::process::id()),
            "the scanner must see the socket this test just bound"
        );
        drop(listener);
    }

    #[test]
    fn stop_is_ok_when_nothing_is_listening() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("pick port");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        match stop("127.0.0.1", port).expect("stop") {
            StopOutcome::NotRunning {
                bind,
                port: stopped,
            } => {
                assert_eq!(bind, "127.0.0.1");
                assert_eq!(stopped, port);
            }
            StopOutcome::Stopped { pid, .. } => {
                panic!("nothing was listening; must not signal pid {pid}")
            }
        }
    }

    #[test]
    fn stop_refuses_to_kill_a_foreign_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("occupy");
        let port = listener.local_addr().expect("addr").port();
        let error = stop("127.0.0.1", port).expect_err("must refuse");
        assert_eq!(error.code(), "invalid_argument");
        let message = error.to_string();
        assert!(
            message.contains("not a loci UI"),
            "refusal must name the reason: {message}"
        );
        assert!(
            listener.local_addr().is_ok(),
            "the foreign socket must still be bound after stop refused"
        );
    }
}

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// Both binaries live in the same target dir.
fn bin(name: &str) -> PathBuf {
    let sitty = PathBuf::from(env!("CARGO_BIN_EXE_sitty"));
    sitty.with_file_name(name)
}

fn sitty(socket: &Path, args: &[&str]) -> Output {
    Command::new(bin("sitty")).env("SITTY_SOCKET", socket).env_remove("SITTY_TOKEN").args(args).output().unwrap()
}

fn ok(o: Output) -> String {
    assert!(o.status.success(), "stderr: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

fn json(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap()
}

fn build_sittyd() {
    assert!(Command::new("cargo").args(["build", "-q", "-p", "sittyd"]).status().unwrap().success());
}

/// Kills the auto-started daemon for this socket (and only that one) via its pid file.
struct Guard(PathBuf);
impl Drop for Guard {
    fn drop(&mut self) {
        if let Ok(pid) = std::fs::read_to_string(self.0.with_file_name("sittyd.pid")) {
            let _ = Command::new("kill").arg(pid.trim()).status();
        }
    }
}

#[test]
fn autostart_open_send_wait_read() {
    build_sittyd();
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.sock");
    let _g = Guard(socket.clone());

    let ws = json(&ok(sitty(&socket, &["--human", "workspace", "open", "demo", "--dir", "/tmp"])));
    let ws = ws["workspace"].as_str().unwrap().to_string();
    let tab = json(&ok(sitty(&socket, &["--human", "tab", "open", "--workspace", &ws, "--", "/bin/sh"])));
    let pane = tab["pane"].as_str().unwrap().to_string();

    ok(sitty(&socket, &["--human", "pane", "send", &pane, "--text", "echo cli-$((1+1))", "--key", "enter"]));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let screen = ok(sitty(&socket, &["pane", "read", &pane]));
        if screen.contains("cli-2") {
            break;
        }
        assert!(Instant::now() < deadline, "screen: {screen}");
        std::thread::sleep(Duration::from_millis(50));
    }

    // External (no --human) cannot type.
    let o = sitty(&socket, &["pane", "send", &pane, "--text", "x"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("PERMISSION_REQUIRED"));

    // Timeout maps to exit code 124.
    let o = sitty(&socket, &["--human", "pane", "wait", &pane, "--match", "never-printed", "--timeout", "200"]);
    assert_eq!(o.status.code(), Some(124));
}

#[test]
fn harness_inside_a_pane_uses_its_own_token() {
    build_sittyd();
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.sock");
    let _g = Guard(socket.clone());
    let sitty_bin = bin("sitty");

    let ws = json(&ok(sitty(&socket, &["--human", "workspace", "open", "demo", "--dir", "/tmp"])));
    let ws = ws["workspace"].as_str().unwrap().to_string();
    // The "harness" is a shell script that uses the CLI from inside its own pane.
    let script = format!(
        "{s} status set working --label scripted && {s} pane open --down -- /bin/cat && {s} whoami",
        s = sitty_bin.display()
    );
    let tab = json(&ok(sitty(&socket, &["--human", "tab", "open", "--workspace", &ws, "--", "/bin/sh", "-c", &format!("{script}; sleep 30")])));
    let pane = tab["pane"].as_str().unwrap().to_string();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let roster = json(&ok(sitty(&socket, &["roster"])));
        let panes = roster["workspaces"][0]["tabs"][0]["panes"].as_array().unwrap().clone();
        let me = panes.iter().find(|p| p["id"] == pane.as_str()).unwrap();
        let child = panes.iter().find(|p| p["owner"] == pane.as_str());
        if me["status"]["label"] == "scripted" && child.is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "roster: {roster}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn second_daemon_refuses_live_socket() {
    build_sittyd();
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.sock");
    let _g = Guard(socket.clone());
    ok(sitty(&socket, &["roster"])); // auto-starts the first daemon
    let second = Command::new(bin("sittyd")).env("SITTY_SOCKET", &socket).output().unwrap();
    assert_eq!(second.status.code(), Some(2));
    ok(sitty(&socket, &["roster"])); // first one still serving
}

#[test]
fn stale_socket_is_replaced() {
    build_sittyd();
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.sock");
    let _g = Guard(socket.clone());
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap()); // leaves a dead socket file
    ok(sitty(&socket, &["roster"]));
}

#[test]
fn relative_dir_is_resolved_against_the_cli_cwd() {
    build_sittyd();
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.sock");
    let _g = Guard(socket.clone());
    let cwd = tempfile::tempdir().unwrap();

    let o = Command::new(bin("sitty"))
        .current_dir(cwd.path())
        .env("SITTY_SOCKET", &socket)
        .env_remove("SITTY_TOKEN")
        .args(["--human", "workspace", "open", "demo", "--dir", "."])
        .output()
        .unwrap();
    ok(o);
    let roster = json(&ok(sitty(&socket, &["roster"])));
    let got = roster["workspaces"][0]["dir"].as_str().unwrap();
    assert_eq!(std::fs::canonicalize(got).unwrap(), std::fs::canonicalize(cwd.path()).unwrap());
}

#[test]
fn human_without_a_readable_token_fails_loudly() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.sock");
    let _g = Guard(socket.clone());
    ok(sitty(&socket, &["roster"])); // auto-starts the daemon, which writes the token
    std::fs::remove_file(socket.with_file_name("human.token")).unwrap();
    let o = sitty(&socket, &["--human", "roster"]);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("error: cannot read human token at"), "stderr: {err}");
}

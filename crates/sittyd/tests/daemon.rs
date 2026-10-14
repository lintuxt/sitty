use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sitty_proto::client::{Client, ClientError};
use sitty_proto::{ErrorKind, PaneOpened, PaneText, Principal, Roster, TabOpened, WaitResult, WorkspaceOpened, paths};

struct Daemon {
    child: Child,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl Daemon {
    fn start() -> Daemon {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("sittyd.sock");
        let child = Command::new(env!("CARGO_BIN_EXE_sittyd")).env("SITTY_SOCKET", &socket).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::os::unix::net::UnixStream::connect(&socket).is_err() || !paths::human_token_path(&socket).exists() {
            assert!(Instant::now() < deadline, "daemon did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        Daemon { child, socket, _dir: dir }
    }

    fn human(&self) -> Client {
        let token = std::fs::read_to_string(paths::human_token_path(&self.socket)).unwrap();
        Client::connect(&self.socket, "test", Some(token.trim().to_string())).unwrap()
    }

    fn external(&self) -> Client {
        Client::connect(&self.socket, "test", None).unwrap()
    }

    /// Read a pane's token the way a harness would: from its own environment.
    fn token_of(&self, human: &mut Client, pane: &str) -> String {
        human.call::<_, Value>("pane.send", json!({"pane": pane, "text": "echo TOKEN=$SITTY_TOKEN\r"})).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let t: PaneText = human.call("pane.read", json!({"pane": pane, "what": "scrollback"})).unwrap();
            if let Some(l) = t.lines.iter().find(|l| l.starts_with("TOKEN=") && l.len() > 6) {
                return l.trim_start_matches("TOKEN=").to_string();
            }
            assert!(Instant::now() < deadline, "no token echoed");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn as_pane(&self, token: String) -> Client {
        Client::connect(&self.socket, "test", Some(token)).unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn kind(e: ClientError) -> ErrorKind {
    match e {
        ClientError::Rpc(r) => r.kind().unwrap(),
        other => panic!("expected rpc error, got {other}"),
    }
}

fn read_until(c: &mut Client, pane: &str, needle: &str) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let t: PaneText = c.call("pane.read", json!({"pane": pane})).unwrap();
        if t.lines.iter().any(|l| l.contains(needle)) {
            return t.lines;
        }
        assert!(Instant::now() < deadline, "never saw {needle:?}; screen: {:?}", t.lines);
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// human: workspace w + tab with a `sh` pane. Returns (workspace, tab, pane).
fn setup(d: &Daemon) -> (Client, String, String, String) {
    let mut h = d.human();
    let w: WorkspaceOpened = h.call("workspace.open", json!({"name": "demo", "dir": "/tmp"})).unwrap();
    let t: TabOpened = h.call("tab.open", json!({"workspace": w.workspace, "cmd": ["/bin/sh"]})).unwrap();
    (h, w.workspace, t.tab, t.pane)
}

#[test]
fn principals_from_tokens() {
    let d = Daemon::start();
    assert_eq!(d.human().principal(), &Principal::Human);
    assert_eq!(d.external().principal(), &Principal::External);
    let bad = Client::connect(&d.socket, "test", Some("forged".into())).err().expect("forged token must be rejected");
    assert_eq!(kind(bad), ErrorKind::PermissionRequired);
}

#[test]
fn human_opens_runs_and_reads() {
    let d = Daemon::start();
    let (mut h, _, _, pane) = setup(&d);
    h.call::<_, Value>("pane.send", json!({"pane": pane, "text": "echo hi-$((40+2))", "keys": ["enter"]})).unwrap();
    read_until(&mut h, &pane, "hi-42");
}

#[test]
fn harness_identity_and_children() {
    let d = Daemon::start();
    let (mut h, ws, _, pane) = setup(&d);
    let mut claude = d.as_pane(d.token_of(&mut h, &pane));
    assert_eq!(claude.principal(), &Principal::Harness { pane: pane.clone(), workspace: ws.clone() });

    let child: PaneOpened = claude.call("pane.open", json!({"cmd": ["/bin/cat"], "placement": "down"})).unwrap();
    let r: Roster = h.call("roster", json!({})).unwrap();
    let info = r.workspaces[0].tabs[0].panes.iter().find(|p| p.id == child.pane).unwrap();
    assert_eq!(info.owner, pane);

    claude.call::<_, Value>("pane.send", json!({"pane": child.pane, "text": "ping\r"})).unwrap();
    read_until(&mut claude, &child.pane, "ping");
    claude.call::<_, Value>("status.set", json!({"state": "working", "label": "testing"})).unwrap();
}

#[test]
fn harness_cannot_type_into_panes_it_does_not_own() {
    let d = Daemon::start();
    let (mut h, ws, tab, pane) = setup(&d);
    let other: PaneOpened = h.call("pane.open", json!({"tab": tab, "cmd": ["/bin/cat"]})).unwrap();
    let mut claude = d.as_pane(d.token_of(&mut h, &pane));

    let e = claude.call::<_, Value>("pane.send", json!({"pane": other.pane, "text": "x"})).unwrap_err();
    assert_eq!(kind(e), ErrorKind::PermissionRequired);
    let e = claude.call::<_, Value>("pane.close", json!({"pane": other.pane})).unwrap_err();
    assert_eq!(kind(e), ErrorKind::PermissionRequired);
    let e = claude.call::<_, Value>("tab.close", json!({"tab": tab})).unwrap_err();
    assert_eq!(kind(e), ErrorKind::PermissionRequired);
    // ...but reading is allowed.
    claude.call::<_, PaneText>("pane.read", json!({"pane": other.pane})).unwrap();
    // ...and other workspaces are off limits.
    let w2: WorkspaceOpened = h.call("workspace.open", json!({"name": "other"})).unwrap();
    assert_ne!(w2.workspace, ws);
    let e = claude.call::<_, Value>("tab.open", json!({"workspace": w2.workspace})).unwrap_err();
    assert_eq!(kind(e), ErrorKind::PermissionRequired);
}

#[test]
fn external_is_read_only() {
    let d = Daemon::start();
    let (_h, ws, _, pane) = setup(&d);
    let mut x = d.external();
    x.call::<_, Roster>("roster", json!({})).unwrap();
    x.call::<_, PaneText>("pane.read", json!({"pane": pane})).unwrap();
    for (m, p) in [
        ("pane.send", json!({"pane": pane, "text": "x"})),
        ("tab.open", json!({"workspace": ws})),
        ("workspace.open", json!({"name": "x"})),
        ("status.set", json!({"state": "idle"})),
    ] {
        assert_eq!(kind(x.call::<_, Value>(m, p).unwrap_err()), ErrorKind::PermissionRequired, "{m}");
    }
}

#[test]
fn closing_harness_pane_orphans_children_to_human() {
    let d = Daemon::start();
    let (mut h, _, _, pane) = setup(&d);
    let mut claude = d.as_pane(d.token_of(&mut h, &pane));
    let child: PaneOpened = claude.call("pane.open", json!({"cmd": ["/bin/cat"]})).unwrap();
    h.call::<_, Value>("pane.close", json!({"pane": pane})).unwrap();
    let r: Roster = h.call("roster", json!({})).unwrap();
    let info = r.workspaces[0].tabs[0].panes.iter().find(|p| p.id == child.pane).unwrap();
    assert_eq!(info.owner, "human");
    assert!(info.alive);
}

#[test]
fn errors_for_unknown_things() {
    let d = Daemon::start();
    let mut h = d.human();
    assert_eq!(kind(h.call::<_, Value>("pane.read", json!({"pane": "p404"})).unwrap_err()), ErrorKind::NoSuchPane);
    assert_eq!(kind(h.call::<_, Value>("tab.open", json!({"workspace": "w404"})).unwrap_err()), ErrorKind::NoSuchWorkspace);
    assert_eq!(kind(h.call::<_, Value>("tab.open", json!({})).unwrap_err()), ErrorKind::InvalidParams);
    assert_eq!(kind(h.call::<_, Value>("pane.read", json!({"bogus": 1})).unwrap_err()), ErrorKind::InvalidParams);
    assert_eq!(kind(h.call::<_, Value>("nope.nope", json!({})).unwrap_err()), ErrorKind::MethodNotFound);
}

#[test]
fn sessions_survive_client_disconnect() {
    let d = Daemon::start();
    let (h, _, _, pane) = setup(&d);
    drop(h);
    let mut h2 = d.human();
    h2.call::<_, Value>("pane.send", json!({"pane": pane, "text": "echo still-here\r"})).unwrap();
    read_until(&mut h2, &pane, "still-here");
}

#[test]
fn garbage_line_gets_error_and_connection_survives() {
    use std::io::{BufRead, BufReader, Write};
    let d = Daemon::start();
    let s = std::os::unix::net::UnixStream::connect(&d.socket).unwrap();
    let mut w = s.try_clone().unwrap();
    let mut r = BufReader::new(s);
    w.write_all(b"{not json\n").unwrap();
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    assert!(line.contains("INVALID_PARAMS"), "{line}");
    // The connection is still usable: a valid hello now succeeds.
    let hello = json!({"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocol_version":1,"client":"x"}});
    writeln!(w, "{hello}").unwrap();
    line.clear();
    r.read_line(&mut line).unwrap();
    assert!(line.contains("\"result\"") && !line.contains("error"), "{line}");
}

#[test]
fn closed_harness_pane_loses_its_rights() {
    let d = Daemon::start();
    let (mut h, ws, _, pane) = setup(&d);
    let mut claude = d.as_pane(d.token_of(&mut h, &pane));
    h.call::<_, Value>("pane.close", json!({"pane": pane})).unwrap();
    let e = claude.call::<_, TabOpened>("tab.open", json!({"workspace": ws})).unwrap_err();
    assert_eq!(kind(e), ErrorKind::PermissionRequired);
    let e = claude.call::<_, Value>("status.set", json!({"state": "idle"})).unwrap_err();
    assert_eq!(kind(e), ErrorKind::PermissionRequired);
    // Read-only methods still work.
    claude.call::<_, Roster>("roster", json!({})).unwrap();
}

#[test]
fn harness_pane_open_with_tab_appends_to_that_tab() {
    let d = Daemon::start();
    let (mut h, ws, _, pane) = setup(&d);
    let t2: TabOpened = h.call("tab.open", json!({"workspace": ws, "cmd": ["/bin/cat"]})).unwrap();
    let mut claude = d.as_pane(d.token_of(&mut h, &pane));
    let opened: PaneOpened = claude.call("pane.open", json!({"tab": t2.tab, "cmd": ["/bin/cat"]})).unwrap();
    assert_eq!(opened.tab, t2.tab);
}

#[test]
fn existing_user_dir_is_not_chmodded() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let sock = dir.path().join("s.sock");
    let _l = sittyd::server::prepare_socket(&sock).unwrap();
    let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755);

    // A group/world-writable dir is refused outright.
    let bad = tempfile::tempdir().unwrap();
    std::fs::set_permissions(bad.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    let e = sittyd::server::prepare_socket(&bad.path().join("s.sock")).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);

    // A missing dir is created 0700.
    let fresh = dir.path().join("new");
    let _l2 = sittyd::server::prepare_socket(&fresh.join("s.sock")).unwrap();
    assert_eq!(std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777, 0o700);
}

#[test]
fn failed_spawn_leaves_no_tab_behind() {
    let d = Daemon::start();
    let mut h = d.human();
    let w: WorkspaceOpened = h.call("workspace.open", json!({"name": "demo", "dir": "/tmp"})).unwrap();
    let e = h.call::<_, Value>("tab.open", json!({"workspace": w.workspace, "cmd": ["/nonexistent/bin"]})).unwrap_err();
    assert_eq!(kind(e), ErrorKind::Internal);
    let r: Roster = h.call("roster", json!({})).unwrap();
    assert!(r.workspaces[0].tabs.is_empty(), "{:?}", r.workspaces[0].tabs);

    // Same for pane.open with placement=tab, alongside a healthy tab.
    let t: TabOpened = h.call("tab.open", json!({"workspace": w.workspace, "cmd": ["/bin/sh"]})).unwrap();
    let e = h
        .call::<_, Value>("pane.open", json!({"of": t.pane, "placement": "tab", "cmd": ["/nonexistent/bin"]}))
        .unwrap_err();
    assert_eq!(kind(e), ErrorKind::Internal);
    let r: Roster = h.call("roster", json!({})).unwrap();
    assert_eq!(r.workspaces[0].tabs.len(), 1);
}

#[test]
fn client_disconnect_mid_request_keeps_pane() {
    use std::io::Write;
    let d = Daemon::start();
    let (mut h, _, _, pane) = setup(&d);
    let token = std::fs::read_to_string(paths::human_token_path(&d.socket)).unwrap();
    // Complete the handshake, then fire a request and vanish without reading its reply.
    let mut s = std::os::unix::net::UnixStream::connect(&d.socket).unwrap();
    let hello = json!({"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocol_version":1,"client":"x","token":token.trim()}});
    let send = json!({"jsonrpc":"2.0","id":2,"method":"pane.send","params":{"pane":pane,"text":"echo survivor\r"}});
    writeln!(s, "{hello}").unwrap();
    let mut reader = std::io::BufReader::new(s.try_clone().unwrap());
    let mut line = String::new();
    std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
    assert!(line.contains("\"result\""), "{line}");
    writeln!(s, "{send}").unwrap();
    drop(reader);
    drop(s);
    read_until(&mut h, &pane, "survivor");
    let r: Roster = h.call("roster", json!({})).unwrap();
    assert!(r.workspaces[0].tabs[0].panes.iter().any(|p| p.id == pane && p.alive));
}

#[test]
fn wait_for_exit_returns_code() {
    let d = Daemon::start();
    let (mut h, ws, _, _) = setup(&d);
    let t: TabOpened = h.call("tab.open", json!({"workspace": ws, "cmd": ["/bin/sh", "-c", "sleep 0.2; exit 7"]})).unwrap();
    let r: WaitResult = h.call("pane.wait", json!({"pane": t.pane, "until": "exit"})).unwrap();
    assert_eq!(r, WaitResult { reason: "exit".into(), exit_code: Some(7), matched: None });
    // Already exited: returns immediately.
    let r: WaitResult = h.call("pane.wait", json!({"pane": t.pane, "until": "exit", "timeout_ms": 50})).unwrap();
    assert_eq!(r.exit_code, Some(7));
}

#[test]
fn match_ignores_output_before_wait() {
    let d = Daemon::start();
    let (mut h, _, _, pane) = setup(&d);
    h.call::<_, Value>("pane.send", json!({"pane": pane, "text": "echo REA\"\"DY\r"})).unwrap();
    // The quotes keep the typed echo of the command from containing READY: only the output does.
    read_until(&mut h, &pane, "READY");
    // READY is already on screen; the wait must not return on it.
    let e = h.call::<_, WaitResult>("pane.wait", json!({"pane": pane, "until": {"match": "READY"}, "timeout_ms": 300})).unwrap_err();
    assert_eq!(kind(e), ErrorKind::Timeout);
}

#[test]
fn match_sees_new_output() {
    let d = Daemon::start();
    let (mut h, _, _, pane) = setup(&d);
    let sock = d.socket.clone();
    let token = std::fs::read_to_string(paths::human_token_path(&sock)).unwrap();
    let p2 = pane.clone();
    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        let mut c = Client::connect(&sock, "test", Some(token.trim().to_string())).unwrap();
        c.call::<_, Value>("pane.send", json!({"pane": p2, "text": "echo done-$((6*7))\r"})).unwrap();
    });
    let r: WaitResult = h.call("pane.wait", json!({"pane": pane, "until": {"match": "done-\\d+"}})).unwrap();
    sender.join().unwrap();
    assert_eq!(r.matched.as_deref(), Some("done-42"));
}

#[test]
fn quiet_and_bad_regex() {
    let d = Daemon::start();
    let (mut h, _, _, pane) = setup(&d);
    let r: WaitResult = h.call("pane.wait", json!({"pane": pane, "until": {"quiet": 150}})).unwrap();
    assert_eq!(r.reason, "quiet");
    let e = h.call::<_, WaitResult>("pane.wait", json!({"pane": pane, "until": {"match": "("}})).unwrap_err();
    assert_eq!(kind(e), ErrorKind::InvalidParams);
}

#[test]
fn match_on_exited_pane_is_pane_exited() {
    let d = Daemon::start();
    let (mut h, ws, _, _) = setup(&d);
    let t: TabOpened = h.call("tab.open", json!({"workspace": ws, "cmd": ["/bin/sh", "-c", "sleep 0.2"]})).unwrap();
    let e = h.call::<_, WaitResult>("pane.wait", json!({"pane": t.pane, "until": {"match": "never"}})).unwrap_err();
    assert_eq!(kind(e), ErrorKind::PaneExited);
}

#[test]
fn client_disconnect_mid_wait_keeps_pane() {
    use std::io::Write;
    let d = Daemon::start();
    let (mut h, _, _, pane) = setup(&d);
    {
        // Raw connection: hello, start a long wait, then vanish.
        let token = std::fs::read_to_string(paths::human_token_path(&d.socket)).unwrap();
        let mut s = std::os::unix::net::UnixStream::connect(&d.socket).unwrap();
        writeln!(s, r#"{{"jsonrpc":"2.0","id":1,"method":"hello","params":{{"protocol_version":1,"client":"t","token":"{}"}}}}"#, token.trim()).unwrap();
        writeln!(s, r#"{{"jsonrpc":"2.0","id":2,"method":"pane.wait","params":{{"pane":"{pane}","until":"exit"}}}}"#).unwrap();
        std::thread::sleep(Duration::from_millis(100));
    }
    h.call::<_, Value>("pane.send", json!({"pane": pane, "text": "echo alive\r"})).unwrap();
    read_until(&mut h, &pane, "alive");
    let r: Roster = h.call("roster", json!({})).unwrap();
    assert!(r.workspaces[0].tabs[0].panes.iter().any(|p| p.id == pane && p.alive));
}

/// Kills (and reaps) every child on drop, including when an assertion panics.
struct KillOnDrop(Vec<Child>);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        for k in &mut self.0 {
            let _ = k.kill();
            let _ = k.wait();
        }
    }
}
impl std::ops::Deref for KillOnDrop {
    type Target = Vec<Child>;
    fn deref(&self) -> &Vec<Child> {
        &self.0
    }
}
impl std::ops::DerefMut for KillOnDrop {
    fn deref_mut(&mut self) -> &mut Vec<Child> {
        &mut self.0
    }
}

/// Several daemons racing for one socket: exactly one survives (the rest exit 2) and the human token
/// on disk is the survivor's, not one a losing daemon wrote.
#[test]
fn concurrent_daemon_start_leaves_exactly_one() {
    for round in 0..4 {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("sittyd.sock");
        let mut kids = KillOnDrop(
            (0..6)
                .map(|_| Command::new(env!("CARGO_BIN_EXE_sittyd")).env("SITTY_SOCKET", &socket).stderr(Stdio::null()).spawn().unwrap())
                .collect(),
        );

        // Losers exit (code 2); the winner keeps running.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut codes = vec![None; kids.len()];
        while codes.iter().filter(|c| c.is_none()).count() > 1 {
            assert!(Instant::now() < deadline, "round {round}: losers did not exit");
            for (i, k) in kids.iter_mut().enumerate() {
                if codes[i].is_none() {
                    codes[i] = k.try_wait().unwrap().map(|s| s.code());
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(200)); // let a wrongly-surviving second one show itself
        for (i, k) in kids.iter_mut().enumerate() {
            if codes[i].is_none() {
                codes[i] = k.try_wait().unwrap().map(|s| s.code());
            }
        }
        let running: Vec<usize> = (0..kids.len()).filter(|&i| codes[i].is_none()).collect();
        assert_eq!(running.len(), 1, "round {round}: exit codes {codes:?}");
        assert!(codes.iter().flatten().all(|c| *c == Some(2)), "round {round}: exit codes {codes:?}");

        let token = std::fs::read_to_string(paths::human_token_path(&socket)).unwrap();
        let c = Client::connect(&socket, "test", Some(token.trim().to_string())).unwrap();
        assert_eq!(*c.principal(), Principal::Human, "round {round}: token on disk is not the survivor's");
        let pid = std::fs::read_to_string(socket.with_file_name("sittyd.pid")).unwrap();
        assert_eq!(pid.trim(), kids[running[0]].id().to_string(), "round {round}: pid file is not the survivor's");

    }
}

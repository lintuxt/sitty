use std::io::ErrorKind as IoKind;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use sitty_proto::client::{Client, ClientError};
use sitty_proto::{ErrorKind, PaneText, paths};

#[derive(Parser)]
#[command(name = "sitty", version, about = "Drive sitty sessions from a shell or a harness")]
struct Cli {
    /// Act as the human (reads the human token). Ignored inside a sitty pane.
    #[arg(long, global = true)]
    human: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show who this connection is.
    Whoami,
    /// Show workspaces, tabs, panes, owners and status.
    Roster {
        #[arg(long)]
        workspace: Option<String>,
    },
    #[command(subcommand)]
    Workspace(WorkspaceCmd),
    #[command(subcommand)]
    Tab(TabCmd),
    #[command(subcommand)]
    Pane(PaneCmd),
    #[command(subcommand)]
    Status(StatusCmd),
}

#[derive(Subcommand)]
enum WorkspaceCmd {
    Open {
        name: String,
        #[arg(long)]
        dir: Option<String>,
    },
}

#[derive(Subcommand)]
enum TabCmd {
    Open {
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(last = true)]
        cmd: Vec<String>,
    },
    Rename { tab: String, title: String },
    Focus { tab: String },
    Close { tab: String },
}

#[derive(Subcommand)]
enum PaneCmd {
    Open {
        #[arg(long, conflicts_with = "tab")]
        down: bool,
        #[arg(long)]
        tab: bool,
        #[arg(long)]
        of: Option<String>,
        #[arg(long = "in-tab")]
        in_tab: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(last = true)]
        cmd: Vec<String>,
    },
    Read {
        pane: String,
        #[arg(long)]
        scrollback: bool,
        #[arg(long)]
        lines: Option<usize>,
    },
    Send {
        pane: String,
        #[arg(long)]
        text: Option<String>,
        #[arg(long = "key")]
        keys: Vec<String>,
    },
    Wait {
        pane: String,
        #[arg(long, group = "until")]
        exit: bool,
        #[arg(long = "match", group = "until")]
        pattern: Option<String>,
        #[arg(long, group = "until")]
        quiet: Option<u64>,
        #[arg(long)]
        timeout: Option<u64>,
    },
    Focus { pane: String },
    Close { pane: String },
}

#[derive(Subcommand)]
enum StatusCmd {
    Set {
        #[arg(value_parser = ["working", "idle", "attention", "error"])]
        state: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        progress: Option<f32>,
    },
}

fn opt_cmd(cmd: Vec<String>) -> Option<Vec<String>> {
    if cmd.is_empty() { None } else { Some(cmd) }
}

fn token(human: bool, socket: &Path) -> Option<String> {
    if let Ok(t) = std::env::var("SITTY_TOKEN") {
        return Some(t);
    }
    if human {
        let path = paths::human_token_path(socket);
        return match std::fs::read_to_string(&path) {
            Ok(t) => Some(t.trim().to_string()),
            Err(e) => {
                // Not a ClientError: its Display would prefix "io: ".
                eprintln!("error: cannot read human token at {}: {e}", path.display());
                std::process::exit(1);
            }
        };
    }
    None
}

/// Daemon-bound paths are resolved against the CLI's cwd; the daemon runs elsewhere.
fn absolutize(p: Option<String>) -> Result<Option<String>, ClientError> {
    p.map(|p| std::path::absolute(&p).map(|a| a.to_string_lossy().into_owned()))
        .transpose()
        .map_err(ClientError::Io)
}

fn sittyd_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .map(|p| p.with_file_name("sittyd"))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("sittyd"))
}

fn start_daemon(socket: &Path) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    if let Some(dir) = socket.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let log = std::fs::OpenOptions::new().create(true).append(true).open(socket.with_file_name("sittyd.log"))?;
    let mut cmd = Command::new(sittyd_path());
    cmd.env("SITTY_SOCKET", socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .current_dir("/");
    // SAFETY: setsid is async-signal-safe; fully detaches the daemon from the CLI's session.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()?;
    Ok(())
}

fn connect(socket: &Path, human: bool) -> Result<Client, ClientError> {
    // Probe first (opens and drops an external connection): keeps auto-start independent of the token.
    // During start-up the socket file can exist before the daemon accepts, so both NotFound and
    // ConnectionRefused mean "not up yet".
    let first = Client::connect(socket, "cli", None).map(|_| ());
    if let Err(ClientError::Io(e)) = &first
        && matches!(e.kind(), IoKind::NotFound | IoKind::ConnectionRefused)
    {
        start_daemon(socket)?;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Client::connect(socket, "cli", None).is_err() {
            if Instant::now() > deadline {
                return Err(ClientError::Io(std::io::Error::other("sittyd did not start (see sittyd.log)")));
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    Client::connect(socket, "cli", token(human, socket))
}

fn run(cli: Cli) -> Result<(), ClientError> {
    let socket = paths::socket_path();
    let mut c = connect(&socket, cli.human)?;
    let v: Value = match cli.cmd {
        Cmd::Whoami => c.call("whoami", json!({}))?,
        Cmd::Roster { workspace } => c.call("roster", json!({"workspace": workspace}))?,
        Cmd::Workspace(WorkspaceCmd::Open { name, dir }) => {
            let dir = absolutize(dir)?;
            c.call("workspace.open", json!({"name": name, "dir": dir}))?
        }
        Cmd::Tab(TabCmd::Open { workspace, title, cwd, cmd }) => {
            let cwd = absolutize(cwd)?;
            c.call(
            "tab.open",
            json!({"workspace": workspace, "title": title, "cwd": cwd, "cmd": opt_cmd(cmd)}),
            )?
        }
        Cmd::Tab(TabCmd::Rename { tab, title }) => c.call("tab.rename", json!({"tab": tab, "title": title}))?,
        Cmd::Tab(TabCmd::Focus { tab }) => c.call("tab.focus", json!({"tab": tab}))?,
        Cmd::Tab(TabCmd::Close { tab }) => c.call("tab.close", json!({"tab": tab}))?,
        Cmd::Pane(PaneCmd::Open { down, tab, of, in_tab, title, cwd, cmd }) => {
            let cwd = absolutize(cwd)?;
            let placement = if tab { "tab" } else if down { "down" } else { "right" };
            c.call(
                "pane.open",
                json!({"placement": placement, "of": of, "tab": in_tab, "title": title, "cwd": cwd, "cmd": opt_cmd(cmd)}),
            )?
        }
        Cmd::Pane(PaneCmd::Read { pane, scrollback, lines }) => {
            let what = if scrollback { "scrollback" } else { "screen" };
            let t: PaneText = c.call("pane.read", json!({"pane": pane, "what": what, "lines": lines}))?;
            println!("{}", t.lines.join("\n"));
            return Ok(());
        }
        Cmd::Pane(PaneCmd::Send { pane, text, keys }) => {
            c.call("pane.send", json!({"pane": pane, "text": text, "keys": keys}))?
        }
        Cmd::Pane(PaneCmd::Wait { pane, exit, pattern, quiet, timeout }) => {
            let until = match (exit, pattern, quiet) {
                (_, Some(p), _) => json!({"match": p}),
                (_, _, Some(ms)) => json!({"quiet": ms}),
                _ => json!("exit"),
            };
            c.call("pane.wait", json!({"pane": pane, "until": until, "timeout_ms": timeout}))?
        }
        Cmd::Pane(PaneCmd::Focus { pane }) => c.call("pane.focus", json!({"pane": pane}))?,
        Cmd::Pane(PaneCmd::Close { pane }) => c.call("pane.close", json!({"pane": pane}))?,
        Cmd::Status(StatusCmd::Set { state, label, progress }) => {
            c.call("status.set", json!({"state": state, "label": label, "progress": progress}))?
        }
    };
    println!("{}", serde_json::to_string_pretty(&v).unwrap());
    Ok(())
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            match e {
                ClientError::Rpc(r) if r.kind() == Some(ErrorKind::Timeout) => ExitCode::from(124),
                _ => ExitCode::from(1),
            }
        }
    }
}

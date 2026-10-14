use std::collections::HashMap;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sitty_core::Size;
use sitty_proto::{
    Direction, ErrorKind, HUMAN, Hello, HelloResult, PROTOCOL_VERSION, PaneOpen, PaneOpened, PaneRead, PaneRef,
    PaneSend, PaneText, Placement, Principal, ReadWhat, Request, Response, Roster, RosterQuery, RpcError, StatusSet,
    TabOpen, TabOpened, TabRef, TabRename, WorkspaceOpen, WorkspaceOpened, paths,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::keys::key_bytes;
use crate::model::{Model, NewPane};
use crate::pane::{PaneRuntime, SpawnSpec};

const DEFAULT_SIZE: Size = Size { cols: 80, rows: 24 };

pub struct State {
    pub model: Model,
    pub panes: HashMap<String, Arc<PaneRuntime>>,
}

pub struct Daemon {
    pub state: Mutex<State>,
    pub socket: PathBuf,
    human_token: String,
}

fn rpc(kind: ErrorKind, detail: impl Into<String>) -> RpcError {
    RpcError::new(kind, detail)
}

fn params<T: DeserializeOwned>(v: Value) -> Result<T, RpcError> {
    let v = if v.is_null() { json!({}) } else { v };
    serde_json::from_value(v).map_err(|e| rpc(ErrorKind::InvalidParams, e.to_string()))
}

/// Daemon-side paths must be absolute: a relative one would resolve against the daemon's cwd.
fn absolute_param(name: &str, p: &str) -> Result<PathBuf, RpcError> {
    let path = PathBuf::from(p);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(rpc(ErrorKind::InvalidParams, format!("{name} must be an absolute path: {p}")))
    }
}

fn to_value<T: serde::Serialize>(t: T) -> Result<Value, RpcError> {
    serde_json::to_value(t).map_err(|e| rpc(ErrorKind::Internal, e.to_string()))
}

/// Replace `path` with a fresh file created 0600 (never briefly wider).
fn write_private(path: &Path, contents: &str) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(contents.as_bytes())
}

fn random_token() -> io::Result<String> {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Prepare the directory holding the socket. A missing dir is created 0700. An existing dir is only
/// chmodded if it is the default `sitty-$UID` dir; any other existing dir must already be ours and not
/// group/world-writable (we never chmod a user-chosen dir such as $HOME or /tmp).
fn prepare_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    if !dir.exists() {
        return std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir);
    }
    let uid = unsafe { libc::getuid() };
    let is_default = dir.file_name().is_some_and(|n| n == format!("sitty-{uid}").as_str());
    if is_default {
        return std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let meta = std::fs::metadata(dir)?;
    if meta.uid() != uid || meta.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} must be owned by you and not group/world-writable", dir.display()),
        ));
    }
    Ok(())
}

/// Prepare the socket dir, refuse if a live daemon answers, clear a stale socket, bind (0600).
pub fn prepare_socket(path: &Path) -> io::Result<std::os::unix::net::UnixListener> {
    prepare_socket_with(path, || Ok(()))
}

/// Exclusive advisory lock on `sittyd.lock` next to the socket. Held by the starting daemon for the
/// whole probe -> write token/pid -> bind sequence so racing daemons run that sequence one at a time:
/// a loser then sees the winner's live socket (AddrInUse) instead of clobbering its token/pid files.
/// It is released as soon as the socket is bound and listening (std's `bind` also listens), not held for
/// the daemon's lifetime: a later daemon must reach its liveness probe and exit 2 rather than block.
fn lock_startup(path: &Path) -> io::Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path.with_file_name("sittyd.lock"))?;
    loop {
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(f);
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// Like `prepare_socket`, running `before_bind` after the liveness check and just before binding, so
/// files that clients look for next to the socket exist by the time the socket does.
fn prepare_socket_with(
    path: &Path,
    before_bind: impl FnOnce() -> io::Result<()>,
) -> io::Result<std::os::unix::net::UnixListener> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        prepare_dir(dir)?;
    }
    let _startup_lock = lock_startup(path)?; // released on return, after bind
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        // Never delete something that is not a socket (SITTY_SOCKET may point at a user's file).
        if !std::os::unix::fs::FileTypeExt::is_socket(&meta.file_type()) {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("{} exists and is not a socket; refusing to replace it", path.display()),
            ));
        }
        match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => return Err(io::Error::new(io::ErrorKind::AddrInUse, "a sittyd is already running on this socket")),
            // Only a refused/vanished socket is stale; anything else (e.g. EACCES) is a real error.
            Err(e) if matches!(e.kind(), io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound) => {
                match std::fs::remove_file(path) {
                    Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                    _ => {}
                }
            }
            Err(e) => return Err(e),
        }
    }
    before_bind()?;
    let l = std::os::unix::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

pub async fn run(path: PathBuf) -> io::Result<()> {
    let human_token = random_token()?;
    let std_listener = prepare_socket_with(&path, || {
        write_private(&paths::human_token_path(&path), &human_token)?;
        write_private(&path.with_file_name("sittyd.pid"), &std::process::id().to_string())
    })?;
    std_listener.set_nonblocking(true)?;
    let listener = tokio::net::UnixListener::from_std(std_listener)?;

    let daemon = Arc::new(Daemon {
        state: Mutex::new(State { model: Model::default(), panes: HashMap::new() }),
        socket: path,
        human_token,
    });
    loop {
        let (stream, _) = listener.accept().await?;
        let d = Arc::clone(&daemon);
        tokio::spawn(async move {
            match d.serve(stream).await {
                // A peer vanishing mid-request is routine.
                Err(e) if !matches!(e.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset) => {
                    eprintln!("sittyd: connection ended: {e}");
                }
                _ => {}
            }
        });
    }
}

impl Daemon {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    async fn serve(self: Arc<Self>, stream: UnixStream) -> io::Result<()> {
        let (r, mut w) = stream.into_split();
        let mut lines = BufReader::new(r).lines();
        let mut principal: Option<Principal> = None;
        while let Some(line) = lines.next_line().await? {
            let req: Request = match serde_json::from_str(&line) {
                Ok(r) => r,
                Err(e) => {
                    let resp = Response::err(0, rpc(ErrorKind::InvalidParams, format!("bad request: {e}")));
                    write_line(&mut w, &resp).await?;
                    continue;
                }
            };
            let id = req.id;
            let (resp, close) = match &principal {
                None => match self.hello(req) {
                    Ok((p, result)) => {
                        principal = Some(p);
                        (Response::ok(id, result), false)
                    }
                    Err(e) => (Response::err(id, e), true),
                },
                Some(p) => match self.dispatch(p, req).await {
                    Ok(v) => (Response::ok(id, v), false),
                    Err(e) => (Response::err(id, e), false),
                },
            };
            write_line(&mut w, &resp).await?;
            if close {
                break;
            }
        }
        Ok(())
    }

    fn hello(&self, req: Request) -> Result<(Principal, Value), RpcError> {
        if req.method != "hello" {
            return Err(rpc(ErrorKind::InvalidParams, "expected hello"));
        }
        let h: Hello = params(req.params)?;
        if h.protocol_version != PROTOCOL_VERSION {
            return Err(rpc(
                ErrorKind::ProtocolVersion,
                format!("daemon speaks {PROTOCOL_VERSION}, client speaks {}", h.protocol_version),
            ));
        }
        let principal = match h.token {
            None => Principal::External,
            Some(t) if t == self.human_token => Principal::Human,
            Some(t) => {
                let st = self.lock();
                let p = st.model.pane_by_token(&t).ok_or_else(|| rpc(ErrorKind::PermissionRequired, "unknown token"))?;
                let workspace = st.model.workspace_of_pane(&p.id).unwrap();
                Principal::Harness { pane: p.id.clone(), workspace }
            }
        };
        let result = to_value(HelloResult { protocol_version: PROTOCOL_VERSION, principal: principal.clone() })?;
        Ok((principal, result))
    }

    async fn dispatch(&self, who: &Principal, req: Request) -> Result<Value, RpcError> {
        if req.method == "pane.send" {
            // Check + runtime lookup under one lock; the (possibly blocking) write happens outside it.
            let (rt, bytes) = {
                let st = self.lock();
                Self::live(&st, who)?;
                Self::pane_send_prepare(&st, who, params(req.params)?)?
            };
            tokio::task::spawn_blocking(move || rt.write(&bytes))
                .await
                .map_err(|e| rpc(ErrorKind::Internal, e.to_string()))?
                .map_err(|e| rpc(ErrorKind::Internal, e.to_string()))?;
            return Ok(json!({}));
        }
        if req.method == "pane.wait" {
            // Resolve the runtime under one short-lived guard; the wait itself holds no locks.
            let w: sitty_proto::PaneWait = params(req.params)?;
            let rt = {
                let st = self.lock();
                Self::live(&st, who)?;
                st.panes.get(&w.pane).cloned().ok_or_else(|| rpc(ErrorKind::NoSuchPane, w.pane.clone()))?
            };
            return to_value(crate::wait::wait(rt, w.until, w.timeout_ms).await?);
        }
        self.dispatch_locked(who, &req.method, req.params)
    }

    /// Every other method runs inside one critical section: checks and effects see the same state.
    fn dispatch_locked(&self, who: &Principal, method: &str, p: Value) -> Result<Value, RpcError> {
        let mut st = self.lock();
        let st = &mut *st;
        if !matches!(method, "whoami" | "roster" | "pane.read") {
            Self::live(st, who)?;
        }
        match method {
            "whoami" => to_value(who),
            "roster" => Self::roster(st, params(p)?),
            "workspace.open" => Self::workspace_open(st, who, params(p)?),
            "tab.open" => self.tab_open(st, who, params(p)?),
            "tab.rename" => Self::tab_rename(st, who, params(p)?),
            "tab.focus" => Self::tab_focus(st, who, params(p)?),
            "tab.close" => Self::tab_close(st, who, params(p)?),
            "pane.open" => self.pane_open(st, who, params(p)?),
            "pane.read" => Self::pane_read(st, params(p)?),
            "pane.focus" => Self::pane_focus(st, who, params(p)?),
            "pane.close" => Self::pane_close(st, who, params(p)?),
            "status.set" => Self::status_set(st, who, params(p)?),
            m => Err(rpc(ErrorKind::MethodNotFound, m)),
        }
    }

    /// A harness connection is resolved once at hello; its pane may have been closed since.
    fn live(st: &State, who: &Principal) -> Result<(), RpcError> {
        match who {
            Principal::Harness { pane, .. } if st.model.pane(pane).is_none() => {
                Err(rpc(ErrorKind::PermissionRequired, "pane closed"))
            }
            _ => Ok(()),
        }
    }

    fn roster(st: &State, q: RosterQuery) -> Result<Value, RpcError> {
        let exit_of = |id: &str| st.panes.get(id).and_then(|r| r.io().exit_code);
        let workspaces = st.model.roster(q.workspace.as_deref(), exit_of)?;
        to_value(Roster { workspaces })
    }

    fn workspace_open(st: &mut State, who: &Principal, p: WorkspaceOpen) -> Result<Value, RpcError> {
        if *who != Principal::Human {
            return Err(rpc(ErrorKind::PermissionRequired, "only the human can open workspaces"));
        }
        let dir = p.dir.as_deref().map(|d| absolute_param("dir", d)).transpose()?.unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into()));
        let workspace = st.model.open_workspace(&p.name, dir);
        to_value(WorkspaceOpened { workspace })
    }

    /// Which workspace may `who` create things in? Harnesses: their own (others need a grant).
    fn target_workspace(who: &Principal, requested: Option<String>) -> Result<String, RpcError> {
        match (who, requested) {
            (Principal::External, _) => Err(rpc(ErrorKind::PermissionRequired, "external callers are read-only")),
            (Principal::Human, Some(w)) => Ok(w),
            (Principal::Human, None) => Err(rpc(ErrorKind::InvalidParams, "workspace is required")),
            (Principal::Harness { workspace, .. }, None) => Ok(workspace.clone()),
            (Principal::Harness { workspace, .. }, Some(w)) if &w == workspace => Ok(w),
            (Principal::Harness { .. }, Some(w)) => {
                Err(rpc(ErrorKind::PermissionRequired, format!("workspace {w} is not yours")))
            }
        }
    }

    fn owner_of(who: &Principal) -> String {
        match who {
            Principal::Harness { pane, .. } => pane.clone(),
            _ => HUMAN.into(),
        }
    }

    /// Spawn a PTY and register it. `split` decides placement within the tab; the caller holds the state
    /// lock throughout so the model never holds a tab without a pane.
    #[allow(clippy::too_many_arguments)]
    fn spawn_pane(
        &self,
        st: &mut State,
        tab: &str,
        split: Option<(&str, Direction)>,
        owner: String,
        title: Option<String>,
        cmd: Option<Vec<String>>,
        cwd: Option<String>,
    ) -> Result<String, RpcError> {
        let ws = st.model.tab(tab).ok_or_else(|| rpc(ErrorKind::NoSuchTab, tab))?.workspace.clone();
        let cwd = cwd
            .as_deref()
            .map(|c| absolute_param("cwd", c))
            .transpose()?
            .unwrap_or_else(|| st.model.workspace(&ws).unwrap().dir.clone());
        let token = random_token().map_err(|e| rpc(ErrorKind::Internal, e.to_string()))?;
        // Reserve the id first so the env can carry it.
        let id = st.model.add_pane(
            tab,
            split,
            NewPane { owner, title: title.clone().unwrap_or_default(), cmd: vec![], cwd: cwd.clone(), token: token.clone() },
        )?;
        let spec = SpawnSpec {
            cmd,
            cwd,
            env: vec![
                ("SITTY_PANE".into(), id.clone()),
                ("SITTY_TOKEN".into(), token),
                ("SITTY_SOCKET".into(), self.socket.display().to_string()),
            ],
            size: DEFAULT_SIZE,
        };
        match PaneRuntime::spawn(spec) {
            Ok(rt) => {
                st.model.set_cmd(&id, rt.cmd().to_vec(), title.unwrap_or_else(|| rt.cmd()[0].clone()));
                st.panes.insert(id.clone(), rt);
                Ok(id)
            }
            Err(e) => {
                let _ = st.model.remove_pane(&id);
                Err(rpc(ErrorKind::Internal, format!("spawn failed: {e}")))
            }
        }
    }

    fn tab_open(&self, st: &mut State, who: &Principal, p: TabOpen) -> Result<Value, RpcError> {
        let ws = Self::target_workspace(who, p.workspace)?;
        let owner = Self::owner_of(who);
        let tab = st.model.add_tab(&ws, p.title.as_deref().unwrap_or("tab"), owner.clone())?;
        let pane = match self.spawn_pane(st, &tab, None, owner, None, p.cmd, p.cwd) {
            Ok(pane) => pane,
            Err(e) => {
                st.model.discard_empty_tab(&tab);
                return Err(e);
            }
        };
        to_value(TabOpened { tab, pane })
    }

    fn pane_open(&self, st: &mut State, who: &Principal, p: PaneOpen) -> Result<Value, RpcError> {
        if *who == Principal::External {
            return Err(rpc(ErrorKind::PermissionRequired, "external callers are read-only"));
        }
        let owner = Self::owner_of(who);
        // Split `of`; else append to an explicit `tab`; else (harness only) split the caller's own pane.
        let anchor = match (&p.of, &p.tab, who) {
            (Some(of), _, _) => Some(of.clone()),
            (None, Some(_), _) => None,
            (None, None, Principal::Harness { pane, .. }) => Some(pane.clone()),
            (None, None, _) => return Err(rpc(ErrorKind::InvalidParams, "need `of` or `tab`")),
        };
        let tab = match (&anchor, &p.tab) {
            (Some(of), _) => st.model.tab_of(of).ok_or_else(|| rpc(ErrorKind::NoSuchPane, of.clone()))?,
            (None, Some(t)) => t.clone(),
            (None, None) => unreachable!("rejected above"),
        };
        let ws = st.model.tab(&tab).ok_or_else(|| rpc(ErrorKind::NoSuchTab, tab.clone()))?.workspace.clone();
        Self::target_workspace(who, Some(ws.clone()))?;

        let (tab, pane) = match p.placement {
            Placement::Tab => {
                let t = st.model.add_tab(&ws, p.title.as_deref().unwrap_or("tab"), owner.clone())?;
                let pane = match self.spawn_pane(st, &t, None, owner, p.title, p.cmd, p.cwd) {
                    Ok(pane) => pane,
                    Err(e) => {
                        st.model.discard_empty_tab(&t);
                        return Err(e);
                    }
                };
                (t, pane)
            }
            placement => {
                let dir = if placement == Placement::Down { Direction::Down } else { Direction::Right };
                let split = anchor.as_deref().map(|of| (of, dir));
                let pane = self.spawn_pane(st, &tab, split, owner, p.title, p.cmd, p.cwd)?;
                (tab, pane)
            }
        };
        to_value(PaneOpened { pane, tab })
    }

    fn check_pane(st: &State, who: &Principal, pane: &str) -> Result<(), RpcError> {
        if st.model.pane(pane).is_none() {
            return Err(rpc(ErrorKind::NoSuchPane, pane));
        }
        if !st.model.can_control_pane(who, pane) {
            return Err(rpc(ErrorKind::PermissionRequired, format!("you do not control {pane}")));
        }
        Ok(())
    }

    fn check_tab(st: &State, who: &Principal, tab: &str, all_panes: bool) -> Result<(), RpcError> {
        if st.model.tab(tab).is_none() {
            return Err(rpc(ErrorKind::NoSuchTab, tab));
        }
        let ok = if all_panes { st.model.owns_all_panes(who, tab) } else { st.model.can_control_tab(who, tab) };
        if !ok {
            return Err(rpc(ErrorKind::PermissionRequired, format!("you do not control {tab}")));
        }
        Ok(())
    }

    fn pane_read(st: &State, p: PaneRead) -> Result<Value, RpcError> {
        let rt = st.panes.get(&p.pane).ok_or_else(|| rpc(ErrorKind::NoSuchPane, p.pane.clone()))?;
        let io = rt.io();
        let max = p.lines.unwrap_or(usize::MAX);
        let lines = match p.what {
            ReadWhat::Screen => {
                let all = io.term.screen_lines();
                let skip = all.len().saturating_sub(max);
                all.into_iter().skip(skip).collect()
            }
            ReadWhat::Scrollback => io.term.scrollback_lines(max),
        };
        let c = io.term.cursor();
        to_value(PaneText { lines, cursor: [c.row, c.col] })
    }

    /// Validate and resolve everything `pane.send` needs; the caller performs the write outside the lock.
    fn pane_send_prepare(st: &State, who: &Principal, p: PaneSend) -> Result<(Arc<PaneRuntime>, Vec<u8>), RpcError> {
        Self::check_pane(st, who, &p.pane)?;
        let mut bytes = p.text.unwrap_or_default().into_bytes();
        for k in p.keys.unwrap_or_default() {
            bytes.extend(key_bytes(&k).ok_or_else(|| rpc(ErrorKind::InvalidParams, format!("unknown key {k}")))?);
        }
        let rt = st.panes.get(&p.pane).cloned().ok_or_else(|| rpc(ErrorKind::NoSuchPane, p.pane.clone()))?;
        if rt.io().exit_code.is_some() {
            return Err(rpc(ErrorKind::PaneExited, p.pane));
        }
        Ok((rt, bytes))
    }

    fn pane_focus(st: &mut State, who: &Principal, p: PaneRef) -> Result<Value, RpcError> {
        Self::check_pane(st, who, &p.pane)?;
        st.model.focus_pane(&p.pane)?;
        Ok(json!({}))
    }

    fn close_pane(st: &mut State, pane: &str) -> Result<(), RpcError> {
        if let Some(rt) = st.panes.remove(pane) {
            rt.kill();
        }
        st.model.remove_pane(pane)
    }

    fn pane_close(st: &mut State, who: &Principal, p: PaneRef) -> Result<Value, RpcError> {
        Self::check_pane(st, who, &p.pane)?;
        Self::close_pane(st, &p.pane)?;
        Ok(json!({}))
    }

    fn tab_rename(st: &mut State, who: &Principal, p: TabRename) -> Result<Value, RpcError> {
        Self::check_tab(st, who, &p.tab, false)?;
        st.model.rename_tab(&p.tab, &p.title)?;
        Ok(json!({}))
    }

    fn tab_focus(st: &mut State, who: &Principal, p: TabRef) -> Result<Value, RpcError> {
        Self::check_tab(st, who, &p.tab, false)?;
        st.model.focus_tab(&p.tab)?;
        Ok(json!({}))
    }

    fn tab_close(st: &mut State, who: &Principal, p: TabRef) -> Result<Value, RpcError> {
        Self::check_tab(st, who, &p.tab, true)?;
        for pane in st.model.panes_in_tab(&p.tab) {
            Self::close_pane(st, &pane)?;
        }
        Ok(json!({}))
    }

    fn status_set(st: &mut State, who: &Principal, p: StatusSet) -> Result<Value, RpcError> {
        let Principal::Harness { pane, .. } = who else {
            return Err(rpc(ErrorKind::PermissionRequired, "status is set by a harness for its own pane"));
        };
        st.model.set_status(pane, p)?;
        Ok(json!({}))
    }
}

async fn write_line(w: &mut tokio::net::unix::OwnedWriteHalf, resp: &Response) -> io::Result<()> {
    let mut s = serde_json::to_string(resp).map_err(io::Error::other)?;
    s.push('\n');
    w.write_all(s.as_bytes()).await
}

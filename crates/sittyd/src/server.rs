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

fn to_value<T: serde::Serialize>(t: T) -> Result<Value, RpcError> {
    serde_json::to_value(t).map_err(|e| rpc(ErrorKind::Internal, e.to_string()))
}

fn random_token() -> io::Result<String> {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Create the runtime dir (0700), refuse if a live daemon answers, clear a stale socket, bind (0600).
pub fn prepare_socket(path: &Path) -> io::Result<std::os::unix::net::UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return Err(io::Error::new(io::ErrorKind::AddrInUse, "a sittyd is already running on this socket"));
        }
        std::fs::remove_file(path)?;
    }
    let l = std::os::unix::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

pub async fn run(path: PathBuf) -> io::Result<()> {
    let std_listener = prepare_socket(&path)?;
    std_listener.set_nonblocking(true)?;
    let listener = tokio::net::UnixListener::from_std(std_listener)?;

    let human_token = random_token()?;
    let token_path = paths::human_token_path(&path);
    std::fs::write(&token_path, &human_token)?;
    std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600))?;
    std::fs::write(path.with_file_name("sittyd.pid"), std::process::id().to_string())?;

    let daemon = Arc::new(Daemon {
        state: Mutex::new(State { model: Model::default(), panes: HashMap::new() }),
        socket: path,
        human_token,
    });
    loop {
        let (stream, _) = listener.accept().await?;
        let d = Arc::clone(&daemon);
        tokio::spawn(async move {
            if let Err(e) = d.serve(stream).await {
                eprintln!("sittyd: connection ended: {e}");
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
        let p = req.params;
        match req.method.as_str() {
            "whoami" => to_value(who),
            "roster" => self.roster(params(p)?),
            "workspace.open" => self.workspace_open(who, params(p)?),
            "tab.open" => self.tab_open(who, params(p)?),
            "tab.rename" => self.tab_rename(who, params(p)?),
            "tab.focus" => self.tab_focus(who, params(p)?),
            "tab.close" => self.tab_close(who, params(p)?),
            "pane.open" => self.pane_open(who, params(p)?),
            "pane.read" => self.pane_read(params(p)?),
            "pane.send" => self.pane_send(who, params(p)?).await,
            "pane.focus" => self.pane_focus(who, params(p)?),
            "pane.close" => self.pane_close(who, params(p)?),
            "status.set" => self.status_set(who, params(p)?),
            m => Err(rpc(ErrorKind::MethodNotFound, m)),
        }
    }

    fn runtime(&self, pane: &str) -> Result<Arc<PaneRuntime>, RpcError> {
        self.lock().panes.get(pane).cloned().ok_or_else(|| rpc(ErrorKind::NoSuchPane, pane))
    }

    fn roster(&self, q: RosterQuery) -> Result<Value, RpcError> {
        let st = self.lock();
        let exit_of = |id: &str| st.panes.get(id).and_then(|r| r.io().exit_code);
        let workspaces = st.model.roster(q.workspace.as_deref(), exit_of)?;
        to_value(Roster { workspaces })
    }

    fn workspace_open(&self, who: &Principal, p: WorkspaceOpen) -> Result<Value, RpcError> {
        if *who != Principal::Human {
            return Err(rpc(ErrorKind::PermissionRequired, "only the human can open workspaces"));
        }
        let dir = p.dir.map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into()));
        let workspace = self.lock().model.open_workspace(&p.name, dir);
        to_value(WorkspaceOpened { workspace })
    }

    /// Which workspace may `who` create things in? Harnesses: their own (others need a grant).
    fn target_workspace(&self, who: &Principal, requested: Option<String>) -> Result<String, RpcError> {
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

    /// Spawn a PTY and register it. `split` decides placement within the tab; runs under one lock so the
    /// model never holds a tab without a pane.
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
        let cwd = cwd.map(PathBuf::from).unwrap_or_else(|| st.model.workspace(&ws).unwrap().dir.clone());
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

    fn tab_open(&self, who: &Principal, p: TabOpen) -> Result<Value, RpcError> {
        let ws = self.target_workspace(who, p.workspace)?;
        let mut st = self.lock();
        let owner = Self::owner_of(who);
        let tab = st.model.add_tab(&ws, p.title.as_deref().unwrap_or("tab"), owner.clone())?;
        let pane = match self.spawn_pane(&mut st, &tab, None, owner, None, p.cmd, p.cwd) {
            Ok(pane) => pane,
            Err(e) => {
                st.model.discard_empty_tab(&tab);
                return Err(e);
            }
        };
        to_value(TabOpened { tab, pane })
    }

    fn pane_open(&self, who: &Principal, p: PaneOpen) -> Result<Value, RpcError> {
        if *who == Principal::External {
            return Err(rpc(ErrorKind::PermissionRequired, "external callers are read-only"));
        }
        let owner = Self::owner_of(who);
        let mut st = self.lock();
        let anchor = p.of.clone().or_else(|| match who {
            Principal::Harness { pane, .. } => Some(pane.clone()),
            _ => None,
        });
        // Resolve the tab we are adding to (or the workspace for a new tab).
        let tab = match (&anchor, &p.tab) {
            (Some(of), _) => st.model.tab_of(of).ok_or_else(|| rpc(ErrorKind::NoSuchPane, of.clone()))?,
            (None, Some(t)) => t.clone(),
            (None, None) => return Err(rpc(ErrorKind::InvalidParams, "need `of` or `tab`")),
        };
        let ws = st.model.tab(&tab).ok_or_else(|| rpc(ErrorKind::NoSuchTab, tab.clone()))?.workspace.clone();
        self.target_workspace(who, Some(ws.clone()))?; // pure check, takes no lock

        let (tab, pane) = match p.placement {
            Placement::Tab => {
                let t = st.model.add_tab(&ws, p.title.as_deref().unwrap_or("tab"), owner.clone())?;
                let pane = match self.spawn_pane(&mut st, &t, None, owner, p.title, p.cmd, p.cwd) {
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
                let pane = self.spawn_pane(&mut st, &tab, split, owner, p.title, p.cmd, p.cwd)?;
                (tab, pane)
            }
        };
        to_value(PaneOpened { pane, tab })
    }

    fn check_pane(&self, who: &Principal, pane: &str) -> Result<(), RpcError> {
        let st = self.lock();
        if st.model.pane(pane).is_none() {
            return Err(rpc(ErrorKind::NoSuchPane, pane));
        }
        if !st.model.can_control_pane(who, pane) {
            return Err(rpc(ErrorKind::PermissionRequired, format!("you do not control {pane}")));
        }
        Ok(())
    }

    fn check_tab(&self, who: &Principal, tab: &str, all_panes: bool) -> Result<(), RpcError> {
        let st = self.lock();
        if st.model.tab(tab).is_none() {
            return Err(rpc(ErrorKind::NoSuchTab, tab));
        }
        let ok = if all_panes { st.model.owns_all_panes(who, tab) } else { st.model.can_control_tab(who, tab) };
        if !ok {
            return Err(rpc(ErrorKind::PermissionRequired, format!("you do not control {tab}")));
        }
        Ok(())
    }

    fn pane_read(&self, p: PaneRead) -> Result<Value, RpcError> {
        let rt = self.runtime(&p.pane)?;
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

    async fn pane_send(&self, who: &Principal, p: PaneSend) -> Result<Value, RpcError> {
        self.check_pane(who, &p.pane)?;
        let mut bytes = p.text.unwrap_or_default().into_bytes();
        for k in p.keys.unwrap_or_default() {
            bytes.extend(key_bytes(&k).ok_or_else(|| rpc(ErrorKind::InvalidParams, format!("unknown key {k}")))?);
        }
        let rt = self.runtime(&p.pane)?;
        if rt.io().exit_code.is_some() {
            return Err(rpc(ErrorKind::PaneExited, p.pane));
        }
        // The child may stop reading stdin; keep the blocking write off the async workers (no locks held).
        tokio::task::spawn_blocking(move || rt.write(&bytes))
            .await
            .map_err(|e| rpc(ErrorKind::Internal, e.to_string()))?
            .map_err(|e| rpc(ErrorKind::Internal, e.to_string()))?;
        Ok(json!({}))
    }

    fn pane_focus(&self, who: &Principal, p: PaneRef) -> Result<Value, RpcError> {
        self.check_pane(who, &p.pane)?;
        self.lock().model.focus_pane(&p.pane)?;
        Ok(json!({}))
    }

    fn close_pane(&self, st: &mut State, pane: &str) -> Result<(), RpcError> {
        if let Some(rt) = st.panes.remove(pane) {
            rt.kill();
        }
        st.model.remove_pane(pane)
    }

    fn pane_close(&self, who: &Principal, p: PaneRef) -> Result<Value, RpcError> {
        self.check_pane(who, &p.pane)?;
        let mut st = self.lock();
        self.close_pane(&mut st, &p.pane)?;
        Ok(json!({}))
    }

    fn tab_rename(&self, who: &Principal, p: TabRename) -> Result<Value, RpcError> {
        self.check_tab(who, &p.tab, false)?;
        self.lock().model.rename_tab(&p.tab, &p.title)?;
        Ok(json!({}))
    }

    fn tab_focus(&self, who: &Principal, p: TabRef) -> Result<Value, RpcError> {
        self.check_tab(who, &p.tab, false)?;
        self.lock().model.focus_tab(&p.tab)?;
        Ok(json!({}))
    }

    fn tab_close(&self, who: &Principal, p: TabRef) -> Result<Value, RpcError> {
        self.check_tab(who, &p.tab, true)?;
        let mut st = self.lock();
        for pane in st.model.panes_in_tab(&p.tab) {
            self.close_pane(&mut st, &pane)?;
        }
        Ok(json!({}))
    }

    fn status_set(&self, who: &Principal, p: StatusSet) -> Result<Value, RpcError> {
        let Principal::Harness { pane, .. } = who else {
            return Err(rpc(ErrorKind::PermissionRequired, "status is set by a harness for its own pane"));
        };
        self.lock().model.set_status(pane, p)?;
        Ok(json!({}))
    }
}

async fn write_line(w: &mut tokio::net::unix::OwnedWriteHalf, resp: &Response) -> io::Result<()> {
    let mut s = serde_json::to_string(resp).map_err(io::Error::other)?;
    s.push('\n');
    w.write_all(s.as_bytes()).await
}

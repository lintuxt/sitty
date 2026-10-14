use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Request {
    pub fn new(id: u64, method: &str, params: Value) -> Self {
        Self { jsonrpc: "2.0".into(), id, method: method.into(), params }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Self {
        Self { jsonrpc: "2.0".into(), id, result: Some(result), error: None }
    }
    pub fn err(id: u64, error: RpcError) -> Self {
        Self { jsonrpc: "2.0".into(), id, result: None, error: Some(error) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    PermissionRequired,
    NoSuchPane,
    NoSuchTab,
    NoSuchWorkspace,
    InvalidParams,
    Timeout,
    PaneExited,
    ProtocolVersion,
    MethodNotFound,
    Internal,
}

const KINDS: [ErrorKind; 10] = [
    ErrorKind::PermissionRequired, ErrorKind::NoSuchPane, ErrorKind::NoSuchTab,
    ErrorKind::NoSuchWorkspace, ErrorKind::InvalidParams, ErrorKind::Timeout,
    ErrorKind::PaneExited, ErrorKind::ProtocolVersion, ErrorKind::MethodNotFound,
    ErrorKind::Internal,
];

impl ErrorKind {
    pub fn code(self) -> i64 {
        match self {
            ErrorKind::PermissionRequired => -32001,
            ErrorKind::NoSuchPane => -32002,
            ErrorKind::NoSuchTab => -32003,
            ErrorKind::NoSuchWorkspace => -32004,
            ErrorKind::Timeout => -32005,
            ErrorKind::PaneExited => -32006,
            ErrorKind::ProtocolVersion => -32007,
            ErrorKind::InvalidParams => -32602,
            ErrorKind::MethodNotFound => -32601,
            ErrorKind::Internal => -32603,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ErrorKind::PermissionRequired => "PERMISSION_REQUIRED",
            ErrorKind::NoSuchPane => "NO_SUCH_PANE",
            ErrorKind::NoSuchTab => "NO_SUCH_TAB",
            ErrorKind::NoSuchWorkspace => "NO_SUCH_WORKSPACE",
            ErrorKind::InvalidParams => "INVALID_PARAMS",
            ErrorKind::Timeout => "TIMEOUT",
            ErrorKind::PaneExited => "PANE_EXITED",
            ErrorKind::ProtocolVersion => "PROTOCOL_VERSION",
            ErrorKind::MethodNotFound => "METHOD_NOT_FOUND",
            ErrorKind::Internal => "INTERNAL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self { code: kind.code(), message: format!("{}: {}", kind.name(), detail.into()) }
    }
    pub fn kind(&self) -> Option<ErrorKind> {
        KINDS.into_iter().find(|k| k.code() == self.code)
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RpcError {}

/// `"human"` or a pane id.
pub type Owner = String;
pub const HUMAN: &str = "human";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Principal {
    Human,
    External,
    Harness { pane: String, workspace: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Empty {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u32,
    pub client: String,
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloResult {
    pub protocol_version: u32,
    pub principal: Principal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceOpen {
    pub name: String,
    #[serde(default)]
    pub dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceOpened {
    pub workspace: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TabOpen {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub cmd: Option<Vec<String>>,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabOpened {
    pub tab: String,
    pub pane: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabRef {
    pub tab: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabRename {
    pub tab: String,
    pub title: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    #[default]
    Right,
    Down,
    Tab,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaneOpen {
    #[serde(default)]
    pub cmd: Option<Vec<String>>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub placement: Placement,
    /// Split this pane. Defaults to the caller's own pane for harnesses.
    #[serde(default)]
    pub of: Option<String>,
    /// For human/external callers without `of`: the tab to add to.
    #[serde(default)]
    pub tab: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneOpened {
    pub pane: String,
    pub tab: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneRef {
    pub pane: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadWhat {
    #[default]
    Screen,
    Scrollback,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneRead {
    pub pane: String,
    #[serde(default)]
    pub what: ReadWhat,
    #[serde(default)]
    pub lines: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneText {
    pub lines: Vec<String>,
    pub cursor: [u16; 2],
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaneSend {
    pub pane: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub keys: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Until {
    Exit,
    Match(String),
    Quiet(u64),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneWait {
    pub pane: String,
    pub until: Until,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitResult {
    /// "exit" | "match" | "quiet"
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneState {
    Working,
    Idle,
    Attention,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub state: PaneState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
}

/// `status.set` params are exactly a `Status` for the caller's own pane.
pub type StatusSet = Status;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Right,
    Down,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Layout {
    Pane { id: String },
    Split { direction: Direction, children: Vec<Layout> },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RosterQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Roster {
    pub workspaces: Vec<WorkspaceInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub id: String,
    pub name: String,
    pub dir: String,
    pub focused_tab: Option<String>,
    pub tabs: Vec<TabInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabInfo {
    pub id: String,
    pub title: String,
    pub owner: Owner,
    pub focused_pane: Option<String>,
    pub layout: Layout,
    pub panes: Vec<PaneInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneInfo {
    pub id: String,
    pub title: String,
    pub owner: Owner,
    pub cmd: Vec<String>,
    pub cwd: String,
    pub status: Option<Status>,
    pub alive: bool,
    pub exit_code: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn error_kind_codes_round_trip() {
        for k in [
            ErrorKind::PermissionRequired, ErrorKind::NoSuchPane, ErrorKind::NoSuchTab,
            ErrorKind::NoSuchWorkspace, ErrorKind::InvalidParams, ErrorKind::Timeout,
            ErrorKind::PaneExited, ErrorKind::ProtocolVersion, ErrorKind::MethodNotFound,
            ErrorKind::Internal,
        ] {
            let e = RpcError::new(k, "x");
            assert_eq!(e.kind(), Some(k));
            assert!(e.message.starts_with(k.name()));
        }
    }

    #[test]
    fn until_wire_format() {
        assert_eq!(serde_json::to_value(Until::Exit).unwrap(), json!("exit"));
        assert_eq!(serde_json::to_value(Until::Match("\\$ $".into())).unwrap(), json!({"match": "\\$ $"}));
        assert_eq!(serde_json::to_value(Until::Quiet(500)).unwrap(), json!({"quiet": 500}));
    }

    #[test]
    fn principal_wire_format() {
        let p = Principal::Harness { pane: "p3".into(), workspace: "w1".into() };
        assert_eq!(serde_json::to_value(&p).unwrap(), json!({"kind": "harness", "pane": "p3", "workspace": "w1"}));
        assert_eq!(serde_json::to_value(Principal::Human).unwrap(), json!({"kind": "human"}));
    }

    #[test]
    fn layout_wire_format() {
        let l = Layout::Split {
            direction: Direction::Right,
            children: vec![Layout::Pane { id: "p1".into() }, Layout::Pane { id: "p2".into() }],
        };
        assert_eq!(
            serde_json::to_value(&l).unwrap(),
            json!({"type": "split", "direction": "right", "children": [
                {"type": "pane", "id": "p1"}, {"type": "pane", "id": "p2"}]})
        );
    }

    #[test]
    fn pane_open_defaults() {
        let p: PaneOpen = serde_json::from_value(json!({})).unwrap();
        assert_eq!(p.placement, Placement::Right);
        assert!(p.cmd.is_none() && p.of.is_none() && p.tab.is_none());
    }

    #[test]
    fn response_omits_absent_fields() {
        let ok = serde_json::to_value(Response::ok(7, json!({"a": 1}))).unwrap();
        assert_eq!(ok, json!({"jsonrpc": "2.0", "id": 7, "result": {"a": 1}}));
    }
}

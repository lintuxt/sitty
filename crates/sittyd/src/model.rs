use std::collections::BTreeMap;
use std::path::PathBuf;

use sitty_proto::{
    Direction, ErrorKind, HUMAN, Layout, Owner, PaneInfo, Principal, RpcError, Status, TabInfo,
    WorkspaceInfo,
};

use crate::layout;

#[derive(Debug, Clone)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub dir: PathBuf,
    pub tabs: Vec<String>,
    pub focused_tab: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Tab {
    pub id: String,
    pub workspace: String,
    pub title: String,
    pub owner: Owner,
    pub layout: Option<Layout>,
    pub focused_pane: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PaneMeta {
    pub id: String,
    pub tab: String,
    pub owner: Owner,
    pub title: String,
    pub cmd: Vec<String>,
    pub cwd: PathBuf,
    pub token: String,
    pub status: Option<Status>,
}

pub struct NewPane {
    pub owner: Owner,
    pub title: String,
    pub cmd: Vec<String>,
    pub cwd: PathBuf,
    pub token: String,
}

#[derive(Default)]
pub struct Model {
    workspaces: BTreeMap<String, Workspace>,
    tabs: BTreeMap<String, Tab>,
    panes: BTreeMap<String, PaneMeta>,
    next: u64,
}

fn err(kind: ErrorKind, what: &str) -> RpcError {
    RpcError::new(kind, what)
}

impl Model {
    fn id(&mut self, prefix: char) -> String {
        self.next += 1;
        format!("{prefix}{}", self.next)
    }

    pub fn open_workspace(&mut self, name: &str, dir: PathBuf) -> String {
        if let Some(w) = self.workspaces.values().find(|w| w.name == name) {
            return w.id.clone();
        }
        let id = self.id('w');
        self.workspaces.insert(
            id.clone(),
            Workspace { id: id.clone(), name: name.into(), dir, tabs: vec![], focused_tab: None },
        );
        id
    }

    pub fn workspace(&self, id: &str) -> Option<&Workspace> {
        self.workspaces.get(id)
    }

    pub fn tab(&self, id: &str) -> Option<&Tab> {
        self.tabs.get(id)
    }

    pub fn pane(&self, id: &str) -> Option<&PaneMeta> {
        self.panes.get(id)
    }

    pub fn tab_of(&self, pane: &str) -> Option<String> {
        self.panes.get(pane).map(|p| p.tab.clone())
    }

    pub fn workspace_of_pane(&self, pane: &str) -> Option<String> {
        let tab = self.panes.get(pane)?.tab.clone();
        self.tabs.get(&tab).map(|t| t.workspace.clone())
    }

    pub fn pane_by_token(&self, token: &str) -> Option<&PaneMeta> {
        self.panes.values().find(|p| p.token == token)
    }

    pub fn panes_in_tab(&self, tab: &str) -> Vec<String> {
        self.tabs.get(tab).and_then(|t| t.layout.as_ref()).map(layout::leaves).unwrap_or_default()
    }

    pub fn add_tab(&mut self, ws: &str, title: &str, owner: Owner) -> Result<String, RpcError> {
        if !self.workspaces.contains_key(ws) {
            return Err(err(ErrorKind::NoSuchWorkspace, ws));
        }
        let id = self.id('t');
        self.tabs.insert(
            id.clone(),
            Tab { id: id.clone(), workspace: ws.into(), title: title.into(), owner, layout: None, focused_pane: None },
        );
        let w = self.workspaces.get_mut(ws).unwrap();
        w.tabs.push(id.clone());
        w.focused_tab = Some(id.clone());
        Ok(id)
    }

    pub fn add_pane(&mut self, tab: &str, split: Option<(&str, Direction)>, new: NewPane) -> Result<String, RpcError> {
        if !self.tabs.contains_key(tab) {
            return Err(err(ErrorKind::NoSuchTab, tab));
        }
        let id = self.id('p');
        let t = self.tabs.get_mut(tab).unwrap();
        if let Some(l) = t.layout.as_mut() {
            match split {
                Some((of, dir)) => {
                    if !layout::split(l, of, &id, dir) {
                        return Err(err(ErrorKind::NoSuchPane, of));
                    }
                }
                None => {
                    let last = layout::leaves(l).pop().unwrap();
                    layout::split(l, &last, &id, Direction::Right);
                }
            }
        } else {
            t.layout = Some(Layout::Pane { id: id.clone() });
        }
        t.focused_pane = Some(id.clone());
        self.panes.insert(
            id.clone(),
            PaneMeta {
                id: id.clone(),
                tab: tab.into(),
                owner: new.owner,
                title: new.title,
                cmd: new.cmd,
                cwd: new.cwd,
                token: new.token,
                status: None,
            },
        );
        Ok(id)
    }

    /// Removes the pane; its owned panes and tabs pass to the human; empty tabs disappear.
    pub fn remove_pane(&mut self, id: &str) -> Result<(), RpcError> {
        let meta = self.panes.remove(id).ok_or_else(|| err(ErrorKind::NoSuchPane, id))?;
        for p in self.panes.values_mut().filter(|p| p.owner == id) {
            p.owner = HUMAN.into();
        }
        for t in self.tabs.values_mut().filter(|t| t.owner == id) {
            t.owner = HUMAN.into();
        }
        let tab = self.tabs.get_mut(&meta.tab).unwrap();
        tab.layout = tab.layout.take().and_then(|l| layout::remove(l, id));
        if tab.focused_pane.as_deref() == Some(id) {
            tab.focused_pane = tab.layout.as_ref().and_then(|l| layout::leaves(l).pop());
        }
        if tab.layout.is_none() {
            let ws_id = tab.workspace.clone();
            self.tabs.remove(&meta.tab);
            let ws = self.workspaces.get_mut(&ws_id).unwrap();
            ws.tabs.retain(|t| t != &meta.tab);
            if ws.focused_tab.as_deref() == Some(meta.tab.as_str()) {
                ws.focused_tab = ws.tabs.last().cloned();
            }
        }
        Ok(())
    }

    /// True if `owner` is `me` or a pane descended from `me` via the owner chain.
    fn owned_by(&self, owner: &str, me: &str) -> bool {
        let mut cur = owner;
        loop {
            if cur == me {
                return true;
            }
            match self.panes.get(cur) {
                Some(p) => cur = &p.owner,
                None => return false,
            }
        }
    }

    pub fn can_control_pane(&self, who: &Principal, pane: &str) -> bool {
        match who {
            Principal::Human => true,
            Principal::External => false,
            Principal::Harness { pane: me, .. } => pane == me || self.owned_by(pane, me),
        }
    }

    pub fn can_control_tab(&self, who: &Principal, tab: &str) -> bool {
        match (who, self.tabs.get(tab)) {
            (Principal::Human, _) => true,
            (Principal::Harness { pane: me, .. }, Some(t)) => self.owned_by(&t.owner, me),
            _ => false,
        }
    }

    pub fn owns_all_panes(&self, who: &Principal, tab: &str) -> bool {
        let panes = self.panes_in_tab(tab);
        !panes.is_empty() && panes.iter().all(|p| self.can_control_pane(who, p))
    }

    /// Roll back a tab that never received its first pane. No-op if the tab has a layout.
    pub fn discard_empty_tab(&mut self, tab: &str) {
        if self.tabs.get(tab).is_none_or(|t| t.layout.is_some()) {
            return;
        }
        let t = self.tabs.remove(tab).unwrap();
        if let Some(w) = self.workspaces.get_mut(&t.workspace) {
            w.tabs.retain(|x| x != tab);
            if w.focused_tab.as_deref() == Some(tab) {
                w.focused_tab = w.tabs.last().cloned();
            }
        }
    }

    pub fn rename_tab(&mut self, tab: &str, title: &str) -> Result<(), RpcError> {
        let t = self.tabs.get_mut(tab).ok_or_else(|| err(ErrorKind::NoSuchTab, tab))?;
        t.title = title.into();
        Ok(())
    }

    pub fn focus_tab(&mut self, tab: &str) -> Result<(), RpcError> {
        let ws = self.tabs.get(tab).ok_or_else(|| err(ErrorKind::NoSuchTab, tab))?.workspace.clone();
        self.workspaces.get_mut(&ws).unwrap().focused_tab = Some(tab.into());
        Ok(())
    }

    pub fn focus_pane(&mut self, pane: &str) -> Result<(), RpcError> {
        let tab = self.tab_of(pane).ok_or_else(|| err(ErrorKind::NoSuchPane, pane))?;
        self.tabs.get_mut(&tab).unwrap().focused_pane = Some(pane.into());
        self.focus_tab(&tab)
    }

    pub fn set_status(&mut self, pane: &str, status: Status) -> Result<(), RpcError> {
        let p = self.panes.get_mut(pane).ok_or_else(|| err(ErrorKind::NoSuchPane, pane))?;
        p.status = Some(status);
        Ok(())
    }

    pub fn roster(&self, ws: Option<&str>, exit_of: impl Fn(&str) -> Option<i32>) -> Result<Vec<WorkspaceInfo>, RpcError> {
        if let Some(w) = ws
            && !self.workspaces.contains_key(w)
        {
            return Err(err(ErrorKind::NoSuchWorkspace, w));
        }
        Ok(self
            .workspaces
            .values()
            .filter(|w| ws.is_none_or(|id| id == w.id))
            .map(|w| WorkspaceInfo {
                id: w.id.clone(),
                name: w.name.clone(),
                dir: w.dir.display().to_string(),
                focused_tab: w.focused_tab.clone(),
                tabs: w
                    .tabs
                    .iter()
                    .filter_map(|t| self.tabs.get(t))
                    .filter_map(|t| Some((t, t.layout.clone()?)))
                    .map(|(t, layout)| TabInfo {
                        id: t.id.clone(),
                        title: t.title.clone(),
                        owner: t.owner.clone(),
                        focused_pane: t.focused_pane.clone(),
                        layout,
                        panes: self
                            .panes_in_tab(&t.id)
                            .iter()
                            .filter_map(|p| self.panes.get(p))
                            .map(|p| {
                                let exit_code = exit_of(&p.id);
                                PaneInfo {
                                    id: p.id.clone(),
                                    title: p.title.clone(),
                                    owner: p.owner.clone(),
                                    cmd: p.cmd.clone(),
                                    cwd: p.cwd.display().to_string(),
                                    status: p.status.clone(),
                                    alive: exit_code.is_none(),
                                    exit_code,
                                }
                            })
                            .collect(),
                    })
                    .collect(),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sitty_proto::PaneState;

    fn np(owner: &str, token: &str) -> NewPane {
        NewPane { owner: owner.into(), title: "t".into(), cmd: vec!["sh".into()], cwd: "/tmp".into(), token: token.into() }
    }

    fn harness(m: &Model, pane: &str) -> Principal {
        Principal::Harness { pane: pane.into(), workspace: m.workspace_of_pane(pane).unwrap() }
    }

    /// human opens w1 / t2 / p3 (claude); claude opens p4 (split); p4 opens p5.
    fn fixture() -> Model {
        let mut m = Model::default();
        let w = m.open_workspace("solcito", "/tmp".into());
        let t = m.add_tab(&w, "main", HUMAN.into()).unwrap();
        let p1 = m.add_pane(&t, None, np(HUMAN, "tok1")).unwrap();
        let p2 = m.add_pane(&t, Some((&p1, Direction::Right)), np(&p1, "tok2")).unwrap();
        m.add_pane(&t, Some((&p2, Direction::Down)), np(&p2, "tok3")).unwrap();
        m
    }

    #[test]
    fn ids_and_workspace_idempotence() {
        let mut m = fixture();
        // One counter for all ids: w1, t2, p3 (human's), p4 (claude's split), p5 (child of p4).
        assert_eq!(m.open_workspace("solcito", "/elsewhere".into()), "w1");
        assert_eq!(m.workspace_of_pane("p5").as_deref(), Some("w1"));
        assert_eq!(m.tab_of("p4").as_deref(), Some("t2"));
        assert_eq!(m.pane_by_token("tok2").unwrap().id, "p4");
        assert!(m.pane_by_token("nope").is_none());
    }

    #[test]
    fn ownership_follows_the_owner_chain() {
        let m = fixture();
        let p3 = harness(&m, "p3");
        assert!(m.can_control_pane(&p3, "p3"));
        assert!(m.can_control_pane(&p3, "p4"));
        assert!(m.can_control_pane(&p3, "p5"));
        let p4 = harness(&m, "p4");
        assert!(!m.can_control_pane(&p4, "p3"));
        assert!(m.can_control_pane(&Principal::Human, "p3"));
        assert!(!m.can_control_pane(&Principal::External, "p5"));
    }

    #[test]
    fn closing_a_pane_orphans_its_children_to_human() {
        let mut m = fixture();
        m.remove_pane("p4").unwrap();
        assert_eq!(m.pane("p5").unwrap().owner, HUMAN);
        assert!(m.pane("p4").is_none());
        assert_eq!(m.panes_in_tab("t2"), vec!["p3", "p5"]);
    }

    #[test]
    fn closing_last_pane_removes_tab() {
        let mut m = fixture();
        for p in ["p3", "p4", "p5"] {
            m.remove_pane(p).unwrap();
        }
        assert!(m.tab("t2").is_none());
        let r = m.roster(None, |_| None).unwrap();
        assert!(r[0].tabs.is_empty());
    }

    #[test]
    fn tab_ownership_rules() {
        let mut m = fixture();
        let t = m.add_tab("w1", "logs", "p4".into()).unwrap(); // t6
        m.add_pane(&t, None, np("p4", "tok7")).unwrap(); // p7
        let p3 = harness(&m, "p3");
        assert!(m.can_control_tab(&p3, &t)); // p4 is p3's descendant
        assert!(m.owns_all_panes(&p3, &t));
        assert!(!m.owns_all_panes(&harness(&m, "p4"), "t2")); // p3 belongs to human
    }

    #[test]
    fn roster_shape_and_errors() {
        let mut m = fixture();
        m.set_status("p4", Status { state: PaneState::Working, label: Some("build".into()), progress: None }).unwrap();
        let r = m.roster(Some("w1"), |id| if id == "p5" { Some(0) } else { None }).unwrap();
        let tab = &r[0].tabs[0];
        assert_eq!(tab.panes.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["p3", "p4", "p5"]);
        assert_eq!(tab.panes[1].status.as_ref().unwrap().label.as_deref(), Some("build"));
        assert!(!tab.panes[2].alive);
        assert_eq!(tab.panes[2].exit_code, Some(0));
        assert_eq!(m.roster(Some("w9"), |_| None).unwrap_err().kind(), Some(ErrorKind::NoSuchWorkspace));
        assert_eq!(m.add_tab("w9", "x", HUMAN.into()).unwrap_err().kind(), Some(ErrorKind::NoSuchWorkspace));
        assert_eq!(m.remove_pane("p99").unwrap_err().kind(), Some(ErrorKind::NoSuchPane));
    }

    #[test]
    fn roster_skips_layoutless_tab() {
        let mut m = fixture();
        m.add_tab("w1", "empty", HUMAN.into()).unwrap();
        let r = m.roster(None, |_| None).unwrap();
        assert_eq!(r[0].tabs.len(), 1);
        assert_eq!(r[0].tabs[0].id, "t2");
    }

    #[test]
    fn owns_all_panes_false_for_empty_or_missing_tab() {
        let mut m = fixture();
        let t = m.add_tab("w1", "empty", HUMAN.into()).unwrap();
        assert!(!m.owns_all_panes(&Principal::Human, &t));
        assert!(!m.owns_all_panes(&Principal::Human, "t99"));
        assert!(!m.owns_all_panes(&Principal::External, &t));
    }

    #[test]
    fn discard_empty_tab_only_removes_empty() {
        let mut m = fixture();
        let t = m.add_tab("w1", "empty", HUMAN.into()).unwrap();
        m.discard_empty_tab(&t);
        assert!(m.tab(&t).is_none());
        assert_eq!(m.workspace("w1").unwrap().tabs, vec!["t2"]);
        assert_eq!(m.workspace("w1").unwrap().focused_tab.as_deref(), Some("t2"));
        m.discard_empty_tab("t2");
        assert!(m.tab("t2").is_some());
    }

    #[test]
    fn can_control_tab_denials() {
        let mut m = fixture();
        let t = m.add_tab("w1", "logs", "p4".into()).unwrap();
        m.add_pane(&t, None, np("p4", "tokx")).unwrap();
        assert!(!m.can_control_tab(&harness(&m, "p5"), &t));
        assert!(!m.can_control_tab(&Principal::External, &t));
    }

    #[test]
    fn tab_owned_by_removed_pane_passes_to_human() {
        let mut m = fixture();
        let t = m.add_tab("w1", "logs", "p4".into()).unwrap();
        m.add_pane(&t, None, np(HUMAN, "tokx")).unwrap();
        m.remove_pane("p4").unwrap();
        assert_eq!(m.tab(&t).unwrap().owner, HUMAN);
    }
}

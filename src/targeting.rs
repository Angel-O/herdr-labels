//! Snapshot-derived tab and pane targeting.

use std::collections::HashMap;

use crate::config::Invocation;
use crate::herdr::{SessionSnapshot, SessionTab};
use crate::state::State;

pub(super) fn naming_pane<'a>(snapshot: &'a SessionSnapshot, tab: &SessionTab) -> Option<&'a str> {
    let panes = snapshot
        .panes
        .iter()
        .filter(|pane| pane.tab_id == tab.tab.tab_id);
    if tab.pane_count == 1 {
        return panes.map(|pane| pane.pane_id.as_str()).next();
    }
    if tab.focused {
        let focused = snapshot.focused_pane_id.as_deref()?;
        return panes
            .filter(|pane| pane.pane_id == focused)
            .map(|pane| pane.pane_id.as_str())
            .next();
    }
    None
}

pub(super) fn scoped_tabs<'a>(
    snapshot: &'a SessionSnapshot,
    invocation: &Invocation,
) -> Vec<&'a SessionTab> {
    if matches!(invocation, Invocation::ClosedPane { .. }) {
        return Vec::new();
    }
    if let Invocation::Init { pane_id, .. }
    | Invocation::Preexec { pane_id, .. }
    | Invocation::Precmd { pane_id, .. } = invocation
        && !snapshot.panes.iter().any(|pane| pane.pane_id == *pane_id)
    {
        return Vec::new();
    }
    let (workspace, tab) = match invocation {
        Invocation::Workspace(workspace_id)
        | Invocation::ClosedTab {
            workspace_id: Some(workspace_id),
            ..
        } => (Some(workspace_id.as_str()), None),
        Invocation::Tab {
            workspace_id,
            tab_id,
        }
        | Invocation::RenamedTab {
            workspace_id,
            tab_id,
        } => (Some(workspace_id.as_str()), Some(tab_id.as_str())),
        Invocation::Init { pane_id, .. }
        | Invocation::Preexec { pane_id, .. }
        | Invocation::Precmd { pane_id, .. } => (
            None,
            snapshot
                .panes
                .iter()
                .find(|pane| pane.pane_id == *pane_id)
                .map(|pane| pane.tab_id.as_str()),
        ),
        Invocation::Reset {
            workspace_id: Some(workspace_id),
            tab_id,
        }
        | Invocation::Toggle {
            workspace_id: Some(workspace_id),
            tab_id,
        } => (Some(workspace_id.as_str()), tab_id.as_deref()),
        _ => (None, None),
    };
    snapshot
        .tabs
        .iter()
        .filter(|candidate| {
            workspace.is_none_or(|id| candidate.tab.workspace_id == id)
                && tab.is_none_or(|id| candidate.tab.tab_id == id)
        })
        .collect()
}

pub(super) fn scoped_closed_pane_tabs<'a>(
    snapshot: &'a SessionSnapshot,
    invocation: &Invocation,
    tab_id: Option<&str>,
) -> Vec<&'a SessionTab> {
    let Invocation::ClosedPane { workspace_id, .. } = invocation else {
        return Vec::new();
    };
    let Some(tab_id) = tab_id else {
        return Vec::new();
    };
    snapshot
        .tabs
        .iter()
        .filter(|candidate| {
            candidate.tab.workspace_id == *workspace_id && candidate.tab.tab_id == tab_id
        })
        .collect()
}

pub(super) struct ClosedPaneResolution {
    pub(super) mapped_tab_id: Option<String>,
    pub(super) resolution: &'static str,
    pub(super) validation: &'static str,
    pub(super) valid: bool,
    pub(super) outcome: &'static str,
}

pub(super) fn resolve_closed_pane(
    state: &State,
    snapshot: &SessionSnapshot,
    workspace_id: &str,
    pane_id: &str,
) -> ClosedPaneResolution {
    let Some(mapped_tab_id) = state.pane_tab(pane_id).map(str::to_owned) else {
        return ClosedPaneResolution {
            mapped_tab_id: None,
            resolution: "missing",
            validation: "not_run",
            valid: false,
            outcome: "pane_mapping_missing",
        };
    };
    let Some(tab) = snapshot
        .tabs
        .iter()
        .find(|tab| tab.tab.tab_id == mapped_tab_id)
    else {
        return ClosedPaneResolution {
            mapped_tab_id: Some(mapped_tab_id),
            resolution: "resolved",
            validation: "tab_absent",
            valid: false,
            outcome: "pane_mapping_invalid",
        };
    };
    if tab.tab.workspace_id != workspace_id {
        return ClosedPaneResolution {
            mapped_tab_id: Some(mapped_tab_id),
            resolution: "resolved",
            validation: "workspace_mismatch",
            valid: false,
            outcome: "pane_mapping_invalid",
        };
    }
    if snapshot.panes.iter().any(|pane| pane.pane_id == pane_id) {
        return ClosedPaneResolution {
            mapped_tab_id: Some(mapped_tab_id),
            resolution: "resolved",
            validation: "pane_still_present",
            valid: false,
            outcome: "pane_mapping_invalid",
        };
    }
    ClosedPaneResolution {
        mapped_tab_id: Some(mapped_tab_id),
        resolution: "resolved",
        validation: "valid",
        valid: true,
        outcome: "pane_mapping_consumed",
    }
}

pub(super) fn pane_targets_tab(snapshot: &SessionSnapshot, pane_id: &str, tab_id: &str) -> bool {
    snapshot
        .panes
        .iter()
        .any(|pane| pane.pane_id == pane_id && pane.tab_id == tab_id)
}

pub(super) fn tab_positions(snapshot: &SessionSnapshot) -> HashMap<String, usize> {
    let mut positions = HashMap::<String, usize>::new();
    snapshot
        .tabs
        .iter()
        .map(|tab| {
            let position = positions.entry(tab.tab.workspace_id.clone()).or_default();
            *position += 1;
            (tab.tab.tab_id.clone(), *position)
        })
        .collect()
}

#[cfg(test)]
#[path = "../tests/unit/targeting.rs"]
mod tests;

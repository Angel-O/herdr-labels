//! Coordinates process-aware, race-conscious tab label reconciliation.

use std::collections::HashSet;
use std::error::Error;

use crate::config::{Config, Invocation};
use crate::herdr::{HerdrClient, PaneProcessInfo, SessionSnapshot};
use crate::naming::NamingPolicy;
use crate::numbering::{Tab, is_placeholder, strip_numeric_prefix};
use crate::process_selection::process_group_matches_program;
use crate::settings::Settings;
use crate::state::{State, TabOwnership};
use crate::tab_reconciliation::reconcile_tab;
use crate::targeting::{resolve_closed_pane, scoped_closed_pane_tabs, scoped_tabs};
use crate::telemetry::Telemetry;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub(crate) trait TabClient {
    fn snapshot(&mut self) -> Result<SessionSnapshot>;
    fn get_tab(&mut self, tab_id: &str) -> Result<Option<Tab>>;
    fn rename_tab(&mut self, tab_id: &str, label: &str) -> Result<()>;
    fn pane_process_info(&mut self, pane_id: &str) -> Result<PaneProcessInfo>;
}

impl TabClient for HerdrClient {
    fn snapshot(&mut self) -> Result<SessionSnapshot> {
        self.snapshot()
    }

    fn get_tab(&mut self, tab_id: &str) -> Result<Option<Tab>> {
        self.get_tab(tab_id)
    }

    fn rename_tab(&mut self, tab_id: &str, label: &str) -> Result<()> {
        self.rename_tab(tab_id, label)
    }

    fn pane_process_info(&mut self, pane_id: &str) -> Result<PaneProcessInfo> {
        self.pane_process_info(pane_id)
    }
}

pub(crate) fn run_pass(
    config: &Config,
    invocation: &Invocation,
    client: &mut impl TabClient,
    telemetry: &mut Telemetry,
) -> Result<()> {
    let mut state = State::load(&config.state_dir)?;
    match invocation {
        Invocation::Clear => return clear_session(client, &mut state),
        Invocation::Reset { tab_id, .. } => {
            state.set_suspended(false);
            if let Some(tab_id) = tab_id {
                state.set_ownership(tab_id, TabOwnership::ResetPending);
            }
            state.persist()?;
        }
        Invocation::Toggle { .. } => {
            state.set_suspended(false);
            state.persist()?;
        }
        _ if state.is_suspended() => {
            telemetry.set_terminal_outcome("suspended");
            return Ok(());
        }
        _ => {}
    }

    let snapshot = client.snapshot()?;
    let closed_resolution = match invocation {
        Invocation::ClosedPane {
            workspace_id,
            pane_id,
        } => Some(resolve_closed_pane(
            &state,
            &snapshot,
            workspace_id,
            pane_id,
        )),
        _ => None,
    };
    state.refresh_pane_tabs(
        snapshot
            .panes
            .iter()
            .map(|pane| (&pane.pane_id, &pane.tab_id)),
        snapshot.tabs.iter().map(|tab| &tab.tab.tab_id),
    );
    if let (Invocation::ClosedPane { pane_id, .. }, Some(resolution)) =
        (invocation, closed_resolution.as_ref())
    {
        telemetry.record_pane_mapping(
            pane_id,
            resolution.mapped_tab_id.as_deref(),
            resolution.resolution,
            resolution.validation,
            resolution.valid,
        );
        if !resolution.valid {
            telemetry.record_snapshot(&snapshot, &[]);
            telemetry.set_terminal_outcome(resolution.outcome);
            state.persist()?;
            return Ok(());
        }
        state.remove_pane_tab(pane_id);
    }
    recover_pending(&snapshot, &mut state);
    if let Invocation::Toggle {
        tab_id: Some(tab_id),
        ..
    } = invocation
    {
        toggle_ownership(&snapshot, &mut state, tab_id);
        state.persist()?;
    }
    let targets = if let Some(resolution) = closed_resolution.as_ref() {
        scoped_closed_pane_tabs(&snapshot, invocation, resolution.mapped_tab_id.as_deref())
    } else {
        scoped_tabs(&snapshot, invocation)
    };
    let target_ids = targets
        .iter()
        .map(|target| target.tab.tab_id.clone())
        .collect::<Vec<_>>();
    telemetry.record_snapshot(&snapshot, &target_ids);

    for session_tab in targets {
        let mut tab_telemetry = telemetry.new_tab_decision(
            &session_tab.tab,
            state.ownership(&session_tab.tab.tab_id).cloned(),
            session_tab.pane_count,
            session_tab.focused,
        );
        let result = reconcile_tab(
            client,
            &snapshot,
            session_tab,
            invocation,
            config,
            &mut state,
            &mut tab_telemetry,
        );
        telemetry.push_tab_decision(tab_telemetry);
        result?;
    }

    if matches!(invocation, Invocation::Full) {
        state.prune_tabs(snapshot.tabs.iter().map(|tab| &tab.tab.tab_id));
    }
    state.persist()?;
    Ok(())
}

/// Checks whether a pane's foreground process group contains the invoked program.
pub(crate) fn pane_matches_program(
    client: &mut impl TabClient,
    pane_id: &str,
    program: &str,
    settings: &Settings,
) -> Result<bool> {
    let process_info = client.pane_process_info(pane_id)?;
    Ok(process_group_matches_program(
        &process_info,
        program,
        &naming_policy(settings),
    ))
}

fn clear_session(client: &mut impl TabClient, state: &mut State) -> Result<()> {
    state.set_suspended(true);
    state.persist()?;
    let snapshot = client.snapshot()?;
    for session_tab in &snapshot.tabs {
        let current = &session_tab.tab;
        let desired = strip_numeric_prefix(&current.label);
        if desired == current.label {
            continue;
        }
        let Some(latest) = client.get_tab(&current.tab_id)? else {
            continue;
        };
        if latest.label == current.label {
            client.rename_tab(&current.tab_id, desired)?;
        }
    }
    state.prune_tabs(std::iter::empty::<&str>());
    state.persist()?;
    Ok(())
}

fn recover_pending(snapshot: &SessionSnapshot, state: &mut State) {
    for tab in &snapshot.tabs {
        state.resolve_pending_rename(&tab.tab.tab_id, &tab.tab.label);
    }
}

fn toggle_ownership(snapshot: &SessionSnapshot, state: &mut State, tab_id: &str) {
    let Some(tab) = snapshot.tabs.iter().find(|tab| tab.tab.tab_id == tab_id) else {
        return;
    };
    let enable = match state.ownership(tab_id) {
        Some(TabOwnership::Manual | TabOwnership::AutomaticDisabled) => true,
        Some(_) => false,
        None => !is_placeholder(strip_numeric_prefix(&tab.tab.label)),
    };
    state.set_ownership(
        tab_id,
        if enable {
            TabOwnership::ResetPending
        } else {
            TabOwnership::AutomaticDisabled
        },
    );
}

pub(crate) fn naming_policy(settings: &Settings) -> NamingPolicy {
    NamingPolicy {
        hide_idle_shell: settings.hide_idle_shell,
        max_label_chars: settings.max_label_chars,
        shells: settings.shells.iter().cloned().collect::<HashSet<_>>(),
        ignored_processes: settings
            .ignored_processes
            .iter()
            .cloned()
            .collect::<HashSet<_>>(),
        aliases: settings.process_aliases.clone(),
    }
}

#[cfg(test)]
#[path = "../tests/unit/reconciliation.rs"]
mod tests;

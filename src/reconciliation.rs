//! Coordinates process-aware, race-conscious tab label reconciliation.

use std::collections::{HashMap, HashSet};
use std::error::Error;

use crate::config::{Config, Invocation};
use crate::herdr::{HerdrClient, PaneProcessInfo, SessionSnapshot, SessionTab};
use crate::naming::{NamingPolicy, ObservedProcess};
use crate::numbering::{Tab, is_placeholder, numbered_label, strip_numeric_prefix};
use crate::settings::Settings;
use crate::state::{State, TabOwnership};

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
        _ if state.is_suspended() => return Ok(()),
        _ => {}
    }

    let snapshot = client.snapshot()?;
    recover_pending(&snapshot, &mut state);
    let policy = naming_policy(&config.settings);
    let fallback_shell = fallback_shell();
    let targets = scoped_tabs(&snapshot, invocation);
    let positions = tab_positions(&snapshot);

    for session_tab in targets {
        let position = positions[&session_tab.tab.tab_id];
        reconcile_tab(
            client,
            &snapshot,
            session_tab,
            position,
            invocation,
            &config.settings,
            &policy,
            &fallback_shell,
            &mut state,
        )?;
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

#[allow(clippy::too_many_arguments)]
fn reconcile_tab(
    client: &mut impl TabClient,
    snapshot: &SessionSnapshot,
    session_tab: &SessionTab,
    position: usize,
    invocation: &Invocation,
    settings: &Settings,
    policy: &NamingPolicy,
    fallback_shell: &str,
    state: &mut State,
) -> Result<()> {
    let tab = &session_tab.tab;
    let current_base = strip_numeric_prefix(&tab.label);
    let ownership = state.ownership(&tab.tab_id).cloned();
    let forced = matches!(
        invocation,
        Invocation::Reset {
            tab_id: Some(tab_id),
            ..
        } if tab_id == &tab.tab_id
    );
    let ambient_first_adoption = settings.auto_name_tabs
        && ownership.is_none()
        && is_placeholder(current_base)
        && !matches!(
            invocation,
            Invocation::Preexec { .. } | Invocation::Precmd { .. }
        );
    let eligible = if forced {
        true
    } else {
        match ownership.as_ref() {
            Some(TabOwnership::Manual) if current_base.trim().is_empty() => {
                state.remove_ownership(&tab.tab_id);
                true
            }
            Some(TabOwnership::Manual) => false,
            Some(TabOwnership::ResetPending) => true,
            Some(TabOwnership::Owned { last_base, .. }) if current_base == last_base => true,
            Some(TabOwnership::Owned { .. }) => {
                state.set_ownership(&tab.tab_id, TabOwnership::Manual);
                false
            }
            Some(TabOwnership::PendingRename { .. }) => false,
            None if is_placeholder(current_base) => true,
            None => {
                state.set_ownership(&tab.tab_id, TabOwnership::Manual);
                false
            }
        }
    };

    let own_rename_event = matches!(
        (invocation, ownership.as_ref()),
        (
            Invocation::RenamedTab { tab_id, .. },
            Some(TabOwnership::Owned { last_rendered, .. })
        ) if tab_id == &tab.tab_id && last_rendered == &tab.label
    );
    let computed_base = if settings.auto_name_tabs && eligible && !own_rename_event {
        computed_name(
            client,
            snapshot,
            session_tab,
            invocation,
            policy,
            fallback_shell,
            ambient_first_adoption,
        )?
    } else {
        None
    };
    if ambient_first_adoption && computed_base.is_none() {
        return Ok(());
    }
    let desired_base = computed_base.as_ref().map_or(current_base, |name| name);
    let desired = if settings.number_tabs {
        numbered_label(position, desired_base)
    } else {
        desired_base.to_owned()
    };
    let plugin_owned = eligible && computed_base.is_some();

    if desired != tab.label {
        if plugin_owned {
            let (previous_base, previous_rendered, previous_reset_pending) = match &ownership {
                Some(TabOwnership::Owned {
                    last_base,
                    last_rendered,
                }) => (Some(last_base.clone()), Some(last_rendered.clone()), false),
                Some(TabOwnership::ResetPending) => (None, None, true),
                _ => (None, None, false),
            };
            state.set_ownership(
                &tab.tab_id,
                TabOwnership::PendingRename {
                    observed: tab.label.clone(),
                    desired: desired.clone(),
                    desired_base: desired_base.to_owned(),
                    previous_base,
                    previous_rendered,
                    previous_reset_pending,
                },
            );
            state.persist()?;
        }
        let Some(latest) = client.get_tab(&tab.tab_id)? else {
            return Ok(());
        };
        if latest.label != tab.label {
            if plugin_owned {
                state.resolve_pending_rename(&tab.tab_id, &latest.label);
                state.persist()?;
            }
            return Ok(());
        }
        client.rename_tab(&tab.tab_id, &desired)?;
    }

    if plugin_owned {
        state.set_ownership(
            &tab.tab_id,
            TabOwnership::Owned {
                last_base: desired_base.to_owned(),
                last_rendered: desired,
            },
        );
    }
    Ok(())
}

fn computed_name(
    client: &mut impl TabClient,
    snapshot: &SessionSnapshot,
    tab: &SessionTab,
    invocation: &Invocation,
    policy: &NamingPolicy,
    fallback_shell: &str,
    ambient_shell_only: bool,
) -> Result<Option<String>> {
    match invocation {
        Invocation::Preexec {
            pane_id,
            shell,
            program: Some(program),
            ..
        } if pane_targets_tab(snapshot, pane_id, &tab.tab.tab_id) => {
            let Ok(process_info) = client.pane_process_info(pane_id) else {
                return Ok(None);
            };
            let Some(_) = representative_process(&process_info, policy) else {
                return Ok(None);
            };
            if !process_group_matches_program(&process_info, program, policy) {
                return Ok(None);
            }
            Ok(Some(
                policy
                    .label(
                        shell,
                        Some(&ObservedProcess {
                            program: program.to_owned(),
                            command_line: None,
                        }),
                    )
                    .unwrap_or_default(),
            ))
        }
        Invocation::Precmd {
            pane_id,
            shell,
            shell_pid,
            ..
        } if pane_targets_tab(snapshot, pane_id, &tab.tab.tab_id) => {
            let Ok(process_info) = client.pane_process_info(pane_id) else {
                return Ok(None);
            };
            if process_info.foreground_process_group_id != Some(*shell_pid) {
                return Ok(None);
            }
            Ok(Some(policy.label(shell, None).unwrap_or_default()))
        }
        _ => {
            let Some(pane_id) = naming_pane(snapshot, tab) else {
                return Ok(None);
            };
            let Ok(process_info) = client.pane_process_info(pane_id) else {
                return Ok(None);
            };
            let Some(process) = representative_process(&process_info, policy) else {
                return Ok(None);
            };
            if ambient_shell_only && !policy.is_shell_program(process.program()) {
                return Ok(None);
            }
            if policy.is_ignored_program(process.program()) {
                return Ok(None);
            }
            Ok(Some(
                policy
                    .label(
                        fallback_shell,
                        Some(&ObservedProcess {
                            program: process.program().to_owned(),
                            command_line: None,
                        }),
                    )
                    .unwrap_or_default(),
            ))
        }
    }
}

fn process_group_matches_program(
    process_info: &PaneProcessInfo,
    program: &str,
    policy: &NamingPolicy,
) -> bool {
    let Some(leader) = process_info.leader() else {
        return false;
    };
    process_info
        .foreground_processes
        .iter()
        .any(|process| policy.same_program(program, process.program()))
        || leader.argv.as_deref().is_some_and(|arguments| {
            arguments
                .iter()
                .skip(1)
                .any(|argument| policy.same_program(program, argument))
        })
}

fn representative_process<'a>(
    process_info: &'a PaneProcessInfo,
    policy: &NamingPolicy,
) -> Option<&'a crate::herdr::ProcessInfo> {
    let leader = process_info.leader()?;
    let launched_process = leader.argv.as_deref().and_then(|arguments| {
        process_info
            .foreground_processes
            .iter()
            .filter(|process| process.pid != leader.pid)
            .find(|process| {
                arguments
                    .iter()
                    .skip(1)
                    .any(|argument| policy.same_program(argument, process.program()))
            })
    });
    if let Some(process) = launched_process {
        return Some(process);
    }
    if !policy.is_shell_program(leader.program()) {
        return Some(leader);
    }
    process_info
        .foreground_processes
        .iter()
        .find(|process| !policy.is_shell_program(process.program()))
        .or(Some(leader))
}

fn naming_pane<'a>(snapshot: &'a SessionSnapshot, tab: &SessionTab) -> Option<&'a str> {
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

fn scoped_tabs<'a>(snapshot: &'a SessionSnapshot, invocation: &Invocation) -> Vec<&'a SessionTab> {
    if let Invocation::Preexec { pane_id, .. } | Invocation::Precmd { pane_id, .. } = invocation
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
        Invocation::Preexec { pane_id, .. } | Invocation::Precmd { pane_id, .. } => (
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

fn pane_targets_tab(snapshot: &SessionSnapshot, pane_id: &str, tab_id: &str) -> bool {
    snapshot
        .panes
        .iter()
        .any(|pane| pane.pane_id == pane_id && pane.tab_id == tab_id)
}

fn tab_positions(snapshot: &SessionSnapshot) -> HashMap<String, usize> {
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

fn recover_pending(snapshot: &SessionSnapshot, state: &mut State) {
    for tab in &snapshot.tabs {
        state.resolve_pending_rename(&tab.tab.tab_id, &tab.tab.label);
    }
}

fn naming_policy(settings: &Settings) -> NamingPolicy {
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

fn fallback_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .as_deref()
        .and_then(|shell| shell.rsplit('/').next())
        .filter(|shell| !shell.is_empty())
        .unwrap_or("zsh")
        .to_owned()
}

#[cfg(test)]
#[path = "../tests/unit/reconciliation.rs"]
mod tests;

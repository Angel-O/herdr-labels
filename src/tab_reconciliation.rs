//! One-tab reconciliation and process-derived label computation.

use std::error::Error;

use crate::config::Invocation;
use crate::herdr::{PaneProcessInfo, ProcessInfo, SessionSnapshot, SessionTab};
use crate::naming::{NamingPolicy, ObservedProcess};
use crate::numbering::{is_placeholder, numbered_label, strip_numeric_prefix};
use crate::process_selection::{
    RepresentativeSelection, process_group_matches_program, representative_process_with_trace_mode,
};
use crate::reconciliation::TabClient;
use crate::settings::Settings;
use crate::state::{State, TabOwnership};
use crate::targeting::{naming_pane, pane_targets_tab};
use crate::telemetry::{CandidateRecord, ProcessIdentity, ProcessRecord, TabObservation};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[allow(clippy::too_many_arguments)]
pub(super) fn reconcile_tab(
    client: &mut impl TabClient,
    snapshot: &SessionSnapshot,
    session_tab: &SessionTab,
    position: usize,
    invocation: &Invocation,
    settings: &Settings,
    policy: &NamingPolicy,
    fallback_shell: &str,
    state: &mut State,
    mut trace: Option<&mut CandidateRecord>,
) -> Result<()> {
    let tab = &session_tab.tab;
    let current_base = strip_numeric_prefix(&tab.label);
    let ownership = state.ownership(&tab.tab_id).cloned();
    let renamed_to_whitespace = !tab.label.is_empty()
        && tab.label.trim().is_empty()
        && matches!(
            invocation,
            Invocation::RenamedTab { tab_id, .. } if tab_id == &tab.tab_id
        );
    let forced = matches!(
        invocation,
        Invocation::Reset {
            tab_id: Some(tab_id),
            ..
        } if tab_id == &tab.tab_id
    );
    let initial_adoption =
        settings.auto_name_tabs && ownership.is_none() && is_placeholder(current_base);
    let authoritative_initial_event = matches!(
        invocation,
        Invocation::Init { .. }
            | Invocation::Preexec {
                program: Some(_),
                ..
            }
            | Invocation::Precmd { .. }
    );
    if initial_adoption && !authoritative_initial_event {
        trace_result(
            trace.as_deref_mut(),
            Some(false),
            Some("initial_adoption_requires_authoritative_event"),
            "no_op",
        );
        trace_ownership_after(trace.as_deref_mut(), state, &tab.tab_id);
        return Ok(());
    }
    if matches!(
        ownership.as_ref(),
        Some(TabOwnership::Owned {
            last_base,
            last_rendered,
        }) if current_base != last_base || &tab.label != last_rendered
    ) {
        state.set_ownership(&tab.tab_id, TabOwnership::Manual);
        trace_result(
            trace.as_deref_mut(),
            Some(false),
            Some("manual_rename"),
            "ownership_blocked",
        );
        trace_ownership_after(trace.as_deref_mut(), state, &tab.tab_id);
        return Ok(());
    }
    let eligible = if forced {
        true
    } else {
        match ownership.as_ref() {
            Some(TabOwnership::Manual) if current_base.trim().is_empty() => {
                state.remove_ownership(&tab.tab_id);
                true
            }
            Some(TabOwnership::Manual) => false,
            Some(TabOwnership::AutomaticDisabled) if renamed_to_whitespace => {
                state.remove_ownership(&tab.tab_id);
                true
            }
            Some(TabOwnership::AutomaticDisabled) => false,
            Some(TabOwnership::ResetPending) => true,
            Some(TabOwnership::Owned { .. }) => true,
            Some(TabOwnership::PendingRename { .. }) => false,
            None if is_placeholder(current_base) => true,
            None => {
                state.set_ownership(&tab.tab_id, TabOwnership::Manual);
                false
            }
        }
    };
    if let Some(trace) = trace.as_deref_mut() {
        trace.eligible = Some(eligible);
        if !eligible {
            trace.rejection_reason = Some(rejection_reason(ownership.as_ref()));
        }
    }

    let own_rename_event = matches!(
        (invocation, ownership.as_ref()),
        (
            Invocation::RenamedTab { tab_id, .. },
            Some(TabOwnership::Owned { last_rendered, .. })
        ) if tab_id == &tab.tab_id && last_rendered == &tab.label
    );
    let observes_process = matches!(
        invocation,
        Invocation::Tab { .. }
            | Invocation::ClosedPane { .. }
            | Invocation::Init { .. }
            | Invocation::Preexec { .. }
            | Invocation::Precmd { .. }
            | Invocation::Reset { .. }
            | Invocation::Toggle { .. }
    ) || renamed_to_whitespace;
    let computed_base = if settings.auto_name_tabs
        && eligible
        && (observes_process || matches!(ownership, Some(TabOwnership::ResetPending)))
        && !own_rename_event
    {
        computed_name_with_trace(
            client,
            snapshot,
            session_tab,
            invocation,
            policy,
            fallback_shell,
            false,
            trace.as_deref_mut(),
        )?
    } else {
        None
    };
    if let Some(trace) = trace.as_deref_mut() {
        trace.computed_base_label = computed_base.clone();
    }
    if initial_adoption && computed_base.is_none() {
        if let Some(trace) = trace.as_deref_mut() {
            trace.eligible = Some(false);
            trace.rejection_reason = Some(
                trace
                    .rejection_reason
                    .clone()
                    .unwrap_or_else(|| "process_or_naming_unavailable".into()),
            );
            trace.outcome = trace.rejection_reason.clone();
        }
        trace_ownership_after(trace.as_deref_mut(), state, &tab.tab_id);
        return Ok(());
    }
    let owned_base = if eligible {
        match ownership.as_ref() {
            Some(TabOwnership::Owned { last_base, .. }) => Some(last_base.as_str()),
            _ => None,
        }
    } else {
        None
    };
    let desired_base = computed_base
        .as_deref()
        .or(owned_base)
        .unwrap_or(current_base);
    let desired = if settings.number_tabs {
        numbered_label(position, desired_base)
    } else {
        desired_base.to_owned()
    };
    if let Some(trace) = trace.as_deref_mut() {
        trace.desired_label = Some(desired.clone());
    }
    let plugin_owned = eligible && (computed_base.is_some() || owned_base.is_some());

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
        let latest = match client.get_tab(&tab.tab_id) {
            Ok(latest) => latest,
            Err(error) => {
                trace_guard_error(trace.as_deref_mut(), error.to_string());
                trace_ownership_after(trace.as_deref_mut(), state, &tab.tab_id);
                return Err(error);
            }
        };
        let Some(latest) = latest else {
            if let Some(trace) = trace.as_deref_mut() {
                trace.rename_result = Some("tab_not_found".into());
                trace.outcome = Some("tab_missing".into());
            }
            trace_ownership_after(trace.as_deref_mut(), state, &tab.tab_id);
            return Ok(());
        };
        if let Some(trace) = trace.as_deref_mut() {
            trace.guarded_tab = Some(TabObservation {
                tab_id: latest.tab_id.clone(),
                workspace_id: latest.workspace_id.clone(),
                label: latest.label.clone(),
            });
        }
        if latest.label != tab.label {
            if plugin_owned {
                state.resolve_pending_rename(&tab.tab_id, &latest.label);
                state.persist()?;
            }
            if let Some(trace) = trace.as_deref_mut() {
                trace.rename_result = Some("stale_guard".into());
                trace.outcome = Some("stale_guard".into());
            }
            trace_ownership_after(trace.as_deref_mut(), state, &tab.tab_id);
            return Ok(());
        }
        if let Err(error) = client.rename_tab(&tab.tab_id, &desired) {
            trace_rename_error(trace.as_deref_mut(), error.to_string());
            trace_ownership_after(trace.as_deref_mut(), state, &tab.tab_id);
            return Err(error);
        }
        if let Some(trace) = trace.as_deref_mut() {
            trace.rename_attempted = true;
            trace.rename_result = Some("renamed".into());
            trace.outcome = Some("renamed".into());
        }
    } else if let Some(trace) = trace.as_deref_mut() {
        trace.outcome = Some(
            if let Some(reason) = trace.rejection_reason.as_deref() {
                reason
            } else if plugin_owned {
                "already_correct"
            } else if !eligible {
                "ownership_blocked"
            } else {
                "no_op"
            }
            .into(),
        );
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
    trace_ownership_after(trace, state, &tab.tab_id);
    Ok(())
}

#[allow(dead_code)]
pub(super) fn computed_name(
    client: &mut impl TabClient,
    snapshot: &SessionSnapshot,
    tab: &SessionTab,
    invocation: &Invocation,
    policy: &NamingPolicy,
    fallback_shell: &str,
    ambient_shell_only: bool,
) -> Result<Option<String>> {
    computed_name_with_trace(
        client,
        snapshot,
        tab,
        invocation,
        policy,
        fallback_shell,
        ambient_shell_only,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn computed_name_with_trace(
    client: &mut impl TabClient,
    snapshot: &SessionSnapshot,
    tab: &SessionTab,
    invocation: &Invocation,
    policy: &NamingPolicy,
    fallback_shell: &str,
    ambient_shell_only: bool,
    mut trace: Option<&mut CandidateRecord>,
) -> Result<Option<String>> {
    match invocation {
        Invocation::Preexec {
            pane_id,
            shell,
            program: Some(program),
            ..
        } if pane_targets_tab(snapshot, pane_id, &tab.tab.tab_id) => {
            trace_naming_pane(trace.as_deref_mut(), pane_id, "event_pane");
            let process_info = match client.pane_process_info(pane_id) {
                Ok(process_info) => process_info,
                Err(error) => {
                    trace_process_error(trace.as_deref_mut(), error.to_string());
                    return Ok(None);
                }
            };
            trace_process_info(trace.as_deref_mut(), pane_id, &process_info);
            if !process_group_matches_program(&process_info, program, policy) {
                trace_rejection(trace.as_deref_mut(), "event_program_not_foreground");
                return Ok(None);
            }
            let selection = representative_process_with_trace_mode(
                &process_info,
                policy,
                None,
                trace.is_some(),
            );
            if let Some(selection) = selection.as_ref() {
                trace_selection(trace.as_deref_mut(), selection);
            } else if !policy.is_ignored_program(program) {
                trace_rejection(trace.as_deref_mut(), "no_representative_process");
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
        Invocation::Init {
            pane_id,
            shell,
            shell_pid,
        }
        | Invocation::Precmd {
            pane_id,
            shell,
            shell_pid,
            ..
        } if pane_targets_tab(snapshot, pane_id, &tab.tab.tab_id) => {
            trace_naming_pane(trace.as_deref_mut(), pane_id, "event_pane");
            let process_info = match client.pane_process_info(pane_id) {
                Ok(process_info) => process_info,
                Err(error) => {
                    trace_process_error(trace.as_deref_mut(), error.to_string());
                    return Ok(None);
                }
            };
            trace_process_info(trace.as_deref_mut(), pane_id, &process_info);
            if let Some(leader) = process_info.leader() {
                trace_representative(trace.as_deref_mut(), leader);
            }
            if process_info.foreground_process_group_id != Some(*shell_pid) {
                trace_rejection(trace.as_deref_mut(), "shell_pid_mismatch");
                return Ok(None);
            }
            Ok(Some(policy.label(shell, None).unwrap_or_default()))
        }
        _ => {
            let Some(pane_id) = naming_pane(snapshot, tab) else {
                trace_rejection(
                    trace.as_deref_mut(),
                    if tab.pane_count > 1 {
                        "background_multi_pane"
                    } else {
                        "no_naming_pane"
                    },
                );
                return Ok(None);
            };
            trace_naming_pane(trace.as_deref_mut(), pane_id, "snapshot_selection");
            let preferred_program = snapshot
                .panes
                .iter()
                .find(|pane| pane.pane_id == pane_id)
                .and_then(|pane| pane.agent.as_deref());
            let process_info = match client.pane_process_info(pane_id) {
                Ok(process_info) => process_info,
                Err(error) => {
                    trace_process_error(trace.as_deref_mut(), error.to_string());
                    return Ok(None);
                }
            };
            trace_process_info(trace.as_deref_mut(), pane_id, &process_info);
            let Some(selection) = representative_process_with_trace_mode(
                &process_info,
                policy,
                preferred_program,
                trace.is_some(),
            ) else {
                trace_rejection(trace.as_deref_mut(), "no_representative_process");
                return Ok(None);
            };
            trace_selection(trace.as_deref_mut(), &selection);
            let process = selection.process;
            if ambient_shell_only && !policy.is_shell_program(process.program()) {
                trace_rejection(trace.as_deref_mut(), "representative_not_shell");
                return Ok(None);
            }
            if policy.is_ignored_program(process.program()) {
                trace_rejection(trace, "ignored_process");
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

fn trace_result(
    trace: Option<&mut CandidateRecord>,
    eligible: Option<bool>,
    rejection_reason: Option<&str>,
    outcome: &str,
) {
    if let Some(trace) = trace {
        if let Some(eligible) = eligible {
            trace.eligible = Some(eligible);
        }
        if let Some(reason) = rejection_reason {
            trace.rejection_reason = Some(reason.into());
        }
        trace.outcome = Some(outcome.into());
    }
}

fn trace_ownership_after(trace: Option<&mut CandidateRecord>, state: &State, tab_id: &str) {
    if let Some(trace) = trace {
        trace.ownership_after = state.ownership(tab_id).cloned();
    }
}

fn trace_naming_pane(trace: Option<&mut CandidateRecord>, pane_id: &str, reason: &str) {
    if let Some(trace) = trace {
        trace.selected_naming_pane = Some(pane_id.into());
        trace.naming_pane_reason = Some(reason.into());
    }
}

fn trace_rejection(trace: Option<&mut CandidateRecord>, reason: &str) {
    if let Some(trace) = trace {
        trace.rejection_reason = Some(reason.into());
        if trace.outcome.is_none() {
            trace.outcome = Some(reason.into());
        }
    }
}

fn trace_process_info(
    trace: Option<&mut CandidateRecord>,
    pane_id: &str,
    process_info: &PaneProcessInfo,
) {
    if let Some(trace) = trace {
        trace.process_info = Some(ProcessRecord::from_info(pane_id, process_info));
        trace.process_error = None;
    }
}

fn trace_process_error(trace: Option<&mut CandidateRecord>, error: String) {
    if let Some(trace) = trace {
        trace.process_error = Some(error);
        trace.rejection_reason = Some("process_unavailable".into());
        trace.outcome = Some("process_unavailable".into());
    }
}

fn trace_representative(trace: Option<&mut CandidateRecord>, process: &ProcessInfo) {
    if let Some(trace) = trace {
        trace.representative_process = Some(ProcessIdentity::from_process(process));
    }
}

fn trace_selection(trace: Option<&mut CandidateRecord>, selection: &RepresentativeSelection<'_>) {
    if let Some(trace) = trace {
        trace_representative(Some(trace), selection.process);
        trace.representative_selection = Some(selection.reason.into());
        trace.ignored_processes_skipped = selection
            .ignored_processes
            .iter()
            .map(|process| ProcessIdentity::from_process(process))
            .collect();
    }
}

fn trace_rename_error(trace: Option<&mut CandidateRecord>, error: String) {
    if let Some(trace) = trace {
        trace.rename_attempted = true;
        trace.rename_result = Some(format!("error: {error}"));
        trace.outcome = Some("rename_error".into());
    }
}

fn trace_guard_error(trace: Option<&mut CandidateRecord>, error: String) {
    if let Some(trace) = trace {
        trace.rename_attempted = false;
        trace.rename_result = Some(format!("error: {error}"));
        trace.outcome = Some("guard_read_error".into());
    }
}

fn rejection_reason(ownership: Option<&TabOwnership>) -> String {
    match ownership {
        Some(TabOwnership::Manual) => "manual_ownership".into(),
        Some(TabOwnership::AutomaticDisabled) => "automatic_disabled".into(),
        Some(TabOwnership::PendingRename { .. }) => "pending_rename".into(),
        None => "non_placeholder_unowned".into(),
        Some(TabOwnership::Owned { .. } | TabOwnership::ResetPending) => "ineligible".into(),
    }
}

#[cfg(test)]
#[path = "../tests/unit/tab_reconciliation.rs"]
mod tests;

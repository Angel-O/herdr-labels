//! One-tab reconciliation and process-derived label computation.

use std::error::Error;

use crate::config::{Config, Invocation};
use crate::herdr::{SessionSnapshot, SessionTab};
use crate::naming::{NamingPolicy, ObservedProcess};
use crate::numbering::{is_placeholder, numbered_label, strip_numeric_prefix};
use crate::process_selection::{process_group_matches_program, select_representative};
use crate::reconciliation::{TabClient, naming_policy};
use crate::state::{State, TabOwnership};
use crate::targeting::{naming_pane, pane_targets_tab, tab_positions};
use crate::telemetry::TabTelemetry;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub(super) fn reconcile_tab(
    client: &mut impl TabClient,
    snapshot: &SessionSnapshot,
    session_tab: &SessionTab,
    invocation: &Invocation,
    config: &Config,
    state: &mut State,
    telemetry: &mut TabTelemetry,
) -> Result<()> {
    let position = tab_positions(snapshot)[&session_tab.tab.tab_id];
    let settings = &config.settings;
    let policy = naming_policy(settings);
    let fallback_shell = fallback_shell();
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
        telemetry.set_eligible(false, Some("initial_adoption_requires_authoritative_event"));
        telemetry.set_outcome("no_op");
        telemetry.set_ownership_after(state, &tab.tab_id);
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
        telemetry.set_eligible(false, Some("manual_rename"));
        telemetry.set_outcome("ownership_blocked");
        telemetry.set_ownership_after(state, &tab.tab_id);
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
    let rejection = (!eligible).then(|| rejection_reason(ownership.as_ref()));
    telemetry.set_eligible(eligible, rejection.as_deref());

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
        computed_name(
            client,
            snapshot,
            session_tab,
            invocation,
            &policy,
            &fallback_shell,
            telemetry,
        )?
    } else {
        None
    };
    telemetry.record_computed_base(computed_base.as_deref());
    if initial_adoption && computed_base.is_none() {
        telemetry.set_initial_adoption_failure();
        telemetry.set_ownership_after(state, &tab.tab_id);
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
    telemetry.record_desired_label(&desired);
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
                telemetry.record_guard_error(error.to_string());
                telemetry.set_ownership_after(state, &tab.tab_id);
                return Err(error);
            }
        };
        let Some(latest) = latest else {
            telemetry.record_rename_result("tab_not_found", "tab_missing");
            telemetry.set_ownership_after(state, &tab.tab_id);
            return Ok(());
        };
        telemetry.record_guarded_tab(&latest);
        if latest.label != tab.label {
            if plugin_owned {
                state.resolve_pending_rename(&tab.tab_id, &latest.label);
                state.persist()?;
            }
            telemetry.record_rename_result("stale_guard", "stale_guard");
            telemetry.set_ownership_after(state, &tab.tab_id);
            return Ok(());
        }
        if let Err(error) = client.rename_tab(&tab.tab_id, &desired) {
            telemetry.record_rename_error(error.to_string());
            telemetry.set_ownership_after(state, &tab.tab_id);
            return Err(error);
        }
        telemetry.record_rename_success();
    } else {
        telemetry.set_unchanged_outcome(plugin_owned);
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
    telemetry.set_ownership_after(state, &tab.tab_id);
    Ok(())
}

pub(super) fn computed_name(
    client: &mut impl TabClient,
    snapshot: &SessionSnapshot,
    tab: &SessionTab,
    invocation: &Invocation,
    policy: &NamingPolicy,
    fallback_shell: &str,
    telemetry: &mut TabTelemetry,
) -> Result<Option<String>> {
    match invocation {
        Invocation::Preexec {
            pane_id,
            shell,
            program: Some(program),
            ..
        } if pane_targets_tab(snapshot, pane_id, &tab.tab.tab_id) => {
            telemetry.record_naming_pane(pane_id, "event_pane");
            let process_info = match client.pane_process_info(pane_id) {
                Ok(process_info) => process_info,
                Err(error) => {
                    telemetry.record_process_error(error.to_string());
                    return Ok(None);
                }
            };
            telemetry.record_process_info(pane_id, &process_info);
            if !process_group_matches_program(&process_info, program, policy) {
                telemetry.reject("event_program_not_foreground");
                return Ok(None);
            }
            let selection = select_representative(&process_info, policy, None);
            if let Some(selection) = selection.as_ref() {
                telemetry.record_representative_selection(
                    selection.process,
                    selection.reason,
                    &process_info,
                    policy,
                );
            } else if !policy.is_ignored_program(program) {
                telemetry.reject("no_representative_process");
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
            telemetry.record_naming_pane(pane_id, "event_pane");
            let process_info = match client.pane_process_info(pane_id) {
                Ok(process_info) => process_info,
                Err(error) => {
                    telemetry.record_process_error(error.to_string());
                    return Ok(None);
                }
            };
            telemetry.record_process_info(pane_id, &process_info);
            if let Some(leader) = process_info.leader() {
                telemetry.record_representative(leader);
            }
            if process_info.foreground_process_group_id != Some(*shell_pid) {
                telemetry.reject("shell_pid_mismatch");
                return Ok(None);
            }
            Ok(Some(policy.label(shell, None).unwrap_or_default()))
        }
        _ => {
            let Some(pane_id) = naming_pane(snapshot, tab) else {
                telemetry.reject(if tab.pane_count > 1 {
                    "background_multi_pane"
                } else {
                    "no_naming_pane"
                });
                return Ok(None);
            };
            telemetry.record_naming_pane(pane_id, "snapshot_selection");
            let preferred_program = snapshot
                .panes
                .iter()
                .find(|pane| pane.pane_id == pane_id)
                .and_then(|pane| pane.agent.as_deref());
            let process_info = match client.pane_process_info(pane_id) {
                Ok(process_info) => process_info,
                Err(error) => {
                    telemetry.record_process_error(error.to_string());
                    return Ok(None);
                }
            };
            telemetry.record_process_info(pane_id, &process_info);
            let Some(selection) = select_representative(&process_info, policy, preferred_program)
            else {
                telemetry.reject("no_representative_process");
                return Ok(None);
            };
            telemetry.record_representative_selection(
                selection.process,
                selection.reason,
                &process_info,
                policy,
            );
            let process = selection.process;
            if policy.is_ignored_program(process.program()) {
                telemetry.reject("ignored_process");
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

fn rejection_reason(ownership: Option<&TabOwnership>) -> String {
    match ownership {
        Some(TabOwnership::Manual) => "manual_ownership".into(),
        Some(TabOwnership::AutomaticDisabled) => "automatic_disabled".into(),
        Some(TabOwnership::PendingRename { .. }) => "pending_rename".into(),
        None => "non_placeholder_unowned".into(),
        Some(TabOwnership::Owned { .. } | TabOwnership::ResetPending) => "ineligible".into(),
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
#[path = "../tests/unit/tab_reconciliation.rs"]
mod tests;

//! Coordinates process-aware, race-conscious tab label reconciliation.

use std::collections::{HashMap, HashSet};
use std::error::Error;

use crate::config::{Config, Invocation};
use crate::herdr::{HerdrClient, PaneProcessInfo, ProcessInfo, SessionSnapshot, SessionTab};
use crate::naming::{NamingPolicy, ObservedProcess};
use crate::numbering::{Tab, is_placeholder, numbered_label, strip_numeric_prefix};
use crate::settings::Settings;
use crate::state::{State, TabOwnership};
use crate::telemetry::{CandidateRecord, DecisionRecord, TabObservation};

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

#[allow(dead_code)]
pub(crate) fn run_pass(
    config: &Config,
    invocation: &Invocation,
    client: &mut impl TabClient,
) -> Result<()> {
    run_pass_with_telemetry(config, invocation, client, None)
}

pub(crate) fn run_pass_with_telemetry(
    config: &Config,
    invocation: &Invocation,
    client: &mut impl TabClient,
    mut telemetry: Option<&mut DecisionRecord>,
) -> Result<()> {
    if !config.settings.diagnostic_telemetry {
        telemetry = None;
    }
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
            if let Some(record) = telemetry.as_deref_mut() {
                record.set_terminal_outcome("suspended");
            }
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
        if let Some(record) = telemetry.as_deref_mut() {
            record.record_pane_mapping(
                pane_id,
                resolution.mapped_tab_id.as_deref(),
                resolution.resolution,
                resolution.validation,
                resolution.valid,
            );
        }
        if !resolution.valid {
            if let Some(record) = telemetry.as_deref_mut() {
                record.record_snapshot(&snapshot, &[]);
                record.set_terminal_outcome(resolution.outcome);
            }
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
    let policy = naming_policy(&config.settings);
    let fallback_shell = fallback_shell();
    let targets = if let Some(resolution) = closed_resolution.as_ref() {
        scoped_closed_pane_tabs(&snapshot, invocation, resolution.mapped_tab_id.as_deref())
    } else {
        scoped_tabs(&snapshot, invocation)
    };
    let positions = tab_positions(&snapshot);

    if let Some(record) = telemetry.as_deref_mut() {
        let target_ids = targets
            .iter()
            .map(|target| target.tab.tab_id.clone())
            .collect::<Vec<_>>();
        record.record_snapshot(&snapshot, &target_ids);
    }

    for session_tab in targets {
        let position = positions[&session_tab.tab.tab_id];
        let mut candidate = telemetry.as_ref().map(|record| {
            record.new_candidate(
                &session_tab.tab,
                state.ownership(&session_tab.tab.tab_id).cloned(),
                session_tab.pane_count,
                session_tab.focused,
            )
        });
        let result = reconcile_tab(
            client,
            &snapshot,
            session_tab,
            position,
            invocation,
            &config.settings,
            &policy,
            &fallback_shell,
            &mut state,
            candidate.as_mut(),
        );
        if let (Some(record), Some(candidate)) = (telemetry.as_deref_mut(), candidate) {
            record.push_candidate(candidate);
        }
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
fn computed_name(
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
fn computed_name_with_trace(
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
        trace.process_info = Some(crate::telemetry::ProcessRecord::from_info(
            pane_id,
            process_info,
        ));
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

fn trace_representative(trace: Option<&mut CandidateRecord>, process: &crate::herdr::ProcessInfo) {
    if let Some(trace) = trace {
        trace.representative_process =
            Some(crate::telemetry::ProcessIdentity::from_process(process));
    }
}

fn trace_selection(trace: Option<&mut CandidateRecord>, selection: &RepresentativeSelection<'_>) {
    if let Some(trace) = trace {
        trace_representative(Some(trace), selection.process);
        trace.representative_selection = Some(selection.reason.into());
        trace.ignored_processes_skipped = selection
            .ignored_processes
            .iter()
            .map(|process| crate::telemetry::ProcessIdentity::from_process(process))
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

struct RepresentativeSelection<'a> {
    process: &'a ProcessInfo,
    reason: &'static str,
    ignored_processes: Vec<&'a ProcessInfo>,
}

#[allow(dead_code)]
fn representative_process<'a>(
    process_info: &'a PaneProcessInfo,
    policy: &NamingPolicy,
    preferred_program: Option<&str>,
) -> Option<&'a crate::herdr::ProcessInfo> {
    representative_process_with_trace(process_info, policy, preferred_program)
        .map(|selection| selection.process)
}

fn representative_process_with_trace<'a>(
    process_info: &'a PaneProcessInfo,
    policy: &NamingPolicy,
    preferred_program: Option<&str>,
) -> Option<RepresentativeSelection<'a>> {
    representative_process_with_trace_mode(process_info, policy, preferred_program, true)
}

fn representative_process_with_trace_mode<'a>(
    process_info: &'a PaneProcessInfo,
    policy: &NamingPolicy,
    preferred_program: Option<&str>,
    collect_ignored_processes: bool,
) -> Option<RepresentativeSelection<'a>> {
    let leader = process_info.leader()?;
    let ignored_processes = if collect_ignored_processes {
        process_info
            .foreground_processes
            .iter()
            .filter(|process| policy.is_ignored_program(process.program()))
            .collect()
    } else {
        Vec::new()
    };
    if let Some(process) = preferred_program.and_then(|preferred| {
        process_info.foreground_processes.iter().find(|process| {
            policy.same_program(preferred, process.program())
                && !policy.is_ignored_program(process.program())
        })
    }) {
        return Some(RepresentativeSelection {
            process,
            reason: "preferred",
            ignored_processes,
        });
    }
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
                    && !policy.is_ignored_program(process.program())
            })
    });
    if let Some(process) = launched_process {
        return Some(RepresentativeSelection {
            process,
            reason: "launched",
            ignored_processes,
        });
    }
    if !policy.is_shell_program(leader.program()) && !policy.is_ignored_program(leader.program()) {
        return Some(RepresentativeSelection {
            process: leader,
            reason: "leader",
            ignored_processes,
        });
    }
    if let Some(process) = process_info.foreground_processes.iter().find(|process| {
        process.pid != leader.pid
            && !policy.is_shell_program(process.program())
            && !policy.is_ignored_program(process.program())
    }) {
        return Some(RepresentativeSelection {
            process,
            reason: "foreground",
            ignored_processes,
        });
    }
    if policy.is_shell_program(leader.program()) {
        return Some(RepresentativeSelection {
            process: leader,
            reason: "shell_leader_fallback",
            ignored_processes,
        });
    }
    None
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

fn scoped_closed_pane_tabs<'a>(
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

struct ClosedPaneResolution {
    mapped_tab_id: Option<String>,
    resolution: &'static str,
    validation: &'static str,
    valid: bool,
    outcome: &'static str,
}

fn resolve_closed_pane(
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

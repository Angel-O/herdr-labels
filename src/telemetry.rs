//! In-memory diagnostic decision records emitted through the plugin command log.

use std::error::Error;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

use serde::Serialize;

use crate::herdr::{PaneProcessInfo, ProcessInfo, SessionSnapshot};
use crate::naming::NamingPolicy;
use crate::numbering::Tab;
use crate::state::{State, TabOwnership};

const RECORD_TYPE: &str = "herdr_labels_decision";
const SCHEMA_VERSION: u8 = 1;

static PROCESS_START: OnceLock<Instant> = OnceLock::new();

/// Diagnostic recording mode for one invocation.
pub(crate) enum Telemetry {
    Off,
    Recording(Box<DecisionRecord>),
}

/// Diagnostic recording mode for one reconciled tab.
pub(crate) enum TabTelemetry {
    Off,
    Recording(Box<TabDecisionRecord>),
}

/// One in-memory, JSON-serializable decision record for a close or focus-control path.
///
/// The record is emitted once, after reconciliation, so collecting it does not add
/// filesystem I/O to the measured decision. Process arguments are intentionally
/// reduced to executable and `argv0` basenames.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct DecisionRecord {
    pub(crate) record_type: &'static str,
    pub(crate) schema_version: u8,
    pub(crate) trigger: String,
    pub(crate) path: String,
    pub(crate) started_monotonic_ns: u64,
    pub(crate) finished_monotonic_ns: Option<u64>,
    pub(crate) workspace_id: Option<String>,
    pub(crate) event_pane_id: Option<String>,
    pub(crate) event_tab_id: Option<String>,
    pub(crate) available_tab_ids: Vec<String>,
    pub(crate) target_scope: TargetScope,
    pub(crate) snapshot: Option<SnapshotRecord>,
    pub(crate) pane_mapping: Option<PaneMappingRecord>,
    #[serde(rename = "candidates")]
    pub(crate) tab_decisions: Vec<TabDecisionRecord>,
    pub(crate) lock: LockRecord,
    pub(crate) rerun: RerunRecord,
    pub(crate) terminal_outcome: String,
    pub(crate) process_status: String,
    pub(crate) process_error: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct TargetScope {
    pub(crate) kind: String,
    pub(crate) workspace_id: Option<String>,
    pub(crate) tab_id: Option<String>,
    pub(crate) pane_id: Option<String>,
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub(crate) struct LockRecord {
    pub(crate) attempts: Vec<LockAttempt>,
    pub(crate) benign_timeout_or_drop: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct LockAttempt {
    pub(crate) phase: String,
    pub(crate) result: String,
    pub(crate) wait_ns: u64,
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub(crate) struct RerunRecord {
    pub(crate) requested: bool,
    pub(crate) handoff: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct SnapshotRecord {
    pub(crate) tabs: Vec<TabSnapshotRecord>,
    pub(crate) panes: Vec<PaneSnapshotRecord>,
    pub(crate) focused_pane_id: Option<String>,
    pub(crate) closed_pane_present: Option<bool>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct PaneMappingRecord {
    pub(crate) pane_id: String,
    pub(crate) mapped_tab_id: Option<String>,
    pub(crate) resolution: String,
    pub(crate) validation: String,
    pub(crate) consumed: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct TabSnapshotRecord {
    pub(crate) tab_id: String,
    pub(crate) workspace_id: String,
    pub(crate) label: String,
    pub(crate) pane_count: usize,
    pub(crate) focused: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct PaneSnapshotRecord {
    pub(crate) pane_id: String,
    pub(crate) tab_id: String,
    pub(crate) agent: Option<String>,
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub(crate) struct TabDecisionRecord {
    pub(crate) tab_id: String,
    pub(crate) workspace_id: String,
    pub(crate) label: String,
    pub(crate) pane_count: usize,
    pub(crate) focused: bool,
    pub(crate) ownership_before: Option<TabOwnership>,
    pub(crate) ownership_after: Option<TabOwnership>,
    pub(crate) eligible: Option<bool>,
    pub(crate) rejection_reason: Option<String>,
    pub(crate) selected_naming_pane: Option<String>,
    pub(crate) naming_pane_reason: Option<String>,
    pub(crate) process_info: Option<ProcessRecord>,
    pub(crate) process_error: Option<String>,
    pub(crate) representative_process: Option<ProcessIdentity>,
    pub(crate) representative_selection: Option<String>,
    pub(crate) ignored_processes_skipped: Vec<ProcessIdentity>,
    pub(crate) computed_base_label: Option<String>,
    pub(crate) desired_label: Option<String>,
    pub(crate) guarded_tab: Option<TabObservation>,
    pub(crate) rename_attempted: bool,
    pub(crate) rename_result: Option<String>,
    pub(crate) outcome: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ProcessRecord {
    pub(crate) pane_id: String,
    pub(crate) foreground_process_group_id: Option<u32>,
    pub(crate) leader: Option<ProcessIdentity>,
    pub(crate) processes: Vec<ProcessIdentity>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ProcessIdentity {
    pub(crate) pid: u32,
    pub(crate) executable_basename: String,
    pub(crate) argv0_basename: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct TabObservation {
    pub(crate) tab_id: String,
    pub(crate) workspace_id: String,
    pub(crate) label: String,
}

impl Telemetry {
    #[cfg(test)]
    pub(crate) fn recording_mut(&mut self) -> &mut DecisionRecord {
        match self {
            Self::Recording(record) => record,
            Self::Off => panic!("recording telemetry expected"),
        }
    }

    pub(crate) fn record_snapshot(
        &mut self,
        snapshot: &SessionSnapshot,
        candidate_tab_ids: &[String],
    ) {
        let Self::Recording(record) = self else {
            return;
        };
        record.record_snapshot(snapshot, candidate_tab_ids);
    }

    pub(crate) fn record_pane_mapping(
        &mut self,
        pane_id: &str,
        mapped_tab_id: Option<&str>,
        resolution: &str,
        validation: &str,
        consumed: bool,
    ) {
        let Self::Recording(record) = self else {
            return;
        };
        record.pane_mapping = Some(PaneMappingRecord {
            pane_id: pane_id.into(),
            mapped_tab_id: mapped_tab_id.map(str::to_owned),
            resolution: resolution.into(),
            validation: validation.into(),
            consumed,
        });
    }

    pub(crate) fn new_tab_decision(
        &self,
        tab: &Tab,
        ownership: Option<TabOwnership>,
        pane_count: usize,
        focused: bool,
    ) -> TabTelemetry {
        match self {
            Self::Off => TabTelemetry::Off,
            Self::Recording(_) => TabTelemetry::Recording(Box::new(TabDecisionRecord {
                tab_id: tab.tab_id.clone(),
                workspace_id: tab.workspace_id.clone(),
                label: tab.label.clone(),
                pane_count,
                focused,
                ownership_before: ownership,
                ..TabDecisionRecord::default()
            })),
        }
    }

    pub(crate) fn push_tab_decision(&mut self, decision: TabTelemetry) {
        let (Self::Recording(record), TabTelemetry::Recording(decision)) = (self, decision) else {
            return;
        };
        record.tab_decisions.push(*decision);
    }

    pub(crate) fn record_lock_attempt(&mut self, phase: &str, result: &str, wait_ns: u64) {
        let Self::Recording(record) = self else {
            return;
        };
        record.lock.attempts.push(LockAttempt {
            phase: phase.into(),
            result: result.into(),
            wait_ns,
        });
    }

    pub(crate) fn record_rerun_requested(&mut self) {
        let Self::Recording(record) = self else {
            return;
        };
        record.rerun.requested = true;
        record.rerun.handoff = "requested".into();
    }

    pub(crate) fn record_handoff(&mut self, outcome: &str) {
        let Self::Recording(record) = self else {
            return;
        };
        record.rerun.handoff = outcome.into();
    }

    pub(crate) fn set_terminal_outcome(&mut self, outcome: &str) {
        let Self::Recording(record) = self else {
            return;
        };
        record.terminal_outcome = outcome.into();
        if outcome == "lock_dropped" {
            record.lock.benign_timeout_or_drop = true;
        }
    }

    pub(crate) fn finish_and_emit(&mut self, result: &Result<()>) {
        let current = std::mem::replace(self, Self::Off);
        let Self::Recording(mut record) = current else {
            return;
        };
        record.finish(result);
        record.emit();
    }
}

impl TabTelemetry {
    #[cfg(test)]
    pub(crate) fn recording_mut(&mut self) -> &mut TabDecisionRecord {
        match self {
            Self::Recording(record) => record,
            Self::Off => panic!("recording tab telemetry expected"),
        }
    }

    pub(crate) fn set_eligible(&mut self, eligible: bool, rejection_reason: Option<&str>) {
        let Self::Recording(record) = self else {
            return;
        };
        record.eligible = Some(eligible);
        if let Some(reason) = rejection_reason {
            record.rejection_reason = Some(reason.into());
        }
    }

    pub(crate) fn set_ownership_after(&mut self, state: &State, tab_id: &str) {
        let Self::Recording(record) = self else {
            return;
        };
        record.ownership_after = state.ownership(tab_id).cloned();
    }

    pub(crate) fn reject(&mut self, reason: &str) {
        let Self::Recording(record) = self else {
            return;
        };
        record.rejection_reason = Some(reason.into());
        if record.outcome.is_none() {
            record.outcome = Some(reason.into());
        }
    }

    pub(crate) fn set_outcome(&mut self, outcome: &str) {
        let Self::Recording(record) = self else {
            return;
        };
        record.outcome = Some(outcome.into());
    }

    pub(crate) fn set_unchanged_outcome(&mut self, plugin_owned: bool) {
        let Self::Recording(record) = self else {
            return;
        };
        let outcome = record
            .rejection_reason
            .as_deref()
            .unwrap_or(if plugin_owned {
                "already_correct"
            } else {
                "no_op"
            });
        record.outcome = Some(outcome.into());
    }

    pub(crate) fn set_initial_adoption_failure(&mut self) {
        let Self::Recording(record) = self else {
            return;
        };
        record.eligible = Some(false);
        record.rejection_reason = Some(
            record
                .rejection_reason
                .clone()
                .unwrap_or_else(|| "process_or_naming_unavailable".into()),
        );
        record.outcome = record.rejection_reason.clone();
    }

    pub(crate) fn record_naming_pane(&mut self, pane_id: &str, reason: &str) {
        let Self::Recording(record) = self else {
            return;
        };
        record.selected_naming_pane = Some(pane_id.into());
        record.naming_pane_reason = Some(reason.into());
    }

    pub(crate) fn record_process_info(&mut self, pane_id: &str, process_info: &PaneProcessInfo) {
        let Self::Recording(record) = self else {
            return;
        };
        record.process_info = Some(ProcessRecord::from_info(pane_id, process_info));
        record.process_error = None;
    }

    pub(crate) fn record_process_error(&mut self, error: String) {
        let Self::Recording(record) = self else {
            return;
        };
        record.process_error = Some(error);
        record.rejection_reason = Some("process_unavailable".into());
        record.outcome = Some("process_unavailable".into());
    }

    pub(crate) fn record_representative(&mut self, process: &ProcessInfo) {
        let Self::Recording(record) = self else {
            return;
        };
        record.representative_process = Some(ProcessIdentity::from_process(process));
    }

    pub(crate) fn record_representative_selection(
        &mut self,
        process: &ProcessInfo,
        reason: &str,
        process_info: &PaneProcessInfo,
        policy: &NamingPolicy,
    ) {
        let Self::Recording(record) = self else {
            return;
        };
        record.representative_process = Some(ProcessIdentity::from_process(process));
        record.representative_selection = Some(reason.into());
        record.ignored_processes_skipped = process_info
            .foreground_processes
            .iter()
            .filter(|candidate| policy.is_ignored_program(candidate.program()))
            .map(ProcessIdentity::from_process)
            .collect();
    }

    pub(crate) fn record_computed_base(&mut self, computed_base: Option<&str>) {
        let Self::Recording(record) = self else {
            return;
        };
        record.computed_base_label = computed_base.map(str::to_owned);
    }

    pub(crate) fn record_desired_label(&mut self, desired: &str) {
        let Self::Recording(record) = self else {
            return;
        };
        record.desired_label = Some(desired.into());
    }

    pub(crate) fn record_guarded_tab(&mut self, tab: &Tab) {
        let Self::Recording(record) = self else {
            return;
        };
        record.guarded_tab = Some(TabObservation {
            tab_id: tab.tab_id.clone(),
            workspace_id: tab.workspace_id.clone(),
            label: tab.label.clone(),
        });
    }

    pub(crate) fn record_rename_error(&mut self, error: String) {
        let Self::Recording(record) = self else {
            return;
        };
        record.rename_attempted = true;
        record.rename_result = Some(format!("error: {error}"));
        record.outcome = Some("rename_error".into());
    }

    pub(crate) fn record_guard_error(&mut self, error: String) {
        let Self::Recording(record) = self else {
            return;
        };
        record.rename_attempted = false;
        record.rename_result = Some(format!("error: {error}"));
        record.outcome = Some("guard_read_error".into());
    }

    pub(crate) fn record_rename_result(&mut self, result: &str, outcome: &str) {
        let Self::Recording(record) = self else {
            return;
        };
        record.rename_result = Some(result.into());
        record.outcome = Some(outcome.into());
    }

    pub(crate) fn record_rename_success(&mut self) {
        let Self::Recording(record) = self else {
            return;
        };
        record.rename_attempted = true;
        record.rename_result = Some("renamed".into());
        record.outcome = Some("renamed".into());
    }
}

impl DecisionRecord {
    pub(crate) fn new(
        trigger: &str,
        path: &str,
        target_kind: &str,
        workspace_id: Option<String>,
        event_pane_id: Option<String>,
        event_tab_id: Option<String>,
    ) -> Self {
        let target_scope = TargetScope {
            kind: target_kind.into(),
            workspace_id: workspace_id.clone(),
            tab_id: event_tab_id.clone(),
            pane_id: event_pane_id.clone(),
        };
        Self {
            record_type: RECORD_TYPE,
            schema_version: SCHEMA_VERSION,
            trigger: trigger.into(),
            path: path.into(),
            started_monotonic_ns: monotonic_ns(),
            finished_monotonic_ns: None,
            workspace_id,
            event_pane_id,
            event_tab_id,
            available_tab_ids: Vec::new(),
            target_scope,
            snapshot: None,
            pane_mapping: None,
            tab_decisions: Vec::new(),
            lock: LockRecord::default(),
            rerun: RerunRecord {
                handoff: "not_requested".into(),
                ..RerunRecord::default()
            },
            terminal_outcome: "pending".into(),
            process_status: "pending".into(),
            process_error: None,
        }
    }

    fn record_snapshot(&mut self, snapshot: &SessionSnapshot, candidate_tab_ids: &[String]) {
        let tabs = snapshot
            .tabs
            .iter()
            .filter(|tab| {
                self.workspace_id
                    .as_deref()
                    .is_none_or(|workspace| tab.tab.workspace_id == workspace)
            })
            .map(|tab| TabSnapshotRecord {
                tab_id: tab.tab.tab_id.clone(),
                workspace_id: tab.tab.workspace_id.clone(),
                label: tab.tab.label.clone(),
                pane_count: tab.pane_count,
                focused: tab.focused,
            })
            .collect::<Vec<_>>();
        self.available_tab_ids = tabs.iter().map(|tab| tab.tab_id.clone()).collect();
        let tab_ids = tabs
            .iter()
            .map(|tab| tab.tab_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let panes = snapshot
            .panes
            .iter()
            .filter(|pane| tab_ids.contains(pane.tab_id.as_str()))
            .map(|pane| PaneSnapshotRecord {
                pane_id: pane.pane_id.clone(),
                tab_id: pane.tab_id.clone(),
                agent: pane.agent.clone(),
            })
            .collect();
        let closed_pane_present = self
            .event_pane_id
            .as_deref()
            .map(|pane_id| snapshot.panes.iter().any(|pane| pane.pane_id == pane_id));
        self.snapshot = Some(SnapshotRecord {
            tabs,
            panes,
            focused_pane_id: snapshot.focused_pane_id.clone(),
            closed_pane_present,
        });
        self.target_scope.tab_id = match candidate_tab_ids {
            [tab_id] => Some(tab_id.clone()),
            _ => self.target_scope.tab_id.clone(),
        };
    }

    pub(crate) fn finish(&mut self, result: &Result<()>) {
        self.finished_monotonic_ns = Some(monotonic_ns());
        match result {
            Ok(()) => {
                self.process_status = "success".into();
                if self.terminal_outcome == "pending" {
                    self.terminal_outcome = terminal_outcome(&self.tab_decisions);
                }
            }
            Err(error) => {
                self.process_status = "error".into();
                self.process_error = Some(error.to_string());
                if self.terminal_outcome == "pending" {
                    self.terminal_outcome = terminal_outcome(&self.tab_decisions);
                    if self.terminal_outcome == "no_candidate_tabs" {
                        self.terminal_outcome = "error".into();
                    }
                }
            }
        }
    }

    /// Emits one JSON line through the existing plugin command log stream.
    fn emit(&self) {
        if let Ok(line) = serde_json::to_string(self) {
            eprintln!("{line}");
        }
    }
}

fn terminal_outcome(candidates: &[TabDecisionRecord]) -> String {
    let mut outcomes = candidates
        .iter()
        .filter_map(|candidate| candidate.outcome.as_deref())
        .collect::<Vec<_>>();
    outcomes.sort_unstable();
    outcomes.dedup();
    match outcomes.as_slice() {
        [] => "no_candidate_tabs".into(),
        [outcome] => (*outcome).into(),
        outcomes => format!("multiple:{}", outcomes.join(",")),
    }
}

impl ProcessRecord {
    pub(crate) fn from_info(pane_id: &str, info: &PaneProcessInfo) -> Self {
        Self {
            pane_id: pane_id.into(),
            foreground_process_group_id: info.foreground_process_group_id,
            leader: info.leader().map(ProcessIdentity::from_process),
            processes: info
                .foreground_processes
                .iter()
                .map(ProcessIdentity::from_process)
                .collect(),
        }
    }
}

impl ProcessIdentity {
    pub(crate) fn from_process(process: &ProcessInfo) -> Self {
        Self {
            pid: process.pid,
            executable_basename: basename(&process.name),
            argv0_basename: process.argv0.as_deref().map(basename),
        }
    }
}

fn basename(value: &str) -> String {
    Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(value)
        .to_owned()
}

fn monotonic_ns() -> u64 {
    PROCESS_START
        .get_or_init(Instant::now)
        .elapsed()
        .as_nanos()
        .min(u64::MAX as u128) as u64
}

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[cfg(test)]
#[path = "../tests/unit/telemetry.rs"]
mod tests;

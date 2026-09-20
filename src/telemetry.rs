//! In-memory diagnostic decision records emitted through the plugin command log.

use std::error::Error;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

use serde::Serialize;

use crate::config::{Config, Invocation};
use crate::herdr::{PaneProcessInfo, ProcessInfo, SessionSnapshot};
use crate::numbering::Tab;
use crate::state::TabOwnership;

const RECORD_TYPE: &str = "herdr_labels_decision";
const SCHEMA_VERSION: u8 = 1;

static PROCESS_START: OnceLock<Instant> = OnceLock::new();

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
    pub(crate) candidates: Vec<CandidateRecord>,
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
pub(crate) struct CandidateRecord {
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

impl DecisionRecord {
    pub(crate) fn from_config(config: &Config) -> Option<Self> {
        let (trigger, path, target_kind) = match config.event.as_deref() {
            Some("pane.closed") => ("pane.closed", "pane_closed", "workspace"),
            Some("pane.focused" | "tab.focused")
                if matches!(config.invocation, Invocation::Tab { .. }) =>
            {
                (config.event.as_deref().unwrap(), "focus_control", "tab")
            }
            _ if matches!(config.invocation, Invocation::ClosedPane { .. }) => {
                ("pane.closed", "pane_closed", "workspace")
            }
            _ => return None,
        };
        let workspace_id = config
            .event_workspace_id
            .clone()
            .or_else(|| invocation_workspace(&config.invocation));
        let event_pane_id = config.event_pane_id.clone().or_else(|| {
            if let Invocation::ClosedPane { pane_id, .. } = &config.invocation {
                Some(pane_id.clone())
            } else {
                None
            }
        });
        let event_tab_id = config
            .event_tab_id
            .clone()
            .or_else(|| invocation_tab(&config.invocation));
        Some(Self::new(
            trigger,
            path,
            target_kind,
            workspace_id,
            event_pane_id,
            event_tab_id,
        ))
    }

    /// Builds a record from the invocation actually consumed by a deferred pass.
    pub(crate) fn from_invocation(config: &Config, invocation: &Invocation) -> Option<Self> {
        let (trigger, path, target_kind) = match invocation {
            Invocation::ClosedPane { .. } => ("pane.closed", "pane_closed", "workspace"),
            Invocation::Tab { .. }
                if matches!(
                    config.event.as_deref(),
                    Some("pane.focused" | "tab.focused")
                ) =>
            {
                (config.event.as_deref().unwrap(), "focus_control", "tab")
            }
            _ => return None,
        };
        let workspace_id = invocation_workspace(invocation);
        let event_pane_id = match invocation {
            Invocation::ClosedPane { pane_id, .. } => Some(pane_id.clone()),
            _ => config.event_pane_id.clone(),
        };
        let event_tab_id = invocation_tab(invocation);
        Some(Self::new(
            trigger,
            path,
            target_kind,
            workspace_id,
            event_pane_id,
            event_tab_id,
        ))
    }

    fn new(
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
            candidates: Vec::new(),
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

    pub(crate) fn record_snapshot(
        &mut self,
        snapshot: &SessionSnapshot,
        candidate_tab_ids: &[String],
    ) {
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

    pub(crate) fn new_candidate(
        &self,
        tab: &Tab,
        ownership: Option<TabOwnership>,
        pane_count: usize,
        focused: bool,
    ) -> CandidateRecord {
        CandidateRecord {
            tab_id: tab.tab_id.clone(),
            workspace_id: tab.workspace_id.clone(),
            label: tab.label.clone(),
            pane_count,
            focused,
            ownership_before: ownership,
            ..CandidateRecord::default()
        }
    }

    pub(crate) fn push_candidate(&mut self, candidate: CandidateRecord) {
        self.candidates.push(candidate);
    }

    pub(crate) fn record_lock_attempt(&mut self, phase: &str, result: &str, wait_ns: u64) {
        self.lock.attempts.push(LockAttempt {
            phase: phase.into(),
            result: result.into(),
            wait_ns,
        });
    }

    pub(crate) fn record_rerun_requested(&mut self) {
        self.rerun.requested = true;
        self.rerun.handoff = "requested".into();
    }

    pub(crate) fn record_handoff(&mut self, outcome: &str) {
        self.rerun.handoff = outcome.into();
    }

    pub(crate) fn set_terminal_outcome(&mut self, outcome: &str) {
        self.terminal_outcome = outcome.into();
        if outcome == "lock_dropped" {
            self.lock.benign_timeout_or_drop = true;
        }
    }

    pub(crate) fn finish(&mut self, result: &Result<()>) {
        self.finished_monotonic_ns = Some(monotonic_ns());
        match result {
            Ok(()) => {
                self.process_status = "success".into();
                if self.terminal_outcome == "pending" {
                    self.terminal_outcome = terminal_outcome(&self.candidates);
                }
            }
            Err(error) => {
                self.process_status = "error".into();
                self.process_error = Some(error.to_string());
                if self.terminal_outcome == "pending" {
                    self.terminal_outcome = terminal_outcome(&self.candidates);
                    if self.terminal_outcome == "no_candidate_tabs" {
                        self.terminal_outcome = "error".into();
                    }
                }
            }
        }
    }

    /// Emits one JSON line through the existing plugin command log stream.
    pub(crate) fn emit(&self) {
        if let Ok(line) = serde_json::to_string(self) {
            eprintln!("{line}");
        }
    }
}

fn terminal_outcome(candidates: &[CandidateRecord]) -> String {
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

fn invocation_workspace(invocation: &Invocation) -> Option<String> {
    match invocation {
        Invocation::Workspace(workspace_id)
        | Invocation::ClosedPane { workspace_id, .. }
        | Invocation::Tab { workspace_id, .. } => Some(workspace_id.clone()),
        _ => None,
    }
}

fn invocation_tab(invocation: &Invocation) -> Option<String> {
    match invocation {
        Invocation::Tab { tab_id, .. } => Some(tab_id.clone()),
        _ => None,
    }
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

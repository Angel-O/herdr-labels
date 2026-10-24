use std::collections::{HashMap, HashSet};
use std::error::Error;

use super::*;
use crate::config::Invocation;
use crate::herdr::{PaneInfo, PaneProcessInfo, ProcessInfo};
use crate::naming::NamingPolicy;
use crate::numbering::Tab;
use crate::reconciliation::TabClient;
use crate::settings::Settings;
use crate::telemetry::TabTelemetry;

struct FakeClient {
    processes: HashMap<String, PaneProcessInfo>,
}

impl TabClient for FakeClient {
    fn snapshot(&mut self) -> std::result::Result<SessionSnapshot, Box<dyn Error>> {
        unreachable!()
    }

    fn get_tab(&mut self, _tab_id: &str) -> std::result::Result<Option<Tab>, Box<dyn Error>> {
        unreachable!()
    }

    fn rename_tab(
        &mut self,
        _tab_id: &str,
        _label: &str,
    ) -> std::result::Result<(), Box<dyn Error>> {
        unreachable!()
    }

    fn pane_process_info(
        &mut self,
        pane_id: &str,
    ) -> std::result::Result<PaneProcessInfo, Box<dyn Error>> {
        self.processes
            .get(pane_id)
            .cloned()
            .ok_or_else(|| format!("missing process fixture for {pane_id}").into())
    }
}

fn policy(settings: &Settings) -> NamingPolicy {
    NamingPolicy {
        hide_idle_shell: settings.hide_idle_shell,
        max_label_chars: settings.max_label_chars,
        shells: settings.shells.iter().cloned().collect::<HashSet<_>>(),
        ignored_processes: settings.ignored_processes.iter().cloned().collect(),
        aliases: settings.process_aliases.clone(),
    }
}

fn tab() -> SessionTab {
    SessionTab {
        tab: Tab {
            tab_id: "w1:t1".into(),
            workspace_id: "w1".into(),
            label: "[1] bash".into(),
        },
        focused: false,
        pane_count: 1,
    }
}

fn snapshot(tab: SessionTab) -> SessionSnapshot {
    SessionSnapshot {
        focused_pane_id: None,
        tabs: vec![tab],
        panes: vec![PaneInfo {
            pane_id: "w1:t1:pane".into(),
            tab_id: "w1:t1".into(),
            agent: None,
        }],
    }
}

fn process_info(program: &str) -> PaneProcessInfo {
    PaneProcessInfo {
        foreground_process_group_id: Some(7),
        foreground_processes: vec![ProcessInfo {
            pid: 7,
            name: program.into(),
            argv0: Some(program.into()),
            argv: None,
        }],
    }
}

#[test]
fn ignored_preexec_uses_the_hook_shell_not_the_login_shell() {
    let session_tab = tab();
    let session = snapshot(session_tab.clone());
    let mut client = FakeClient {
        processes: HashMap::from([("w1:t1:pane".into(), process_info("git"))]),
    };
    let mut telemetry = TabTelemetry::Off;
    let policy = policy(&Settings::default());
    let invocation = Invocation::Preexec {
        pane_id: "w1:t1:pane".into(),
        shell: "bash".into(),
        program: Some("git".into()),
    };

    assert_eq!(
        computed_name(
            &mut client,
            &session,
            &session_tab,
            &invocation,
            &policy,
            "zsh",
            &mut telemetry,
        )
        .unwrap(),
        Some("bash".into())
    );
}

#[test]
fn ambient_ignored_program_does_not_guess_the_active_shell() {
    let session_tab = tab();
    let session = snapshot(session_tab.clone());
    let mut client = FakeClient {
        processes: HashMap::from([("w1:t1:pane".into(), process_info("git"))]),
    };
    let mut telemetry = TabTelemetry::Off;
    let policy = policy(&Settings::default());

    assert_eq!(
        computed_name(
            &mut client,
            &session,
            &session_tab,
            &Invocation::Full,
            &policy,
            "zsh",
            &mut telemetry,
        )
        .unwrap(),
        None
    );
}

#[test]
fn unchanged_owned_label_preserves_a_naming_rejection_outcome() {
    let session_tab = tab();
    let session = snapshot(session_tab.clone());
    let mut client = FakeClient {
        processes: HashMap::from([("w1:t1:pane".into(), process_info("git"))]),
    };
    let policy = policy(&Settings::default());
    let mut telemetry = TabTelemetry::Recording(Box::default());

    assert_eq!(
        computed_name(
            &mut client,
            &session,
            &session_tab,
            &Invocation::Full,
            &policy,
            "zsh",
            &mut telemetry,
        )
        .unwrap(),
        None
    );
    telemetry.set_unchanged_outcome(true);

    let decision = telemetry.recording_mut();
    assert_eq!(
        decision.rejection_reason.as_deref(),
        Some("no_representative_process")
    );
    assert_eq!(
        decision.outcome.as_deref(),
        Some("no_representative_process")
    );
}

#[test]
fn ignored_selection_is_recorded_only_when_trace_is_enabled() {
    let mut settings = Settings::default();
    settings.ignored_processes.push("starship".into());
    let policy = policy(&settings);
    let session_tab = tab();
    let session = snapshot(session_tab.clone());
    let mut client = FakeClient {
        processes: HashMap::from([(
            "w1:t1:pane".into(),
            PaneProcessInfo {
                foreground_process_group_id: Some(7),
                foreground_processes: vec![
                    ProcessInfo {
                        pid: 8,
                        name: "starship".into(),
                        argv0: Some("starship".into()),
                        argv: None,
                    },
                    ProcessInfo {
                        pid: 7,
                        name: "zsh".into(),
                        argv0: Some("zsh".into()),
                        argv: None,
                    },
                ],
            },
        )]),
    };
    let mut telemetry = TabTelemetry::Recording(Box::default());

    let name = computed_name(
        &mut client,
        &session,
        &session_tab,
        &Invocation::Full,
        &policy,
        "zsh",
        &mut telemetry,
    )
    .unwrap();

    assert_eq!(name.as_deref(), Some("zsh"));
    assert_eq!(
        telemetry
            .recording_mut()
            .representative_selection
            .as_deref(),
        Some("shell_leader_fallback")
    );
    assert_eq!(telemetry.recording_mut().ignored_processes_skipped.len(), 1);
    assert_eq!(
        telemetry.recording_mut().ignored_processes_skipped[0].executable_basename,
        "starship"
    );
}

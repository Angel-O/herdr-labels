use std::path::PathBuf;

use super::*;
use crate::config::Config;
use crate::herdr::{PaneInfo, PaneProcessInfo, ProcessInfo, SessionSnapshot, SessionTab};
use crate::numbering::Tab;
use crate::settings::Settings;

fn config(invocation: Invocation, event: Option<&str>) -> Config {
    let settings = Settings {
        diagnostic_telemetry: true,
        ..Settings::default()
    };
    Config {
        socket_path: PathBuf::from("unused.sock"),
        state_dir: PathBuf::from("unused-state"),
        settings,
        invocation,
        event: event.map(str::to_owned),
        event_workspace_id: Some("w1".into()),
        event_tab_id: Some("w1:t1".into()),
        event_pane_id: Some("w1:t1:closed".into()),
    }
}

fn snapshot() -> SessionSnapshot {
    SessionSnapshot {
        focused_pane_id: Some("w1:t1:survivor".into()),
        tabs: vec![SessionTab {
            tab: Tab {
                tab_id: "w1:t1".into(),
                workspace_id: "w1".into(),
                label: "[1] ai board".into(),
            },
            focused: true,
            pane_count: 1,
        }],
        panes: vec![PaneInfo {
            pane_id: "w1:t1:survivor".into(),
            tab_id: "w1:t1".into(),
            agent: None,
        }],
    }
}

#[test]
fn closed_event_record_keeps_context_and_exact_snapshot() {
    let invocation = Invocation::ClosedPane {
        workspace_id: "w1".into(),
        pane_id: "w1:t1:closed".into(),
    };
    let config = config(invocation, Some("pane.closed"));
    let mut record = DecisionRecord::from_config(&config).unwrap();

    record.record_snapshot(&snapshot(), &["w1:t1".into()]);

    assert_eq!(record.trigger, "pane.closed");
    assert_eq!(record.event_pane_id.as_deref(), Some("w1:t1:closed"));
    assert_eq!(record.event_tab_id.as_deref(), Some("w1:t1"));
    assert_eq!(record.available_tab_ids, ["w1:t1"]);
    assert_eq!(record.snapshot.as_ref().unwrap().tabs[0].pane_count, 1);
    assert_eq!(
        record.snapshot.as_ref().unwrap().closed_pane_present,
        Some(false)
    );
}

#[test]
fn deferred_close_record_uses_the_consumed_request_context() {
    let holder = config(
        Invocation::Tab {
            workspace_id: "holder".into(),
            tab_id: "holder:t1".into(),
        },
        Some("tab.focused"),
    );
    let request = Invocation::ClosedPane {
        workspace_id: "deferred".into(),
        pane_id: "deferred:t2:closed".into(),
    };

    let record = DecisionRecord::from_invocation(&holder, &request).unwrap();

    assert_eq!(record.trigger, "pane.closed");
    assert_eq!(record.workspace_id.as_deref(), Some("deferred"));
    assert_eq!(record.event_pane_id.as_deref(), Some("deferred:t2:closed"));
    assert_eq!(record.event_tab_id, None);
    assert_eq!(record.target_scope.kind, "workspace");
}

#[test]
fn focus_control_record_is_distinct_and_parseable() {
    let invocation = Invocation::Tab {
        workspace_id: "w1".into(),
        tab_id: "w1:t1".into(),
    };
    let config = config(invocation, Some("tab.focused"));
    let mut record = DecisionRecord::from_config(&config).unwrap();
    record.finish(&Ok(()));

    let value: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap();
    assert_eq!(value["record_type"], "herdr_labels_decision");
    assert_eq!(value["trigger"], "tab.focused");
    assert_eq!(value["terminal_outcome"], "no_candidate_tabs");
}

#[test]
fn process_record_exposes_only_safe_process_identity() {
    let info = PaneProcessInfo {
        foreground_process_group_id: Some(7),
        foreground_processes: vec![ProcessInfo {
            pid: 7,
            name: "/bin/zsh".into(),
            argv0: Some("/bin/zsh".into()),
            argv: Some(vec!["zsh".into(), "--secret-token".into()]),
        }],
    };

    let record = ProcessRecord::from_info("w1:t1:survivor", &info);
    let json = serde_json::to_string(&record).unwrap();

    assert!(!json.contains("secret-token"));
    let leader = record.leader.as_ref().unwrap();
    assert_eq!(leader.executable_basename, "zsh");
    assert_eq!(leader.argv0_basename.as_deref(), Some("zsh"));
}

use std::collections::HashSet;

use super::*;
use crate::naming::NamingPolicy;
use crate::settings::Settings;

fn policy(settings: &Settings) -> NamingPolicy {
    NamingPolicy {
        hide_idle_shell: settings.hide_idle_shell,
        max_label_chars: settings.max_label_chars,
        shells: settings.shells.iter().cloned().collect::<HashSet<_>>(),
        ignored_processes: settings.ignored_processes.iter().cloned().collect(),
        aliases: settings.process_aliases.clone(),
    }
}

#[test]
fn process_group_leader_is_required() {
    let info = PaneProcessInfo {
        foreground_process_group_id: Some(4),
        foreground_processes: vec![ProcessInfo {
            pid: 3,
            name: "nvim".into(),
            argv0: None,
            argv: None,
        }],
    };
    assert!(info.leader().is_none());
}

#[test]
fn a_non_shell_child_wins_over_a_shell_script_group_leader() {
    let policy = policy(&Settings::default());
    let info = PaneProcessInfo {
        foreground_process_group_id: Some(7),
        foreground_processes: vec![
            ProcessInfo {
                pid: 8,
                name: "opencode".into(),
                argv0: Some("opencode".into()),
                argv: None,
            },
            ProcessInfo {
                pid: 7,
                name: "zsh".into(),
                argv0: Some("zsh".into()),
                argv: Some(vec!["zsh".into(), "opencode-env".into()]),
            },
        ],
    };
    assert_eq!(
        representative_process(&info, &policy, None)
            .unwrap()
            .program(),
        "opencode"
    );
}

#[test]
fn ignored_child_is_skipped_and_shell_leader_is_selected() {
    let mut settings = Settings::default();
    settings.ignored_processes.push("starship".into());
    let policy = policy(&settings);
    let info = PaneProcessInfo {
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
    };

    let selection = representative_process_with_trace(&info, &policy, None).unwrap();
    assert_eq!(selection.process.program(), "zsh");
    assert_eq!(selection.reason, "shell_leader_fallback");
    assert_eq!(
        selection
            .ignored_processes
            .iter()
            .map(|process| process.program())
            .collect::<Vec<_>>(),
        ["starship"]
    );
    let naming_only = representative_process_with_trace_mode(&info, &policy, None, false).unwrap();
    assert!(naming_only.ignored_processes.is_empty());
}

#[test]
fn ignored_non_shell_leader_is_skipped_for_a_later_usable_child() {
    let policy = policy(&Settings::default());
    let info = PaneProcessInfo {
        foreground_process_group_id: Some(7),
        foreground_processes: vec![
            ProcessInfo {
                pid: 7,
                name: "git".into(),
                argv0: Some("git".into()),
                argv: Some(vec!["git".into(), "status".into()]),
            },
            ProcessInfo {
                pid: 8,
                name: "nvim".into(),
                argv0: Some("nvim".into()),
                argv: None,
            },
        ],
    };

    let selection = representative_process_with_trace(&info, &policy, None).unwrap();
    assert_eq!(selection.process.program(), "nvim");
    assert_eq!(selection.reason, "foreground");
}

#[test]
fn a_launched_binary_wins_over_its_node_launcher() {
    let policy = policy(&Settings::default());
    let info = PaneProcessInfo {
        foreground_process_group_id: Some(7),
        foreground_processes: vec![
            ProcessInfo {
                pid: 8,
                name: "codex".into(),
                argv0: Some("codex".into()),
                argv: Some(vec!["/opt/codex/bin/codex".into()]),
            },
            ProcessInfo {
                pid: 7,
                name: "node".into(),
                argv0: Some("node".into()),
                argv: Some(vec!["node".into(), "/usr/local/bin/codex".into()]),
            },
        ],
    };
    assert_eq!(
        representative_process(&info, &policy, None)
            .unwrap()
            .program(),
        "codex"
    );
}

#[test]
fn a_recognized_agent_wins_over_its_descendant_processes() {
    let policy = policy(&Settings::default());
    let info = PaneProcessInfo {
        foreground_process_group_id: Some(7),
        foreground_processes: vec![
            ProcessInfo {
                pid: 10,
                name: "Python".into(),
                argv0: Some("Python".into()),
                argv: Some(vec!["Python".into(), "run-host.py".into()]),
            },
            ProcessInfo {
                pid: 9,
                name: "bash".into(),
                argv0: Some("bash".into()),
                argv: Some(vec!["bash".into(), "run-with-ui.sh".into()]),
            },
            ProcessInfo {
                pid: 8,
                name: "opencode".into(),
                argv0: Some("opencode".into()),
                argv: Some(vec!["opencode".into()]),
            },
            ProcessInfo {
                pid: 7,
                name: "zsh".into(),
                argv0: Some("zsh".into()),
                argv: Some(vec!["zsh".into(), "opencode-env".into()]),
            },
        ],
    };

    assert_eq!(
        representative_process(&info, &policy, Some("opencode"))
            .unwrap()
            .program(),
        "opencode"
    );
}

#[test]
fn launcher_arguments_verify_a_program_before_its_child_appears() {
    let policy = policy(&Settings::default());
    let info = PaneProcessInfo {
        foreground_process_group_id: Some(7),
        foreground_processes: vec![ProcessInfo {
            pid: 7,
            name: "node".into(),
            argv0: Some("node".into()),
            argv: Some(vec!["node".into(), "/usr/local/bin/codex".into()]),
        }],
    };
    assert!(process_group_matches_program(&info, "codex", &policy));
}

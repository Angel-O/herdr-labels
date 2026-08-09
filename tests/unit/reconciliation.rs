use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::herdr::{PaneInfo, ProcessInfo};

static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "herdr-labels-reconcile-test-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

struct FakeClient {
    snapshot: SessionSnapshot,
    current: HashMap<String, Tab>,
    processes: HashMap<String, PaneProcessInfo>,
    renamed: Vec<(String, String)>,
}

impl FakeClient {
    fn new(snapshot: SessionSnapshot, programs: &[(&str, &str)]) -> Self {
        let current = snapshot
            .tabs
            .iter()
            .map(|tab| (tab.tab.tab_id.clone(), tab.tab.clone()))
            .collect();
        let processes = programs
            .iter()
            .map(|(pane, program)| ((*pane).to_owned(), process_info(program)))
            .collect();
        Self {
            snapshot,
            current,
            processes,
            renamed: Vec::new(),
        }
    }
}

impl TabClient for FakeClient {
    fn snapshot(&mut self) -> Result<SessionSnapshot> {
        Ok(self.snapshot.clone())
    }

    fn get_tab(&mut self, tab_id: &str) -> Result<Option<Tab>> {
        Ok(self.current.get(tab_id).cloned())
    }

    fn rename_tab(&mut self, tab_id: &str, label: &str) -> Result<()> {
        self.renamed.push((tab_id.to_owned(), label.to_owned()));
        if let Some(tab) = self.current.get_mut(tab_id) {
            tab.label = label.to_owned();
        }
        Ok(())
    }

    fn pane_process_info(&mut self, pane_id: &str) -> Result<PaneProcessInfo> {
        self.processes
            .get(pane_id)
            .cloned()
            .ok_or_else(|| format!("missing process fixture for {pane_id}").into())
    }
}

fn process_info(program: &str) -> PaneProcessInfo {
    PaneProcessInfo {
        foreground_process_group_id: Some(7),
        foreground_processes: vec![ProcessInfo {
            pid: 7,
            name: program.to_owned(),
            argv0: Some(program.to_owned()),
            argv: None,
        }],
    }
}

fn tab(id: &str, workspace: &str, label: &str, focused: bool) -> SessionTab {
    SessionTab {
        tab: Tab {
            tab_id: id.into(),
            workspace_id: workspace.into(),
            label: label.into(),
        },
        focused,
        pane_count: 1,
    }
}

fn snapshot(tabs: Vec<SessionTab>) -> SessionSnapshot {
    let panes = tabs
        .iter()
        .map(|tab| PaneInfo {
            pane_id: format!("{}:pane", tab.tab.tab_id),
            tab_id: tab.tab.tab_id.clone(),
        })
        .collect();
    SessionSnapshot {
        focused_pane_id: None,
        tabs,
        panes,
    }
}

fn config(state_dir: &TestDir, invocation: Invocation) -> Config {
    Config {
        socket_path: PathBuf::from("unused.sock"),
        state_dir: state_dir.0.clone(),
        settings: Settings::default(),
        invocation,
    }
}

fn set_ownership(directory: &TestDir, ownership: TabOwnership) {
    let mut state = State::load(&directory.0).unwrap();
    state.set_ownership("w1:t1", ownership);
    state.persist().unwrap();
}

#[test]
fn positions_are_independent_per_workspace_and_continue_after_nine() {
    let mut tabs = (1..=11)
        .map(|position| tab(&format!("w1:t{position}"), "w1", "name", false))
        .collect::<Vec<_>>();
    tabs.push(tab("w2:t1", "w2", "other", false));
    let positions = tab_positions(&snapshot(tabs));
    assert_eq!(positions["w1:t10"], 10);
    assert_eq!(positions["w1:t11"], 11);
    assert_eq!(positions["w2:t1"], 1);
}

#[test]
fn pane_selection_is_conservative_for_background_splits() {
    let mut split = tab("w1:t1", "w1", "1", false);
    split.pane_count = 2;
    let mut session = snapshot(vec![split.clone()]);
    session.panes.push(PaneInfo {
        pane_id: "other".into(),
        tab_id: "w1:t1".into(),
    });
    assert_eq!(naming_pane(&session, &split), None);

    split.focused = true;
    session.focused_pane_id = Some("other".into());
    assert_eq!(naming_pane(&session, &split), Some("other"));
}

#[test]
fn shell_invocations_follow_the_pane_after_it_moves() {
    let session = snapshot(vec![
        tab("w1:t1", "w1", "one", false),
        tab("w2:t1", "w2", "two", false),
    ]);
    let invocation = Invocation::Preexec {
        pane_id: "w2:t1:pane".into(),
        shell: "zsh".into(),
        program: Some("nvim".into()),
    };
    let targets = scoped_tabs(&session, &invocation);
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].tab.tab_id, "w2:t1");

    let stale = Invocation::Preexec {
        pane_id: "w1:old-pane-id".into(),
        shell: "zsh".into(),
        program: Some("nvim".into()),
    };
    assert!(scoped_tabs(&session, &stale).is_empty());
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
    let policy = naming_policy(&Settings::default());
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
        representative_process(&info, &policy).unwrap().program(),
        "opencode"
    );
}

#[test]
fn a_launched_binary_wins_over_its_node_launcher() {
    let policy = naming_policy(&Settings::default());
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
        representative_process(&info, &policy).unwrap().program(),
        "codex"
    );
}

#[test]
fn launcher_arguments_verify_a_program_before_its_child_appears() {
    let policy = naming_policy(&Settings::default());
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

#[test]
fn ambient_startup_helper_cannot_claim_an_unowned_placeholder() {
    let directory = TestDir::new();
    let mut client = FakeClient::new(
        snapshot(vec![tab("w1:t1", "w1", "1", false)]),
        &[("w1:t1:pane", "startup-helper")],
    );
    let config = config(&directory, Invocation::Full);
    run_pass(&config, &config.invocation, &mut client).unwrap();

    assert!(client.renamed.is_empty());
    assert_eq!(State::load(&directory.0).unwrap().ownership("w1:t1"), None);

    client
        .processes
        .insert("w1:t1:pane".into(), process_info("zsh"));
    run_pass(&config, &config.invocation, &mut client).unwrap();

    assert_eq!(client.renamed, [("w1:t1".into(), "[1] zsh".into())]);
    assert_eq!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(&TabOwnership::Owned {
            last_base: "zsh".into(),
            last_rendered: "[1] zsh".into(),
        })
    );
}

#[test]
fn verified_preexec_can_claim_an_unowned_placeholder() {
    let directory = TestDir::new();
    let mut client = FakeClient::new(
        snapshot(vec![tab("w1:t1", "w1", "1", false)]),
        &[("w1:t1:pane", "bv")],
    );
    let config = config(
        &directory,
        Invocation::Preexec {
            pane_id: "w1:t1:pane".into(),
            shell: "zsh".into(),
            program: Some("bv".into()),
        },
    );

    run_pass(&config, &config.invocation, &mut client).unwrap();

    assert_eq!(client.renamed, [("w1:t1".into(), "[1] bv".into())]);
    assert!(matches!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(TabOwnership::Owned { last_base, .. }) if last_base == "bv"
    ));
}

#[test]
fn ignored_preexec_uses_the_hook_shell_not_the_login_shell() {
    let session_tab = tab("w1:t1", "w1", "[1] bash", false);
    let session = snapshot(vec![session_tab.clone()]);
    let mut client = FakeClient::new(session.clone(), &[("w1:t1:pane", "git")]);
    let policy = naming_policy(&Settings::default());
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
            false,
        )
        .unwrap(),
        Some("bash".into())
    );
}

#[test]
fn ambient_ignored_program_does_not_guess_the_active_shell() {
    let session_tab = tab("w1:t1", "w1", "[1] bash", false);
    let session = snapshot(vec![session_tab.clone()]);
    let mut client = FakeClient::new(session.clone(), &[("w1:t1:pane", "git")]);
    let policy = naming_policy(&Settings::default());

    assert_eq!(
        computed_name(
            &mut client,
            &session,
            &session_tab,
            &Invocation::Full,
            &policy,
            "zsh",
            false,
        )
        .unwrap(),
        None
    );
}

#[test]
fn meaningful_existing_name_is_manual_but_still_numbered() {
    let directory = TestDir::new();
    let mut client = FakeClient::new(
        snapshot(vec![tab("w1:t1", "w1", "tests", false)]),
        &[("w1:t1:pane", "nvim")],
    );
    let config = config(&directory, Invocation::Full);
    run_pass(&config, &config.invocation, &mut client).unwrap();

    assert_eq!(client.renamed, [("w1:t1".into(), "[1] tests".into())]);
    assert_eq!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(&TabOwnership::Manual)
    );
}

#[test]
fn numbering_can_be_disabled_independently() {
    let directory = TestDir::new();
    let mut client = FakeClient::new(
        snapshot(vec![tab("w1:t1", "w1", "1", false)]),
        &[("w1:t1:pane", "zsh")],
    );
    let mut config = config(&directory, Invocation::Full);
    config.settings.number_tabs = false;
    run_pass(&config, &config.invocation, &mut client).unwrap();
    assert_eq!(client.renamed[0].1, "zsh");
}

#[test]
fn an_unexpected_owned_base_becomes_manual() {
    let directory = TestDir::new();
    set_ownership(
        &directory,
        TabOwnership::Owned {
            last_base: "nvim".into(),
            last_rendered: "[1] nvim".into(),
        },
    );
    let mut client = FakeClient::new(
        snapshot(vec![tab("w1:t1", "w1", "[1] release notes", false)]),
        &[("w1:t1:pane", "cargo")],
    );
    let config = config(&directory, Invocation::Full);
    run_pass(&config, &config.invocation, &mut client).unwrap();

    assert!(client.renamed.is_empty());
    assert_eq!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(&TabOwnership::Manual)
    );
}

#[test]
fn hidden_idle_shell_keeps_only_the_number() {
    let directory = TestDir::new();
    let mut client = FakeClient::new(
        snapshot(vec![tab("w1:t1", "w1", "1", false)]),
        &[("w1:t1:pane", "zsh")],
    );
    let mut config = config(
        &directory,
        Invocation::Precmd {
            pane_id: "w1:t1:pane".into(),
            shell: "zsh".into(),
            shell_pid: 7,
        },
    );
    config.settings.hide_idle_shell = true;
    run_pass(&config, &config.invocation, &mut client).unwrap();
    assert_eq!(client.renamed[0].1, "[1]");
}

#[test]
fn stale_precmd_does_not_mistake_a_shell_script_for_the_prompt() {
    let directory = TestDir::new();
    set_ownership(
        &directory,
        TabOwnership::Owned {
            last_base: "nvim".into(),
            last_rendered: "[1] nvim".into(),
        },
    );
    let mut client = FakeClient::new(
        snapshot(vec![tab("w1:t1", "w1", "[1] nvim", false)]),
        &[("w1:t1:pane", "zsh")],
    );
    let config = config(
        &directory,
        Invocation::Precmd {
            pane_id: "w1:t1:pane".into(),
            shell: "zsh".into(),
            shell_pid: 9,
        },
    );

    run_pass(&config, &config.invocation, &mut client).unwrap();

    assert!(client.renamed.is_empty());
    assert!(matches!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(TabOwnership::Owned { last_base, .. }) if last_base == "nvim"
    ));
}

#[test]
fn reset_reclaims_a_manual_tab() {
    let directory = TestDir::new();
    set_ownership(&directory, TabOwnership::Manual);
    let mut client = FakeClient::new(
        snapshot(vec![tab("w1:t1", "w1", "[1] tests", false)]),
        &[("w1:t1:pane", "cargo")],
    );
    let config = config(
        &directory,
        Invocation::Reset {
            workspace_id: Some("w1".into()),
            tab_id: Some("w1:t1".into()),
        },
    );
    run_pass(&config, &config.invocation, &mut client).unwrap();

    assert_eq!(client.renamed[0].1, "[1] cargo");
    assert!(matches!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(TabOwnership::Owned { last_base, .. }) if last_base == "cargo"
    ));
}

#[test]
fn reset_uses_the_current_numbering_configuration() {
    let directory = TestDir::new();
    set_ownership(&directory, TabOwnership::Manual);
    let mut client = FakeClient::new(
        snapshot(vec![tab("w1:t1", "w1", "[1] tests", false)]),
        &[("w1:t1:pane", "cargo")],
    );
    let mut config = config(
        &directory,
        Invocation::Reset {
            workspace_id: Some("w1".into()),
            tab_id: Some("w1:t1".into()),
        },
    );
    config.settings.number_tabs = false;
    run_pass(&config, &config.invocation, &mut client).unwrap();
    assert_eq!(client.renamed[0].1, "cargo");
}

#[test]
fn reset_intent_survives_until_process_information_is_available() {
    let directory = TestDir::new();
    set_ownership(&directory, TabOwnership::Manual);
    let session = snapshot(vec![tab("w1:t1", "w1", "[1] tests", false)]);
    let mut client = FakeClient::new(session, &[]);
    let reset = config(
        &directory,
        Invocation::Reset {
            workspace_id: Some("w1".into()),
            tab_id: Some("w1:t1".into()),
        },
    );
    run_pass(&reset, &reset.invocation, &mut client).unwrap();
    assert_eq!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(&TabOwnership::ResetPending)
    );

    client
        .processes
        .insert("w1:t1:pane".into(), process_info("cargo"));
    let event = config(&directory, Invocation::Full);
    run_pass(&event, &event.invocation, &mut client).unwrap();
    assert_eq!(client.renamed[0].1, "[1] cargo");
}

#[test]
fn failed_owned_transition_retries_without_becoming_manual() {
    let directory = TestDir::new();
    set_ownership(
        &directory,
        TabOwnership::PendingRename {
            observed: "[1] nvim".into(),
            desired: "[1] cargo".into(),
            desired_base: "cargo".into(),
            previous_base: Some("nvim".into()),
            previous_rendered: Some("[1] nvim".into()),
            previous_reset_pending: false,
        },
    );
    let mut client = FakeClient::new(
        snapshot(vec![tab("w1:t1", "w1", "[1] nvim", false)]),
        &[("w1:t1:pane", "cargo")],
    );
    let config = config(&directory, Invocation::Full);
    run_pass(&config, &config.invocation, &mut client).unwrap();

    assert_eq!(client.renamed[0].1, "[1] cargo");
    assert!(matches!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(TabOwnership::Owned { last_base, .. }) if last_base == "cargo"
    ));
}

#[test]
fn own_rename_event_does_not_undo_a_fast_path_name() {
    let directory = TestDir::new();
    set_ownership(
        &directory,
        TabOwnership::Owned {
            last_base: "nvim".into(),
            last_rendered: "[1] nvim".into(),
        },
    );
    let session = snapshot(vec![tab("w1:t1", "w1", "[1] nvim", false)]);
    let mut client = FakeClient::new(session, &[("w1:t1:pane", "zsh")]);
    let renamed = config(
        &directory,
        Invocation::RenamedTab {
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
        },
    );
    run_pass(&renamed, &renamed.invocation, &mut client).unwrap();
    assert!(client.renamed.is_empty());

    let focused = config(
        &directory,
        Invocation::Tab {
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
        },
    );
    run_pass(&focused, &focused.invocation, &mut client).unwrap();
    assert_eq!(client.renamed[0].1, "[1] zsh");
}

#[test]
fn preexec_applies_only_while_that_program_is_foreground() {
    let directory = TestDir::new();
    let owned = || TabOwnership::Owned {
        last_base: "zsh".into(),
        last_rendered: "[1] zsh".into(),
    };
    set_ownership(&directory, owned());
    let invocation = Invocation::Preexec {
        pane_id: "w1:t1:pane".into(),
        shell: "zsh".into(),
        program: Some("nvim".into()),
    };
    let current = snapshot(vec![tab("w1:t1", "w1", "[1] zsh", false)]);
    let mut running = FakeClient::new(current.clone(), &[("w1:t1:pane", "nvim")]);
    let config = config(&directory, invocation);
    run_pass(&config, &config.invocation, &mut running).unwrap();
    assert_eq!(running.renamed[0].1, "[1] nvim");

    set_ownership(&directory, owned());
    let mut finished = FakeClient::new(current, &[("w1:t1:pane", "zsh")]);
    run_pass(&config, &config.invocation, &mut finished).unwrap();
    assert!(finished.renamed.is_empty());
}

#[test]
fn clear_strips_numbers_and_suspends_future_events() {
    let directory = TestDir::new();
    let mut client = FakeClient::new(
        snapshot(vec![tab("w1:t1", "w1", "[1] nvim", false)]),
        &[("w1:t1:pane", "nvim")],
    );
    let clear = config(&directory, Invocation::Clear);
    run_pass(&clear, &clear.invocation, &mut client).unwrap();
    assert_eq!(client.renamed[0].1, "nvim");
    assert!(State::load(&directory.0).unwrap().is_suspended());

    client.snapshot.tabs[0].tab.label = "nvim".into();
    let event = config(&directory, Invocation::Full);
    run_pass(&event, &event.invocation, &mut client).unwrap();
    assert_eq!(client.renamed.len(), 1);
}

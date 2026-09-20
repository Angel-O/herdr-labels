use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::herdr::{PaneInfo, ProcessInfo, SessionTab};
use crate::telemetry::Telemetry;

fn run_pass(config: &Config, invocation: &Invocation, client: &mut impl TabClient) -> Result<()> {
    let mut telemetry = Telemetry::Off;
    super::run_pass(config, invocation, client, &mut telemetry)
}

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
    process_queries: Vec<String>,
    renamed: Vec<(String, String)>,
    get_error: Option<String>,
    rename_error: Option<String>,
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
            process_queries: Vec::new(),
            renamed: Vec::new(),
            get_error: None,
            rename_error: None,
        }
    }
}

impl TabClient for FakeClient {
    fn snapshot(&mut self) -> Result<SessionSnapshot> {
        Ok(self.snapshot.clone())
    }

    fn get_tab(&mut self, tab_id: &str) -> Result<Option<Tab>> {
        if let Some(error) = &self.get_error {
            return Err(error.clone().into());
        }
        Ok(self.current.get(tab_id).cloned())
    }

    fn rename_tab(&mut self, tab_id: &str, label: &str) -> Result<()> {
        if let Some(error) = &self.rename_error {
            return Err(error.clone().into());
        }
        self.renamed.push((tab_id.to_owned(), label.to_owned()));
        if let Some(tab) = self.current.get_mut(tab_id) {
            tab.label = label.to_owned();
        }
        Ok(())
    }

    fn pane_process_info(&mut self, pane_id: &str) -> Result<PaneProcessInfo> {
        self.process_queries.push(pane_id.to_owned());
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
            agent: None,
        })
        .collect();
    SessionSnapshot {
        focused_pane_id: None,
        tabs,
        panes,
    }
}

fn config(state_dir: &TestDir, invocation: Invocation) -> Config {
    let settings = Settings {
        diagnostic_telemetry: true,
        ..Settings::default()
    };
    Config {
        socket_path: PathBuf::from("unused.sock"),
        state_dir: state_dir.0.clone(),
        settings,
        invocation,
        event: None,
        event_workspace_id: None,
        event_tab_id: None,
        event_pane_id: None,
    }
}

fn set_ownership(directory: &TestDir, ownership: TabOwnership) {
    let mut state = State::load(&directory.0).unwrap();
    state.set_ownership("w1:t1", ownership);
    state.persist().unwrap();
}

fn set_pane_mapping(directory: &TestDir, pane_id: &str, tab_id: &str) {
    let mut state = State::load(&directory.0).unwrap();
    state.set_pane_tab(pane_id, tab_id);
    state.persist().unwrap();
}

#[test]
fn unowned_placeholder_waits_for_authoritative_shell_hook() {
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
    assert!(client.renamed.is_empty());

    let precmd = Invocation::Precmd {
        pane_id: "w1:t1:pane".into(),
        shell: "zsh".into(),
        shell_pid: 7,
    };
    run_pass(&config, &precmd, &mut client).unwrap();

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
fn eager_shell_claim_is_owned_before_ambient_events_run() {
    let directory = TestDir::new();
    let mut client = FakeClient::new(
        snapshot(vec![tab("w1:t1", "w1", "1", false)]),
        &[("w1:t1:pane", "zsh")],
    );
    let init = config(
        &directory,
        Invocation::Init {
            pane_id: "w1:t1:pane".into(),
            shell: "zsh".into(),
            shell_pid: 7,
        },
    );

    run_pass(&init, &init.invocation, &mut client).unwrap();

    assert_eq!(client.renamed, [("w1:t1".into(), "[1] zsh".into())]);
    assert_eq!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(&TabOwnership::Owned {
            last_base: "zsh".into(),
            last_rendered: "[1] zsh".into(),
        })
    );

    client.snapshot.tabs[0].tab.label = "[1] zsh".into();
    client
        .processes
        .insert("w1:t1:pane".into(), process_info("locale"));
    let ambient = config(&directory, Invocation::Full);
    run_pass(&ambient, &ambient.invocation, &mut client).unwrap();

    assert_eq!(client.renamed.len(), 1);
    assert_eq!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(&TabOwnership::Owned {
            last_base: "zsh".into(),
            last_rendered: "[1] zsh".into(),
        })
    );
}

#[test]
fn manual_rename_after_eager_startup_is_preserved() {
    let directory = TestDir::new();
    set_ownership(
        &directory,
        TabOwnership::Owned {
            last_base: "zsh".into(),
            last_rendered: "[1] zsh".into(),
        },
    );
    let mut client = FakeClient::new(
        snapshot(vec![tab("w1:t1", "w1", "custom", false)]),
        &[("w1:t1:pane", "locale")],
    );
    let renamed = config(
        &directory,
        Invocation::RenamedTab {
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
        },
    );

    run_pass(&renamed, &renamed.invocation, &mut client).unwrap();

    assert!(client.renamed.is_empty());
    assert_eq!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(&TabOwnership::Manual)
    );
}

#[test]
fn same_base_manual_renames_transfer_ownership() {
    for label in ["zsh", "[9] zsh"] {
        let directory = TestDir::new();
        set_ownership(
            &directory,
            TabOwnership::Owned {
                last_base: "zsh".into(),
                last_rendered: "[1] zsh".into(),
            },
        );
        let mut client = FakeClient::new(
            snapshot(vec![tab("w1:t1", "w1", label, false)]),
            &[("w1:t1:pane", "zsh")],
        );
        let renamed = config(
            &directory,
            Invocation::RenamedTab {
                workspace_id: "w1".into(),
                tab_id: "w1:t1".into(),
            },
        );

        run_pass(&renamed, &renamed.invocation, &mut client).unwrap();

        assert!(client.renamed.is_empty(), "unexpected rename for {label}");
        assert_eq!(
            State::load(&directory.0).unwrap().ownership("w1:t1"),
            Some(&TabOwnership::Manual),
            "ownership for {label}"
        );
    }
}

#[test]
fn prompt_cannot_overwrite_manual_rename_before_its_event_arrives() {
    let directory = TestDir::new();
    set_ownership(
        &directory,
        TabOwnership::Owned {
            last_base: "zsh".into(),
            last_rendered: "[1] zsh".into(),
        },
    );
    let mut client = FakeClient::new(
        snapshot(vec![tab("w1:t1", "w1", "custom", false)]),
        &[("w1:t1:pane", "zsh")],
    );
    let prompt = config(
        &directory,
        Invocation::Precmd {
            pane_id: "w1:t1:pane".into(),
            shell: "zsh".into(),
            shell_pid: 7,
        },
    );

    run_pass(&prompt, &prompt.invocation, &mut client).unwrap();

    assert!(client.renamed.is_empty());
    assert_eq!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(&TabOwnership::Manual)
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
fn sampled_preexec_cannot_claim_an_unowned_placeholder() {
    let directory = TestDir::new();
    let mut client = FakeClient::new(
        snapshot(vec![tab("w1:t1", "w1", "1", false)]),
        &[("w1:t1:pane", "locale")],
    );
    let config = config(
        &directory,
        Invocation::Preexec {
            pane_id: "w1:t1:pane".into(),
            shell: "zsh".into(),
            program: None,
        },
    );

    run_pass(&config, &config.invocation, &mut client).unwrap();

    assert!(client.renamed.is_empty());
    assert_eq!(State::load(&directory.0).unwrap().ownership("w1:t1"), None);
}

#[test]
fn rejected_authoritative_events_leave_an_unowned_placeholder_untouched() {
    let directory = TestDir::new();
    let mut client = FakeClient::new(
        snapshot(vec![tab("w1:t1", "w1", "1", false)]),
        &[("w1:t1:pane", "zsh")],
    );
    let mismatched_preexec = config(
        &directory,
        Invocation::Preexec {
            pane_id: "w1:t1:pane".into(),
            shell: "zsh".into(),
            program: Some("nvim".into()),
        },
    );
    run_pass(
        &mismatched_preexec,
        &mismatched_preexec.invocation,
        &mut client,
    )
    .unwrap();

    let stale_precmd = config(
        &directory,
        Invocation::Precmd {
            pane_id: "w1:t1:pane".into(),
            shell: "zsh".into(),
            shell_pid: 8,
        },
    );
    run_pass(&stale_precmd, &stale_precmd.invocation, &mut client).unwrap();

    let stale_init = config(
        &directory,
        Invocation::Init {
            pane_id: "w1:t1:pane".into(),
            shell: "zsh".into(),
            shell_pid: 8,
        },
    );
    run_pass(&stale_init, &stale_init.invocation, &mut client).unwrap();

    assert!(client.renamed.is_empty());
    assert_eq!(State::load(&directory.0).unwrap().ownership("w1:t1"), None);
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
    let mut config = config(
        &directory,
        Invocation::Precmd {
            pane_id: "w1:t1:pane".into(),
            shell: "zsh".into(),
            shell_pid: 7,
        },
    );
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
fn toggle_disables_automatic_naming_without_changing_the_label() {
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
        &[("w1:t1:pane", "cargo")],
    );
    let config = config(
        &directory,
        Invocation::Toggle {
            workspace_id: Some("w1".into()),
            tab_id: Some("w1:t1".into()),
        },
    );

    run_pass(&config, &config.invocation, &mut client).unwrap();

    assert!(client.renamed.is_empty());
    assert_eq!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(&TabOwnership::AutomaticDisabled)
    );
}

#[test]
fn toggle_reenables_automatic_naming_for_a_manual_tab() {
    let directory = TestDir::new();
    set_ownership(&directory, TabOwnership::Manual);
    let mut client = FakeClient::new(
        snapshot(vec![tab("w1:t1", "w1", "[1] tests", false)]),
        &[("w1:t1:pane", "cargo")],
    );
    let config = config(
        &directory,
        Invocation::Toggle {
            workspace_id: Some("w1".into()),
            tab_id: Some("w1:t1".into()),
        },
    );

    run_pass(&config, &config.invocation, &mut client).unwrap();

    assert_eq!(client.renamed, [("w1:t1".into(), "[1] cargo".into())]);
    assert!(matches!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(TabOwnership::Owned { last_base, .. }) if last_base == "cargo"
    ));
}

#[test]
fn toggle_off_survives_empty_labels_and_numbering_changes() {
    let directory = TestDir::new();
    set_ownership(
        &directory,
        TabOwnership::Owned {
            last_base: String::new(),
            last_rendered: "[2]".into(),
        },
    );
    let session = snapshot(vec![tab("w1:t1", "w1", "[2]", false)]);
    let mut client = FakeClient::new(session, &[]);
    let toggle = config(
        &directory,
        Invocation::Toggle {
            workspace_id: Some("w1".into()),
            tab_id: Some("w1:t1".into()),
        },
    );
    run_pass(&toggle, &toggle.invocation, &mut client).unwrap();

    client.snapshot.tabs[0].tab.label = "[1]".into();
    client.current.get_mut("w1:t1").unwrap().label = "[1]".into();
    let renamed = config(
        &directory,
        Invocation::RenamedTab {
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
        },
    );
    run_pass(&renamed, &renamed.invocation, &mut client).unwrap();

    assert_eq!(client.renamed, [("w1:t1".into(), "[1]".into())]);
    assert_eq!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(&TabOwnership::AutomaticDisabled)
    );
}

#[test]
fn whitespace_rename_reenables_naming_after_toggle_off() {
    let directory = TestDir::new();
    set_ownership(&directory, TabOwnership::AutomaticDisabled);
    let mut client = FakeClient::new(
        snapshot(vec![tab("w1:t1", "w1", "   ", false)]),
        &[("w1:t1:pane", "nvim")],
    );
    let mut config = config(
        &directory,
        Invocation::RenamedTab {
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
        },
    );
    config.settings.number_tabs = false;

    run_pass(&config, &config.invocation, &mut client).unwrap();

    assert_eq!(client.renamed, [("w1:t1".into(), "nvim".into())]);
    assert!(matches!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(TabOwnership::Owned { last_base, .. }) if last_base == "nvim"
    ));
}

#[test]
fn toggle_resolves_a_completed_pending_rename_before_disabling() {
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
        snapshot(vec![tab("w1:t1", "w1", "[1] cargo", false)]),
        &[("w1:t1:pane", "cargo")],
    );
    let config = config(
        &directory,
        Invocation::Toggle {
            workspace_id: Some("w1".into()),
            tab_id: Some("w1:t1".into()),
        },
    );

    run_pass(&config, &config.invocation, &mut client).unwrap();

    assert!(client.renamed.is_empty());
    assert_eq!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(&TabOwnership::AutomaticDisabled)
    );
}

#[test]
fn whitespace_only_manual_label_reenables_naming_without_numbering() {
    let directory = TestDir::new();
    set_ownership(&directory, TabOwnership::Manual);
    let mut client = FakeClient::new(
        snapshot(vec![tab("w1:t1", "w1", "   ", false)]),
        &[("w1:t1:pane", "nvim")],
    );
    let mut config = config(
        &directory,
        Invocation::RenamedTab {
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
        },
    );
    config.settings.number_tabs = false;

    run_pass(&config, &config.invocation, &mut client).unwrap();

    assert_eq!(client.renamed, [("w1:t1".into(), "nvim".into())]);
    assert!(matches!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(TabOwnership::Owned { last_base, .. }) if last_base == "nvim"
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
fn failed_owned_transition_waits_for_an_authoritative_process_event() {
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

    assert!(client.renamed.is_empty());
    assert!(matches!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(TabOwnership::Owned { last_base, .. }) if last_base == "nvim"
    ));
}

#[test]
fn rename_events_do_not_replace_an_owned_semantic_name() {
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
    assert!(matches!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(TabOwnership::Owned { last_base, .. }) if last_base == "nvim"
    ));
}

#[test]
fn focusing_a_pane_updates_an_owned_tab_from_the_active_pane() {
    let directory = TestDir::new();
    set_ownership(
        &directory,
        TabOwnership::Owned {
            last_base: "nvim".into(),
            last_rendered: "[1] nvim".into(),
        },
    );
    let mut split = tab("w1:t1", "w1", "[1] nvim", true);
    split.pane_count = 2;
    let mut session = snapshot(vec![split]);
    session.panes.push(PaneInfo {
        pane_id: "w1:t1:other".into(),
        tab_id: "w1:t1".into(),
        agent: None,
    });
    session.focused_pane_id = Some("w1:t1:other".into());
    let mut client = FakeClient::new(session, &[("w1:t1:pane", "nvim"), ("w1:t1:other", "cargo")]);
    let focused = config(
        &directory,
        Invocation::Tab {
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
        },
    );
    run_pass(&focused, &focused.invocation, &mut client).unwrap();
    assert_eq!(client.renamed, [("w1:t1".into(), "[1] cargo".into())]);
    assert!(matches!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(TabOwnership::Owned { last_base, .. }) if last_base == "cargo"
    ));
}

#[test]
fn native_pane_close_refreshes_an_owned_split_without_focus_change() {
    let directory = TestDir::new();
    set_ownership(
        &directory,
        TabOwnership::Owned {
            last_base: "ai board".into(),
            last_rendered: "[1] ai board".into(),
        },
    );
    set_pane_mapping(&directory, "w1:t1:closed", "w1:t1");
    let mut surviving = tab("w1:t1", "w1", "[1] ai board", false);
    surviving.pane_count = 1;
    let mut session = snapshot(vec![surviving]);
    session.panes[0].pane_id = "w1:t1:survivor".into();
    let mut client = FakeClient::new(session, &[("w1:t1:survivor", "zsh")]);
    let closed = config(
        &directory,
        Invocation::ClosedPane {
            workspace_id: "w1".into(),
            pane_id: "w1:t1:closed".into(),
        },
    );

    run_pass(&closed, &closed.invocation, &mut client).unwrap();

    assert_eq!(client.renamed, [("w1:t1".into(), "[1] zsh".into())]);
    assert!(matches!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(TabOwnership::Owned { last_base, .. }) if last_base == "zsh"
    ));
}

#[test]
fn closed_pane_mapping_scopes_process_query_and_rename_to_one_tab() {
    let directory = TestDir::new();
    set_ownership(
        &directory,
        TabOwnership::Owned {
            last_base: "ai board".into(),
            last_rendered: "[1] ai board".into(),
        },
    );
    set_pane_mapping(&directory, "w1:t1:closed", "w1:t1");
    let session = snapshot(vec![
        tab("w1:t1", "w1", "[1] ai board", false),
        tab("w1:t2", "w1", "[2] other", false),
    ]);
    let mut client = FakeClient::new(session, &[("w1:t1:pane", "zsh")]);
    let closed = config(
        &directory,
        Invocation::ClosedPane {
            workspace_id: "w1".into(),
            pane_id: "w1:t1:closed".into(),
        },
    );

    run_pass(&closed, &closed.invocation, &mut client).unwrap();

    assert_eq!(client.process_queries, ["w1:t1:pane"]);
    assert_eq!(client.renamed, [("w1:t1".into(), "[1] zsh".into())]);
}

#[test]
fn missing_closed_pane_mapping_is_an_observable_no_op() {
    let directory = TestDir::new();
    let session = snapshot(vec![tab("w1:t1", "w1", "[1] ai board", false)]);
    let mut client = FakeClient::new(session, &[("w1:t1:pane", "zsh")]);
    let closed = config(
        &directory,
        Invocation::ClosedPane {
            workspace_id: "w1".into(),
            pane_id: "w1:t1:closed".into(),
        },
    );
    let mut telemetry = crate::runner::telemetry_from_config(&closed);

    super::run_pass(&closed, &closed.invocation, &mut client, &mut telemetry).unwrap();
    telemetry.recording_mut().finish(&Ok(()));
    let record = telemetry.recording_mut();

    assert!(client.process_queries.is_empty());
    assert!(client.renamed.is_empty());
    assert_eq!(record.tab_decisions.len(), 0);
    assert_eq!(record.terminal_outcome, "pane_mapping_missing");
    let mapping = record.pane_mapping.as_ref().unwrap();
    assert_eq!(mapping.resolution, "missing");
    assert_eq!(mapping.validation, "not_run");
    assert!(!mapping.consumed);
}

#[test]
fn mismatched_closed_pane_mapping_is_an_observable_no_op() {
    let directory = TestDir::new();
    set_pane_mapping(&directory, "w1:t1:closed", "w2:t2");
    let session = snapshot(vec![
        tab("w1:t1", "w1", "[1] ai board", false),
        tab("w2:t2", "w2", "[1] other", false),
    ]);
    let mut client = FakeClient::new(session, &[]);
    let closed = config(
        &directory,
        Invocation::ClosedPane {
            workspace_id: "w1".into(),
            pane_id: "w1:t1:closed".into(),
        },
    );
    let mut telemetry = crate::runner::telemetry_from_config(&closed);

    super::run_pass(&closed, &closed.invocation, &mut client, &mut telemetry).unwrap();
    telemetry.recording_mut().finish(&Ok(()));
    let record = telemetry.recording_mut();

    assert!(client.process_queries.is_empty());
    assert!(client.renamed.is_empty());
    assert_eq!(record.terminal_outcome, "pane_mapping_invalid");
    assert_eq!(
        record.pane_mapping.as_ref().unwrap().validation,
        "workspace_mismatch"
    );
}

#[test]
fn closed_pane_refresh_preserves_manual_ownership() {
    let directory = TestDir::new();
    set_ownership(&directory, TabOwnership::Manual);
    set_pane_mapping(&directory, "w1:t1:closed", "w1:t1");
    let session = snapshot(vec![tab("w1:t1", "w1", "[1] manual", false)]);
    let mut client = FakeClient::new(session, &[("w1:t1:pane", "zsh")]);
    let closed = config(
        &directory,
        Invocation::ClosedPane {
            workspace_id: "w1".into(),
            pane_id: "w1:t1:closed".into(),
        },
    );

    run_pass(&closed, &closed.invocation, &mut client).unwrap();

    assert!(client.process_queries.is_empty());
    assert!(client.renamed.is_empty());
    assert_eq!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(&TabOwnership::Manual)
    );
}

#[test]
fn closed_pane_refresh_preserves_disabled_ownership() {
    let directory = TestDir::new();
    set_ownership(&directory, TabOwnership::AutomaticDisabled);
    set_pane_mapping(&directory, "w1:t1:closed", "w1:t1");
    let session = snapshot(vec![tab("w1:t1", "w1", "[1] disabled", false)]);
    let mut client = FakeClient::new(session, &[("w1:t1:pane", "zsh")]);
    let closed = config(
        &directory,
        Invocation::ClosedPane {
            workspace_id: "w1".into(),
            pane_id: "w1:t1:closed".into(),
        },
    );

    run_pass(&closed, &closed.invocation, &mut client).unwrap();

    assert!(client.process_queries.is_empty());
    assert!(client.renamed.is_empty());
    assert_eq!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(&TabOwnership::AutomaticDisabled)
    );
}

#[test]
fn close_telemetry_uses_the_decision_snapshot_and_process_observation() {
    let directory = TestDir::new();
    set_ownership(
        &directory,
        TabOwnership::Owned {
            last_base: "ai board".into(),
            last_rendered: "[1] ai board".into(),
        },
    );
    set_pane_mapping(&directory, "w1:t1:closed", "w1:t1");
    let mut surviving = tab("w1:t1", "w1", "[1] ai board", false);
    surviving.pane_count = 1;
    let mut session = snapshot(vec![surviving]);
    session.panes[0].pane_id = "w1:t1:survivor".into();
    let mut client = FakeClient::new(session, &[("w1:t1:survivor", "zsh")]);
    let mut closed = config(
        &directory,
        Invocation::ClosedPane {
            workspace_id: "w1".into(),
            pane_id: "w1:t1:closed".into(),
        },
    );
    closed.event = Some("pane.closed".into());
    closed.event_workspace_id = Some("w1".into());
    closed.event_pane_id = Some("w1:t1:closed".into());
    let mut telemetry = crate::runner::telemetry_from_config(&closed);

    super::run_pass(&closed, &closed.invocation, &mut client, &mut telemetry).unwrap();
    telemetry.recording_mut().finish(&Ok(()));
    let record = telemetry.recording_mut();

    let candidate = &record.tab_decisions[0];
    let mapping = record.pane_mapping.as_ref().unwrap();
    assert_eq!(mapping.mapped_tab_id.as_deref(), Some("w1:t1"));
    assert_eq!(mapping.resolution, "resolved");
    assert_eq!(mapping.validation, "valid");
    assert!(mapping.consumed);
    assert_eq!(
        State::load(&directory.0).unwrap().pane_tab("w1:t1:closed"),
        None
    );
    assert_eq!(
        record.snapshot.as_ref().unwrap().closed_pane_present,
        Some(false)
    );
    assert_eq!(
        candidate.selected_naming_pane.as_deref(),
        Some("w1:t1:survivor")
    );
    assert_eq!(
        candidate.process_info.as_ref().unwrap().pane_id,
        "w1:t1:survivor"
    );
    assert_eq!(
        candidate
            .representative_process
            .as_ref()
            .unwrap()
            .executable_basename,
        "zsh"
    );
    assert_eq!(candidate.desired_label.as_deref(), Some("[1] zsh"));
    assert_eq!(candidate.rename_result.as_deref(), Some("renamed"));
    assert_eq!(record.terminal_outcome, "renamed");
    serde_json::from_str::<serde_json::Value>(&serde_json::to_string(&record).unwrap()).unwrap();
}

#[test]
fn guarded_tab_read_failure_keeps_the_real_candidate_trace_without_claiming_a_rename() {
    let directory = TestDir::new();
    set_ownership(
        &directory,
        TabOwnership::Owned {
            last_base: "ai board".into(),
            last_rendered: "[1] ai board".into(),
        },
    );
    set_pane_mapping(&directory, "w1:t1:closed", "w1:t1");
    let mut session = snapshot(vec![tab("w1:t1", "w1", "[1] ai board", false)]);
    session.panes[0].pane_id = "w1:t1:survivor".into();
    let mut client = FakeClient::new(session, &[("w1:t1:survivor", "zsh")]);
    client.get_error = Some("tab.get unavailable".into());
    let closed = config(
        &directory,
        Invocation::ClosedPane {
            workspace_id: "w1".into(),
            pane_id: "w1:t1:closed".into(),
        },
    );
    let mut telemetry = crate::runner::telemetry_from_config(&closed);

    let error =
        super::run_pass(&closed, &closed.invocation, &mut client, &mut telemetry).unwrap_err();
    telemetry.recording_mut().finish(&Err(error));
    let record = telemetry.recording_mut();

    let candidate = &record.tab_decisions[0];
    assert_eq!(
        candidate.selected_naming_pane.as_deref(),
        Some("w1:t1:survivor")
    );
    assert!(candidate.process_info.is_some());
    assert_eq!(candidate.desired_label.as_deref(), Some("[1] zsh"));
    assert!(candidate.guarded_tab.is_none());
    assert!(matches!(
        candidate.ownership_after,
        Some(TabOwnership::PendingRename { .. })
    ));
    assert!(matches!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(TabOwnership::PendingRename { .. })
    ));
    assert!(!candidate.rename_attempted);
    assert!(
        candidate
            .rename_result
            .as_deref()
            .is_some_and(|result| result.contains("tab.get unavailable"))
    );
    assert_eq!(candidate.outcome.as_deref(), Some("guard_read_error"));
}

#[test]
fn missing_tab_keeps_the_persisted_pending_rename_in_telemetry() {
    let directory = TestDir::new();
    set_ownership(
        &directory,
        TabOwnership::Owned {
            last_base: "ai board".into(),
            last_rendered: "[1] ai board".into(),
        },
    );
    set_pane_mapping(&directory, "w1:t1:closed", "w1:t1");
    let mut session = snapshot(vec![tab("w1:t1", "w1", "[1] ai board", false)]);
    session.panes[0].pane_id = "w1:t1:survivor".into();
    let mut client = FakeClient::new(session, &[("w1:t1:survivor", "zsh")]);
    client.current.clear();
    let closed = config(
        &directory,
        Invocation::ClosedPane {
            workspace_id: "w1".into(),
            pane_id: "w1:t1:closed".into(),
        },
    );
    let mut telemetry = crate::runner::telemetry_from_config(&closed);

    super::run_pass(&closed, &closed.invocation, &mut client, &mut telemetry).unwrap();
    telemetry.recording_mut().finish(&Ok(()));
    let record = telemetry.recording_mut();

    let candidate = &record.tab_decisions[0];
    assert_eq!(candidate.rename_result.as_deref(), Some("tab_not_found"));
    assert!(matches!(
        candidate.ownership_after,
        Some(TabOwnership::PendingRename { .. })
    ));
    assert!(matches!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(TabOwnership::PendingRename { .. })
    ));
    assert_eq!(candidate.outcome.as_deref(), Some("tab_missing"));
}

#[test]
fn tab_rename_failure_keeps_the_guard_observation_and_records_the_attempt() {
    let directory = TestDir::new();
    set_ownership(
        &directory,
        TabOwnership::Owned {
            last_base: "ai board".into(),
            last_rendered: "[1] ai board".into(),
        },
    );
    set_pane_mapping(&directory, "w1:t1:closed", "w1:t1");
    let mut session = snapshot(vec![tab("w1:t1", "w1", "[1] ai board", false)]);
    session.panes[0].pane_id = "w1:t1:survivor".into();
    let mut client = FakeClient::new(session, &[("w1:t1:survivor", "zsh")]);
    client.rename_error = Some("tab.rename rejected".into());
    let closed = config(
        &directory,
        Invocation::ClosedPane {
            workspace_id: "w1".into(),
            pane_id: "w1:t1:closed".into(),
        },
    );
    let mut telemetry = crate::runner::telemetry_from_config(&closed);

    let error =
        super::run_pass(&closed, &closed.invocation, &mut client, &mut telemetry).unwrap_err();
    telemetry.recording_mut().finish(&Err(error));
    let record = telemetry.recording_mut();

    let candidate = &record.tab_decisions[0];
    assert_eq!(
        candidate.guarded_tab.as_ref().unwrap().label,
        "[1] ai board"
    );
    assert!(matches!(
        candidate.ownership_after,
        Some(TabOwnership::PendingRename { .. })
    ));
    assert!(matches!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(TabOwnership::PendingRename { .. })
    ));
    assert!(candidate.rename_attempted);
    assert!(
        candidate
            .rename_result
            .as_deref()
            .is_some_and(|result| result.contains("tab.rename rejected"))
    );
    assert_eq!(candidate.outcome.as_deref(), Some("rename_error"));
}

#[test]
fn exiting_a_focused_pane_updates_an_owned_tab_from_the_survivor() {
    let directory = TestDir::new();
    set_ownership(
        &directory,
        TabOwnership::Owned {
            last_base: "nvim".into(),
            last_rendered: "[1] nvim".into(),
        },
    );
    let mut surviving = tab("w1:t1", "w1", "[1] nvim", true);
    surviving.pane_count = 1;
    let mut session = snapshot(vec![surviving]);
    session.panes[0].pane_id = "w1:t1:survivor".into();
    let mut client = FakeClient::new(session, &[("w1:t1:survivor", "cargo")]);
    let exited = config(
        &directory,
        Invocation::Tab {
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
        },
    );

    run_pass(&exited, &exited.invocation, &mut client).unwrap();

    assert_eq!(client.renamed, [("w1:t1".into(), "[1] cargo".into())]);
    assert!(matches!(
        State::load(&directory.0).unwrap().ownership("w1:t1"),
        Some(TabOwnership::Owned { last_base, .. }) if last_base == "cargo"
    ));
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

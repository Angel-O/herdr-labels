use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::herdr::{PaneProcessInfo, SessionSnapshot};
use crate::numbering::Tab;
use crate::settings::Settings;

static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "herdr-labels-runner-test-{}-{id}",
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
    tabs: VecDeque<Option<Tab>>,
    fallback_tab: Option<Tab>,
    tab_reads: usize,
    snapshots: usize,
    rerun_on_first_snapshot: Option<PathBuf>,
    rerun_on_every_snapshot: Option<PathBuf>,
}

struct ProcessClient {
    processes: VecDeque<PaneProcessInfo>,
    fallback: PaneProcessInfo,
    observations: usize,
}

struct FailingProcessClient {
    observations: usize,
}

impl FakeClient {
    fn with_tab(tab: Option<Tab>) -> Self {
        Self {
            tabs: VecDeque::new(),
            fallback_tab: tab,
            tab_reads: 0,
            snapshots: 0,
            rerun_on_first_snapshot: None,
            rerun_on_every_snapshot: None,
        }
    }
}

impl TabClient for FakeClient {
    fn snapshot(&mut self) -> Result<SessionSnapshot> {
        self.snapshots += 1;
        if self.snapshots == 1
            && let Some(state_dir) = &self.rerun_on_first_snapshot
        {
            ReconciliationLock::request_rerun(state_dir)?;
        }
        if let Some(state_dir) = &self.rerun_on_every_snapshot {
            ReconciliationLock::request_rerun(state_dir)?;
        }
        Ok(SessionSnapshot {
            focused_pane_id: None,
            tabs: Vec::new(),
            panes: Vec::new(),
        })
    }

    fn get_tab(&mut self, _tab_id: &str) -> Result<Option<Tab>> {
        self.tab_reads += 1;
        Ok(self
            .tabs
            .pop_front()
            .unwrap_or_else(|| self.fallback_tab.clone()))
    }

    fn rename_tab(&mut self, _tab_id: &str, _label: &str) -> Result<()> {
        Ok(())
    }

    fn pane_process_info(&mut self, _pane_id: &str) -> Result<PaneProcessInfo> {
        unreachable!()
    }
}

impl TabClient for ProcessClient {
    fn snapshot(&mut self) -> Result<SessionSnapshot> {
        unreachable!()
    }

    fn get_tab(&mut self, _tab_id: &str) -> Result<Option<Tab>> {
        unreachable!()
    }

    fn rename_tab(&mut self, _tab_id: &str, _label: &str) -> Result<()> {
        unreachable!()
    }

    fn pane_process_info(&mut self, _pane_id: &str) -> Result<PaneProcessInfo> {
        self.observations += 1;
        Ok(self
            .processes
            .pop_front()
            .unwrap_or_else(|| self.fallback.clone()))
    }
}

impl TabClient for FailingProcessClient {
    fn snapshot(&mut self) -> Result<SessionSnapshot> {
        unreachable!()
    }

    fn get_tab(&mut self, _tab_id: &str) -> Result<Option<Tab>> {
        unreachable!()
    }

    fn rename_tab(&mut self, _tab_id: &str, _label: &str) -> Result<()> {
        unreachable!()
    }

    fn pane_process_info(&mut self, _pane_id: &str) -> Result<PaneProcessInfo> {
        self.observations += 1;
        Err("process API unavailable".into())
    }
}

fn tab(label: &str) -> Tab {
    Tab {
        tab_id: "w1:t1".into(),
        workspace_id: "w1".into(),
        label: label.into(),
    }
}

fn process_info(program: &str) -> PaneProcessInfo {
    PaneProcessInfo {
        foreground_process_group_id: Some(7),
        foreground_processes: vec![crate::herdr::ProcessInfo {
            pid: 7,
            name: program.into(),
            argv0: Some(program.into()),
            argv: None,
        }],
    }
}

fn config(directory: &TestDir, invocation: Invocation) -> Config {
    Config {
        socket_path: PathBuf::from("unused.sock"),
        state_dir: directory.0.clone(),
        settings: Settings::default(),
        invocation,
    }
}

#[test]
fn contended_own_rename_is_not_promoted_to_a_full_pass() {
    let directory = TestDir::new();
    let mut state = State::load(&directory.0).unwrap();
    state.set_ownership(
        "w1:t1",
        TabOwnership::PendingRename {
            observed: "[1] zsh".into(),
            desired: "[1] nvim".into(),
            desired_base: "nvim".into(),
            previous_base: Some("zsh".into()),
            previous_rendered: Some("[1] zsh".into()),
            previous_reset_pending: false,
        },
    );
    state.persist().unwrap();
    let mut client = FakeClient::with_tab(Some(tab("[1] nvim")));
    let config = config(
        &directory,
        Invocation::RenamedTab {
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
        },
    );

    assert!(is_owned_rename_event(&config, &mut client).unwrap());
    state.set_ownership(
        "w1:t1",
        TabOwnership::Owned {
            last_base: "nvim".into(),
            last_rendered: "[1] nvim".into(),
        },
    );
    state.persist().unwrap();
    assert!(is_owned_rename_event(&config, &mut client).unwrap());
    state.set_ownership("w1:t1", TabOwnership::Manual);
    state.persist().unwrap();
    assert!(!is_owned_rename_event(&config, &mut client).unwrap());
}

#[test]
fn coalescing_consumes_a_rerun_requested_during_the_first_pass() {
    let directory = TestDir::new();
    let config = config(&directory, Invocation::Full);
    let mut client = FakeClient::with_tab(None);
    client.rerun_on_first_snapshot = Some(directory.0.clone());
    let mut remaining_passes = MAX_RECONCILIATION_PASSES;

    run_coalesced_passes(
        &config,
        &Invocation::Workspace("w1".into()),
        &mut client,
        &mut remaining_passes,
    )
    .unwrap();

    assert_eq!(client.snapshots, 2);
    assert_eq!(remaining_passes, MAX_RECONCILIATION_PASSES - 2);
    assert!(!ReconciliationLock::rerun_requested(&directory.0).unwrap());
}

#[test]
fn continuous_reruns_stop_at_the_process_pass_budget() {
    let directory = TestDir::new();
    let config = config(&directory, Invocation::Full);
    let mut client = FakeClient::with_tab(None);
    client.rerun_on_every_snapshot = Some(directory.0.clone());
    let mut remaining_passes = MAX_RECONCILIATION_PASSES;

    run_coalesced_passes(
        &config,
        &Invocation::Full,
        &mut client,
        &mut remaining_passes,
    )
    .unwrap();

    assert_eq!(client.snapshots, MAX_RECONCILIATION_PASSES);
    assert_eq!(remaining_passes, 0);
    assert!(ReconciliationLock::rerun_requested(&directory.0).unwrap());
}

#[test]
fn handoff_uses_only_the_remaining_process_pass_budget() {
    let directory = TestDir::new();
    let config = config(&directory, Invocation::Full);
    let mut client = FakeClient::with_tab(None);
    client.rerun_on_every_snapshot = Some(directory.0.clone());
    ReconciliationLock::request_rerun(&directory.0).unwrap();
    let mut remaining_passes = 2;

    handoff_after_release(&config, &mut client, &mut remaining_passes).unwrap();

    assert_eq!(client.snapshots, 2);
    assert_eq!(remaining_passes, 0);
    assert!(ReconciliationLock::rerun_requested(&directory.0).unwrap());
}

#[test]
fn invocation_classes_have_distinct_bounded_lock_policies() {
    let init = Invocation::Init {
        pane_id: "pane".into(),
        shell: "zsh".into(),
        shell_pid: 7,
    };
    let shell = Invocation::Precmd {
        pane_id: "pane".into(),
        shell: "zsh".into(),
        shell_pid: 7,
    };
    let focus = Invocation::Tab {
        workspace_id: "w1".into(),
        tab_id: "w1:t1".into(),
    };
    let action = Invocation::Clear;

    assert_eq!(exact_lock_timeout(&init), Some(INIT_LOCK_TIMEOUT));
    assert_eq!(exact_lock_timeout(&shell), Some(SHELL_LOCK_TIMEOUT));
    assert_eq!(exact_lock_timeout(&focus), Some(SHELL_LOCK_TIMEOUT));
    assert_eq!(exact_lock_timeout(&action), Some(ACTION_LOCK_TIMEOUT));
    assert!(timeout_is_benign(&init));
    assert!(timeout_is_benign(&shell));
    assert!(timeout_is_benign(&focus));
    assert!(!timeout_is_benign(&action));
    assert!(exact_lock_timeout(&Invocation::Full).is_none());
}

#[test]
fn contended_init_times_out_without_requesting_a_generic_rerun() {
    let directory = TestDir::new();
    let _lock = ReconciliationLock::acquire(&directory.0).unwrap();
    let config = config(
        &directory,
        Invocation::Init {
            pane_id: "w1:t1:pane".into(),
            shell: "zsh".into(),
            shell_pid: 7,
        },
    );

    run(config).unwrap();

    assert!(!ReconciliationLock::rerun_requested(&directory.0).unwrap());
    assert!(INIT_LOCK_TIMEOUT < Duration::from_secs(1));
}

#[test]
fn contended_shell_update_exits_within_its_short_admission_bound() {
    let directory = TestDir::new();
    let _lock = ReconciliationLock::acquire(&directory.0).unwrap();
    let config = config(
        &directory,
        Invocation::Precmd {
            pane_id: "w1:t1:pane".into(),
            shell: "zsh".into(),
            shell_pid: 7,
        },
    );
    let started = Instant::now();

    run(config).unwrap();

    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!ReconciliationLock::rerun_requested(&directory.0).unwrap());
}

#[test]
fn contended_focus_refresh_is_not_downgraded_to_a_generic_rerun() {
    let directory = TestDir::new();
    let _lock = ReconciliationLock::acquire(&directory.0).unwrap();
    let config = config(
        &directory,
        Invocation::Tab {
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
        },
    );
    let started = Instant::now();

    run(config).unwrap();

    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!ReconciliationLock::rerun_requested(&directory.0).unwrap());
}

#[test]
fn contended_preexec_skips_settling_before_it_touches_the_socket() {
    let directory = TestDir::new();
    let _lock = ReconciliationLock::acquire(&directory.0).unwrap();
    let config = config(
        &directory,
        Invocation::Preexec {
            pane_id: "w1:t1:pane".into(),
            shell: "zsh".into(),
            program: Some("nvim".into()),
        },
    );
    let started = Instant::now();

    run(config).unwrap();

    assert!(started.elapsed() < SHELL_LOCK_TIMEOUT);
    assert!(!ReconciliationLock::rerun_requested(&directory.0).unwrap());
}

#[test]
fn close_settling_stops_when_the_tab_disappears() {
    let mut client = FakeClient::with_tab(None);
    client.tabs = VecDeque::from([Some(tab("work")), None]);

    wait_until_closed(
        &mut client,
        "w1:t1",
        Duration::from_millis(1),
        Duration::ZERO,
    )
    .unwrap();

    assert!(client.tabs.is_empty());
}

#[test]
fn close_settling_stops_when_its_deadline_is_exhausted() {
    let mut client = FakeClient::with_tab(Some(tab("work")));

    let error =
        wait_until_closed(&mut client, "w1:t1", Duration::ZERO, Duration::ZERO).unwrap_err();

    assert!(error.to_string().contains("remained visible"));
    assert_eq!(client.tab_reads, 1);
}

#[test]
fn program_settling_stops_as_soon_as_the_program_appears() {
    let mut client = ProcessClient {
        processes: VecDeque::from([process_info("zsh"), process_info("bv")]),
        fallback: process_info("zsh"),
        observations: 0,
    };

    settle_program(
        &mut client,
        "pane",
        "bv",
        &Settings::default(),
        &[Duration::ZERO, Duration::ZERO],
    );

    assert_eq!(client.observations, 2);
}

#[test]
fn program_settling_has_a_strict_nonzero_production_bound() {
    assert_eq!(PROGRAM_SETTLE_DELAYS.len(), 5);
    assert!(PROGRAM_SETTLE_DELAYS.iter().all(|delay| !delay.is_zero()));
    assert!(PROGRAM_SETTLE_DELAYS.iter().sum::<Duration>() < Duration::from_secs(1));

    let mut client = ProcessClient {
        processes: VecDeque::new(),
        fallback: process_info("zsh"),
        observations: 0,
    };
    settle_program(
        &mut client,
        "pane",
        "bv",
        &Settings::default(),
        &[Duration::ZERO, Duration::ZERO, Duration::ZERO],
    );

    assert_eq!(client.observations, 4);
}

#[test]
fn program_settling_does_not_retry_api_errors() {
    let mut client = FailingProcessClient { observations: 0 };

    settle_program(
        &mut client,
        "pane",
        "bv",
        &Settings::default(),
        &[Duration::ZERO, Duration::ZERO],
    );

    assert_eq!(client.observations, 1);
}

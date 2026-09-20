//! Invocation scheduling, close settling, locking, and event coalescing.

use std::error::Error;
use std::thread;
use std::time::{Duration, Instant};

use crate::config::{Config, Invocation};
use crate::herdr::HerdrClient;
use crate::lock::ReconciliationLock;
use crate::reconciliation::{TabClient, pane_matches_program, run_pass};
use crate::state::{State, TabOwnership};
use crate::telemetry::{DecisionRecord, Telemetry};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const ACTION_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const CLOSE_SETTLE_TIMEOUT: Duration = Duration::from_secs(3);
const CLOSE_RETRY_DELAY: Duration = Duration::from_millis(50);
const INIT_LOCK_TIMEOUT: Duration = Duration::from_millis(100);
const SHELL_LOCK_TIMEOUT: Duration = Duration::from_millis(250);
const SAMPLE_DELAY: Duration = Duration::from_millis(200);
const PROGRAM_SETTLE_DELAYS: [Duration; 5] = [
    Duration::from_millis(25),
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(150),
    Duration::from_millis(200),
];
const MAX_RECONCILIATION_PASSES: usize = 8;

/// Runs one invocation, preserving exact operations and coalescing structural events.
pub(crate) fn run(config: Config) -> Result<()> {
    let mut telemetry = telemetry_from_config(&config);
    let result = run_inner(&config, &mut telemetry);
    telemetry.finish_and_emit(&result);
    result
}

fn run_inner(config: &Config, telemetry: &mut Telemetry) -> Result<()> {
    let mut client = HerdrClient::new(&config.socket_path);
    if let Invocation::ClosedTab {
        tab_id: Some(tab_id),
        ..
    } = &config.invocation
    {
        wait_until_closed(&mut client, tab_id, CLOSE_SETTLE_TIMEOUT, CLOSE_RETRY_DELAY)?;
    }
    if matches!(config.invocation, Invocation::Preexec { .. }) {
        let Some(probe) = ReconciliationLock::try_acquire(&config.state_dir)? else {
            telemetry.set_terminal_outcome("lock_dropped");
            return Ok(());
        };
        // Settling must not hold the session lock: a newer prompt update needs
        // to overtake a stale preexec after a fast command finishes.
        drop(probe);
    }
    match &config.invocation {
        Invocation::Preexec {
            pane_id,
            program: Some(program),
            ..
        } => settle_program(
            &mut client,
            pane_id,
            program,
            &config.settings,
            &PROGRAM_SETTLE_DELAYS,
        ),
        Invocation::Preexec { program: None, .. } => thread::sleep(SAMPLE_DELAY),
        _ => {}
    }

    let mut remaining_passes = MAX_RECONCILIATION_PASSES;
    if let Some(timeout) = exact_lock_timeout(&config.invocation) {
        let started = Instant::now();
        let lock = match ReconciliationLock::acquire_with_timeout(&config.state_dir, timeout) {
            Ok(lock) => {
                telemetry.record_lock_attempt("exact", "acquired", elapsed_ns(started));
                lock
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::TimedOut
                    && timeout_is_benign(&config.invocation) =>
            {
                telemetry.record_lock_attempt("exact", "timeout", elapsed_ns(started));
                telemetry.set_terminal_outcome("lock_dropped");
                return Ok(());
            }
            Err(error) => {
                telemetry.record_lock_attempt("exact", "error", elapsed_ns(started));
                return Err(error.into());
            }
        };
        run_coalesced_passes(
            config,
            &config.invocation,
            &mut client,
            &mut remaining_passes,
            telemetry,
        )?;
        drop(lock);
        return handoff_after_release(config, &mut client, &mut remaining_passes, telemetry);
    }

    let mut handoff = None;
    let started = Instant::now();
    let lock = match ReconciliationLock::try_acquire(&config.state_dir)? {
        Some(lock) => {
            telemetry.record_lock_attempt("try", "acquired", elapsed_ns(started));
            lock
        }
        None => {
            telemetry.record_lock_attempt("try", "busy", elapsed_ns(started));
            if is_owned_rename_event(config, &mut client)? {
                telemetry.set_terminal_outcome("owned_rename_noop");
                return Ok(());
            }
            ReconciliationLock::request_rerun(&config.state_dir, &config.invocation)?;
            telemetry.record_rerun_requested();
            telemetry.record_lock_attempt("queue", "published", 0);
            telemetry.record_handoff("queued");
            telemetry.set_terminal_outcome("queued");
            let queued = Ok(());
            telemetry.finish_and_emit(&queued);
            let started = Instant::now();
            let Some(lock) = ReconciliationLock::try_acquire(&config.state_dir)? else {
                telemetry.record_lock_attempt("handoff", "busy", elapsed_ns(started));
                return Ok(());
            };
            handoff = ReconciliationLock::take_rerun(&config.state_dir)?;
            if let Some(requested) = handoff.as_ref() {
                *telemetry = telemetry_from_invocation(config, requested);
                telemetry.record_rerun_requested();
                telemetry.record_lock_attempt("handoff", "acquired", elapsed_ns(started));
                telemetry.record_handoff("deferred_consumed");
            }
            lock
        }
    };
    let initial = handoff.as_ref().unwrap_or(&config.invocation);
    run_coalesced_passes(
        config,
        initial,
        &mut client,
        &mut remaining_passes,
        telemetry,
    )?;
    drop(lock);
    handoff_after_release(config, &mut client, &mut remaining_passes, telemetry)
}

fn exact_lock_timeout(invocation: &Invocation) -> Option<Duration> {
    match invocation {
        Invocation::Init { .. } => Some(INIT_LOCK_TIMEOUT),
        Invocation::Tab { .. } | Invocation::Preexec { .. } | Invocation::Precmd { .. } => {
            Some(SHELL_LOCK_TIMEOUT)
        }
        Invocation::Clear | Invocation::Reset { .. } | Invocation::Toggle { .. } => {
            Some(ACTION_LOCK_TIMEOUT)
        }
        _ => None,
    }
}

fn timeout_is_benign(invocation: &Invocation) -> bool {
    matches!(
        invocation,
        Invocation::Tab { .. }
            | Invocation::Init { .. }
            | Invocation::Preexec { .. }
            | Invocation::Precmd { .. }
    )
}

fn settle_program(
    client: &mut impl TabClient,
    pane_id: &str,
    program: &str,
    settings: &crate::settings::Settings,
    delays: &[Duration],
) {
    if !matches!(
        pane_matches_program(client, pane_id, program, settings),
        Ok(false)
    ) {
        return;
    }
    for delay in delays {
        thread::sleep(*delay);
        if !matches!(
            pane_matches_program(client, pane_id, program, settings),
            Ok(false)
        ) {
            return;
        }
    }
}

fn handoff_after_release(
    config: &Config,
    client: &mut impl TabClient,
    remaining_passes: &mut usize,
    telemetry: &mut Telemetry,
) -> Result<()> {
    if *remaining_passes == 0 || !ReconciliationLock::rerun_requested(&config.state_dir)? {
        telemetry.record_handoff("not_needed");
        return Ok(());
    }
    let started = Instant::now();
    let Some(lock) = ReconciliationLock::try_acquire(&config.state_dir)? else {
        telemetry.record_lock_attempt("post_release", "busy", elapsed_ns(started));
        telemetry.record_handoff("busy");
        return Ok(());
    };
    let requested = match ReconciliationLock::take_rerun(&config.state_dir) {
        Ok(Some(requested)) => requested,
        Ok(None) => {
            telemetry.record_lock_attempt("post_release", "acquired", elapsed_ns(started));
            telemetry.record_handoff("empty");
            return Ok(());
        }
        Err(error) => {
            let result: Result<()> = Err(error.into());
            telemetry.finish_and_emit(&result);
            return result;
        }
    };
    *telemetry = telemetry_from_invocation(config, &requested);
    telemetry.record_rerun_requested();
    telemetry.record_lock_attempt("post_release", "acquired", elapsed_ns(started));
    telemetry.record_handoff("deferred_consumed");
    run_coalesced_passes(config, &requested, client, remaining_passes, telemetry)?;
    drop(lock);
    Ok(())
}

fn is_owned_rename_event(config: &Config, client: &mut impl TabClient) -> Result<bool> {
    let Invocation::RenamedTab { tab_id, .. } = &config.invocation else {
        return Ok(false);
    };
    let state = State::load(&config.state_dir)?;
    let Some(current) = client.get_tab(tab_id)? else {
        return Ok(true);
    };
    Ok(match state.ownership(tab_id) {
        Some(TabOwnership::PendingRename { desired, .. }) => desired == &current.label,
        Some(TabOwnership::Owned { last_rendered, .. }) => last_rendered == &current.label,
        _ => false,
    })
}

fn run_coalesced_passes(
    config: &Config,
    initial: &Invocation,
    client: &mut impl TabClient,
    remaining_passes: &mut usize,
    telemetry: &mut Telemetry,
) -> Result<()> {
    let mut deferred = initial.clone();
    let mut invocation = &deferred;
    while *remaining_passes > 0 {
        *remaining_passes -= 1;
        let mut pass_telemetry = std::mem::replace(telemetry, Telemetry::Off);
        if matches!(pass_telemetry, Telemetry::Off) {
            pass_telemetry = telemetry_from_invocation(config, invocation);
        }
        let pass_result = run_pass(config, invocation, client, &mut pass_telemetry);
        if let Err(error) = pass_result {
            let result: Result<()> = Err(error);
            pass_telemetry.finish_and_emit(&result);
            return result;
        }
        if *remaining_passes == 0 {
            // Preserve a final marker for the next real event rather than
            // extending this process through another unbounded batch.
            pass_telemetry.finish_and_emit(&Ok(()));
            break;
        }
        let requested = match ReconciliationLock::take_rerun(&config.state_dir) {
            Ok(Some(requested)) => requested,
            Ok(None) => {
                pass_telemetry.finish_and_emit(&Ok(()));
                break;
            }
            Err(error) => {
                let result: Result<()> = Err(error.into());
                pass_telemetry.finish_and_emit(&result);
                return result;
            }
        };
        pass_telemetry.record_rerun_requested();
        pass_telemetry.record_handoff("deferred_published");
        pass_telemetry.finish_and_emit(&Ok(()));
        deferred = requested;
        invocation = &deferred;
        *telemetry = telemetry_from_invocation(config, invocation);
        telemetry.record_rerun_requested();
        telemetry.record_handoff("deferred_consumed");
    }
    Ok(())
}

fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u64::MAX as u128) as u64
}

pub(crate) fn telemetry_from_config(config: &Config) -> Telemetry {
    if !config.settings.diagnostic_telemetry {
        return Telemetry::Off;
    }
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
        _ => return Telemetry::Off,
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
    Telemetry::Recording(Box::new(DecisionRecord::new(
        trigger,
        path,
        target_kind,
        workspace_id,
        event_pane_id,
        event_tab_id,
    )))
}

pub(crate) fn telemetry_from_invocation(config: &Config, invocation: &Invocation) -> Telemetry {
    if !config.settings.diagnostic_telemetry {
        return Telemetry::Off;
    }
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
        _ => return Telemetry::Off,
    };
    let workspace_id = invocation_workspace(invocation);
    let event_pane_id = match invocation {
        Invocation::ClosedPane { pane_id, .. } => Some(pane_id.clone()),
        _ => config.event_pane_id.clone(),
    };
    let event_tab_id = invocation_tab(invocation);
    Telemetry::Recording(Box::new(DecisionRecord::new(
        trigger,
        path,
        target_kind,
        workspace_id,
        event_pane_id,
        event_tab_id,
    )))
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

fn wait_until_closed(
    client: &mut impl TabClient,
    tab_id: &str,
    timeout: Duration,
    retry_delay: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if client.get_tab(tab_id)?.is_none() {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        thread::sleep(retry_delay.min(remaining));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("tab {tab_id} remained visible after its close event"),
    )
    .into())
}

#[cfg(test)]
#[path = "../tests/unit/runner.rs"]
mod tests;

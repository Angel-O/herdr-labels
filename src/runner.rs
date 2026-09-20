//! Invocation scheduling, close settling, locking, and event coalescing.

use std::error::Error;
use std::thread;
use std::time::{Duration, Instant};

use crate::config::{Config, Invocation};
use crate::herdr::HerdrClient;
use crate::lock::ReconciliationLock;
use crate::reconciliation::{TabClient, pane_matches_program, run_pass_with_telemetry};
use crate::state::{State, TabOwnership};
use crate::telemetry::DecisionRecord;

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
    let mut telemetry = DecisionRecord::from_config(&config);
    let result = run_inner(&config, &mut telemetry);
    finish_record(&mut telemetry, &result);
    result
}

fn run_inner(config: &Config, telemetry: &mut Option<DecisionRecord>) -> Result<()> {
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
            if let Some(record) = telemetry.as_mut() {
                record.set_terminal_outcome("lock_dropped");
            }
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
                if let Some(record) = telemetry.as_mut() {
                    record.record_lock_attempt("exact", "acquired", elapsed_ns(started));
                }
                lock
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::TimedOut
                    && timeout_is_benign(&config.invocation) =>
            {
                if let Some(record) = telemetry.as_mut() {
                    record.record_lock_attempt("exact", "timeout", elapsed_ns(started));
                    record.set_terminal_outcome("lock_dropped");
                }
                return Ok(());
            }
            Err(error) => {
                if let Some(record) = telemetry.as_mut() {
                    record.record_lock_attempt("exact", "error", elapsed_ns(started));
                }
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
            if let Some(record) = telemetry.as_mut() {
                record.record_lock_attempt("try", "acquired", elapsed_ns(started));
            }
            lock
        }
        None => {
            if let Some(record) = telemetry.as_mut() {
                record.record_lock_attempt("try", "busy", elapsed_ns(started));
            }
            if is_owned_rename_event(config, &mut client)? {
                if let Some(record) = telemetry.as_mut() {
                    record.set_terminal_outcome("owned_rename_noop");
                }
                return Ok(());
            }
            ReconciliationLock::request_rerun(&config.state_dir, &config.invocation)?;
            if let Some(record) = telemetry.as_mut() {
                record.record_rerun_requested();
                record.record_lock_attempt("queue", "published", 0);
                record.record_handoff("queued");
                record.set_terminal_outcome("queued");
            }
            let queued = Ok(());
            finish_record(telemetry, &queued);
            let started = Instant::now();
            let Some(lock) = ReconciliationLock::try_acquire(&config.state_dir)? else {
                if let Some(record) = telemetry.as_mut() {
                    record.record_lock_attempt("handoff", "busy", elapsed_ns(started));
                }
                return Ok(());
            };
            handoff = ReconciliationLock::take_rerun(&config.state_dir)?;
            if let Some(requested) = handoff.as_ref() {
                *telemetry = DecisionRecord::from_invocation(config, requested);
                if let Some(record) = telemetry.as_mut() {
                    record.record_rerun_requested();
                    record.record_lock_attempt("handoff", "acquired", elapsed_ns(started));
                    record.record_handoff("deferred_consumed");
                }
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
    telemetry: &mut Option<DecisionRecord>,
) -> Result<()> {
    if *remaining_passes == 0 || !ReconciliationLock::rerun_requested(&config.state_dir)? {
        if let Some(record) = telemetry.as_mut() {
            record.record_handoff("not_needed");
        }
        return Ok(());
    }
    let started = Instant::now();
    let Some(lock) = ReconciliationLock::try_acquire(&config.state_dir)? else {
        if let Some(record) = telemetry.as_mut() {
            record.record_lock_attempt("post_release", "busy", elapsed_ns(started));
            record.record_handoff("busy");
        }
        return Ok(());
    };
    let requested = match ReconciliationLock::take_rerun(&config.state_dir) {
        Ok(Some(requested)) => requested,
        Ok(None) => {
            if let Some(record) = telemetry.as_mut() {
                record.record_lock_attempt("post_release", "acquired", elapsed_ns(started));
                record.record_handoff("empty");
            }
            return Ok(());
        }
        Err(error) => {
            let result: Result<()> = Err(error.into());
            finish_record(telemetry, &result);
            return result;
        }
    };
    *telemetry = DecisionRecord::from_invocation(config, &requested);
    if let Some(record) = telemetry.as_mut() {
        record.record_rerun_requested();
        record.record_lock_attempt("post_release", "acquired", elapsed_ns(started));
        record.record_handoff("deferred_consumed");
    }
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
    telemetry: &mut Option<DecisionRecord>,
) -> Result<()> {
    let mut deferred = initial.clone();
    let mut invocation = &deferred;
    while *remaining_passes > 0 {
        *remaining_passes -= 1;
        let mut record = telemetry
            .take()
            .or_else(|| DecisionRecord::from_invocation(config, invocation));
        let pass_result = run_pass_with_telemetry(config, invocation, client, record.as_mut());
        if let Err(error) = pass_result {
            let result: Result<()> = Err(error);
            finish_record(&mut record, &result);
            return result;
        }
        if *remaining_passes == 0 {
            // Preserve a final marker for the next real event rather than
            // extending this process through another unbounded batch.
            finish_record(&mut record, &Ok(()));
            break;
        }
        let requested = match ReconciliationLock::take_rerun(&config.state_dir) {
            Ok(Some(requested)) => requested,
            Ok(None) => {
                finish_record(&mut record, &Ok(()));
                break;
            }
            Err(error) => {
                let result: Result<()> = Err(error.into());
                finish_record(&mut record, &result);
                return result;
            }
        };
        if let Some(record) = record.as_mut() {
            record.record_rerun_requested();
            record.record_handoff("deferred_published");
        }
        finish_record(&mut record, &Ok(()));
        deferred = requested;
        invocation = &deferred;
        *telemetry = DecisionRecord::from_invocation(config, invocation);
        if let Some(record) = telemetry.as_mut() {
            record.record_rerun_requested();
            record.record_handoff("deferred_consumed");
        }
    }
    Ok(())
}

fn finish_record(record: &mut Option<DecisionRecord>, result: &Result<()>) {
    if let Some(mut record) = record.take() {
        record.finish(result);
        record.emit();
    }
}

fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u64::MAX as u128) as u64
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

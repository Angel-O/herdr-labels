//! Invocation scheduling, close settling, locking, and event coalescing.

use std::error::Error;
use std::thread;
use std::time::Duration;

use crate::config::{Config, Invocation};
use crate::herdr::HerdrClient;
use crate::lock::ReconciliationLock;
use crate::reconciliation::{TabClient, pane_matches_program, run_pass};
use crate::state::{State, TabOwnership};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const CLOSE_RETRIES: usize = 60;
const CLOSE_RETRY_DELAY: Duration = Duration::from_millis(50);
const SAMPLE_DELAY: Duration = Duration::from_millis(200);
const PROGRAM_SETTLE_DELAYS: [Duration; 5] = [
    Duration::from_millis(25),
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(150),
    Duration::from_millis(200),
];
const MAX_COALESCED_PASSES: usize = 8;

/// Runs one invocation, preserving exact operations and coalescing structural events.
pub(crate) fn run(config: Config) -> Result<()> {
    let mut client = HerdrClient::new(&config.socket_path);
    if let Invocation::ClosedTab {
        tab_id: Some(tab_id),
        ..
    } = &config.invocation
    {
        wait_until_closed(&mut client, tab_id)?;
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

    if is_exact(&config.invocation) {
        let lock = ReconciliationLock::acquire(&config.state_dir)?;
        run_pass(&config, &config.invocation, &mut client)?;
        if ReconciliationLock::take_rerun(&config.state_dir)? {
            run_coalesced_passes(&config, &Invocation::Full, &mut client)?;
        }
        drop(lock);
        return handoff_after_release(&config, &mut client);
    }

    let mut lock = match ReconciliationLock::try_acquire(&config.state_dir)? {
        Some(lock) => lock,
        None => {
            if is_owned_rename_event(&config, &mut client)? {
                return Ok(());
            }
            ReconciliationLock::request_rerun(&config.state_dir)?;
            let Some(lock) = ReconciliationLock::try_acquire(&config.state_dir)? else {
                return Ok(());
            };
            ReconciliationLock::take_rerun(&config.state_dir)?;
            lock
        }
    };
    let mut initial = &config.invocation;
    loop {
        run_coalesced_passes(&config, initial, &mut client)?;
        drop(lock);
        if !ReconciliationLock::rerun_requested(&config.state_dir)? {
            break;
        }
        let Some(reacquired) = ReconciliationLock::try_acquire(&config.state_dir)? else {
            break;
        };
        lock = reacquired;
        ReconciliationLock::take_rerun(&config.state_dir)?;
        initial = &Invocation::Full;
    }
    Ok(())
}

fn is_exact(invocation: &Invocation) -> bool {
    matches!(
        invocation,
        Invocation::Clear
            | Invocation::Reset { .. }
            | Invocation::Toggle { .. }
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

fn handoff_after_release(config: &Config, client: &mut impl TabClient) -> Result<()> {
    if !ReconciliationLock::rerun_requested(&config.state_dir)? {
        return Ok(());
    }
    let Some(lock) = ReconciliationLock::try_acquire(&config.state_dir)? else {
        return Ok(());
    };
    ReconciliationLock::take_rerun(&config.state_dir)?;
    run_coalesced_passes(config, &Invocation::Full, client)?;
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
) -> Result<()> {
    let mut invocation = initial;
    for pass in 0..MAX_COALESCED_PASSES {
        run_pass(config, invocation, client)?;
        if pass + 1 == MAX_COALESCED_PASSES {
            break;
        }
        if !ReconciliationLock::take_rerun(&config.state_dir)? {
            break;
        }
        invocation = &Invocation::Full;
    }
    Ok(())
}

fn wait_until_closed(client: &mut impl TabClient, tab_id: &str) -> Result<()> {
    for _ in 0..CLOSE_RETRIES {
        if client.get_tab(tab_id)?.is_none() {
            return Ok(());
        }
        thread::sleep(CLOSE_RETRY_DELAY);
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

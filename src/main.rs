//! Process-aware Herdr tab naming and numbering.

mod config;
mod filesystem;
mod herdr;
mod lock;
mod naming;
mod numbering;
mod process_selection;
mod reconciliation;
mod runner;
mod settings;
mod state;
mod tab_reconciliation;
mod targeting;
mod telemetry;

fn main() {
    if let Err(error) = run() {
        eprintln!("herdr-labels: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    runner::run(config::Config::from_env()?)
}

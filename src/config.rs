//! Runtime invocation and typed user settings.

use std::env;
use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::filesystem::absolute_path;
use crate::settings::Settings;

const SOCKET_PATH_ENV: &str = "HERDR_SOCKET_PATH";
const PLUGIN_EVENT_ENV: &str = "HERDR_PLUGIN_EVENT";
const WORKSPACE_ID_ENV: &str = "HERDR_WORKSPACE_ID";
const TAB_ID_ENV: &str = "HERDR_TAB_ID";
const PANE_ID_ENV: &str = "HERDR_PANE_ID";

/// Runtime dependencies and requested operation for one invocation.
pub(crate) struct Config {
    pub(crate) socket_path: PathBuf,
    pub(crate) state_dir: PathBuf,
    pub(crate) settings: Settings,
    pub(crate) invocation: Invocation,
}

/// Operation selected by plugin context or a shell-hook command.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Invocation {
    Full,
    Workspace(String),
    Tab {
        workspace_id: String,
        tab_id: String,
    },
    RenamedTab {
        workspace_id: String,
        tab_id: String,
    },
    ClosedTab {
        workspace_id: Option<String>,
        tab_id: Option<String>,
    },
    Preexec {
        pane_id: String,
        shell: String,
        program: Option<String>,
    },
    Precmd {
        pane_id: String,
        shell: String,
        shell_pid: u32,
    },
    Reset {
        workspace_id: Option<String>,
        tab_id: Option<String>,
    },
    Toggle {
        workspace_id: Option<String>,
        tab_id: Option<String>,
    },
    Clear,
}

impl Config {
    /// Reads the invocation, settings, and session paths from arguments and the environment.
    pub(crate) fn from_env() -> Result<Self, Box<dyn Error>> {
        let socket_path = required_path(SOCKET_PATH_ENV)?;
        let state_dir = session_state_dir(&socket_path)?;
        let args: Vec<String> = env::args().skip(1).collect();
        let invocation = invocation_from(
            &args,
            env::var(PLUGIN_EVENT_ENV).ok().as_deref(),
            env::var(WORKSPACE_ID_ENV).ok(),
            env::var(TAB_ID_ENV).ok(),
            env::var(PANE_ID_ENV).ok(),
        )?;
        let settings = if matches!(invocation, Invocation::Clear) {
            Settings::default()
        } else {
            Settings::load()?
        };

        Ok(Self {
            socket_path,
            state_dir,
            settings,
            invocation,
        })
    }
}

fn required_path(name: &str) -> io::Result<PathBuf> {
    let path = env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other(format!("{name} is not set")))?;
    absolute_path(path, name)
}

fn state_base_dir() -> io::Result<PathBuf> {
    let base = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .ok_or_else(|| io::Error::other("HOME and XDG_STATE_HOME are not set"))?;
    Ok(absolute_path(base, "state base")?.join("herdr-labels"))
}

fn session_state_dir(socket_path: &Path) -> io::Result<PathBuf> {
    let mut digest = Sha256::new();
    digest.update(socket_path.as_os_str().as_encoded_bytes());
    let key = format!("{:x}", digest.finalize());
    Ok(state_base_dir()?.join(&key[..16]))
}

fn invocation_from(
    args: &[String],
    event: Option<&str>,
    workspace_id: Option<String>,
    tab_id: Option<String>,
    pane_id: Option<String>,
) -> io::Result<Invocation> {
    match args {
        [mode] if mode == "clear" => return Ok(Invocation::Clear),
        [mode] if mode == "reset" => {
            return Ok(Invocation::Reset {
                workspace_id,
                tab_id,
            });
        }
        [mode] if mode == "toggle" => {
            return Ok(Invocation::Toggle {
                workspace_id,
                tab_id,
            });
        }
        [mode, shell_flag, shell, action]
            if mode == "preexec" && shell_flag == "--shell" && action == "--sample" =>
        {
            return Ok(Invocation::Preexec {
                pane_id: required_value(PANE_ID_ENV, pane_id)?,
                shell: shell.clone(),
                program: None,
            });
        }
        [mode, shell_flag, shell, action, program]
            if mode == "preexec" && shell_flag == "--shell" && action == "--program" =>
        {
            return Ok(Invocation::Preexec {
                pane_id: required_value(PANE_ID_ENV, pane_id)?,
                shell: shell.clone(),
                program: Some(program.clone()),
            });
        }
        [mode, shell_flag, shell, pid_flag, shell_pid]
            if mode == "precmd" && shell_flag == "--shell" && pid_flag == "--shell-pid" =>
        {
            return Ok(Invocation::Precmd {
                pane_id: required_value(PANE_ID_ENV, pane_id)?,
                shell: shell.clone(),
                shell_pid: shell_pid
                    .parse()
                    .map_err(|_| io::Error::other("--shell-pid must be an unsigned integer"))?,
            });
        }
        [] => {}
        _ => return Err(io::Error::other("invalid herdr-labels invocation")),
    }

    Ok(event_invocation(event, workspace_id, tab_id))
}

fn required_value(name: &str, value: Option<String>) -> io::Result<String> {
    value.ok_or_else(|| io::Error::other(format!("{name} is not set")))
}

fn event_invocation(
    event: Option<&str>,
    workspace_id: Option<String>,
    tab_id: Option<String>,
) -> Invocation {
    match event {
        Some("tab.closed") => Invocation::ClosedTab {
            workspace_id,
            tab_id,
        },
        Some("tab.created" | "tab.moved" | "pane.closed" | "pane.created" | "pane.exited") => {
            workspace_id.map_or(Invocation::Full, Invocation::Workspace)
        }
        Some("pane.moved") => Invocation::Full,
        Some("tab.renamed") => match (workspace_id, tab_id) {
            (Some(workspace_id), Some(tab_id)) => Invocation::RenamedTab {
                workspace_id,
                tab_id,
            },
            (Some(workspace_id), None) => Invocation::Workspace(workspace_id),
            _ => Invocation::Full,
        },
        Some("tab.focused" | "pane.focused") => match (workspace_id, tab_id) {
            (Some(workspace_id), Some(tab_id)) => Invocation::Tab {
                workspace_id,
                tab_id,
            },
            (Some(workspace_id), None) => Invocation::Workspace(workspace_id),
            _ => Invocation::Full,
        },
        _ => Invocation::Full,
    }
}

#[cfg(test)]
#[path = "../tests/unit/config.rs"]
mod tests;

//! Typed user settings and configuration-file discovery.

use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::filesystem::absolute_path;

/// User-configurable tab naming and numbering behavior.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Settings {
    pub(crate) auto_name_tabs: bool,
    pub(crate) number_tabs: bool,
    pub(crate) hide_idle_shell: bool,
    pub(crate) max_label_chars: usize,
    pub(crate) shells: Vec<String>,
    pub(crate) ignored_processes: Vec<String>,
    pub(crate) process_aliases: HashMap<String, String>,
}

impl Settings {
    /// Loads settings from the configured path, or returns defaults when absent.
    pub(crate) fn load() -> Result<Self, Box<dyn Error>> {
        load_settings(&settings_path()?)
    }
}

fn load_settings(path: &Path) -> Result<Settings, Box<dyn Error>> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(toml::from_str(&contents)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(error) => Err(error.into()),
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_name_tabs: true,
            number_tabs: true,
            hide_idle_shell: false,
            max_label_chars: 24,
            shells: ["zsh", "bash", "sh", "dash", "ksh"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            ignored_processes: ["ls", "cat", "pwd", "clear", "git", "direnv", "scutil"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            process_aliases: HashMap::new(),
        }
    }
}

fn settings_path() -> io::Result<PathBuf> {
    if let Some(path) = env::var_os("HERDR_LABELS_CONFIG") {
        return absolute_path(PathBuf::from(path), "HERDR_LABELS_CONFIG");
    }
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| io::Error::other("HOME and XDG_CONFIG_HOME are not set"))?;
    Ok(absolute_path(base, "configuration base")?.join("herdr-labels/config.toml"))
}

#[cfg(test)]
#[path = "../tests/unit/settings.rs"]
mod tests;

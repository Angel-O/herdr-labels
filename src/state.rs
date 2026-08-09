//! Durable per-session tab ownership state.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::filesystem::{ensure_private_directory, reject_symlink};

const STATE_FILE_NAME: &str = "state.json";
const STATE_VERSION: u32 = 1;
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

/// The plugin's relationship to a tab label.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum TabOwnership {
    /// The label belongs to the user and must not be rewritten automatically.
    Manual,
    /// Automatic naming was explicitly disabled while preserving the current label.
    AutomaticDisabled,
    /// The user requested re-adoption, but a usable process name is not available yet.
    ResetPending,
    /// The label was last rendered by the plugin from `last_base`.
    Owned {
        last_base: String,
        last_rendered: String,
    },
    /// A rename was recorded before being sent to Herdr.
    PendingRename {
        observed: String,
        desired: String,
        desired_base: String,
        previous_base: Option<String>,
        previous_rendered: Option<String>,
        previous_reset_pending: bool,
    },
}

/// Result of reconciling a pending rename with the currently observed label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PendingRenameResolution {
    /// Herdr applied the desired label, so ownership was established.
    Owned,
    /// Herdr still reports the old label; the pending entry was removed for retry.
    Retry,
    /// A different label appeared and is now treated as user-owned.
    Manual,
}

/// Versioned state for one Herdr session.
#[derive(Debug)]
pub(crate) struct State {
    path: PathBuf,
    persisted: PersistedState,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedState {
    version: u32,
    suspended: bool,
    tabs: BTreeMap<String, TabOwnership>,
}

impl State {
    /// Loads state from `session_dir`, returning empty active state when absent.
    ///
    /// Malformed data and unsupported versions are reported as
    /// [`io::ErrorKind::InvalidData`] so callers do not accidentally overwrite
    /// ownership information with defaults.
    pub(crate) fn load(session_dir: impl AsRef<Path>) -> io::Result<Self> {
        let path = session_dir.as_ref().join(STATE_FILE_NAME);
        reject_symlink(&path)?;
        let persisted = match fs::read(&path) {
            Ok(contents) => {
                let state: PersistedState = serde_json::from_slice(&contents)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                if state.version != STATE_VERSION {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("unsupported state version {}", state.version),
                    ));
                }
                state
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => PersistedState::default(),
            Err(error) => return Err(error),
        };

        Ok(Self { path, persisted })
    }

    pub(crate) fn is_suspended(&self) -> bool {
        self.persisted.suspended
    }

    pub(crate) fn set_suspended(&mut self, suspended: bool) {
        self.persisted.suspended = suspended;
    }

    pub(crate) fn ownership(&self, tab_id: &str) -> Option<&TabOwnership> {
        self.persisted.tabs.get(tab_id)
    }

    pub(crate) fn set_ownership(
        &mut self,
        tab_id: impl Into<String>,
        ownership: TabOwnership,
    ) -> Option<TabOwnership> {
        self.persisted.tabs.insert(tab_id.into(), ownership)
    }

    pub(crate) fn remove_ownership(&mut self, tab_id: &str) -> Option<TabOwnership> {
        self.persisted.tabs.remove(tab_id)
    }

    /// Removes ownership records for tabs not present in `tab_ids`.
    pub(crate) fn prune_tabs<I, S>(&mut self, tab_ids: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let tab_ids: HashSet<String> = tab_ids
            .into_iter()
            .map(|tab_id| tab_id.as_ref().to_owned())
            .collect();
        self.persisted
            .tabs
            .retain(|tab_id, _| tab_ids.contains(tab_id));
    }

    /// Resolves a recorded rename after reading the tab's current label.
    ///
    /// Returns `None` when the tab has no pending rename.
    pub(crate) fn resolve_pending_rename(
        &mut self,
        tab_id: &str,
        current_label: &str,
    ) -> Option<PendingRenameResolution> {
        let TabOwnership::PendingRename {
            observed,
            desired,
            desired_base,
            previous_base,
            previous_rendered,
            previous_reset_pending,
        } = self.persisted.tabs.get(tab_id)?.clone()
        else {
            return None;
        };

        let resolution = if current_label == desired {
            self.persisted.tabs.insert(
                tab_id.to_owned(),
                TabOwnership::Owned {
                    last_base: desired_base,
                    last_rendered: desired,
                },
            );
            PendingRenameResolution::Owned
        } else if current_label == observed {
            if previous_reset_pending {
                self.persisted
                    .tabs
                    .insert(tab_id.to_owned(), TabOwnership::ResetPending);
            } else if let (Some(last_base), Some(last_rendered)) =
                (previous_base, previous_rendered)
            {
                self.persisted.tabs.insert(
                    tab_id.to_owned(),
                    TabOwnership::Owned {
                        last_base,
                        last_rendered,
                    },
                );
            } else {
                self.persisted.tabs.remove(tab_id);
            }
            PendingRenameResolution::Retry
        } else {
            self.persisted
                .tabs
                .insert(tab_id.to_owned(), TabOwnership::Manual);
            PendingRenameResolution::Manual
        };
        Some(resolution)
    }

    /// Atomically writes the complete state to its session directory.
    pub(crate) fn persist(&self) -> io::Result<()> {
        let directory = self
            .path
            .parent()
            .expect("a state file joined to a session directory has a parent");
        ensure_private_directory(directory)?;
        let contents = serde_json::to_vec_pretty(&self.persisted).map_err(io::Error::other)?;
        let (temp_path, mut temp_file) = create_temp_file(directory)?;

        let result = (|| {
            temp_file.write_all(&contents)?;
            temp_file.write_all(b"\n")?;
            temp_file.flush()?;
            temp_file.sync_all()?;
            drop(temp_file);
            fs::rename(&temp_path, &self.path)?;
            File::open(directory)?.sync_all()
        })();

        if result.is_err() {
            let _ = fs::remove_file(temp_path);
        }
        result
    }
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            suspended: false,
            tabs: BTreeMap::new(),
        }
    }
}

fn create_temp_file(directory: &Path) -> io::Result<(PathBuf, File)> {
    for _ in 0..100 {
        let id = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(
            ".{STATE_FILE_NAME}.{}.{}.tmp",
            std::process::id(),
            id
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique state temporary file",
    ))
}

#[cfg(test)]
#[path = "../tests/unit/state.rs"]
mod tests;

//! Cross-process serialization for reconciliation hooks.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::config::Invocation;
use crate::filesystem::{ensure_private_directory, reject_symlink};

#[cfg(test)]
const LOCK_TIMEOUT: Duration = Duration::from_secs(1);
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(25);
const RERUN_DIRECTORY: &str = "reruns";
const LEGACY_RERUN_FILE: &str = "rerun";

static NEXT_RERUN_ID: AtomicU64 = AtomicU64::new(1);

/// Exclusive reconciliation ownership held until this value is dropped.
pub(crate) struct ReconciliationLock {
    _file: File,
}

impl ReconciliationLock {
    #[cfg(test)]
    pub(crate) fn acquire(state_dir: &Path) -> io::Result<Self> {
        Self::acquire_with_timeout(state_dir, LOCK_TIMEOUT)
    }

    /// Attempts to acquire reconciliation ownership without waiting.
    pub(crate) fn try_acquire(state_dir: &Path) -> io::Result<Option<Self>> {
        ensure_private_directory(state_dir)?;
        let file = open_lock_file(state_dir)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }

    /// Records the requesting event for the current lock holder's next pass.
    pub(crate) fn request_rerun(state_dir: &Path, invocation: &Invocation) -> io::Result<()> {
        let directory = rerun_directory(state_dir)?;
        loop {
            let id = format!(
                "{}-{:020}",
                std::process::id(),
                NEXT_RERUN_ID.fetch_add(1, Ordering::Relaxed)
            );
            let temporary = directory.join(format!(".{id}.tmp"));
            let published = directory.join(format!("{id}.json"));
            let mut file = match OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&temporary)
            {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };
            let result = (|| {
                let requested = match invocation {
                    Invocation::ClosedPane { .. } => invocation,
                    _ => &Invocation::Full,
                };
                serde_json::to_writer(&mut file, requested).map_err(io::Error::other)?;
                file.sync_all()?;
                drop(file);
                match std::fs::hard_link(&temporary, &published) {
                    Ok(()) => {
                        let _ = std::fs::remove_file(&temporary);
                        Ok(())
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        let _ = std::fs::remove_file(&temporary);
                        Err(error)
                    }
                    Err(error) => Err(error),
                }
            })();
            if result.is_ok() {
                return Ok(());
            }
            let _ = std::fs::remove_file(&temporary);
            if let Err(error) = result
                && error.kind() != io::ErrorKind::AlreadyExists
            {
                return Err(error);
            }
        }
    }

    /// Consumes a pending rerun request.
    pub(crate) fn take_rerun(state_dir: &Path) -> io::Result<Option<Invocation>> {
        if let Some(path) = pending_rerun_files(state_dir)?.into_iter().next() {
            return read_and_remove_rerun(&path);
        }
        let legacy = state_dir.join(LEGACY_RERUN_FILE);
        reject_symlink(&legacy)?;
        match std::fs::read(&legacy) {
            Ok(contents) => read_rerun_contents(&legacy, contents),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Returns whether a rerun request is waiting without consuming it.
    pub(crate) fn rerun_requested(state_dir: &Path) -> io::Result<bool> {
        if !pending_rerun_files(state_dir)?.is_empty() {
            return Ok(true);
        }
        let legacy = state_dir.join(LEGACY_RERUN_FILE);
        reject_symlink(&legacy)?;
        match std::fs::metadata(legacy) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn acquire_with_timeout(state_dir: &Path, timeout: Duration) -> io::Result<Self> {
        ensure_private_directory(state_dir)?;
        let file = open_lock_file(state_dir)?;
        let deadline = Instant::now() + timeout;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {
                    if let Some(delay) = retry_delay(deadline, Instant::now()) {
                        std::thread::sleep(delay);
                    } else {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "timed out waiting for another reconciliation",
                        ));
                    }
                }
                Err(TryLockError::Error(error)) => return Err(error),
            }
        }
        Ok(Self { _file: file })
    }
}

fn retry_delay(deadline: Instant, now: Instant) -> Option<Duration> {
    let remaining = deadline.saturating_duration_since(now);
    (!remaining.is_zero()).then_some(LOCK_RETRY_DELAY.min(remaining))
}

fn open_lock_file(state_dir: &Path) -> io::Result<File> {
    reject_symlink(&state_dir.join("reconcile.lock"))?;
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(state_dir.join("reconcile.lock"))
}

fn rerun_directory(state_dir: &Path) -> io::Result<PathBuf> {
    let directory = state_dir.join(RERUN_DIRECTORY);
    ensure_private_directory(&directory)?;
    Ok(directory)
}

fn pending_rerun_files(state_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let directory = state_dir.join(RERUN_DIRECTORY);
    reject_symlink(&directory)?;
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        reject_symlink(&path)?;
        if entry.file_type()?.is_file() && path.extension().is_some_and(|ext| ext == "json") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn read_and_remove_rerun(path: &Path) -> io::Result<Option<Invocation>> {
    let contents = std::fs::read(path)?;
    read_rerun_contents(path, contents)
}

fn read_rerun_contents(path: &Path, contents: Vec<u8>) -> io::Result<Option<Invocation>> {
    let invocation = if contents.is_empty() {
        Invocation::Full
    } else {
        serde_json::from_slice(&contents).map_err(io::Error::other)?
    };
    std::fs::remove_file(path)?;
    Ok(Some(invocation))
}

#[cfg(test)]
#[path = "../tests/unit/lock.rs"]
mod tests;

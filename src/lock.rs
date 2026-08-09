//! Cross-process serialization for reconciliation hooks.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::filesystem::{ensure_private_directory, reject_symlink};

const LOCK_RETRY_DELAY: Duration = Duration::from_millis(25);

/// Exclusive reconciliation ownership held until this value is dropped.
pub(crate) struct ReconciliationLock {
    _file: File,
}

impl ReconciliationLock {
    #[cfg(test)]
    pub(crate) fn acquire(state_dir: &Path) -> io::Result<Self> {
        Self::acquire_with_timeout(state_dir, Duration::from_secs(1))
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

    /// Records that the current lock holder should perform another pass.
    pub(crate) fn request_rerun(state_dir: &Path) -> io::Result<()> {
        ensure_private_directory(state_dir)?;
        reject_symlink(&state_dir.join("rerun"))?;
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(state_dir.join("rerun"))?;
        Ok(())
    }

    /// Consumes a pending rerun request.
    pub(crate) fn take_rerun(state_dir: &Path) -> io::Result<bool> {
        reject_symlink(&state_dir.join("rerun"))?;
        match std::fs::remove_file(state_dir.join("rerun")) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Returns whether a rerun request is waiting without consuming it.
    pub(crate) fn rerun_requested(state_dir: &Path) -> io::Result<bool> {
        reject_symlink(&state_dir.join("rerun"))?;
        match std::fs::metadata(state_dir.join("rerun")) {
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
                Err(TryLockError::WouldBlock)
                    if let Some(delay) = retry_delay(deadline, Instant::now()) =>
                {
                    std::thread::sleep(delay);
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "timed out waiting for another reconciliation",
                    ));
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

#[cfg(test)]
#[path = "../tests/unit/lock.rs"]
mod tests;

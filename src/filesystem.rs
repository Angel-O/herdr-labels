//! Shared path validation and private filesystem policy.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Accepts an absolute path and rejects process-relative filesystem state.
pub(crate) fn absolute_path(path: PathBuf, name: &str) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(io::Error::other(format!("{name} must be an absolute path")))
    }
}

/// Creates a private directory without accepting a symlink at its final path.
pub(crate) fn ensure_private_directory(directory: &Path) -> io::Result<()> {
    reject_symlink(directory)?;
    fs::create_dir_all(directory)?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
}

/// Rejects a symlink at `path`, while allowing a path that does not exist yet.
pub(crate) fn reject_symlink(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::other(format!(
            "refusing symlinked path {}",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[path = "../tests/unit/filesystem.rs"]
mod tests;

use std::os::unix::fs::{MetadataExt, symlink};

use super::*;

#[test]
fn absolute_paths_are_required() {
    assert!(absolute_path(PathBuf::from("relative"), "test").is_err());
    assert_eq!(
        absolute_path(PathBuf::from("/absolute"), "test").unwrap(),
        PathBuf::from("/absolute")
    );
}

#[test]
fn private_directories_use_owner_only_permissions() {
    let path = std::env::temp_dir().join(format!(
        "herdr-labels-filesystem-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    ensure_private_directory(&path).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o700);
    fs::remove_dir(path).unwrap();
}

#[test]
fn final_path_symlinks_are_rejected() {
    let root =
        std::env::temp_dir().join(format!("herdr-labels-symlink-test-{}", std::process::id()));
    let target = root.join("target");
    let link = root.join("link");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root).unwrap();
    fs::create_dir(&target).unwrap();
    symlink(&target, &link).unwrap();
    assert!(reject_symlink(&link).is_err());
    fs::remove_dir_all(root).unwrap();
}

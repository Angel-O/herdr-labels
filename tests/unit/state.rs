use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "herdr-labels-state-test-{}-{id}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn child(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn pending() -> TabOwnership {
    TabOwnership::PendingRename {
        observed: "work".into(),
        desired: "[1] work".into(),
        desired_base: "work".into(),
        previous_base: None,
        previous_rendered: None,
        previous_reset_pending: false,
    }
}

#[test]
fn absent_file_loads_empty_active_state_without_creating_a_directory() {
    let root = TestDir::new();
    let session_dir = root.child("session");

    let state = State::load(&session_dir).unwrap();

    assert!(!state.is_suspended());
    assert!(state.ownership("tab-1").is_none());
    assert!(!session_dir.exists());
}

#[test]
fn all_fields_and_ownership_variants_round_trip() {
    let root = TestDir::new();
    let session_dir = root.child("nested/session");
    let mut state = State::load(&session_dir).unwrap();
    state.set_suspended(true);
    state.set_ownership("manual", TabOwnership::Manual);
    state.set_ownership("disabled", TabOwnership::AutomaticDisabled);
    state.set_ownership(
        "owned",
        TabOwnership::Owned {
            last_base: "build".into(),
            last_rendered: "[2] build".into(),
        },
    );
    state.set_ownership("pending", pending());

    state.persist().unwrap();
    let loaded = State::load(&session_dir).unwrap();

    assert!(loaded.is_suspended());
    assert_eq!(loaded.ownership("manual"), Some(&TabOwnership::Manual));
    assert_eq!(
        loaded.ownership("disabled"),
        Some(&TabOwnership::AutomaticDisabled)
    );
    assert_eq!(
        loaded.ownership("owned"),
        Some(&TabOwnership::Owned {
            last_base: "build".into(),
            last_rendered: "[2] build".into(),
        })
    );
    assert_eq!(loaded.ownership("pending"), Some(&pending()));
}

#[test]
fn malformed_and_unsupported_state_fail_conservatively() {
    let root = TestDir::new();
    fs::write(root.child(STATE_FILE_NAME), b"not json").unwrap();
    assert_eq!(
        State::load(&root.0).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );

    fs::write(
        root.child(STATE_FILE_NAME),
        br#"{"version":2,"suspended":false,"tabs":{}}"#,
    )
    .unwrap();
    assert_eq!(
        State::load(&root.0).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn ownership_can_be_set_removed_and_pruned() {
    let root = TestDir::new();
    let mut state = State::load(&root.0).unwrap();
    assert_eq!(state.set_ownership("one", TabOwnership::Manual), None);
    state.set_ownership("two", pending());
    state.set_ownership("three", TabOwnership::Manual);

    assert_eq!(
        state.set_ownership("one", pending()),
        Some(TabOwnership::Manual)
    );
    assert_eq!(state.remove_ownership("three"), Some(TabOwnership::Manual));
    state.prune_tabs(["two"]);

    assert!(state.ownership("one").is_none());
    assert_eq!(state.ownership("two"), Some(&pending()));
    assert!(state.ownership("three").is_none());
}

#[test]
fn desired_pending_label_promotes_to_owned() {
    let root = TestDir::new();
    let mut state = State::load(&root.0).unwrap();
    state.set_ownership("tab", pending());

    assert_eq!(
        state.resolve_pending_rename("tab", "[1] work"),
        Some(PendingRenameResolution::Owned)
    );
    assert_eq!(
        state.ownership("tab"),
        Some(&TabOwnership::Owned {
            last_base: "work".into(),
            last_rendered: "[1] work".into(),
        })
    );
}

#[test]
fn observed_pending_label_is_removed_for_retry() {
    let root = TestDir::new();
    let mut state = State::load(&root.0).unwrap();
    state.set_ownership("tab", pending());

    assert_eq!(
        state.resolve_pending_rename("tab", "work"),
        Some(PendingRenameResolution::Retry)
    );
    assert!(state.ownership("tab").is_none());
}

#[test]
fn observed_reset_rename_restores_pending_reset_intent() {
    let root = TestDir::new();
    let mut state = State::load(&root.0).unwrap();
    state.set_ownership(
        "tab",
        TabOwnership::PendingRename {
            observed: "[1] manual".into(),
            desired: "[1] cargo".into(),
            desired_base: "cargo".into(),
            previous_base: None,
            previous_rendered: None,
            previous_reset_pending: true,
        },
    );

    assert_eq!(
        state.resolve_pending_rename("tab", "[1] manual"),
        Some(PendingRenameResolution::Retry)
    );
    assert_eq!(state.ownership("tab"), Some(&TabOwnership::ResetPending));
}

#[test]
fn unexpected_pending_label_becomes_manual() {
    let root = TestDir::new();
    let mut state = State::load(&root.0).unwrap();
    state.set_ownership("tab", pending());

    assert_eq!(
        state.resolve_pending_rename("tab", "user edit"),
        Some(PendingRenameResolution::Manual)
    );
    assert_eq!(state.ownership("tab"), Some(&TabOwnership::Manual));
}

#[test]
fn recovery_ignores_non_pending_and_unknown_tabs() {
    let root = TestDir::new();
    let mut state = State::load(&root.0).unwrap();
    state.set_ownership("manual", TabOwnership::Manual);

    assert_eq!(state.resolve_pending_rename("manual", "anything"), None);
    assert_eq!(state.resolve_pending_rename("missing", "anything"), None);
    assert_eq!(state.ownership("manual"), Some(&TabOwnership::Manual));
}

#[test]
fn persistence_replaces_the_state_and_leaves_no_temp_file() {
    let root = TestDir::new();
    let mut state = State::load(&root.0).unwrap();
    state.set_ownership("old", TabOwnership::Manual);
    state.persist().unwrap();
    state.remove_ownership("old");
    state.set_ownership("new", pending());

    state.persist().unwrap();

    let loaded = State::load(&root.0).unwrap();
    assert!(loaded.ownership("old").is_none());
    assert_eq!(loaded.ownership("new"), Some(&pending()));
    let files: Vec<_> = fs::read_dir(&root.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(files, [STATE_FILE_NAME]);
}

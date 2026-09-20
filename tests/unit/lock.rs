use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(1);

#[test]
fn excludes_a_second_holder() {
    let state_dir = test_state_dir();
    let first = ReconciliationLock::acquire(&state_dir).unwrap();

    let error = ReconciliationLock::acquire_with_timeout(&state_dir, Duration::from_millis(20))
        .err()
        .expect("second holder should time out");

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    drop(first);
    std::fs::remove_dir_all(state_dir).unwrap();
}

#[test]
fn releases_the_lock_when_dropped() {
    let state_dir = test_state_dir();
    let first = ReconciliationLock::acquire(&state_dir).unwrap();
    drop(first);

    let second = ReconciliationLock::acquire_with_timeout(&state_dir, Duration::from_millis(20))
        .expect("lock should be available after its holder is dropped");

    drop(second);
    std::fs::remove_dir_all(state_dir).unwrap();
}

#[test]
fn contender_can_claim_its_marker_after_holder_releases() {
    let state_dir = test_state_dir();
    let first = ReconciliationLock::try_acquire(&state_dir)
        .unwrap()
        .expect("first holder");
    assert!(
        ReconciliationLock::try_acquire(&state_dir)
            .unwrap()
            .is_none()
    );
    ReconciliationLock::request_rerun(&state_dir, &Invocation::Full).unwrap();
    drop(first);

    let second = ReconciliationLock::try_acquire(&state_dir)
        .unwrap()
        .expect("contender should acquire after release");
    assert!(
        ReconciliationLock::take_rerun(&state_dir)
            .unwrap()
            .is_some()
    );
    drop(second);
    std::fs::remove_dir_all(state_dir).unwrap();
}

#[test]
fn rerun_marker_preserves_a_closed_pane_request_scope() {
    let state_dir = test_state_dir();
    let requested = Invocation::ClosedPane {
        workspace_id: "w2".into(),
        pane_id: "w2:p1".into(),
    };

    ReconciliationLock::request_rerun(&state_dir, &requested).unwrap();

    assert_eq!(
        ReconciliationLock::take_rerun(&state_dir).unwrap(),
        Some(requested)
    );
    std::fs::remove_dir_all(state_dir).unwrap();
}

#[test]
fn close_followed_by_full_keeps_both_requests_in_order() {
    let state_dir = test_state_dir();
    let closed = Invocation::ClosedPane {
        workspace_id: "w1".into(),
        pane_id: "w1:p1".into(),
    };

    ReconciliationLock::request_rerun(&state_dir, &closed).unwrap();
    ReconciliationLock::request_rerun(&state_dir, &Invocation::Full).unwrap();

    assert_eq!(
        ReconciliationLock::take_rerun(&state_dir).unwrap(),
        Some(closed)
    );
    assert_eq!(
        ReconciliationLock::take_rerun(&state_dir).unwrap(),
        Some(Invocation::Full)
    );
    assert!(
        ReconciliationLock::take_rerun(&state_dir)
            .unwrap()
            .is_none()
    );
    std::fs::remove_dir_all(state_dir).unwrap();
}

#[test]
fn full_followed_by_close_keeps_both_requests_in_order() {
    let state_dir = test_state_dir();
    let closed = Invocation::ClosedPane {
        workspace_id: "w1".into(),
        pane_id: "w1:p1".into(),
    };

    ReconciliationLock::request_rerun(&state_dir, &Invocation::Full).unwrap();
    ReconciliationLock::request_rerun(&state_dir, &closed).unwrap();

    assert_eq!(
        ReconciliationLock::take_rerun(&state_dir).unwrap(),
        Some(Invocation::Full)
    );
    assert_eq!(
        ReconciliationLock::take_rerun(&state_dir).unwrap(),
        Some(closed)
    );
    assert!(
        ReconciliationLock::take_rerun(&state_dir)
            .unwrap()
            .is_none()
    );
    std::fs::remove_dir_all(state_dir).unwrap();
}

#[test]
fn close_requests_for_multiple_workspaces_are_preserved() {
    let state_dir = test_state_dir();
    let first = Invocation::ClosedPane {
        workspace_id: "w1".into(),
        pane_id: "w1:p1".into(),
    };
    let second = Invocation::ClosedPane {
        workspace_id: "w2".into(),
        pane_id: "w2:p1".into(),
    };

    ReconciliationLock::request_rerun(&state_dir, &first).unwrap();
    ReconciliationLock::request_rerun(&state_dir, &second).unwrap();

    assert_eq!(
        ReconciliationLock::take_rerun(&state_dir).unwrap(),
        Some(first)
    );
    assert_eq!(
        ReconciliationLock::take_rerun(&state_dir).unwrap(),
        Some(second)
    );
    assert!(
        ReconciliationLock::take_rerun(&state_dir)
            .unwrap()
            .is_none()
    );
    std::fs::remove_dir_all(state_dir).unwrap();
}

#[test]
fn ordinary_deferred_requests_remain_full_reconciliations() {
    let state_dir = test_state_dir();

    ReconciliationLock::request_rerun(&state_dir, &Invocation::Workspace("w2".into())).unwrap();

    assert_eq!(
        ReconciliationLock::take_rerun(&state_dir).unwrap(),
        Some(Invocation::Full)
    );
    std::fs::remove_dir_all(state_dir).unwrap();
}

#[test]
fn requests_publish_complete_records_and_are_consumed_atomically() {
    let state_dir = test_state_dir();
    let requested = Invocation::ClosedPane {
        workspace_id: "w1".into(),
        pane_id: "w1:p1".into(),
    };

    ReconciliationLock::request_rerun(&state_dir, &requested).unwrap();

    let entries = std::fs::read_dir(state_dir.join(RERUN_DIRECTORY))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].extension().and_then(|ext| ext.to_str()),
        Some("json")
    );
    assert_eq!(
        serde_json::from_slice::<Invocation>(&std::fs::read(&entries[0]).unwrap()).unwrap(),
        requested
    );

    assert_eq!(
        ReconciliationLock::take_rerun(&state_dir).unwrap(),
        Some(requested)
    );
    assert!(
        std::fs::read_dir(state_dir.join(RERUN_DIRECTORY))
            .unwrap()
            .next()
            .is_none()
    );
    std::fs::remove_dir_all(state_dir).unwrap();
}

#[test]
fn retry_sleep_does_not_exceed_the_remaining_admission_window() {
    let now = Instant::now();

    assert_eq!(
        retry_delay(now + Duration::from_millis(7), now),
        Some(Duration::from_millis(7))
    );
    assert_eq!(retry_delay(now, now), None);
}

fn test_state_dir() -> std::path::PathBuf {
    let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "herdr-labels-lock-test-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir(&path).unwrap();
    path
}

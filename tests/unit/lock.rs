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
    ReconciliationLock::request_rerun(&state_dir).unwrap();
    drop(first);

    let second = ReconciliationLock::try_acquire(&state_dir)
        .unwrap()
        .expect("contender should acquire after release");
    assert!(ReconciliationLock::take_rerun(&state_dir).unwrap());
    drop(second);
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

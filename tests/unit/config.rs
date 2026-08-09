use super::*;

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn shell_modes_require_only_safe_arguments() {
    assert_eq!(
        invocation_from(
            &strings(&["preexec", "--shell", "zsh", "--program", "nvim"]),
            None,
            Some("w1".into()),
            Some("w1:t1".into()),
            Some("w1:p1".into())
        )
        .unwrap(),
        Invocation::Preexec {
            pane_id: "w1:p1".into(),
            shell: "zsh".into(),
            program: Some("nvim".into())
        }
    );
    assert_eq!(
        invocation_from(
            &strings(&["precmd", "--shell", "bash", "--shell-pid", "42"]),
            None,
            Some("w1".into()),
            Some("w1:t1".into()),
            Some("w1:p1".into())
        )
        .unwrap(),
        Invocation::Precmd {
            pane_id: "w1:p1".into(),
            shell: "bash".into(),
            shell_pid: 42,
        }
    );
    assert!(
        invocation_from(
            &strings(&["preexec", "npm run secret"]),
            None,
            Some("w1".into()),
            Some("w1:t1".into()),
            Some("w1:p1".into())
        )
        .is_err()
    );
}

#[test]
fn toggle_uses_the_current_plugin_context() {
    assert_eq!(
        invocation_from(
            &strings(&["toggle"]),
            None,
            Some("w1".into()),
            Some("w1:t1".into()),
            Some("w1:p1".into())
        )
        .unwrap(),
        Invocation::Toggle {
            workspace_id: Some("w1".into()),
            tab_id: Some("w1:t1".into()),
        }
    );
}

#[test]
fn close_and_focus_events_have_precise_invocations() {
    assert_eq!(
        event_invocation(Some("tab.closed"), Some("w1".into()), Some("w1:t2".into())),
        Invocation::ClosedTab {
            workspace_id: Some("w1".into()),
            tab_id: Some("w1:t2".into())
        }
    );
    assert_eq!(
        event_invocation(
            Some("pane.focused"),
            Some("w1".into()),
            Some("w1:t2".into())
        ),
        Invocation::Tab {
            workspace_id: "w1".into(),
            tab_id: "w1:t2".into()
        }
    );
}

#[test]
fn rename_events_are_distinct_from_focus_refreshes() {
    assert_eq!(
        event_invocation(Some("tab.renamed"), Some("w1".into()), Some("w1:t2".into())),
        Invocation::RenamedTab {
            workspace_id: "w1".into(),
            tab_id: "w1:t2".into()
        }
    );
}

#[test]
fn pane_moves_reconcile_source_and_destination_through_a_full_pass() {
    assert_eq!(
        event_invocation(
            Some("pane.moved"),
            Some("destination".into()),
            Some("destination:t1".into())
        ),
        Invocation::Full
    );
}

#[test]
fn session_paths_are_stable_and_isolated() {
    let first = session_state_dir(Path::new("/tmp/a/herdr.sock")).unwrap();
    let same = session_state_dir(Path::new("/tmp/a/herdr.sock")).unwrap();
    let second = session_state_dir(Path::new("/tmp/b/herdr.sock")).unwrap();
    assert_eq!(first, same);
    assert_ne!(first, second);
}

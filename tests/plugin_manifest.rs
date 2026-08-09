const MANIFEST: &str = include_str!("../herdr-plugin.toml");

#[test]
fn closing_a_pane_triggers_reconciliation() {
    let events = MANIFEST.lines().filter_map(|line| {
        line.trim()
            .strip_prefix("on = \"")
            .and_then(|event| event.strip_suffix('"'))
    });

    assert!(events.into_iter().any(|event| event == "pane.closed"));
}

#[test]
fn renaming_a_tab_triggers_reconciliation() {
    let events = MANIFEST.lines().filter_map(|line| {
        line.trim()
            .strip_prefix("on = \"")
            .and_then(|event| event.strip_suffix('"'))
    });

    assert!(events.into_iter().any(|event| event == "tab.renamed"));
}

#[test]
fn naming_lifecycle_events_are_subscribed() {
    for event in [
        "tab.focused",
        "pane.created",
        "pane.exited",
        "pane.focused",
        "pane.moved",
    ] {
        assert!(MANIFEST.contains(&format!("on = \"{event}\"")));
    }
}

#[test]
fn reset_and_clear_actions_are_declared() {
    assert!(MANIFEST.contains("id = \"reset\""));
    assert!(MANIFEST.contains("id = \"clear\""));
    assert!(MANIFEST.contains("[\"target/release/herdr-labels\", \"reset\"]"));
    assert!(MANIFEST.contains("[\"target/release/herdr-labels\", \"clear\"]"));
}

use super::*;
use crate::herdr::PaneInfo;
use crate::numbering::Tab;

fn tab(id: &str, workspace: &str, label: &str, focused: bool) -> SessionTab {
    SessionTab {
        tab: Tab {
            tab_id: id.into(),
            workspace_id: workspace.into(),
            label: label.into(),
        },
        focused,
        pane_count: 1,
    }
}

fn snapshot(tabs: Vec<SessionTab>) -> SessionSnapshot {
    let panes = tabs
        .iter()
        .map(|tab| PaneInfo {
            pane_id: format!("{}:pane", tab.tab.tab_id),
            tab_id: tab.tab.tab_id.clone(),
            agent: None,
        })
        .collect();
    SessionSnapshot {
        focused_pane_id: None,
        tabs,
        panes,
    }
}

#[test]
fn positions_are_independent_per_workspace_and_continue_after_nine() {
    let mut tabs = (1..=11)
        .map(|position| tab(&format!("w1:t{position}"), "w1", "name", false))
        .collect::<Vec<_>>();
    tabs.push(tab("w2:t1", "w2", "other", false));
    let positions = tab_positions(&snapshot(tabs));
    assert_eq!(positions["w1:t10"], 10);
    assert_eq!(positions["w1:t11"], 11);
    assert_eq!(positions["w2:t1"], 1);
}

#[test]
fn pane_selection_is_conservative_for_background_splits() {
    let mut split = tab("w1:t1", "w1", "1", false);
    split.pane_count = 2;
    let mut session = snapshot(vec![split.clone()]);
    session.panes.push(PaneInfo {
        pane_id: "other".into(),
        tab_id: "w1:t1".into(),
        agent: None,
    });
    assert_eq!(naming_pane(&session, &split), None);

    split.focused = true;
    session.focused_pane_id = Some("other".into());
    assert_eq!(naming_pane(&session, &split), Some("other"));
}

#[test]
fn shell_invocations_follow_the_pane_after_it_moves() {
    let session = snapshot(vec![
        tab("w1:t1", "w1", "one", false),
        tab("w2:t1", "w2", "two", false),
    ]);
    let invocation = Invocation::Preexec {
        pane_id: "w2:t1:pane".into(),
        shell: "zsh".into(),
        program: Some("nvim".into()),
    };
    let targets = scoped_tabs(&session, &invocation);
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].tab.tab_id, "w2:t1");

    let stale = Invocation::Preexec {
        pane_id: "w1:old-pane-id".into(),
        shell: "zsh".into(),
        program: Some("nvim".into()),
    };
    assert!(scoped_tabs(&session, &stale).is_empty());
}

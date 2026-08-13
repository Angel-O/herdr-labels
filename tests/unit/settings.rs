use super::*;

#[test]
fn settings_are_typed_and_reject_unknown_fields() {
    let parsed: Settings = toml::from_str(
        r#"
            auto_name_tabs = false
            number_tabs = true
            max_label_chars = 12
            [process_aliases]
            bv = "beads_viewer"
        "#,
    )
    .unwrap();
    assert!(!parsed.auto_name_tabs);
    assert!(parsed.number_tabs);
    assert_eq!(parsed.max_label_chars, 12);
    assert_eq!(parsed.process_aliases["bv"], "beads_viewer");
    assert!(toml::from_str::<Settings>("unknown = true").is_err());
}

#[test]
fn malformed_settings_files_are_rejected() {
    let path = std::env::temp_dir().join(format!(
        "herdr-labels-config-test-{}.toml",
        std::process::id()
    ));
    std::fs::write(&path, "not = valid = toml").unwrap();
    assert!(load_settings(&path).is_err());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn transient_prompt_programs_are_ignored_by_default() {
    let settings = Settings::default();
    for program in ["git", "direnv", "scutil"] {
        assert!(
            settings
                .ignored_processes
                .iter()
                .any(|ignored| ignored == program),
            "{program} should be ignored"
        );
    }
}

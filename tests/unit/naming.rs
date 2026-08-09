use super::*;

fn policy() -> NamingPolicy {
    NamingPolicy {
        hide_idle_shell: false,
        max_label_chars: 40,
        shells: HashSet::from(["bash".into(), "zsh".into()]),
        ignored_processes: HashSet::from(["direnv".into()]),
        aliases: HashMap::new(),
    }
}

fn process(program: &str) -> ObservedProcess {
    ObservedProcess {
        program: program.into(),
        command_line: None,
    }
}

#[test]
fn absent_or_empty_process_uses_the_supplied_shell() {
    let policy = policy();

    assert_eq!(policy.label("zsh", None), Some("zsh".into()));
    assert_eq!(policy.label("zsh", Some(&process(""))), Some("zsh".into()));
    assert_eq!(policy.label("zsh", Some(&process("-"))), Some("zsh".into()));
}

#[test]
fn idle_names_are_suppressed_when_configured() {
    let mut policy = policy();
    policy.hide_idle_shell = true;

    assert_eq!(policy.label("zsh", None), None);
    assert_eq!(policy.label("zsh", Some(&process("bash"))), None);
    assert_eq!(policy.label("zsh", Some(&process("direnv"))), None);
}

#[test]
fn shell_program_uses_its_own_name() {
    assert_eq!(
        policy().label("zsh", Some(&process("/usr/local/bin/bash"))),
        Some("bash".into())
    );
}

#[test]
fn shell_detection_normalizes_paths_and_login_markers() {
    let policy = policy();
    assert!(policy.is_shell_program("/bin/-zsh"));
    assert!(!policy.is_shell_program("/usr/bin/nvim"));
    assert!(policy.same_program("/usr/bin/nvim", "nvim"));
    assert!(!policy.same_program("nvim", "vim"));
}

#[test]
fn ignored_program_preserves_the_supplied_shell() {
    assert_eq!(
        policy().label("zsh", Some(&process("/opt/bin/direnv"))),
        Some("zsh".into())
    );
}

#[test]
fn aliases_are_exact_and_take_precedence() {
    let mut policy = policy();
    policy.aliases.insert("bash".into(), "terminal".into());
    policy.aliases.insert("git status".into(), "status".into());
    policy
        .aliases
        .insert("git".into(), "version control".into());
    policy.hide_idle_shell = true;

    assert_eq!(
        policy.label("zsh", Some(&process("/bin/bash"))),
        Some("terminal".into())
    );
    assert_eq!(
        policy.label(
            "zsh",
            Some(&ObservedProcess {
                program: "/usr/bin/git".into(),
                command_line: Some("git status".into()),
            })
        ),
        Some("status".into())
    );
    assert_eq!(
        policy.label(
            "zsh",
            Some(&ObservedProcess {
                program: "git".into(),
                command_line: Some("git status --short".into()),
            })
        ),
        Some("version control".into())
    );
}

#[test]
fn normal_program_uses_its_basename() {
    assert_eq!(
        policy().label("zsh", Some(&process("/Applications/Editor/bin/nvim"))),
        Some("nvim".into())
    );
    assert_eq!(
        policy().label("zsh", Some(&process(r"C:\Tools\pwsh.exe"))),
        Some("pwsh.exe".into())
    );
}

#[test]
fn login_shell_marker_is_removed_after_the_path() {
    assert_eq!(
        policy().label("bash", Some(&process("/bin/-zsh"))),
        Some("zsh".into())
    );
    assert_eq!(
        policy().label("bash", Some(&process("--tool"))),
        Some("tool".into())
    );
}

#[test]
fn truncation_counts_unicode_characters_not_bytes() {
    let mut policy = policy();
    policy.max_label_chars = 3;

    assert_eq!(
        policy.label("zsh", Some(&process("cafe\u{301}"))),
        Some("caf".into())
    );
    assert_eq!(
        policy.label("zsh", Some(&process("\u{732b}abc"))),
        Some("\u{732b}ab".into())
    );
}

#[test]
fn controls_are_removed_before_truncation() {
    let mut policy = policy();
    policy.max_label_chars = 4;

    assert_eq!(
        policy.label("zsh", Some(&process("ab\u{001b}[31m\u{007f}\u{0085}cd"))),
        Some("ab[3".into())
    );
}

#[test]
fn controls_are_removed_from_shell_and_alias_labels() {
    let mut policy = policy();
    policy
        .aliases
        .insert("nvim".into(), "ed\u{001b}itor".into());

    assert_eq!(policy.label("z\u{0000}sh", None), Some("zsh".into()));
    assert_eq!(
        policy.label("zsh", Some(&process("nvim"))),
        Some("editor".into())
    );
}

#[test]
fn zero_character_limit_produces_an_empty_label() {
    let mut policy = policy();
    policy.max_label_chars = 0;

    assert_eq!(
        policy.label("zsh", Some(&process("cargo"))),
        Some(String::new())
    );
}

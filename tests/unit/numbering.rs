use super::*;

#[test]
fn replaces_a_leading_numeric_prefix() {
    assert_eq!(numbered_label(2, "[3] setup"), "[2] setup");
    assert_eq!(numbered_label(2, "[003]\tsetup"), "[2] setup");
    assert_eq!(numbered_label(2, "[3]setup"), "[2] setup");
    assert_eq!(numbered_label(2, "[9] [3] setup"), "[2] setup");
}

#[test]
fn preserves_non_numeric_bracketed_text() {
    assert_eq!(numbered_label(1, "[dev] setup"), "[1] [dev] setup");
    assert_eq!(numbered_label(1, "[] setup"), "[1] [] setup");
    assert_eq!(numbered_label(1, "setup"), "[1] setup");
}

#[test]
fn recognizes_generated_placeholders() {
    assert!(is_placeholder(""));
    assert!(is_placeholder("42"));
    assert!(!is_placeholder("42 tests"));
}

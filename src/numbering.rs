//! Pure policy for deriving numbered tab labels.

/// Tab state needed to calculate its desired label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tab {
    /// Stable Herdr identifier for the tab.
    pub(crate) tab_id: String,
    /// Stable Herdr identifier for the containing workspace.
    pub(crate) workspace_id: String,
    /// Current user-visible tab label.
    pub(crate) label: String,
}

pub(crate) fn numbered_label(position: usize, label: &str) -> String {
    let base = strip_numeric_prefix(label);
    if base.is_empty() {
        format!("[{position}]")
    } else {
        format!("[{position}] {base}")
    }
}

pub(crate) fn strip_numeric_prefix(mut label: &str) -> &str {
    loop {
        let Some(rest) = label.strip_prefix('[') else {
            return label;
        };
        let Some(closing) = rest.find(']') else {
            return label;
        };
        let digits = &rest[..closing];
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return label;
        }

        label = rest[closing + 1..].trim_start_matches(char::is_whitespace);
    }
}

/// Returns whether Herdr likely generated this empty or all-numeric base label.
pub(crate) fn is_placeholder(label: &str) -> bool {
    label.is_empty() || label.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
#[path = "../tests/unit/numbering.rs"]
mod tests;

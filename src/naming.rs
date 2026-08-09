//! Pure policy for deriving semantic tab names from observed processes.

use std::collections::{HashMap, HashSet};

/// Configuration used to turn an observed foreground process into a label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NamingPolicy {
    /// Suppress a semantic label while a shell or ignored program is active.
    pub(crate) hide_idle_shell: bool,
    /// Maximum number of Unicode scalar values retained in a generated label.
    pub(crate) max_label_chars: usize,
    /// Process basenames considered interactive shells.
    pub(crate) shells: HashSet<String>,
    /// Process basenames that do not replace the supplied shell name.
    pub(crate) ignored_processes: HashSet<String>,
    /// Exact command-line or process-basename replacements.
    pub(crate) aliases: HashMap<String, String>,
}

/// Foreground process data available to the naming policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObservedProcess {
    /// Executable name, which may be a path or login-shell name.
    pub(crate) program: String,
    /// Command line when supplied by the process observer; arguments are opaque.
    pub(crate) command_line: Option<String>,
}

impl NamingPolicy {
    /// Returns whether a process name resolves to a configured interactive shell.
    pub(crate) fn is_shell_program(&self, program: &str) -> bool {
        self.shells.contains(&normalized_name(program))
    }

    /// Compares program names after removing paths and login-shell markers.
    pub(crate) fn same_program(&self, first: &str, second: &str) -> bool {
        normalized_name(first) == normalized_name(second)
    }

    /// Returns whether a process should leave the current ambient label unchanged.
    pub(crate) fn is_ignored_program(&self, program: &str) -> bool {
        self.ignored_processes.contains(&normalized_name(program))
    }

    /// Derives a safe semantic label for reconciliation.
    ///
    /// `None` means an idle shell-like observation should not contribute a
    /// semantic label. Aliases match an entire command line first, then the
    /// normalized process basename. No command-line parsing is performed.
    pub(crate) fn label(
        &self,
        shell_name: &str,
        process: Option<&ObservedProcess>,
    ) -> Option<String> {
        let shell_name = normalized_name(shell_name);
        let Some(process) = process else {
            return self.idle_label(&shell_name);
        };
        let program = normalized_name(&process.program);
        if program.is_empty() {
            return self.idle_label(&shell_name);
        }

        let alias = process
            .command_line
            .as_deref()
            .filter(|command_line| !command_line.is_empty())
            .and_then(|command_line| self.aliases.get(command_line))
            .or_else(|| self.aliases.get(&program));
        if let Some(alias) = alias {
            return Some(self.finish(alias));
        }

        if self.shells.contains(&program) {
            return (!self.hide_idle_shell).then(|| self.finish(&program));
        }
        if self.ignored_processes.contains(&program) {
            return self.idle_label(&shell_name);
        }

        Some(self.finish(&program))
    }

    fn idle_label(&self, shell_name: &str) -> Option<String> {
        (!self.hide_idle_shell).then(|| self.finish(shell_name))
    }

    fn finish(&self, name: &str) -> String {
        name.chars()
            .filter(|character| !is_control(*character))
            .take(self.max_label_chars)
            .collect()
    }
}

fn normalized_name(name: &str) -> String {
    name.rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .trim_start_matches('-')
        .chars()
        .filter(|character| !is_control(*character))
        .collect()
}

fn is_control(character: char) -> bool {
    matches!(character, '\u{0000}'..='\u{001f}' | '\u{007f}'..='\u{009f}')
}

#[cfg(test)]
#[path = "../tests/unit/naming.rs"]
mod tests;

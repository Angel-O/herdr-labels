//! Process-group inspection and representative-process selection.

use crate::herdr::{PaneProcessInfo, ProcessInfo};
use crate::naming::NamingPolicy;

pub(super) fn process_group_matches_program(
    process_info: &PaneProcessInfo,
    program: &str,
    policy: &NamingPolicy,
) -> bool {
    let Some(leader) = process_info.leader() else {
        return false;
    };
    process_info
        .foreground_processes
        .iter()
        .any(|process| policy.same_program(program, process.program()))
        || leader.argv.as_deref().is_some_and(|arguments| {
            arguments
                .iter()
                .skip(1)
                .any(|argument| policy.same_program(program, argument))
        })
}

pub(super) struct RepresentativeSelection<'a> {
    pub(super) process: &'a ProcessInfo,
    pub(super) reason: &'static str,
}

pub(super) fn select_representative<'a>(
    process_info: &'a PaneProcessInfo,
    policy: &NamingPolicy,
    preferred_program: Option<&str>,
) -> Option<RepresentativeSelection<'a>> {
    let leader = process_info.leader()?;
    if let Some(process) = preferred_program.and_then(|preferred| {
        process_info.foreground_processes.iter().find(|process| {
            policy.same_program(preferred, process.program())
                && !policy.is_ignored_program(process.program())
        })
    }) {
        return Some(RepresentativeSelection {
            process,
            reason: "preferred",
        });
    }
    let launched_process = leader.argv.as_deref().and_then(|arguments| {
        process_info
            .foreground_processes
            .iter()
            .filter(|process| process.pid != leader.pid)
            .find(|process| {
                arguments
                    .iter()
                    .skip(1)
                    .any(|argument| policy.same_program(argument, process.program()))
                    && !policy.is_ignored_program(process.program())
            })
    });
    if let Some(process) = launched_process {
        return Some(RepresentativeSelection {
            process,
            reason: "launched",
        });
    }
    if !policy.is_shell_program(leader.program()) && !policy.is_ignored_program(leader.program()) {
        return Some(RepresentativeSelection {
            process: leader,
            reason: "leader",
        });
    }
    if let Some(process) = process_info.foreground_processes.iter().find(|process| {
        process.pid != leader.pid
            && !policy.is_shell_program(process.program())
            && !policy.is_ignored_program(process.program())
    }) {
        return Some(RepresentativeSelection {
            process,
            reason: "foreground",
        });
    }
    if policy.is_shell_program(leader.program()) {
        return Some(RepresentativeSelection {
            process: leader,
            reason: "shell_leader_fallback",
        });
    }
    None
}

#[cfg(test)]
#[path = "../tests/unit/process_selection.rs"]
mod tests;

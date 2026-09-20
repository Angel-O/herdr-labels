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
    pub(super) ignored_processes: Vec<&'a ProcessInfo>,
}

#[allow(dead_code)]
pub(super) fn representative_process<'a>(
    process_info: &'a PaneProcessInfo,
    policy: &NamingPolicy,
    preferred_program: Option<&str>,
) -> Option<&'a ProcessInfo> {
    representative_process_with_trace(process_info, policy, preferred_program)
        .map(|selection| selection.process)
}

pub(super) fn representative_process_with_trace<'a>(
    process_info: &'a PaneProcessInfo,
    policy: &NamingPolicy,
    preferred_program: Option<&str>,
) -> Option<RepresentativeSelection<'a>> {
    representative_process_with_trace_mode(process_info, policy, preferred_program, true)
}

pub(super) fn representative_process_with_trace_mode<'a>(
    process_info: &'a PaneProcessInfo,
    policy: &NamingPolicy,
    preferred_program: Option<&str>,
    collect_ignored_processes: bool,
) -> Option<RepresentativeSelection<'a>> {
    let leader = process_info.leader()?;
    let ignored_processes = if collect_ignored_processes {
        process_info
            .foreground_processes
            .iter()
            .filter(|process| policy.is_ignored_program(process.program()))
            .collect()
    } else {
        Vec::new()
    };
    if let Some(process) = preferred_program.and_then(|preferred| {
        process_info.foreground_processes.iter().find(|process| {
            policy.same_program(preferred, process.program())
                && !policy.is_ignored_program(process.program())
        })
    }) {
        return Some(RepresentativeSelection {
            process,
            reason: "preferred",
            ignored_processes,
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
            ignored_processes,
        });
    }
    if !policy.is_shell_program(leader.program()) && !policy.is_ignored_program(leader.program()) {
        return Some(RepresentativeSelection {
            process: leader,
            reason: "leader",
            ignored_processes,
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
            ignored_processes,
        });
    }
    if policy.is_shell_program(leader.program()) {
        return Some(RepresentativeSelection {
            process: leader,
            reason: "shell_leader_fallback",
            ignored_processes,
        });
    }
    None
}

#[cfg(test)]
#[path = "../tests/unit/process_selection.rs"]
mod tests;

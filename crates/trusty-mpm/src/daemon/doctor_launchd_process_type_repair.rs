//! `tm doctor --fix` repair for the `launchd_process_type` row (#8562).
//!
//! Why: #8415 made the supervisor template declare `ProcessType=Interactive`,
//! but the `com.trusty.mpm` daemon plist has no generator in this workspace,
//! and a supervisor plist already on disk keeps its old value. The row's only
//! remedy was a hand-run `plutil`. This is the supported write path.
//!
//! What: for the daemon and supervisor plists, sets `ProcessType` to
//! `Interactive` — replacing the value of the key launchd loads, or adding the
//! key to the top-level dict — with an atomic write. It ONLY rewrites the file:
//! it never runs `launchctl`, never unloads, reloads or signals the running
//! job. Every step says the change takes effect at the next daemon restart.
//! No backup is taken: a plist may carry `EnvironmentVariables` credentials
//! (#8236), and a backup would be a second readable copy. The atomic write
//! leaves the original byte-identical until the rename.
//!
//! Test: `doctor_launchd_process_type_repair_tests.rs`.

use std::path::Path;

use trusty_common::atomic_file::write_atomic;
use trusty_common::launchd_labels::{MPM, MPM_SUPERVISOR};

use super::{
    CHECK_NAME, EXPECTED_PROCESS_TYPE, ProcessTypeReading, launch_agents_dir, process_type_of,
    read_plist,
};
use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};

/// Set `ProcessType=Interactive` in the tm daemon and supervisor plists.
///
/// Why/What: see the module docs. Reads the same LaunchAgents directory the
/// `launchd_process_type` row reads, so `--fix` acts on what the operator saw.
/// Test: as [`repair_process_type_in`].
pub fn repair_launchd_process_type(home: &Path, mode: RepairMode) -> Vec<RepairStep> {
    repair_process_type_in(&launch_agents_dir(home).path, mode)
}

/// [`repair_launchd_process_type`] against an explicit LaunchAgents directory.
///
/// What: one step per plist that is installed and not already `Interactive`;
/// an absent or already-correct plist produces no step, so a second run is
/// silent. [`StepStatus::Planned`] under [`RepairMode::DryRun`];
/// [`StepStatus::Applied`] with no backup once written; [`StepStatus::Refused`]
/// for a plist this repair cannot judge or edit safely (binary, symlinked, an
/// unexpected layout); [`StepStatus::Failed`] when the write fails.
/// Test: `dry_run_plans_without_writing`,
/// `apply_adds_the_key_and_the_row_passes`,
/// `apply_replaces_a_background_value_and_keeps_the_rest`,
/// `repair_is_silent_when_already_interactive_or_absent`,
/// `repair_refuses_a_binary_plist`, `repair_refuses_a_symlinked_plist`,
/// `every_step_says_it_takes_effect_at_the_next_restart`.
pub(crate) fn repair_process_type_in(agents: &Path, mode: RepairMode) -> Vec<RepairStep> {
    [MPM, MPM_SUPERVISOR]
        .into_iter()
        .filter_map(|label| repair_one(&agents.join(format!("{label}.plist")), label, mode))
        .collect()
}

/// One plist's repair, or `None` when there is nothing to do.
fn repair_one(path: &Path, label: &str, mode: RepairMode) -> Option<RepairStep> {
    let step = |status| RepairStep {
        check: CHECK_NAME,
        path: path.to_path_buf(),
        what: describe(label),
        status,
    };
    let refused = |why: String| Some(step(StepStatus::Refused(format!("{why}; set it by hand"))));
    match read_plist(path) {
        ProcessTypeReading::NotInstalled => return None,
        ProcessTypeReading::Declared(Some(v)) if v == EXPECTED_PROCESS_TYPE => return None,
        ProcessTypeReading::Unjudged(why) => return refused(why),
        ProcessTypeReading::Declared(_) => {}
    }
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return refused("the plist is a symlink, and rewriting it would sever the link".into());
    }
    let xml = match std::fs::read_to_string(path) {
        Ok(xml) => xml,
        Err(e) => return refused(format!("could not read it: {}", e.kind())),
    };
    let edited = match set_process_type_interactive(&xml) {
        Ok(edited) => edited,
        Err(why) => return refused(why),
    };
    if mode == RepairMode::DryRun {
        return Some(step(StepStatus::Planned));
    }
    // #8562: the file only — launchd is never told; see `describe`.
    Some(step(match write_atomic(path, edited.as_bytes()) {
        Ok(()) => StepStatus::Applied { backup: None },
        Err(e) => StepStatus::Failed(format!("could not write it: {e}")),
    }))
}

/// The step text, the same in both modes.
///
/// Why (#8562 owner ruling 2026-09-28): the repair must not restart the daemon,
/// so the operator has to be told when the new class applies.
fn describe(label: &str) -> String {
    format!(
        "set ProcessType={EXPECTED_PROCESS_TYPE} in `{label}`; the plist file only — launchd is \
         not reloaded, so it takes effect at the next daemon restart, and a tmux server already \
         running keeps its class until it exits"
    )
}

/// Byte offsets of `needle` in `xml` that lie outside `<!-- … -->` comments.
fn outside_comments(xml: &str, needle: &str) -> Vec<usize> {
    let mut comments = Vec::new();
    let mut from = 0;
    while let Some(open) = xml[from..].find("<!--").map(|i| from + i) {
        let close = xml[open..]
            .find("-->")
            .map_or(xml.len(), |i| open + i + "-->".len());
        comments.push(open..close);
        from = close;
    }
    xml.match_indices(needle)
        .map(|(at, _)| at)
        .filter(|at| !comments.iter().any(|c| c.contains(at)))
        .collect()
}

/// Return `xml` with `ProcessType` set to `Interactive`.
///
/// What: when a `ProcessType` key exists, replaces the `<string>` value of the
/// LAST one outside a comment — the one [`process_type_of`] reads and launchd
/// loads. Otherwise inserts the key and value before the top-level dict's
/// closing `</dict>`, which must be followed only by whitespace and
/// `</plist>`. `Err` for any other layout.
///
/// # Code Contract
/// Postcondition: on `Ok(out)`, `process_type_of(out)` is
/// `Ok(Some("Interactive"))`; the function checks this before returning.
///
/// Test: `apply_adds_the_key_and_the_row_passes`,
/// `apply_replaces_a_background_value_and_keeps_the_rest`,
/// `an_unexpected_layout_is_refused`.
pub(crate) fn set_process_type_interactive(xml: &str) -> Result<String, String> {
    const KEY: &str = "<key>ProcessType</key>";
    let out = match outside_comments(xml, KEY).last() {
        Some(&at) => {
            let after = at + KEY.len();
            let rest = &xml[after..];
            let open = after + (rest.len() - rest.trim_start().len());
            let body = xml[open..]
                .strip_prefix("<string>")
                .ok_or("ProcessType is not followed by a <string>")?;
            let len = body
                .find("</string>")
                .ok_or("ProcessType <string> is not closed")?;
            let start = open + "<string>".len();
            format!(
                "{}{EXPECTED_PROCESS_TYPE}{}",
                &xml[..start],
                &xml[start + len..]
            )
        }
        None => {
            let close = *outside_comments(xml, "</dict>")
                .last()
                .ok_or("no top-level </dict>")?;
            let tail = xml[close + "</dict>".len()..].trim();
            if tail != "</plist>" {
                return Err("the top-level </dict> is not followed by </plist>".into());
            }
            let line_start = xml[..close].rfind('\n').map_or(0, |i| i + 1);
            let at = if xml[line_start..close].trim().is_empty() {
                line_start
            } else {
                close
            };
            let entry = format!("\t{KEY}\n\t<string>{EXPECTED_PROCESS_TYPE}</string>\n");
            format!("{}{entry}{}", &xml[..at], &xml[at..])
        }
    };
    match process_type_of(&out) {
        Ok(Some(v)) if v == EXPECTED_PROCESS_TYPE => Ok(out),
        other => Err(format!(
            "the edit did not produce ProcessType={EXPECTED_PROCESS_TYPE}: {other:?}"
        )),
    }
}

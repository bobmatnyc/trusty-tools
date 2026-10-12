//! The channel taint directory as a trust anchor (#8433 P0, plan §2 item 2).
//!
//! Why: absence of a taint file is the one allow the egress guard grants, so a
//! session able to delete, move, truncate or overwrite its own taint file
//! could untaint itself. The directory is therefore sealed like the Architect
//! launch directory: itself, every directory above it, and everything under
//! it. Unlike the #8878 anchors it binds the Architect's main thread too —
//! the Architect is the session inbound answers are delivered to.
//! What: [`TaintAnchor::locate`] resolves the directory and whether a taint
//! is live (any file in it, or it cannot be read). The parent adds it to every
//! session's anchor set ([`TaintAnchor::extend`]). [`taint_verdict`] runs
//! before the Architect exemption: while a taint is live, it judges the call's
//! writes against the taint directory alone, under the parent's full rules
//! (an unplaceable write, an unresolvable target, a `*.json` in an unknown
//! directory all deny). With no live taint it defers to the parent, so the
//! Architect's other writes are unchanged.
//! Residual: the parent's — `xargs rm`, `find -delete`, a glob after a `cd`
//! and writers outside its verb list (`dd`, interpreters). A
//! truncated or emptied taint file still reads tainted (malformed).
//! Test: `pm_guard_trust_anchor_taint_tests.rs`.

use std::path::{Path, PathBuf};

use trusty_mpm::core::channels_taint::{self, Presence};

use super::{AnchorWrite, Anchors, Resolved, UnplaceableWrite, component_names, resolve};

/// The taint directory under one home.
pub(super) struct TaintAnchor {
    /// `~/.trusty-tools/trusty-mpm/channels/taint`, as spelled.
    lexical: PathBuf,
    /// The directory resolved, or `None` when it does not resolve.
    resolved: Option<PathBuf>,
    /// Whether a taint file or marker exists, or presence cannot be read.
    pub(super) live: bool,
}

impl TaintAnchor {
    /// Locate the taint directory under `home`.
    pub(super) fn locate(home: &Path) -> Self {
        let lexical = channels_taint::taint_dir(home);
        let resolved = match resolve(&lexical) {
            Resolved::Path(dir) => Some(dir),
            Resolved::Unresolvable => None,
        };
        let live = channels_taint::presence(&lexical) != Presence::None;
        Self {
            lexical,
            resolved,
            live,
        }
    }

    /// Seal the directory: guarded with every directory above it, and fenced.
    ///
    /// What: a directory that does not resolve is guarded and fenced as
    /// spelled, so every write under it and every move or delete above it
    /// denies — fail closed.
    /// Test: `an_unresolvable_taint_dir_still_guards_the_dirs_above_it`.
    pub(super) fn extend(
        &self,
        guarded: &mut Vec<PathBuf>,
        fence: &mut Vec<PathBuf>,
        paths: &mut Vec<PathBuf>,
    ) {
        paths.push(self.lexical.clone());
        match &self.resolved {
            Some(dir) => {
                guarded.push(dir.clone());
                fence.push(dir.clone());
                paths.push(dir.clone());
            }
            // #8433 fix round: guard the directories above it too, so `rm -rf`
            // of its parent cannot remove a taint the guard cannot see.
            None => {
                guarded.push(self.lexical.clone());
                fence.push(self.lexical.clone());
            }
        }
    }
}

/// The deny for a call that writes into a live taint directory, for every
/// session; `None` when no taint is live or the call misses it.
///
/// Test: `anchor_denies_rm_of_taint_file`, `anchor_denies_write_and_mv_of_taint_dir`,
/// `anchor_denies_truncate_of_taint_file`, `the_architect_cannot_write_a_live_taint`.
pub(super) fn taint_verdict(
    writes: &Result<Vec<AnchorWrite>, UnplaceableWrite>,
    cwd: Option<&Path>,
    home: Option<&Path>,
) -> Option<String> {
    let home = home?;
    let anchor = TaintAnchor::locate(home);
    if !anchor.live {
        return None;
    }
    let writes = match writes {
        Ok(writes) => writes,
        Err(what) => return Some(taint_reason(what)),
    };
    let mut anchors = Anchors {
        guarded: Vec::new(),
        ids: Vec::new(),
        armed: None,
        fence: Vec::new(),
        records_live: true,
        names: Vec::new(),
    };
    let mut paths = Vec::new();
    anchor.extend(&mut anchors.guarded, &mut anchors.fence, &mut paths);
    anchors.names = component_names(&paths);
    super::judge_writes(&anchors, writes, cwd, home).map(|_| taint_reason("this write"))
}

/// The deny text for a write that could reach the taint directory.
fn taint_reason(what: &str) -> String {
    format!(
        "Trust-anchor write denied (#8433): {what} could change \
         `~/.trusty-tools/trusty-mpm/channels/taint/`, which records that a session received a \
         channel answer. No session may write, move or delete it — the Architect included, and \
         `TRUSTY_MPM_PM_UNRESTRICTED` / `TRUSTY_MPM_DISABLE_HOOKS` do not lift this rule. A taint \
         ends only when its managed session is stopped or decommissioned."
    )
}

#[cfg(test)]
#[path = "pm_guard_trust_anchor_taint_tests.rs"]
mod tests;

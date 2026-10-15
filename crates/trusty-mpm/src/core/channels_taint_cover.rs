//! Which live taint mark covers a directory, for the two readers that settle
//! a session's taint outside the daemon (#8433 D09, #9923).
//!
//! Why: the SessionStart hook ([`covering_mark`]) and the egress guard
//! ([`taint_covered`]) both scope a mark by its workspace; split from
//! `channels_taint.rs` to keep that file under the SLOC cap (#9923).
//! What: one `managed/` listing per call, marks read in id order.
//! Test: `covering_mark_follows_the_workspace_and_fails_closed`,
//! `a_hook_less_id_is_tainted_for_the_mark_covering_its_launch_dir`.

use std::path::Path;

use super::{
    Cover, MANAGED_DIR, TaintError, canon, entry_names, json_stem, managed_mark, sanitize_id,
    write_taint,
};

/// The managed session whose mark covers a `SessionStart` at `cwd` (D09).
///
/// Why (critic HIGH 2): the hook settles an unconfirmed `SessionStart` from
/// the user tier, so a taint must reach only the sessions of its own
/// workspace, and the record it writes must name a managed session whose
/// retire removes it.
/// What: reads each `managed/<id>.json` in id order; the first mark that
/// [`super::TaintMark::covers`] the canonical `cwd` names its id. A mark that
/// cannot be read covers every cwd. A listing error is [`Cover::Unknown`].
/// Test: `covering_mark_follows_the_workspace_and_fails_closed`.
pub fn covering_mark(dir: &Path, cwd: Option<&Path>) -> Cover {
    let Ok(names) = entry_names(&dir.join(MANAGED_DIR)) else {
        return Cover::Unknown;
    };
    let cwd = cwd.map(|cwd| std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf()));
    for managed in names.iter().filter_map(|name| json_stem(name)) {
        let covers = match managed_mark(dir, managed) {
            Ok(Some(mark)) => mark.covers(cwd.as_deref()),
            // Retired between the listing and the read.
            Ok(None) => false,
            Err(_) => true,
        };
        if covers {
            return Cover::Managed(managed.to_owned());
        }
    }
    Cover::None
}

/// Taint an untainted Claude id that a live mark covers; the covering managed
/// id, or `None` when no mark covers it (#9923).
///
/// Why (#9923, owner ruling Def, option (c)): an id whose SessionStart never
/// reached the daemon has no seen line and no taint file, so
/// [`super::taint_managed_session`] cannot reach it. The egress guard calls
/// this on the id's first gated call, at the point of use.
/// What: one listing of `managed/`; the first mark, in id order, that covers
/// `launch` (the session's launch directory, primary per Architect Q10) or
/// the live `cwd` gets a taint file for `session_id`, attributed to that mark
/// so its [`super::retire`] removes the file. An unknown `launch` is covered
/// by every mark; an unknown `cwd` adds nothing. A bad id, a listing error, a
/// mark that cannot be read and a failed write are each an `Err` the caller
/// denies on.
/// Test: `a_hook_less_id_is_tainted_for_the_mark_covering_its_launch_dir`,
/// `lazy_taint_fails_closed_on_every_error`; end to end in
/// `a_hook_less_id_launched_under_a_live_mark_is_denied_and_tainted_for_the_mark`.
pub fn taint_covered(
    dir: &Path,
    session_id: &str,
    launch: Option<&Path>,
    cwd: Option<&Path>,
) -> Result<Option<String>, TaintError> {
    sanitize_id(session_id)?;
    let names = entry_names(&dir.join(MANAGED_DIR))?;
    let (launch, cwd) = (launch.map(canon), cwd.map(canon));
    for managed in names.iter().filter_map(|name| json_stem(name)) {
        // Retired between the listing and the read: it covers nothing.
        let Some(mark) = managed_mark(dir, managed)? else {
            continue;
        };
        if mark.covers(launch.as_deref()) || cwd.as_deref().is_some_and(|c| mark.covers(Some(c))) {
            write_taint(dir, &mark.record_for(session_id, managed))?;
            return Ok(Some(managed.to_owned()));
        }
    }
    Ok(None)
}

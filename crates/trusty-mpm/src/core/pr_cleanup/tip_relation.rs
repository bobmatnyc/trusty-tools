//! How a kept worktree's tip relates to the merged head (#8603).
//!
//! Why: `tm pr cleanup` kept a tree and printed its tip beside the merged head
//! without saying how they relate. A downstream agent read an ancestor tip as
//! an unpushed post-merge commit. Every kept tree now states the relation.
//! What: [`relation`] asks git and returns one of four answers; [`note`]
//! renders the three quiet ones as a suffix for the kept line, and
//! [`unpushed_warning`] renders the loud one as its own line.
//! Test: `cleanup_8603_a_kept_tree_on_an_ancestor_says_every_commit_is_in_the_merge`,
//! `cleanup_8603_a_kept_tree_ahead_of_the_head_warns_separately`,
//! `cleanup_8603_an_unreadable_relation_is_stated_as_unknown`.

use super::driver::Git;
use super::{CleanupRequest, owned, short};

/// A kept tree's tip, against the merged head commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TipRelation {
    /// The tip IS the merged head.
    IsHead,
    /// The tip is an ancestor of the merged head: every commit on it is in the merge.
    Ancestor,
    /// The tip carries `n` commits the merged head does not contain.
    Ahead(u64),
    /// git could not answer, carrying why.
    Unknown(String),
}

/// Ask git how `tip` relates to `head`.
///
/// What: equal OIDs are [`TipRelation::IsHead`] with no git call. Otherwise
/// `git merge-base --is-ancestor`: exit 0 is [`TipRelation::Ancestor`]; a
/// failure with stderr is an error (git exits 128 for an unknown object), and
/// a silent failure is "not an ancestor", counted with
/// `git rev-list --count <head>..<tip>`. Any error is
/// [`TipRelation::Unknown`], never a guessed relation.
pub(super) fn relation<T: Git>(
    git: &T,
    req: &CleanupRequest,
    tip: &str,
    head: &str,
) -> TipRelation {
    let (tip, head) = (tip.trim(), head.trim());
    if tip.is_empty() || head.is_empty() {
        return TipRelation::Unknown("the tip or the merged head is not known".into());
    }
    if tip.eq_ignore_ascii_case(head) {
        return TipRelation::IsHead;
    }
    match git.run(
        &req.repo_root,
        &owned(&["merge-base", "--is-ancestor", tip, head]),
    ) {
        Ok(out) if out.success => return TipRelation::Ancestor,
        Ok(out) if !out.stderr.trim().is_empty() => {
            return TipRelation::Unknown(out.stderr.trim().to_string());
        }
        Ok(_) => {}
        Err(e) => return TipRelation::Unknown(format!("{e:#}")),
    }
    let range = format!("{head}..{tip}");
    match git.run(&req.repo_root, &owned(&["rev-list", "--count", &range])) {
        Ok(out) if out.success => match out.stdout.trim().parse::<u64>() {
            Ok(n) => TipRelation::Ahead(n),
            Err(e) => TipRelation::Unknown(format!("`git rev-list --count` answered {e}")),
        },
        Ok(out) => TipRelation::Unknown(out.stderr.trim().to_string()),
        Err(e) => TipRelation::Unknown(format!("{e:#}")),
    }
}

/// The suffix a kept line carries, e.g. `; its tip abc12345 is an ancestor…`.
pub(super) fn note(rel: &TipRelation, tip: &str, head: &str) -> String {
    let (tip, head) = (short(tip), short(head));
    match rel {
        TipRelation::IsHead => format!("; its tip {tip} is the merged head"),
        TipRelation::Ancestor => format!(
            "; its tip {tip} is an ancestor of the merged head {head} — every commit on it is in the merge"
        ),
        TipRelation::Ahead(n) => {
            format!("; its tip {tip} has {n} commit(s) the merged head {head} does not contain")
        }
        TipRelation::Unknown(why) => format!(
            "; how its tip {tip} relates to the merged head {head} could not be read: {why}"
        ),
    }
}

/// The separate, louder line for a tip carrying commits the merge did not.
pub(super) fn unpushed_warning(
    path: &str,
    rel: &TipRelation,
    tip: &str,
    head: &str,
) -> Option<String> {
    match rel {
        TipRelation::Ahead(n) if *n > 0 => Some(format!(
            "WARNING: {path} holds {n} commit(s) beyond the merged head {} (tip {}) that the \
             merge did not carry — unpushed or unmerged work; inspect it before removing the tree",
            short(head),
            short(tip)
        )),
        _ => None,
    }
}

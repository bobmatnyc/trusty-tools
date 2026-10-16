//! The channel question table as a trust anchor (#9815, P2a of #8433, G7).
//!
//! Why: `questions.jsonl` decides which inbound answer resolves an owner
//! question, and its `.lock` keeps a second daemon off it. A session able to
//! append to, replace or delete either could forge or replay a resolution.
//! The taint anchor seals only `channels/taint/`, and only while a taint is
//! live; the Architect did not accept that gap (22:51Z 2026-10-12).
//! What: [`QuestionsAnchor::locate`] resolves the log, the lock and the
//! torn-line file the daemon appends to, and (#9923 Q3) the inbound event
//! claim log with its lock, torn and prune files. The parent adds them to every
//! session's anchor set, so for a PM or agent they are anchors under the
//! #8878 rules: each file, every directory above it, and a hard link to an
//! existing one are refused, and a location that does not resolve refuses
//! every write. The Architect's main thread, which the parent exempts, is
//! judged by [`questions_verdict`] against these files alone, under the same
//! rules: a write the guard cannot place and an unknown home deny too. A
//! target in a directory the guard cannot place is judged by its name against
//! the table's file names and its last three directories only (Architect
//! ruling (b), `pm_guard_trust_anchor_names.rs`).
//! Residual: the parent's — `xargs rm`, `find -delete`, a glob after a `cd`
//! and writers outside its verb list (`dd`, interpreters).
//! Test: `pm_guard_trust_anchor_questions_tests.rs`.

use std::path::{Path, PathBuf};

use trusty_mpm::channels::question_table::claims::{claims_path, prune_path};
use trusty_mpm::channels::question_table::{lock_path, questions_path, torn_path};

use super::{
    AnchorWrite, Anchors, FileId, Resolved, UnplaceableWrite, file_id, judge_writes, names, resolve,
};

/// The question table's files under one home, resolved.
pub(super) struct QuestionsAnchor {
    /// The resolved files, protected with every directory above them.
    pub(super) guarded: Vec<PathBuf>,
    /// Lexical and resolved spellings, for the parent's name list.
    pub(super) paths: Vec<PathBuf>,
    /// The identities of the files that exist, for hard links.
    pub(super) ids: Vec<FileId>,
}

impl QuestionsAnchor {
    /// Resolve the log, lock and torn file under `home`; `Err(path)` when
    /// one's location does not resolve, which denies every write.
    ///
    /// Test: `an_unresolvable_questions_log_denies_every_write`,
    /// `a_hard_link_to_the_questions_log_is_an_anchor`.
    pub(super) fn locate(home: &Path) -> Result<Self, PathBuf> {
        let log = questions_path(home);
        let mut out = Self {
            guarded: Vec::new(),
            paths: Vec::new(),
            ids: Vec::new(),
        };
        // #9923 Q3: the inbound event claim log is sealed beside the table.
        let claims = claims_path(home);
        let files = [lock_path(&log), torn_path(&log), log];
        let claim_files = [
            lock_path(&claims),
            torn_path(&claims),
            prune_path(&claims),
            claims,
        ];
        for lexical in files.into_iter().chain(claim_files) {
            // #9815: a location that does not resolve refuses every write.
            let Resolved::Path(resolved) = resolve(&lexical) else {
                return Err(lexical);
            };
            // #9815: a hard link made before the rule is the file itself.
            out.ids.extend(file_id(&resolved));
            out.guarded.push(resolved.clone());
            out.paths.extend([lexical, resolved]);
        }
        Ok(out)
    }
}

/// The deny for an Architect main-thread call that could write the question
/// table, taint or none; `None` when the call misses it.
///
/// Test: `anchor_denies_append_rm_mv_and_truncate_of_the_questions_log`,
/// `anchor_denies_rm_and_replace_of_the_questions_lock`,
/// `anchor_denies_a_link_planted_at_the_torn_file`,
/// `the_architect_cannot_reach_the_question_table_through_an_unplaceable_write`,
/// `questions_anchor_unknown_home_denies_every_write`,
/// `the_architect_is_denied_targets_that_could_name_the_table`,
/// `the_architect_may_make_routine_writes_that_cannot_name_the_table`.
pub(super) fn questions_verdict(
    writes: &Result<Vec<AnchorWrite>, UnplaceableWrite>,
    cwd: Option<&Path>,
    home: Option<&Path>,
) -> Option<String> {
    // #9815: no home, no table location — refuse every write.
    let Some(home) = home else {
        return Some(questions_reason(
            "this write (the home directory is unknown)",
        ));
    };
    let writes = match writes {
        Ok(writes) => writes,
        // #9815: an unplaceable write could land on the table.
        Err(what) => return Some(questions_reason(what)),
    };
    let table = match QuestionsAnchor::locate(home) {
        Ok(table) => table,
        Err(path) => {
            return Some(questions_reason(&format!(
                "this write (`{}` does not resolve)",
                path.display()
            )));
        }
    };
    let mut anchors = Anchors::bare(false);
    anchors.guarded = table.guarded;
    anchors.ids = table.ids;
    // #9815 (ruling (b)): only the table's own names, not every component.
    anchors.table_names = Some(names::table_names(&table.paths));
    judge_writes(&anchors, writes, cwd, home).map(|_| questions_reason("this write"))
}

/// The deny text for a write that could reach the question table.
fn questions_reason(what: &str) -> String {
    format!(
        "Trust-anchor write denied (#9815): {what} could change \
         `~/.trusty-tools/trusty-mpm/channels/questions.jsonl`, its `.lock` or its `.torn` file, \
         or `inbound_claims.jsonl` and its `.lock`, `.torn` or `.prune` file (#9923), \
         which decide which channel answer resolves an owner question. No session may write, \
         move or delete them — the Architect included, with or without a live taint, and \
         `TRUSTY_MPM_PM_UNRESTRICTED` / `TRUSTY_MPM_DISABLE_HOOKS` do not lift this rule. Only \
         the daemon writes the question table."
    )
}

#[cfg(test)]
#[path = "pm_guard_trust_anchor_questions_tests.rs"]
mod tests;

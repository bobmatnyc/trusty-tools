//! The `[EPIC_N PHASE_M]` tag on a phase PR's title (#9571).
//!
//! Why: a phase issue's title carries `[EPIC_N PHASE_M]`, but the PR that
//! implements it did not, so the squash subject on main could not be tied to
//! its phase. `tm pr open` is the single place the tag is applied. It goes
//! AFTER the conventional prefix — `feat(x): [EPIC_12 PHASE_2] add X` — because
//! `cliff.toml` drops a subject that opens with a bracket from release notes.
//!
//! What: [`decide`] is a pure function of (the PR title, what the link-line
//! lookup found). The apply half lives in `metadata_apply`, so the edit rides
//! the post-create `gh pr edit` and inherits its partial-exit contract.
//!
//! Test: `pr_9571_*` in the sibling `phase_title_tests.rs` (the grammar and
//! [`decide`]) and `phase_title_open_tests.rs` (through `tm pr open`).

use super::metadata::RefsLookup;
use crate::commands::issue::epic::render::{epic_number_of, phase_number_of};

/// GitHub's maximum pull-request title length, in characters.
pub(crate) const GITHUB_TITLE_LIMIT: usize = 256;

/// What `tm pr open` does to the PR title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TitleDecision {
    /// Nothing to do: no phase issue is linked, or the title already carries a tag.
    Unchanged,
    /// Edit the PR to this title.
    Retitle(String),
    /// The title would earn a tag but cannot take it; the reason is printed.
    Warn(String),
    /// The linked issue could not be read, so whether a tag is owed is unknown.
    /// Carries the missing-field description (Fail-Open Check).
    Unknown(String),
}

/// The canonical `[EPIC_N PHASE_M]` tag of a phase issue title, if it is one.
///
/// Why: the phase grammar already has one parser (`render.rs`, used by
/// `tm issue epic`), and a second spelling here could disagree with it.
/// What: `Some` only when both the epic and phase numbers parse; the tag is
/// rebuilt from the numbers, so `PHASE_02` yields `PHASE_2`. A tracker title
/// (`[EPIC 12]`) or a lowercase tag yields `None`.
/// Test: `pr_9571_phase_title_grammar`.
pub(crate) fn phase_tag(issue_title: &str) -> Option<String> {
    let epic = epic_number_of(issue_title)?;
    let phase = phase_number_of(issue_title)?;
    Some(format!("[EPIC_{epic} PHASE_{phase}]"))
}

/// The byte length of the conventional-commit prefix, `": "` included.
///
/// Why: the tag is inserted right after the prefix, and only a title matching
/// the `cliff.toml` grammar `^[a-z]+(\(.+\))?!?: ` has one.
/// What: the SHORTEST prefix the grammar accepts, so a later `): ` in the
/// subject never moves the insertion point; `None` when the title does not match.
/// Test: `pr_9571_conventional_prefix_grammar`.
pub(crate) fn conventional_prefix_len(title: &str) -> Option<usize> {
    let type_len = title.bytes().take_while(u8::is_ascii_lowercase).count();
    if type_len == 0 {
        return None;
    }
    let rest = &title[type_len..];
    if !rest.starts_with('(') {
        return separator_len(rest).map(|n| type_len + n);
    }
    // `\(.+\)`: at least one character between the parentheses.
    rest.match_indices(')')
        .filter(|(i, _)| *i >= 2)
        .find_map(|(i, _)| separator_len(&rest[i + 1..]).map(|n| type_len + i + 1 + n))
}

/// `!: ` or `: ` at the start of `s`, as a length.
fn separator_len(s: &str) -> Option<usize> {
    if s.starts_with("!: ") {
        Some(3)
    } else if s.starts_with(": ") {
        Some(2)
    } else {
        None
    }
}

/// Decide the PR title from the link-line lookup.
///
/// Why (#9571): the tag must be applied without a pre-create issue read, so
/// `--dry-run` and `plan()` stay free of `gh`. The post-create step already
/// reads the linked issue, so the decision is made there.
/// What: a found phase issue retitles a conventional, untagged title, or warns
/// when the title is not conventional or would pass 256 characters. A title
/// that already contains `[EPIC_` is left alone. An unreadable issue is
/// [`TitleDecision::Unknown`] when the title could have taken a tag.
/// Test: `pr_9571_open_tags_a_phase_pr_title`, `pr_9571_title_left_unchanged`,
/// `pr_9571_an_unreadable_issue_reports_the_title_missing`.
pub(crate) fn decide(title: &str, refs: &RefsLookup<'_>) -> TitleDecision {
    let title = title.trim();
    if title.contains("[EPIC_") {
        return TitleDecision::Unchanged;
    }
    let tag = match refs {
        RefsLookup::Absent => return TitleDecision::Unchanged,
        RefsLookup::Found(issue) => match phase_tag(&issue.title) {
            Some(tag) => tag,
            None => return TitleDecision::Unchanged,
        },
        RefsLookup::Unreadable(number) => {
            // A non-conventional title would never be tagged, whatever the issue says.
            return match conventional_prefix_len(title) {
                Some(_) => TitleDecision::Unknown(format!(
                    "title (issue #{number} could not be read, so its [EPIC_N PHASE_M] tag \
                     is unknown)"
                )),
                None => TitleDecision::Unchanged,
            };
        }
    };
    let Some(at) = conventional_prefix_len(title) else {
        return TitleDecision::Warn(format!(
            "title not tagged {tag}: \"{title}\" does not match `<type>(<scope>): <subject>`"
        ));
    };
    let tagged = format!("{}{tag} {}", &title[..at], &title[at..]);
    if tagged.chars().count() > GITHUB_TITLE_LIMIT {
        return TitleDecision::Warn(format!(
            "title not tagged {tag}: the tagged title would pass GitHub's \
             {GITHUB_TITLE_LIMIT}-character limit"
        ));
    }
    TitleDecision::Retitle(tagged)
}

#[cfg(test)]
#[path = "phase_title_tests.rs"]
mod tests;

//! What the picker's launch-new action asks the daemon for: a name, and
//! whether this session gets its own git worktree (#5773).
//!
//! Why: the picker is the default human launch surface, and until #5773 it
//! could not express an isolation request at all — the wire body it built
//! carried no `worktree` key, so `SpawnRequest`'s `#[serde(default)]` decoded
//! `false` and every picker launch landed in the project's main checkout.
//! ADR-0037 decision 1 makes an explicit launch-time request the ONE input that
//! decides placement; `tm launch --worktree` was the only surface that could
//! send it. This module is where the picker's half of that request lives, and
//! it lives in one place so the flag's spelling, its wire meaning, and the note
//! shown when a launch joins an occupied checkout cannot drift from each other.
//!
//! What the note is NOT: ADR-0048 decision 8 removed the daemon's `warn!` for
//! two sessions in one checkout, because writers are isolated by the worktree
//! grant and the write boundary — so the arrangement is intended and a warning
//! about it is noise. [`shared_checkout_note`] does not reintroduce that
//! warning. It states a fact the operator cannot otherwise see (which sessions
//! are already standing in this directory) and names the alternative, in the
//! one place the operator is choosing between them. It carries no hazard
//! language, changes no daemon log line, and is absent for the first session in
//! a project — the common case.
//! What: [`LaunchIsolation`] (the request), [`LaunchNewRequest`] (name +
//! request, carried on `PickerDecision::LaunchNew`),
//! [`split_isolation_flag`] (the `n <name> --worktree` grammar), and
//! [`shared_checkout_note`].
//! Test: `split_isolation_flag_*`, `shared_checkout_note_*` in
//! `picker_launch_new_tests.rs`; the wire effect by
//! `launch_new_session_and_attach_requests_a_worktree_when_asked` in
//! `session::start_tests`.

use trusty_mpm::client::ManagedSessionSummary;

/// The token the picker accepts, spelled exactly as `tm launch`'s flag.
///
/// One spelling, so an operator who learned it on either surface can use it on
/// the other.
pub(crate) const WORKTREE_FLAG: &str = "--worktree";

/// Where a launch-new asks to be placed.
///
/// Why: ADR-0037 decision 1 gives placement exactly one input — an explicit
/// launch-time request — so the picker needs a value to carry that request
/// from the parsed input to the wire body. A named type rather than a bare
/// `bool` keeps the two directions ("no request" vs "the main checkout is
/// wrong for this session") from reading identically at every call site.
/// What: [`Self::SessionCheckout`] is the absent request, which ADR-0037
/// resolves to the project's main checkout; [`Self::OwnWorktree`] is
/// `--worktree`.
/// Test: `split_isolation_flag_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum LaunchIsolation {
    /// No isolation request — the session runs in the project's main checkout.
    #[default]
    SessionCheckout,
    /// `--worktree` — provision this session its own git worktree.
    OwnWorktree,
}

impl LaunchIsolation {
    /// Whether this request sets the spawn body's `worktree` key.
    pub(crate) const fn requests_worktree(self) -> bool {
        matches!(self, Self::OwnWorktree)
    }
}

/// A parsed launch-new: the sanitized name leaf, and the isolation asked for.
///
/// Why: `PickerDecision::LaunchNew` used to carry only `Option<String>`, so
/// there was nowhere for an isolation request to travel between
/// `parse_picker_choice` and the launch call. Both fields describe the same
/// one action, so they ride together rather than as parallel variants.
/// What: `name_hint` is `None` for the bare-Enter / `[N]` / bare-`n` paths and
/// `Some(leaf)` for `n <name>`, already kebab-cased (see
/// `session_picker::parse_picker_choice`). `isolation` defaults to
/// [`LaunchIsolation::SessionCheckout`].
/// Test: `guided_picker_n_*` in `tests_behavior_c_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct LaunchNewRequest {
    /// Operator-supplied name leaf, already sanitized; `None` when unnamed.
    pub(crate) name_hint: Option<String>,
    /// The placement the operator asked for.
    pub(crate) isolation: LaunchIsolation,
}

impl LaunchNewRequest {
    /// An unnamed launch into the session's default placement.
    pub(crate) fn unnamed() -> Self {
        Self::default()
    }

    /// A launch carrying an already-sanitized name leaf.
    pub(crate) fn named(leaf: impl Into<String>) -> Self {
        Self {
            name_hint: Some(leaf.into()),
            isolation: LaunchIsolation::SessionCheckout,
        }
    }

    /// Attach an isolation request to this launch.
    pub(crate) fn with_isolation(mut self, isolation: LaunchIsolation) -> Self {
        self.isolation = isolation;
        self
    }
}

/// Split an `n` remainder into its name text and its isolation request.
///
/// Why: `--worktree` has to be recognisable in the one position an operator
/// naturally types it — before or after the name — without a flag parser, and
/// without swallowing a name that merely contains the text. A `n <name>` name
/// may be several words (`n My Auth Fix!`), so the flag is accepted only as a
/// whole leading or trailing token; anywhere else it is name text.
/// What: returns `(name, isolation)`. `name` is trimmed and may be empty (the
/// bare `n` and bare `n --worktree` forms). `foo--worktree` is a name, not a
/// request.
/// Test: `split_isolation_flag_bare_remainder`, `split_isolation_flag_trailing`,
/// `split_isolation_flag_leading`, `split_isolation_flag_flag_only`,
/// `split_isolation_flag_requires_a_whole_token`.
pub(crate) fn split_isolation_flag(rest: &str) -> (&str, LaunchIsolation) {
    let rest = rest.trim();
    if let Some(head) = rest.strip_suffix(WORKTREE_FLAG)
        && (head.is_empty() || head.ends_with(char::is_whitespace))
    {
        return (head.trim_end(), LaunchIsolation::OwnWorktree);
    }
    if let Some(tail) = rest.strip_prefix(WORKTREE_FLAG)
        && (tail.is_empty() || tail.starts_with(char::is_whitespace))
    {
        return (tail.trim_start(), LaunchIsolation::OwnWorktree);
    }
    (rest, LaunchIsolation::SessionCheckout)
}

/// One line naming the sessions already standing in the checkout this launch
/// is about to join, or `None` when it joins nobody (#5773).
///
/// Why: the picker's rows show a name and a state, never a working directory,
/// so an operator launching a third session into one checkout saw nothing at
/// all — the daemon records the sharing, in a log the operator is not reading.
/// This is the missing fact, not a gate: it never refuses, never asks for a
/// confirmation, and never calls the sharing a problem. ADR-0048 decision 8's
/// reasoning is preserved exactly — sharing a read-only checkout is the
/// intended arrangement, so the line reads as a statement about where this
/// session lands and what the alternative is.
/// What: `None` when `isolation` already asks for a worktree (this launch joins
/// nobody), or when no `active` session's `workspace_path` equals `target`.
/// Otherwise a `tm: `-prefixed line naming the ordinal, the checkout, the
/// sessions already there, and [`WORKTREE_FLAG`]. Only `active` sessions count:
/// a stopped record's runtime is not standing anywhere, which is the same
/// predicate the daemon's own detector uses.
/// Test: `shared_checkout_note_absent_for_an_empty_checkout`,
/// `shared_checkout_note_names_the_occupants`,
/// `shared_checkout_note_counts_only_active_sessions`,
/// `shared_checkout_note_absent_when_isolation_was_requested`.
pub(crate) fn shared_checkout_note(
    sessions: &[ManagedSessionSummary],
    target: &str,
    isolation: LaunchIsolation,
) -> Option<String> {
    if isolation.requests_worktree() {
        return None;
    }
    let target_path = std::path::Path::new(target);
    let occupants: Vec<&str> = sessions
        .iter()
        .filter(|s| !s.deleted && s.state.eq_ignore_ascii_case("active"))
        .filter(|s| {
            s.workspace_path
                .as_deref()
                .is_some_and(|p| std::path::Path::new(p) == target_path)
        })
        .map(|s| s.name.as_str())
        .collect();
    if occupants.is_empty() {
        return None;
    }
    Some(format!(
        "tm: this is session {} in {target}, shared with {} — add `{WORKTREE_FLAG}` to give \
         this one its own worktree instead.",
        occupants.len() + 1,
        occupants.join(", ")
    ))
}

#[cfg(test)]
#[path = "picker_launch_new_tests.rs"]
mod tests;

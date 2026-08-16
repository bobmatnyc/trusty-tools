//! Unit tests for the picker's launch-new request (#5773).
//!
//! Why: the isolation flag's grammar and the shared-checkout note are pure
//! functions, so they are provable without stdin, tmux, or a daemon. Kept in
//! their own file and included via `picker_launch_new.rs`'s `#[path]`
//! `mod tests`, matching the `session_picker_tests.rs` convention.
//! What: `split_isolation_flag_*` (the `n <name> --worktree` grammar) and
//! `shared_checkout_note_*` (who is already standing in the checkout).

use trusty_mpm::client::ManagedSessionSummary;

use super::{LaunchIsolation, LaunchNewRequest, shared_checkout_note, split_isolation_flag};

/// Minimal `ManagedSessionSummary` fixture with a state and a workspace path.
fn session(name: &str, state: &str, workspace_path: Option<&str>) -> ManagedSessionSummary {
    ManagedSessionSummary {
        id: format!("{name}-id"),
        name: name.to_string(),
        state: state.to_string(),
        persisted_state: None,
        workspace_path: workspace_path.map(str::to_string),
        repo_url: None,
        branch: None,
        created_at: None,
        last_activity_at: None,
        pending_decision: None,
        proposed_default: None,
        source_id: None,
        task: None,
        cwd: None,
        claude_session_id: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: None,
        unresumable: false,
        stale_assets: false,
        stale_assets_unchecked: false,
        attached: false,
        slot: 1,
        deleted: false,
    }
}

// ── split_isolation_flag (#5773) ────────────────────────────────────────────

/// A remainder with no flag is entirely name text, and asks for nothing.
#[test]
fn split_isolation_flag_bare_remainder() {
    assert_eq!(
        split_isolation_flag("auth refactor"),
        ("auth refactor", LaunchIsolation::SessionCheckout)
    );
    assert_eq!(
        split_isolation_flag(""),
        ("", LaunchIsolation::SessionCheckout)
    );
}

/// `n <name> --worktree` — the flag trails the name, which is where an
/// operator who knows `tm launch --worktree` will type it.
#[test]
fn split_isolation_flag_trailing() {
    assert_eq!(
        split_isolation_flag("auth-refactor --worktree"),
        ("auth-refactor", LaunchIsolation::OwnWorktree)
    );
    // A multi-word name keeps every word; only the flag token is removed.
    assert_eq!(
        split_isolation_flag("My Auth Fix! --worktree"),
        ("My Auth Fix!", LaunchIsolation::OwnWorktree)
    );
}

/// `n --worktree <name>` — the flag leads, which is the shell habit.
#[test]
fn split_isolation_flag_leading() {
    assert_eq!(
        split_isolation_flag("--worktree auth-refactor"),
        ("auth-refactor", LaunchIsolation::OwnWorktree)
    );
}

/// `n --worktree` alone is an unnamed isolated launch, not a name of `--worktree`.
#[test]
fn split_isolation_flag_flag_only() {
    assert_eq!(
        split_isolation_flag("--worktree"),
        ("", LaunchIsolation::OwnWorktree)
    );
}

/// The flag is a whole token or it is name text.
///
/// Why: without the whole-token test, `n refactor--worktree` would silently
/// become an isolated launch named `refactor`, which is a placement the
/// operator did not choose.
#[test]
fn split_isolation_flag_requires_a_whole_token() {
    assert_eq!(
        split_isolation_flag("refactor--worktree"),
        ("refactor--worktree", LaunchIsolation::SessionCheckout)
    );
    assert_eq!(
        split_isolation_flag("--worktreeish name"),
        ("--worktreeish name", LaunchIsolation::SessionCheckout)
    );
}

/// The request type carries both halves of one action.
#[test]
fn launch_new_request_carries_name_and_isolation() {
    assert_eq!(LaunchNewRequest::unnamed().name_hint, None);
    assert_eq!(
        LaunchNewRequest::unnamed().isolation,
        LaunchIsolation::SessionCheckout
    );
    let req = LaunchNewRequest::named("auth").with_isolation(LaunchIsolation::OwnWorktree);
    assert_eq!(req.name_hint.as_deref(), Some("auth"));
    assert!(req.isolation.requests_worktree());
}

// ── shared_checkout_note (#5773) ────────────────────────────────────────────

/// The first session in a project — the common case — sees nothing.
#[test]
fn shared_checkout_note_absent_for_an_empty_checkout() {
    let sessions = vec![session("tm-other-01", "active", Some("/work/elsewhere"))];
    assert_eq!(
        shared_checkout_note(&sessions, "/work/proj", LaunchIsolation::SessionCheckout),
        None
    );
    assert_eq!(
        shared_checkout_note(&[], "/work/proj", LaunchIsolation::SessionCheckout),
        None
    );
}

/// Joining an occupied checkout names the ordinal, the directory, every
/// session already there, and the alternative.
///
/// FAILS BEFORE #5773: nothing computed this — the picker's rows show a name
/// and a state and no working directory, so the operator saw only a success
/// message.
#[test]
fn shared_checkout_note_names_the_occupants() {
    let sessions = vec![
        session("tm-task-a-01", "active", Some("/work/proj")),
        session("tm-task-b-01", "active", Some("/work/proj")),
    ];
    let note = shared_checkout_note(&sessions, "/work/proj", LaunchIsolation::SessionCheckout)
        .expect("a launch joining two live sessions must say so");
    assert!(note.contains("session 3"), "ordinal missing: {note}");
    assert!(note.contains("/work/proj"), "checkout missing: {note}");
    assert!(note.contains("tm-task-a-01"), "occupant missing: {note}");
    assert!(note.contains("tm-task-b-01"), "occupant missing: {note}");
    assert!(note.contains("--worktree"), "remedy missing: {note}");
    // ADR-0048 decision 8: sharing a checkout is the intended arrangement, so
    // this line must not reintroduce the warning that decision removed.
    let lowered = note.to_lowercase();
    for alarm in ["warn", "collision", "race", "danger", "unsafe"] {
        assert!(
            !lowered.contains(alarm),
            "the note must inform, not alarm — found {alarm:?} in: {note}"
        );
    }
}

/// A stopped record's runtime is not standing in the checkout, so it is not an
/// occupant — the same `Active` predicate the daemon's own detector uses.
#[test]
fn shared_checkout_note_counts_only_active_sessions() {
    let sessions = vec![
        session("tm-stopped-01", "stopped", Some("/work/proj")),
        session("tm-live-01", "active", Some("/work/proj")),
    ];
    let note = shared_checkout_note(&sessions, "/work/proj", LaunchIsolation::SessionCheckout)
        .expect("one live occupant must still produce the note");
    assert!(
        note.contains("session 2"),
        "ordinal must count 1 live: {note}"
    );
    assert!(
        !note.contains("tm-stopped-01"),
        "a stopped record is not standing there: {note}"
    );
}

/// A launch that already asked for a worktree joins nobody, so there is
/// nothing to report.
#[test]
fn shared_checkout_note_absent_when_isolation_was_requested() {
    let sessions = vec![session("tm-task-a-01", "active", Some("/work/proj"))];
    assert_eq!(
        shared_checkout_note(&sessions, "/work/proj", LaunchIsolation::OwnWorktree),
        None
    );
}

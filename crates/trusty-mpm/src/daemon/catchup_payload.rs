//! The `session_context_catchup` response body, shaped from its parts.
//!
//! Why: split out of `mcp_context.rs` to keep that file under the 500-SLOC
//! production cap; the shaping is a pure function, so it lives apart from the
//! I/O that gathers its inputs.
//! What: [`catchup_payload`], the [`HydrationReceipt`] it reports, and
//! [`resolved_note`], the reason a null `resolved_snapshot` was not tried
//! further (#8408).
//! Test: the `catchup_payload_*` tests in `super::mcp_context`, plus this
//! file's `resolved_note_*` tests.

use serde_json::{Value, json};

use crate::core::catchup::resolve::{CallerIdentity, ResolvedSnapshot, session_name_of};
use crate::daemon::catchup_bounds::{CATCHUP_BUDGET_BYTES, bound_catchup};
use crate::daemon::catchup_superseded::SnapshotFreshness;

/// `resolved_note` when the tmux-session route was skipped for want of a
/// creation time (#8408).
pub(crate) const TMUX_SESSION_ROUTE_NOT_TRIED: &str = "tmux_session route not tried: \
     tmux_session_created was not supplied; pass the tmux #{session_created} of \
     the caller's session to try it";

/// Why a catch-up resolved nothing although the caller named a resumable window.
///
/// Why: #8408 — with `tmux_window` but no `tmux_session_created`, a window-route
/// miss returned `resolved_snapshot: null` and `resolved_via: null`, which reads
/// exactly like "nothing paused". The cause was only in prose.
/// What: [`TMUX_SESSION_ROUTE_NOT_TRIED`] when nothing resolved, the window
/// names a tmux session (so the route would have run), and no creation time was
/// supplied; `None` in every other case.
/// Test: `resolved_note_names_only_the_skipped_tmux_session_route`,
/// `session_context_catchup_resolves_by_tmux_session_after_the_window_id_changes`.
pub(crate) fn resolved_note(
    resolved: Option<&ResolvedSnapshot>,
    caller: &CallerIdentity<'_>,
) -> Option<&'static str> {
    let route_open = caller.tmux_window.and_then(session_name_of).is_some();
    (resolved.is_none() && route_open && caller.tmux_session_created.is_none())
        .then_some(TMUX_SESSION_ROUTE_NOT_TRIED)
}

/// Shape the merged digest into the `session_context_catchup` response body.
///
/// Why: `undatable_sessions_dropped` is a receipt — an empty `sessions` array
/// means "nothing paused" only when it is 0 — and nothing else pins that it
/// reaches the wire. No end-to-end test can drive it non-zero, because an
/// undatable session is unreachable through the filesystem once both
/// `PausedSession` arms fall back to mtime, so substituting a literal `0` here
/// would leave the suite green while the receipt stopped working (#5072). A
/// pure function is the seam that makes the field assertable.
///
/// #5557: the same argument now covers SIZE. The digest arrays used to go on
/// the wire whole, so the body grew with the project's snapshot history until
/// the harness could no longer deliver it. They are paged through
/// [`bound_catchup`] here, and the page's own receipt —
/// `truncated` / `truncation_notice` / `sessions_total` / `sessions_next_offset`
/// — travels with it, because a capped response that reads exactly like a
/// complete one recreates the silent-loss defect the withheld count exists to
/// prevent.
/// What: the seven original response keys, unchanged in meaning, plus six
/// additive paging keys. `resolved_via` names which lookup produced
/// `resolved_snapshot` (`session_id`, `tmux_window`, `tmux_session` since
/// #8408, or `null` alongside a null
/// snapshot), so a caller can tell an exact match from the window fallback
/// instead of reading both as ownership. `watermark_advanced` is always `false`
/// by construction — no path in this module calls `save_catchup_state`.
///
/// #7501: three more keys say whether the resolved snapshot has been overtaken
/// — `resolved_snapshot_superseded`, `commits_since_snapshot` and
/// `commits_since_snapshot_total`. A snapshot's `next_steps` can go stale within
/// minutes of the pause, and a PM reading them as current re-plans work that
/// already merged; the commits since are the evidence that settles which.
///
/// #8408: `resolved_note` is added only when `note` is `Some` (see
/// [`resolved_note`]); every other response omits the key.
/// Test: `catchup_payload_carries_the_undatable_drop_count`,
/// `session_context_catchup_returns_expected_shape`,
/// `catchup_payload_bounds_an_oversized_store`,
/// `catchup_payload_announces_what_it_withheld`,
/// `catchup_payload_reports_a_superseded_snapshot`,
/// `resolved_note_reaches_the_wire_only_when_set`.
pub(crate) fn catchup_payload(
    merged: trusty_common::catchup::CatchupJson,
    sessions_offset: usize,
    resolved: Option<ResolvedSnapshot>,
    note: Option<&'static str>,
    session_refs: HydrationReceipt,
    freshness: SnapshotFreshness,
) -> Value {
    let (snapshot, via) = match resolved {
        Some(r) => (Some(r.path.display().to_string()), Some(r.via.as_str())),
        None => (None, None),
    };
    let undatable_sessions_dropped = merged.undatable_sessions_dropped;
    // #5557: page the digest so the body cannot outgrow what a caller can read.
    let page = bound_catchup(merged, sessions_offset, CATCHUP_BUDGET_BYTES);
    let mut body = json!({
        "sessions": page.sessions,
        "sessions_total": page.sessions_total,
        "sessions_offset": page.sessions_offset,
        "sessions_next_offset": page.next_offset(),
        "recent_commits": page.recent_commits,
        "recent_commits_total": page.recent_commits_total,
        "recent_memory": page.recent_memory,
        "recent_memory_total": page.recent_memory_total,
        "truncated": page.truncated(),
        "over_budget": page.over_budget(),
        "page_bytes": page.page_bytes,
        "truncation_notice": page.truncation_notice(),
        "resolved_snapshot": snapshot,
        "resolved_via": via,
        // #7501: a snapshot the repo has moved past describes work that may
        // already be finished — the commits since are what says which.
        "resolved_snapshot_superseded": freshness.superseded,
        "commits_since_snapshot": freshness.commits_since,
        "commits_since_snapshot_total": freshness.total_since,
        "undatable_sessions_dropped": undatable_sessions_dropped,
        "watermark_advanced": false,
        // #7830 review: hydration used to fail into `tracing::debug!`, below
        // the default filter, so a resume that silently read an empty cache
        // looked identical to one with nothing to restore.
        "session_refs": {
            "hydrated": session_refs.hydrated,
            "refs_seen": session_refs.refs_seen,
            "own_ref_found": session_refs.own_ref_found,
            "restored": session_refs.restored,
            "error": session_refs.error,
        },
    });
    if let (Some(note), Some(map)) = (note, body.as_object_mut()) {
        map.insert("resolved_note".into(), json!(note));
    }
    body
}

/// What one session-ref hydration pass produced, as the catch-up reports it.
///
/// Why (#7830 review): the resume digest is only as complete as the cache it
/// was built from. When hydration cannot run — no `origin`, no user id, an
/// unreachable remote — the caller must be able to see that the cache was NOT
/// refreshed, rather than infer "nothing paused" from an empty digest.
/// What: whether the pass completed, how many refs the aggregator saw, whether
/// the caller's OWN ref was among them, how many snapshots were restored, and
/// the failure text otherwise. `Default` is the "disabled" state.
///
/// `refs_seen` alone cannot be read as success (#7830 review round 2): after a
/// hostname change or a `gh` account switch every old ref is unreachable
/// forever, so `hydrated: true, refs_seen: 5, own_ref_found: false,
/// restored: 0` is a real and permanent state that used to look identical to
/// "nothing to do". The field is `own_ref_found` rather than `owned` because
/// `sessions[].owned` in the same response is a different, per-session concept
/// (#7830 review round 3).
/// Test: `catchup_reports_a_hydration_failure_without_failing_the_catchup`,
/// `catchup_hydrates_a_deleted_cache_from_the_session_ref`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HydrationReceipt {
    /// Whether the hydration pass ran to completion.
    pub(crate) hydrated: bool,
    /// How many `refs/tm/sessions/**` refs the aggregator enumerated.
    pub(crate) refs_seen: usize,
    /// Whether the caller's own ref was among them.
    pub(crate) own_ref_found: bool,
    /// How many snapshot files this pass wrote back.
    pub(crate) restored: usize,
    /// Why the pass did not complete, when it did not.
    pub(crate) error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::catchup::resolve::ResolutionPath;

    /// Why (#8408): the note must mark exactly the case where the tmux-session
    /// route was skipped for want of a creation time, or a null snapshot is
    /// again ambiguous — in either direction.
    /// What: only a null resolution, a window naming a tmux session, and no
    /// creation time yields the note; a supplied time (the route ran), no
    /// window, a malformed window, or a resolved snapshot yields none.
    /// Test: itself.
    #[test]
    fn resolved_note_names_only_the_skipped_tmux_session_route() {
        let window = Some("tm-supervisor:0:@262");
        let hit = ResolvedSnapshot::new("/tmp/s.md".into(), ResolutionPath::TmuxWindow);
        let caller = |w, created| CallerIdentity::new(None, w).with_tmux_session_created(created);

        assert_eq!(
            resolved_note(None, &caller(window, None)),
            Some(TMUX_SESSION_ROUTE_NOT_TRIED)
        );
        for (resolved, w, created) in [
            (None, window, Some(1_600_000_000)),
            (None, None, None),
            (None, Some("not-a-window"), None),
            (Some(&hit), window, None),
        ] {
            assert_eq!(
                resolved_note(resolved, &caller(w, created)),
                None,
                "{w:?} {created:?} resolved={}",
                resolved.is_some()
            );
        }
    }

    /// Why (#8408): the key is additive — an older caller must see the exact
    /// body it saw before whenever there is nothing to note.
    /// What: `Some` puts the note on the wire; `None` omits the key entirely.
    /// Test: itself.
    #[test]
    fn resolved_note_reaches_the_wire_only_when_set() {
        let body = |note| {
            catchup_payload(
                Default::default(),
                0,
                None,
                note,
                HydrationReceipt::default(),
                SnapshotFreshness::default(),
            )
        };
        let noted = body(Some(TMUX_SESSION_ROUTE_NOT_TRIED));
        assert_eq!(noted["resolved_note"], TMUX_SESSION_ROUTE_NOT_TRIED);
        assert!(body(None).get("resolved_note").is_none());
    }
}

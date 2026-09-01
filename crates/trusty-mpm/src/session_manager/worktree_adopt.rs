//! Transfer a worktree's ownership away from a session or agent that is
//! provably dead (#6497).
//!
//! Why: a session can die holding an agent's worktree that still contains
//! uncommitted work, and until #6497 there was no compliant way for a successor
//! to finish that work. Every write route into the tree is refused — the
//! ownership sentinel still names the dead owner, so `worktree_reclaim`'s gate 4
//! spares it and the write-boundary guard keeps a successor out — while the
//! serialize-instead route is refused too, because the daemon's delegation
//! records still name the dead session's agents as running in the shared
//! checkout. The recorded workaround was to rebuild the branch by hand in a new
//! tree, which loses the original tree's identity and leaves both stale claims
//! in place.
//!
//! What: [`evaluate_adoption`] — the pure policy, which permits a transfer ONLY
//! when the current owner is positively known to be gone — and
//! [`adopt_worktree`], which rewrites the sentinel to name the adopting managed
//! session. Adoption is an EXPLICIT operator verb
//! (`tm session adopt-worktree <path> --as <session>`), never automatic: a tree
//! that changes hands on its own is indistinguishable from one that was taken
//! from a working agent.
//!
//! FAIL-CLOSED, in the same direction as every other worktree gate here: the
//! only permitting arm requires positive evidence of death. An owner the
//! registry has merely never heard of is UNDETERMINABLE, not absent (ADR-0045),
//! and is refused — a delegation map is rebuilt empty at every daemon boot, so
//! its silence proves nothing about an agent that is still working.
//!
//! What adoption does NOT do, stated rather than hidden: it changes the
//! sentinel, so it changes what the reclaim and prune gates believe about the
//! tree. It does not move the harness's own worktree isolation — a subagent
//! confined to a different tree still cannot reach into this one — and it does
//! not commit, push, or merge anything.
//! Test: `worktree_adopt_tests`.

use std::path::Path;

use super::decommission::WORKTREE_SENTINEL_FILE;
use super::record::ManagedSessionId;
use super::worktree_ownership::{AgentDelegationState, SentinelOwner, sentinel_payload_bytes};

/// What the caller was able to establish about the CURRENT owner's liveness
/// (#6497).
///
/// Why: the policy must not go looking for this itself. The two owner shapes
/// resolve through completely different registries — a managed session through
/// the session-record store, a dispatched agent through the delegation map —
/// and only the daemon holds both. Passing the answer in keeps the policy a
/// pure function and forces every caller to say which of the three answers it
/// actually has, rather than collapsing "not found" into "gone".
/// What: [`Dead`](Self::Dead) — the owner is positively known to have ended;
/// [`Alive`](Self::Alive) — positively known to be running;
/// [`Undeterminable`](Self::Undeterminable) — the registry could not answer,
/// which refuses.
/// Test: `adoption_refuses_a_live_owner`, `adoption_takes_a_dead_owners_tree`,
/// `adoption_refuses_an_owner_the_registry_never_heard_of`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnerLiveness {
    /// The owner has provably ended — its record is terminal or its session is
    /// gone from the registry that tracked it.
    Dead,
    /// The owner is still running.
    Alive,
    /// The registry holds no usable answer.
    Undeterminable,
}

impl OwnerLiveness {
    /// Map the delegation registry's answer for an agent onto this axis
    /// (#6497).
    ///
    /// Why: [`AgentDelegationState::Unknown`] and
    /// [`AgentDelegationState::Ended`] are the same empty-looking answer read
    /// two ways, and only one of them is evidence — the distinction #5661
    /// established for the reclaim gate. Adoption is a weaker action than
    /// deletion but it still takes a tree away from whoever holds it, so it
    /// keeps the same reading.
    /// Test: `adoption_refuses_an_owner_the_registry_never_heard_of`.
    pub(crate) fn from_agent_state(state: AgentDelegationState) -> Self {
        match state {
            AgentDelegationState::Live => Self::Alive,
            AgentDelegationState::Ended => Self::Dead,
            AgentDelegationState::Unknown => Self::Undeterminable,
        }
    }
}

/// Whether a worktree may change hands, and if not, why not (#6497).
///
/// Test: every test in `worktree_adopt_tests`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AdoptionVerdict {
    /// The current owner is provably gone and nothing live claims the tree.
    Adopt,
    /// Refused — `reason` is the operator-facing explanation.
    Refuse(String),
}

/// Decide whether `owner`'s claim on a worktree may be transferred (#6497).
///
/// Why: this is the whole safety argument for the verb, written so `Adopt` is
/// reachable only by falling off the end. Both conditions the issue asks for are
/// gates here, in this order, and each one `return`s.
/// What: three gates.
/// 1. **A readable claim.** [`SentinelOwner::Unknown`] means the sentinel is
///    absent, empty or unparsable, so there is no claim to transfer and nothing
///    to prove dead. Refused.
/// 2. **The owner is provably gone.** Only [`OwnerLiveness::Dead`] proceeds;
///    `Alive` refuses because taking a working owner's tree is the harm this
///    verb must never cause, and `Undeterminable` refuses because an
///    unanswerable liveness question is not an absent one (ADR-0045).
/// 3. **Nothing references the tree since the claim.** `live_claimants` is the
///    set of agents or sessions the caller found still working IN this
///    directory. A non-empty set refuses whatever the sentinel says: the
///    sentinel names who provisioned the tree, not everyone who is in it.
///
/// Test: `adoption_refuses_an_unreadable_claim`, `adoption_refuses_a_live_owner`,
/// `adoption_refuses_an_owner_the_registry_never_heard_of`,
/// `adoption_refuses_a_tree_something_still_works_in`,
/// `adoption_takes_a_dead_owners_tree`.
pub(crate) fn evaluate_adoption(
    path: &Path,
    owner: &SentinelOwner,
    owner_liveness: OwnerLiveness,
    live_claimants: &[String],
) -> AdoptionVerdict {
    // Gate 1 (#6497): no claim to transfer, and an unreadable claim could be
    // hiding a live one.
    if matches!(owner, SentinelOwner::Unknown) {
        return AdoptionVerdict::Refuse(format!(
            "{} carries no readable ownership sentinel — absent, empty or malformed. There is no \
             claim to transfer, and an unreadable claim could be hiding a live one, so adoption \
             cannot prove this tree is free (ADR-0045).",
            path.display()
        ));
    }
    // Gate 2 (#6497): the owner must be positively gone.
    match owner_liveness {
        OwnerLiveness::Dead => {}
        OwnerLiveness::Alive => {
            return AdoptionVerdict::Refuse(format!(
                "{} is owned by {}, which is still running. Adoption transfers a tree away from \
                 its owner and must never take one from a live session or agent — wait for it to \
                 finish, or ask the PM to serialize behind it.",
                path.display(),
                describe_owner(owner)
            ));
        }
        OwnerLiveness::Undeterminable => {
            return AdoptionVerdict::Refuse(format!(
                "{} is owned by {}, and the registry that tracks it holds no record either way. A \
                 delegation map is rebuilt empty at every daemon boot, so its silence is \
                 undeterminable rather than absent (ADR-0045) — adoption refuses rather than \
                 guess.",
                path.display(),
                describe_owner(owner)
            ));
        }
    }
    // Gate 3 (#6497): the sentinel names who PROVISIONED the tree, never
    // everyone who is working in it.
    if !live_claimants.is_empty() {
        return AdoptionVerdict::Refuse(format!(
            "{} is still claimed by {} — something has referenced this tree since the owner's \
             claim, so it is not free to transfer.",
            path.display(),
            live_claimants.join(", ")
        ));
    }
    AdoptionVerdict::Adopt
}

/// Name the current owner for a refusal message.
///
/// Why: "owned by a dead session" without the id sends the reader to the wrong
/// registry. One line each, because the whole message is already long.
fn describe_owner(owner: &SentinelOwner) -> String {
    match owner {
        SentinelOwner::Agent(agent, _) => format!("dispatched agent {}", agent.agent_id),
        SentinelOwner::Known(session, _) => format!("managed session {session}"),
        SentinelOwner::Unknown => "an unreadable claim".to_string(),
    }
}

/// Rewrite `path`'s ownership sentinel to name `new_owner` (#6497).
///
/// Why: the sentinel IS the ownership record every gate reads, so a transfer is
/// exactly this write and nothing else — no new state, no second registry, and
/// no new expiry to keep in step with the first. Writing the SESSION shape
/// rather than a fresh agent claim is deliberate: a managed session's liveness
/// resolves through the session-record store, which has a real terminal state, so
/// the adopted tree re-enters the ordinary reclaim lifecycle instead of becoming
/// permanently unreclaimable.
/// What: `<path>/.trusty-mpm-worktree` replaced with a payload naming
/// `new_owner`, timestamped now. Refuses when `path` is not a directory, because
/// writing a sentinel into a path that is not a worktree would manufacture a
/// claim on nothing.
/// Test: `adopt_worktree_rewrites_the_sentinel`,
/// `adopt_worktree_refuses_a_path_that_is_not_a_directory`.
pub(crate) fn adopt_worktree(path: &Path, new_owner: ManagedSessionId) -> Result<(), String> {
    if !path.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    let sentinel = path.join(WORKTREE_SENTINEL_FILE);
    std::fs::write(&sentinel, sentinel_payload_bytes(new_owner))
        .map_err(|e| format!("could not write {}: {e}", sentinel.display()))
}

#[cfg(test)]
#[path = "worktree_adopt_tests.rs"]
mod worktree_adopt_tests;

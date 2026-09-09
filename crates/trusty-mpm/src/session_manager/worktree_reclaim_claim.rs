//! Who claims a reclaim candidate's directory, and whether that claim is the
//! caller's own (#2919, #6806).
//!
//! Why: gate 2 used to answer one bit — "does any session claim this path?" —
//! and a session that ran `tm session prune-worktrees --merged-prs --force`
//! from inside its own workspace therefore blocked every worktree it had just
//! created, because those worktrees sit under the workspace path its own
//! session record claims. Observed 2026-09-04 in a client monorepo: 31
//! clean, pushed, merged worktrees, all refused at `gate 2 (liveness)`, the
//! claimant being the caller. One bit cannot express "claimed, but by you",
//! so the claim set now carries session identity and the caller names itself.
//! What: [`WorkspaceClaim`] is one session's claim on one path, [`LiveClaims`]
//! is the whole claim set plus the calling session's id, and [`ClaimState`] is
//! the answer gate 2 acts on.
//!
//! **A claim outlives the session that made it (#7232).** The claim set is
//! every stored record's `workspace_path`, and a record is never dropped — it
//! is tombstoned. On 2026-09-09 one `deleted` record, for the adopted tmux pane
//! `tm-bobmatnyc`, carried the ORG-level `workspace_path`
//! `~/trusty-mpm-projects/bobmatnyc`, so its claim covered every worktree in
//! every project underneath: `tm session prune-worktrees --merged-prs` reported
//! "would remove 0" and named that one session on every candidate, across seven
//! repositories. A claim now carries [`ClaimLiveness`], and one whose session
//! the store's own tmux probe finds gone is discarded rather than obeyed. A
//! LIVE foreign session's claim still refuses exactly as #6806 made it, and an
//! unobservable probe leaves every claim live — the fail-closed direction.
//! Test: `worktree_reclaim_claim_tests`.

use std::path::{Path, PathBuf};

/// One session's claim on one workspace path (#6806).
///
/// Why: a bare `PathBuf` cannot say WHOSE claim it is, which is what both
/// halves of #6806 need — the caller's own claim must not block, and a
/// refusal must name the session that does.
/// What: the managed session id as the store spells it, and the
/// `workspace_path` that record carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceClaim {
    /// The claiming session's id.
    pub session: String,
    /// The workspace path that session claims.
    pub path: PathBuf,
    /// Whether the claiming session is still running (#7232).
    pub liveness: ClaimLiveness,
}

/// Whether the session behind a claim is still running (#7232).
///
/// Why: gate 2 reads a stored record's `workspace_path`, and a record survives
/// its session — `deleted`, `decommissioned` and crashed-`active` records all
/// keep their path. Without this the claim set is a permanent ledger of every
/// directory any session ever occupied, and a tombstone with a broad enough
/// path blocks reclaim machine-wide. The state field is NOT the test: #2919
/// measured a live session sitting in a directory whose record read terminal,
/// which is why only an actual tmux probe may mark a claim gone.
/// What: `Live` is both "the session is running" and "liveness could not be
/// established", because those must behave identically at a gate whose pass
/// deletes a directory. `SessionGone` is reserved for a probe that ANSWERED and
/// did not find the session.
/// Test: `a_dead_sessions_claim_no_longer_blocks`,
/// `a_live_foreign_sessions_claim_still_blocks`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ClaimLiveness {
    /// The session is running, or nothing could establish that it is not.
    #[default]
    Live,
    /// A liveness probe answered, and the session it names is gone.
    SessionGone,
}

/// The stand-in id for a claim whose producer only knew the path (#6806).
///
/// Why: `tm doctor`'s report-only disk probe reads workspace paths through a
/// helper shared by four checks, none of which carries session ids. It has no
/// caller identity either, so every claim it builds is foreign whatever the id
/// says — and it renders counts, never refusal text.
pub(crate) const UNATTRIBUTED_SESSION: &str = "<unattributed>";

impl WorkspaceClaim {
    /// Build a claim from a session id and the path it holds.
    ///
    /// #7232: liveness defaults to [`ClaimLiveness::Live`], so a producer that
    /// has not probed cannot accidentally weaken the gate — only
    /// [`Self::with_liveness`] can mark a claim gone.
    pub(crate) fn new(session: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self::with_liveness(session, path, ClaimLiveness::Live)
    }

    /// Build a claim whose liveness a probe has already answered (#7232).
    pub(crate) fn with_liveness(
        session: impl Into<String>,
        path: impl Into<PathBuf>,
        liveness: ClaimLiveness,
    ) -> Self {
        Self {
            session: session.into(),
            path: path.into(),
            liveness,
        }
    }

    /// A claim whose producer knows the path but not the session that holds it.
    pub(crate) fn unattributed(path: impl Into<PathBuf>) -> Self {
        Self::new(UNATTRIBUTED_SESSION, path)
    }
}

/// How a claim overlaps a candidate directory.
///
/// Why: containment direction decides safety, not merely presence. A candidate
/// that CONTAINS the claimed workspace cannot be deleted without deleting that
/// workspace; a candidate INSIDE it is a nested worktree the workspace's own
/// session may well want reclaimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Overlap {
    /// The claim does not touch the candidate.
    None,
    /// The candidate IS the claimed workspace, or contains it.
    CoversWorkspace,
    /// The candidate lies strictly inside the claimed workspace.
    Nested,
}

/// The whole live-claim set, plus who is asking (#6806).
///
/// Why: the caller's identity has to travel WITH the claim set, because the
/// only correct comparison is between a claim and the caller — and every
/// producer of the claim set (the daemon's store read, each test fixture)
/// then has to state both, rather than leaving the caller implicit and
/// defaulting to "nobody", which is the fail-closed answer.
/// What: `claims` is unfiltered by session state, exactly as the pre-#6806
/// path list was (see the #4288 note at the route's store read); `caller` is
/// the invoking managed session id, `None` when the caller named none.
/// Test: `claims_from_the_caller_alone_do_not_block`,
/// `a_foreign_sessions_claim_still_blocks`.
#[derive(Debug, Clone, Default)]
pub(crate) struct LiveClaims {
    /// Every claim the session store currently reports.
    pub claims: Vec<WorkspaceClaim>,
    /// The managed session that invoked this sweep, when it identified itself.
    pub caller: Option<String>,
}

impl LiveClaims {
    /// A claim set with no caller, for the tests and callers that only need
    /// "some other session holds these paths".
    pub(crate) fn foreign(claims: Vec<WorkspaceClaim>) -> Self {
        Self {
            claims,
            caller: None,
        }
    }

    /// Resolve `path` against this claim set (#2919, #6806).
    ///
    /// Why: containment is tested in BOTH directions and over canonical AND
    /// raw spellings, unchanged from the `is_live` predicate this replaces —
    /// if canonicalization fails on either side the raw form still matches, so
    /// a failed observation can only ever spare a worktree, never expose one.
    /// What #6806 adds is precedence over that same matching: a foreign claim
    /// outranks every caller claim, and a caller claim that COVERS the
    /// candidate (the candidate is, or contains, the caller's own workspace)
    /// outranks one that merely nests inside it. Only the nested case is
    /// permitted, so a session can never delete the workspace it is sitting in.
    /// #7232 adds one step ahead of all of that: a claim whose session a
    /// liveness probe found gone is not a claim. It is collected into
    /// [`ClaimState::DeadClaimsDiscarded`] — which permits — instead of
    /// deciding anything, so a tombstoned record's path can no longer block
    /// reclaim forever. Only [`ClaimLiveness::SessionGone`] does this, and only
    /// a probe that actually answered may set it.
    /// Test: `claim_state_matches_exact_ancestor_and_descendant_paths`,
    /// `a_caller_may_not_reclaim_its_own_workspace`,
    /// `a_dead_sessions_claim_no_longer_blocks`.
    pub(crate) fn claim_state(&self, path: &Path) -> ClaimState {
        let candidate_forms = path_forms(path);
        let mut caller_nested: Option<&str> = None;
        let mut caller_workspace: Option<&str> = None;
        let mut discarded: Vec<String> = Vec::new();
        for claim in &self.claims {
            let overlap = overlap_of(&candidate_forms, &claim.path);
            if overlap == Overlap::None {
                continue;
            }
            // #7232: before asking WHOSE claim this is, ask whether the session
            // still exists. A dead session's claim decides nothing.
            if claim.liveness == ClaimLiveness::SessionGone {
                discarded.push(claim.session.clone());
                continue;
            }
            let is_caller = self.caller.as_deref() == Some(claim.session.as_str());
            if !is_caller {
                // #6806: a foreign claim is decisive — return at once so no
                // later caller-owned claim can soften it.
                return ClaimState::Foreign {
                    session: claim.session.clone(),
                    caller: self.caller.clone(),
                };
            }
            match overlap {
                Overlap::CoversWorkspace => caller_workspace = Some(&claim.session),
                Overlap::Nested => caller_nested = Some(&claim.session),
                Overlap::None => unreachable!("skipped above"),
            }
        }
        if let Some(session) = caller_workspace {
            return ClaimState::CallerWorkspace {
                session: session.to_string(),
            };
        }
        if let Some(session) = caller_nested {
            return ClaimState::CallerNested {
                session: session.to_string(),
            };
        }
        if discarded.is_empty() {
            ClaimState::Unclaimed
        } else {
            ClaimState::DeadClaimsDiscarded {
                sessions: discarded,
            }
        }
    }
}

/// What gate 2 found out about a candidate's claimants (#6806).
///
/// Test: every variant is asserted in `worktree_reclaim_claim_tests`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClaimState {
    /// No live session claims this path.
    Unclaimed,
    /// Only the CALLING session claims it, and only because the candidate sits
    /// inside that session's workspace — the nested-worktree case #6806 is
    /// about. Permitted.
    CallerNested {
        /// The calling session's id.
        session: String,
    },
    /// The candidate IS the calling session's workspace, or contains it.
    /// Refused: a session must not delete the directory it is working in.
    CallerWorkspace {
        /// The calling session's id.
        session: String,
    },
    /// Every claim touching this path belongs to a session a liveness probe
    /// found gone (#7232). Permitted — the later gates still decide — and the
    /// discarded ids are carried so [`ClaimState::note`] can name them.
    DeadClaimsDiscarded {
        /// The dead sessions whose claims were discarded.
        sessions: Vec<String>,
    },
    /// A session other than the caller claims it. Refused.
    Foreign {
        /// The claiming session's id.
        session: String,
        /// The caller's own id, when it named one.
        caller: Option<String>,
    },
}

impl ClaimState {
    /// Why this claim refuses, or `None` when it permits.
    ///
    /// Why: #6806 closure criterion 2 — a refusal must name WHICH session
    /// holds the claim and say whether that is the caller. A caller that named
    /// no session of its own says so, because "not the caller" would be a
    /// claim about an identity nobody supplied.
    /// What: `now` words the refusal for the pre-delete re-check, which
    /// re-asks the same question against a fresher snapshot.
    /// Test: `a_foreign_sessions_claim_still_blocks`,
    /// `a_foreign_refusal_names_the_claimant_and_denies_it_is_the_caller`,
    /// `a_foreign_refusal_says_when_the_caller_named_no_session`.
    pub(crate) fn refusal(&self, now: bool) -> Option<String> {
        let tense = if now {
            "claims this workspace now"
        } else {
            "still claims this workspace"
        };
        match self {
            Self::Unclaimed | Self::CallerNested { .. } | Self::DeadClaimsDiscarded { .. } => None,
            Self::CallerWorkspace { session } => Some(format!(
                "session {session} {tense}, and that session IS the caller — this path is its \
                 own workspace, not a worktree inside it, so reclaiming it would delete the \
                 directory the caller is working in (#6806)"
            )),
            Self::Foreign {
                session,
                caller: Some(caller),
            } => Some(format!(
                "session {session} {tense}, and that is not the calling session ({caller}) \
                 (#6806)"
            )),
            Self::Foreign {
                session,
                caller: None,
            } => Some(format!(
                "session {session} {tense}, and the caller named no session of its own, so the \
                 claim cannot be attributed to it — run from inside a managed session, or stop \
                 that session, to reclaim this worktree (#6806)"
            )),
        }
    }
}

impl ClaimState {
    /// What this decision discarded, when it discarded something (#7232).
    ///
    /// Why: a claim that stops blocking has to be visible, or the gate becomes
    /// unexplainable in the other direction — an operator who read "session
    /// 51786c9c… still claims this workspace" on Monday and nothing at all on
    /// Tuesday cannot tell a fix from a regression. Gate 2 logs this beside the
    /// candidate it admitted.
    /// What: `Some(text)` only for [`Self::DeadClaimsDiscarded`], naming every
    /// dead session whose claim was set aside; `None` for every other state,
    /// which already speaks through [`Self::refusal`].
    /// Test: `a_dead_sessions_claim_no_longer_blocks`.
    pub(crate) fn note(&self) -> Option<String> {
        match self {
            Self::DeadClaimsDiscarded { sessions } => Some(format!(
                "discarded {} stale workspace claim(s) — session(s) {} claim this path but no \
                 live tmux session answers to them, so the claim is a tombstone, not an owner \
                 (#7232)",
                sessions.len(),
                sessions.join(", ")
            )),
            _ => None,
        }
    }
}

/// The canonical and raw spellings of one path.
fn path_forms(path: &Path) -> [PathBuf; 2] {
    [
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        path.to_path_buf(),
    ]
}

/// How `claimed` overlaps a candidate already reduced to its two spellings.
///
/// Containment is checked over every spelling pair; the STRONGEST overlap any
/// pair reports wins, so a claim that covers the candidate under one spelling
/// is never downgraded to nested by another.
fn overlap_of(candidate_forms: &[PathBuf; 2], claimed: &Path) -> Overlap {
    let claimed_forms = path_forms(claimed);
    let mut best = Overlap::None;
    for c in &claimed_forms {
        for p in candidate_forms {
            // `c` is the claimed workspace, `p` the candidate directory.
            let overlap = if c == p || c.starts_with(p) {
                Overlap::CoversWorkspace
            } else if p.starts_with(c) {
                Overlap::Nested
            } else {
                Overlap::None
            };
            if overlap == Overlap::CoversWorkspace {
                return Overlap::CoversWorkspace;
            }
            if overlap == Overlap::Nested {
                best = Overlap::Nested;
            }
        }
    }
    best
}

#[cfg(test)]
#[path = "worktree_reclaim_claim_tests.rs"]
mod worktree_reclaim_claim_tests;

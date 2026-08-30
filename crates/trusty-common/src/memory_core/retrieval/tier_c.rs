//! ADR-0028 Tier C — "current facts": slot admission, the default TTL, and
//! atomic retire-on-write.
//!
//! Why: a point-in-time fact fails by the OPPOSITE mechanism from a standing
//! rule (ADR-0028 §C7). A standing rule is never ranked high enough; a current
//! fact stays ranked high long after it stopped being true, because nothing
//! retires it. The estate's most-injected drawer is a 19-day-old session
//! checkpoint reaching 44.8% of turns and asserting a `origin/main` SHA that
//! has been wrong for four weeks. Mandatory retirement (D4) is the invariant
//! that prevents that, and the writer-chosen `fact_key` slot (D5) is what makes
//! the store self-limiting: `pr:4818/state` can be written fifty times and
//! still occupy exactly one slot.
//! What: [`admit_tier_c`] is the fail-closed admission gate — a write that names a
//! slot but cannot declare a valid retirement condition is refused Tier C and
//! degrades to an ordinary Tier E drawer, never admitted-and-warned.
//! [`persist_with_retirement`] is the write half: it resolves the slot's
//! current occupant through the `DRAWERS_BY_FACT_KEY` index (#4884) and commits
//! the incumbent's retirement and the newcomer's arrival in ONE redb
//! transaction, so no reader and no crash can observe a slot with two claimants
//! or with an incumbent retired and no replacement landed.
//! Test: the inline `tests` module below covers admission; the retirement
//! invariant and the concurrency guarantee live in `retrieval::tier_c_tests` —
//! `tier_c_write_retires_the_prior_slot_occupant`,
//! `tier_c_retirement_clears_the_displaced_drawers_own_fact_key`, and
//! `concurrent_tier_c_writes_to_one_slot_leave_exactly_one_claimant`.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::OwnedMutexGuard;
use uuid::Uuid;

use crate::memory_core::palace::{Drawer, PalaceId};
use crate::memory_core::store::kg::KnowledgeGraph;

/// Default Tier C lifetime when the writer names a slot but no `expires_at`
/// (ADR-0028 D4, retirement condition 3).
///
/// Why: the decay half-life is 90 days and a point-in-time fact has a useful
/// life measured in hours — a mismatch of roughly three orders of magnitude
/// (§C7). 24 hours is the ADR's chosen floor: short enough that a forgotten
/// fact stops being asserted within a day, long enough that a fact written at
/// the start of a working session survives it.
pub const TIER_C_DEFAULT_TTL_HOURS: i64 = 24;

/// Upper bound on a `fact_key`'s encoded length.
///
/// Why: the key is a redb table key in `DRAWERS_BY_FACT_KEY`. An unbounded
/// caller-supplied key is an unbounded index entry; capping it keeps a
/// pathological writer from bloating the slot index. 128 bytes is ~4x the
/// longest key the ADR gives as an example.
pub const FACT_KEY_MAX_LEN: usize = 128;

/// Why a Tier C write was refused the privileged tier.
///
/// Why: the caller needs to distinguish "you did not ask for Tier C" from "you
/// asked and were refused", because only the second is a defect the writer can
/// fix. Carrying the reason lets the MCP surface report it instead of silently
/// downgrading — fail-closed must be observable, or writers never learn that
/// their slot never took effect.
/// What: a refusal always means the drawer is written as ordinary Tier E, i.e.
/// exactly today's behaviour.
/// Test: `refuses_a_bare_unnamespaced_key`, `refuses_an_already_elapsed_ttl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TierCRefusal {
    /// The `fact_key` does not match the `<domain>:<id>/<aspect>` grammar.
    MalformedKey { key: String, detail: &'static str },
    /// An explicit `expires_at` that had already elapsed when the write ran.
    RetirementAlreadyElapsed {
        key: String,
        expires_at: DateTime<Utc>,
    },
}

impl std::fmt::Display for TierCRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MalformedKey { key, detail } => write!(
                f,
                "fact_key {key:?} {detail}; expected the ADR-0028 D5 form \
                 <domain>:<id>/<aspect> (e.g. `pr:4818/state`). Written as an \
                 ordinary drawer instead"
            ),
            Self::RetirementAlreadyElapsed { key, expires_at } => write!(
                f,
                "fact_key {key:?} was given expires_at {expires_at} which has \
                 already elapsed, so the fact declares no live window; it \
                 cannot claim a slot or retire that slot's occupant. Written \
                 as an ordinary drawer instead"
            ),
        }
    }
}

/// The outcome of the ADR-0028 D4 admission decision.
///
/// Test: the `admits_*` / `refuses_*` tests in this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TierCAdmission {
    /// No `fact_key` was supplied — this is not a Tier C write at all.
    NotRequested,
    /// Admitted, with the resolved slot and the resolved retirement instant.
    Admitted {
        fact_key: String,
        expires_at: DateTime<Utc>,
    },
    /// Asked for Tier C and refused; degrade to Tier E.
    Refused(TierCRefusal),
}

impl TierCAdmission {
    /// `"C"` when admitted, `"E"` otherwise — the tier label for a response
    /// envelope.
    pub fn tier_label(&self) -> &'static str {
        match self {
            Self::Admitted { .. } => "C",
            _ => "E",
        }
    }
}

/// Validate a `fact_key` against the ADR-0028 D5 namespaced grammar.
///
/// Why: D5 rejected the subject string and the tag set as replacement keys and
/// requires namespacing (`<domain>:<id>/<aspect>`) explicitly "to prevent
/// unrelated facts clobbering each other — a bare key like `state` would
/// collide across every workstream". Admitting a bare key would let one
/// workstream's write retire another's live fact, which is a worse failure than
/// the staleness this tier exists to fix. Enforcing the grammar at admission is
/// what makes that unreachable.
/// What: requires exactly one `:` and, after it, exactly one `/`; each of the
/// three resulting segments must be non-empty and drawn from
/// `[A-Za-z0-9._-]`. Returns the reason on failure so the refusal can name it.
/// Test: `accepts_the_adr_example_keys`, `refuses_a_bare_unnamespaced_key`,
/// `refuses_keys_with_empty_segments`, `refuses_an_over_long_key`.
pub fn validate_fact_key(key: &str) -> Result<(), &'static str> {
    if key.is_empty() {
        return Err("is empty");
    }
    if key.len() > FACT_KEY_MAX_LEN {
        return Err("is longer than 128 bytes");
    }
    let Some((domain, rest)) = key.split_once(':') else {
        return Err("has no `<domain>:` prefix");
    };
    let Some((id, aspect)) = rest.split_once('/') else {
        return Err("has no `/<aspect>` suffix");
    };
    for segment in [domain, id, aspect] {
        if segment.is_empty() {
            return Err("has an empty <domain>, <id>, or <aspect> segment");
        }
        if !segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        {
            return Err("has a segment outside [A-Za-z0-9._-]");
        }
    }
    Ok(())
}

/// Decide whether a write may enter Tier C (ADR-0028 D4).
///
/// Why: D4 is the load-bearing invariant — "a fact cannot enter Tier C without
/// declaring how it ends" — and its safety property is that the worst case
/// degrades to today's behaviour, never below it. That is only true if the gate
/// fails CLOSED: a fact with no valid retirement condition must never become
/// privileged, because a privileged fact that goes stale is worse than an
/// ordinary stale drawer (it surfaces every turn). Admitting-and-warning would
/// leave exactly that failure mode reachable, so every refusal here writes an
/// ordinary drawer instead.
/// What: `fact_key = None` is [`TierCAdmission::NotRequested`] — the caller
/// never asked. Otherwise the key must satisfy [`validate_fact_key`], and the
/// retirement instant resolves as: an explicit `expires_at` strictly after
/// `now` is used as given; an `expires_at` at or before `now` is refused (a
/// fact born expired declares no live window, and admitting it would retire a
/// possibly-live incumbent in exchange for nothing); absent `expires_at`, the
/// [`TIER_C_DEFAULT_TTL_HOURS`] default applies. `live_while` (D4 condition 2)
/// is deliberately absent — the ADR's Implementation-scope section puts it out
/// of the first wave, since no GitHub-state checker exists in the workspace.
/// `now` is a parameter so one write judges key and TTL against a single
/// instant and tests can pin the clock.
/// Test: `admits_a_well_formed_key_with_the_default_ttl`,
/// `admits_an_explicit_future_expiry_unchanged`,
/// `refuses_a_bare_unnamespaced_key`, `refuses_an_already_elapsed_ttl`,
/// `no_fact_key_is_not_a_tier_c_request`.
pub fn admit_tier_c(
    fact_key: Option<&str>,
    expires_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> TierCAdmission {
    let Some(key) = fact_key else {
        return TierCAdmission::NotRequested;
    };
    if let Err(detail) = validate_fact_key(key) {
        return TierCAdmission::Refused(TierCRefusal::MalformedKey {
            key: key.to_string(),
            detail,
        });
    }
    let resolved = match expires_at {
        Some(t) if t > now => t,
        Some(t) => {
            return TierCAdmission::Refused(TierCRefusal::RetirementAlreadyElapsed {
                key: key.to_string(),
                expires_at: t,
            });
        }
        None => now + Duration::hours(TIER_C_DEFAULT_TTL_HOURS),
    };
    TierCAdmission::Admitted {
        fact_key: key.to_string(),
        expires_at: resolved,
    }
}

/// Stamp the admission decision onto a drawer about to be written (#4886).
///
/// Why: this is the ONE place in the workspace where a drawer acquires a
/// `fact_key`. Every write path — the MCP tools, the HTTP handlers, the chat
/// tool surface, the importer, bootstrap/scan, kg_extract, and the dream cycle
/// — reaches storage through `remember_with_options`, and `RememberOptions` is
/// the only type that can carry a slot name, so routing the decision here makes
/// the D4 gate unbypassable rather than merely conventional. Paths that build a
/// `Drawer` by hand and call `kg.upsert_drawer` directly (the kuzu migration,
/// the git narrative writer, semantic consolidation) never set `fact_key`, so
/// they cannot claim a slot at all.
/// What: on admission, writes the resolved slot and retirement instant onto the
/// drawer. On refusal, writes NOTHING and logs — the drawer stays an ordinary
/// Tier E drawer, which is the fail-closed guarantee D4 rests on. When no slot
/// was requested, an explicitly supplied `expires_at` is still honoured as a
/// plain TTL (the field's pre-ADR-0028 meaning); absent one, whatever policy
/// `Drawer::with_type` applied is left intact.
/// Test: `tier_c_write_retires_the_prior_slot_occupant`,
/// `malformed_fact_key_degrades_to_tier_e`,
/// `already_elapsed_expiry_degrades_to_tier_e`,
/// `explicit_expiry_is_honoured_without_a_fact_key`.
pub(super) fn apply_admission(
    drawer: &mut Drawer,
    opts: &super::types::RememberOptions,
    palace: &crate::memory_core::palace::PalaceId,
) {
    match admit_tier_c(opts.fact_key.as_deref(), opts.expires_at, Utc::now()) {
        TierCAdmission::Admitted {
            fact_key,
            expires_at,
        } => {
            drawer.fact_key = Some(fact_key);
            drawer.expires_at = Some(expires_at);
        }
        TierCAdmission::Refused(refusal) => {
            tracing::warn!(palace = %palace, "#4886 tier C admission refused: {refusal}");
        }
        TierCAdmission::NotRequested => {
            if let Some(t) = opts.expires_at {
                drawer.expires_at = Some(t);
            }
        }
    }
}

/// Mirror a completed retirement into the in-memory drawer table (#4886).
///
/// Why: `PalaceHandle::drawers` is a full mirror of `DRAWERS` and is what
/// `list_drawers` and the L1 refresh read. Leaving the displaced drawer's
/// `fact_key` set there would reproduce, in memory, the exact two-claimants
/// state the durable write just avoided. The caller holds the drawer table's
/// write lock across this and the newcomer's push, so the two changes land
/// together or not at all.
/// What: clears `fact_key` and `expires_at` on the retired drawer, matching the
/// record `persist_with_retirement` committed. No-op when nothing was retired.
/// Test: `tier_c_retirement_clears_the_displaced_drawers_own_fact_key`.
pub(super) fn retire_in_memory(drawers: &mut [Drawer], retired_id: Option<Uuid>) {
    let Some(retired_id) = retired_id else {
        return;
    };
    for d in drawers.iter_mut().filter(|d| d.id == retired_id) {
        d.fact_key = None;
        d.expires_at = None;
    }
}

/// Persist `drawer`, atomically retiring whatever drawer currently holds its
/// `fact_key` slot (ADR-0028 D5).
///
/// Why: retire-on-write is a read-decide-write sequence — ask the index who
/// holds the slot, decide the incumbent, retire it, write the newcomer. If
/// those steps are not atomic with respect to a concurrent writer on the same
/// slot, two drawers end up claiming one slot, or an incumbent is retired
/// without a replacement landing. Two things make that unreachable here.
/// First, [`commit_and_mirror`] holds the per-palace commit-order guard across
/// this call AND the in-memory mirror that follows it, so no second writer on
/// this palace interleaves. That guard, not the write mutex (#154), is what
/// orders writers: #6366 releases the write mutex as soon as a write exhausts
/// its budget, while the commit it already dispatched keeps running.
/// Second — and this is the guarantee no lock can give — both drawer rows are
/// written in ONE redb transaction, so a crash between them is impossible and
/// no reader can observe the intermediate state. The guard orders writers; the
/// transaction makes each write indivisible.
///
/// Why the displaced drawer's own field is cleared: #4884's storage layer moves
/// the INDEX entry to the new owner but deliberately leaves the displaced
/// drawer's `DrawerRecord.fact_key` reading the old slot name — correct for
/// storage groundwork, since nothing read `fact_key` as a liveness signal then.
/// It is wrong once a write path exists: `load_drawers()` would show two
/// drawers both claiming `pr:4818/state` while only one is indexed, and any
/// future consumer that trusts the field rather than the index would read the
/// retired fact as live. Clearing the field makes the row agree with the index.
/// `expires_at` is cleared with it because on a Tier C drawer `expires_at` IS
/// the retirement condition, and supersession has already discharged it —
/// leaving it set would make the demoted record self-destruct at the next
/// open-time sweep, contradicting D6 ("demoted, never deleted") and orphaning
/// the supersession pointer #4887 will hang off it. What is left is an ordinary
/// Tier E drawer: permanent, ranked, readable — exactly D6.
///
/// What: no-ops to a plain `upsert_drawer` when the drawer claims no slot.
/// Otherwise looks the slot up via `drawer_id_for_fact_key`, and when a
/// different drawer holds it, commits `[retired_incumbent, newcomer]` through
/// `upsert_drawers_atomic`. Returns the retired drawer's id so the caller can
/// mirror the change into the in-memory drawer table. An incumbent the index
/// names but the in-memory table does not hold is logged and skipped: the
/// newcomer's write still moves the index, so the slot is never left with two
/// indexed claimants.
/// Test: `tier_c_write_retires_the_prior_slot_occupant`,
/// `tier_c_retirement_clears_the_displaced_drawers_own_fact_key`,
/// `concurrent_tier_c_writes_to_one_slot_leave_exactly_one_claimant`.
pub(super) async fn persist_with_retirement(
    ctx: &CommitCtx,
    drawer: &Drawer,
) -> Result<Option<Uuid>> {
    let Some(key) = drawer.fact_key.as_deref() else {
        ctx.kg.upsert_drawer(drawer).await?;
        return Ok(None);
    };

    let incumbent_id = ctx
        .kg
        .drawer_id_for_fact_key(key)
        .context("resolve current fact_key slot occupant")?
        .filter(|id| *id != drawer.id);

    let Some(incumbent_id) = incumbent_id else {
        ctx.kg.upsert_drawer(drawer).await?;
        return Ok(None);
    };

    // The in-memory table is a complete mirror of DRAWERS (hydrated at open,
    // maintained by remember/forget), so the point lookup avoids decoding
    // every row just to rewrite one.
    let incumbent = ctx
        .drawers
        .read()
        .iter()
        .find(|d| d.id == incumbent_id)
        .cloned();
    let Some(incumbent) = incumbent else {
        tracing::warn!(
            palace = %ctx.palace,
            fact_key = %key,
            incumbent = %incumbent_id,
            "#4886: fact_key slot names a drawer absent from the in-memory \
             table; writing the newcomer only — the index still moves, so the \
             slot keeps exactly one indexed claimant"
        );
        ctx.kg.upsert_drawer(drawer).await?;
        return Ok(None);
    };

    let mut retired = incumbent;
    retired.fact_key = None;
    retired.expires_at = None;

    // Order matters: the retirement releases the index entry it owns, then the
    // newcomer's upsert claims it. Reversed, the release would evict the
    // newcomer's own entry and report an occupied slot as free.
    ctx.kg
        .upsert_drawers_atomic(vec![retired, drawer.clone()])
        .await
        .context("commit tier-c retirement and replacement")?;
    Ok(Some(incumbent_id))
}

/// The owned slice of a palace the durable commit needs (#6366).
///
/// Why: [`commit_and_mirror`] runs in a task the caller's write budget cannot
/// cancel, so it must own what it touches rather than borrow a `PalaceHandle`.
/// Every field here is already an `Arc` on the handle, so this is three clones,
/// not a copy of the palace.
/// What: the palace id (for diagnostics), the KG handle (the durable write),
/// and the in-memory drawer table (the mirror).
/// Test: `an_abandoned_commit_still_leaves_one_claimant_for_the_slot`.
pub(super) struct CommitCtx {
    pub palace: PalaceId,
    pub kg: Arc<KnowledgeGraph>,
    pub drawers: Arc<RwLock<Vec<Drawer>>>,
    /// Test-only stall between the durable commit and the in-memory mirror.
    ///
    /// Why: the ordering guard only arbitrates when an abandoned commit's tail
    /// is still running as the next writer reaches it. That is the production
    /// shape — #6366's trigger is a slow commit on a large `kg.redb` — but a
    /// test cannot reproduce it by timing alone, because a test palace commits
    /// in microseconds. Without a seam the regression test passes whether or
    /// not the guard holds anything: measured 60/60 green with the guard
    /// released before the mirror. This is what makes it discriminating.
    /// What: `None` in every production path. When set, `commit_and_mirror`
    /// sleeps for it after the durable write and before the mirror, so the
    /// window the guard covers is wide enough for a second writer to race.
    /// Test: `an_abandoned_commit_still_leaves_one_claimant_for_the_slot`.
    #[cfg(test)]
    pub commit_delay: Option<std::time::Duration>,
}

/// Commit `drawer` durably and mirror the outcome into the in-memory table, as
/// one step no caller's timeout can split (#6366).
///
/// Why: #6366 bounds the write pipeline so an over-budget write releases the
/// palace write mutex. Dropping the pipeline future does NOT stop the commit it
/// dispatched — `upsert_drawers_atomic` runs on `spawn_blocking`, which is
/// uncancellable, and `KgWriter::upsert_drawer` hands the op to an actor task
/// the future does not own. So the durable half landed while the in-memory
/// mirror (`retire_in_memory` + `push`) never ran. The next writer then read
/// the moved `DRAWERS_BY_FACT_KEY` index, failed to find that id in `drawers`,
/// and took [`persist_with_retirement`]'s "absent from the in-memory table"
/// branch: it wrote its own newcomer WITHOUT retiring the incumbent, leaving
/// two drawer rows carrying one live `fact_key`. That is the two-claimant state
/// this module's header says no reader may observe.
///
/// What: takes `order` — the palace's commit-order guard, acquired by the
/// caller while still cancellable — and holds it across BOTH the durable commit
/// and the mirror, then releases it. Running inside `tokio::spawn` makes the
/// pair uncancellable; holding the guard across it makes the next writer's
/// incumbent read wait for the mirror rather than race it. A write abandoned
/// mid-commit therefore still converges: it lands in redb and in `drawers`
/// together, and the writer behind it retires it normally.
///
/// Panic contract: `commit_mutex` is a `tokio::sync::Mutex` and does NOT
/// poison. A panic between the durable commit and the mirror unwinds the task,
/// drops the guard, and releases the mutex with no signal — leaving the write
/// durable but unmirrored and the next writer free to walk into the fallback
/// this function exists to close. The mirror block does no fallible work today,
/// so this is latent. Code inserted between the commit and the mirror must stay
/// panic-free, or must re-verify the slot on the panic path.
///
/// Two writers abandoned in the same window commit in guard-acquisition order,
/// which is the order their callers reached the commit — not necessarily the
/// order they took the write mutex. Either order leaves exactly one claimant,
/// which is the invariant; which of the two wins the slot is not.
/// Test: `an_abandoned_commit_still_leaves_one_claimant_for_the_slot`,
/// `a_second_writer_waits_for_an_abandoned_commit_to_mirror`.
pub(super) async fn commit_and_mirror(
    ctx: CommitCtx,
    drawer: Drawer,
    order: OwnedMutexGuard<()>,
) -> Result<()> {
    let retired = persist_with_retirement(&ctx, &drawer)
        .await
        .context("persist drawer metadata")?;

    // #6366: the seam a test uses to widen the commit window to the shape the
    // issue is actually about. In production the trigger is a slow commit — a
    // 326 MB `kg.redb` — so an abandoned write's tail OUTLASTS the next
    // writer's fresh pipeline, which is the wide window this guard exists to
    // arbitrate. A test that dispatches its abandoned writer directly, skipping
    // the pre-commit legs the second writer must still run, closes that window
    // by construction and would pass whether or not the guard held anything.
    #[cfg(test)]
    if let Some(delay) = ctx.commit_delay {
        tokio::time::sleep(delay).await;
    }

    {
        // #4886: mirror the retirement under the SAME lock as the push, so a
        // reader never sees the newcomer present while the displaced drawer
        // still claims the slot. No await inside — this is a parking_lot lock.
        let mut drawers = ctx.drawers.write();
        retire_in_memory(&mut drawers, retired);
        drawers.push(drawer);
    }
    // #6366: explicit so the release is visibly ordered AFTER the mirror — the
    // next writer's incumbent read must not start before this point.
    drop(order);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(offset_hours: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).expect("fixed epoch")
            + Duration::hours(offset_hours)
    }

    #[test]
    fn accepts_the_adr_example_keys() {
        for key in [
            "pr:4818/state",
            "ws:tm-trusty-tools-03/resume",
            "daemon:trusty-search/install-state",
        ] {
            assert!(validate_fact_key(key).is_ok(), "{key} should be valid");
        }
    }

    #[test]
    fn refuses_a_bare_unnamespaced_key() {
        assert_eq!(validate_fact_key("state"), Err("has no `<domain>:` prefix"));
        assert_eq!(
            validate_fact_key("pr:4818"),
            Err("has no `/<aspect>` suffix")
        );
    }

    #[test]
    fn refuses_keys_with_empty_segments() {
        for key in [":4818/state", "pr:/state", "pr:4818/"] {
            assert!(validate_fact_key(key).is_err(), "{key} should be refused");
        }
    }

    #[test]
    fn refuses_an_over_long_key() {
        let key = format!("pr:{}/state", "x".repeat(FACT_KEY_MAX_LEN));
        assert_eq!(validate_fact_key(&key), Err("is longer than 128 bytes"));
    }

    #[test]
    fn refuses_a_key_with_extra_separators() {
        assert!(validate_fact_key("pr:4818/state/extra").is_err());
        assert!(validate_fact_key("pr:48:18/state").is_err());
    }

    #[test]
    fn no_fact_key_is_not_a_tier_c_request() {
        assert_eq!(admit_tier_c(None, None, t(0)), TierCAdmission::NotRequested);
        // A bare TTL without a slot is still not a Tier C request — it is the
        // pre-existing `expires_at` field doing what it always did.
        assert_eq!(
            admit_tier_c(None, Some(t(1)), t(0)),
            TierCAdmission::NotRequested
        );
    }

    #[test]
    fn admits_a_well_formed_key_with_the_default_ttl() {
        let now = t(0);
        assert_eq!(
            admit_tier_c(Some("pr:4818/state"), None, now),
            TierCAdmission::Admitted {
                fact_key: "pr:4818/state".to_string(),
                expires_at: now + Duration::hours(TIER_C_DEFAULT_TTL_HOURS),
            }
        );
    }

    #[test]
    fn admits_an_explicit_future_expiry_unchanged() {
        let now = t(0);
        let ttl = t(3);
        assert_eq!(
            admit_tier_c(Some("pr:4818/state"), Some(ttl), now),
            TierCAdmission::Admitted {
                fact_key: "pr:4818/state".to_string(),
                expires_at: ttl,
            }
        );
    }

    #[test]
    fn refuses_an_already_elapsed_ttl() {
        let now = t(0);
        let admission = admit_tier_c(Some("pr:4818/state"), Some(t(-1)), now);
        assert!(matches!(
            admission,
            TierCAdmission::Refused(TierCRefusal::RetirementAlreadyElapsed { .. })
        ));
        assert_eq!(admission.tier_label(), "E");
        // An expiry exactly at `now` has not elapsed under `is_expired_at`'s
        // strict comparison, but it leaves a zero-length live window — treat it
        // as elapsed so admission and read-time expiry cannot disagree.
        assert!(matches!(
            admit_tier_c(Some("pr:4818/state"), Some(now), now),
            TierCAdmission::Refused(TierCRefusal::RetirementAlreadyElapsed { .. })
        ));
    }

    #[test]
    fn a_malformed_key_is_refused_not_admitted_with_a_default() {
        let admission = admit_tier_c(Some("state"), None, t(0));
        assert!(matches!(
            admission,
            TierCAdmission::Refused(TierCRefusal::MalformedKey { .. })
        ));
        assert_eq!(admission.tier_label(), "E");
    }

    #[test]
    fn refusal_display_names_the_key_and_the_degradation() {
        let msg = TierCRefusal::MalformedKey {
            key: "state".into(),
            detail: "has no `<domain>:` prefix",
        }
        .to_string();
        assert!(msg.contains("state"), "{msg}");
        assert!(msg.contains("ordinary drawer"), "{msg}");
    }
}

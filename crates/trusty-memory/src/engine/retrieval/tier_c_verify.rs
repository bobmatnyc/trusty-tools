//! A Tier C slot's re-verify command, stored as a KG triple (#9390).
//!
//! Why: a slot names a fact a later event makes false, such as an open PR's
//! head SHA. Storing the command that re-reads it (`gh pr view 123 --json
//! headRefOid`) lets a resume pass check the value instead of trusting it. The
//! command lives in the KG as `drawer:<id> verify_with <command>`, keyed by
//! drawer id, so the drawer row and the palace format stay unchanged
//! (ADR-0067 D3; the `superseded_by` edge of #9433 is the precedent).
//! What: [`check_verify_command`] refuses a command the write cannot honour,
//! [`record_verify_command`] writes the triple for a committed slot drawer,
//! and [`close_retired_verify_command`] closes a retired drawer's triple. The
//! last two run inside `tier_c::commit_and_mirror`, under the commit-order
//! guard that also covers the slot write.
//! Test: `crates/trusty-memory/src/tools/tests/verify_with_tests.rs`, plus
//! `tier_c_verify_tests` in this directory.

use anyhow::{Result, anyhow, bail};
use uuid::Uuid;

use super::tier_c::CommitCtx;
use super::types::RememberOptions;
use crate::engine::filter::check_secret;
use crate::engine::store::kg::Triple;
use crate::engine::store::kg_store::VERIFY_WITH;

/// Provenance stamped on a `verify_with` triple.
const VERIFY_PROVENANCE: &str = "tier_c:verify-on-write";

/// A slot drawer committed, but its verify command was not stored (#9390).
///
/// Why: the drawer is durable and holds its slot, so every post-commit step
/// (deferred embed, L1 snapshot, closets, and the daemon's BM25 enqueue and
/// event) must still run, or recall cannot find it. The caller asked for the
/// command, so it must still learn the command is missing.
/// What: the committed drawer's id and the record failure. The write pipeline
/// returns it as the error AFTER its post-commit steps; a caller with its own
/// post-write steps finds `drawer` by downcasting and runs them first. A retry
/// is safe: it writes a new drawer to the slot and retires this one.
/// Test: `crates/trusty-memory/src/tools/tests/verify_with_tests.rs` (failed-record case).
#[derive(Debug, thiserror::Error)]
#[error("drawer {drawer} is stored, but its verify command was not: {reason}")]
pub struct VerifyNotStored {
    /// The committed drawer that holds the slot without a command.
    pub drawer: Uuid,
    /// The record failure, rendered with its cause chain.
    pub reason: String,
}

impl VerifyNotStored {
    /// Why: other crates build this error without a struct literal (#9278).
    /// What: sets `drawer` and `reason`.
    /// Test: `verify_not_stored_new_carries_the_drawer_and_reason`.
    pub fn new(drawer: Uuid, reason: impl Into<String>) -> Self {
        Self {
            drawer,
            reason: reason.into(),
        }
    }
}

/// Refuse a `verify_with` command this write cannot store (#9390).
///
/// Why: a command on a drawer with no slot has nothing to re-check, and a
/// blank one re-checks nothing; both are caller mistakes that must fail before
/// anything is written. The command is stored verbatim in the KG, so it passes
/// the same secret gate as the drawer content.
/// What: `Ok(None)` when `opts.verify_with` is unset. Otherwise refuses a
/// missing `fact_key`, a command that is empty after trimming, and (unless
/// `allow_secret_like`) a secret-shaped command. Returns the trimmed command.
/// Test: `a_verify_command_without_a_fact_key_is_refused_and_writes_nothing`.
pub(super) fn check_verify_command(opts: &RememberOptions) -> Result<Option<String>> {
    let Some(raw) = opts.verify_with.as_deref() else {
        return Ok(None);
    };
    if opts.fact_key.is_none() {
        bail!("a verify command needs a fact_key: only a Tier C slot can be re-verified");
    }
    let command = raw.trim();
    if command.is_empty() {
        bail!("a verify command must not be empty or whitespace");
    }
    if !opts.allow_secret_like {
        check_secret(command).map_err(|reject| anyhow!("verify command refused: {reject}"))?;
    }
    Ok(Some(command.to_string()))
}

/// Store `command` as `drawer:<drawer> verify_with <command>` (#9390).
///
/// Why: the caller asked for the command, so a failed write is an error, not a
/// warning. The drawer is already committed when this runs, so the error is a
/// [`VerifyNotStored`] the pipeline reports after its post-commit steps.
/// What: asserts one triple with confidence 1.0. `verify_with` is functional,
/// so the assert closes any earlier active command on the same drawer.
/// Test: `a_refused_slot_stores_no_verify_command`, `slot_retirement_closes_the_retirees_verify_command`,
/// and the failed-record case in `crates/trusty-memory/src/tools/tests/verify_with_tests.rs`.
pub(super) async fn record_verify_command(
    ctx: &CommitCtx,
    drawer: Uuid,
    command: &str,
) -> Result<(), VerifyNotStored> {
    let missed = |e: anyhow::Error| VerifyNotStored {
        drawer,
        reason: format!("{e:#}"),
    };
    // #9390: the test seam stands in for a failed KG write; see `PalaceHandle`.
    #[cfg(any(test, feature = "embedder-test-support"))]
    if ctx
        .fail_next_verify_record
        .swap(false, std::sync::atomic::Ordering::SeqCst)
    {
        return Err(missed(anyhow!("injected verify_with failure")));
    }
    let triple = Triple {
        subject: format!("drawer:{drawer}"),
        predicate: VERIFY_WITH.to_string(),
        object: command.to_string(),
        valid_from: chrono::Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: Some(VERIFY_PROVENANCE.to_string()),
    };
    ctx.kg.assert(triple).await.map_err(missed)
}

/// Close the `verify_with` command of a drawer its slot just retired (#9390).
///
/// Why: a retired drawer no longer holds the slot, so its command re-checks a
/// fact nobody asserts. Like the `superseded_by` edge (#9433) this runs after
/// the retirement committed, so its failure must not fail the remember; it
/// logs a WARN instead.
/// What: reads the drawer's active command and retracts the
/// `(drawer:<retired>, verify_with)` pair only when one exists, so a slot
/// written without commands costs a read, not a KG write.
/// Test: `slot_retirement_closes_the_retirees_verify_command`.
pub(super) async fn close_retired_verify_command(ctx: &CommitCtx, retired: Uuid) {
    let result = async {
        let held = ctx.kg.verify_with_many(vec![retired]).await?;
        if held.contains_key(&retired) {
            ctx.kg
                .retract(&format!("drawer:{retired}"), VERIFY_WITH)
                .await?;
        }
        anyhow::Ok(())
    }
    .await;
    if let Err(e) = result {
        tracing::warn!(
            palace = %ctx.palace,
            retired = %retired,
            "#9390: retired the fact_key incumbent but could not close its \
             verify_with command; memory_list hides it, the drawer holds no slot: {e:#}"
        );
    }
}

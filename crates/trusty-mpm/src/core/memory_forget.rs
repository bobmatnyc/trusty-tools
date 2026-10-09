//! `tm memory forget <drawer-id>` and `--fact-key <key>` — drawer removal over
//! the daemon socket (#9340).
//!
//! Why: QA and operators write throwaway drawers through `tm memory
//! remember`/`note`, and expiry cannot drop them — a retired slot's
//! `expires_at` clears and a refused slot stays as an ordinary drawer. The only
//! cleanup path was a hand-built `memory_forget` call on the socket.
//! What: the forget-specific halves of [`super::memory_verbs::MemoryVerb::Forget`]
//! — the drawer-id check that runs before any RPC, and [`forget_failure`],
//! which reads the daemon's answer. The call itself goes through
//! [`super::memory_verbs::run_verb`], so palace and socket resolution are the
//! ones `recall`/`remember`/`note` use.
//!
//! `--fact-key` resolves a Tier C slot to its drawer id first: trusty-memory has
//! no slot lookup tool, but `memory_list` reports each drawer's `fact_key`, set
//! only on the slot's current occupant. [`resolve_fact_key`] lists the whole
//! palace and [`match_fact_key`] picks the one live occupant; the forget then
//! runs as the id form. Every doubt fails closed with nothing deleted.
//! Test: `memory_verbs_tests.rs`, `tests/memory_verbs_socket.rs`.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::memory_verbs::{
    MemoryVerbError, MemoryVerbOptions, call_method, resolve_palace_for, resolve_verb_socket,
};

/// The trusty-memory method `tm memory forget` calls.
pub const FORGET_METHOD: &str = "memory_forget";

/// The trusty-memory method `--fact-key` lists the palace with.
pub const LIST_METHOD: &str = "memory_list";

/// The `limit` `--fact-key` asks `memory_list` for.
///
/// Why (#9340): `memory_list` has no cursor and defaults to 50 drawers, so the
/// only way to see a whole palace is one page larger than the palace. A page
/// that comes back this full cannot prove it held every drawer, and the
/// resolution refuses it. The client's 32 MiB frame cap may refuse a huge
/// palace first; that is a transport error, also fail-closed.
pub const FACT_KEY_LIST_LIMIT: usize = 100_000;

/// The `status` `memory_forget` reports when a drawer was removed.
const DELETED: &str = "deleted";

/// Reject a drawer id that is not a UUID, before anything is sent.
///
/// Why: the daemon rejects it too, but its error would arrive wrapped as
/// "trusty-memory did not answer", which reads as a transport fault.
/// Test: `a_malformed_drawer_id_is_refused_before_any_rpc`.
pub(crate) fn validate_drawer_id(drawer_id: &str) -> Result<(), MemoryVerbError> {
    uuid::Uuid::parse_str(drawer_id.trim())
        .map(|_| ())
        .map_err(|e| MemoryVerbError::DrawerId {
            value: drawer_id.to_string(),
            detail: e.to_string(),
        })
}

/// Why a forget the daemon answered did not remove a drawer, if it did not.
///
/// Why: `memory_forget` answers an unknown id with a SUCCESSFUL body,
/// `{"status": "not_found"}` (#5231, `handle_memory_forget`). Exiting 0 on that
/// would tell a cleanup script it removed a drawer it never touched.
/// What: `None` only when `status` is `"deleted"`. Any other status, or none,
/// returns a message naming the drawer, the palace and the status.
/// Test: `forget_failure_accepts_only_deleted`,
/// `an_unknown_drawer_exits_non_zero_and_says_nothing_was_deleted`.
pub fn forget_failure(drawer_id: &str, palace: Option<&str>, result: &Value) -> Option<String> {
    let status = result.get("status").and_then(Value::as_str);
    if status == Some(DELETED) {
        return None;
    }
    let palace = palace.unwrap_or("(none resolved)");
    Some(match status {
        Some("not_found") => {
            format!("drawer {drawer_id} not found in palace {palace}; nothing was deleted")
        }
        Some(other) => format!(
            "trusty-memory answered status {other:?} for drawer {drawer_id} in palace {palace}; \
             it did not report the drawer deleted"
        ),
        None => format!(
            "trusty-memory answered without a status for drawer {drawer_id} in palace {palace}; \
             it did not report the drawer deleted"
        ),
    })
}

/// The drawer a `--fact-key` resolved to, and where it lives.
#[derive(Debug, Clone)]
pub struct FactKeyTarget {
    /// The slot's one live occupant.
    pub drawer_id: String,
    /// The palace that was listed; the forget must name the same one.
    pub palace: String,
    /// The socket that answered the listing.
    pub socket: PathBuf,
}

/// Resolve `--fact-key` to the one drawer that holds the slot now.
///
/// Why (#9340): no trusty-memory tool maps a slot to a drawer id, and the
/// supervisor ruled the CLI filters `memory_list` rather than adding one.
/// What: resolves the palace as a write would and the socket as every verb
/// does, sends `memory_list { palace, limit: FACT_KEY_LIST_LIMIT, full: true }`
/// — `full` turns off the daemon's 48 KiB byte fold — and hands the body to
/// [`match_fact_key`]. Sends nothing else; the caller forgets by id.
///
/// # Errors
///
/// The palace, socket and call errors of [`super::memory_verbs::run_verb`], and
/// [`MemoryVerbError::FactKey`] for every arm [`match_fact_key`] refuses.
///
/// Test: `forget_by_fact_key_forgets_the_one_listed_occupant`.
pub async fn resolve_fact_key(
    fact_key: &str,
    opts: &MemoryVerbOptions,
) -> Result<FactKeyTarget, MemoryVerbError> {
    // A write never resolves to `None`: no palace is an `Err` for it.
    let palace = resolve_palace_for(true, opts)?.ok_or_else(|| MemoryVerbError::Palace {
        cwd: opts.cwd.clone().unwrap_or_default().display().to_string(),
        detail: "no palace resolved".to_string(),
    })?;
    let socket = resolve_verb_socket(opts)?;
    let params = serde_json::json!({
        "palace": palace,
        "limit": FACT_KEY_LIST_LIMIT,
        "full": true,
    });
    let listing = call_method(&socket, LIST_METHOD, params).await?;
    let drawer_id = match_fact_key(fact_key, &palace, &listing, FACT_KEY_LIST_LIMIT, Utc::now())?;
    Ok(FactKeyTarget {
        drawer_id,
        palace,
        socket,
    })
}

/// Pick the one live drawer whose listed `fact_key` equals `fact_key`.
///
/// Why (#9340): a wrong guess deletes the wrong fact, so every case that
/// cannot be proven is refused rather than resolved.
/// What, in order, each refusal a [`MemoryVerbError::FactKey`]:
/// - no `drawers` array: the answer is not a listing;
/// - `truncated: true`, or `drawers.len() >= limit`: the listing may be
///   incomplete, so absence and uniqueness are unprovable;
/// - a drawer with no `fact_key` field: the daemon predates the field, and
///   reading it as "no match" would hide that;
/// - matches whose `expires_at` is at or before `now` are dropped — the daemon
///   does not sweep an expired Tier C drawer, so it stays listed with its key,
///   but it is no longer the current fact. An unparseable `expires_at` on a
///   match is refused, since it cannot be called live or expired;
/// - zero live matches (the error names any expired ones, for the id form),
///   or more than one (the error names every candidate).
///
/// Test: `forget_by_fact_key_with_no_match_fails_closed`,
/// `forget_by_fact_key_with_two_matches_names_every_candidate`,
/// `forget_by_fact_key_on_an_incomplete_listing_fails_closed`,
/// `forget_by_fact_key_skips_an_expired_occupant`,
/// `forget_by_fact_key_against_a_pre_fact_key_daemon_fails_closed`.
pub fn match_fact_key(
    fact_key: &str,
    palace: &str,
    listing: &Value,
    limit: usize,
    now: DateTime<Utc>,
) -> Result<String, MemoryVerbError> {
    let refuse = |detail: String| MemoryVerbError::FactKey {
        key: fact_key.to_string(),
        detail,
    };
    let drawers = listing
        .get("drawers")
        .and_then(Value::as_array)
        .ok_or_else(|| refuse(format!("{LIST_METHOD} answered without a drawers array")))?;
    if listing.get("truncated").and_then(Value::as_bool) == Some(true) {
        return Err(refuse(format!(
            "{LIST_METHOD} folded its answer (truncated: true), so the listing of palace \
             {palace} is incomplete"
        )));
    }
    if drawers.len() >= limit {
        return Err(refuse(format!(
            "{LIST_METHOD} returned a full page of {limit} drawers and has no cursor, so the \
             listing of palace {palace} may be incomplete"
        )));
    }
    let (mut live, mut expired) = (Vec::new(), Vec::new());
    for drawer in drawers {
        let id = drawer
            .get("drawer_id")
            .and_then(Value::as_str)
            .unwrap_or("(no id)");
        let Some(listed_key) = drawer.get("fact_key") else {
            return Err(refuse(format!(
                "the trusty-memory daemon is too old: its {LIST_METHOD} reports no fact_key \
                 field (drawer {id}); upgrade it to resolve a slot"
            )));
        };
        if listed_key.as_str() != Some(fact_key) {
            continue;
        }
        match drawer.get("expires_at").and_then(Value::as_str) {
            None => live.push(id),
            Some(raw) => match DateTime::parse_from_rfc3339(raw) {
                Ok(at) if at <= now => expired.push(id),
                Ok(_) => live.push(id),
                Err(e) => {
                    return Err(refuse(format!(
                        "drawer {id} has an unreadable expires_at {raw:?} ({e}), so it cannot \
                         be called live"
                    )));
                }
            },
        }
    }
    match live.as_slice() {
        [one] => Ok((*one).to_string()),
        [] if expired.is_empty() => Err(refuse(format!(
            "no live drawer holds it in palace {palace}"
        ))),
        [] => Err(refuse(format!(
            "no live drawer holds it in palace {palace}; its expired occupant(s) {} can be \
             removed with `tm memory forget <drawer-id>`",
            expired.join(", ")
        ))),
        many => Err(refuse(format!(
            "{} live drawers hold it in palace {palace}: {}; forget one by id",
            many.len(),
            many.join(", ")
        ))),
    }
}

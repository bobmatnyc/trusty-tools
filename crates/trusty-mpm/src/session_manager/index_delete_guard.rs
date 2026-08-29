//! The one door a destructive trusty-search index delete may pass through (#4743).
//!
//! Why: `search_gc` had two independent sites that formatted
//! `DELETE /indexes/{id}?delete_data=true` — the decommission-time removal and
//! the orphan sweep's delete loop. Destroying the data is opt-in (#4123), and
//! the daemon those requests reached was whichever one discovery found. Under
//! `cargo test` that is the OPERATOR'S live daemon: a test process resolves it
//! exactly like production does. Fixture workspaces derive their index id from
//! a bare `file_name()` — `full`, `sess`, `live` — so a real index sharing that
//! basename is destroyed by a test run, silently.
//!
//! Why a capability type rather than an `if` at each site: two review rounds on
//! PR #4725 each found a different destructive effect that a caller had failed
//! to gate, and the response there was the same one taken here — stop relying on
//! every call site remembering, and make the ungated shape unrepresentable. A
//! caller cannot build the request, because [`DestructiveIndexDelete`] holds the
//! only copy of the `delete_data` opt-in and exposes no constructor that takes a
//! daemon address. The only way to obtain one is
//! [`DestructiveIndexDelete::acquire`], which decides for itself whether the
//! process may destroy data. A new destructive site added later inherits the
//! refusal by construction: it has to call `acquire` to get anything it can
//! delete with.
//!
//! Why the refusal is a RUNTIME check: the delete lands in a DIFFERENT PROCESS.
//! Issue #4094's `cfg(test)` arm in trusty-search's `default_data_dir` isolates
//! that daemon's own data-dir resolution, and #4255/PR #4864 extended isolation
//! to registry writes — but neither can help here. No compile-time guard in
//! trusty-search governs what a trusty-mpm test binary puts on the wire.
//! `trusty_common::running_under_test_harness` is the process-level answer, and
//! reusing it keeps this crate from growing a second, drifting copy of the same
//! detection.
//!
//! What #6285 changed: the transport, and nothing else. The call is
//! `search.index.delete` over the daemon's Unix socket (ADR-0032) rather than
//! `DELETE /indexes/{id}`, sent through [`crate::daemon::search_rpc`] — this
//! crate's one trusty-search client. Who may acquire the capability did not
//! move: the test-harness refusal is still the first thing `acquire` evaluates,
//! still ahead of any resolution, and the `delete_data` opt-in still exists in
//! exactly one place.
//!
//! Test: `acquire_is_refused_under_a_test_harness`,
//! `acquire_refuses_when_no_daemon_socket_is_bound`,
//! `acquire_succeeds_when_production_state_is_explicitly_allowed`,
//! `delete_params_opt_into_data_deletion`,
//! `delete_over_a_stale_socket_is_a_transport_failure`,
//! `a_refusal_is_never_reported_as_removed`,
//! `a_result_that_did_not_remove_is_not_reported_as_removed`; end-to-end,
//! `decommission_issues_no_request_to_a_live_daemon_under_test` in
//! `search_gc_guard_tests`.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use tracing::{debug, warn};

use crate::daemon::search_rpc::{self, SearchRpcError};

/// Per-request timeout for the index delete.
///
/// Why: the destructive call runs off the interactive request path
/// (decommission, periodic GC) but must still never hang the daemon
/// indefinitely if trusty-search is wedged.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// What a destructive index delete did, for the caller to log in its own voice.
///
/// Why: the two call sites want different log messages for the same outcomes,
/// and threading the RPC types back to them would leak the transport this
/// module exists to own.
///
/// Why four variants rather than worked / did not work: only [`Self::Removed`]
/// licenses a caller to record an index as gone. The other three are distinct
/// evidence — the daemon refused, the daemon answered but kept the index, or
/// nothing answered at all — and collapsing them lets an unanswered call read
/// as a completed one.
#[derive(Debug)]
pub(super) enum DeleteOutcome {
    /// The daemon answered, and its answer says the registration is gone.
    Removed,
    /// The daemon answered, and its answer says the index is still registered.
    ///
    /// Reachable when an in-flight writer never released the teardown lock
    /// (#3049): with `delete_data` requested, trusty-search abandons the delete
    /// rather than destroying data underneath a running write.
    NotRemoved(String),
    /// The daemon answered with an error frame — its own code and wording.
    Refused {
        /// The daemon's JSON-RPC error code.
        code: i64,
        /// The daemon's own message.
        message: String,
    },
    /// The call never got an answer.
    Transport(String),
}

/// The capability to destroy a trusty-search index's on-disk data (#4743).
///
/// Why: see the module doc — holding one of these is the proof that this
/// process is allowed to delete real data, and it is the only thing in the
/// crate that can produce a `delete_data` request.
/// What: the socket of a trusty-search daemon that was bound when the capability
/// was acquired. The field is private and there is no constructor other than
/// [`Self::acquire`], so the capability cannot be forged from a path a caller
/// happens to have.
/// Test: see the module doc.
pub(super) struct DestructiveIndexDelete {
    socket: PathBuf,
}

impl DestructiveIndexDelete {
    /// Acquire the capability, or `None` when this process must not destroy
    /// index data.
    ///
    /// Why: the refusal lives here, at the single point of acquisition, rather
    /// than at each call site — a guard a caller has to remember is one new
    /// call site away from being forgotten, which is precisely how the second
    /// destructive site in `search_gc` came to have no guard at all.
    /// What: `None` when (a) `trusty_common::running_under_test_harness()` says
    /// this is a `cargo test` process, (b) the socket path cannot be resolved,
    /// or (c) nothing is bound at it. `Some` otherwise. The test refusal is
    /// checked FIRST so a test run never even resolves where the operator's
    /// daemon lives.
    ///
    /// #6285: (c) replaces the old "no `http_addr` discovery file" arm. A bound
    /// socket is what a running daemon leaves behind, so its absence carries the
    /// same "there is nothing to talk to" signal, evaluated at the same point —
    /// before any request is built. A socket file that outlived its daemon still
    /// passes here and fails at [`Self::delete`], which reports it as
    /// [`DeleteOutcome::Transport`] and deletes nothing.
    ///
    /// A test that genuinely needs to drive a real daemon sets
    /// `TRUSTY_ALLOW_PRODUCTION_STATE=1` (`trusty_common::test_harness::ALLOW_PRODUCTION_ENV`),
    /// which makes that intent explicit and greppable instead of ambient.
    /// Test: `acquire_is_refused_under_a_test_harness`,
    /// `acquire_refuses_when_no_daemon_socket_is_bound`,
    /// `acquire_succeeds_when_production_state_is_explicitly_allowed`.
    pub(super) fn acquire() -> Option<Self> {
        // #4743: a `cargo test` process may not destroy index data. Checked
        // before resolution so a test run does not even look up where the
        // operator's daemon lives.
        if trusty_common::running_under_test_harness() {
            warn!(
                "refusing a destructive trusty-search index delete: this is a test process \
                 (#4743). Set {} to override.",
                trusty_common::test_harness::ALLOW_PRODUCTION_ENV
            );
            return None;
        }
        let socket = match search_rpc::search_socket() {
            Ok(socket) => socket,
            Err(e) => {
                warn!("cannot resolve the trusty-search socket; skipping index removal: {e:#}");
                return None;
            }
        };
        // #6285: nothing bound means no daemon to ask — the no-op the old
        // `resolve_daemon_base_url` miss produced.
        if !socket.exists() {
            debug!(
                socket = %socket.display(),
                "no trusty-search daemon socket; skipping index removal (#2033)"
            );
            return None;
        }
        Some(Self { socket })
    }

    /// The destructive params for `index_id` — the crate's only `delete_data`
    /// opt-in.
    ///
    /// Why (#4123): trusty-search's `search.index.delete` preserves on-disk data
    /// unless `delete_data` is `true`. Both callers opt in deliberately: the
    /// workspace each index describes is a disposable worktree that is being (or
    /// has been) deleted, so preserved index data would be unreachable garbage
    /// on disk forever.
    /// What: `{"index_id": …, "delete_data": true}`. A method rather than a free
    /// function so it cannot be called without a capability in hand.
    /// Test: `delete_params_opt_into_data_deletion`.
    fn delete_params(&self, index_id: &str) -> Value {
        serde_json::json!({ "index_id": index_id, "delete_data": true })
    }

    /// Issue the delete and classify the result.
    ///
    /// Why: never returns an error — every failure mode maps to a
    /// [`DeleteOutcome`] variant so both callers stay fail-soft (an unreachable
    /// or erroring search daemon must not block session teardown).
    /// What: one [`REQUEST_TIMEOUT`]-bounded `search.index.delete`. A refusal
    /// carries the daemon's own code, an unanswered call is
    /// [`DeleteOutcome::Transport`], and a result is believed only as far as its
    /// own `removed` field — see [`classify_delete_result`]. No arm but a
    /// daemon-confirmed removal produces [`DeleteOutcome::Removed`].
    /// Test: `delete_over_a_stale_socket_is_a_transport_failure`,
    /// `a_refusal_is_never_reported_as_removed`,
    /// `a_result_that_did_not_remove_is_not_reported_as_removed`; end-to-end by
    /// `decommission_issues_no_request_to_a_live_daemon_under_test`, which
    /// asserts this never runs under a test harness.
    pub(super) async fn delete(&self, index_id: &str) -> DeleteOutcome {
        let params = self.delete_params(index_id);
        match search_rpc::call_at(
            &self.socket,
            search_rpc::METHOD_INDEX_DELETE,
            params,
            REQUEST_TIMEOUT,
        )
        .await
        {
            Ok(body) => classify_delete_result(&body),
            Err(e) => match e.downcast_ref::<SearchRpcError>() {
                Some(rpc) => DeleteOutcome::Refused {
                    code: rpc.code,
                    message: rpc.message.clone(),
                },
                None => DeleteOutcome::Transport(format!("{e:#}")),
            },
        }
    }
}

/// Read a delete result as the daemon's own verdict, not as "it answered".
///
/// Why: `search.index.delete` answers a RESULT — not an error — for a delete it
/// abandoned because an in-flight writer never quiesced (#3049). Over HTTP that
/// was a `200`, and treating any 2xx as success recorded an index as reclaimed
/// while it was still registered and still on disk. The daemon states the
/// outcome in the body; this reads it.
/// What: [`DeleteOutcome::Removed`] only when the body's `removed` is `true`. A
/// body that says otherwise — or that omits the field, which is unverifiable
/// rather than affirmative — is [`DeleteOutcome::NotRemoved`] carrying the
/// daemon's `quiesced` flag, the field that explains the abandonment.
/// Test: `a_result_that_did_not_remove_is_not_reported_as_removed`,
/// `a_result_reporting_removal_is_removed`.
fn classify_delete_result(body: &Value) -> DeleteOutcome {
    if body.get("removed").and_then(Value::as_bool) == Some(true) {
        return DeleteOutcome::Removed;
    }
    let quiesced = match body.get("quiesced").and_then(Value::as_bool) {
        Some(v) => v.to_string(),
        None => "unreported".to_string(),
    };
    DeleteOutcome::NotRemoved(format!(
        "the daemon did not remove the index (quiesced: {quiesced})"
    ))
}

#[cfg(test)]
#[path = "index_delete_guard_tests.rs"]
mod tests;

//! The one door a destructive trusty-search index DELETE may pass through (#4743).
//!
//! Why: `search_gc` had two independent sites that formatted
//! `DELETE /indexes/{id}?delete_data=true` — the decommission-time removal and
//! the orphan sweep's delete loop. `?delete_data=true` destroys the index's
//! on-disk data directory (trusty-search's own
//! `delete_index_with_delete_data_true_destroys_data` asserts exactly that;
//! its orphan reaper deliberately passes `delete_data=false`). The daemon those
//! requests reach is whichever one `resolve_daemon_base_url` discovers, and
//! under `cargo test` that is the OPERATOR'S live daemon: the address comes from
//! `~/Library/Application Support/trusty-search/http_addr`, which a test process
//! reads exactly like production does. Fixture workspaces derive their index id
//! from a bare `file_name()` — `full`, `sess`, `live` — so a real index sharing
//! that basename is destroyed by a test run, silently.
//!
//! Why a capability type rather than an `if` at each site: two review rounds on
//! PR #4725 each found a different destructive effect that a caller had failed
//! to gate, and the response there was the same one taken here — stop relying on
//! every call site remembering, and make the ungated shape unrepresentable. A
//! caller cannot build the URL, because [`DestructiveIndexDelete`] holds the
//! only copy of the `?delete_data=true` literal and exposes no constructor that
//! takes a base URL. The only way to obtain one is [`DestructiveIndexDelete::acquire`],
//! which decides for itself whether the process may destroy data. A new
//! destructive site added later inherits the refusal by construction: it has to
//! call `acquire` to get anything it can delete with.
//!
//! Why the refusal is a RUNTIME check: the delete lands in a DIFFERENT PROCESS.
//! Issue #4094's `cfg(test)` arm in trusty-search's `default_data_dir` isolates
//! that daemon's own data-dir resolution, and #4255/PR #4864 extended isolation
//! to registry writes — but neither can help here. No compile-time guard in
//! trusty-search governs what a trusty-mpm test binary puts on the wire, and no
//! data-dir seam moves a request that is already addressed to port 7878.
//! `trusty_common::running_under_test_harness` is the process-level answer, and
//! reusing it keeps this crate from growing a second, drifting copy of the same
//! detection.
//!
//! Test: `acquire_is_refused_under_a_test_harness`,
//! `acquire_succeeds_when_production_state_is_explicitly_allowed`,
//! `delete_url_opts_into_data_deletion`; end-to-end,
//! `decommission_issues_no_request_to_a_live_daemon_under_test` in
//! `search_gc_guard_tests`.

use std::time::Duration;

use tracing::{debug, warn};

/// Per-request timeout for the index DELETE.
///
/// Why: the destructive call runs off the interactive request path
/// (decommission, periodic GC) but must still never hang the daemon
/// indefinitely if trusty-search is wedged.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Connect timeout, tighter than the overall request timeout so an unreachable
/// host fails fast.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(750);

/// What a destructive index DELETE did, for the caller to log in its own voice.
///
/// Why: the two call sites want different log messages for the same three
/// outcomes, and threading `reqwest` types back to them would leak the
/// transport this module exists to own.
#[derive(Debug)]
pub(super) enum DeleteOutcome {
    /// The daemon reported the index removed.
    Removed,
    /// The daemon answered, but not with a 2xx.
    Rejected(reqwest::StatusCode),
    /// The request never got an answer.
    Transport(String),
}

/// The capability to destroy a trusty-search index's on-disk data (#4743).
///
/// Why: see the module doc — holding one of these is the proof that this
/// process is allowed to delete real data, and it is the only thing in the
/// crate that can produce a `?delete_data=true` request.
/// What: an acquired daemon base URL plus the short-timeout client to reach it.
/// Both fields are private and there is no constructor other than
/// [`Self::acquire`], so the capability cannot be forged from a base URL a
/// caller happens to have.
/// Test: see the module doc.
pub(super) struct DestructiveIndexDelete {
    base: String,
    client: reqwest::Client,
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
    /// this is a `cargo test` process, (b) no trusty-search daemon is
    /// discoverable, or (c) the HTTP client cannot be built. `Some` otherwise.
    /// The test refusal is checked FIRST so a test run never even resolves the
    /// operator's daemon address.
    ///
    /// A test that genuinely needs to drive a real daemon sets
    /// `TRUSTY_ALLOW_PRODUCTION_STATE=1` (`trusty_common::test_harness::ALLOW_PRODUCTION_ENV`),
    /// which makes that intent explicit and greppable instead of ambient.
    /// Test: `acquire_is_refused_under_a_test_harness`,
    /// `acquire_succeeds_when_production_state_is_explicitly_allowed`.
    pub(super) fn acquire() -> Option<Self> {
        // #4743: a `cargo test` process may not destroy index data. Checked
        // before discovery so a test run does not even look up where the
        // operator's daemon lives.
        if trusty_common::running_under_test_harness() {
            warn!(
                "refusing a destructive trusty-search index DELETE: this is a test process \
                 (#4743). Set {} to override.",
                trusty_common::test_harness::ALLOW_PRODUCTION_ENV
            );
            return None;
        }
        let Some(base) = trusty_common::resolve_daemon_base_url("trusty-search") else {
            debug!("trusty-search daemon not discoverable; skipping index removal (#2033)");
            return None;
        };
        match reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
        {
            Ok(client) => Some(Self { base, client }),
            Err(e) => {
                warn!("trusty-search index-delete client build failed: {e}");
                None
            }
        }
    }

    /// The destructive URL for `index_id` — the crate's only `?delete_data=true`.
    ///
    /// Why (#4123): trusty-search's `DELETE /indexes/:id` preserves on-disk data
    /// unless `delete_data=true` is passed. Both callers opt in deliberately:
    /// the workspace each index describes is a disposable worktree that is
    /// being (or has been) deleted, so preserved index data would be
    /// unreachable garbage on disk forever.
    /// What: `{base}/indexes/{index_id}?delete_data=true`. Private, and
    /// reachable only through an acquired capability.
    /// Test: `delete_url_opts_into_data_deletion`.
    fn delete_url(&self, index_id: &str) -> String {
        format!("{}/indexes/{index_id}?delete_data=true", self.base)
    }

    /// Issue the DELETE and classify the result.
    ///
    /// What: never returns an error — every failure mode maps to a
    /// [`DeleteOutcome`] variant so both callers stay fail-soft (an unreachable
    /// or erroring search daemon must not block session teardown).
    /// Test: covered end-to-end by
    /// `decommission_issues_no_request_to_a_live_daemon_under_test`, which
    /// asserts this never runs under a test harness.
    pub(super) async fn delete(&self, index_id: &str) -> DeleteOutcome {
        match self.client.delete(self.delete_url(index_id)).send().await {
            Ok(resp) if resp.status().is_success() => DeleteOutcome::Removed,
            Ok(resp) => DeleteOutcome::Rejected(resp.status()),
            Err(e) => DeleteOutcome::Transport(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The load-bearing assertion, made from inside the very thing it detects:
    /// this test IS a `cargo test` process, so `acquire` must refuse it.
    ///
    /// Why no injection: a decision table with fabricated inputs would prove
    /// the precedence rules (`trusty-common` already tests those) but not the
    /// only fact that matters here — that a real trusty-mpm test binary is
    /// classified as one. Reverting the `running_under_test_harness` branch in
    /// `acquire` fails this on any machine with a running trusty-search daemon.
    /// Test: this function IS the test.
    #[test]
    fn acquire_is_refused_under_a_test_harness() {
        assert!(
            DestructiveIndexDelete::acquire().is_none(),
            "a cargo test process must never acquire the destructive-delete capability (#4743)"
        );
    }

    /// The escape hatch works, and the guard is not over-applied into a
    /// permanent disablement of the orphan GC.
    ///
    /// Why: a refusal that could never be lifted would be indistinguishable
    /// from having deleted the feature, and nothing would notice if `acquire`
    /// started returning `None` unconditionally.
    /// What: with `TRUSTY_ALLOW_PRODUCTION_STATE=1` and a discoverable daemon
    /// address written into an ISOLATED data dir, `acquire` yields a capability
    /// pointed at that isolated address. Deliberately never calls `delete` — the
    /// address belongs to nothing, and issuing a request is not what is under
    /// test.
    /// Test: this function IS the test.
    #[serial_test::serial]
    #[test]
    fn acquire_succeeds_when_production_state_is_explicitly_allowed() {
        let dir = crate::test_support::hermetic_temp_dir();
        std::fs::create_dir_all(dir.path().join("trusty-search")).unwrap();
        std::fs::write(
            dir.path().join("trusty-search").join("http_addr"),
            "127.0.0.1:59999",
        )
        .unwrap();

        // SAFETY: `#[serial]` — no other test thread races this set/restore.
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, dir.path());
            std::env::set_var(trusty_common::test_harness::ALLOW_PRODUCTION_ENV, "1");
        }
        let acquired = DestructiveIndexDelete::acquire();
        let url = acquired.as_ref().map(|d| d.delete_url("some-index"));
        unsafe {
            std::env::remove_var(trusty_common::test_harness::ALLOW_PRODUCTION_ENV);
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }

        assert_eq!(
            url.as_deref(),
            Some("http://127.0.0.1:59999/indexes/some-index?delete_data=true"),
            "the explicit production opt-in must still yield a working capability"
        );
    }

    /// The opt-in that makes the call destructive must survive refactoring —
    /// a URL that lost `?delete_data=true` would silently leak index data
    /// forever (#4123), the failure this whole module is downstream of.
    /// Test: this function IS the test.
    #[serial_test::serial]
    #[test]
    fn delete_url_opts_into_data_deletion() {
        let cap = DestructiveIndexDelete {
            base: "http://127.0.0.1:7878".into(),
            client: reqwest::Client::new(),
        };
        assert_eq!(
            cap.delete_url("my-index"),
            "http://127.0.0.1:7878/indexes/my-index?delete_data=true"
        );
    }
}

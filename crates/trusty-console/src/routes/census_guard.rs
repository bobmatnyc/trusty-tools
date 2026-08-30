//! The delete-time census re-check the batch prune applies to every id (#6380).
//!
//! Why: the prune deletes ids an operator confirmed from a census they were
//! shown. The confirm step is human-paced — a roster is fetched, read, ticked
//! and submitted — so minutes can pass between the daemon classifying a root as
//! gone and the console asking it to delete that registration. An index id is
//! derived deterministically from its `root_path`, so a path wiped and then
//! recreated inside that window yields the SAME id for a live index, and the
//! prune swept it. The census the operator saw is a fact with an expiry, and
//! nothing was re-reading it.
//!
//! What: [`OrphanGuard`] re-fetches `search.registry.orphans` from the daemon
//! immediately before EACH delete, and hands back the `root_path` the daemon
//! reports for that id right now. An id the CURRENT census no longer lists
//! under `orphans` is refused — that covers the recreated root (the daemon now
//! classifies it `Present`), a root that became unjudgeable, and a registration
//! that vanished on its own.
//!
//! Per id rather than once per batch, because a batch runs under a 120s budget
//! and one census at the top would leave the last delete acting on a
//! two-minute-old fact — the same window this module exists to close, only
//! smaller. A census is one registry read and one `stat` per row, and
//! `MAX_PRUNE_BATCH` bounds how many of them a request can ask for.
//!
//! **Every failure refuses.** An unreachable daemon, a JSON-RPC error, a census
//! that does not parse: none of them says the registration is still stale, so
//! none of them may let a delete through. A drop partway through a batch fails
//! every remaining id and the operator retries the batch.
//!
//! Test: the `guard_*` tests below.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::routes::ACTION_TIMEOUT;
use crate::routes::SEARCH_SERVICE_ID;

/// Re-reads the daemon's stale-registration census before each delete.
///
/// Why a type rather than a free function: the socket path is the same for
/// every id in a batch, and the caller must not be able to skip the re-check for
/// one id by forgetting to pass it. Holding the socket here makes
/// [`OrphanGuard::expected_root`] the only way to obtain the argument
/// `delete_index_on_socket` now needs.
/// Test: see the module docs.
pub(crate) struct OrphanGuard {
    socket: PathBuf,
}

impl OrphanGuard {
    /// A guard that re-censuses `socket` before every check.
    pub(crate) fn new(socket: &Path) -> Self {
        Self {
            socket: socket.to_path_buf(),
        }
    }

    /// The root path the daemon reports for `id` right now, or why the delete
    /// must not proceed.
    ///
    /// Why the root path comes back rather than a bare yes: it is the
    /// expectation the delete then pins itself to, so the residual window
    /// between this check and the delete is closed by the daemon rather than
    /// merely narrowed here. `search.index.delete` refuses when the
    /// registration's root is no longer this one.
    /// What: one `search.registry.orphans` exchange, then a lookup of `id` in
    /// the `orphans` list — never in `indeterminate`, which is the set of roots
    /// the daemon declined to judge.
    ///
    /// # Errors
    ///
    /// A string naming what stopped the check, for the row the batch reports.
    /// Every arm — unreachable, refused, malformed, id absent — is an error.
    ///
    /// Test: `guard_refuses_an_id_the_current_census_no_longer_calls_stale`,
    /// `guard_refuses_every_id_once_the_daemon_stops_answering`,
    /// `guard_returns_the_root_path_the_daemon_reports_now`.
    pub(crate) async fn expected_root(&self, id: &str) -> Result<String, String> {
        let census = crate::search_uds::call(
            &self.socket,
            crate::search_uds::METHOD_REGISTRY_ORPHANS,
            serde_json::json!({}),
            ACTION_TIMEOUT,
        )
        .await
        .map_err(|e| {
            format!(
                "not deleted: {SEARCH_SERVICE_ID} could not re-check whether '{id}' is \
                 still stale ({})",
                e.message()
            )
        })?;

        match orphan_root(&census, id) {
            Some(root) => Ok(root),
            None => Err(format!(
                "not deleted: {SEARCH_SERVICE_ID} no longer lists '{id}' as a stale \
                 registration, so the census it was confirmed from is out of date"
            )),
        }
    }
}

/// The `root_path` `id` carries in this census's `orphans` list, if it is there.
///
/// Reads `orphans` alone. A row in `indeterminate` is one the daemon declined to
/// judge, and treating "could not check" as "still stale" is the fail-open shape
/// the census's two-list split exists to prevent.
/// Test: `guard_reads_the_orphans_list_and_not_the_indeterminate_one`.
fn orphan_root(census: &Value, id: &str) -> Option<String> {
    census
        .get("orphans")?
        .as_array()?
        .iter()
        .find(|row| row.get("id").and_then(Value::as_str) == Some(id))?
        .get("root_path")?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A census listing one gone root and one the daemon declined to judge.
    fn census() -> Value {
        json!({
            "orphans": [{ "id": "wiped", "root_path": "/tmp/wiped", "colocated": false }],
            "indeterminate": [{ "id": "unplugged", "root_path": "/Volumes/x", "reason": "…" }],
            "live_count": 1,
            "total": 3,
        })
    }

    /// Why: the guard hands the delete an expectation, so it has to be the root
    /// the daemon reports NOW rather than anything the caller supplied.
    /// Test: this is the test.
    #[test]
    fn guard_returns_the_root_path_the_daemon_reports_now() {
        assert_eq!(
            orphan_root(&census(), "wiped").as_deref(),
            Some("/tmp/wiped")
        );
    }

    /// Why (#6380): the census splits gone roots from roots it could not judge
    /// precisely so a caller cannot delete the second set. Reading
    /// `indeterminate` as "still stale" would delete an unmounted volume's whole
    /// index roster.
    /// Test: this is the test.
    #[test]
    fn guard_reads_the_orphans_list_and_not_the_indeterminate_one() {
        assert_eq!(orphan_root(&census(), "unplugged"), None);
    }

    /// Why (#6380): the recreated-root case. The operator confirmed `wiped`
    /// while its root was gone; by delete time the daemon has reclassified it,
    /// so the id is absent from `orphans` and the delete must not happen.
    /// Test: this is the test.
    #[test]
    fn guard_refuses_an_id_the_current_census_no_longer_calls_stale() {
        let recreated = json!({ "orphans": [], "indeterminate": [], "live_count": 1, "total": 1 });
        assert_eq!(orphan_root(&recreated, "wiped"), None);
    }

    /// Why: a census body that is not the shape this reader expects says nothing
    /// about staleness, so it must not read as a pass.
    /// Test: this is the test.
    #[test]
    fn guard_refuses_a_census_it_cannot_parse() {
        for malformed in [json!({}), json!({ "orphans": "nope" }), Value::Null] {
            assert_eq!(orphan_root(&malformed, "wiped"), None, "{malformed}");
        }
    }

    /// Why (#6380 closure condition 2): a daemon that stops answering partway
    /// through a batch must fail the remaining ids, not let them through
    /// unchecked. A socket nothing is bound to is that state.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn guard_refuses_every_id_once_the_daemon_stops_answering() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let guard = OrphanGuard::new(&tmp.path().join("absent.sock"));
        let refusal = guard
            .expected_root("wiped")
            .await
            .expect_err("a dead socket must refuse the delete");
        assert!(
            refusal.contains("not deleted") && refusal.contains("re-check"),
            "the row must say the delete did not happen and why: {refusal}"
        );
    }
}

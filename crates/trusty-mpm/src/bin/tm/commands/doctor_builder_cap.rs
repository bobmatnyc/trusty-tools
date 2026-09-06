//! The `builder_cap` row of `tm doctor` (#6892).
//!
//! Why: the builder cap denies dispatches from a count no operator can see. A
//! deny names the holders at the moment it fires, and an operator asking "what
//! is holding my machine" has no dispatch to hang that on — so the census gets
//! its own row. It also surfaces the one lease state that means something went
//! wrong: a lease past `BUILDER_LEASE_TTL_SECS` that no other signal ended.
//!
//! Why it lives in the `tm` binary rather than in the daemon's own
//! `run_doctor`: the same reason `doctor_stale` and `doctor_orphan` do — it
//! reasons about a daemon's ANSWER, so it belongs on the client side of the one
//! `/health` probe `doctor_local` already takes, and it is skipped entirely when
//! no daemon answered.
//! Test: the `#[cfg(test)]` suite below.

use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};

/// The row's stable name.
const CHECK: &str = "builder_cap";

/// One row of the census as the daemon reports it.
///
/// Why: the daemon's own types live behind the library, and the row only needs
/// three fields of them. Reading the JSON here rather than importing the struct
/// keeps a daemon one version ahead or behind from breaking the row.
/// What: agent, session, and elapsed seconds. A row missing an agent name is
/// skipped by [`holders_in`].
/// Test: `census_rows_are_read_out_of_the_daemons_answer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CensusRow {
    /// The holding agent's name.
    pub(crate) agent: String,
    /// How long it has been running, in seconds.
    pub(crate) elapsed_secs: i64,
}

/// The `builder_cap` row for one census answer.
///
/// Why: kept pure — it takes the daemon's answer rather than fetching it — so
/// every verdict is assertable with no daemon and no machine of a particular
/// size.
/// What: `Ok` reporting `N/cap` held with the holders named; `Warn` when any
/// lease is past the TTL and has not been reaped, because reaching that state
/// means neither a terminal status nor a PID check could end the lease and the
/// slot was held on a backstop; `Unknown` when the census could not be read at
/// all, which is never `Ok` (#4005) — a cap whose count is unavailable has not
/// passed anything.
/// Test: `an_idle_machine_is_ok`, `holders_are_named_in_the_ok_row`,
/// `an_expired_lease_warns`, `an_unreadable_census_is_unknown`,
/// `a_full_machine_is_still_ok`.
pub(crate) fn builder_cap_check(census: Option<(&[CensusRow], &[CensusRow], u32)>) -> DoctorCheck {
    let Some((holders, expired, cap)) = census else {
        return DoctorCheck::new(
            CHECK,
            CheckStatus::Unknown,
            "could not read the machine's builder-slot census from the daemon — the cap is \
             still enforced (a dispatch that cannot be counted is denied, #6892), but this \
             report cannot say how many builders are running. `tm restart` clears an unhealthy \
             daemon.",
        );
    };
    let held = format!("{}/{} builder slots held", holders.len(), cap);
    if !expired.is_empty() {
        return DoctorCheck::new(
            CHECK,
            CheckStatus::Warn,
            format!(
                "{held}; {} lease(s) past the 45-minute TTL and not yet reaped: {}. Reaching \
                 the TTL means neither a terminal status nor a dispatching-process check could \
                 end the lease, so the slot was held on the backstop alone. The slot is already \
                 free — this reports that the agent never came back, not that anything is \
                 blocked. {}",
                expired.len(),
                render(expired),
                configured(cap),
            ),
        );
    }
    if holders.is_empty() {
        return DoctorCheck::new(
            CHECK,
            CheckStatus::Ok,
            format!("{held}; no builders running. {}", configured(cap)),
        );
    }
    DoctorCheck::new(
        CHECK,
        CheckStatus::Ok,
        format!("{held}: {}. {}", render(holders), configured(cap)),
    )
}

/// Where the cap came from, appended to every row.
///
/// Why: the number is the first thing an operator asks about, and the file that
/// sets it is the second. `.trusty-mpm.toml` is named as a non-answer because
/// putting the key there is the obvious wrong guess.
fn configured(cap: u32) -> String {
    format!(
        "The cap is `builders.max_concurrent` in `~/.trusty-mpm/config.toml` (currently {cap}, \
         defaulting to this host's memory tier); a project's `.trusty-mpm.toml` cannot set it."
    )
}

/// `agent (running 12m)`, comma-separated.
fn render(rows: &[CensusRow]) -> String {
    rows.iter()
        .map(|r| format!("{} (running {}m)", r.agent, r.elapsed_secs / 60))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Read one census list out of the daemon's answer.
///
/// Why: shared by the `holders` and `expired` lists, which have identical shape.
/// What: rows carrying an `agent`; `elapsed_secs` defaults to `0`.
/// Test: `census_rows_are_read_out_of_the_daemons_answer`.
fn holders_in(body: &serde_json::Value, key: &str) -> Vec<CensusRow> {
    body.get(key)
        .and_then(serde_json::Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(CensusRow {
                        agent: row
                            .get("agent")
                            .and_then(serde_json::Value::as_str)?
                            .to_string(),
                        elapsed_secs: row
                            .get("elapsed_secs")
                            .and_then(serde_json::Value::as_i64)
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Fetch the census and render the row.
///
/// Why: the network half, split from the pure verdict above for the same reason
/// `doctor_stale` splits — the comparison is what is worth pinning, and the
/// fetch is not testable without a live daemon.
/// What: `GET <url>/api/v1/builder-slots` under the same tight bounds every
/// other `tm doctor` probe uses; any failure renders the `Unknown` row. Its
/// only caller is `doctor_local::daemon_rows`, which reaches this line only when
/// a daemon has already answered `/health`.
/// Test: `an_unreadable_census_is_unknown` covers the failure verdict; the
/// live fetch is exercised by the executor's live-daemon doctor test.
pub(crate) async fn builder_cap_row(url: &str) -> DoctorCheck {
    let Some(body) = fetch_census(url).await else {
        return builder_cap_check(None);
    };
    let holders = holders_in(&body, "holders");
    let expired = holders_in(&body, "expired");
    let cap = body
        .get("cap")
        .and_then(serde_json::Value::as_u64)
        .and_then(|c| u32::try_from(c).ok())
        .unwrap_or_default();
    builder_cap_check(Some((&holders, &expired, cap)))
}

/// One bounded GET of the census, or `None` for any failure.
async fn fetch_census(url: &str) -> Option<serde_json::Value> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(500))
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()?;
    client
        .get(format!("{url}/api/v1/builder-slots"))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json::<serde_json::Value>()
        .await
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(agent: &str, elapsed_secs: i64) -> CensusRow {
        CensusRow {
            agent: agent.to_string(),
            elapsed_secs,
        }
    }

    #[test]
    fn an_idle_machine_is_ok() {
        let check = builder_cap_check(Some((&[], &[], 3)));
        assert_eq!(check.status, CheckStatus::Ok);
        assert_eq!(check.name, "builder_cap");
        assert!(check.message.contains("0/3"), "{}", check.message);
        assert!(
            check.message.contains("builders.max_concurrent"),
            "{}",
            check.message
        );
    }

    #[test]
    fn holders_are_named_in_the_ok_row() {
        let holders = [row("rust-engineer", 754), row("local-ops", 61)];
        let check = builder_cap_check(Some((&holders, &[], 3)));
        assert_eq!(check.status, CheckStatus::Ok);
        assert!(check.message.contains("2/3"), "{}", check.message);
        assert!(
            check.message.contains("rust-engineer (running 12m)"),
            "{}",
            check.message
        );
        assert!(
            check.message.contains("local-ops (running 1m)"),
            "{}",
            check.message
        );
    }

    /// A machine AT its cap is healthy, not a warning — that is the cap doing
    /// its job. Only an un-reaped lease is a signal about the harness.
    #[test]
    fn a_full_machine_is_still_ok() {
        let holders = [row("rust-engineer", 60), row("python-engineer", 60)];
        let check = builder_cap_check(Some((&holders, &[], 2)));
        assert_eq!(check.status, CheckStatus::Ok);
        assert!(check.message.contains("2/2"), "{}", check.message);
    }

    #[test]
    fn an_expired_lease_warns() {
        let expired = [row("rust-engineer", 3600)];
        let check = builder_cap_check(Some((&[], &expired, 2)));
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(
            check.message.contains("past the 45-minute TTL"),
            "{}",
            check.message
        );
        assert!(
            check.message.contains("rust-engineer (running 60m)"),
            "{}",
            check.message
        );
    }

    /// Never `Ok` (#4005): a count that could not be read has not passed.
    #[test]
    fn an_unreadable_census_is_unknown() {
        let check = builder_cap_check(None);
        assert_eq!(check.status, CheckStatus::Unknown);
        // And it must say the cap is still being enforced, or the reader
        // concludes builds are unguarded.
        assert!(
            check.message.contains("still enforced"),
            "{}",
            check.message
        );
    }

    #[test]
    fn census_rows_are_read_out_of_the_daemons_answer() {
        let body = serde_json::json!({
            "cap": 2,
            "holders": [
                {"agent": "rust-engineer", "session": "s", "elapsed_secs": 90},
                {"session": "s", "elapsed_secs": 10},
            ],
            "expired": [],
        });
        let rows = holders_in(&body, "holders");
        assert_eq!(rows, vec![row("rust-engineer", 90)]);
        assert!(holders_in(&body, "expired").is_empty());
        assert!(holders_in(&body, "absent").is_empty());
    }

    #[tokio::test]
    async fn the_row_is_unknown_when_no_daemon_answers() {
        let check = builder_cap_row("http://127.0.0.1:1").await;
        assert_eq!(check.status, CheckStatus::Unknown);
    }
}

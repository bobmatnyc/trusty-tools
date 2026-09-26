//! A PR's `statusCheckRollup`: the entry shape and latest-run selection
//! (#8638).
//!
//! Why: a workflow that runs twice on one head SHA — a concurrency-cancelled
//! run, then a fresh one — leaves two rollup entries under one name, oldest
//! first. Reading the first match lets the stale run decide: a false BLOCKED
//! on PR #8637, and a fail-open when a stale SUCCESS shadows a later FAILURE.
//! What: [`RollupEntry`] is the one entry type every `tm` rollup reader
//! parses. [`latest_named`] and [`latest_per_name`] pick the run that decides
//! each check name: any run without a result wins, so a rerun in flight is
//! pending; otherwise the latest by `completedAt`, else `startedAt`.
//! Test: `rollup_latest_prefers_later_completion`,
//! `rollup_unfinished_run_wins_whatever_the_timestamps`,
//! `rollup_untimed_entry_sorts_oldest`, `rollup_latest_per_name_keeps_unnamed`,
//! plus `queue_duplicate_*` and `check_condition_duplicate_*`.

use chrono::{DateTime, Datelike as _, Utc};
use serde::Deserialize;

/// One entry of `statusCheckRollup`.
///
/// Why: the rollup mixes two GraphQL types. A `CheckRun` carries `name` +
/// `status` + `conclusion`; a `StatusContext` carries `context` + `state`.
/// Deserializing both permissively into one struct keeps every reader on one
/// shape. `bucket` is NOT deserialized: GitHub reports a bucketed-complete
/// value before a check has settled, and not reading it is a stronger
/// guarantee than reading it and remembering not to trust it.
/// What: every field optional; the methods normalize across the two shapes.
/// Test: `queue_accepts_status_context`, `check_condition_ignores_bucket`.
#[derive(Debug, Deserialize)]
pub(crate) struct RollupEntry {
    /// `CheckRun` or `StatusContext`.
    #[serde(default, rename = "__typename")]
    typename: Option<String>,
    /// `CheckRun` display name.
    #[serde(default)]
    name: Option<String>,
    /// `StatusContext` display name.
    #[serde(default)]
    context: Option<String>,
    /// `CheckRun` lifecycle: `QUEUED` / `IN_PROGRESS` / `COMPLETED`.
    #[serde(default)]
    status: Option<String>,
    /// `CheckRun` result, present only once it has completed.
    #[serde(default)]
    conclusion: Option<String>,
    /// `StatusContext` result: `PENDING` / `EXPECTED` / `SUCCESS` / `FAILURE` / `ERROR`.
    #[serde(default)]
    state: Option<String>,
    /// #8638: recency key that picks the latest of duplicate runs.
    #[serde(default, rename = "completedAt")]
    completed_at: Option<String>,
    /// #8638: fallback recency key.
    #[serde(default, rename = "startedAt")]
    started_at: Option<String>,
}

/// Is `s` one of the terminal `StatusContext` states?
fn terminal_state(s: &str) -> bool {
    ["SUCCESS", "FAILURE", "ERROR"]
        .iter()
        .any(|t| s.eq_ignore_ascii_case(t))
}

impl RollupEntry {
    /// The check name this entry reports under, or `None` when unnamed.
    pub(crate) fn label(&self) -> Option<&str> {
        self.name
            .as_deref()
            .or(self.context.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// The name to show for this entry, never empty.
    pub(crate) fn display_label(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.context.clone())
            .or_else(|| self.typename.clone())
            .unwrap_or_else(|| "<unnamed check>".to_string())
    }

    /// Did this check pass?
    ///
    /// Why: `SUCCESS` only. `NEUTRAL` and `SKIPPED` are not success, and a
    /// required context that skipped has not proven anything.
    /// What: `conclusion == SUCCESS` (CheckRun) or `state == SUCCESS`
    /// (StatusContext).
    /// Test: `queue_required_context_not_success`.
    pub(crate) fn is_success(&self) -> bool {
        let v = self.conclusion.as_deref().or(self.state.as_deref());
        v.is_some_and(|s| s.eq_ignore_ascii_case("SUCCESS"))
    }

    /// Is this run still queued or running?
    ///
    /// Why (#8638): a run without a result supersedes every older result of
    /// the same check, so it must make the check pending.
    /// What: no non-empty `conclusion` and no terminal `state`, or a `status`
    /// that is present and not `COMPLETED`.
    /// Test: `rollup_unfinished_run_wins_whatever_the_timestamps`,
    /// `queue_duplicate_success_then_queued_is_pending`.
    pub(crate) fn is_unfinished(&self) -> bool {
        let concluded = self
            .conclusion
            .as_deref()
            .is_some_and(|c| !c.trim().is_empty())
            || self.state.as_deref().is_some_and(terminal_state);
        let open_status = self
            .status
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty() && !s.eq_ignore_ascii_case("COMPLETED"));
        !concluded || open_status
    }

    /// Whether this entry has genuinely finished.
    ///
    /// Why: fails CLOSED. An entry carrying neither a completed `status` nor a
    /// terminal `state` — an unrecognised shape, or a truncated response — is
    /// unsettled, so a wait keeps waiting rather than declaring a false DONE.
    /// What: `CheckRun` needs `COMPLETED` plus a non-empty `conclusion`;
    /// anything else needs a terminal `state`.
    /// Test: `check_condition_pending_until_all_settled`.
    pub(crate) fn settled(&self) -> bool {
        let completed = self
            .status
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case("COMPLETED"))
            && self
                .conclusion
                .as_deref()
                .is_some_and(|c| !c.trim().is_empty());
        completed || self.state.as_deref().is_some_and(terminal_state)
    }

    /// Whether this settled entry reports a failure.
    pub(crate) fn failed(&self) -> bool {
        let bad = |v: &str| ["FAILURE", "ERROR", "TIMED_OUT", "CANCELLED"].contains(&v);
        self.conclusion
            .as_deref()
            .is_some_and(|c| bad(&c.to_ascii_uppercase()))
            || self
                .state
                .as_deref()
                .is_some_and(|s| bad(&s.to_ascii_uppercase()))
    }
}

/// Parse one rollup timestamp.
///
/// Why: `gh` reports a still-running check's `completedAt` as Go's zero time
/// `0001-01-01T00:00:00Z`; read literally, it would sort as a real instant.
/// What: RFC 3339 to UTC; the zero time, an empty string and an unparsable
/// value are all `None`.
fn parse_stamp(raw: Option<&str>) -> Option<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(raw?.trim()).ok()?;
    let utc = parsed.with_timezone(&Utc);
    (utc.year() > 1).then_some(utc)
}

/// The recency key: `completedAt`, else `startedAt`, else `None` (oldest).
fn recency(entry: &RollupEntry) -> Option<DateTime<Utc>> {
    parse_stamp(entry.completed_at.as_deref()).or_else(|| parse_stamp(entry.started_at.as_deref()))
}

/// The run that decides check `name`, or `None` when no entry carries it.
///
/// Why: a required context is proven by its latest run, never by whichever
/// run `gh` happened to list first, and a rerun in flight must never let an
/// older SUCCESS through.
/// What: if ANY run of `name` is [`RollupEntry::is_unfinished`], returns an
/// unfinished run, whatever the timestamps. Otherwise returns the run with
/// the greatest [`recency`] key; `None` sorts below every timestamp, and a
/// tie goes to the later entry in rollup order (`max_by_key` returns the
/// last maximum, and `gh` lists entries oldest first).
/// Test: `rollup_latest_prefers_later_completion`,
/// `rollup_unfinished_run_wins_whatever_the_timestamps`,
/// `rollup_untimed_entry_sorts_oldest`.
pub(crate) fn latest_named<'a>(entries: &'a [RollupEntry], name: &str) -> Option<&'a RollupEntry> {
    let runs = || entries.iter().filter(move |e| e.label() == Some(name));
    // #8638: any run without a result makes the check pending.
    runs()
        .filter(|e| e.is_unfinished())
        .max_by_key(|e| recency(e))
        .or_else(|| runs().max_by_key(|e| recency(e)))
}

/// Every entry that decides its check name, in rollup order.
///
/// Why: a reader that judges the whole rollup (`tm wait --for check`) must
/// not count a superseded run as a live failure or a live pending check.
/// What: keeps one entry per name — the one [`latest_named`] picks — and
/// every unnamed entry untouched, since nothing proves two unnamed entries
/// are runs of the same check.
/// Test: `rollup_latest_per_name_keeps_unnamed`,
/// `check_condition_duplicate_run_uses_latest`.
pub(crate) fn latest_per_name(entries: &[RollupEntry]) -> Vec<&RollupEntry> {
    entries
        .iter()
        .filter(|e| {
            e.label().is_none_or(|n| {
                latest_named(entries, n).is_some_and(|chosen| std::ptr::eq(chosen, *e))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a JSON array of rollup entries.
    fn runs(json: &str) -> Vec<RollupEntry> {
        serde_json::from_str(json).expect("valid rollup JSON")
    }

    #[test]
    fn rollup_latest_prefers_later_completion() {
        let runs = runs(
            r#"[{"name":"ci","status":"COMPLETED","conclusion":"SUCCESS","completedAt":"2026-09-25T22:53:26Z"},
                {"name":"ci","status":"COMPLETED","conclusion":"CANCELLED","completedAt":"2026-09-25T22:51:15Z"}]"#,
        );
        assert!(latest_named(&runs, "ci").expect("named").is_success());
        assert!(latest_named(&runs, "other").is_none());
    }

    #[test]
    fn rollup_unfinished_run_wins_whatever_the_timestamps() {
        // A running run that started before the SUCCESS completed, and a
        // queued run with no timestamps at all: either one makes `ci` pending.
        for open in [
            r#"{"name":"ci","status":"IN_PROGRESS","conclusion":"","startedAt":"2026-09-25T22:51:00Z","completedAt":"0001-01-01T00:00:00Z"}"#,
            r#"{"name":"ci","status":"QUEUED","conclusion":""}"#,
        ] {
            let runs = runs(&format!(
                r#"[{{"name":"ci","status":"COMPLETED","conclusion":"SUCCESS",
                     "startedAt":"2026-09-25T22:50:00Z","completedAt":"2026-09-25T22:51:15Z"}},{open}]"#
            ));
            let got = latest_named(&runs, "ci").expect("named");
            assert!(got.is_unfinished(), "{open}");
        }
    }

    #[test]
    fn rollup_untimed_entry_sorts_oldest() {
        // Every run completed: the timed one beats the untimed one.
        let runs = runs(
            r#"[{"name":"ci","status":"COMPLETED","conclusion":"SUCCESS","completedAt":"2026-09-25T22:51:15Z"},
                {"name":"ci","status":"COMPLETED","conclusion":"CANCELLED"}]"#,
        );
        assert!(latest_named(&runs, "ci").expect("named").is_success());
    }

    #[test]
    fn rollup_latest_per_name_keeps_unnamed() {
        let runs = runs(
            r#"[{"name":"ci","conclusion":"CANCELLED","completedAt":"2026-09-25T22:51:15Z"},
                {"__typename":"Mystery"},
                {"name":"ci","conclusion":"SUCCESS","completedAt":"2026-09-25T22:53:26Z"},
                {"__typename":"Mystery"},
                {"context":"lint","state":"SUCCESS"}]"#,
        );
        let kept = latest_per_name(&runs);
        assert_eq!(kept.len(), 4, "one `ci`, one `lint`, both unnamed");
        for (k, i) in kept.iter().zip([1, 2, 3, 4]) {
            assert!(std::ptr::eq(*k, &runs[i]));
        }
    }
}

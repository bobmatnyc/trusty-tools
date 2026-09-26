//! A PR's `statusCheckRollup`: the entry shape and latest-run selection
//! (#8638).
//!
//! Why: a workflow that runs twice on one head SHA — a concurrency-cancelled
//! run, then a fresh one — leaves two rollup entries under one name, oldest
//! first. Reading the first match lets the stale run decide: a false BLOCKED
//! on PR #8637, and a fail-open when a stale SUCCESS shadows a later FAILURE.
//! What: [`RollupEntry`] is the one entry type every `tm` rollup reader
//! parses. [`deciding_runs`] and [`deciding_per_check`] pick the run that
//! decides each check name, separately per GraphQL type: any run without a
//! result wins, so a rerun in flight is pending; otherwise the latest by
//! `completedAt`, else `startedAt`.
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

    /// Which GraphQL type this entry is: a `CheckRun` and a `StatusContext`
    /// that share a name are two separate requirements (#8638).
    ///
    /// What: `__typename` when present, else inferred from the shape — a
    /// `name` means `CheckRun`, a `context` means `StatusContext`.
    fn kind(&self) -> &str {
        match self.typename.as_deref().map(str::trim) {
            Some(t) if !t.is_empty() => t,
            _ if self.name.is_some() => "CheckRun",
            _ => "StatusContext",
        }
    }

    /// Is this run still queued or running?
    ///
    /// Why (#8638): a run without a result supersedes every older result of
    /// the same check, so it must make the check pending.
    /// What: exactly `!settled()` — one rule for "finished" in every reader.
    /// Test: `rollup_unfinished_run_wins_whatever_the_timestamps`,
    /// `queue_duplicate_success_then_queued_is_pending`.
    pub(crate) fn is_unfinished(&self) -> bool {
        !self.settled()
    }

    /// `(<status>, started <startedAt>)` for a pending-reason message.
    ///
    /// Test: `queue_duplicate_success_then_queued_is_pending`.
    pub(crate) fn run_summary(&self) -> String {
        let status = self
            .status
            .as_deref()
            .or(self.state.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("no status");
        match parse_stamp(self.started_at.as_deref()) {
            Some(t) => format!("({status}, started {})", t.to_rfc3339()),
            None => format!("({status}, no startedAt)"),
        }
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

    /// Whether this entry settled on anything but a passing result.
    ///
    /// Why (#8638): a deny-list missed `STARTUP_FAILURE`, `ACTION_REQUIRED`
    /// and `STALE`; an allow-list of passing results cannot miss a new one.
    /// What: settled, and its `conclusion` (else `state`) is not `SUCCESS`,
    /// `NEUTRAL` or `SKIPPED`.
    /// Test: `check_condition_counts_every_non_passing_result`.
    pub(crate) fn failed(&self) -> bool {
        let verdict = self
            .conclusion
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .or(self.state.as_deref())
            .unwrap_or_default();
        self.settled()
            && !["SUCCESS", "NEUTRAL", "SKIPPED"]
                .iter()
                .any(|p| verdict.eq_ignore_ascii_case(p))
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

/// The runs that decide check `name`: one per GraphQL type, in rollup order.
/// Empty when no entry carries `name`.
///
/// Why: a required context is proven by its latest run, never by whichever
/// run `gh` happened to list first, and a rerun in flight must never let an
/// older SUCCESS through. A `CheckRun` and a `StatusContext` that share a
/// name are two requirements, and GitHub gates on both.
/// What: groups the runs of `name` by type. Within a group, if ANY run is
/// [`RollupEntry::is_unfinished`], an unfinished run decides, whatever the
/// timestamps. Otherwise the run with the greatest [`recency`] key decides;
/// `None` sorts below every timestamp, and a tie goes to the later entry in
/// rollup order (`max_by_key` returns the last maximum, and `gh` lists
/// entries oldest first). A caller must require EVERY returned run to pass.
/// Test: `rollup_latest_prefers_later_completion`,
/// `rollup_unfinished_run_wins_whatever_the_timestamps`,
/// `rollup_untimed_entry_sorts_oldest`,
/// `queue_check_and_status_same_name_both_required`.
pub(crate) fn deciding_runs<'a>(entries: &'a [RollupEntry], name: &str) -> Vec<&'a RollupEntry> {
    let mut kinds: Vec<&str> = Vec::new();
    for e in entries.iter().filter(|e| e.label() == Some(name)) {
        if !kinds.contains(&e.kind()) {
            kinds.push(e.kind());
        }
    }
    kinds
        .into_iter()
        .filter_map(|kind| {
            let runs = || {
                entries
                    .iter()
                    .filter(move |e| e.label() == Some(name) && e.kind() == kind)
            };
            // #8638: any run without a result makes the check pending.
            runs()
                .filter(|e| e.is_unfinished())
                .max_by_key(|e| recency(e))
                .or_else(|| runs().max_by_key(|e| recency(e)))
        })
        .collect()
}

/// Every entry that decides its check, in rollup order.
///
/// Why: a reader that judges the whole rollup (`tm wait --for check`) must
/// not count a superseded run as a live failure or a live pending check.
/// What: keeps the entries [`deciding_runs`] picks for each name — one per
/// name and type — and every unnamed entry untouched, since nothing proves
/// two unnamed entries are runs of the same check.
/// Test: `rollup_latest_per_name_keeps_unnamed`,
/// `check_condition_duplicate_run_uses_latest`,
/// `check_condition_check_and_status_same_name_both_counted`.
pub(crate) fn deciding_per_check(entries: &[RollupEntry]) -> Vec<&RollupEntry> {
    entries
        .iter()
        .filter(|e| {
            e.label().is_none_or(|n| {
                deciding_runs(entries, n)
                    .iter()
                    .any(|chosen| std::ptr::eq(*chosen, *e))
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

    /// The single run that decides `name` (one GraphQL type in play).
    fn pick<'a>(runs: &'a [RollupEntry], name: &str) -> &'a RollupEntry {
        let got = deciding_runs(runs, name);
        assert_eq!(got.len(), 1, "one type, one deciding run");
        got[0]
    }

    #[test]
    fn rollup_latest_prefers_later_completion() {
        let runs = runs(
            r#"[{"name":"ci","status":"COMPLETED","conclusion":"SUCCESS","completedAt":"2026-09-25T22:53:26Z"},
                {"name":"ci","status":"COMPLETED","conclusion":"CANCELLED","completedAt":"2026-09-25T22:51:15Z"}]"#,
        );
        assert!(pick(&runs, "ci").is_success());
        assert!(deciding_runs(&runs, "other").is_empty());
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
            let got = pick(&runs, "ci");
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
        assert!(pick(&runs, "ci").is_success());
    }

    #[test]
    fn rollup_latest_per_name_keeps_unnamed() {
        let runs = runs(
            r#"[{"name":"ci","status":"COMPLETED","conclusion":"CANCELLED","completedAt":"2026-09-25T22:51:15Z"},
                {"__typename":"Mystery"},
                {"name":"ci","status":"COMPLETED","conclusion":"SUCCESS","completedAt":"2026-09-25T22:53:26Z"},
                {"__typename":"Mystery"},
                {"context":"lint","state":"SUCCESS"}]"#,
        );
        let kept = deciding_per_check(&runs);
        assert_eq!(kept.len(), 4, "one `ci`, one `lint`, both unnamed");
        for (k, i) in kept.iter().zip([1, 2, 3, 4]) {
            assert!(std::ptr::eq(*k, &runs[i]));
        }
    }
}

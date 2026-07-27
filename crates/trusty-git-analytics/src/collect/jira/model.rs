//! JIRA changelog and comment domain types, plus the wire shapes they are
//! parsed from (issue #3966).
//!
//! Split out of `client.rs` so that file stays under the 500-SLOC production
//! cap: `client.rs` owns HTTP orchestration (pagination, retry, timezone
//! discovery), this module owns "what a JIRA payload means". Nothing here
//! performs I/O, so every parsing rule below is unit-testable on a fixture
//! string.
//!
//! Visibility is `pub(crate)` on the wire types rather than private because
//! they are constructed only by deserialization and consumed only by
//! `client.rs`; the public surface is [`ChangelogIssue`], [`JiraTransition`]
//! and [`JiraComment`].

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use tracing::warn;

/// One parsed status transition from a JIRA changelog history entry
/// (issue #3966).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JiraTransition {
    /// Status before the transition. `None` when this is the first
    /// changelog entry touching the `status` field on the ticket.
    pub from_status: Option<String>,
    /// Status after the transition.
    pub to_status: String,
    /// Display name of the author who made the transition, when JIRA
    /// reports one (some automation-triggered transitions omit an author).
    pub author: Option<String>,
    /// Timestamp of the transition.
    pub created: DateTime<Utc>,
}

/// The outcome of one changelog walk, including the two ways a walk can be
/// incomplete without being an error (PR #4067 review round 2).
///
/// Why these travel with the issues rather than being logged and forgotten:
/// both leave the warehouse quietly short of data while every other signal
/// says the run succeeded, which is the failure family this whole command was
/// written to eliminate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogWalk {
    /// Distinct issues read, in `updated ASC` order.
    pub issues: Vec<ChangelogIssue>,
    /// Start of the earliest minute the pager had to traverse by offset. See
    /// [`super::paging::KeysetPager::offset_paged_minute`] — a concurrent
    /// edit inside such a minute can drop one ticket permanently.
    pub offset_paged_minute: Option<DateTime<Utc>>,
    /// `true` when the walk stopped at `max_results` with more available, so
    /// the run covered only a prefix of the requested window.
    pub truncated: bool,
}

/// A JIRA issue plus its parsed status-transition changelog (issue #3966).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogIssue {
    /// Issue key, e.g. `PROJ-123`.
    pub key: String,
    /// JIRA project key. Falls back to the `key` prefix (everything before
    /// the last `-`) when the `project` field is absent from the response,
    /// which should not happen in practice but keeps this infallible.
    pub project_key: String,
    /// The issue's `updated` timestamp, when present. Drives the
    /// incremental-sync cursor (see `collect::jira::sync::next_cursor`).
    pub updated: Option<DateTime<Utc>>,
    /// `status`-field transitions extracted from the changelog, oldest
    /// first (JIRA returns histories in chronological order).
    pub transitions: Vec<JiraTransition>,
}

impl ChangelogIssue {
    pub(crate) fn from_api(api: ChangelogApiIssue) -> Self {
        let project_key = api
            .fields
            .project
            .map(|p| p.key)
            .unwrap_or_else(|| project_key_from_issue_key(&api.key));
        let updated = api.fields.updated.as_deref().and_then(parse_jira_datetime);
        let transitions = match api.changelog {
            Some(cl) => transitions_from_histories(&api.key, cl.histories),
            None => Vec::new(),
        };
        Self {
            key: api.key,
            project_key,
            updated,
            transitions,
        }
    }
}

/// Extract `status`-field transitions from a changelog history list,
/// oldest first.
///
/// Why shared: both the search-embedded changelog and the dedicated
/// `/issue/{key}/changelog` fallback ([`super::changelog`], issue #4084) run
/// through this one extractor, so they produce identical [`JiraTransition`]
/// values for the same history entry. That equivalence is what makes the
/// fallback a safe wholesale REPLACEMENT of the embedded transitions rather
/// than a merge — and a replacement cannot duplicate a transition, so it
/// cannot collide on `fact_ticket_transitions`'s
/// `(ticket_key, transitioned_at, to_status)` primary key.
///
/// What: non-`status` items (`assignee`, `priority`, …) are dropped, since
/// only status transitions are in scope for that fact table. A history entry
/// with an unparseable `created` timestamp is skipped with a warning rather
/// than failing the batch or fabricating a timestamp that would corrupt
/// downstream ordering. The final stable sort defends the documented
/// oldest-first ordering against either endpoint changing its emission
/// order; both currently return chronological order, so it is normally a
/// no-op.
///
/// Test: `changelog_search_response_parses_status_transition`,
/// `changelog_ignores_non_status_fields`,
/// `changelog_skips_unparseable_timestamp`.
pub(crate) fn transitions_from_histories(
    key: &str,
    histories: Vec<ApiChangelogHistory>,
) -> Vec<JiraTransition> {
    let mut transitions = Vec::new();
    for history in histories {
        let author = history.author.map(|a| a.display_name);
        let Some(created) = parse_jira_datetime(&history.created) else {
            warn!(
                key = %key,
                created = %history.created,
                "unparseable changelog history timestamp; skipping this history entry"
            );
            continue;
        };
        for item in history.items {
            if item.field != "status" {
                continue;
            }
            let Some(to_status) = item.to_string else {
                continue;
            };
            transitions.push(JiraTransition {
                from_status: item.from_string,
                to_status,
                author: author.clone(),
                created,
            });
        }
    }
    transitions.sort_by_key(|t| t.created);
    transitions
}

/// Decide whether an issue's search-embedded changelog is provably
/// truncated and therefore needs the dedicated-endpoint fallback
/// (issue #4084).
///
/// Why: JIRA reports the issue's true history-entry count in
/// `changelog.total` while embedding only the first page of entries — and
/// the entries it omits are the OLDEST. `total` greater than the embedded
/// count is that truncation, stated by the server itself.
///
/// A MISSING `total` proves nothing either way. Rather than quietly
/// assuming completeness (the habit that produced this bug) or paying a
/// per-issue round trip on every ticket, a non-empty embedded changelog with
/// no `total` is warned about by ticket key so the gap is at least visible
/// in the logs.
///
/// Test: `search_with_changelog_falls_back_when_embedded_is_truncated`,
/// `search_with_changelog_skips_fallback_when_embedded_is_complete`,
/// `missing_changelog_total_does_not_trigger_fallback`.
pub(crate) fn embedded_changelog_is_truncated(issue: &ChangelogApiIssue) -> bool {
    let Some(cl) = issue.changelog.as_ref() else {
        return false;
    };
    match cl.total {
        Some(total) => total > cl.histories.len() as u64,
        None => {
            if !cl.histories.is_empty() {
                warn!(
                    key = %issue.key,
                    embedded = cl.histories.len(),
                    "JIRA omitted changelog.total; embedded-changelog completeness \
                     cannot be verified for this ticket"
                );
            }
            false
        }
    }
}

/// Fallback project-key extraction from an issue key (`PROJ-123` -> `PROJ`).
///
/// Used only when a search response omits the `project` field, which is not
/// expected in normal operation but keeps [`ChangelogIssue::from_api`]
/// infallible rather than panicking on a malformed response.
pub(crate) fn project_key_from_issue_key(key: &str) -> String {
    key.rsplit_once('-')
        .map(|(prefix, _)| prefix.to_string())
        .unwrap_or_else(|| key.to_string())
}

/// Parse a JIRA-flavoured ISO8601 timestamp.
///
/// JIRA Cloud emits offsets without a colon (e.g.
/// `2026-01-01T00:00:00.000+0000`); strict RFC3339 parsers reject these. Try
/// chrono's `%+` (RFC3339) first, then fall back to JIRA's
/// `%Y-%m-%dT%H:%M:%S%.3f%z` shape.
///
/// Duplicated (rather than shared) from the equivalent helper in
/// `commands/incidents/mod.rs` — that copy is private to its module and
/// this crate does not currently have a shared date-parsing utility module;
/// see issue #3966 for context. A future cleanup could hoist both into
/// `collect::jira` or a new `collect::datetime` module.
pub(crate) fn parse_jira_datetime(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&Utc));
    }
    chrono::DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.3f%z")
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// One JIRA comment, reduced to what `fact_jira_comment_detail` needs
/// (issue #3966).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JiraComment {
    /// Comment ID (unique per-instance).
    pub id: String,
    /// Display name of the comment's author, when JIRA reports one.
    pub author: Option<String>,
    /// Comment creation timestamp.
    pub created: DateTime<Utc>,
    /// Length of the comment body — see [`JiraClient::fetch_comments`] doc
    /// comment for the ADF-vs-plain-text caveat.
    pub body_len: i64,
}

impl JiraComment {
    /// Convert one wire-form comment, or `None` if its `created` timestamp
    /// is unparseable (mirrors `ChangelogIssue::from_api`'s per-entry skip
    /// behavior — one malformed record must not abort the whole batch, nor
    /// fabricate a fake timestamp that would corrupt freshness/ordering
    /// queries downstream).
    pub(crate) fn from_api(api: ApiComment) -> Option<Self> {
        let created = match parse_jira_datetime(&api.created) {
            Some(c) => c,
            None => {
                warn!(
                    comment_id = %api.id,
                    created = %api.created,
                    "unparseable comment timestamp; skipping this comment"
                );
                return None;
            }
        };
        let body_len = match &api.body {
            Value::String(s) => s.len() as i64,
            other => serde_json::to_string(other).map(|s| s.len()).unwrap_or(0) as i64,
        };
        Some(Self {
            id: api.id,
            author: api.author.map(|a| a.display_name),
            created,
            body_len,
        })
    }
}

/// Wire shape of `POST /rest/api/3/search` with `expand=changelog`.
///
/// Neither `startAt` nor `total` is modelled: the keyset walk terminates on
/// a short page and tracks its own window (see [`super::paging`]), so it
/// depends on no server-side bookkeeping field.
#[derive(Debug, Deserialize)]
pub(crate) struct ChangelogSearchResponse {
    pub(crate) issues: Vec<ChangelogApiIssue>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChangelogApiIssue {
    pub(crate) key: String,
    pub(crate) fields: ChangelogApiFields,
    #[serde(default)]
    pub(crate) changelog: Option<ApiChangelog>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct ChangelogApiFields {
    #[serde(default)]
    pub(crate) project: Option<ApiProjectRef>,
    #[serde(default)]
    pub(crate) updated: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiProjectRef {
    pub(crate) key: String,
}

/// The `changelog` object embedded in a search response by
/// `expand=changelog`.
#[derive(Debug, Deserialize)]
pub(crate) struct ApiChangelog {
    /// The embedded (possibly truncated) page of history entries.
    pub(crate) histories: Vec<ApiChangelogHistory>,
    /// Server-reported count of ALL history entries on the issue. Exceeds
    /// `histories.len()` exactly when JIRA truncated the embedded copy.
    /// `None` when the field is absent from the response — see
    /// [`embedded_changelog_is_truncated`] for how that is handled.
    #[serde(default)]
    pub(crate) total: Option<u64>,
}

/// Wire shape of `GET /rest/api/3/issue/{key}/changelog` (issue #4084).
///
/// The entry list is `values`, **not** `histories` — the dedicated endpoint
/// returns Atlassian's generic paged-bean envelope, unlike the
/// search-embedded changelog which nests the same entries under `histories`.
/// The entry shape itself is identical, hence the shared
/// [`ApiChangelogHistory`].
///
/// Unlike [`CommentSearchResponse`], `total` IS modelled here and is load
/// bearing: it is the only signal that distinguishes "this ticket's history
/// is fully retrieved" from "the server stopped early", and
/// [`super::changelog`] turns a shortfall into an error rather than a short
/// history. The walk still terminates on an empty page, so a missing `total`
/// cannot wedge it.
#[derive(Debug, Deserialize)]
pub(crate) struct ChangelogPageResponse {
    #[serde(default)]
    pub(crate) values: Vec<ApiChangelogHistory>,
    #[serde(default)]
    pub(crate) total: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiChangelogHistory {
    #[serde(default)]
    pub(crate) author: Option<ApiAuthor>,
    pub(crate) created: String,
    pub(crate) items: Vec<ApiChangelogItem>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiAuthor {
    #[serde(rename = "displayName")]
    pub(crate) display_name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiChangelogItem {
    pub(crate) field: String,
    #[serde(rename = "fromString", default)]
    pub(crate) from_string: Option<String>,
    #[serde(rename = "toString", default)]
    pub(crate) to_string: Option<String>,
}

/// Wire shape of `GET /rest/api/3/issue/{key}/comment`.
///
/// `total` is deliberately NOT modelled, for the same reason `startAt` is not
/// modelled on the search response (see [`super::client`]): under
/// `#[serde(default)]` an absent field silently reads as `0`, and a loop that
/// terminates on `start_at >= total` would then stop after its first page and
/// report success. That is not hypothetical — a response omitting `total`
/// ingested 100 of a ticket's 150 comments with no error at all, and because
/// the fetch returned `Ok` the failed-ticket cursor clamp let the cursor
/// advance past the ticket, making the loss permanent (PR #4067 review round
/// 3). [`super::client::JiraClient::fetch_comments`] now terminates on a
/// short page instead, which depends on no server-side bookkeeping field.
#[derive(Debug, Deserialize)]
pub(crate) struct CommentSearchResponse {
    pub(crate) comments: Vec<ApiComment>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiComment {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) author: Option<ApiAuthor>,
    pub(crate) created: String,
    #[serde(default = "default_comment_body")]
    pub(crate) body: Value,
}

/// Default `body` value when JIRA omits it entirely (should not happen in
/// practice, but keeps deserialization infallible).
pub(crate) fn default_comment_body() -> Value {
    Value::String(String::new())
}

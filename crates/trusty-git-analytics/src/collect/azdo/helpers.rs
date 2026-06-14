//! Free-function helpers for the Azure DevOps integration.
//!
//! Why: separates stateless parsing and extraction logic from the
//! `AzureDevOpsClient` struct impl so each file stays under the SLOC cap and
//! the helpers can be tested without constructing a client.
//! What: provides `parse_work_item`, `parse_work_item_extended`,
//! `extract_commit_shas_from_relations`, `extract_work_item_refs`,
//! `feed_azdo_users`, and `fetch_referenced_work_items`.
//! Test: see `azdo/tests.rs` for unit and integration tests covering each
//! function.

use crate::collect::azdo::{
    client::AzureDevOpsClient,
    errors::AzdoError,
    types::{AzdoUser, AzdoWorkItem, AzdoWorkItemExtended},
    wire::{WorkItemRaw, WorkItemRelationRaw},
};

/// Map an HTTP response with a non-success status to the appropriate
/// [`AzdoError`] variant.
///
/// Why: centralises the 401/403/404/other mapping so every client method uses
/// the same logic without duplicating the match arm.
/// What: reads the status code and, for `other`, drains the body as a string.
/// Test: exercised by every `*_maps_4xx` test in `azdo/tests.rs`.
pub(super) async fn map_response_error(resp: reqwest::Response) -> AzdoError {
    let status = resp.status().as_u16();
    match status {
        401 => AzdoError::Unauthorized,
        403 => AzdoError::Forbidden,
        404 => AzdoError::NotFound,
        s => {
            let message = resp.text().await.unwrap_or_default();
            AzdoError::Http { status: s, message }
        }
    }
}

/// Percent-encode a single path segment (e.g. an ADO project name).
///
/// Encodes any byte outside the unreserved set
/// (`ALPHA / DIGIT / "-" / "." / "_" / "~"`) as `%HH`. This is conservative
/// but correct: it never produces an invalid URL, even if the project name
/// contains spaces, slashes, or non-ASCII characters.
///
/// Why: ADO project names may contain spaces; URLs must encode them.
/// What: percent-encodes every byte outside the RFC 3986 unreserved set.
/// Test: `encode_path_segment_*` tests in `azdo/tests.rs`.
pub(super) fn encode_path_segment(s: &str) -> String {
    fn is_unreserved(b: u8) -> bool {
        b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
    }
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if is_unreserved(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Build an authenticated [`reqwest::Client`] for ADO API calls.
///
/// * Uses HTTP Basic auth with an empty username and `pat` as the password
///   via reqwest's per-request [`reqwest::RequestBuilder::basic_auth`] — no
///   `base64` dependency required.
/// * Sets a 30-second total request timeout.
/// * Identifies via `User-Agent: tga/{CARGO_PKG_VERSION}`.
///
/// Why: centralises reqwest client construction so all ADO methods share the
/// same timeout and headers.
/// What: builds and returns a configured `reqwest::Client`.
/// Test: exercised by every async client test in `azdo/tests.rs`.
pub(super) fn build_client() -> Result<reqwest::Client, AzdoError> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::USER_AGENT,
        reqwest::header::HeaderValue::from_static(concat!("tga/", env!("CARGO_PKG_VERSION"))),
    );
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("application/json"),
    );

    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(AzdoError::Request)
}

/// Project a raw ADO work item (with arbitrary fields map) into our
/// flat [`AzdoWorkItem`] shape. Missing fields default to empty strings.
///
/// Why: isolates the field-extraction logic from the HTTP client methods.
/// What: maps `WorkItemRaw.fields` into a typed `AzdoWorkItem`.
/// Test: `get_work_items_batch_parses_response` in `azdo/tests.rs`.
pub(super) fn parse_work_item(raw: WorkItemRaw) -> AzdoWorkItem {
    let get_str = |key: &str| -> String {
        raw.fields
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let tags_raw = get_str("System.Tags");
    let tags = if tags_raw.is_empty() {
        Vec::new()
    } else {
        tags_raw
            .split(';')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    let get_opt = |key: &str| -> Option<String> {
        raw.fields
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    };

    AzdoWorkItem {
        id: raw.id,
        title: get_str("System.Title"),
        state: get_str("System.State"),
        work_item_type: get_str("System.WorkItemType"),
        tags,
        team_project: get_str("System.TeamProject"),
        url: raw.url,
        iteration_path: get_opt("System.IterationPath"),
        area_path: get_opt("System.AreaPath"),
    }
}

/// Build an [`AzdoWorkItemExtended`] from a raw single-fetch work item
/// (the `$expand=all` shape). Splits `System.Tags` on `; ` and routes
/// non-standard fields into [`AzdoWorkItemExtended::custom_fields`].
///
/// Why: isolates extended-item parsing so the client method stays lean.
/// What: maps `WorkItemRaw.fields` into `AzdoWorkItemExtended`, routing
/// non-standard fields into `custom_fields`.
/// Test: `get_work_item_extended_returns_full_fields` in `azdo/tests.rs`.
pub(super) fn parse_work_item_extended(raw: WorkItemRaw) -> AzdoWorkItemExtended {
    use std::collections::HashMap;

    // The "standard" fields we surface as named struct fields; everything
    // else lands in `custom_fields`.
    const STANDARD_FIELDS: &[&str] = &[
        "System.Id",
        "System.Title",
        "System.State",
        "System.WorkItemType",
        "System.Tags",
        "System.IterationPath",
        "System.AreaPath",
    ];

    let get_str = |key: &str| -> String {
        raw.fields
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let get_opt = |key: &str| -> Option<String> {
        raw.fields
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    };

    let tags_raw = get_str("System.Tags");
    let tags = if tags_raw.is_empty() {
        Vec::new()
    } else {
        tags_raw
            .split(';')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    let mut custom_fields: HashMap<String, serde_json::Value> = HashMap::new();
    for (k, v) in &raw.fields {
        if !STANDARD_FIELDS.contains(&k.as_str()) {
            custom_fields.insert(k.clone(), v.clone());
        }
    }

    AzdoWorkItemExtended {
        id: raw.id,
        title: get_str("System.Title"),
        state: get_str("System.State"),
        work_item_type: get_str("System.WorkItemType"),
        iteration_path: get_opt("System.IterationPath"),
        area_path: get_opt("System.AreaPath"),
        tags,
        custom_fields,
    }
}

/// Extract commit SHAs from a list of ADO work-item relations.
///
/// ADO encodes a commit link as a relation with `rel == "ArtifactLink"`
/// and a `url` of the form
/// `vstfs:///Git/Commit/<projectId>%2F<repoId>%2F<sha>`. The SHA is the
/// segment after the second `%2F` (or `/` after URL-decoding).
///
/// Why: isolates commit-link extraction logic from the HTTP client.
/// What: scans relations for artifact/versioned commit links and returns SHA
/// strings.
/// Test: `extract_commit_shas_handles_versioned_and_artifact_rels` in
/// `azdo/tests.rs`.
pub(super) fn extract_commit_shas_from_relations(relations: &[WorkItemRelationRaw]) -> Vec<String> {
    let mut out = Vec::new();
    for r in relations {
        // ADO uses `ArtifactLink` with attribute `name == "Fixed in Commit"`
        // (or "Branch", "Pull Request", ...). We accept any artifact link
        // whose URL points to `vstfs:///Git/Commit/...`. We also keep the
        // legacy `System.LinkTypes.Versioned*` `rel` values in case ADO
        // surfaces them on older work items.
        let is_artifact = r.rel.eq_ignore_ascii_case("ArtifactLink");
        let is_versioned = r.rel.starts_with("System.LinkTypes.Versioned");
        if !(is_artifact || is_versioned) {
            continue;
        }
        // Match the commit URL scheme. We accept both `%2F` (URL-encoded)
        // and `/` separators between the path segments.
        let lower = r.url.to_lowercase();
        if !lower.starts_with("vstfs:///git/commit/") {
            continue;
        }
        let suffix = &r.url["vstfs:///Git/Commit/".len()..];
        // Take the last segment after either `%2F` or `/`. ADO emits
        // `%2F` in practice; we tolerate both.
        let last = suffix
            .rsplit_once("%2F")
            .or_else(|| suffix.rsplit_once("%2f"))
            .or_else(|| suffix.rsplit_once('/'))
            .map(|(_, sha)| sha)
            .unwrap_or(suffix);
        // Strip any trailing query string just in case.
        let sha = last.split('?').next().unwrap_or(last).trim();
        if !sha.is_empty() {
            out.push(sha.to_string());
        }
    }
    out
}

/// Extract Azure DevOps work-item IDs from arbitrary text using a
/// caller-provided regex.
///
/// The first capture group of `re` is treated as the numeric work-item ID.
/// The default `AB#(\d+)` pattern lives on
/// [`AzureDevOpsConfig::ticket_regex`](crate::core::config::AzureDevOpsConfig);
/// callers are expected to compile it once and reuse the result. IDs are
/// deduplicated in first-seen order. Captures whose first group does not
/// parse as `u32` are silently skipped.
///
/// Why: callers (e.g. `fetch_referenced_work_items`, `backfill`) need to
/// extract ADO IDs from commit messages without coupling to the client.
/// What: iterates regex captures on `text` and returns deduplicated `u32` IDs.
/// Test: `extract_work_item_refs_finds_ids` and related tests in `azdo/tests.rs`.
pub fn extract_work_item_refs(re: &regex::Regex, text: &str) -> Vec<u32> {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for cap in re.captures_iter(text) {
        if let Some(m) = cap.get(1) {
            if let Ok(id) = m.as_str().parse::<u32>() {
                if seen.insert(id) {
                    out.push(id);
                }
            }
        }
    }
    out
}

/// Feed ADO Graph users into an [`crate::collect::identity::IdentityResolver`].
///
/// For each user with a non-empty `mail_address`, registers the email
/// address as an alias for the user's display name via the resolver's
/// alias map. Users without an email are skipped — there is no reliable
/// canonical join key to register them under.
///
/// This is a one-shot ingestion helper; it does not mutate the resolver
/// after construction. Callers that need a long-lived ingestion loop should
/// roll their own using the resolver's public alias-update APIs.
///
/// Why: decouples Graph-user ingestion from the HTTP client so it can be
/// called after fetching users without needing the client struct.
/// What: iterates `users`, registering `mail_address → display_name` aliases
/// in the resolver.
/// Test: `feed_azdo_users_registers_email_aliases` in `azdo/tests.rs`.
pub fn feed_azdo_users(
    resolver: &mut crate::collect::identity::IdentityResolver,
    users: &[AzdoUser],
) {
    for u in users {
        let Some(email) = u.mail_address.as_deref() else {
            continue;
        };
        let email = email.trim();
        if email.is_empty() || u.display_name.trim().is_empty() {
            continue;
        }
        resolver.add_alias(email, &u.display_name);
    }
}

/// Scan a list of commit messages (or other text) for work-item references and
/// fetch the referenced work items in a single batch call (Phase 6).
///
/// `re` is the caller-compiled work-item-reference pattern (typically
/// [`AzureDevOpsConfig::ticket_regex`](crate::core::config::AzureDevOpsConfig)).
/// IDs are deduplicated across all messages. `project` is currently unused —
/// the batch endpoint is organisation-scoped — but is retained in the API for
/// future per-project filtering and for symmetry with the other methods.
///
/// Returns an empty vector if no references are found.
///
/// # Errors
///
/// Same set as [`AzureDevOpsClient::get_work_items`].
///
/// Why: provides a convenient single-call entry point for the common pattern
/// of "extract IDs from messages then batch-fetch them."
/// What: calls `extract_work_item_refs` across all messages then calls
/// `client.get_work_items`.
/// Test: `fetch_referenced_work_items_aggregates_from_messages` in `azdo/tests.rs`.
pub async fn fetch_referenced_work_items(
    client: &AzureDevOpsClient,
    re: &regex::Regex,
    messages: &[&str],
    _project: &str,
) -> Result<Vec<AzdoWorkItem>, AzdoError> {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for msg in messages {
        for id in extract_work_item_refs(re, msg) {
            if seen.insert(id) {
                ids.push(id);
            }
        }
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    client.get_work_items(&ids).await
}

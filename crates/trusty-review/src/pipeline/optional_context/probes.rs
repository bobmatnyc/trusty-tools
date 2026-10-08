//! Ledger rows for the context the review gathers itself (#9194).
//!
//! Why: trusty-search, trusty-analyze and the external sources fail open, so
//! a review that got nothing from them looked like one that was never asked,
//! and a context-starved APPROVE read like a fully-informed one (AC3).
//! What: [`search_row`], [`analyze_row`] and [`external_row`] turn what the
//! review's own calls returned into the `search`, `analyze` and
//! `external_sources` rows; [`ContextRows`] carries them to
//! `ContextLedger::finish`; [`cap_detail`] bounds and redacts every detail.
//! No function here makes a call.
//! Test: `report_context_lists_every_source_in_canonical_order`,
//! `search_failure_is_unavailable_not_absent`,
//! `analyze_hotspot_error_makes_the_row_unavailable`,
//! `external_row_is_worst_of_its_sources_and_names_the_failed_one`,
//! `cap_detail_cuts_to_one_line_of_200_characters`, `cap_detail_redacts_credentials`.

use super::docs_render::rank;
use crate::{
    integrations::context::{ContextSource, ContextSourcesConfig, orchestrator::SourceOutcome},
    models::{ContextItemRecord, ContextSourceRecord, SourceState},
};

/// The longest detail a ledger row carries (#9194).
pub(crate) const MAX_DETAIL_CHARS: usize = 200;

/// The rows the review's own context gathering produced (#9194).
#[derive(Debug, Clone)]
pub(crate) struct ContextRows {
    /// The `search` row.
    pub(crate) search: ContextSourceRecord,
    /// The `analyze` row.
    pub(crate) analyze: ContextSourceRecord,
    /// The `external_sources` row.
    pub(crate) external: ContextSourceRecord,
}

/// What the code-context query did (#9194, plan §2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SearchProbe {
    /// No trusty-search index covers this checkout (#8411).
    NoIndex,
    /// No identifiers and no title, so no query was sent.
    EmptyQuery,
    /// The query answered with this many hits.
    Hits(usize),
    /// The query failed (not a 404); the error text.
    Failed(String),
}

/// What the static-analysis calls did (#9194, plan §2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AnalyzeProbe {
    /// `ReviewDeps::analyze` is `None`.
    NoClient,
    /// No trusty-search index covers this checkout.
    NoIndex,
    /// `AnalyzeClient::analysis_status` said no; its reason.
    NotReady(String),
    /// Both calls ran: items in the changed files, or the error text.
    Ran {
        /// Hotspots in the changed files, or why the call failed.
        hotspots: Result<usize, String>,
        /// Smells in the changed files, or why the call failed.
        smells: Result<usize, String>,
    },
}

/// The `search` row for `probe`.
///
/// Test: `search_failure_is_unavailable_not_absent`, `search_with_no_hits_is_absent`,
/// `search_with_an_empty_query_is_absent`, `search_with_no_index_is_unavailable`,
/// `search_used_when_hits_arrive`.
pub(crate) fn search_row(probe: &SearchProbe) -> ContextSourceRecord {
    let (state, detail) = match probe {
        SearchProbe::NoIndex => (
            SourceState::Unavailable,
            "no trusty-search index covers this checkout".to_string(),
        ),
        SearchProbe::EmptyQuery => (
            SourceState::Absent,
            "no identifiers or title to query".to_string(),
        ),
        SearchProbe::Hits(0) => (SourceState::Absent, "no hits for the query".to_string()),
        SearchProbe::Hits(_) => (SourceState::Used, String::new()),
        SearchProbe::Failed(e) => (
            SourceState::Unavailable,
            format!("trusty-search query failed: {e}"),
        ),
    };
    ContextSourceRecord::new("search", state).with_detail(&cap_detail(&detail))
}

/// The `analyze` row for `probe`: with items `hotspots` and `smells` when
/// both calls ran, the row being the worst of them (`absent` when neither
/// found anything in the changed files).
///
/// Test: `analyze_without_a_client_is_unavailable`, `analyze_without_an_index_is_unavailable`,
/// `analyze_hotspot_error_makes_the_row_unavailable`,
/// `analyze_smells_error_makes_the_row_unavailable`,
/// `analyze_with_nothing_in_the_changed_files_is_absent`,
/// `analyze_used_when_hotspots_are_in_the_changed_files`.
pub(crate) fn analyze_row(probe: &AnalyzeProbe) -> ContextSourceRecord {
    let unavailable = |detail: &str| {
        ContextSourceRecord::new("analyze", SourceState::Unavailable)
            .with_detail(&cap_detail(detail))
    };
    let (hotspots, smells) = match probe {
        AnalyzeProbe::NoClient => return unavailable("analyze client absent"),
        AnalyzeProbe::NoIndex => return unavailable("no trusty-search index covers this checkout"),
        AnalyzeProbe::NotReady(e) => return unavailable(e),
        AnalyzeProbe::Ran { hotspots, smells } => (hotspots, smells),
    };
    let items = vec![
        analyze_item("hotspots", "complexity_hotspots", hotspots),
        analyze_item("smells", "smells", smells),
    ];
    let mut row = ContextSourceRecord::new("analyze", worst(&items));
    if row.state == SourceState::Absent {
        row.detail = Some("no hotspots or smells in the changed files".to_string());
    }
    row.items = items;
    row
}

/// One analyze call's item: `used`, `absent`, or `unavailable` naming `call`.
fn analyze_item(id: &str, call: &str, outcome: &Result<usize, String>) -> ContextItemRecord {
    match outcome {
        Ok(0) => ContextItemRecord::new(id, SourceState::Absent, 0, 0),
        Ok(_) => ContextItemRecord::new(id, SourceState::Used, 0, 0),
        Err(e) => ContextItemRecord::new(id, SourceState::Unavailable, 0, 0)
            .with_detail(&cap_detail(&format!("{call} failed: {e}"))),
    }
}

/// The detail of a source config enables that is disabled (amendment 2).
pub(crate) const CONFIGURED_BUT_DISABLED: &str =
    "enabled in config but disabled: credentials or transport missing";

/// The `external_sources` row (#9194, plan §2.4, rulings Q4 and R2).
///
/// Why: a failed enrichment source must show, and a source the operator
/// enabled that could not run must not read as never asked for.
/// What: one item per source in `sources` order: an enabled source's
/// [`SourceOutcome`]; a disabled source whose `cs` entry is an explicit
/// `enabled = true` reads `unavailable` with [`CONFIGURED_BUT_DISABLED`];
/// any other disabled source gets no item. No item at all is
/// `not_requested`. Otherwise the row is the worst item, `chars` is the
/// rendered block's length, and an `unavailable` row names its sources.
/// Test: `no_enabled_external_source_is_not_requested`,
/// `external_row_is_worst_of_its_sources_and_names_the_failed_one`,
/// `configured_but_disabled_source_is_unavailable`, `detail_is_bounded_and_single_line`.
pub(crate) fn external_row(
    sources: &[Box<dyn ContextSource>],
    outcomes: &[SourceOutcome],
    cs: &ContextSourcesConfig,
    chars: usize,
) -> ContextSourceRecord {
    let items: Vec<ContextItemRecord> = sources
        .iter()
        .filter_map(|source| {
            let name = source.name();
            if let Some(o) = outcomes.iter().find(|o| o.name == name) {
                let detail = o.detail.as_deref().map(cap_detail).unwrap_or_default();
                return Some(
                    ContextItemRecord::new(name, o.state, o.chars, 0).with_detail(&detail),
                );
            }
            (!source.is_enabled() && configured_on(cs, name)).then(|| {
                ContextItemRecord::new(name, SourceState::Unavailable, 0, 0)
                    .with_detail(CONFIGURED_BUT_DISABLED)
            })
        })
        .collect();
    if items.is_empty() {
        return ContextSourceRecord::new("external_sources", SourceState::NotRequested)
            .with_detail("no external context source is configured");
    }
    let mut row = ContextSourceRecord::new("external_sources", worst(&items));
    let failed: Vec<&str> = items
        .iter()
        .filter(|i| i.state == SourceState::Unavailable)
        .map(|i| i.id.as_str())
        .collect();
    if !failed.is_empty() {
        row.detail = Some(cap_detail(&format!("unavailable: {}", failed.join(", "))));
    }
    row.chars = chars;
    row.items = items;
    row
}

/// Whether `cs` explicitly enables the source `name`.
fn configured_on(cs: &ContextSourcesConfig, name: &str) -> bool {
    let enabled = match name {
        "jira" => cs.jira.enabled,
        "confluence" => cs.confluence.enabled,
        "github_issues" => cs.github_issues.enabled,
        "conformance" => cs.conformance.base.enabled,
        "pr_history" => cs.pr_history.enabled,
        _ => None,
    };
    enabled == Some(true)
}

/// The worst item state (`unavailable` > `truncated`/`omitted` > `used` >
/// `absent`), `absent` for no items (the `docs_render` rule, plan §2.1).
fn worst(items: &[ContextItemRecord]) -> SourceState {
    items
        .iter()
        .map(|i| i.state)
        .max_by_key(|s| rank(*s))
        .filter(|s| rank(*s) > 0)
        .unwrap_or(SourceState::Absent)
}

/// `text` as a ledger detail: one line, credentials redacted, at most
/// [`MAX_DETAIL_CHARS`] characters (#9194, ruling R3).
///
/// Why: a detail carries transport and API error text, which can span lines,
/// echo a response body, or hold a token.
/// What: collapses every whitespace run to one space, replaces the value
/// after a `bearer`/`basic` scheme (also glued as `authorization:bearer`,
/// `authorization=bearer` or JSON-quoted), the value of an
/// `authorization`/`token`/`password`/`x-api-key` header (spaced or glued to
/// its colon), a URL userinfo password and any `token=`/`key=`/`secret=`/
/// `password=` pair with `[redacted]`, masks credential-shaped runs, then
/// cuts to the cap with a trailing `…`.
/// Test: `cap_detail_cuts_to_one_line_of_200_characters`,
/// `cap_detail_redacts_credentials`, `a_bearer_token_never_reaches_a_detail`,
/// `cap_detail_masks_bearer_without_a_space`, `cap_detail_masks_an_x_api_key_value`,
/// `cap_detail_masks_a_url_userinfo_password`,
/// `cap_detail_masks_a_header_value_glued_to_its_colon`,
/// `cap_detail_masks_a_json_quoted_authorization_header`,
/// `cap_detail_masks_an_equals_joined_bearer_scheme`.
pub(crate) fn cap_detail(text: &str) -> String {
    let redacted = redact_credentials(text);
    let words: Vec<&str> = redacted.split_whitespace().collect();
    let line = crate::pipeline::reply_shape::mask_credential_shapes(&words.join(" "));
    if line.chars().count() <= MAX_DETAIL_CHARS {
        return line;
    }
    let mut cut: String = line.chars().take(MAX_DETAIL_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// `text` with every credential [`cap_detail`] hides replaced by
/// `[redacted]`; whitespace and all other text kept, uncapped (#9431).
///
/// Why: error and log text that names a configured URL (the trusty-search
/// URL, #9431) must drop its userinfo password and credential query values
/// but stay a readable, full-length message.
/// What: the word rules `cap_detail` applies — the value after a credential
/// scheme or header, a URL userinfo password, a `token=`/`key=`/`secret=`/
/// `password=`/`auth` pair — without its whitespace collapse, shape mask or
/// length cap.
/// Test: `redact_credentials_keeps_the_message_and_hides_url_credentials`.
pub(crate) fn redact_credentials(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut hide_next = false;
    let mut rest = text;
    while !rest.is_empty() {
        let word_at = rest
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(rest.len());
        out.push_str(&rest[..word_at]);
        rest = &rest[word_at..];
        let word_len = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (word, tail) = rest.split_at(word_len);
        rest = tail;
        if !word.is_empty() {
            out.push_str(&redact_word(word, &mut hide_next));
        }
    }
    out
}

/// One word of [`redact_credentials`]; `hide_next` carries a scheme or
/// header name over to the word holding its value.
fn redact_word(word: &str, hide_next: &mut bool) -> String {
    match credential_word(&word.to_ascii_lowercase()) {
        Some(Hide::Next) => {
            *hide_next = true;
            word.to_string()
        }
        Some(Hide::After(keep)) => {
            *hide_next = false;
            format!("{}[redacted]", &word[..keep])
        }
        None if std::mem::take(hide_next) => "[redacted]".to_string(),
        None => redact_pairs(&redact_userinfo(word)),
    }
}

/// What a credential word hides: the next word, or its own tail.
enum Hide {
    /// The value is the next word (`Bearer`, `Authorization:`).
    Next,
    /// Keep this many leading bytes, redact the rest (`Token:abc`).
    After(usize),
}

/// How `lower` (one lowercased word) introduces a credential, if it does.
///
/// #9194: the name and scheme are compared with surrounding punctuation
/// stripped, so `"Authorization":"Bearer` and `authorization=bearer` match.
fn credential_word(lower: &str) -> Option<Hide> {
    const SCHEMES: [&str; 2] = ["bearer", "basic"];
    const HEADERS: [&str; 4] = ["authorization", "token", "password", "x-api-key"];
    if SCHEMES.contains(&bare(lower)) {
        return Some(Hide::Next);
    }
    let (head, rest) = lower.split_once([':', '='])?;
    let value = bare(rest);
    if SCHEMES.contains(&value) {
        return Some(Hide::Next);
    }
    // `key=value` pairs are left to `redact_pairs`, which keeps `&` siblings.
    if !lower[head.len()..].starts_with(':') || !HEADERS.contains(&bare(head)) {
        return None;
    }
    Some(if value.is_empty() {
        Hide::Next
    } else {
        Hide::After(head.len() + 1)
    })
}

/// `s` without leading or trailing punctuation (`-` kept, for `x-api-key`).
fn bare(s: &str) -> &str {
    s.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-')
}

/// `word` with the password of a `scheme://user:password@host` URL replaced
/// by `[redacted]`; the user, host and path are kept (#9194).
fn redact_userinfo(word: &str) -> String {
    let Some(start) = word.find("://").map(|i| i + 3) else {
        return word.to_string();
    };
    let end = word[start..]
        .find(['/', '?', '#'])
        .map_or(word.len(), |i| start + i);
    let Some(at) = word[start..end].rfind('@').map(|i| start + i) else {
        return word.to_string();
    };
    let Some(colon) = word[start..at].find(':').map(|i| start + i) else {
        return word.to_string();
    };
    format!("{}[redacted]{}", &word[..=colon], &word[at..])
}

/// `word` with the value of every `key=value` pair whose key names a
/// credential replaced by `[redacted]`.
fn redact_pairs(word: &str) -> String {
    const SECRET_KEYS: [&str; 5] = ["token", "key", "secret", "password", "auth"];
    word.split('&')
        .map(|pair| match pair.split_once('=') {
            Some((key, _))
                if SECRET_KEYS
                    .iter()
                    .any(|k| key.to_ascii_lowercase().contains(k)) =>
            {
                format!("{key}=[redacted]")
            }
            _ => pair.to_string(),
        })
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
#[path = "probes_tests.rs"]
mod tests;

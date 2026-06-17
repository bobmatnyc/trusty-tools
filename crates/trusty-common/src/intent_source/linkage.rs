//! PR → ticket linkage: extract the governing ticket-id from a pull request.
//!
//! Why: the BACK gate starts from a PR, not a ticket-id, so the ISR must
//! resolve *which* ticket governs the change. trusty-review's existing linkage
//! is fuzzy keyword search (spec §2.1); the ISR replaces it with explicit
//! parsing. The regexes are **lifted from tga** (`collect/ticket.rs`) so both
//! gates parse linkage without pulling tga's git2/rusqlite stack (spec §6.3).
//! What: `is_ticketed` / `extract_ticket_id` (lifted verbatim in behaviour),
//! plus `extract_pr_ticket` which applies the spec's source-precedence order
//! (PR body trailers → commit trailers → branch name).
//! Test: `super::tests::linkage_*` (AC-5).

use std::sync::OnceLock;

use regex::Regex;

/// Compiled regexes that qualify text as "ticketed" (lifted from tga).
///
/// Why: a bare `#N` is too noisy to qualify on its own (tga issue #445); only
/// JIRA/Linear, GitHub action-keyword refs, and ADO refs qualify.
/// What: the three qualifying patterns. `gh_bare` lives in [`ExtractPatterns`].
/// Test: `super::tests::linkage_is_ticketed`.
struct TicketPatterns {
    jira: Regex,
    gh_action: Regex,
    azdo: Regex,
}

/// Lazily-initialised qualifying pattern set.
///
/// Why: compile the regexes exactly once across all resolver calls.
/// What: returns a `'static` reference to the compiled `TicketPatterns`.
/// Test: exercised by every `linkage_*` test.
fn patterns() -> &'static TicketPatterns {
    static PATTERNS: OnceLock<TicketPatterns> = OnceLock::new();
    PATTERNS.get_or_init(|| TicketPatterns {
        jira: Regex::new(r"\b[A-Z][A-Z0-9]*-\d+\b").expect("jira pattern compiles"),
        gh_action: Regex::new(r"(?i)\b(?:fix(?:es|ed)?|close[sd]?|resolve[sd]?)\s+#\d+\b")
            .expect("gh_action pattern compiles"),
        azdo: Regex::new(r"\bAB#\d+\b").expect("azdo pattern compiles"),
    })
}

/// Compiled extraction patterns, most-specific first (lifted from tga).
///
/// Why: when several ref styles appear, the highest-fidelity match wins.
/// What: ADO, then JIRA/Linear, then a bare `#N` last-resort.
/// Test: `super::tests::linkage_extract_*`.
struct ExtractPatterns {
    azdo: Regex,
    jira: Regex,
    gh_bare: Regex,
}

/// Lazily-initialised extraction pattern set.
///
/// Why: compile once; reuse across all resolver calls.
/// What: returns a `'static` reference to the compiled `ExtractPatterns`.
/// Test: exercised by every `linkage_extract_*` test.
fn extract_patterns() -> &'static ExtractPatterns {
    static EXTRACT: OnceLock<ExtractPatterns> = OnceLock::new();
    EXTRACT.get_or_init(|| ExtractPatterns {
        azdo: Regex::new(r"\bAB#\d+\b").expect("azdo extract pattern compiles"),
        jira: Regex::new(r"\b[A-Z][A-Z0-9]*-\d+\b").expect("jira extract pattern compiles"),
        gh_bare: Regex::new(r"(?:^|\s)(#\d+)\b").expect("gh_bare extract pattern compiles"),
    })
}

/// Return `true` if `message` contains a *qualifying* ticket reference.
///
/// Why: a bare `#N` is too noisy to count as linkage on its own (tga #445);
/// only action-keyword GitHub refs, JIRA/Linear ids, and ADO refs qualify.
/// What: lifted verbatim from `tga::collect::ticket::is_ticketed`.
/// Test: `super::tests::linkage_is_ticketed`.
#[must_use]
pub fn is_ticketed(message: &str) -> bool {
    let p = patterns();
    p.jira.is_match(message) || p.gh_action.is_match(message) || p.azdo.is_match(message)
}

/// Extract the first recognisable ticket id from arbitrary text.
///
/// Why: populate the linkage as a last-resort identifier even when no action
/// keyword is present (e.g. a bare `#N`).
/// What: lifted verbatim from `tga::collect::ticket::extract_ticket_id` —
/// tries ADO, then JIRA/Linear, then bare `#N`, in that priority order.
/// Test: `super::tests::linkage_extract_*`.
#[must_use]
pub fn extract_ticket_id(message: &str) -> Option<String> {
    let p = extract_patterns();
    if let Some(m) = p.azdo.find(message) {
        return Some(m.as_str().to_string());
    }
    if let Some(m) = p.jira.find(message) {
        return Some(m.as_str().to_string());
    }
    if let Some(m) = p.gh_bare.captures(message).and_then(|caps| caps.get(1)) {
        return Some(m.as_str().to_string());
    }
    None
}

/// Extract a `#N` ticket id from a branch name (`fix/1325-x` → `#1325`).
///
/// Why: a branch name is the spec's third linkage source (§6.3) and does not
/// carry the `#` sigil that [`extract_ticket_id`]'s bare pattern needs.
/// What: matches the first run of digits in a `kind/NNNN-slug` branch and
/// returns it as a `#N` GitHub-style id; returns `None` for branches with no
/// leading numeric segment.
/// Test: `super::tests::linkage_branch_*`.
#[must_use]
pub fn extract_branch_ticket(branch: &str) -> Option<String> {
    static BRANCH: OnceLock<Regex> = OnceLock::new();
    let re = BRANCH.get_or_init(|| {
        // kind/NNNN-slug or kind/NNNN ; capture the numeric work-item segment.
        Regex::new(r"(?:^|/)(\d+)(?:[-_/]|$)").expect("branch pattern compiles")
    });
    re.captures(branch)
        .and_then(|c| c.get(1))
        .map(|m| format!("#{}", m.as_str()))
}

/// Resolve the governing ticket-id from a PR, applying source precedence.
///
/// Why: the BACK gate must pick *one* ticket deterministically from several
/// possible linkage sources; the spec fixes the order (§6.3).
/// What: tries, in order — (a) the PR **body**, which is free-text prose and so
/// must carry a *qualifying* reference ([`is_ticketed`]: an action-keyword
/// GitHub ref, a JIRA/Linear id, or an ADO ref) before a ticket-id is taken
/// from it; (b) each commit message (a bare `#N` here is acceptable — commit
/// trailers are conventionally terse); then (c) the branch name via
/// [`extract_branch_ticket`]. Returns the first match; `None` when the PR
/// carries no linkage at all.
///
/// The body is gated on `is_ticketed` specifically to avoid the tga #445
/// false-positive: a PR body like `"See discussion in #42 for background"`
/// mentions `#42` only in passing and must NOT resolve to ticket 42, whereas
/// `"Closes #42"` (an action keyword) qualifies. Commit messages and branch
/// names are NOT free-text discussion, so they retain the bare-`#N`
/// last-resort fallback (spec §6.3).
/// Test: `super::tests::linkage_pr_*`, `super::tests::ac5_pr_body_bare_ref_*`
/// (AC-5).
#[must_use]
pub fn extract_pr_ticket(
    body: &str,
    commit_messages: &[String],
    branch: Option<&str>,
) -> Option<String> {
    // (a) PR body — free-text prose: require a QUALIFYING ref (tga #445). A bare
    // `#N` mentioned in passing must not link; an action keyword / JIRA / ADO
    // ref does. (Combinator form avoids a let-chain — trusty-common is edition
    // 2021 — while satisfying clippy::collapsible_if.)
    if let Some(id) = is_ticketed(body).then(|| extract_ticket_id(body)).flatten() {
        return Some(id);
    }
    // (b) commit-message trailers — terse by convention, so a bare `#N` is an
    // acceptable last resort here.
    for msg in commit_messages {
        if let Some(id) = extract_ticket_id(msg) {
            return Some(id);
        }
    }
    // (c) branch name (e.g. `fix/1325-…`).
    branch.and_then(extract_branch_ticket)
}

//! Ticket-reference detection for commit messages.
//!
//! A commit is considered *ticketed* if its message contains any reference
//! to an external work-tracking system. We currently recognize:
//!
//! - **JIRA / Linear style**: `PROJ-123`, `ENG-456`, `ABC-9` —
//!   uppercase project key, hyphen, digits, declared at the start of the
//!   commit SUBJECT. The Linear identifier format (`ENG-123`, `FE-456`) is a
//!   subset of this pattern.
//! - **GitHub action-keyword refs**: `fixes #123`, `closes #45`,
//!   `resolves #7` (case-insensitive, also matches `fix`/`close`/`resolve`).
//! - **Azure DevOps work-item refs**: `AB#123`.
//!
//! **Note on JIRA-shaped tokens (issues #5199, #5735):** [`extract_ticket_id`]
//! and [`is_ticketed`] read a JIRA/Linear key the same way — from the
//! **subject line only**, rejecting documentation prefixes (`ADR`, `DOC`,
//! `RFC`, `SPEC`) via [`subject_ticket_key`]. #5199 narrowed
//! [`extract_ticket_id`] and left [`is_ticketed`] scanning the whole message
//! with the unrestricted pattern; #5735 measured what that cost — 237 of this
//! repository's 2633 commits counted ticketed on nothing but a `UTF-8`- or
//! `ADR-0034`-shaped string in a body. The two now agree.
//!
//! **Note on bare `#N` refs (issue #445):** A bare `#N` preceded by
//! whitespace (the `gh_bare` pattern) is explicitly *excluded* from
//! [`is_ticketed`]. It fires on almost any multi-line commit body and was
//! inflating the ticketed rate to ~100%. The `gh_bare` pattern is still
//! used by [`extract_ticket_id`] to populate `commits.ticket_id` as a
//! last-resort identifier, so the data is not lost — it just no longer
//! counts as "ticketed" for quality-metric purposes.
//!
//! Patterns are compiled exactly once on first use via [`OnceLock`].

use std::sync::OnceLock;

use regex::Regex;

/// Compiled regexes used by [`is_ticketed`].
///
/// Note: `gh_bare` is intentionally excluded from this struct (issue #445).
/// A bare `#N` reference no longer qualifies a commit as "ticketed" — only
/// JIRA/Linear, GitHub action-keyword refs, and Azure DevOps refs do.
/// The bare pattern still lives in [`ExtractPatterns`] for `ticket_id` population.
///
/// #5735: there is no `jira` field either. A JIRA/Linear key now reaches
/// [`is_ticketed`] only through [`subject_ticket_key`], so the whole-message
/// pattern that used to live here has no remaining caller.
struct TicketPatterns {
    gh_action: Regex,
    azdo: Regex,
}

/// Global, lazily-initialized pattern set.
fn patterns() -> &'static TicketPatterns {
    static PATTERNS: OnceLock<TicketPatterns> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        // SAFETY of unwrap: these literals are validated by the test
        // [`patterns_compile`] below — any regression is caught at test
        // time, not at runtime.
        TicketPatterns {
            // GitHub action keyword: fix(es|ed)?|close(s|d)?|resolve(s|d)?  #123
            gh_action: Regex::new(r"(?i)\b(?:fix(?:es|ed)?|close[sd]?|resolve[sd]?)\s+#\d+\b")
                .expect("gh_action pattern compiles"),
            // Azure DevOps work-item reference: AB#123.
            // Bare #N intentionally excluded — collides with GitHub PR/issue numbers.
            azdo: Regex::new(r"\bAB#\d+\b").expect("azdo pattern compiles"),
        }
    })
}

/// Prefixes that name a *document*, never a work item (#5199).
///
/// Why: `ADR-0029` and `PROJ-1234` are the same shape, so shape alone cannot
/// separate them. These four prefixes are cross-industry documentation
/// conventions, and a repo that writes them in a commit subject is citing a
/// document, not a ticket.
/// What: compared against the segment before the first `-` of an otherwise
/// well-formed key.
/// Test: `tests::adr_reference_is_not_a_ticket_key`.
const DOC_REF_PREFIXES: &[&str] = &["ADR", "DOC", "RFC", "SPEC"];

/// Compiled extraction patterns used by [`extract_ticket_id`], ordered from
/// most-specific to least-specific so the highest-fidelity match wins.
struct ExtractPatterns {
    /// Azure DevOps work-item reference: `AB#123`.
    azdo: Regex,
    /// Leading noise stripped from a subject line before anchoring the
    /// JIRA/Linear key: an optional conventional-commit `type(scope)!:`
    /// prefix, then an optional `[` or `(`.
    subject_prefix: Regex,
    /// JIRA / Linear style anchored to the start of the stripped subject:
    /// `PROJ-123`, `ENG-456`, `DRE-405`.
    jira_anchored: Regex,
    /// GitHub bare issue reference: `#123`.
    gh_bare: Regex,
    /// #5734: JIRA/Linear key anchored to the start of one `/`-separated
    /// branch-name segment. Distinct from [`Self::jira_anchored`] only in its
    /// right boundary, which must also accept `-` and `_` because a branch
    /// slug runs the key straight into its description
    /// (`feature/PROJ-123-add-thing`).
    jira_branch_segment: Regex,
    /// #5734: GitHub action-keyword reference with the issue number captured
    /// (`Closes #5734`). [`TicketPatterns::gh_action`] only answers whether one
    /// is present; this one yields the `#N` to store.
    gh_action_ref: Regex,
}

/// Global, lazily-initialized extraction pattern set.
fn extract_patterns() -> &'static ExtractPatterns {
    static EXTRACT: OnceLock<ExtractPatterns> = OnceLock::new();
    EXTRACT.get_or_init(|| {
        // SAFETY of unwrap: all literals are validated by the test
        // [`extract_patterns_compile`] — any regression is caught at test time.
        ExtractPatterns {
            azdo: Regex::new(r"\bAB#\d+\b").expect("azdo extract pattern compiles"),
            subject_prefix: Regex::new(
                r"^(?:[a-z][a-z0-9]*(?:\([^)\n]*\))?!?:[ \t]*)?[\[(]?[ \t]*",
            )
            .expect("subject_prefix pattern compiles"),
            // #5199: anchored to `^`, so a key that is the tail of a longer
            // hyphenated identifier (`SPEC-INSTALLER-01` → `INSTALLER-01`)
            // can never match.
            jira_anchored: Regex::new(r"^([A-Z][A-Z0-9]*-\d+)(?:$|[\s:,.;)\]])")
                .expect("jira_anchored pattern compiles"),
            gh_bare: Regex::new(r"(?:^|\s)(#\d+)\b").expect("gh_bare extract pattern compiles"),
            // #5734: same `^`-anchoring as `jira_anchored`, so a tail segment of
            // a longer identifier stays unreachable here too.
            jira_branch_segment: Regex::new(r"^([A-Z][A-Z0-9]*-\d+)(?:$|[-_./\s])")
                .expect("jira_branch_segment pattern compiles"),
            gh_action_ref: Regex::new(
                r"(?i)\b(?:fix(?:es|ed)?|close[sd]?|resolve[sd]?)\s+(#\d+)\b",
            )
            .expect("gh_action_ref pattern compiles"),
        }
    })
}

/// Is `key` a documentation reference rather than a work-tracking key?
///
/// Why: see [`DOC_REF_PREFIXES`].
/// What: splits at the first `-` and matches the prefix against that list.
/// Test: `tests::adr_reference_is_not_a_ticket_key`.
fn is_doc_reference(key: &str) -> bool {
    key.split_once('-')
        .is_some_and(|(prefix, _)| DOC_REF_PREFIXES.contains(&prefix))
}

/// The JIRA/Linear key a commit *subject* declares, if any.
///
/// Why: a JIRA-shaped token is only evidence of a ticket when the author put
/// it where a ticket key goes. Accepting one from anywhere in the message
/// makes every `UTF-8`, `SHA-256`, `RUSTSEC-2026`, `GCC-11` and `ADR-0034` in
/// a commit body a ticket key — measured at 466 false keys across 178 distinct
/// prefixes on this repo's own 2633 commits, against zero genuine ones (#5199).
/// What: strips an optional conventional-commit prefix and an optional opening
/// bracket, then requires the key to be the very first token of what remains
/// and not a [`DOC_REF_PREFIXES`] citation.
/// Test: `tests::extract_ticket_id_subject_forms`,
/// `tests::adr_reference_is_not_a_ticket_key`,
/// `tests::body_prose_identifier_is_not_a_ticket_key`.
fn subject_ticket_key(subject: &str) -> Option<String> {
    let p = extract_patterns();
    let stripped = p.subject_prefix.replace(subject, "");
    let key = p
        .jira_anchored
        .captures(stripped.as_ref())?
        .get(1)?
        .as_str()
        .to_string();
    if is_doc_reference(&key) {
        None
    } else {
        Some(key)
    }
}

/// Return `true` if `message` contains any recognized ticket reference.
///
/// Why: downstream metrics (ticketed-commit rate, quality score) must only
/// count commits that are genuinely linked to a tracked work item. Two
/// separate noise classes had to be excluded to make that true.
///
/// A bare `#N` reference (e.g. `#42` from a release note) fires on nearly
/// every multi-line commit body and inflated the ticketed rate to ~100%
/// (issue #445). The `gh_bare` pattern is intentionally **excluded** from
/// this OR-chain; it is still used by [`extract_ticket_id`] to populate
/// `commits.ticket_id` as a last-resort identifier.
///
/// A JIRA-shaped token anywhere in the message is prose, not a declaration
/// (issue #5735). `UTF-8`, `SHA-256`, `GCC-11`, `ADR-0034` and `DOC-39` are
/// the same shape as `PROJ-123`, so no pattern can separate them and no
/// deny-list can enumerate them — 237 of this repository's 2633 commits were
/// counted ticketed on such a string alone. Position separates them:
/// [`subject_ticket_key`] accepts a key only where the author declares one.
///
/// What: returns `true` for a JIRA/Linear key the SUBJECT declares
/// (`PROJ-123: …`), a GitHub action-keyword ref anywhere in the message
/// (`closes #N`, `fixes #N`), or an Azure DevOps ref anywhere (`AB#N`).
/// A bare `#N`, and a JIRA-shaped token that is not the subject's leading
/// token, both return `false`.
/// Test: `tests::ticketed_*` below; the critical regression cases are
/// `bare_hash_alone_is_not_ticketed`, `closes_hash_is_ticketed`,
/// `jira_is_ticketed`, `azdo_is_ticketed`, and
/// `jira_shaped_prose_is_not_ticketed`.
///
/// # Examples
///
/// ```
/// use tga::collect::ticket::is_ticketed;
///
/// assert!(is_ticketed("ENG-123: add feature"));
/// assert!(is_ticketed("Fix login (closes #42)"));
/// assert!(!is_ticketed("misc cleanup"));
/// assert!(!is_ticketed("some note about #42"));
///
/// // #5735: a JIRA-shaped token in prose is not a ticket reference.
/// assert!(!is_ticketed("fix: stem handling\n\nA multi-byte UTF-8 name breaks it.\n"));
/// assert!(!is_ticketed("docs: amend ADR-0034"));
/// ```
pub fn is_ticketed(message: &str) -> bool {
    let p = patterns();
    p.gh_action.is_match(message)
        || p.azdo.is_match(message)
        // #5735: the same subject-anchored, doc-prefix-rejecting rule
        // `extract_ticket_id` uses, so the two cannot disagree.
        || subject_ticket_key(message.lines().next().unwrap_or("")).is_some()
}

/// Extract the ticket identifier a commit message declares.
///
/// Why: `commits.ticket_id` must be populated at insert time so that JIRA
/// classification and ticket-rate metrics work without requiring a separate
/// `tga backfill ticket-ids` pass. Issue #316 identified that 32% of
/// uncategorized commits had clearly extractable JIRA IDs (e.g. `BB-2746`,
/// `SRE-3104`, `DRE-405`) but NULL `ticket_id` because this extraction
/// only happened during backfill, not during `tga collect`.
/// [`crate::collect::correlate_commits`] then joins that key against
/// `work_items`, so a wrong key here is not a missed link — it is a reported
/// coverage gap against a ticket that exists on no board (#5199).
///
/// What: three tiers, most-specific first.
///
/// 1. Azure DevOps `AB#N`, anywhere in the message.
/// 2. A JIRA/Linear key **the subject line declares** — see
///    [`subject_ticket_key`] for what qualifies. A JIRA-shaped token anywhere
///    else in the message is prose, not a ticket key.
/// 3. GitHub bare `#N`, anywhere in the message.
///
/// Returns the first tier that matches, else `None`.
///
/// This takes no configuration and consults none: there is no
/// project-key allow-list to be present or absent, so there is no branch that
/// can silently return nothing because a repo was never configured. A repo
/// whose ticket keys are unanchored prose loses them — that is the deliberate
/// trade, measured in the [`subject_ticket_key`] doc.
///
/// Test: `tests::extract_ticket_id_*` and
/// `tests::adr_reference_is_not_a_ticket_key` below; also exercised by
/// `collect::git::extractor` tests that verify `ticket_id` is populated
/// at INSERT time during `tga collect`.
///
/// # Examples
///
/// ```
/// use tga::collect::ticket::extract_ticket_id;
///
/// assert_eq!(extract_ticket_id("BB-2746: refactor auth"), Some("BB-2746".to_string()));
/// assert_eq!(extract_ticket_id("SRE-3104: increase RDS timeout"), Some("SRE-3104".to_string()));
/// assert_eq!(extract_ticket_id("DRE-405 fix demand calculation"), Some("DRE-405".to_string()));
/// assert_eq!(extract_ticket_id("fixes #99"), Some("#99".to_string()));
/// assert_eq!(extract_ticket_id("misc cleanup"), None);
///
/// // #5199: a documentation citation is not a ticket key, and a subject-line
/// // issue reference is no longer overridden by one in the body.
/// assert_eq!(extract_ticket_id("docs: amend ADR-0034"), None);
/// assert_eq!(
///     extract_ticket_id("fix: crash on start\n\nSee ADR-0034.\nCloses #5089"),
///     Some("#5089".to_string())
/// );
/// ```
pub fn extract_ticket_id(message: &str) -> Option<String> {
    let p = extract_patterns();

    // ADO: AB#123 — most specific, checked first.
    if let Some(m) = p.azdo.find(message) {
        return Some(m.as_str().to_string());
    }

    // #5199: JIRA/Linear keys are read from the subject line only. Scanning
    // the whole message made every hyphenated uppercase token in the body a
    // ticket key.
    if let Some(key) = subject_ticket_key(message.lines().next().unwrap_or("")) {
        return Some(key);
    }

    // GitHub bare: #123 — the capture group strips the leading whitespace
    // that the pattern uses as a left-boundary guard.
    if let Some(caps) = p.gh_bare.captures(message) {
        if let Some(m) = caps.get(1) {
            return Some(m.as_str().to_string());
        }
    }

    None
}

/// The ticket key a branch NAME declares, if any.
///
/// Why: #5734 — `feature/PROJ-123-thing` names its work item as deliberately as
/// a commit subject does, and nothing harvested it. The rule has to be as tight
/// as #5199's, because a branch called `fix/ADR-0029-followup` is the same
/// shape as a real key: #5199 measured 466 false keys across 178 prefixes on
/// this repository's commit bodies, and re-admitting them through a branch name
/// would undo that work through a side door.
///
/// What: strips a leading `refs/heads/` (Azure DevOps reports full refs), then
/// tries each `/`-separated segment in order and returns the first key anchored
/// at that segment's START. The right boundary accepts `-` and `_` as well as
/// end-of-segment, because a branch slug runs the key into its description with
/// no space. [`DOC_REF_PREFIXES`] is applied unchanged, so `ADR`, `DOC`, `RFC`
/// and `SPEC` are rejected here exactly as they are in a commit subject.
/// Anchoring at `^` also keeps `SPEC-INSTALLER-01` from yielding
/// `INSTALLER-01`, the tail-segment case #5199 closed.
///
/// A wrong key here is contained the same way #5199's are:
/// [`crate::collect::correlate_commits`] only links a key that matches a row
/// already in `work_items`, so a bad key becomes a reported gap, never an
/// invented link.
///
/// Measured over this repository's 2376 merged pull requests, this yields
/// ZERO keys — branch names here are lowercase (`fix/5734-slug`). That is a
/// property of this repository's conventions, not of the extractor.
///
/// Test: `tests::branch_ticket_key_reads_the_key_a_branch_declares`,
/// `tests::branch_ticket_key_rejects_documentation_prefixes`.
///
/// # Examples
///
/// ```
/// use tga::collect::ticket::branch_ticket_key;
///
/// assert_eq!(branch_ticket_key("feature/PROJ-123-thing"), Some("PROJ-123".to_string()));
/// assert_eq!(branch_ticket_key("refs/heads/ENG-7"), Some("ENG-7".to_string()));
/// // #5199's rejection rules apply here unchanged.
/// assert_eq!(branch_ticket_key("fix/ADR-0029-followup"), None);
/// assert_eq!(branch_ticket_key("fix/5734-harvest-refs"), None);
/// ```
pub fn branch_ticket_key(branch: &str) -> Option<String> {
    let p = extract_patterns();
    let trimmed = branch
        .trim()
        .strip_prefix("refs/heads/")
        .unwrap_or(branch.trim());
    for segment in trimmed.split('/') {
        let Some(key) = p
            .jira_branch_segment
            .captures(segment)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
        else {
            continue;
        };
        // #5734: the same deny-list #5199 applied to a commit subject.
        if !is_doc_reference(&key) {
            return Some(key);
        }
    }
    None
}

/// The issue reference a pull-request BODY declares, if any.
///
/// Why: #5734 — a PR body routinely carries `Closes #N` for work the commit
/// subject never names, and none of it was harvested. What it must NOT do is
/// re-admit the noise #5199 removed: an unrestricted JIRA-shaped scan over this
/// repository's 2433 PR bodies returns 2501 matches across 423 distinct keys,
/// led by `DOC-39` (88), `DOC-48` (63) and `ADR-0024` (46) — five times the
/// false-key volume #5199 deleted from commit bodies. Gating that scan on an
/// action keyword does not rescue it either: `Closes <JIRA-key>` appears 8
/// times and 6 are `ADR-0038`, `ADR-0032`, `DOC-56`, `DOC-29`,
/// `RUSTSEC-2026` and a `HIGH-1` severity label.
///
/// What: reads ONLY an action-keyword GitHub reference — `closes #N`,
/// `fixes #N`, `resolves #N` and their tense variants — and returns the `#N`.
/// No JIRA/Linear pattern runs against a PR body at all, and a BARE `#N` is
/// rejected as well: bare refs fire on 147 further bodies here, which is the
/// #445 over-counting the bare pattern was excluded from [`is_ticketed`] for.
/// A body must name its issue with an action keyword to be believed.
///
/// Measured over this repository, this recovers a genuine issue reference for
/// 51 merged commits whose subject declares no key.
///
/// Test: `tests::pr_body_ticket_key_needs_an_action_keyword`,
/// `tests::pr_body_ticket_key_never_reads_a_jira_shaped_token`.
///
/// # Examples
///
/// ```
/// use tga::collect::ticket::pr_body_ticket_key;
///
/// assert_eq!(pr_body_ticket_key("Rework the guard.\n\nCloses #5734"), Some("#5734".to_string()));
/// // A bare mention is not a declaration.
/// assert_eq!(pr_body_ticket_key("Supersedes #123, see #456"), None);
/// // The #5199 noise class never reaches a key.
/// assert_eq!(pr_body_ticket_key("Per DOC-39 and ADR-0024, fixes UTF-8 handling"), None);
/// ```
pub fn pr_body_ticket_key(body: &str) -> Option<String> {
    extract_patterns()
        .gh_action_ref
        .captures(body)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_compile() {
        // Force lazy init; if any pattern is malformed this will panic.
        let _ = patterns();
    }

    #[test]
    fn extract_patterns_compile() {
        // Force lazy init; if any pattern is malformed this will panic.
        let _ = extract_patterns();
    }

    // ── extract_ticket_id: issue #316 sample commits ──────────────────────

    #[test]
    fn extract_ticket_id_bb_2746() {
        // Sample from issue #316 — was producing NULL ticket_id before fix.
        assert_eq!(
            extract_ticket_id("BB-2746: refactor auth service"),
            Some("BB-2746".to_string())
        );
    }

    #[test]
    fn extract_ticket_id_sre_3104() {
        // Sample from issue #316 — was producing NULL ticket_id before fix.
        assert_eq!(
            extract_ticket_id("SRE-3104: increase RDS connection timeout"),
            Some("SRE-3104".to_string())
        );
    }

    #[test]
    fn extract_ticket_id_dre_405() {
        // Sample from issue #316 — note: no colon separator, space only.
        assert_eq!(
            extract_ticket_id("DRE-405 fix demand calculation"),
            Some("DRE-405".to_string())
        );
    }

    #[test]
    fn extract_ticket_id_returns_none_for_plain_message() {
        assert_eq!(extract_ticket_id("misc cleanup"), None);
        assert_eq!(extract_ticket_id("update README"), None);
        assert_eq!(extract_ticket_id("bump version to 1.2.3"), None);
    }

    #[test]
    fn extract_ticket_id_github_bare_ref() {
        assert_eq!(extract_ticket_id("fixes #99"), Some("#99".to_string()));
    }

    #[test]
    fn extract_ticket_id_azdo_ref() {
        assert_eq!(
            extract_ticket_id("AB#42 implement feature"),
            Some("AB#42".to_string())
        );
    }

    #[test]
    fn extract_ticket_id_azdo_preferred_over_jira() {
        // When both AB# and JIRA patterns appear, ADO wins (more specific).
        assert_eq!(
            extract_ticket_id("AB#10 fixes PROJ-99"),
            Some("AB#10".to_string())
        );
    }

    #[test]
    fn extract_ticket_id_jira_preferred_over_gh_bare() {
        // JIRA is checked before bare GitHub ref.
        assert_eq!(
            extract_ticket_id("ENG-7 closes #10"),
            Some("ENG-7".to_string())
        );
    }

    /// Why: this test used to assert that `Relates to SRE-999` in a commit
    /// BODY yields `SRE-999`. That assertion encoded the #5199 defect — it is
    /// the exact shape that turned `UTF-8`, `SHA-256`, `RUSTSEC-2026` and
    /// `ADR-0034` into ticket keys. It is inverted here deliberately, not
    /// weakened: a body-line JIRA-shaped token is no longer a ticket key.
    /// What: the body key is ignored; a body `#N` is still the last resort.
    /// Test: this test itself.
    #[test]
    fn extract_ticket_id_multiline_body() {
        let msg = "Refactor module structure\n\nRelates to SRE-999.\n";
        assert_eq!(extract_ticket_id(msg), None);

        // A subject that declares the key still resolves from a multi-line
        // message — only the position changed, not the support for bodies.
        let anchored = "SRE-999 refactor module structure\n\nDetails here.\n";
        assert_eq!(extract_ticket_id(anchored), Some("SRE-999".to_string()));
    }

    /// Why: #5199 — `ADR-0029` and `PROJ-1234` are the same shape, so the
    /// unrestricted JIRA pattern read this repo's own architecture-decision
    /// citations as ticket keys and `correlate_commits` reported them as
    /// coverage gaps against tickets that exist on no board.
    /// What: an ADR citation yields no ticket key, and the commit falls
    /// through to the GitHub issue its subject or body actually names.
    /// Test: this test itself.
    #[test]
    fn adr_reference_is_not_a_ticket_key() {
        // Subject-leading ADR citation: not a ticket, and nothing else to fall
        // back to.
        assert_eq!(extract_ticket_id("docs(adr): ADR-0030 point 7"), None);
        // The #5199 reproduction: subject cites real issues, body cites an ADR.
        // Before the fix this returned Some("ADR-0034").
        let msg = "fix(relay): verify the HMAC once\n\nPer ADR-0034 the relay spools durably.\nCloses #5089\nCloses #5175\n";
        assert_eq!(extract_ticket_id(msg), Some("#5089".to_string()));
        // The other three documentation prefixes.
        assert_eq!(extract_ticket_id("docs: DOC-67 §2 rewrite"), None);
        assert_eq!(extract_ticket_id("RFC-2119 keyword sweep"), None);
        assert_eq!(extract_ticket_id("SPEC-14 tightened"), None);
    }

    /// Why: #5199 — the long tail of false keys was not documentation
    /// prefixes at all but ordinary technical identifiers in commit bodies. A
    /// deny-list can never enumerate them; anchoring to the subject removes
    /// them by construction.
    /// What: none of these body-prose identifiers becomes a ticket key.
    /// Test: this test itself.
    #[test]
    fn body_prose_identifier_is_not_a_ticket_key() {
        for body in [
            "fix: stem handling\n\nA filename containing a multi-byte UTF-8 character breaks.\n",
            "chore: pin the digest\n\nVerifies the artifact against the published SHA-256.\n",
            "chore: bump anydoc\n\nAvoids reintroducing RUSTSEC-2026-0187.\n",
            "build: probe the toolchain\n\nMirrors the AL2023/GCC-11 desync.\n",
            "docs: rate table\n\nGPT-5 and Gemini rates differ.\n",
            "fix: parse timestamps\n\nISO-8601 offsets were dropped.\n",
        ] {
            assert_eq!(extract_ticket_id(body), None, "message: {body:?}");
        }
    }

    /// Why: #5199 narrowed where a JIRA/Linear key is read from, so the real
    /// subject shapes #316 relied on must be proven still to resolve.
    /// What: bare, colon-separated, conventional-commit-prefixed and
    /// bracketed subject forms all yield the key.
    /// Test: this test itself.
    #[test]
    fn extract_ticket_id_subject_forms() {
        for (msg, want) in [
            ("BB-2746: refactor auth service", "BB-2746"),
            ("DRE-405 fix demand calculation", "DRE-405"),
            ("fix(auth): PROJ-12 tighten token check", "PROJ-12"),
            ("[ENG-77] add endpoint", "ENG-77"),
            ("feat!: API-9 breaking rename", "API-9"),
            ("SRE-3104", "SRE-3104"),
        ] {
            assert_eq!(extract_ticket_id(msg), Some(want.to_string()), "{msg:?}");
        }
    }

    /// Why: #5199 — a key that is the tail of a longer hyphenated identifier
    /// (`SPEC-INSTALLER-01`) used to surface as `INSTALLER-01`, which the
    /// `SPEC` deny-list alone would not have caught.
    /// What: anchoring at `^` makes the tail unreachable.
    /// Test: this test itself.
    #[test]
    fn hyphenated_tail_segment_is_not_a_ticket_key() {
        assert_eq!(extract_ticket_id("SPEC-INSTALLER-01 detect target"), None);
        assert_eq!(
            extract_ticket_id("docs: SPEC-MPM-CUTOVER-03 decision"),
            None
        );
    }

    #[test]
    fn jira_style_is_ticketed() {
        assert!(is_ticketed("ENG-123: add feature"));
        assert!(is_ticketed("PROJ-1 initial commit"));
        // #5735: `Backport from upstream (ABC-4567)` used to pass here. It is
        // now covered as a NEGATIVE in
        // `jira_shaped_prose_is_not_ticketed` — a mid-subject parenthetical is
        // the same position `UTF-8` and `ADR-0034` occupy.
        assert!(is_ticketed("[ABC-4567] backport from upstream"));
    }

    #[test]
    fn linear_style_is_ticketed() {
        // Linear identifiers are a subset of the JIRA pattern.
        assert!(is_ticketed("FE-456 fix login"));
        assert!(is_ticketed("API-9 add endpoint"));
    }

    #[test]
    fn github_action_keyword_is_ticketed() {
        assert!(is_ticketed("Fix race condition, fixes #123"));
        assert!(is_ticketed("closes #45"));
        assert!(is_ticketed("Resolves #7 by reworking auth"));
        assert!(is_ticketed("CLOSED #99")); // case-insensitive
    }

    /// Why: regression guard for issue #445. Bare `#N` refs no longer make a
    /// commit "ticketed" — only JIRA/Linear, GitHub action keywords, and ADO
    /// refs do. This test confirms bare refs are NOT ticketed while confirming
    /// they still produce a `ticket_id` via `extract_ticket_id`.
    /// What: asserts `is_ticketed` returns false for bare `#N`, and that
    /// action keywords and JIRA refs still return true.
    /// Test: this test itself.
    #[test]
    fn bare_hash_alone_is_not_ticketed() {
        // Bare #N with no action keyword is NOT ticketed (issue #445 fix).
        assert!(!is_ticketed("Bug from #123 still present"));
        assert!(!is_ticketed("#42 follow-up"));
        assert!(!is_ticketed("some note about #42"));
        // But the ticket_id is still extractable.
        assert_eq!(extract_ticket_id("#42 follow-up"), Some("#42".to_string()));
        assert_eq!(
            extract_ticket_id("Bug from #123 still present"),
            Some("#123".to_string())
        );
    }

    #[test]
    fn closes_hash_is_ticketed() {
        // Action keyword + bare ref IS ticketed.
        assert!(is_ticketed("closes #42"));
        assert!(is_ticketed("fixes #123"));
        assert!(is_ticketed("resolves #7"));
    }

    #[test]
    fn jira_is_ticketed() {
        assert!(is_ticketed("ENG-123: add feature"));
        assert!(is_ticketed("PROJ-1 initial commit"));
    }

    #[test]
    fn azdo_is_ticketed() {
        assert!(is_ticketed("AB#1234 implement new feature"));
        assert!(is_ticketed("Refactor module (AB#42)"));
    }

    #[test]
    fn plain_message_is_not_ticketed() {
        assert!(!is_ticketed("misc cleanup"));
        assert!(!is_ticketed("update README"));
        assert!(!is_ticketed("bump version to 1.2.3"));
        // Hex color shouldn't false-positive — `#abc123` is not preceded by
        // whitespace+digits-only.
        assert!(!is_ticketed("set color to #abc123"));
        // Lowercase project key is not a JIRA identifier.
        assert!(!is_ticketed("eng-123 lowercase doesn't count"));
    }

    #[test]
    fn multiline_body_with_ticket_is_ticketed() {
        // #5735: a JIRA ref in the BODY is no longer ticketed — that position
        // is where `UTF-8` and `ADR-0034` live. The subject still declares.
        let body_only = "Refactor module structure\n\nMoves things around.\nRelates to PROJ-789.\n";
        assert!(!is_ticketed(body_only));
        let msg = "PROJ-789: refactor module structure\n\nMoves things around.\n";
        assert!(is_ticketed(msg));

        // Bare #N in body is NOT ticketed (issue #445 fix); action keyword is.
        let msg2 = "First line no ticket\n\nSee #321 for context.";
        assert!(!is_ticketed(msg2));

        let msg3 = "First line no ticket\n\nCloses #321.";
        assert!(is_ticketed(msg3));
    }

    #[test]
    fn azdo_ab_ref_is_ticketed() {
        assert!(is_ticketed("AB#1234 implement new feature"));
        assert!(is_ticketed("Refactor module (AB#42)"));
        assert!(is_ticketed("First line\n\nbody mentions AB#7 explicitly"));
    }

    #[test]
    fn bare_hash_without_ab_prefix_is_not_azdo() {
        // GitHub bare `#N` still matches via the gh_bare pattern, but it
        // must NOT match the ADO `AB#` pattern specifically.
        let p = patterns();
        assert!(!p.azdo.is_match("#1234 some work"));
        assert!(!p.azdo.is_match("fixes #99"));
        // And the JIRA route must not fire on AB#N either (different
        // separator: `#` vs `-`), so `AB#1234` is ticketed only as ADO.
        assert_eq!(subject_ticket_key("AB#1234"), None);
    }

    /// Why: #5735 — `is_ticketed` ran an unrestricted `\b[A-Z][A-Z0-9]*-\d+\b`
    /// over the whole message, so a `UTF-8` or `ADR-0034` in a body marked the
    /// commit ticketed. 237 of this repository's 2633 commits were counted on
    /// nothing else. `commits.ticketed` is the operator-facing quality metric
    /// #445 tuned deliberately, so the inflation lands in a reported number.
    /// What: every one of these is a JIRA-SHAPED string sitting where prose
    /// sits — a body line, a mid-subject parenthetical, a tail segment — and
    /// none of them makes a commit ticketed. The last two are the documentation
    /// prefixes [`DOC_REF_PREFIXES`] rejects even in declaring position.
    /// Test: this test itself.
    #[test]
    fn jira_shaped_prose_is_not_ticketed() {
        for msg in [
            // The two cases #5735 names.
            "fix: stem handling\n\nA filename containing a multi-byte UTF-8 character breaks.\n",
            "docs: amend ADR-0034 point 7",
            // The rest of the measured long tail, in body position.
            "chore: pin the digest\n\nVerifies the artifact against the published SHA-256.\n",
            "chore: bump anydoc\n\nAvoids reintroducing RUSTSEC-2026-0187.\n",
            "build: probe the toolchain\n\nMirrors the AL2023/GCC-11 desync.\n",
            "docs: rate table\n\nGPT-5 and Gemini rates differ.\n",
            "fix: parse timestamps\n\nISO-8601 offsets were dropped.\n",
            "docs: manifest\n\nPer DOC-39 the manifest is an allowlist.\n",
            "chore: triage\n\nResolves the HIGH-1 finding from the audit.\n",
            // A mid-subject parenthetical is the same position as prose.
            "Backport from upstream (ABC-4567)",
            // A tail segment of a longer identifier stays unreachable.
            "SPEC-INSTALLER-01 detect target",
            // Declaring position, documentation prefix: still not a ticket.
            "DOC-67 §2 rewrite",
            "RFC-2119 keyword sweep",
        ] {
            assert!(!is_ticketed(msg), "message: {msg:?}");
        }
    }

    /// Why: #5735 narrowed where a JIRA/Linear key counts, so the subject
    /// shapes that DO declare one must be proven still to count — a fix that
    /// zeroed the metric would be as wrong as the inflation it replaced.
    /// What: `is_ticketed` agrees with `extract_ticket_id` on every subject
    /// form, and the GitHub and Azure DevOps routes are untouched by position.
    /// Test: this test itself.
    #[test]
    fn subject_declared_key_is_still_ticketed() {
        for msg in [
            "BB-2746: refactor auth service",
            "DRE-405 fix demand calculation",
            "fix(auth): PROJ-12 tighten token check",
            "[ENG-77] add endpoint",
            "feat!: API-9 breaking rename",
            "SRE-3104",
            "ENG-7 refactor\n\nBody mentions UTF-8 and ADR-0034.\n",
        ] {
            assert!(is_ticketed(msg), "message: {msg:?}");
            assert!(extract_ticket_id(msg).is_some(), "message: {msg:?}");
        }
        // Position never mattered for these two routes and still does not.
        assert!(is_ticketed("Rework the guard\n\nCloses #5735\n"));
        assert!(is_ticketed(
            "Refactor module\n\nbody mentions AB#7 explicitly"
        ));
    }

    #[test]
    fn empty_message_is_not_ticketed() {
        assert!(!is_ticketed(""));
        assert!(!is_ticketed("\n\n"));
    }

    // ── #5734: branch names ───────────────────────────────────────────────

    /// Why: #5734 — the branch shapes a real workflow produces must all
    /// resolve, whichever `/`-separated segment carries the key.
    /// What: bare, prefixed, nested and full-ref forms yield the key.
    /// Test: this test itself.
    #[test]
    fn branch_ticket_key_reads_the_key_a_branch_declares() {
        for (branch, want) in [
            ("PROJ-123", "PROJ-123"),
            ("PROJ-123-add-thing", "PROJ-123"),
            ("feature/PROJ-123-thing", "PROJ-123"),
            ("feature/ENG-7_login", "ENG-7"),
            ("bobmatnyc/feature/API-9", "API-9"),
            ("refs/heads/feature/BB-2746-auth", "BB-2746"),
            ("refs/heads/SRE-3104", "SRE-3104"),
        ] {
            assert_eq!(
                branch_ticket_key(branch),
                Some(want.to_string()),
                "branch: {branch:?}"
            );
        }
    }

    /// Why: #5734's central risk — a branch named `fix/ADR-0029-followup` is
    /// exactly the shape #5199 spent its effort excluding, and harvesting one
    /// would reintroduce that noise through a side door.
    /// What: every [`DOC_REF_PREFIXES`] entry is rejected in a branch name, the
    /// hyphenated-tail case stays unreachable, and a lowercase or
    /// numeric-leading branch yields nothing.
    /// Test: this test itself.
    #[test]
    fn branch_ticket_key_rejects_documentation_prefixes() {
        for branch in [
            "fix/ADR-0029-followup",
            "docs/DOC-67-rewrite",
            "chore/RFC-2119-sweep",
            "feat/SPEC-14-tightened",
            "refs/heads/ADR-0034",
            // The tail of a longer identifier is unreachable, as in a subject.
            "feat/SPEC-INSTALLER-01-detect",
            // This repository's own convention: lowercase type, numeric slug.
            "fix/5734-harvest-branch-and-pr-refs",
            "feat/pre-publish-sha-targeting-5740",
            "main",
            "",
        ] {
            assert_eq!(branch_ticket_key(branch), None, "branch: {branch:?}");
        }
    }

    // ── #5734: pull-request bodies ────────────────────────────────────────

    /// Why: #5734 — a PR body's `Closes #N` is the reference worth harvesting;
    /// a bare mention is the #445 over-counting that fires on 147 further
    /// bodies in this repository alone.
    /// What: action-keyword forms and their tense variants yield the `#N`;
    /// bare mentions yield nothing.
    /// Test: this test itself.
    #[test]
    fn pr_body_ticket_key_needs_an_action_keyword() {
        for (body, want) in [
            ("Closes #5734", "#5734"),
            ("Rework the guard.\n\nCloses #5734\n", "#5734"),
            ("fixes #99 and tidies up", "#99"),
            ("RESOLVED #7", "#7"),
            ("This closed #42 at last", "#42"),
        ] {
            assert_eq!(
                pr_body_ticket_key(body),
                Some(want.to_string()),
                "body: {body:?}"
            );
        }
        for body in [
            "Supersedes #123, see #456",
            "Follow-up to #5199.",
            "#5734 is the tracking issue",
            "no reference at all",
            "",
        ] {
            assert_eq!(pr_body_ticket_key(body), None, "body: {body:?}");
        }
    }

    /// Why: #5734 — the measured reason no JIRA/Linear pattern runs over a PR
    /// body. An unrestricted scan of this repository's 2433 bodies returns
    /// 2501 matches across 423 keys led by `DOC-39`, `DOC-48` and `ADR-0024`;
    /// gating on an action keyword still leaves `RUSTSEC-2026` and a `HIGH-1`
    /// severity label. Both routes are closed, not filtered.
    /// What: none of these bodies yields a JIRA-shaped key, including the ones
    /// where an action keyword sits directly in front of it.
    /// Test: this test itself.
    #[test]
    fn pr_body_ticket_key_never_reads_a_jira_shaped_token() {
        for body in [
            "Per DOC-39 the manifest is an allowlist.",
            "Implements ADR-0024 and DOC-48.",
            "fixes UTF-8 filename handling",
            "closes RUSTSEC-2026-0187",
            "resolves HIGH-1 from the audit",
            "Fixes PROJ-123 in the tracker",
            "SHA-256 digest pinned; ISO-8601 offsets kept",
        ] {
            assert_eq!(pr_body_ticket_key(body), None, "body: {body:?}");
        }
        // A body that names BOTH still yields only the GitHub reference.
        assert_eq!(
            pr_body_ticket_key("Per ADR-0034 the relay spools durably.\n\nCloses #5089\n"),
            Some("#5089".to_string())
        );
    }
}

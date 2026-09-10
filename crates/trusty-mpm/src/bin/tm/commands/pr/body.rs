//! The PR-body contract `tm pr open` enforces before spawning `gh` (#6653).
//!
//! Why: `tm-workflow.md`'s "Minimal PR Body (seven fields)" section is the
//! repo's PR-body standard, and it is enforced today only by a reviewer
//! noticing a thin body after the PR is already open. Its seven numbered
//! fields are, verbatim from that section:
//!
//!   1. Primary outcome and linked issue(s), with the `Refs owner/repo#N` link.
//!   2. What changed, and what is intentionally out of scope.
//!   3. Risk / blast radius.
//!   4. Test evidence at the applicable levels.
//!   5. Baseline/pre-existing failures and their canonical issue.
//!   6. Documentation/changelog status.
//!   7. Review-finding disposition: fixed here, kept on the parent, or
//!      separately ticketed.
//!
//! The same section fixes the attribution footer
//! (`🤖🤖🤖 Generated with trusty-mpm — …`) as the body's exact ending. Both
//! are string-shaped facts about a file, so both are checkable.
//!
//! What: [`Field`] names the seven, [`validate`] reports which are missing or
//! empty and whether the body ends with the attribution block — the footer,
//! optionally followed by the harness's session link (#7297) — and
//! [`apply_issue_link`] resolves this repo's `Refs #N` / `Closes #N` rule
//! (root `CLAUDE.md`, "Key Conventions": fix PRs use `Refs #N`, **never**
//! `Closes #N`).
//! Test: the sibling `tests.rs` — `body_*`, `footer_*`, `issue_link_*`.

// #6807: one definition of the footer, shared with the settings seed that
// writes Claude Code's `attribution` key — this validator must hold a body to
// the same text the setting produces.
pub(crate) use trusty_mpm::core::attribution::ATTRIBUTION_FOOTER;

/// One of the seven required PR-body fields.
///
/// Why: naming each field lets a failure report say WHICH one is missing
/// rather than "the body is incomplete", which is the whole point of moving
/// this off the reviewer.
/// What: the seven fields of `tm-workflow.md`'s "Minimal PR Body" section, in
/// its own order. [`Field::heading`] is the canonical heading; the aliases in
/// [`Field::matches`] accept the wordings a human body actually uses.
/// Test: `body_field_table_covers_seven`, `body_accepts_alias_headings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Field {
    /// 1. Primary outcome and linked issue(s).
    Outcome,
    /// 2. What changed, and what is intentionally out of scope.
    Changes,
    /// 3. Risk / blast radius.
    Risk,
    /// 4. Test evidence at the applicable levels.
    Tests,
    /// 5. Baseline/pre-existing failures and their canonical issue.
    Baseline,
    /// 6. Documentation/changelog status.
    Docs,
    /// 7. Review-finding disposition.
    Review,
}

/// The seven fields, in `tm-workflow.md`'s own order.
pub(crate) const FIELDS: [Field; 7] = [
    Field::Outcome,
    Field::Changes,
    Field::Risk,
    Field::Tests,
    Field::Baseline,
    Field::Docs,
    Field::Review,
];

impl Field {
    /// The canonical heading text this field is written under.
    pub(crate) fn heading(self) -> &'static str {
        match self {
            Self::Outcome => "Outcome",
            Self::Changes => "Changes",
            Self::Risk => "Risk",
            Self::Tests => "Tests",
            Self::Baseline => "Baseline",
            Self::Docs => "Docs",
            Self::Review => "Review",
        }
    }

    /// Accepted heading phrases, lowercase, matched as substrings.
    ///
    /// Why: a body written by hand says "Test evidence" or "Risk / blast
    /// radius", not the one-word canonical key. Rejecting those would push
    /// authors to satisfy the tool rather than the contract.
    /// What: the canonical key plus the wordings `tm-workflow.md` itself uses.
    /// Test: `body_accepts_alias_headings`.
    fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::Outcome => &["outcome", "summary", "primary outcome"],
            Self::Changes => &["changes", "what changed", "scope"],
            Self::Risk => &["risk", "blast radius"],
            Self::Tests => &["tests", "test evidence", "testing", "test plan"],
            Self::Baseline => &["baseline", "pre-existing", "preexisting"],
            Self::Docs => &["docs", "documentation", "changelog"],
            Self::Review => &["review"],
        }
    }

    /// Does `heading_text` (already normalized) name this field?
    fn matches(self, heading_text: &str) -> bool {
        self.aliases().iter().any(|a| heading_text.contains(a))
    }
}

/// What [`validate`] found wrong with a body.
///
/// Why: `tm pr open` must print WHICH check failed and exit 2 without calling
/// `gh`, so the failure has to be a value, not a formatted string.
/// What: the fields with no heading, the fields whose section held no content,
/// and the footer verdict.
/// Test: `body_reports_each_missing_field`, `body_reports_empty_section`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct BodyReport {
    /// Fields with no matching heading anywhere in the body.
    pub(crate) missing: Vec<Field>,
    /// Fields whose heading exists but whose section body is blank.
    pub(crate) empty: Vec<Field>,
    /// Whether the body ends with the attribution block — see [`footer_closes`].
    pub(crate) footer_ok: bool,
    /// Fields that were present AND non-empty, in contract order.
    pub(crate) supplied: Vec<Field>,
}

impl BodyReport {
    /// The failed checks, one human line each. Empty means the body is good.
    pub(crate) fn failures(&self) -> Vec<String> {
        let mut out = Vec::new();
        for f in &self.missing {
            out.push(format!(
                "missing required body field {} (heading `## {}`)",
                field_index(*f),
                f.heading()
            ));
        }
        for f in &self.empty {
            out.push(format!(
                "body field {} (`## {}`) has no content",
                field_index(*f),
                f.heading()
            ));
        }
        if !self.footer_ok {
            out.push(format!(
                "body does not end with the attribution footer `{ATTRIBUTION_FOOTER}` \
                 (a single trailing `{SESSION_LINK_PREFIX}…` line after it is allowed)"
            ));
        }
        out
    }
}

/// The field's 1-based position in the contract, for messages.
fn field_index(f: Field) -> usize {
    FIELDS.iter().position(|c| *c == f).unwrap_or(0) + 1
}

/// Normalize a heading line to its comparable text.
///
/// Why: headings arrive as `## 4. Test evidence` / `### RISK / blast radius`,
/// and the contract is about the words, not the punctuation or the numbering.
/// What: strips leading `#`, digits, `.`/`)`/`:` and `*`/`_` emphasis, then
/// lowercases. Returns `None` for a line that is not an ATX heading.
/// Test: `heading_text_strips_numbering_and_emphasis`.
fn heading_text(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix('#')?;
    let rest = rest.trim_start_matches('#').trim();
    let cleaned: String = rest
        .chars()
        .map(|c| if c == '*' || c == '_' { ' ' } else { c })
        .collect();
    let cleaned = cleaned
        .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')' || c == ' ')
        .trim()
        .to_ascii_lowercase();
    Some(cleaned)
}

/// Check `body` against the seven-field contract and the footer rule.
///
/// Why: this is the gate `tm pr open` runs before spawning `gh`, so a thin
/// body is caught by the author rather than by the review gate after the PR
/// is already open and labelled.
/// What: walks the body once. Each ATX heading claims the FIRST contract field
/// it matches that is not already claimed, so `## Docs / changelog` cannot
/// also satisfy `## Review`. A field's section is its lines up to the next
/// heading; no line of the attribution block — the footer, and the session link
/// the harness pairs with it — counts as section content, so a section holding
/// only those is reported empty. The footer must be the last non-blank line,
/// or the last one above a single trailing session link (#7297).
/// Test: `body_accepts_a_complete_body`, `body_reports_each_missing_field`,
/// `body_reports_empty_section`, `footer_must_be_last_line`,
/// `footer_alone_does_not_fill_a_section`,
/// `footer_accepts_the_trailing_session_link`,
/// `footer_accepts_a_session_link_before_the_footer`,
/// `footer_rejects_a_body_with_no_attribution_line`.
pub(crate) fn validate(body: &str) -> BodyReport {
    // (field, heading seen, section held content) — one row per contract field.
    let mut seen: Vec<(Field, bool, bool)> = FIELDS.iter().map(|f| (*f, false, false)).collect();
    let mut current: Option<usize> = None;

    for line in body.lines() {
        if let Some(text) = heading_text(line) {
            current = seen
                .iter()
                .position(|(f, claimed, _)| !*claimed && f.matches(&text));
            if let Some(idx) = current {
                seen[idx].1 = true;
            }
            continue;
        }
        let trimmed = line.trim();
        // #7297: the session link is part of the attribution block, so like the
        // footer it can never be what fills a section.
        if trimmed.is_empty() || trimmed == ATTRIBUTION_FOOTER || is_session_link(trimmed) {
            continue;
        }
        if let Some(idx) = current {
            seen[idx].2 = true;
        }
    }

    let footer_ok = footer_closes(body);

    let mut report = BodyReport {
        footer_ok,
        ..BodyReport::default()
    };
    for (field, claimed, filled) in seen {
        match (claimed, filled) {
            (false, _) => report.missing.push(field),
            (true, false) => report.empty.push(field),
            (true, true) => report.supplied.push(field),
        }
    }
    report
}

/// The Claude Code session link the harness pairs with the footer.
///
/// Why (#7297): Claude Code's provisioned attribution instructs a session to
/// end a PR body with the footer, a blank line, then this link. The validator
/// required the footer to be the LAST non-blank line, so a session that
/// followed its own instructions could not open a PR with `tm pr open` and fell
/// back to `gh pr create` — which skips the seven-field gate entirely.
/// What: the URL prefix, matched with or without the `Claude-Session:` label
/// the commit-message form of the same block uses.
/// Test: `footer_accepts_the_trailing_session_link`,
/// `footer_accepts_a_session_link_before_the_footer`.
const SESSION_LINK_PREFIX: &str = "https://claude.ai/code/session_";

/// Label the commit-message form of the session line carries.
const SESSION_LINE_LABEL: &str = "Claude-Session:";

/// Is `line` a session link belonging to the attribution block?
///
/// Why: see [`SESSION_LINK_PREFIX`]. A prefix test alone accepted anything that
/// merely STARTED with the URL, so a line of prose trailing the link — or an
/// entire sentence after it — passed as attribution and could close a body or
/// be skipped as section content.
/// What: the whole line, after an optional [`SESSION_LINE_LABEL`] and the
/// whitespace behind it, must be the prefix plus a non-empty session id of
/// ASCII alphanumerics, `_` and `-`. Anything else — an empty id, a space, a
/// trailing word, punctuation — is not a session link.
/// Test: `footer_accepts_the_trailing_session_link`,
/// `footer_accepts_a_session_link_before_the_footer`,
/// `footer_rejects_a_session_link_with_trailing_junk`.
fn is_session_link(line: &str) -> bool {
    let text = line.trim();
    let url = text
        .strip_prefix(SESSION_LINE_LABEL)
        .map_or(text, str::trim_start);
    let Some(id) = url.strip_prefix(SESSION_LINK_PREFIX) else {
        return false;
    };
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Does `body` end with the attribution block?
///
/// Why: the block is the footer, optionally followed by the session link — both
/// orderings are in live use, because agents blocked by the old rule moved the
/// link ABOVE the footer to get a PR open (#7297). Accepting both keeps those
/// bodies valid while letting a session follow its own attribution instruction.
/// What: true when the last non-blank line is [`ATTRIBUTION_FOOTER`], or when it
/// is a single session link whose preceding non-blank line is that footer. A
/// body with no footer at all is false, whatever else it ends with.
/// Test: `footer_must_be_last_line`, `footer_accepts_the_trailing_session_link`,
/// `footer_accepts_a_session_link_before_the_footer`,
/// `footer_rejects_a_body_with_no_attribution_line`,
/// `footer_rejects_a_session_link_with_trailing_junk`,
/// `footer_rejects_two_stacked_session_links`.
fn footer_closes(body: &str) -> bool {
    let mut tail = body.lines().rev().filter(|l| !l.trim().is_empty());
    match tail.next() {
        Some(last) if last.trim() == ATTRIBUTION_FOOTER => true,
        Some(last) if is_session_link(last) => {
            tail.next().is_some_and(|l| l.trim() == ATTRIBUTION_FOOTER)
        }
        _ => false,
    }
}

/// How an issue reference must appear in the body.
///
/// Why: root `CLAUDE.md` is explicit — fix PRs use `Refs #N`, **never**
/// `Closes #N`, because a merge must not auto-close the issue before live
/// verification. Opting into `Closes` has to be a deliberate flag, and a body
/// that smuggles a `Closes #N` past a caller who did not pass it is the exact
/// mistake the rule exists to stop.
/// What: `Refs` by default, `Closes` only under `--closes`.
/// Test: `issue_link_defaults_to_refs`, `issue_link_closes_is_opt_in`,
/// `issue_link_rejects_unrequested_closes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IssueLink {
    /// `Refs #N` — the default; merge does not auto-close.
    Refs,
    /// `Closes #N` — opt-in via `--closes`.
    Closes,
}

impl IssueLink {
    /// The keyword this link renders with.
    pub(crate) fn keyword(self) -> &'static str {
        match self {
            Self::Refs => "Refs",
            Self::Closes => "Closes",
        }
    }
}

/// Insert the issue link into `body`, or report why it cannot be.
///
/// Why: the link is field 1's obligation and the `Refs`/`Closes` choice is a
/// hard project rule, so `tm pr open` owns both rather than trusting the body
/// file to have got them right.
/// What: with no `--issue`, a body carrying an unrequested closing keyword is
/// rejected. With `--issue N`, a body already carrying the correct
/// `<keyword> #N` line is left alone; otherwise the line is inserted
/// immediately above the attribution footer. Any closing keyword present when
/// `--closes` was NOT passed is always an error, whatever `--issue` says.
/// Test: `issue_link_defaults_to_refs`, `issue_link_is_idempotent`,
/// `issue_link_rejects_unrequested_closes`,
/// `issue_link_rejects_every_closing_keyword`,
/// `issue_link_rejects_a_closing_keyword_outside_field_one`,
/// `issue_link_rejects_a_negated_closing_keyword`,
/// `issue_link_allows_prose_that_only_looks_like_a_keyword`,
/// `issue_link_without_issue_leaves_body_alone`,
/// `issue_link_lands_above_a_footer_a_session_link_trails`.
pub(crate) fn apply_issue_link(
    body: &str,
    issue: Option<u64>,
    link: IssueLink,
) -> Result<String, String> {
    if link == IssueLink::Refs
        && let Some(found) = closing_reference(body)
    {
        return Err(format!(
            "body contains the closing keyword `{found}` but `--closes` was not passed; this \
             repo's fix PRs use `Refs #N` so a merge does not auto-close the issue before live \
             verification"
        ));
    }
    let Some(n) = issue else {
        return Ok(body.to_string());
    };
    let wanted = format!("{} #{n}", link.keyword());
    if body.lines().any(|l| l.trim() == wanted) {
        return Ok(body.to_string());
    }

    let mut lines: Vec<&str> = body.lines().collect();
    let insert_at = lines
        .iter()
        .rposition(|l| l.trim() == ATTRIBUTION_FOOTER)
        .unwrap_or(lines.len());
    let block = [wanted.as_str(), ""];
    for (offset, text) in block.iter().enumerate() {
        lines.insert(insert_at + offset, text);
    }
    let mut out = lines.join("\n");
    if body.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// GitHub's closing keywords, lowercased.
const CLOSING_KEYWORDS: [&str; 9] = [
    "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
];

/// The first closing keyword + issue reference in `body`, if any.
///
/// Why: GitHub scans the whole body, not just field 1, and honours a keyword
/// inside a negation, so `Fixes #N` anywhere auto-closes the issue on
/// squash-merge before live verification. Matching only a line that STARTS
/// with `closes ` let PR #6894 close #6888 off a `Fixes #6888` in field 1 —
/// see #6895, and #5389 for the negation case.
/// What: scans each line token by token for a keyword whose next token is an
/// issue reference (`#12` or `owner/repo#12`), mirroring GitHub's grammar, and
/// returns the matched pair so the error can quote it.
/// Test: `issue_link_rejects_unrequested_closes`,
/// `issue_link_rejects_every_closing_keyword`,
/// `issue_link_rejects_a_closing_keyword_outside_field_one`,
/// `issue_link_rejects_a_negated_closing_keyword`,
/// `issue_link_allows_prose_that_only_looks_like_a_keyword`.
fn closing_reference(body: &str) -> Option<String> {
    for line in body.lines() {
        let lowered = line.to_ascii_lowercase();
        let words: Vec<&str> = lowered.split_whitespace().collect();
        for pair in words.windows(2) {
            let keyword = pair[0].trim_matches(|c: char| !c.is_ascii_alphanumeric());
            if CLOSING_KEYWORDS.contains(&keyword) && is_issue_reference(pair[1]) {
                return Some(format!("{keyword} {}", pair[1]));
            }
        }
    }
    None
}

/// Is `token` a `#12` / `owner/repo#12` issue reference, trailing punctuation
/// and all?
fn is_issue_reference(token: &str) -> bool {
    let Some((_, number)) = token.split_once('#') else {
        return false;
    };
    let digits = number.trim_end_matches(|c: char| !c.is_ascii_digit());
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

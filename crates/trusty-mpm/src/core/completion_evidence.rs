//! The completion-standard check behind `tm hook --completion-evidence` (#10575).
//!
//! Why: a document task was reported done after one table row was deleted,
//! and a later invented word-count target cut rules the owner needed. The
//! completion standard asks a done claim to carry an evidence map whose rows
//! sort every item into verified / helper-reported / unfinished / untested,
//! and bans a size target the owner did not ask for. Nothing measured either.
//! What: [`warnings_for`] reads one final message and returns warning lines:
//!
//! * `MISSING_EVIDENCE_MAP`: the message claims completion and has no
//!   `Evidence map` heading or label.
//! * `EVIDENCE_MAP_INCOMPLETE`: the map names fewer than all four categories.
//! * `INVENTED_TARGET`: the message sets a size target (`cut to 600 words`)
//!   with no `OWNER-REQUESTED-LIMIT:` line (owner ruling Dr8 C: a warning first).
//! * `OWNER-REQUESTED-LIMIT`: such a line carries no quoted owner text.
//!
//! A negated claim (`not finished`, `not done yet`) is not a completion claim,
//! so an honest incomplete report draws no warning. Text inside fenced blocks
//! is ignored. Warn-only: the caller never blocks.
//! Test: `completion_evidence_tests.rs`.

use std::sync::LazyLock;

use regex::Regex;

/// The four evidence-map categories, as the completion standard names them.
pub const CATEGORIES: [&str; 4] = ["verified", "helper-reported", "unfinished", "untested"];

/// The label that records an owner's own size limit.
pub const OWNER_LIMIT_LABEL: &str = "OWNER-REQUESTED-LIMIT";

static CLAIM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        // #10575: `n't` carries no leading `\b` ("isn't done"), and `yet` may sit
        // between the negation and the claim ("not yet done").
        r"(?i)(\b(?:not|never)\s+(?:yet\s+)?|n't\s+(?:yet\s+)?)?\b(done|complete|completed|finished|verified working|ready to merge|all set)\b",
    )
    .expect("claim regex")
});

static MAP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?im)^\s*(#+\s*|\*\*)?evidence map\b").expect("map regex"));

static TARGET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(cut|trim|shorten|reduce|limit|cap|keep|under|within|at most|max(imum)?)\b[^\n]{0,60}?\b\d[\d,]*\s*(%|(words?|lines?|pages?|characters?|chars|tokens?|percent)\b)",
    )
    .expect("target regex")
});

static OWNER_LIMIT: LazyLock<Regex> = LazyLock::new(|| {
    // #10575: markdown bold may wrap the label, before or after the colon.
    Regex::new(r#"(?m)\**OWNER-REQUESTED-LIMIT\**:\**\s*(.*)$"#).expect("owner-limit regex")
});

/// One whole-word pattern per entry of [`CATEGORIES`]; `unverified` is not `verified`.
static CATEGORY_WORDS: LazyLock<[Regex; 4]> = LazyLock::new(|| {
    [
        r"\bverified\b",
        r"\bhelper[- ]reported\b",
        r"\bunfinished\b",
        r"\buntested\b",
    ]
    .map(|p| Regex::new(p).expect("category regex"))
});

/// The prose of `message`, fenced blocks removed.
fn prose(message: &str) -> String {
    let mut inside = false;
    let mut out = String::new();
    for line in message.lines() {
        if line.trim_start().starts_with("```") {
            inside = !inside;
            continue;
        }
        if !inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Whether `text` claims completion: a claim word not preceded by a negation.
///
/// Test: `a_negated_claim_is_not_a_completion_claim`.
pub fn claims_completion(text: &str) -> bool {
    CLAIM.captures_iter(text).any(|c| c.get(1).is_none())
}

/// Every warning line for `message`; empty when it meets the standard.
///
/// Test: `a_done_claim_without_a_map_warns`, `a_full_map_draws_no_warning`,
/// `a_map_missing_a_category_warns`, `an_invented_target_warns`,
/// `an_owner_quoted_limit_passes`, `an_owner_limit_without_a_quote_warns`.
pub fn warnings_for(message: &str) -> Vec<String> {
    let text = prose(message);
    let mut out = Vec::new();
    if claims_completion(&text) {
        match MAP.find(&text) {
            None => out.push(
                "MISSING_EVIDENCE_MAP: the message claims completion but carries no \
                 `Evidence map` (rows under verified / helper-reported / unfinished / untested)"
                    .to_string(),
            ),
            Some(m) => {
                let lowered = text[m.start()..].to_lowercase();
                let missing: Vec<&str> = CATEGORIES
                    .iter()
                    .zip(CATEGORY_WORDS.iter())
                    .filter(|(_, word)| !word.is_match(&lowered))
                    .map(|(c, _)| *c)
                    .collect();
                if !missing.is_empty() {
                    out.push(format!(
                        "EVIDENCE_MAP_INCOMPLETE: the evidence map names no {} category",
                        missing.join(" / ")
                    ));
                }
            }
        }
    }
    let owner_lines: Vec<String> = OWNER_LIMIT
        .captures_iter(&text)
        // #10575: a sentence-ending `.` after the closing quote is allowed.
        .map(|c| c[1].trim().trim_end_matches('.').trim_end().to_string())
        .collect();
    for quote in &owner_lines {
        if !(quote.len() > 2 && quote.starts_with('"') && quote.ends_with('"')) {
            out.push(format!(
                "{OWNER_LIMIT_LABEL}: the line carries no quoted owner text; quote the owner's \
                 own words from the owner-decisions record"
            ));
        }
    }
    if owner_lines.is_empty()
        && let Some(m) = TARGET.find(&text)
    {
        out.push(format!(
            "INVENTED_TARGET: `{}` sets a size target with no `{OWNER_LIMIT_LABEL}: \"<owner \
             quote>\"` line; a size target the owner did not ask for is not a criterion",
            m.as_str().trim()
        ));
    }
    out
}

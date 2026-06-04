//! Universal best-practices principles layer (#756).
//!
//! Why: research into authoritative code-review guidance (Google Engineering
//! Practices, Bacchelli & Bird ICSE 2013, Sadowski et al. ICSE-SEIP 2018,
//! SmartBear/Cisco study, Conventional Comments) distilled into a reusable
//! injectable prompt component.  This layer captures WHAT the field considers
//! effective code review — distinct from HOW the Duetto team reviews (the voice
//! layer).  It is composed BETWEEN the stock base prompt and the voice layer:
//! stock → principles → voice.
//!
//! What: exposes a single constant `PRINCIPLES_ADDENDUM` (the prompt text) and
//! `principles_addendum()` (the accessor function) so callers remain
//! source-agnostic to whether this is a constant or loaded at runtime in the
//! future.
//!
//! Sources (cited in research/best-practices/code-review-best-practices-2026-06-04.md):
//!   Google Eng Practices — Reviewer Guide (https://google.github.io/eng-practices/review/reviewer/)
//!   Conventional Comments (https://conventionalcomments.org/)
//!   Bacchelli & Bird, ICSE 2013 (https://dl.acm.org/doi/10.5555/2486788.2486882)
//!   Sadowski et al., ICSE-SEIP 2018 (https://dl.acm.org/doi/10.1145/3183519.3183525)
//!   SmartBear/Cisco study (review size, latency, defect density)
//!   Automated Code Review In Practice, arXiv 2412.18531
//!
//! Test: `principles_addendum_is_non_empty`, `principles_contains_key_concepts`
//! in voice/tests.rs.

/// The universal best-practices principles layer injected between the stock
/// base prompt and any voice package addendum.
const PRINCIPLES_ADDENDUM: &str = r#"## Review principles

Review changes in priority order: design and correctness first, then tests,
then complexity, then naming and clarity, then style last. Approve any change
that genuinely improves the codebase even if it is not perfect; never approve
one that worsens it.

**Severity — be explicit, be sparse with blocks.**
BLOCK only on real correctness defects, security issues, data-integrity risk,
or a broken build/tests. ADVISE on design, reuse, naming, and coverage — these
are strong recommendations, not mandates. DEMOTE pure style to nits labeled as
such. Use Conventional Comments labels (issue:, suggestion:, nitpick:,
question:, thought:, praise:, todo:) so authors know the weight of every
comment without asking.

**Feedback — actionable, reasoned, respectful.**
Comment on the code, not the author. Explain *why* every finding matters; a
conclusion without reasoning reads as personal opinion. Propose the concrete
fix when you can. When uncertain, ask a question rather than issuing a mandate.
Leave at least one `praise:` per review — genuine recognition accelerates
learning and sustains a healthy review culture. Avoid the words "simply,"
"just," "obviously"; do not use sarcasm or hyperbole. Request that unclear code
be rewritten in the source — explanations in review comments do not help future
readers.

**Calibration — keep signal-to-noise high.**
Limit the number of comments: a review flooded with low-severity nits trains
authors to ignore the feed entirely, burying real issues. For a style pattern
appearing many times, one comment noting the pattern is enough. Never block on
preference when the author's approach is valid; if multiple approaches are
equally sound, defer to the author.

**Scope discipline.**
Separate in-scope fixes from out-of-scope cleanup; defer the latter to a
tracked follow-up rather than expanding the PR. Do not approve "I'll fix it
later" promises for real correctness or complexity problems."#;

/// Return the universal best-practices principles addendum.
///
/// Why: function wrapper keeps the call site source-agnostic to whether the
/// text is a constant or loaded from disk, and makes the call clearly
/// identifiable in prompt-assembly code.
/// What: returns `PRINCIPLES_ADDENDUM` — the prompt component compiled from
/// Google Eng Practices, Conventional Comments, and peer-reviewed code-review
/// research (see module doc for full citation list).
/// Test: `principles_addendum_is_non_empty`, `principles_contains_key_concepts`.
///
/// # Forward-compatibility note
///
/// Today this returns a compile-time constant (`&'static str`).  A future
/// runtime-config path (e.g., loading a user-supplied `principles.md` from the
/// XDG config dir, similar to how `VoiceLoader` handles voice packages) would
/// change the return type to `Cow<'static, str>` or `String`.  All call sites
/// already go through this function, so that migration will be a single-file
/// change here plus a `Cow`/`String` adjustment at the handful of call sites
/// that currently elide the type.  No over-engineering is warranted until that
/// runtime path is actually needed.
pub fn principles_addendum() -> &'static str {
    PRINCIPLES_ADDENDUM
}

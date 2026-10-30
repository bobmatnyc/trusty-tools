## Review principles

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
Do not open with or pad reviews with praise, affirmation, or restated
approval — lead with what needs action and keep every comment proportionate
to its substance. Praise from an automated reviewer is not equivalent to
praise from a human colleague: it costs the reader attention without carrying
the social value, so include a `praise:` comment only when clearly justified —
a non-obvious correct decision or a real risk averted. It is never a per-review
formality and never used to soften criticism. When nothing clearly rises to
that bar, omit praise entirely. Avoid the words "simply,"
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
later" promises for real correctness or complexity problems.

**Do NOT flag.**
Stay silent on — do not emit a finding for — any of the following:
- Pure style/formatting nits already enforced by fmt/clippy/linters (spacing,
  import order, trailing commas, line length).
- Speculative or hypothetical concerns ("if someone later calls this with X…")
  with no evidence the diff actually triggers them.
- Restating what the diff obviously does, or narrating the change back to the
  author without identifying a problem.
- Subjective preferences (naming taste, "I'd structure this differently") that
  carry no correctness or maintainability consequence.
A finding must name a concrete consequence; if you cannot, do not raise it.
## Context first, opinion second

A model handed a bare diff will find style nits and miss the thing that matters,
because the caller it breaks is in a file it never saw. trusty-review fixes the
input rather than the prompt: it retrieves code context from trusty-search and
complexity data from trusty-analyze before the reviewer model is called at all.

That is also why it will refuse. A review produced without that context is worse
than no review — it reads exactly like a real one. When a required dependency is
unreachable, a hosted review is skipped and the caller is told the reason, in a
shape distinct from any verdict so it cannot be mistaken for one.

## Point it at anything

- A GitHub pull request: `trusty-review run owner repo 123`.
- A ref range, with no manual diff step:
  `trusty-review run --base origin/main`.
- A patch on stdin: `git diff origin/main...HEAD | trusty-review run --local-diff -`.
- A checkout the search daemon has never seen, via `--source-root`.

Only a GitHub PR review can post a comment, and only once you turn dry-run off.
Every other source is dry-run by construction — a local diff cannot reach your
repository's review thread however it is invoked.

`trusty-review compare` runs the same diff past several models at once, which is
the honest way to decide whether a cheaper reviewer is good enough for your
codebase.

## A verdict, not a wall of prose

Every review returns a letter grade, a verdict — APPROVE, APPROVE with
reservations, REQUEST_CHANGES, BLOCK, or UNKNOWN when the diff was too truncated
to judge — and findings carrying their own severity and confidence. The verdict
is derived from the grade, then clamped so it can never come out weaker than the
findings' own severity floor already requires. Token counts and an estimated
cost ride along in the footer.

UNKNOWN exists deliberately. A reviewer that cannot see enough to form an
opinion should say so, not approve.

## Standards that live in the repo

Drop a `.trusty-review.toml` at your repository root and every contributor and
CI run picks up the same review standards with no per-machine setup — a voice
package, and optionally a named template that appends extra scrutiny on top of
the stock rubric. Template names are validated as bare identifiers precisely
because that file is attacker-controlled: any PR author can add one.

A template only appends. It never replaces the grade scale, the verdict table,
or the severity anchors, so a project cannot quietly redefine what BLOCK means.

## Due-diligence reports

`trusty-review report --manifest <file>` generates a structured technical
due-diligence report across one or more repositories: executive summary,
per-application scorecards, findings by severity, and graph-ready data
appendices in Markdown and JSON.

The default run is fully deterministic — measured from the checkouts themselves,
no model involved. `--synthesize` layers LLM prose over the summary and the
non-healthy findings only, and fails closed to the deterministic output rather
than emit a partially-trusted result. Every value carries a marker saying
whether it was measured, declared, or inferred, and a figure that appears
nowhere in the underlying data is rejected before it can reach the page.

The data appendix is not left as pipe tables alone. Each populated dataset
carries a declared chart type, and the renderer turns it into a Mermaid chart
under its table — an `xychart-beta` for bar and stacked-bar data, a `radar-beta`
for radar. That pass is pure rendering from the rows already in the table: no
model, no network. The table stays the authoritative source and the chart is a
derived view of it, so a dataset that stayed empty simply gets no chart.

`--analyze` fills the complexity sections from a running trusty-analyze daemon —
the complexity distribution and the RED/AMBER finding bands, mapped from the
daemon's own measurements rather than from prose. It fails open per dataset:
whatever answered is kept and whatever did not is named under Gaps & Caveats,
and a run where the daemon is absent falls back to the built-in scan and
produces the same output a run without the flag would.

## Telling it what to look for

A report can carry an analyst brief: a free-form markdown file naming the focus
areas, the concerns to chase, and the questions this particular review has to
answer. Pass it with `--instructions <path>`, name it under the manifest's
`[report].instructions` key, or drop a file called `instructions.md` next to
`manifest.toml` and it is picked up with no flag and no key. Those three are a
precedence order, highest first.

The brief is recorded verbatim in the report as its Analyst Instructions
section, so every report says what it was asked to look for. Under
`--synthesize` it also steers where the prose puts its emphasis. What it cannot
do is loosen a guard: a figure the numeric guardrail cannot trace back to the
collected data is withheld whether or not the brief asked for it, and the report
discloses the withholding. Leave the file out and the run is unchanged.
[The instructions.md guide](/docs/guides/audit-instructions) has the mechanism
and a template to start from.

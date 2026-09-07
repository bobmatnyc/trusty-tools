## Three stages, one command

`tga analyze` runs the whole pipeline. Each stage is also a subcommand, because
on a large history you will want to re-run one without paying for the others.

- `tga collect` — walk each configured repository, extract commit metadata and
  diff stats, resolve author identities, and write it all to SQLite. Optionally
  pull pull-request and issue metadata from GitHub, JIRA, Linear, or Azure
  DevOps alongside it.
- `tga classify` — run every unclassified commit through the cascade and write
  the verdict back. Rule tiers run in parallel.
- `tga report` — aggregate per author, per week, and per DORA metric, then write
  CSV, JSON, and Markdown into the output directory.

The database is a local SQLite file, so every number in a report is one you can
go and check with a query.

## The classification cascade

Naming what a commit actually did is the hard part, and a single heuristic gets
it wrong often enough to be useless. tga tries tiers in order and takes the
first confident answer: a manual override you pinned, the issue type from a
linked ticket, a project-key mapping, an Aho-Corasick scan for
conventional-commit prefixes, regex patterns, a weighted sum over several
independent signals, and fuzzy heuristics for merges and reverts.

An LLM tier sits at the end for the commits the rules could not place, disabled
by default and enabled with `--use-llm`. Its answers are accepted only above a
confidence threshold you set. `--no-external` skips every network-bound source,
which is what you want while iterating on a rule file.

The rule set is introspectable rather than a black box: `tga rules list`
enumerates it, `tga rules test "<message>"` shows you which tier would fire, and
`tga override` pins a verdict that outranks all of them.

## What comes out

- `tga author <email>` — a per-engineer drill-down: commits, effort, pull
  requests, category mix.
- `tga pr-metrics` — pull-request metrics per engineer, once PR fetching is
  turned on.
- `tga dora` — all four DORA metrics, fed by `tga deployments` and
  `tga incidents`.
- `tga aliases` — merge the four email addresses one person has committed under,
  so the per-author numbers mean anything at all.

## Getting started

`tga install` is an interactive wizard that writes the config for you. A
hand-written one can be as small as a list of repository paths — every other
section has a default. Note the package and binary are both `tga`, not the crate
directory's longer name.

## Acquisition due diligence: `tga audit`

One non-interactive command runs the whole pipeline across every repository your
config names and hands the result to trusty-review, which renders an
eight-section technical due-diligence report — the deliverable someone reads
while deciding whether to buy the codebase. A stage that fails does not stop the
sweep; it becomes a named line in the report's Gaps & Caveats instead of a zero
in a cell.

It has its own page: the eight stages, the six flags, what lands in the output
directory, and the six things it deliberately refuses to estimate.

[Install and run `tga audit` →](/tools/trusty-git-analytics/audit)

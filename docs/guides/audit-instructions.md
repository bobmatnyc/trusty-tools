# Engagement instructions (`instructions.md`)

A due-diligence report is generated from measurements, not opinions. But two
engagements over the same repository are rarely asking the same question — one
cares about how an acquisition target's test coverage will age, another about
whether a team can absorb a migration. `instructions.md` is where that question
goes: a free-form markdown brief that travels with the engagement and tells the
report what this particular review has to address.

It is optional. Leave it out and the run produces exactly what it would have
produced anyway.

## How it is found

Three sources, highest precedence first:

1. `trusty-review report --instructions <path>` — an explicit path on the
   command line.
2. The `[report].instructions` key in `manifest.toml`, resolved relative to the
   manifest's directory.
3. A file named `instructions.md` sitting beside `manifest.toml`, discovered
   with no flag and no key.

The third is what makes an engagement self-contained. A client unzips the
engagement directory, drops `instructions.md` next to the manifest, and
`trusty-audit render` picks it up — that command runs a `trusty-review report`
per manifest, so both entry points resolve the brief the same way.

## What it does to the report

- **Recorded verbatim.** The brief is reproduced as the report's *Analyst
  Instructions* section, so every report states what it was asked to look for.
  Nothing paraphrases it.
- **Steers synthesis.** Under `--synthesize` the brief is also injected into the
  synthesis prompt as focus directives, shifting where the prose spends its
  attention.
- **Never relaxes a guard.** Every number in synthesized prose must trace back to
  a figure already in the collected data. A figure that cannot is withheld and
  the report discloses the withholding — asking for it in the brief changes
  nothing. Instructions steer emphasis; they do not authorize invention.

## Failure behavior

| Situation | Result |
|---|---|
| No `instructions.md`, no flag, no manifest key | Fail-open. The run is byte-identical to one that never had a brief. |
| `--instructions <path>` naming a file that does not exist | Hard error. A mistyped path fails loudly rather than silently producing an unbriefed report. |
| `instructions.md` present but unreadable — bad permissions, a dangling symlink, non-UTF-8 bytes | Hard error. Fail-open covers a missing input, never one the author put there and the run could not honour. |
| Either source present but empty | Warning, then treated as absent. |

## A template to start from

The file is free-form markdown — there is no schema, and nothing below is a
required heading. What follows is a starting point that matches how the brief is
consumed: it is read as prose, so write it as prose a reviewer would understand.

```markdown
# Engagement instructions

## Context

One paragraph: what this review is for and who reads the output. A technical
diligence ahead of an acquisition reads differently from an internal health
check, and the report's tone should follow the reader.

## Focus areas

Rank these — the first is where depth is most valuable.

1. <Area>. Why it matters for this engagement, and what a good or bad answer
   would look like.
2. <Area>. Same.
3. <Area>. Same.

## Known concerns to investigate

Specific claims or worries to test against the code, one per bullet. Frame each
as a question the report can answer from evidence.

- <Concern>. What would confirm or dispel it?
- <Concern>. What would confirm or dispel it?

## De-emphasize

Areas that are out of scope or already understood, so effort is not spent
restating them.

- <Area>, and why it does not need coverage here.

## Audience and tone

Who the report is written for and how much background to assume — a CTO, a deal
team with limited engineering context, an internal platform group. Note any
vocabulary to prefer or avoid.

## Note on limits

Findings stay grounded in what the tooling measured. Where the evidence does not
support an answer to something asked for above, the report says so rather than
filling the gap.
```

That last section is not decoration. The numeric guardrail withholds figures it
cannot trace back to the data whatever the brief says, so a brief that asks for
a conclusion the evidence does not support gets a disclosure, not a number.
Writing that expectation into the brief keeps the reader and the tool agreed on
what an unanswered question looks like.

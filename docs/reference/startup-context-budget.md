# Startup-Context Budget — What Turn 1 Costs, and What to Cut

> **The measurement is `tm doctor`'s `startup_context` row and the gate is
> `scripts/check_context_budget.sh`.** This page is the breakdown those two
> point at: what the startup prompt is made of, what each piece weighs, and
> which lever moves which number. Issue #7424; parent audit #4513.

## What "startup context" means here

One number: the FIRST assistant turn's `input_tokens +
cache_creation_input_tokens + cache_read_input_tokens`, read from the session's
own Claude Code transcript. Turn 1 is the only turn whose input is entirely
instructions, tool definitions and injected context — every later turn mixes in
the conversation itself, so only turn 1 measures what the harness spent before
the operator's first request was answered.

The 2026-09-11 pass of #4513 measured turn-1 totals of **98k–107k tokens**
against a **50k** target.

## The two surfaces, and why there are two

| Surface | Measures | When it speaks |
|---|---|---|
| `tm doctor` → `startup_context` | the OUTCOME, in tokens, from sessions that really ran | on a machine that has run managed sessions |
| `scripts/check_context_budget.sh` | the INPUT, in bytes of instruction text | on the PR that changes that text |

The audits were manual (2026-08-01, 2026-09-11) and the creep between them was
many small additions rather than one reviewable commit — no single PR ever
looked like the problem. The doctor row makes the outcome standing; the CI gate
refuses the growth at the only moment it is still cheap to refuse.

## What the CI gate weighs

`scripts/check_context_budget.sh` sums the bytes of every row in
`scripts/context-budget-baseline.tsv` and fails when the TOTAL grows more than
5% over it, or any single file more than 10%. The sources:

- `CLAUDE.md` — the project's own instructions, by far the largest single row.
- `crates/trusty-mpm/src/assets/instructions/sections/*.md` — the framework
  prompt's composed sections.
- `crates/trusty-mpm/src/assets/output-styles/trusty-mpm.md` — the active
  output style.

`README.md` inside the sections directory is measured too. The glob is taken
literally so that what is weighed is auditable in the baseline diff rather than
hidden behind an exception list; a README edit that trips the gate is one
`--update` away.

A deliberate growth is recorded in the same PR:

```bash
bash scripts/check_context_budget.sh --update   # rewrite the baseline
bash scripts/check_context_budget.sh            # confirm it passes
```

## Where the tokens actually go

The three sources the CI gate weighs are the part this repo controls directly.
They are not the whole turn-1 total — a session's prompt also carries the
harness's own system prompt, the tool and MCP tool definitions, the bundled
agent and skill rosters the session loads, and whatever the session-start hooks
inject. The gate deliberately does not try to weigh those: their sizes are set
by the harness and the machine's MCP configuration, not by a diff in this
repository, so a gate over them would fail for reasons no PR could fix.

Levers, in rough order of how much they move the number:

1. **Trim `CLAUDE.md`.** It is the largest row under this repo's control. The
   pattern that works is the one #7423 used on the SLOC section: keep the rule
   and the prohibition inline, move the mechanics to a `docs/reference/` page,
   and link it. An agent that needs the mechanics reads the link; every session
   stops paying for them.
2. **Trim the instruction sections.** Same move, same reasoning — a section
   that explains itself twice pays twice on every launch.
3. **Drop MCP servers the project does not use.** Tool definitions are sent in
   full on every turn, and an unused server's schemas cost the same as a used
   one's.
4. **Narrow the deployed skill and agent rosters.** Each deployed asset's
   frontmatter reaches the prompt.

## Reading the doctor row

```
tm doctor
  ⚠ startup_context   turn-1 startup context median 101000 / latest 98000 tokens
                      over 3 session(s) reaches the 50000-token ceiling — …
```

It samples the newest recorded readings for the project `tm doctor` was run in,
and reports both the median (what the project costs) and the latest (whether the
change in front of you just pushed it over). It **warns, never fails**: the
ceiling is a budget, and a prompt the operator deliberately grew is a preference
rather than a defect. A project with no reading yet reports UNKNOWN — nothing
was measured, so nothing passed.

The ceiling and the sample size are operator config, in
`~/.trusty-tools/trusty-mpm/config.yaml`:

```yaml
startup_context:
  enabled: true        # false silences the row entirely
  ceiling_tokens: 50000
  sessions: 10
```

## Where the readings live

Each managed session's turn-1 figure is recorded once, at the first `tm
statusline` render that can see an assistant turn, under
`~/.trusty-mpm/usage/session-startup-context/<claude-session-id>` — the same
per-session store the model id and transcript path use, beside the savings
ledger the `💸` segment folds. The record carries the session's own working
directory, which is what scopes the doctor sample to one project: the check
opens no transcript at all, so it cannot read another project's session data.

`tm session ls` shows the same figure per row in its `START` column, rounded to
the nearest thousand; `-` means no reading was recorded for that session.

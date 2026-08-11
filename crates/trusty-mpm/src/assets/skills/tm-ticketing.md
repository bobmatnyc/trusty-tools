---
name: tm-ticketing
description: The single authority on work-planning artifacts — issue promotion and deduplication, titles/bodies, labels, milestones, GitHub Projects, lifecycle comments, and code-backed portfolio triage
user-invocable: true
version: "2.1.0"
category: pm-workflow
tags: [tickets, issues, promotion-gate, deduplication, labels, pm-required]
effort: medium
---

# tm-ticketing — Issue Policy

<!-- #5202: this skill owns the ISSUE. It owns no git operation and no PR
     mutation, including PR title and body. The delivery chain that wraps it is
     `tm-workflow`. -->

The PM never calls ticketing tools directly — always delegate to the `ticketing`
agent (P6 / CB#6). This skill is what the PM reads before deciding to file,
comment on, or close anything.

## Scope

Yours: whether an issue should exist, search and dedup, issue title and body,
labels, assignee, milestone, comments, state, parent/child links, and GitHub
Project structure/items/fields.

Not yours: any git operation, and **any PR mutation — including the PR title and
body**. Those are the `version-control` agent's, delegated by the PM. Ticketing
supplies the canonical issue context that goes *into* the PR body; version
control writes it. The full boundary and the handoff sequence are stated once, in
`tm-workflow`.

For session-local task tracking that is not a formal ticket, use
`mcp__trusty-memory__task_add` / `task_list` / `task_complete` directly. These
are in-session TODOs, not a ticketing system, and are not a forbidden MCP family
under CB#6.

## Backends

The `ticketing` agent is granted the full tool set, so `gh` is available to it
whichever backend a project uses. Its priority order:

1. `mcp__mcp-ticketer__*` MCP tools, when configured.
2. GitHub Issues via `gh issue …` (or `mcp__github__*`) when the project's
   tracker is GitHub — the common case.
3. `aitrackdown` CLI when neither is available.

Route **every** Issue API operation to `ticketing`, whichever backend applies.

## Ask Before Creating

If the user references a ticket or issue and no matching one is found, ticketing
MUST NOT auto-create. Ask: "I didn't find an existing issue for [topic]. Create
one, or did you mean a different one?" Auto-create only on an explicit "create a
ticket/issue for X."

## Ticket-Promotion Gate

**A finding is not automatically a ticket.** Most findings belong to the work
already in flight; only some are worth a durable artifact someone else has to
triage, prioritize, and eventually close. Run this gate before every issue
creation.

### 1. Search first, then choose a disposition

Searching open **and** closed issues is a required ordered procedure the
`ticketing` agent runs on every dispatch, specified once in the agent asset
(`assets/agents/ticketing.md`, "Search, Then Choose a Disposition"). Do not
restate it in a delegation brief; state the finding and let the agent run its own
gate.

Every finding that could have become a ticket ends in exactly one of four
dispositions, and the agent reports which:

| Disposition | When |
|---|---|
| `COMMENT` | An open issue already covers it — add the new occurrence there |
| `REOPEN` | A closed issue covers the same defect and the fix has not held — reopen it with the new occurrence |
| `NEW REGRESSION` | A closed issue's fix landed and verified, and this is a *different* failure mode or a different root cause — file new and link the closed one |
| `NO TICKET` | The promotion criteria below are not met — session task, PR comment, or a checklist item on the parent |

Reopening is not unconditional. Reopen when the same defect recurs with the same
root cause; file a new regression when the recurrence has a different cause, a
different symptom class, or arrives after a verified fix that a reader would need
to see as separate work.

### 2. Promote only an independently prioritizable outcome

File a standalone issue only when at least one of these holds:

| # | Promotion criterion |
|---|---|
| a | A reproduced, user-visible defect |
| b | Accepted feature work |
| c | A different owner, release, dependency, or security disposition from the current outcome |
| d | It cannot fit the current PR without changing that PR's outcome or risk |
| e | The user explicitly asked for it to be tracked |

Otherwise it stays a session task, a PR review comment, or a checklist item on
the parent issue. **"Follow-up" is not a category that bypasses this gate.**

An easy fix spotted while working on a file does not enter this gate at all: it
is noted on the CURRENT issue and made in the same work — see **Opportunistic
Fixes** in the instruction package, which this gate extends rather than restates.

A code-review or QA finding reaches this gate by exactly one route: the `Promote`
disposition in `code-review-standards`. A reviewer marking `Promote` has
recommended, not filed — the finding still has to clear the criteria above, and
an APPROVE verdict never files a ticket on its own.

### 3. Label the confidence state

Every filed issue states which of these it is, in the body:

| State | Meaning | Default disposition |
|---|---|---|
| Observed | User-visible behaviour directly seen | Ticket if independently actionable |
| Reproduced | Repeatable with recorded steps or a test | Ticket if independently actionable |
| Inferred | Code evidence supports the risk; no reproduction | Note on the parent issue/PR unless high-severity |
| Speculative | Plausible concern or analogy only | Session note; no ticket |

If your own draft says "not confirmed", "possible", or "same risk class", the
state is Inferred or Speculative — keep it on the parent unless severity
justifies escalation.

### 4. Size issues by outcome, not by finding

- One issue may hold several symptoms sharing one root cause, owner, and
  acceptance test.
- Never file separate issues for the implementation, tests, documentation,
  changelog, or review cleanup needed to finish the same outcome — those are one
  PR (`tm-workflow`, "One Outcome, One PR").
- Split only when the parts can be prioritized, shipped, reverted, or accepted
  independently.
- Experiments stay session-local until the project accepts the result.
- A recurring flaky test or failure family gets **one canonical issue**. Append
  each new occurrence (run URL, SHA, command, failure signature) to it under
  `COMMENT`.

## What a Ticket Says

🔴 **Title**: outcome-first and specific. Labels already carry type, so do not
repeat `bug:`, `fix(…)`, `feat(…)`, or `epic:`. Keep a component prefix only
when it disambiguates the issue, e.g. `trusty-search: Watcher misses
external-volume renames`. Aim for under ~90 characters. Not "Bug in system".

🔴 **Body**: a concise problem/outcome statement, the decisive evidence, and
**one to four observable closure conditions**. Nothing else. The form is binding
and is specified once in the agent asset (`assets/agents/ticketing.md`, "Sparse
Ticket Bodies") — no structured headings, point rather than restate, cite file
and symbol rather than line numbers, and stop when the body fills a short screen.
Most tickets do all of it in under ten lines.

Alongside the closure conditions a reader must be able to tell the confidence
state (§3) and the relationship to parent work, including the search/dispatch
outcome. Those are facts to convey, not headings to fill in.

**Bounded exceptions.** Three shapes may exceed the short-body form, and only
these:

| Shape | What it may add |
|---|---|
| `epic` | A child-work checklist and the scope boundary between children |
| Security | Impact, affected versions, and disclosure state |
| Research / audit | The evidence inventory the audit produced |

An exception buys length for *evidence*, never for narrative. Everything else
stays sparse.

This governs issue bodies only. It does **not** relax the evidence rule for
claiming a gate passed: raw test output stays mandatory there (`BASE-AGENT.md` —
never summarise test results in your own words).

## Labels

Four separable families. The `ticketing` agent applies them at creation and the
exact command form lives in the agent asset ("Label at Creation"); this is the
model, so a delegation brief never needs to spell it out.

| Family | Cardinality | Content |
|---|---|---|
| Type | exactly one | `bug`, `enhancement`, `refactor`, `chore`, `documentation`, `epic` |
| Owning component | one or more | The crate or subsystem the defect actually lives in |
| Priority | optional | `P0`–`P3`, **only** when the issue text itself asserts severity. A guessed priority is noise |
| Workflow state | optional | Stable states such as `release-cleanup`, `backlog`, `paused`, or `blocked` when the repository uses them |

🔴 **Never invent a label the repository does not carry.** Check `gh label list`
before using one; create a genuinely missing label rather than dropping the
family or substituting an approximation.

Do not create or apply `ws/<session-name>` labels by default. A session is
execution provenance, not a durable planning axis. Preserve one only when the
repository explicitly declares it as an active automation contract.

## Milestones

For a single new issue, leave the milestone unset unless the user, parent work,
or repository policy supplies a delivery lane. A milestone may be a named
release or an intentional portfolio lane such as active release cleanup,
backlog, or paused work. During an explicit roadmap/portfolio organization
request, classify every in-scope open issue into one confirmed lane so the
tracker has no accidental orphans. Set or change a milestone when the item is:

- deliberately scheduled into a release you have confirmed is open;
- child work that a release-gating parent already carries into that release;
- identified as a blocker for a release already in flight;
- deliberately assigned to an established active/backlog/paused lane as part
  of a user-requested portfolio cleanup.

A milestone is not a label and not a project view. An issue holds many labels
and exactly one milestone, so a workstream or theme there evicts the delivery
lane. No-new-features milestones may contain bugs, regressions, security work,
CI/tests, documentation corrections, packaging, and bounded maintenance. They
must not contain `enhancement`/`epic` work or feature-titled requests; route
those to backlog or paused instead.

## Portfolio Triage and GitHub Projects

When the user asks to organize issues, milestones, groupings, or a Project,
ticketing runs the full portfolio protocol in the agent asset ("Portfolio Audit
and Safe Bulk Mutation"). The non-negotiable parts are:

- inventory and paginate before designing;
- validate stale/implemented claims against the current default branch, specs,
  tests, and commit history;
- state deterministic classification rules and counts;
- create destinations before migrating sources;
- verify coverage before deleting labels or closing milestones;
- reconcile GitHub milestone counts with open PRs as well as issues;
- retry only missing mutations after transient API failures.

GitHub Projects are portfolio views over canonical issues, not a second issue
store. Use one when the user needs a board, roadmap, cross-repository view, or
multiple planning axes. Prefer a minimal field model: built-in `Status`, then
only stable fields such as `Track`, `Delivery lane`, `Priority`, and `Target
release`. Reuse an existing Project where possible; create one only when the
user explicitly asks to build/create it.

### Consolidation decisions

- A duplicate shares the same outcome, root cause/decision, owner, and closure
  test. Similar provider/platform/component siblings are not duplicates.
- A commit mentioning an issue is not proof it is complete. Close only when
  current code/tests satisfy the issue's requested outcome, and comment with
  the implementing commit plus the verification seam.
- For label aliases, migrate associations and verify counts before deleting the
  source label. If pagination, rate limits, or a transient API error make the
  migration incomplete, retain the source label and report the remainder.

## Lifecycle

When a ticket or issue reference is detected (an ID pattern, a URL, "work on
issue #123"), the PM delegates:

1. **Work start** — mark in progress and comment with initial findings: for bugs
   the root cause or hypothesis, for features a brief scope summary, plus any
   user workaround.
2. **Each phase** — a progress comment at meaningful state transitions
   (diagnosis confirmed, fix pushed, review verdict received, blocked). Not
   per-poll spam. Include deliverables and links to commits/PRs.
3. **Work complete** — close with the fix SHA/version, verification evidence, and
   the merged PR link. The PM hands `ticketing` the merged PR and squash SHA that
   `version-control` reported.
4. **Blockers** — mark blocked and comment with the blocker, its impact, and the
   unblock criteria.

In-flight visibility is standard practice where tracking artifacts exist.
Projects without formal tracking workflows are not subject to it.

Every delegation in this chain carries the ticket context, so downstream agents
can reference it in their own output.

### Attribution on Issues and Comments

Every issue body and issue comment ends with one line:

```
🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools
```

One line, machine-readable, no preamble around it. Commit and PR attribution is
governed separately by the Framework-Guaranteed Conventions in the instruction
package, and the PR body is `version-control`'s to write.

## `/tm-ticket` Subcommands

High-level orchestration over the ticketing agent, for whichever tracker is
configured:

| Subcommand | Purpose |
|---|---|
| `/tm-ticket organize` | Review, transition states, update priorities, flag stale tickets |
| `/tm-ticket proceed` | Analyze the board, recommend the top 3 next actions |
| `/tm-ticket status` | Health metrics, ticket counts, high-priority work, blockers |
| `/tm-ticket project <url>` | Set the default project/tracker context |
| `/tm-ticket portfolio` | Align issues, labels, milestones, and Projects against current code/specs |
| `/tm-ticket project-build` | Create or repair a minimal GitHub Project view over canonical issues |

Every subcommand is a PM delegation to the ticketing agent — the PM constructs
the prompt and presents the result, never calling the underlying tools itself.

## Documentation Routing With Ticket Context

With a ticket context present, delegate research findings and specs as ticket
comments (or linked files), and still write a local backup doc under
`docs/research/` (or the configured `documentation.docs_path`). Without ticket
context, everything goes to the local docs path only, named `{topic}-{date}.md`.

## Related Skills

- `tm-workflow` — the delivery chain this issue lifecycle sits inside, and the ticketing/version-control boundary
- `tm-circuit-breaker` — CB#6 enforcement detail
- `tm-delegation-patterns` — where ticketing fits in the broader agent matrix
- `tm-bug-reporting` — the MCP-native path for daemon-captured errors

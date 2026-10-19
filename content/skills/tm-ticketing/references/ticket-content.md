# tm-ticketing — title and body

## What a Ticket Says

🔴 **Title**: type-aware and specific — `<type>: <what is wrong or wanted>`, e.g.
`fix(trusty-search): watcher misses renames on external volumes`. Under ~70
characters. Not "Bug in system".

🔴 **Body**: a concise problem/outcome statement, the decisive evidence, and
**one to four observable closure conditions**. Nothing else. The form is binding
and is specified once in the agent asset (`assets/agents/ticketing.md`, "Sparse
Ticket Bodies") — no structured headings, point rather than restate, cite file
and symbol rather than line numbers, and stop when the body fills a short screen.
Most tickets do all of it in under ten lines.

Alongside the closure conditions, a **defect** ticket must let the reader tell
the confidence state (`references/promotion-gate.md`, §3); a feature, task, epic, or spec ticket carries none.
Every ticket, defect or not, conveys its relationship to parent work,
including the search/dispatch outcome — a fact about the issue, not a heading
to fill in.

**Bounded exceptions.** Four shapes may exceed the short-body form, and only
these:

| Shape | What it may add |
|---|---|
| `epic` | A child-work checklist and the scope boundary between children |
| Security | Impact, affected versions, and disclosure state |
| Research / audit | The evidence inventory the audit produced |
| Phase of an epic | The five headings Scope, Acceptance criteria, Non-goals, Risk, Gate — `tm-epic`, `references/phase-template.md` |

An exception buys length for *evidence*, never for narrative. Everything else
stays sparse.

This governs issue bodies only. It does **not** relax the evidence rule for
claiming a gate passed: raw test output stays mandatory there (`BASE-AGENT.md` —
never summarise test results in your own words).

## Symptom Titles and Area Labels

🔴 **Title states the observed symptom**, never the cause or the fix. Describe
what the observer sees, e.g. "index remove deletes the wrong index when
TRUSTY_INDEX is set" rather than the internal function at fault.

🔴 **Body carries a Symptom section and an Evidence section.** A Suspected
cause section is optional and labeled a hypothesis, never presented as a
diagnosis.

🟡 **A project may define an `area:` label family**, one label per issue,
tagging the subsystem where the symptom shows — never where the fix will
land. The area set itself is per-project, named in that project's own
`TICKETING.md`; this skill defines none. Example: a project might carry
`area:secrets`, `area:ci`, `area:other` (fallback).

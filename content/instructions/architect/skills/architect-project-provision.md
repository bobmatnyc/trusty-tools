---
name: architect-project-provision
description: Architect skill. Load when the user sets up a new project for the fleet, when a new tm-* session appears whose project has no record in local/projects/, or when a mandate is checked. The provisioning checklist - confirm the stack, CLAUDE.md markers, the ADR folder, review and release gates, ticketing conventions, institutional mandates, tm registration and the push hook. Plans first; the user approves before anything is written into the project. Architect only.
user-invocable: true
metadata:
  version: "0.1.1"
category: monitoring
tags: [architect, provisioning, mandates, trusty-mpm]
effort: high
---

# architect-project-provision — Provision a Project

<!-- rule:r-067 -->
**Provisioning.** When a new project is set up, run this checklist: stack,
CLAUDE.md markers, ADR folder, review and release gates, ticketing
conventions, institutional mandates, tm registration and the push hook. A
change to the project's CLAUDE.md goes to the user first.

<!-- rule:r-070 -->
**Institutional mandates.** The user supplies organization-wide rules
(ticketing, commit and PR style, cloud log drains and similar) as one TOML
file each in `local/mandates/`. The schema is in `local/mandates/README.md`.
Check the mandates in scope during provisioning, in reviews and on full-poll
passes. Send each violation to the owning PM as a plain relay. Bring a
conflict between a mandate and a project's own rules to the user.

The Architect plans the setup and runs the machine-level steps himself.
Content inside the project repo (CLAUDE.md, ADRs, TICKETING.md) is written by
the project's PM, or by an engineer agent when no PM runs yet, from a brief in
`local/briefs/`. A change to a project's CLAUDE.md always goes to the user
first (`architect-operating-rules`, relay format).

## 1. Survey (read-only)

- Path, repo and account: `git -C <dir> remote -v`; the GitHub account for
  this work comes from `local/user.md`.
- Goal: which goal in `local/goals.md` the project serves. None means ask the
  user before provisioning.
- Stack: list the manifests at the root and one level down: `Cargo.toml`,
  `package.json` and its lock file, `pyproject.toml` or `requirements*.txt`,
  `go.mod`, `Dockerfile`, `*.tf`, `.github/workflows/`.
- Existing conventions: send a research agent to list and summarize the
  project's CLAUDE.md, TICKETING.md and CONTRIBUTING.md; you check for an ADR
  folder (`<dir>/docs/adr/`, `<dir>/docs/decisions/`, `<dir>/adr/`) yourself
  with `git ls-tree`.
- Gates: `gh api repos/<o>/<r>/branches/main/protection` (required checks,
  reviews), release workflows, deploy targets.
- Mandates: every `local/mandates/*.toml` whose `scope` covers the project
  (`local/mandates/README.md`). Run each `check`; note violations.

## 2. Plan

Write `local/briefs/<date>-<project>-provision.md` with one line per item:
current state, proposed state, who does it. Bring the user one
multiple-choice question per open decision, recommendation first:
- the stack as detected (confirm or correct);
- CLAUDE.md marker overrides: default none. Never write a sample marker pair
  into any file: a marker alone on a line takes effect;
- ADR folder: keep an existing convention; otherwise `<dir>/docs/adr/` with a
  README index and ADR-0001 "Record architecture decisions";
- review and release gates to add (required checks, review count, release
  dry run). Never change branch protection without the user's answer;
- ticketing: a TICKETING.md in the project (overrides tm-ticketing defaults;
  the ticketing agent manages it), one `area:` label per issue,
  symptom-first titles, milestones;
- each mandate violation, and any conflict between a mandate and the
  project's own rules (conflicts go to the user, never resolved alone);
- factory designation: is this session a factory session, one that sweeps
  its own board, issues and milestones and drives them to Done? Ask on every
  new session start, not only a new project. The answer goes to the
  `local/state.md` factory list. A factory session gets the "Factory sessions"
  rule of `architect-strategy` in its first relay.

## 3. Apply (after the user's answer)

1. Register with tm: `tm project init --dir <dir>`. Confirm with
   `tm project list`.
2. Push hook: the PM adds a `Notification` hook entry to
   `<dir>/.claude/settings.json` whose command runs this directory's script
   through an explicit interpreter, with the absolute path quoted:
   `/bin/bash "<abs>/scripts/notify-supervisor.sh"`. Never the bare script
   path: a hook or statusLine command never execs a `#!` script (D73). The
   Intent Ledger hook takes the same shape:
   `/bin/sh "<abs>/scripts/datg-hook.sh" <session>`. `<abs>` is this
   directory's absolute path; never `~` or `$CLAUDE_PROJECT_DIR`. Verify the
   entry in that file, then watch `inbox/events.jsonl` for the project's
   first event.
3. Board check: run `tm issue board check` in the project directory and
   report its output in the brief. Exit 0 means the board matches the spec.
   A non-zero exit means drift, or no `project:` block in
   `issue-state.yaml`; report either to the user, and do not run
   `tm issue board apply` without the user's answer.
4. Send the PM (or engineer agent) a short relay pointing to the brief for the
   in-repo items. The PM's own gates apply to that work.
5. Verify each item before recording it as verified: use shell commands for
   gates and manifests, and send a research agent to verify items that require
   reading project documents.

## 4. Record

- `local/projects/<name>.md` from `local/projects/_template.md`, with the
  gates, the mandates in scope and the architecture baseline
  (`architect-drift-check`, "Set a baseline").
- `local/state.md` watch set, `local/goals.md` session-to-goal map, and one
  `local/actions.md` entry.
- Offer the first architectural review (`architect-review`) as a proposal.

## Related skills

- `architect-drift-check`: setting the architecture baseline.
- `architect-review`: the first review offered after provisioning.
- `architect-strategy`: factory sessions.

---
name: architect-project-provision
description: The Architect's provisioning checklist for a new trusty-mpm project — confirm the stack, seed CLAUDE.md markers, set up the ADR folder, apply review and release gates, set ticketing conventions, check institutional mandates, register with tm and add the push hook. Plans first; the user approves before anything is written into the project.
version: 1.0.0
category: architecture
when_to_use: when the user sets up a new project for the fleet, or a new tm-* session appears whose project has no record in its palace
author: trusty supervisor
license: Elastic-2.0
effort: high
tags:
  - architect
  - supervisor
  - provisioning
  - trusty-mpm
requires_tools:
  - bash
---

Kind: framework (the Architect's own skill; portable with the framework core).

# Architect: provision a project

The Architect plans the setup and runs the machine-level steps himself. Content
inside the project repo (CLAUDE.md, ADRs, TICKETING.md) is written by the
project's PM, or by an engineer agent when no PM runs yet, from a brief in
`local/briefs/`. A change to a project's CLAUDE.md always goes to the user
first (see architect.md "Operating rules", Relay format).

## 1. Survey (read-only)

- Path, repo and account: `git -C <dir> remote -v`; the GitHub account for
  this work comes from the user profile in `AGENTS.md`.
- Goal: which goal in `local/goals.md` the project serves. None means ask the
  user before provisioning.
- Stack: list the manifests at the root and one level down: `Cargo.toml`,
  `package.json` and its lock file, `pyproject.toml` or `requirements*.txt`,
  `go.mod`, `Dockerfile`, `*.tf`, `.github/workflows/`.
- Existing conventions: send a research agent to list and summarize `CLAUDE.md`,
  `architect.md`, `TICKETING.md`, and `CONTRIBUTING.md`; you check for an ADR
  folder (`docs/adr/`, `docs/decisions/`, `adr/`) yourself with `git ls-tree`.
- Gates: `gh api repos/<o>/<r>/branches/main/protection` (required checks,
  reviews), release workflows, deploy targets.
- Mandates: every `local/mandates/*.toml` whose `scope` covers the project
  (`templates/mandates/README.md`). Run each `check`; note violations.

## 2. Plan

Write `local/briefs/<date>-<project>-provision.md` with one line per item:
current state, proposed state, who does it. Bring the user one
multiple-choice question per open decision, recommendation first:
- the stack as detected (confirm or correct);
- CLAUDE.md marker overrides: default none. Tokens are listed in this
  directory's `CLAUDE.md` ("Framework Instructions"). Never write a sample
  marker pair into any file: a marker alone on a line takes effect;
- ADR folder: keep an existing convention; otherwise `docs/adr/` with a
  `README.md` index and ADR-0001 "Record architecture decisions";
- review and release gates to add (required checks, review count, release
  dry run). Never change branch protection without the user's answer;
- ticketing: `TICKETING.md` (overrides tm-ticketing defaults; the ticketing
  agent manages it), one `area:` label per issue, symptom-first titles,
  milestones;
- each mandate violation, and any conflict between a mandate and the
  project's own rules (conflicts go to the user, never resolved alone);
- factory designation (owner, 2026-10-07): is this session a factory session,
  one that sweeps its own board, issues and milestones and drives them to
  Done? Ask on every new session start, not only a new project. The answer
  goes to the `local/state.md` factory list. A factory session gets the brief
  `local/briefs/2026-10-07-factory-sessions.md` in its first relay.

## 3. Apply (after the user's answer)

1. Register with tm: `tm project init --dir <dir>` (add `--account <login>`
   when the repo needs a non-default gh account). Confirm with
   `tm project list`.
2. Push hook: `scripts/install-notify-hook.sh <dir>`. Verify the
   `Notification` entry in `<dir>/.claude/settings.json`, then watch
   `inbox/events.jsonl` for the project's first event.
3. Send the PM (or engineer agent) a short relay pointing to the brief for the
   in-repo items. The PM's own gates apply to that work.
4. Verify each item before recording it as verified: use shell commands for
   gates and manifests, and send a research agent to verify items that require
   reading project documents.

## 4. Record

- The project's palace record (`memory_remember` in that project's palace), with the
  gates, the mandates in scope and the architecture baseline
  (`architect-drift-check`, "Set a baseline").
- `local/state.md` watch set, `local/goals.md` session-to-goal map, and one
  `local/actions.md` entry.
- Offer the first architectural review (`architect-review`) as a proposal.

## Architect duty text (moved from the operating manual "Architect duties", restructure 2026-10-07)

Kind: framework. Index line in architect.md: load this when a new project is set up or a mandate is checked.

<!-- rule:r-067 -->
1. **Provisioning.** When a new project is set up, run `architect-project-provision`: stack, CLAUDE.md markers, ADR folder, review and release gates, ticketing conventions, institutional mandates, tm registration and the push hook. A change to the project's CLAUDE.md goes to the user first.

<!-- rule:r-070 -->
4. **Institutional mandates.** The user supplies organization-wide rules (ticketing, commit and PR style, cloud log drains and similar) as one TOML file each in `local/mandates/`. The schema is in `templates/mandates/README.md`. Check the mandates in scope during provisioning, in reviews and on full-poll passes. Send each violation to the owning PM as a plain relay. Bring a conflict between a mandate and a project's own rules to the user.

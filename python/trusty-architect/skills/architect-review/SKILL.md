---
name: architect-review
description: The Architect's architectural review of one project — scoped, delegated to code-analyzer or research agents (the Architect does not code), spot-verified, and returned to the user as findings plus one multiple-choice proposal.
version: 1.0.0
category: architecture
when_to_use: when the user asks for an architectural review, or accepts one the Architect offered (new project, large merge, detected drift, weekly per active project)
author: trusty supervisor
license: Elastic-2.0
effort: high
tags:
  - architect
  - supervisor
  - review
  - architecture
requires_tools:
  - bash
---

Kind: framework (the Architect's own skill; portable with the framework core).

# Architect: architectural review

## When to offer

Offer a review as a proposal the user can decline, never run one unasked:
- a new project (after provisioning);
- a large merge: a new top-level module, crate or package, a new direct
  dependency, a change to a boundary in the baseline, or a release;
- unintended drift from `architect-drift-check`;
- at least weekly for each active project.

Record each offer and the answer in the project's palace record
("Architecture"). A declined review is not offered again before its next
trigger.

## 1. Scope

State the question the review answers, the repo, the ref as a commit SHA, and
the baseline SHA from the project's palace record. One review, one project.

## 2. Gather (read-only)

Send a research agent to read the project's CLAUDE.md and architect.md, its ADR
list, and the baseline (from the project's palace record); you read only the
latest drift-check result and mandates in scope (`local/mandates/`). Query the
project's trusty-memory palace and trusty-search index for prior rulings yourself.

## 3. Delegate

From the Architect's main session, dispatch one agent with a brief in
`local/briefs/<date>-<project>-review-brief.md`:
- `code-analyzer` for structure, dependencies, coupling and complexity;
- `research` for design questions and prior art.
The brief names the repo path, the SHA, the scope, the questions and the
mandates to check. It requires read-only work: no edits, commits, branch
switches or fetches in the project checkout (`git show <sha>:<path>`,
`git -C <repo> grep <pattern> <sha>`). Each finding carries file:line
evidence, severity and a recommended action. When running as the `architect`
subagent, return the brief to the main session instead of dispatching.

## 4. Verify

Send the research agent to spot-check the top findings (they read, you record
the evidence). Label each finding verified or reported based on the agent's
evidence.

## 5. Report

Write the findings to `local/briefs/<date>-<project>-architecture-review.md`.
Bring the user a short summary and one multiple-choice question,
recommendation first, for example:
- (A) relay the top fixes to the PM as a brief;
- (B) record the drift as intended: the PM writes an ADR and the baseline
  moves;
- (C) file issues (only on the user's answer);
- (D) no action.
Mandate violations go to the owning PM as plain relays; conflicts with the
project's own rules go to the user.

## 6. Record

the project's palace record ("Architecture"): date, SHA, verdict, the user's
answer. One `local/actions.md` entry.

## Architect duty text (moved from the operating manual "Architect duties", restructure 2026-10-07)

Kind: framework. Index line in architect.md: load this when a review is offered, accepted or due.

<!-- rule:r-068 -->
2. **Architectural reviews.** Offer a review for a new project, after a large merge, when drift is detected, and at least weekly for each active project. Each offer is a proposal the user can decline. Run `architect-review` when the user accepts or asks. Record offers and answers in the project's palace record; do not re-offer a declined review before its next trigger.

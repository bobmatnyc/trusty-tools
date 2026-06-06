---
name: cto-apex-framework
description: APEX artifact review framework — types, slash commands, status workflows
tags: [apex, duetto, cto, framework, prd, initiative, experiment]
agents: [cto-assistant]
---

# APEX Framework — Master Reference

APEX (AI Product Execution) is a Git-based product development framework with
AI augmentation used by Duetto's product and engineering teams. It tracks
strategic bets as initiatives, validates them through time-boxed experiments,
auto-generates PRDs from accumulated context, and links all artifacts in a
queryable Git history.

---

## 1. Core Concepts

| Artifact | ID Pattern | Purpose |
|----------|-----------|---------|
| **Initiative** | `I-YYYY-XX-NNN` | Strategic bet with hypothesis to validate (e.g., `I-2026-GC-001`) |
| **Experiment** | `E-YYYY-NNN` | Time-boxed hypothesis validation (e.g., `E-2026-007`) |
| **PRD** | `PRD-YYYY-NNN` | Product requirements doc auto-generated from initiative context |
| **Implementation** | `IMPL-YYYY-XX-NNN` | Engineering execution plan linked to approved PRD |
| **Decision** | `DEC-YYYY-NNN` | ADR-style record capturing context, decision, consequences |
| **Proposal** | `PROP-YYYY-NNN` | Process or cross-cutting scope proposal |
| **Team Charter** | `TC-NNN` | Team structure, mission, and ownership |

**ID format:** product code (XX) is 2-4 uppercase letters derived from the
product slug. Examples: `GC` (gamechanger), `PRC` (pricing), `FOR` (forecasting),
`APX` (the APEX framework itself).

**POD** — team assignment stored as frontmatter metadata, never as folder structure.

---

## 2. Directory Structure

```
products/
  {domain}/                  # analytics, apex, applications,
    {product}/               #   data-platform, engineering-platform, integrations
      initiatives/
        {initiative-slug}/
          Initiative.md      # Required: main artifact with frontmatter + body
          repos.yaml         # Links code repos to initiative
          proposals/
          meetings/
          discovery/
          experiments/       # E-YYYY-XX-NNN.md files
          designs/
          docs/

proposals/           # Active global/cross-cutting proposals
templates/           # Canonical document templates
schemas/             # JSON Schema (frontmatter.schema.json)
docs/decisions/      # DEC-YYYY-NNN.md formal decision records
```

### Domains and Products

| Domain | Key Products |
|--------|-------------|
| `applications` | blockbuster, advance, dre, gamechanger, scoreboard, commandcenter |
| `analytics` | pricing, forecasting, ml-platform, product-analytics |
| `data-platform` | Data infrastructure |
| `engineering-platform` | infrastructure, monolith, security, ingestion |
| `integrations` | External system connectors |
| `apex` | apex-framework (meta) |

---

## 3. Status Workflows

| Artifact | Workflow |
|----------|----------|
| Proposal | `draft → review → approved → implemented` (or `rejected`) |
| Initiative | `discovery → validated → delivery → deployed → learning → success` (or `killed`) |
| Experiment | `planned → running → completed` with outcome (`validated \| invalidated \| inconclusive`) |
| PRD | `draft → review → approved → in-development → deployed → learning` |
| Implementation | `draft → review → approved → in-progress → complete` |
| Decision | `proposed → accepted` (or `deprecated \| superseded`) |

### Status-Dependent Field Requirements

| Status | Requirement |
|--------|------------|
| Initiative `discovery` | Requires: `metric_target`, `hypothesis`, `author`, `pod` |
| Initiative `delivery` | Requires: `related_prd` set (not null) |
| Initiative `deployed/learning/success` | Requires: `related_prd` set |
| Experiment `completed` | Should have `learning.outcome` set |

---

## 4. Slash Commands — Full Reference

All 21 `/apex-*` commands:

### Core
| Command | Purpose |
|---------|---------|
| `/apex-help` | Show all available APEX commands |
| `/apex-setup` | Configure git identity and verify GitHub access |
| `/apex-proposal` | Create or update a proposal at any level |
| `/apex-initiative` | Create new initiative directory with required files |
| `/apex-experiment` | Create experiment or record outcome within initiative |
| `/apex-prd` | Generate PRD from initiative context |

### Status and Workflow
| Command | Purpose |
|---------|---------|
| `/apex-status` | Display status of all initiatives and pipeline position |
| `/apex-update` | Update status of any APEX artifact with validation |
| `/apex-update-initiative` | Re-synthesize Initiative.md and bump version |
| `/apex-validate` | Validate frontmatter and document links |
| `/apex-commit` | Create conventional commit with proper formatting |
| `/apex-dashboard` | Generate cross-product pipeline status dashboard |

### Development and Delivery
| Command | Purpose |
|---------|---------|
| `/apex-implementation` | Create engineering execution plan linked to approved PRD |
| `/apex-epic` | Convert approved PRD to JIRA epic with user stories |
| `/apex-ship` | Record deployment milestone |

### PR Workflow
| Command | Purpose |
|---------|---------|
| `/apex-pr-create` | Create pull request with APEX conventions |
| `/apex-pr-review` | Review PR systematically with structured checklists |
| `/apex-pr-approve` | Approve a pull request with optional comment |
| `/apex-pr-comment` | Add a comment to a pull request |
| `/apex-pr-request-changes` | Request changes with structured feedback |

### Learning and Measurement
| Command | Purpose |
|---------|---------|
| `/apex-learn` | Document learnings and create retrospective |
| `/apex-measure` | Record metrics checkpoint to evaluate initiative success |

---

## 5. Frontmatter — Top Three Errors to Avoid

**Error 1 — Hypothesis as string (WRONG for experiments):**
```yaml
# WRONG
hypothesis: "We believe X will Y because Z"

# CORRECT for experiments (must be an object)
hypothesis:
  statement: "We believe X will Y because Z"
  confidence: medium   # low | medium | high
  validation_method: prototype  # interview | analytics | prototype | a_b_test | spike
```

**Error 2 — `id` in frontmatter (DEPRECATED):** ID is inferred from directory path.

**Error 3 — Missing status-conditional fields:** `delivery` requires `related_prd`.

### Required Initiative Frontmatter

```yaml
title, type, status, domain, product, created, updated, tags
# Plus at discovery status: metric_target, hypothesis, author, pod
```

### Required Experiment Frontmatter

```yaml
title, type, parent_initiative, status, hypothesis (object), time_box,
success_criteria, learning (object), created, updated, author, tags
```

### ID Regex Patterns

| Artifact | Pattern | Example |
|----------|---------|---------|
| Initiative | `^I-[0-9]{4}-[A-Z]{2,4}-[0-9]{3}$` | `I-2026-APX-001` |
| Experiment | `^E-[0-9]{4}-[0-9]{3}(?:-[a-z0-9-]+)?$` | `E-2026-007` |
| PRD | `^PRD-[0-9]{4}-[0-9]{3}$` | `PRD-2026-003` |
| Team Charter | `^TC-[0-9]{3}$` | `TC-001` |

---

## 6. End-to-End Workflow Patterns

### New Feature (Full Discovery Path)

```
/apex-initiative          Create initiative in discovery status
  ↓
Add meetings/, discovery/ Populate context
  ↓
/apex-experiment          Design and run time-boxed validation
  ↓ (outcome: validated)
/apex-prd                 Generate PRD from context
  ↓ (status: review → approved)
/apex-implementation      Create engineering execution plan
  ↓
/apex-epic                Convert PRD to JIRA epic
  ↓
/apex-ship → /apex-measure → /apex-learn
```

### Delivery-Mode (Skip Discovery)

```
/apex-initiative (status: delivery)  →  /apex-prd  →  /apex-epic  →  /apex-ship
```

### Status Transition Gates

- Initiative `discovery → validated`: formal experiment with `learning.outcome: validated`
- Initiative `validated → delivery`: PRD written and `related_prd` set
- Initiative transitions trigger MAJOR version bump + explicit user confirmation

---

## 7. CI / Validation

- **Pre-commit hook** runs `apex validate --root .` on every `git commit`
- **GitHub Actions** runs `apex validate` and posts results as PR comment on failures
- **Schema:** `schemas/frontmatter.schema.json` is the authoritative validation spec
- Fix validation failures one commit at a time — do NOT batch fixes with other changes

---

## 8. Validation Checklist

Before committing any APEX artifact:

- [ ] `id` field is NOT in frontmatter (deprecated — path-based identification)
- [ ] `hypothesis` is an object on experiments (not a string)
- [ ] `learning` is an object on experiments (not a string)
- [ ] `parent_initiative` exists and is valid for experiments and PRDs
- [ ] `related_prd` is set when `status: delivery` on initiatives
- [ ] `domain` and `product` are from valid lists
- [ ] Dates are in `YYYY-MM-DD` format
- [ ] Cross-references point to existing artifacts

---

## 9. Quick Reference: Where Files Go

| Action | Location |
|--------|----------|
| New initiative | `products/{domain}/{product}/initiatives/{slug}/Initiative.md` |
| New experiment | `products/{domain}/{product}/initiatives/{slug}/experiments/E-YYYY-NNN.md` |
| New PRD | `products/{domain}/{product}/initiatives/{slug}/PRD-YYYY-NNN.md` |
| Session insight | Row in `Initiative.md` Insights table |
| Lightweight decision | Row in `Initiative.md` Decision Log table |
| Significant decision | `docs/decisions/DEC-YYYY-NNN.md` |
| Repo linking | `products/{domain}/{product}/initiatives/{slug}/repos.yaml` |

---

## 10. Commit Conventions

```bash
# Standard
feat(scope): add new initiative
fix(scope): correct hypothesis object format

# Session sync (granular — one per artifact)
insight(pricerator): override rate has zero churn correlation
decision(pricerator): use INPUT_OVERRIDE dispatch
experiment(pricerator): structured A/B test with 110 hotels
update(pricerator): add multi-price popover delivery narrative
```

Always end session sync commits with `Co-Authored-By:` line.

---
name: cto-duetto-org
description: Duetto Research org structure, SELT, team composition, contractors
tags: [duetto, cto, org, selt, contractors, budget, projects]
agents: [cto-assistant]
---

# Duetto Research — Org Structure & Context

Internal reference for the CTO Assistant. Use this to answer "who reports to who",
"who owns X", contractor vendor questions, and project status queries.

---

## Company Snapshot

- **Business:** Hospitality revenue management SaaS
- **R&D:** ~$25M annual spend
- **Headcount:** ~178 R&D staff total
  - 94 Duetto FTEs
  - ~84 contractors
- **Codebase:** ~1.2M-line Java monolith (legacy core) + growing microservices

---

## C-Suite

| Person | Role |
|--------|------|
| Alex Zoghlin | CEO |
| Robert Matsuoka (Masa) | CTO — primary user of this assistant |
| Kartik Yellepeddi | CPO |
| Linda Mudadu | Chief People Officer |

---

## Key Operations

| Person | Role |
|--------|------|
| Andrea Kovac | Engineering Operations Coordinator — programme management, meeting notes, tracking. Authorized user of this assistant. |

---

## Senior Engineering Leadership Team (SELT)

The five direct reports to the CTO. All of engineering rolls up through this group.

### Antonio Cortes — Senior Engineering Manager (Core Platform)
- Owns the legacy Java monolith and core platform stability
- Focus areas: monolith maintenance, runtime performance, technical debt reduction
- Partners closely with Shiv Yadav on the Strangler Pattern extraction work

### Catherine Daves — Director of Engineering (Metrics, Allocation)
- Owns engineering metrics, capacity planning, work-type allocation
- Focus areas: DORA metrics, contractor quality assessment, R&D budget allocation
- Drives the quarterly resource planning cycle and FTE/contractor mix decisions

### Chris Montford — VP Engineering (Infrastructure)
- Largest org under the SELT — owns infrastructure, ingestion, security
- Focus areas: AWS infrastructure, data ingestion pipelines, platform security
- Several layers of managers report under him

### Ram Katraju — Senior DevOps Manager (DevOps, BCDR, Cloud)
- Owns DevOps, business continuity / disaster recovery, cloud operations
- Focus areas: deployment automation, BCDR posture, AWS cost optimization
- Point person for cross-cutting DevOps and SRE concerns

### Shiv Yadav — Director of Engineering (Architecture, Strangler Pattern)
- Owns architecture strategy and the Strangler Pattern POC
- Focus areas: shadow extraction from monolith, architecture reviews, Polaris strategy
- Drives the modernisation roadmap and architecture decision records

---

## Contractor Vendors (~84 contractors)

| Vendor | Approx headcount | Notes |
|--------|------------------|-------|
| Encora | ~30 | Largest single vendor; mix of full-stack engineers and QA |
| Saksoft | ~20 | India-based; primarily backend Java engineers |
| Sirma | ~15 | Bulgaria-based; mix across services and integrations |
| Brandorr | ~8 | DevOps, infra, AWS specialists |
| Winder AI | ~5 | ML/AI specialists, applied-ML support |
| Others | ~6 | Smaller vendors and specialist contractors |

Contractor activity, FTE-equivalence scores, and quality tiers are tracked in
the CTO database (cto.db) via `contractor_activity` and `contractor_rankings`
tables.

---

## Key Projects

### Polaris Architecture
Modernisation strategy (supersedes the older "North Star" framing). Polaris is
the umbrella for the move off the monolith toward a more service-oriented,
data-first architecture. Owned across SELT with Shiv Yadav driving architecture.

### Strangler Pattern POC
Shadow-extraction approach: route real production traffic in parallel through
new service implementations to validate behavior before cutting over. Owned by
**Shiv Yadav**. Active POC.

### APEX (AI Product Execution)
Git-based product development framework with AI augmentation. Tracks initiatives,
experiments, PRDs, decisions, and implementations as version-controlled artifacts.
21 `/apex-*` slash commands cover the full product lifecycle. See the
`cto-apex-framework` skill for detail.

### RateGain
Active migration project — replacing legacy RateGain integration components.

### Data-First Architecture
Strategic initiative to decouple the data layer from application logic.
Long-running architectural shift, several initiatives feed into it.

---

## Budget — 2026 R&D

- ~$25M total R&D spend
- Tracked in `rd_budget_2026` table (cto.db) — 200 rows, monthly allocation
  (`jan_26` … `dec_26` columns plus `cy_26_total`)
- 94 FTEs + ~84 contractors = 178 R&D heads
- Budget views in analytics.duckdb: `budget_by_initiative`, `budget_by_product`,
  `budget_comparison` (actuals vs plan variance)

---

## Reporting Structure (high-level)

```
CEO (Alex Zoghlin)
├── CTO (Robert Matsuoka / Masa)
│   ├── Antonio Cortes — Core Platform
│   ├── Catherine Daves — Metrics, Allocation
│   ├── Chris Montford — Infrastructure (largest org)
│   ├── Ram Katraju — DevOps, BCDR, Cloud
│   └── Shiv Yadav — Architecture, Strangler Pattern
├── CPO (Kartik Yellepeddi)
└── CPO People (Linda Mudadu)
```

The full org chart (~115 engineering/product leadership rows) lives in
`org_chart` table in cto.db. Use `sub_org` for team/group (no `team` column).

---

## Quick Lookups

- "Who reports to Chris Montford?" → query `org_chart WHERE manager = 'Chris Montford'`
- "Who's a contractor?" → `person WHERE employment_type = 'Contractor'` or
  `contractor_activity` table
- "What's our spend on Encora?" → `contractor_activity WHERE organization = 'Encora'`
  joined to budget figures
- "What did Shiv ship this week?" → `commit_details` joined via `developer_aliases`

---

## Confirmed vs Inferred — Source Notes

When generating tables, matrices, or compliance docs, ALWAYS append a
source-transparency block:

```
✅ Confirmed: <items> — sourced from cto-duetto-org skill / cto.db / Confluence
⚠️ Inferred: <items> — based on general knowledge, not verified from live data
```

Numbers in this skill (94 FTEs, ~84 contractors, ~$25M, vendor headcounts) are
**approximate baselines** — for precise figures, query cto.db directly.

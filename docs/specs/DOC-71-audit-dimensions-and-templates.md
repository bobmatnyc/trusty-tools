---
spec_refs: []
---

# DOC-71 — Audit Dimensions and Templates

**Status:** Draft. Design record only — no code in this PR.
**Spec ID:** `SPEC-AUDITTPL-01~draft` … `SPEC-AUDITTPL-08~draft`
**Subsystem:** `trusty-audit` — dimension query tables (`grounding/evidence.rs`,
`grounding/hotspots.rs`) and the engagement manifest (`config.rs`); `trusty-review`
— report narrative templates and section instructions (`report/template.rs`,
`report/section_instructions.rs`, `report/investigate/{select,deps}.rs`);
`trusty-git-analytics` (tga) — agentic-commit detection and authorship/bus-factor
data consumed by the new dimensions.
**Owner:** Bob Matsuoka
**Last-updated:** 2026-08-21
**DOC-N claim:** `DOC-71`, scan-before-claim per DOC-38 §4.1. `docs/specs/README.md`'s
catalog note names DOC-71 as the next free number after DOC-70; `scripts/check_doc_numbers.sh`
reports a clean baseline (0 violations) before this file; a `gh pr list` search for
open PRs touching `docs/specs/` returned none.
**Builds on:** the owner ruling cited at `crates/trusty-audit/src/grounding/evidence.rs:136-137`
("this intelligence lives here, never in trusty-review", 2026-08-19) — this spec's
template model (§3) extends that ruling rather than revisiting it.
**Cross-ref:** issues #6138, #6141, #6142, #6144 (run-provenance discipline, §6);
[docs/reference/domain-consolidation-audit.md](../reference/domain-consolidation-audit.md).

---

## Purpose

trusty-audit's report pipeline assesses six dimensions today, chosen ad hoc.
Established technical-due-diligence practice covers a wider, well-sourced set.
This spec (1) distills that practice into a checklist, (2) grounds a gap
assessment against the current evidence chain in code citations, (3) fixes the
template model that lets a future audit pick a stack-specific dimension/checklist
pair without touching the query-intelligence placement ruling, (4) specifies the
first three templates, and (5) orders the work smallest-first.

No client, industry, or deal is named anywhere in this document. Where an
engagement needs stack- or client-specific checks beyond a template's generic
content, those are supplied at run time through the `client_conventions`
specialization slot (§7) and are never committed to this repository.

---

## {#SPEC-AUDITTPL-01~draft} 1. The 16(+1)-dimension checklist

Acquirer-grade technical-DD frameworks, CISQ/CAST's automated quality
characteristics, ISO/IEC 25010 maintainability, OWASP secure-code-review
practice, lightweight ATAM, and bus-factor/DOA research converge on the same
five buckets — team, architecture, code quality, security, operations —
expanded here into 16 concrete dimensions. Each is tagged with what a tool can
gather **without reading all code** ("machine-gatherable": metrics, manifest
parsing, retrieval queries, graph facts) versus what needs **LLM reading of a
targeted sample** ("sample-read").

| # | Dimension | Machine-gatherable evidence | Sample-read evidence |
|---|---|---|---|
| 1 | Architecture & design conformance | module/crate dependency graph, layering violations via KG call-chain edges | does the code match the stated pattern (layered/hexagonal/microservice)? |
| 2 | Code quality / maintainability (ISO 25010) | cyclomatic/cognitive complexity, file-size distribution, duplication | is the complexity justified by the domain, or accidental? |
| 3 | Reliability (CISQ) | crash-prone pattern counts (panic/unwrap grep) | are failure paths handled or silently swallowed? |
| 4 | Security posture (OWASP) | secret-pattern grep, dependency CVE feed | auth/session/input-validation logic read in context |
| 5 | Dependency & license risk (SCA/SBOM) | manifest+lockfile parse, license classifier, CVE join | none — almost entirely machine-gatherable |
| 6 | API surface / contract quality | schema files, OpenAPI/proto presence, breaking-change diff | is the contract versioned and validated at the boundary? |
| 7 | Concurrency correctness | lock/mutex/channel keyword grep, `unsafe` count | deadlock/race shape in the actual critical section |
| 8 | State management & data consistency | cache/queue/pool path heuristics | transaction boundaries, invalidation-on-crash logic |
| 9 | Test posture | test-file presence/ratio, CI config presence | does the test assert behavior or just exercise a path? |
| 10 | Observability / operability | logging/metrics/tracing import counts, health-endpoint presence | is the signal actionable, or noise? |
| 11 | Data handling / privacy | PII-pattern grep, encryption-at-rest keyword hits | is sensitive data scoped/redacted at the boundary? |
| 12 | Build / CI / release hygiene | CI workflow file parse, branch-protection API, release-tag cadence | none — mechanical |
| 13 | Documentation quality (two levels — see below) | spec/ADR/README presence+density per subsystem; doc-comment density per public API | does the spec/ADR narrative match the code? does an inline doc match the real contract? |
| 14 | Scalability / performance | query-in-loop and pool-config heuristics | is the bottleneck real under the domain's actual load shape? |
| 15 | Team / key-person risk (bus factor) | git blame/DOA computation, commit trajectory | none — a pure metric dimension |
| 16 | Delivery traceability | commit↔ticket correlation | none — mechanical |

**Dimension 13, expanded.** Documentation quality is assessed at two levels, not
one: **(a) spec level** — do specs/ADRs/READMEs exist, do they cover the major
subsystems, and does their narrative match the code (machine leg: presence,
density, and a subsystem-coverage map; sample-read leg: narrative-vs-code spot
check); **(b) inline level** — doc-comment density and quality against a
convention (for a Rust-stack engagement, this repo's own Why/What/Test pattern
is the model, per its `CLAUDE.md`; generically, public-API doc coverage). Both
legs are machine-gatherable except the two narrative-match spot checks.

**Dimension 17 — agentic build estimation (repo-tooling extension, not
literature-sourced).** tga already ships an agentic-commit marker detector
(`crates/trusty-git-analytics/src/collect/ai_markers.rs`,
`ai_marker_config.rs`) that yields a per-commit AI-tool label and mode from an
ordered marker list — a `BUILTIN` set plus operator-supplied markers loaded from
disk (`ai_marker_config.rs`, since #5414), so a house commit-footer convention
is addable without a code change. Its own doc comment states the current
limitation plainly: a fixed seven-regex predecessor caught 47.7% of commits on
this repository's own history where the true agentic share was 91.0% — a
confidently wrong number is worse than an absent one. The dimension reports the
estimated agentic share of commits/LoC **alongside** the detector's own coverage
rate, never the share alone, plus the same identity-map caveat as dimension 15
(alias fragmentation skews both metrics the same way). Improvement path:
broaden the configurable marker set further, and corroborate with a
documentation-signal check (does the diff carry an AI-assistant-shaped commit
message even where no footer marker matched). Machine-gatherable almost
entirely; the LLM leg is narrative only, never a re-estimate of the number.

Sources: [Technical due diligence checklist](https://www.shorepod.com/post/technical-due-diligence-checklist-8-key-areas-for-2025),
[CISQ automated quality characteristics](https://www.it-cisq.org/overview/),
[ISO/IEC 25010 maintainability](https://blog.codacy.com/iso-25010-software-quality-model),
[OWASP Secure Code Review](https://cheatsheetseries.owasp.org/cheatsheets/Secure_Code_Review_Cheat_Sheet.html),
[ATAM](https://en.wikipedia.org/wiki/Architecture_tradeoff_analysis_method),
[Bus factor / DOA](https://contributoriq.com/documentation/bus-factor),
[SCA/SBOM license risk](https://www.wiz.io/academy/application-security/software-composition-analysis).

---

## {#SPEC-AUDITTPL-02~draft} 2. Coverage against the current evidence chain

Grounding, current `main`:

- Six DD dimensions and their queries live in `DD_DIMENSIONS`
  (`crates/trusty-audit/src/grounding/evidence.rs:151-196`) — authentication &
  secrets, dependencies, state management, error handling, scalability, test
  coverage. `ANALYST_FOCUS` (`evidence.rs:130`) is a seventh, run-time-only
  slot fed by the engagement brief, not the table. The same six are spelled
  identically in `DIMENSIONS` (`crates/trusty-review/src/report/investigate/select.rs:162-169`).
- Selection ranks by path/name heuristics (`select.rs:194-258`), trusty-analyze
  complexity hotspots (`crates/trusty-audit/src/grounding/hotspots.rs`), and
  trusty-search dimension queries, blended round-robin (`evidence.rs::blend`).
- `DependencyInventory` is declared+locked versions only, no license/CVE join
  (`crates/trusty-review/src/report/investigate/deps.rs:52-92`); the report
  template already carries CVE/License/Obsolescence/Cloud-Readiness tables
  (`templates/report-technical-dd.md:286-316`) with no producer wired to them.
- Authorship/bus-factor is already covered, but out-of-band via tga, rendered
  into `templates/report-technical-dd.md:261-282` — not through `evidence.rs`.

| Dimension | Status | Grounding to add if missing |
|---|---|---|
| 1. Architecture conformance | Missing | New `DimensionQueries` entry: `search_kg`/`get_call_chain` for layering violations (graph facts, not text search) |
| 2. Code quality / maintainability | Partial | Complexity hotspots already feed ranking (`hotspots.rs`); add a duplication/god-object query set |
| 3. Reliability | Covered | "error handling", `evidence.rs:174-181` |
| 4. Security posture | Covered | "authentication & secrets", `evidence.rs:153-159` |
| 5. Dependency & license risk | Partial | Versions yes (`deps.rs`); add an SPDX license classifier + CVE join, both machine-only |
| 6. API surface / contract quality | Missing | New entry: schema/OpenAPI/proto presence + breaking-signature diff |
| 7. Concurrency correctness | Partial | Folded into "state management" today (`evidence.rs:168-173`); split into its own dimension |
| 8. State management | Covered | `evidence.rs:167-173` |
| 9. Test posture | Covered, shallow | Presence-only via `is_test_path` (`select.rs:261-274`); extend with a flakiness/skip-marker grep |
| 10. Observability | Missing | New entry: structured-logging/tracing/metrics wiring, health-check endpoints |
| 11. Data handling / privacy | Missing | New entry: PII field names, encryption-at-rest/in-transit, retention/deletion logic |
| 12. Build / CI hygiene | Missing | Machine-only: CI workflow presence, branch-protection API — no LLM |
| 13. Documentation quality | Missing (elevated — see §8 item 2.5) | Two-level machine legs (§1); LLM only for narrative-match spot checks |
| 14. Scalability | Covered | `evidence.rs:182-188` |
| 15. Team / key-person risk | Covered (adjacent) | Out-of-band via tga, not through `evidence.rs` |
| 16. Delivery traceability | Covered | `templates/report-technical-dd.md:386-396` |
| 17. Agentic build estimation | Missing (new capability) | tga already computes it (`ai_markers.rs`); wire commit/LoC share + coverage rate into a new section — synthesis-and-render, no new query infrastructure |

Net: 6 of 17 fully covered through the evidence chain, 2 covered by adjacent
(non-evidence-chain) machinery, 2 partial, 7 missing outright. The missing
ones split cleanly into two kinds: architecture conformance, API contracts,
observability, and data/privacy need new `DimensionQueries` entries
(search-driven, same shape as the existing six); build/CI hygiene, documentation
presence, and agentic-build estimation are machine-only legs needing no search
query at all — the same "read it off disk, no LLM and no network" principle
`deps.rs:1-10` already states for dependencies.

---

## {#SPEC-AUDITTPL-03~draft} 3. Template model

Two mechanisms exist today and stay separate:

1. **Report narrative templates** — markdown files, bundled + XDG-overridable,
   loaded by `TemplateLoader` (`crates/trusty-review/src/report/template.rs:48-51`,
   `impl` from `:53`). `report-technical-dd.md` and `report-technical-dd-cast.md`
   ship today. A template overrides per-section LLM instructions via
   `<!-- instruct:<section_id> ... -->` comments, parsed against
   `ALL_SECTION_IDS` (`crates/trusty-review/src/report/section_instructions.rs:39-46`),
   layered generic-default → template-override → analyst-brief-additive.
2. **Dimension query sets** — the `DD_DIMENSIONS` const table in
   `crates/trusty-audit/src/grounding/evidence.rs:151-196`, under the owner
   ruling cited at `evidence.rs:136-137`: this intelligence lives here, never
   in trusty-review. It is code, not TOML, by deliberate placement — not an
   oversight. **This spec does not revisit that ruling.**

A per-stack template therefore needs **two coordinated halves**, selected by
one name: a markdown narrative template (existing mechanism, extended as-is)
plus a Rust-side dimension-query table. Proposal: a sibling const table,
`LANGUAGE_DIMENSIONS: &[(&str, &[DimensionQueries])]`, next to `DD_DIMENSIONS`
in `evidence.rs` — the query intelligence stays exactly where the ruling put
it, while the markdown side resolves the matching narrative file by the same
name (`report-technical-dd-rust.md`, `-enterprise-architecture.md`,
`-integration-platform.md`).

**Selector key.** The engagement manifest (`EngagementConfig`,
`crates/trusty-audit/src/config.rs:334`, which already carries sibling
sub-tables `tools: ToolPins` and `models: ModelPins`) gains one new key,
`[report] template = "rust"`, resolved by both loaders. This mirrors — without
merging with — `ReviewFileConfig.template`'s four-tier precedence resolver
(`crates/trusty-review/src/config/review_template.rs:40-43`), which selects
the unrelated PR-review-addendum template loaded by `ReviewTemplateLoader`
(`crates/trusty-review/src/review_template/mod.rs:93-98`). The two `template`
keys name different pipelines (PR review vs. due-diligence report) and must
never be conflated.

What a template alters, concretely:

- **Evidence query sets** — the stack's `DimensionQueries` table adds or
  reweights queries (Rust adds `unsafe`-block queries; the integration
  template adds a webhook/event-contract dimension, §6).
- **Section instructions** — `<!-- instruct:code_quality_summary ... -->`,
  the same mechanism `report-technical-dd-cast.md` already uses for
  `executive_summary`.
- **Acceptance criteria** — a new section id, `architecture_conformance_summary`
  (not currently in `ALL_SECTION_IDS`), stating what "clean" means for the stack.

---

## {#SPEC-AUDITTPL-04~draft} 4. Template: Rust

Extends `DD_DIMENSIONS` with Rust-specific rows, using this repository's own
conventions as the checklist (this repo is its own test bed):

- **Dependencies** — add `unsafe`-block density and `#[allow(clippy::...)]`
  suppression count; this repo's own "no `unwrap()` in library code" /
  `thiserror`-for-libraries rule becomes a machine-gatherable grep, not a
  sample-read judgment.
- **Error handling** — the existing query set fits (`evidence.rs:176-181`);
  add "library crate returning `anyhow::Result` instead of a `thiserror` enum"
  as a Rust-specific refinement.
- **Concurrency (new, split from state management)** — `unsafe impl
  Send/Sync`, `Mutex`/`RwLock` nesting, an `async` task spawned without a join
  handle.
- **Architecture conformance (new)** — workspace-member layering (a leaf crate
  depending on a crate that should depend on it instead, via a KG reversed-edge
  check); SLOC-cap compliance as a machine fact (mirrors
  `scripts/check_line_cap.sh`).
- **Build/CI hygiene (new, machine-only)** — MSRV pin present, edition
  consistency, a `clippy -D warnings` gate present in CI — all parseable from
  `Cargo.toml`/`.github/workflows/*.yml`, no query needed.

---

## {#SPEC-AUDITTPL-05~draft} 5. Template: enterprise architecture (generic)

No stack assumption; checks structure, not idiom:

- **Layering** — presentation → application → domain → infrastructure edges
  point inward only (KG-derived, machine fact).
- **Shared-library discipline** — a capability implemented independently in
  two or more services is a finding, modeled on this repo's own "common
  entry point, clean domain demarcation" rule and
  [docs/reference/domain-consolidation-audit.md](../reference/domain-consolidation-audit.md),
  generalized as a template-level check.
- **Config/secrets handling** — reuses the existing "authentication & secrets"
  query set as-is (`evidence.rs:153-159`); no template variant needed.
- **Deployment topology** — machine leg: Dockerfile/Helm/Terraform presence
  and count; LLM leg only for "does the topology match the stated
  architecture."

---

## {#SPEC-AUDITTPL-06~draft} 6. Template: integration platform (generic, industry-standard checks)

Built from DLQ/idempotency/fencing-token/tenant-isolation practice, generalized
into checks any integration platform should pass. **No company name, industry
vertical, or deal context appears in this template or in this spec.** Where an
engagement supplies a specific, real-world profile of findings against these
checks, that profile is anonymized and generalized before it reaches this
template — never committed as client content — and any engagement-specific
elaboration is supplied at run time through the `client_conventions`
specialization slot (§7), never in-repo.

**Runtime & shared-library hygiene:**
- Runtime-version pinning consistency: one pinned language/runtime version
  across the estate, not a mix of exact and floor constraints.
- No pre-release/RC pins on core frameworks in production services.
- Shared-library floor policy: security-sensitive persistence — credential
  storage above all — must live in a shared library; scaffold-cloning it per
  service/connector is a finding. Probe for byte-identical logic across
  supposedly independent components as scaffold-drift evidence, and for a
  versioned scaffold template with a re-sync mechanism.
- Shared-library version pins: lower-bound-only pins on first-party shared
  libraries are a drift hazard — check for upper bounds or lockfile discipline.
- Config-default hygiene: secret/JWT/key defaults banned at the
  template/scaffold level.

**Data & migration safety:**
- Migration safety: destructive migration operations (truncate/drop-reseed)
  carry an environment guard.

**Event & messaging integrity:**
- Webhook/event authenticity: incoming webhook signature verified with a
  timing-safe comparison against the raw body, shared secret read from
  configuration rather than hardcoded, and HMAC (or equivalent) verification
  enforced by shared middleware rather than reimplemented per handler
  ([OWASP-aligned webhook checklist](https://www.augmentcode.com/guides/secure-code-review-checklist-owasp-aligned-framework)).
- Idempotency, fail-closed: a durable uniqueness constraint on an event/dedup
  key, not an in-memory check; dedup guards fail closed when the backing store
  is unavailable rather than letting reprocessed events through.
- DLQ/retry policy: a delivery-attempt counter and bounded threshold before
  dead-lettering, as one standard poison-message + bounded-backoff policy
  rather than a per-consumer reinvention; absence of a DLQ path for a message
  queue is itself a finding.
- Distributed-lock fencing: lock acquisition returns a monotonically
  increasing token verified on write; fencing/ownership tokens are required,
  and reinvention of unfenced locks across independently-owned components is
  a finding on its own
  ([fencing token pattern](https://dev.to/sertaoseracloud/engineering-multicloud-consensus-implementing-distributed-locking-with-fencing-tokens-31c)).
- Contract/schema versioning: an API/schema change is additive or carries an
  explicit version bump (machine leg: OpenAPI/proto diff between commits); an
  event-schema registry exists, and delivery semantics — at-least-once vs.
  exactly-once — are stated explicitly.

**Access & topology:**
- Internal RPC auth: a required shared interceptor/standard enforces
  authentication on internal service-to-service calls, not a per-service
  choice; probe specifically for an unauthenticated internal RPC path.
- Tenant isolation: a `tenant_id` predicate applied at the query/repository
  boundary, not only in the request handler — followed through background
  jobs and migrations, not just the request path.
- Cloud topology coherence: simultaneous multi-cloud primitives inside one
  service's configuration are flagged for review (a migration in progress
  reads differently from unowned drift, and the finding states which);
  database-per-module is reviewed as a deliberate pattern, not assumed a
  defect.

**Authorship trust (extends dimension 15):**
- Bus-factor metrics require an operator-supplied git identity map before
  they are trusted — alias fragmentation understates concentration. Probe
  for named backup ownership on any bus-factor-1 component, and for
  current-team coverage where authorship concentrates in departed or
  external contributors.

**Run-provenance discipline** (generalized from this repository's own
audit-tooling issues, not client content — this repo audits itself with the
same tool):
- sampling disclosure — a partial-coverage run states so explicitly rather
  than reading as a clean pass (#6138, open)
- interrupted-run artifact hygiene — a stale/partial output is distinguishable
  from a completed one (#6141, open)
- author-alias/identity merging before attribution — unmerged identities skew
  any authorship or bus-factor metric (#6142, open)
- model/provider provenance — a generated artifact states what produced it
  (#6144, open)
- shared-library vs. scaffold-clone discipline — a capability re-implemented
  per service/consumer instead of routed through one shared entry point (this
  repo's own "common entry point" rule, generalized rather than tied to a
  numbered issue)

---

## {#SPEC-AUDITTPL-07~draft} 7. The `client_conventions` specialization slot

The integration-platform template (§6) ships with every check populated
generically. A `<!-- client_conventions -->` block — new, template-local, not
a global `ALL_SECTION_IDS` entry — sits at the top of the architecture section.
An operator fills it per engagement with engagement-specific naming, topology,
or profile conventions to check conformance against, using the same
XDG-override tier `ReviewTemplateLoader` already exposes: an operator drops a
local file without touching the bundled default
(`crates/trusty-review/src/review_template/mod.rs`, the `with_extra_dirs` /
XDG-then-bundled resolution order documented at `:85-91`).

**Nothing engagement-specific ships in this repository.** The mechanism — a
resolvable, XDG-scoped override slot — is what ships; the content an operator
puts in it is theirs, lives outside version control, and is supplied fresh at
run time for each engagement.

---

## {#SPEC-AUDITTPL-08~draft} 8. Implementation plan — smallest first

1. **Section ids.** Add `architecture_conformance_summary` and
   `client_conventions` to `ALL_SECTION_IDS` (`section_instructions.rs`) with
   generic defaults. No new query logic; unblocks every template in §4-§6.
2. **Two missing dimensions.** Add `DimensionQueries` entries for "API surface
   / contract quality" and "observability / operability" to `DD_DIMENSIONS`.
   Reuses the existing merge/blend/quality machinery untouched; closes 2 of
   the 7 missing dimensions with zero new infrastructure.
3. **Documentation completeness (elevated by owner ruling).** Wire dimension
   13's two-level assessment (§1): a machine leg computing spec/ADR/README
   presence-and-density per subsystem plus doc-comment density per public API,
   and an LLM leg limited to the narrative-vs-code spot check. Elevated ahead
   of the remaining missing dimensions because the owner calls it important,
   not because it is technically smaller than item 2.
4. **Build/CI hygiene, machine-only.** Wire a CI-workflow-presence and
   branch-protection-API leg into the investigation pass alongside `deps.rs`'s
   `build_inventory`. No search query, no LLM — the same enumerable-not-searchable
   pattern already proven for dependency manifests.
5. **Agentic build estimation.** Wire tga's existing `ai_markers.rs` /
   `ai_marker_config.rs` detector output — commit/LoC agentic share plus the
   detector's own coverage rate — into a new report section. tga already
   computes the number; this item is synthesis and rendering, not new
   detection, and it must render the coverage-rate caveat every time it
   renders the share.
6. **License classification.** Add an SPDX lookup per ecosystem to
   `DependencyInventory` (`deps.rs`) and wire it to the already-templated but
   unproduced License Risk table (`report-technical-dd.md:294-300`) — closes
   the largest concrete gap between what the template promises and what the
   pipeline delivers today.
7. **The template mechanism.** Introduce the `[report] template` manifest key
   (mirroring `ReviewFileConfig.template`'s resolution precedence) plus the
   `LANGUAGE_DIMENSIONS` sibling table in `evidence.rs`, and ship the Rust,
   enterprise-architecture, and integration-platform template pairs (markdown
   + query table) as the first three named templates. Largest and last: it
   depends on items 1-2 for the section ids it needs, and is the only item
   that touches the manifest schema.

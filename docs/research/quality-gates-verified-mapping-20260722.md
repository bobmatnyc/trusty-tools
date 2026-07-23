# Quality-Gates Article — Verified Mapping Against Our Actual Pipeline

- **Source article summary:** `docs/research/quality-gates-agent-prs-article-2026-04.md` ("Automating Change Acceptance Without a Human in the Loop — Quality Gates," Jaroslaw Wasowski, Medium, Apr 2026).
- **Prior analysis being corrected:** an earlier session analyzed this article inference-only (the article was paywalled, no source text available) and produced a gap/non-adoption list from memory of our pipeline. This document replaces that inference with a verified, file-cited mapping, done with the actual article summary in hand and the actual repo checked out.
- **Verification method:** direct inspection of `crates/trusty-review/`, `crates/trusty-mpm/src/driver/policy.rs`, the `code-critic` agent and `code-review-standards`/`code-production-process` skills, `.github/workflows/*.yml`, live GitHub branch-protection settings (`gh api repos/bobmatnyc/trusty-tools/branches/main/protection`), and targeted greps for techniques the article names (mutation testing, property-based testing, Semgrep, SBOM, semantic/AST diff, canary, feature flags, trust/risk scoring). All findings below are file-cited; absence claims are backed by an explicit grep/search, not by memory.

## Verified mapping: the six gate categories

### 1. Deterministic (unit/integration/e2e, mutation testing, PBT)

| Technique | Status | Evidence |
|---|---|---|
| Unit / integration / e2e tests in CI | **HAVE** | `.github/workflows/ci.yml` `test` job runs `cargo test --workspace`; gate briefs additionally require a full local `cargo test` (no `--lib`, so integration tests run) before handoff (memory: gate-briefs-full-test-surface). |
| Mutation testing (cargo-mutants or equivalent) | **GAP** | No `cargo-mutants` reference anywhere in the workspace (`git grep -n "cargo-mutants\|cargo mutants"` across `Makefile`/`*.yml` — zero hits). Test *effectiveness* is not measured, only presence/pass-fail. |
| Property-based testing (proptest/quickcheck) | **GAP** | No crate declares `proptest` or `quickcheck` as a dependency (`git grep` across every `Cargo.toml` — zero hits). The `contract-driven-testing` skill *recommends* PBT as a general practice for other projects, but no crate in this workspace actually uses it. |

### 2. Static (lint, SAST, SCA, SBOM, secrets, package-install gate)

| Technique | Status | Evidence |
|---|---|---|
| Lint + type-check | **HAVE** | Branch protection requires `Format check` and `Clippy` (fmt --check, clippy -D warnings) — confirmed live via `gh api .../branches/main/protection`. |
| SCA / dependency-vulnerability scan | **HAVE** | `.github/workflows/cargo-audit.yml` — scheduled weekly `cargo audit` against the RustSec advisory DB; its own header notes this replaced an aspirational-but-unreal claim in `SECURITY.md`. |
| SAST (dedicated static security analyzer) | **GAP** | No Semgrep/CodeQL/dedicated SAST tool wired into CI. `git grep -l semgrep` only turns up generic advisory prose in `security-scanning` skill reference docs (guidance for *other* projects), not a tool actually run against this repo. |
| SBOM generation | **GAP** | No `cargo-cyclonedx` or equivalent; `git grep -l "SBOM\|sbom\|cargo-cyclonedx"` across workflows/Cargo files — zero hits. |
| Secret scanning | **HAVE** | `.pre-commit-config.yaml` + `.secrets.baseline` (detect-secrets). |
| Custom project-specific static/policy linters (beyond the article's taxonomy) | **HAVE — exceeds article** | Nine dedicated CI guard workflows not named by the article at all: `line-cap.yml` (500-line file cap, branch-protection-required), `sld-lint.yml` (spec-reference resolution), `instruction-floor-guard.yml`, `token-drift.yml`, `version-parity.yml`, `generation-artifact-lint.yml`, `claude-md-guard.yml`, `capabilities-drift.yml`, `test-pointers.yml`. These are real, CI-enforced, and more granular than the article's generic "static" bucket. |
| Package-install approval gate (anti-slopsquatting) | **GAP** | No explicit approval gate before an agent runs `cargo add`/`pip install`/etc.; no slopsquatting-specific defense found (`git grep -l "slopsquat"` — one unrelated doc hit only). |

### 3. Semantic (AI code review, grounding, semantic diff, spec drift)

| Technique | Status | Evidence |
|---|---|---|
| AI code review tool | **HAVE** | `trusty-review` crate: LLM-backed `review_pr` / `review_diff` MCP tools (`crates/trusty-review/src/mcp/tools.rs:51,91`), producing a letter grade + verdict + findings. Separately, the `code-critic` agent (`crates/trusty-mpm/src/assets/agents/code-critic.md`) runs an adversarial rubric review (`code-review-standards` skill) inside the trusty-mpm coding pipeline, with an explicit CRITICAL/HIGH/MEDIUM/LOW severity taxonomy and an 80%-confidence filter before any finding is asserted. |
| LLM grounded by a deterministic analyzer (article: Semgrep) | **PARTIAL** | `trusty-review` grounds its LLM pass with real context/metrics from `trusty-search` (code retrieval) and `trusty-analyze` (complexity), and runs a second, independent LLM **verification round** (`crates/trusty-review/src/pipeline/verify.rs`, "Phase 2, #583") that CONFIRMS/REFUTES each candidate finding and re-derives the verdict — explicitly built because "the reviewer LLM over-fires" on speculative findings (verify.rs module doc). This is real grounding, but it is LLM-verifies-LLM plus complexity metrics, not a deterministic SAST tool like Semgrep feeding the prompt. |
| Semantic diff (AST-level) | **GAP** | No AST-level diff tooling found (`git grep -l "semantic_diff\|ast_diff"` — zero hits). Diffs are reviewed as unified-diff text. |
| Spec-driven drift detection (article: OpenAPI vs implementation) | **PARTIAL** | We have an analogous but differently-scoped drift gate: `sld-lint.yml` (DOC-38, Spec-Linked Documentation) fails CI when a `# Spec References` block or `spec_refs:` frontmatter doesn't resolve to a real spec, and `version-parity.yml` fails CI on push-to-main when a crate's published version has drifted from its source. Both are real, CI-enforced drift gates — but for docs/spec-references and publish-version parity, not an OpenAPI-contract-vs-code check specifically. |

### 4. Operational (idempotency, blast radius, retry/budget limits)

| Technique | Status | Evidence |
|---|---|---|
| Idempotency checks | **GAP** | No idempotency-testing framework or convention found. |
| Retry-limit / API-budget enforcement (the article's own $4,200 case) | **GAP** | Not found as a general gate. (Note: this is the specific failure mode the article says only an operational gate would have caught — code correct, tests green, lint clean, but a runaway retry loop.) |
| Blast-radius / change-classification routing | **PARTIAL — exists, wrong layer** | `crates/trusty-mpm/src/driver/policy.rs` implements a real `ChangeClass` → `AutonomyTier` (T1–T4) model, explicitly modeled on "the unicorn-factory tiered-PR-autonomy model" (`docs/trusty-mpm/spec/SESSION_MANAGER_DRIVER_AGENT.md` §4) — nearly a structural match to the article's Light/Medium/Full/Full+HITL lanes. But this governs the trusty-mpm **session-manager driver's** auto-accept of a proposed agent action, not this repo's own PR-merge routing; it is not wired into `trusty-tools`' CI or branch protection. |

### 5. Behavioral (canary, SLO auto-rollback — post-merge)

| Technique | Status | Evidence |
|---|---|---|
| Canary rollout (0.1%→100% traffic ramp) | **N/A — architectural mismatch, not rejected on risk grounds** | `release.yml` ships GitHub Releases + crates.io publishes of CLI binaries/library crates; the long-lived services (`trusty-search`, `trusty-mpm`, etc.) run as **local daemons on individual developer machines** (loopback-only doctrine, memory: loopback-only-doctrine), not a shared multi-tenant production fleet. There is no user traffic to ramp 0.1%→100% against and no baseline population for a Kayenta-style Mann-Whitney comparison. |
| SLO-based auto-rollback | **GAP (same root cause as above)** | No SLO monitoring or auto-rollback exists; follows from having no live multi-tenant service to monitor. |
| Feature flags | **GAP** | No feature-flag system found in the workspace. |

### 6. Meta (merge queue, Trust Score aggregation, HITL rule definitions)

| Technique | Status | Evidence |
|---|---|---|
| Merge queue / speculative batch execution | **GAP** | No GitHub Merge Queue or speculative-execution batching; each PR runs CI independently. |
| Composite per-PR Trust Score (weighted 0–100 across categories) | **GAP** | No composite/aggregate risk score anywhere (`git grep -n "risk_score\|trust_score\|composite.*risk"` across all `*.rs` — zero hits). `trusty-review`'s letter grade is a single dimension (the LLM review itself), not a weighted sum across deterministic/static/semantic/operational signals. |
| Policy-based HITL hard-block (destructive-op deny-list) | **HAVE — exists, wrong layer** | `crates/trusty-mpm/src/driver/policy.rs` `DESTRUCTIVE_KEYWORDS` (`delete`, `drop table`, `drop database`, `force-push`/`force push`, `decommission`, `rm -rf`, `truncate`, `revoke`, `rotate secret`/`rotate key`, `wipe`) forces `AutonomyTier::T4` (always-escalate, "regardless of guardrails") when matched — a near-verbatim structural match to the article's policy-based HITL trigger class. Same caveat as above: this is the trusty-mpm driver's action-acceptance policy, not a gate wired into `trusty-tools`' own PR pipeline. |
| Trust-score-can-override-red-CI | **CONFIRMED NEVER — hard-enforced, multi-layer** | `policy.rs` T2 gate requires CI non-red as an unconditional guardrail (not a weighted input); live branch protection requires 6 named status checks to pass with no override short of `enforce_admins:false` + the repo owner's explicit admin-merge, which per project convention (memory: admin-merge-only-on-green) is *only* ever used on **green** CI — `--admin` bypasses bot/review-approval requirements, never a failing check. |
| Human-in-the-loop preserved on every merge (not "radar-only") | **CONFIRMED, and stricter than the article's own default** | `crates/trusty-review/src/integrations/github/posting.rs`: the bot **always** posts its GitHub review as a `COMMENT` event — never `APPROVE`/`REQUEST_CHANGES` — with the code comment "Phase 1 always posts as COMMENT (advisory, never API-level blocking)." Branch protection independently requires 1 approving PR review on every PR. A human (in practice, the repo owner via admin-merge) is structurally never removed from the merge trigger today. |

## Corrected / confirmed gaps (worth adopting)

The prior inference-only analysis named four gaps. Verification against real code **confirms two as-is, and substantially narrows the other two**:

1. **Flaky-test quarantine — CONFIRMED GAP, unchanged.** No CI mechanism detects or quarantines flaky tests. The only adjacent code is `trusty-git-analytics`'s commit-message classifier, which tags commits like "fix flaky test" for analytics purposes — it does not observe flakiness or gate on it. Still a genuine, real gap worth adopting.

2. **Critic calibration / escape-rate tracking — CORRECTED: GAP → PARTIAL.** We already ship real calibration infrastructure the prior analysis missed entirely:
   - `trusty-review calibrate` (`crates/trusty-review/src/commands/calibrate.rs`) runs the real review pipeline against a JSONL corpus of merged PRs with human ground truth and computes recall/precision (including a `rust_semantic_fp_rate` metric) — a repeatable calibration harness, not a one-off eval.
   - A live outcome-tracking loop (`crates/trusty-review/src/integrations/github/outcomes.rs` + `crates/trusty-review/src/store/outcome_store.rs`) polls reactions and follow-up commits on posted findings and feeds a chronic-false-positive **suppression list** (issue #1421).
   - What genuinely remains a gap: (a) this calibration exists only for the `trusty-review` automated bot, not for the `code-critic` agent used in the trusty-mpm coding pipeline — its own reference doc literally says `<!-- TODO: Expand with critic calibration guidance -->` (`code-production-process/references/stage-critic.md`); (b) neither mechanism tracks true **escape rate** — production incidents traced back to a prior APPROVE — only static-corpus precision/recall and developer-reaction-based FP suppression. Adopt: code-critic calibration + true escape-rate (incident-to-approval) tracking. Do not re-adopt what already exists.

3. **Composite per-PR risk line — CONFIRMED GAP, unchanged.** No weighted composite score exists anywhere (verified by grep, not memory). `trusty-review`'s grade is single-dimension (LLM review only). Genuinely worth adopting, and the article's own Goodhart's-Law defenses (hard blocks bypass the score; re-weight from observed failure rates) should be designed in from the start rather than retrofitted.

4. **Operational → merge feedback — CORRECTED: GAP → PARTIAL.** This bundles two different loops that are in different states:
   - Review-outcome → suppression feedback (dismissed/ignored findings quiet future noise) **already exists** (`outcomes.rs`/#1421 above) — not a gap.
   - Production-operational telemetry (incidents, error budgets, retry storms) feeding back into the review/merge gate **does not exist** — nothing in the repo ties post-deploy signals to `trusty-review` or to `code-critic`. This half is the genuine, narrower gap worth adopting.

**New gaps surfaced during verification that were not in the original inference-only list** (the article names these as core techniques; none exist here, and none were previously flagged): mutation testing, property-based testing, SBOM generation, an explicit package-install approval gate (anti-slopsquatting), and AST-level semantic diff. These are legitimate additions to the adoption backlog, not corrections to prior claims — they simply weren't considered before.

## Corrected / confirmed non-adoptions (with rationale)

1. **Radar-only, no-human-in-the-loop merge — CONFIRMED, rationale sharpened.** Not "we haven't gotten there yet": `trusty-review` deliberately posts as `COMMENT` only ("Phase 1... never API-level blocking," `posting.rs`), and branch protection independently requires a human approving review on every PR. This is a considered, code-evidenced decision to keep a human at the merge trigger, consistent with this project's own "never merge red/pending CI" and "admin-merge only on green" doctrine — stricter than the article's own recommended 3–7% HITL rate, by choice.

2. **Canary rollout — REFRAMED: not a risk-based rejection, an architectural non-fit.** The prior framing implied we evaluated and declined it. In fact `trusty-tools` ships CLI binaries and library crates via GitHub Releases/crates.io, and its services run as single-tenant local daemons on developer machines (loopback-only doctrine). There is no shared multi-tenant traffic to ramp and no baseline population for a Mann-Whitney canary comparison — the technique doesn't have a target to apply to here, independent of any risk judgment.

3. **Trust-score-around-red-CI — CONFIRMED, and shown to be enforced at more layers than assumed.** It isn't just a stated policy; it's structurally enforced at (a) live GitHub branch protection (required status checks, no admin bypass of failing checks), (b) `trusty-mpm` driver policy (`policy.rs` T2 gate treats CI-red as an unconditional block, not a weighted input), and (c) documented project convention (memory: admin-merge-only-on-green). Rationale: red CI is an absolute veto, never averaged into a score that could out-vote it — this directly guards against the exact Goodhart's-Law failure mode the article itself warns about.

## What changed vs. the earlier inference-only analysis

This is the key finding: **the earlier inference-only pass underestimated how much of the article's "meta"/HITL-policy layer we already have, and overestimated how clean the "critic calibration" gap was.**

- **Biggest correction:** a near-verbatim implementation of the article's policy-based HITL trigger (a destructive-operation keyword deny-list forcing an always-escalate tier, `crates/trusty-mpm/src/driver/policy.rs`) already exists in this codebase, explicitly modeled on a documented tiered-autonomy spec (`SESSION_MANAGER_DRIVER_AGENT.md` §4). The inference-only analysis had no visibility into this and implicitly treated the whole "meta" category as unaddressed. The caveat that keeps this from being a full HAVE: it governs the trusty-mpm session-manager driver's action-acceptance, not this repo's own PR/CI pipeline — so there's a real follow-up (wire the same policy into `trusty-tools`' own gate) rather than a build-from-scratch gap.
- **Second correction:** "critic calibration / escape-rate tracking" was named as a clean gap; it isn't. `trusty-review` already has a real recall/precision calibration harness and a live outcome-based suppression-feedback loop. The genuine residual gap is narrower and different in kind: calibration for the `code-critic` agent specifically (explicitly marked TODO in its own reference doc), and true production-escape-rate tracking (incidents traced back to a prior approval), which neither existing mechanism does.
- **The two non-adoption rationales needed correcting, not the conclusions.** Radar-only-no-human turns out to be a deliberate, code-evidenced Phase-1 design choice (advisory-only posting) rather than an unexamined default. Canary rejection is better described as "doesn't apply to a CLI-tool/local-daemon product" than "we weighed it and said no" — there's no risk tradeoff being declined because there's no multi-tenant traffic surface to apply it to.
- **Unchanged:** flaky-test quarantine and the composite per-PR risk line remain confirmed, real gaps exactly as originally stated. Trust-score-around-red-CI remains confirmed as a non-adoption, now with stronger, multi-layer evidence than the original inference assumed.
- **Net new information:** mutation testing, property-based testing, SBOM generation, a package-install approval gate, and AST-level semantic diff are all genuinely absent and were not part of the original four-gap list — worth adding to any resulting backlog.

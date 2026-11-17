# Reflektor vs trusty-review

Date: 2026-11-17. Reflektor commit: `4490b50919502d490c208c5b0a518e2a1288c2eb` (`bashovski/reflektor`, shallow clone, read only, not executed). Reflektor paths below are relative to that repo. trusty-review paths are relative to `crates/trusty-review/`.

## 1. What reflektor does

A skill for coding agents (Claude Code, Codex, Cursor, Gemini CLI, Copilot) plus a local daemon. It turns a branch into a browser review report for a human reviewer. Input: git repo state (branch vs base, uncommitted files, optional coverage files, optional PR). Output: `report.json` rendered by a React viewer (`viewer/src/components/*`), with an inbox and optional upload to reflektor.dev (`docs/reflektor.md`).

Stack: TypeScript scripts run with Node 18 (`skill/scripts/*.mts`, 8.8k lines), daemon at `skill/scripts/daemon.mts` (127.0.0.1:7337), viewer in React/Tailwind, distributed via `npx skills add` or a signed installer. No model API is called by reflektor itself. The host agent is the reviewer, and `skill/SKILL.md` is the prompt.

## 2. How it reviews

- **Pipeline** (`skill/SKILL.md` steps 1-9): pick base, find coverage, start project checks in the background, `collect.mjs`, agent reads diffs and writes `report.json`, `build.mjs` validates it, `review.mjs open` publishes, a waiter blocks until the human comments, then a fix loop.
- **Context gathering**: deterministic scripts do the facts. `collect.mts` computes diff, per-file patch coverage (`parseCoverage`, line 815), generated/lockfile flags (`LOCKFILES`, `GENERATED`, lines 27-43) and base-branch guessing (lines 162-190). The agent searches the repo for callers of changed symbols (SKILL.md step 4, "Who else is affected") and reads the CI config to find lint/type/test commands (step 2b).
- **Deterministic safety rules** on added lines (`collect.mts:627-800`): 11 secret regexes with placeholder and test-path exclusions, committed `.env`/key files, and migration rules (drop table/column, NOT NULL without default, rename, type change, write without WHERE, non-concurrent index; rollback sections excluded). Every high/medium hit must appear in the report, or `build.mts:261` warns.
- **Prompting**: one long skill prompt with a fixed JSON schema (`skill/schema/types.ts`) and a style guide (`skill/style-guide.md`). Output is judgment about the change (title, summary, sections with `review: must|skim|skip`, highlights of kind behavior/risk/contract/decision/gap/affected, flows, schema, contracts, plan check, test gaps), not a list of defects.
- **Multi-pass / agents**: optional "Subagent mode" (`skill/subagents.md`): one subagent per section, at most 8, returning JSON. The parent writes flows from the subagents' `steps`. Used only for 3+ sections or about 400 changed lines.
- **Filtering / validation**: `build.mts` checks the agent's output against the repo. It drops refs to files not in the diff or lines outside the file (140-154), drops contract names absent from the file (298-317), marks flow steps "unverified" when the symbol is not found (291), warns when a risk/gap has no `fix` (183), caps highlights at 5 per section. There is no finding dedup and no model-based verification.
- **Severity**: `high|medium|low` only on the deterministic safety findings (`schema/types.ts:49`). Agent highlights use kinds, not severity. There is no verdict or grade.
- **Output**: browser report, `pr-description.md`, optional publish to reflektor.dev (`skill/scripts/reflektor.mts`).
- **Feedback loops**: reviewer comments (`fix|question|nit|accepted|decision`, `schema/types.ts:327`) go back to the agent through the waiter. The agent posts a plan, the human approves, the agent fixes and reports per-comment status. `collect --update` starts the next round and moves every ref in `report.json` through the new hunks (`collect.mts:534`, `moveLine`; carry loop around 560-600). `accepted` means "do not flag again".
- **Project checks** (`skill/scripts/checks.mts`): runs the repo's lint/typecheck/tests in the background and shows results in the report. It refuses to run when the branch has commits by untrusted authors until the user says yes (`checks.mts:234`, `repo.mts` `untrustedAuthors`).

## 3. How trusty-review reviews today

Rust crate: CLI, MCP stdio server (`review_pr`, `review_diff`, `review_health`, `src/mcp/tools.rs:70,143,174`), webhook service. It calls LLMs itself (Bedrock, OpenRouter, Fireworks; `src/llm/`). `run_pipeline` (`src/pipeline/runner.rs:221`): load diff, noise filter (`pipeline/diff_analyzer/mod.rs:47`), context retrieval from trusty-search plus optional issue/doc/symbol/call-graph/linked-issue context (`pipeline/optional_context/`), LLM review, map-reduce per file for large diffs (`pipeline/mapreduce/mod.rs:69`), parse findings, hygiene filters (`finding_hygiene.rs:192`, `quote_admission.rs`, `absence_claim.rs`, `claim_grounding.rs`, `evidence_admission.rs`), coverage floor (`runner.rs:726`), citation gate that drops findings whose cited line does not hold the quoted code (`citation_gate.rs:1`), per-finding verifier round CONFIRMED/REFUTED/UNVERIFIABLE (`verify.rs:213`), severity-anchored grade and verdict (`grade.rs:582`), shallow-review grade cap (`runner.rs:814`), inline comments (`runner.rs:870`), post or log. Per-PR claim store prevents duplicate reviews (`store/dedup.rs:226`). Output is a verdict, letter grade and findings, aimed at a PR gate, not a human walkthrough.

## 4. Techniques trusty-review lacks

| Technique | Reflektor evidence | trusty-review gap |
|---|---|---|
| Deterministic secret and migration scan on added lines, fed to the reviewer as mandatory facts | `skill/scripts/collect.mts:627-800`, enforced by `skill/scripts/build.mts:261` | No secret or migration rules in `src/` (grep: only test fixtures). Detection is LLM-only. |
| Running the project's own lint/typecheck/tests during review and folding results in | `skill/scripts/checks.mts`, SKILL.md step 2b, 7b | Only an LCOV file is read (`runner_coverage.rs:1`). No test or lint execution. |
| Trust gate before running code from other authors' commits | `skill/scripts/checks.mts:234`, `skill/scripts/repo.mts` | Not needed today because nothing is executed. Required if the previous row is adopted. |
| Prior-round memory: carry refs through new hunks, per-comment status, `accepted` suppresses re-flagging | `skill/scripts/collect.mts:534` (`moveLine`), `schema/types.ts:327,376` | Each review is stateless. `store/dedup.rs` dedups review claims, not findings across pushes. No re-review of the same PR remembers dismissed findings. |
| Diff-vs-plan check (planned items done/partial/missing, plus unplanned extras) | SKILL.md step 5 "plan", `skill/schema/types.ts` | Conformance gate checks only explicit contradiction of a ticket's method (README.md:618), default off. No completeness or scope-creep check. |
| Structural change summary: contracts (types/enums/constants added, changed, removed), flows (entry point to storage), DB schema diff | SKILL.md step 5, `build.mts:244-323` | Findings only. `report` subcommand (README.md:767) is a separate repo-analysis feature. |
| Existence check on the model's identifiers at build time (symbols named in flows/contracts must occur in the cited file) | `skill/scripts/build.mts:291,298-317` | Citation gate checks quotes and lines, not named symbols in prose (`citation_gate.rs`). Partial overlap. |
| Review-effort triage per file (`must`/`skim`/`skip` with reason) and reading-time estimate | SKILL.md step 5; `build.mts` `reviewMinutes` | Diff analyzer drops noise but does not rank the remainder. |
| Repo-local config for target areas and generated globs | SKILL.md "Repo config", `.reflektor.json` | `.trusty-review.toml` exists (README.md:699); target-area mapping does not. |

Reflektor lacks, and trusty-review has: verifier pass, citation gate, hygiene filters, grade/verdict, PR posting, embedded model client, benchmarks. Do not port in that direction.

## 5. Ranked adoptable ideas

Rank is gain divided by effort.

| Rank | Idea | Effort | Expected gain | Notes / risks |
|---|---|---|---|---|
| 1 | Deterministic safety scan (secrets, migrations) over added lines, injected as context and as a floor: a high hit the reviewer ignores becomes a finding | S | Catches leaked keys and risky migrations with zero model cost; verified facts feed the verifier | Port regexes with their placeholder and test-path exclusions (`collect.mts:627-640`). Secret values must be masked in output. Findings must pass the citation gate unchanged. Pick the Rust approach: new module under `src/pipeline/`, reuse `diff.rs` hunks. |
| 2 | Prior-review memory: persist findings per PR head and, on re-review, suppress dismissed/fixed ones and report which earlier findings are resolved | M | Cuts repeat noise on push-after-push PRs; enables "round N" behavior | Needs a store beyond `store/dedup.rs` keyed by file + quote hash, since line numbers move (`moveLine` approach). Needs a signal for "dismissed" (GitHub resolved thread). Risk: suppressing a real finding. |
| 3 | Plan/ticket completeness check: items done/partial/missing plus extras, using existing issue/linked-issue context | M | Finds missing requirements and scope creep, which the diff-only reviewer misses | Input already exists in `optional_context/linked_issues.rs`. Add a separate model pass and output section, not a finding kind. Risk: model hallucinates missing items. Gate behind the citation/verifier rules. |
| 4 | Run project checks (lint/typecheck/tests) in a sandbox and attach failures as findings or context | L | High signal (real failures) | Executes untrusted PR code. Needs sandbox and an authors-trust gate like `checks.mts:234`. Heavy for a webhook service. Local `--local-diff` surface only is a safe first cut. |
| 5 | Build-time symbol existence check on identifiers in finding prose | S | Small drop in fabricated-API findings beyond citation gate | Overlaps `citation_gate.rs`, `claim_grounding.rs`. Do only if corpus shows misses. |
| 6 | Contracts/schema summary section (changed types, enums, DB tables) | M | Reviewer orientation | Human-report feature; low value for a PR gate. Fits the `report` subcommand more than `review_pr`. |
| 7 | `must/skim/skip` file triage as reviewer prompt input | S | Focus on large diffs | Map-reduce splitter already budgets per file (`mapreduce/splitter.rs`); marginal. |

## Sources not read

Reflektor `viewer/` components, `daemon.mts` (2129 lines), `screenshots.mts` and the platform API were not read in detail. Claims above about them rest on `README.md`, `DESIGN.md`, `docs/reflektor.md` and `SKILL.md`.

# Content Changelog

Changes to the instructional-content bundle (`content-vX.Y.Z` releases,
ADR-0064). Crate changes are recorded in each crate's own CHANGELOG.md.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
Sections are rolled up from `content/changelog.d/` by
`scripts/assemble-changelog.sh content <version>`.

---

## [2.3.3] — 2026-11-01

### Added

- The `architect-strategy` skill (0.2.0) says each factory board gets one GitHub Projects v2 status update per day, posted by a single owning session when several share the board, and that sweeps check the latest update's date before posting.
- `prototype-then-port` skill (0.1.0): prototype in Python while the shape changes, then port the settled parts to Rust. The entry point gives the port trigger (stable, measured cost, named consumer), an eight-step path and a checklist; references cover type mapping, idioms, parity and shadow runs, PyO3 versus a whole binary, and silent behavior changes. Owner ruling 2026-10-30.
- The Architect instructions carry rule r-192 (self-improvement is a routine task; the daily pass checks that every scheduled routine ran, `architect-improvements` 0.2.0) and rule r-193 (every owner ask is classified as Throwaway, Q&A, Task, Decision or Goal; answers go to the `questions/Q..` collection in `architect-dashboard`).
- `instructions/trusty-review/prompts/principles_addendum.md`: trusty-review's review-principles addendum, moved out of the `trusty-review` binary byte for byte. Like the other reviewer prompts it carries no frontmatter, because the file is sent to the model verbatim, and it declares no version, so it is not a manifest member (#9991).
- `neon-postgres` skill (1.0.0): rules first (never hand-set the Neon integration's `DATABASE_URL`, `DATABASE_URL_UNPOOLED` or preview-branch variables), how the Vercel integration and preview-branch databases work, pooled vs direct strings, drizzle migrate/push/baseline against Neon, read-only inspection, and gated production changes.

### Fixed

- The `tm-capabilities` doctor reference describes the `events` row as it now behaves: it warns on a host without the trusty-events bus, and its not-running remedy is `tctl start trusty-events` only, not `tctl install trusty-events` (#10151).

### Changed

- The ticketing agent runs `tm issue board add <N>` after `gh issue create` instead of passing `--project` or `--add-project`, and the Architect provisioning checklist runs `tm issue board check` (#10277).
- The `tm-architect-setup` skill says `tm fleet status` also exits 1 when its `binding` row is `UNBOUND` (#10448).
- The tm-secrets skill and the secrets-manager agent say an agent may `set`, `import` and locally `copy` a key with no deny, and name the new `agent_key_denied` refusal beside `agent_use_refused` (#10459).
- The tm-capabilities MCP tool catalog lists the optional `parent_session_id` argument of `session_new`, which the `tm serve --stdio` bridge sets (#10476).
- The trusty-mpm output styles, the session-management skill and the sessions architecture doc state the new session-name rule: bare `tm-<project>` first, a `-02`, `-03`, … counter only on a duplicate (#10478).
- The tm-capabilities CLI catalog describes `tm tui` as opening on the console home screen, with one tile per service and Enter on MPM opening the projects dashboard (#10491).
- `tm-session-pause` and the `tm-capabilities` workflows reference say the session-ref push now happens only on a GitHub-confirmed private `origin` (#10614). A public, non-GitHub or unconfirmed `origin` gets no push and a `ref_error`; the agent reports it and never pushes the ref by hand.

## [2.3.0] — 2026-10-24

### Added

- `instructions/architect/architect.md` holds the Architect's instructions in one file: the Architect operating manual (mandate, strategic management, workflow, operating rules, skill index) plus the parts of the eight `instructions/supervisor/` sections the manual does not state (relay protocol, evidence labels, decisions, question ids, heartbeat, tool priority). `instructions/architect/core.md` holds the Architect core: the hard limits of both sources, no `MEMORY.md` or other static memory file, `AGENTS.md` as the only owner override and unable to override the core, the memory and search protocol, and the Completion Standard bar. No binary reads them yet, and the eight legacy sections stay in the bundle (#8436).
- `tm-capabilities` documents the `egress_guard` row of `tm doctor` (#8454).
- `tm-memory-import` skill (1.0.0): the kuzu-memory import runbook with the refused-row review — `trusty-memory import kuzu --dry-run --export-refusals`, `import kuzu review record|status|finish`, then `--review-ledger`, behind the READY FOR IMPORT / GO gate. The reviewer reads masked rows only and never the raw store ([#8625](https://github.com/bobmatnyc/trusty-tools/issues/8625)).
- The `tm-capabilities` doctor reference describes the new `sccache_socket` row: whether the sccache unix-socket server for unsandboxed cargo builds answers on `~/.trusty-mpm/sccache.sock` when `build.sccache` is true, and why sandboxed sessions never use it (#8928 step 3).
- The `code-review-standards` Fail-Open Check gains two checks: `cmd | grep -q` under `set -o pipefail` (SIGPIPE makes a found match read as a failure), and a pre-filtered `gh` test stub. The PM workflow section now says the check has seven items (#8974).
- `agents/trusty-code/` (12 agents) and `instructions/trusty-code/skills/` (28 skills with their references): trusty-code's own agents and default skill catalog, moved out of the `tcode` binary. Each agent declares `metadata: {version: "0.1.0"}` and each skill's top-level `version:` became `metadata: {version}`; the text is otherwise unchanged. The agents sit beneath `agents/`, so the shared roster does not list them; the skills sit beneath `instructions/`, so `tm` does not deploy them (#9013).
- `agents/trusty-agents/agents/` (the bundled `tagent` roster: flat agents and directory packages) and `agents/trusty-agents/workflows/` (the workflow definitions): trusty-agents' own content, moved out of the `tagent` binary. Each `.md` declares `metadata: {version: "0.1.0"}`; the only other edit is a comment in `writing-assistant/agent.toml` that named the retired embed. They sit beneath `agents/`, so the shared roster does not list them (#9014).
- `instructions/trusty-review/prompts/` (`system_prompt_stock.md`, `system_prompt_coverage_gating.md`): trusty-review's reviewer system prompts, moved out of the `trusty-review` binary byte for byte. They carry no frontmatter, because the files are sent to the model verbatim; under `instructions/` a version is optional, so they are not manifest members (#9015).
- `instructions/trusty-review/templates/` (`report-technical-dd.md`, `report-technical-dd-cast.md`), `instructions/trusty-review/templates/review/strict-security.md` and `instructions/trusty-review/voices/duetto/voice.toml`: trusty-review's report templates, review template and voice package, moved out of the binary byte for byte. They declare no version and are not manifest members (#9015).
- The `tm-capabilities` CLI reference lists `tm secrets resolve`, which prints one key's value inside a `tm secrets exec` grant (#9072).
- `instructions/architect/skills/tm-fleet-check.md`, `instructions/architect/skills/tm-context-refresh.md` and `instructions/architect/templates/CLAUDE.md`: the Architect's seed skills and `CLAUDE.md` template, moved out of the `tm` binary. The skills declare `metadata: {version: "0.1.0"}`; their text is otherwise unchanged, and an untouched copy seeded from the old text is refreshed on the next Architect launch (#9132).
- `tm-capabilities` documents the four `search_project_*` rows of `tm doctor` (#9169).
- `rust-runtime-performance` skill (1.0.0): runtime measurement discipline, allocation and data movement, bounded async concurrency, tuning as profiled experiments; `rust-engineer` loads it on demand (#9296).
- The Architect leak gate (`scripts/check_architect_subproject.sh`, and the new `scripts/check_content.py leaks`) now rejects owner names, GitHub orgs outside an allow-list, `.local`/`.lan`/`.internal`/`.ts.net` hostnames, Slack ids and non-example emails, over `content/instructions/architect/**` as well as the Python sub-project and the shipped assets. Both tools read one pattern file, `scripts/architect_leak_patterns.txt`, and fail closed on a missing pattern file, a missing scan root, an empty pattern or a grep error (#9996).
- Eight Architect skills under `instructions/architect/skills/`: `architect-tmux-ops`, `architect-operating-rules`, `architect-workflow`, `architect-search-memory`, `architect-github`, `architect-strategy`, `architect-completion-standard` and `architect-owner-model`. Each one the operating manual names now ships, so no `(details: …)` pointer in the manual is dangling (#9997).
- `skills/tm-ticketing.md` gains one section, "Every issue: milestone, relationships, state": every issue filed or touched carries a milestone, its native relationships (parent and sub-issues, blocked-by and blocking, linked PR) and its state, the project `Status` field and the `status:*` label moved in step. With no usable milestone set the agent proposes one to the PM. A checklist runs before the report. The `ticketing` agent (and the trusty-code copy) calls all three mandatory and reports them per issue; `tm-epic`, its manual procedure, `tm-issues-prune`, `tm-workflow`, `tm-bug-reporting`, `tm-delegation-patterns`, `tm-postmortem`, `tm-tool-usage-guide` and `rust-delivery-workflow` point at the section. The `ticketing` agent drops its "Bidirectional Linking" section, whose prose parent comment contradicted native relationships (#10045).
- 87 bundled skill, agent, doc and section rules now carry a stable rule-id marker, a line `<!-- rule:<file>/<slug> -->` above the paragraph it pins. Bundle tests check the marker instead of the wording, so rewording a sentence beside a marker breaks no test; removing a marker does. The agent composer and the PM prompt fold drop marker lines, so deployed agents and the delivered PM prompt are unchanged. Output styles carry no markers, because they are delivered verbatim on every PM turn (#10058).
- `tm-adr` skill (2.1.0): generic create/supersede/amend steps on `docs/adr/`, an `init` procedure that bootstraps a project's ADR setup as one staged pull request, and a `status` procedure that reports each managed copy against the deployed skill with five outcomes (`up to date`, `update available`, `locally edited`, `unmanaged`, `tm behind`) ([#10080](https://github.com/bobmatnyc/trusty-tools/issues/10080)).
  - `references/adr-standard.md` is the one source for when to write an ADR, metadata, lifecycle, numbering, vetting and governance; `references/README.md`, `template.md`, `INDEX.md`, `0001-record-architecture-decisions.md` and `adr-check.yml` are the files `init` seeds.
  - `scripts/check_adr.sh` (structural ADR gate, `--self-test`, `--verify-self` over a `body-sha256` header), `scripts/adr_init.sh` and `scripts/adr_status.sh`; each needs bash 4+ and carries its own self-test.
  - The skill no longer references repository-specific documents or an ADR-standard spec outside the skill; the layout-ADR paragraph is removed.
- The `architect-dashboard` Architect skill: the served dashboard page, its data model, the per-pass update, the owner answer flow and the page controls; the Architect manual indexes it and rule r-181 points at it (#10161).
- Architect skills: six more skills ship in trusty-tools form - `architect-review`, `architect-drift-check`, `architect-project-provision`, `architect-macos`, `architect-improvements` and `architect-host-admin`. The operating manual indexes them, and the improvement, resource, subsystem-health, provisioning, review, drift and mandate rules point at them (#10161).
- The `tm-capabilities` skill's `references/workflows.md` gains section 6, the field reference for `session_context_pause` and `session_context_catchup`, moved out of their MCP descriptions (#10300).
- `data-design` skill (0.1.0): a decision table from data type and question to chart form, encoding, colour and interaction, nine hard rules, and progressive references for chart selection, colour, encoding and labels, hierarchy, time/flow/network/geo, empty and missing data, and accessibility.
- `data-design` ships `scripts/check_palette.py`, a stdlib-only offline palette checker (CVD simulation, OKLCH lightness band, WCAG contrast), and vendored ColorBrewer hex subsets (Apache-2.0) with provenance and the retained notice.
- `data-designer` agent (0.1.0): extends `base-agent`, has no web tools, and returns a design spec for an engineer; chart data never leaves the host.

### Fixed

- `version-control` and `tm-workflow` read the auto-merge setting with `gh api repos/<owner>/<repo> --jq .allow_auto_merge`. The previous `gh repo view` field is rejected by gh 2.98, so every read failed. An empty answer means the token cannot read the setting and counts as `false` (#9747).
- `tm-session-management` 1.2.1 and `tm-init` 1.0.1 teach `tm session catchup --full --all-projects` for the cross-project digest, since `--all-projects` alone is now refused, and describe the lean default (the resolved snapshot plus live `git status`) (#9883).
- The `security` agent's credential scan treats the required `Claude-Session: https://claude.ai/code/session_<id>` commit trailer as attribution, not a leak. Only that exact line, once, in a commit message's last paragraph is allowed; the same URL in a diff, a body line or a credential-shaped value still fails (#10056).
- The `version-control` agent's fallback line now reads "Fallback without `tm` (this path has no merge gate):"; "which" wrongly referred to `tm` ([#10104](https://github.com/bobmatnyc/trusty-tools/issues/10104)).
- The `secrets-manager` agent now checks `command -v tm` before it names `tm hook --pm-guard`, and says what to do when `tm` is absent, so the probe-then-degrade lint passes now that it reads wrapped spans ([#10104](https://github.com/bobmatnyc/trusty-tools/issues/10104)).

### Changed

- The `tm-capabilities` doctor reference lists the `content_requires` row, which reports assets whose advisory `requires:` this `tm` does not satisfy and never blocks them (#8390).
- Shared assets that name a `tm` command now check `command -v tm` and say what to do without it: the `BASE-AGENT`, `BASE-ENGINEER`, `gcp-ops`, `local-ops`, `mpm-agent-manager`, `mpm-skills-manager`, `ticketing` and `version-control` agents, and the `condition-based-waiting`, `git-workflow` (both copies), `rust-build-performance`, `rust-delivery-workflow`, `self-improvement-loop` and `verification-before-completion` skills. `check_content.py tree` now refuses a shared asset that names a `tm`, `tcode` or `tagent` command — any inline span, or a fenced line, even after `&&` — before such a probe; only a negation that directly governs the span exempts it (ADR-0064 decision 6, #8390).
- `tm-capabilities` describes the `auto_memory` row of `tm doctor` as auto memory off for every session, with no trusty-memory fallback (#8436).
- The tm-capabilities doctor reference lists the `owner_config` row (#8988).
- The tm-capabilities `owner_config` row says the owner policy is applied at launch through a per-session `--settings` file, and that a deleted owner file's last-good record is retired (#8988).
- Every bundled agent description is now at most 100 characters, so the agent listing each session loads is about 1.9 KB smaller. The descriptions keep the specialty and the main technologies each agent is routed on (#8992).
- The composed PM prompt is 4,140 bytes smaller (bundled-fallback golden 26,881 → 22,741) and the default `trusty-mpm` output style 1,189 bytes smaller (7,987 → 6,798). Duplicated wording is cut, not rules: the tmux carve-out is stated once with P10, the connector table and `index_id` resolution order moved to `tm-cli-operations` and `tm-tool-usage-guide`, the model fallthrough to `tm-delegation-patterns`, and the agent-row reading guide to that skill's allowlist reference. The style's Error Handling, Standard Operating Procedure, TodoWrite, Commits and Response Format blocks are condensed; its PRIMARY DIRECTIVE and Communication floor are unchanged. Byte budgets now hold both: 23,200 for the composed prompt and 6,950 for the style (#8999).
- The Completion Standard core bar moved from the `workflow` instruction section to `core`, which no project override replaces; `workflow.md` keeps a pointer and `tm-workflow` keeps the full text (#9182).
- `rust-build-performance` 1.2.0 (smaller than 1.1.0): objective and comparison discipline, thin generic wrapper with `cargo llvm-lines`, generated-code measurement instead of a proc-macro rule, conditional sccache, `cargo check` default-members, crate splits versus file-size caps, feature resolution, macOS first-launch scan; host-specific figures removed. `rust-engineer` 0.1.1 gains a trigger for `rust-runtime-performance` (#9296).
- The tm-capabilities MCP tool reference describes the `resume_verify` field of `session_context_catchup` and the `resume_slot` field of `session_context_pause` (#9390).
- `tm-cli-operations`: the default `tm services` manifest starts trusty-search with plain `trusty-search start`; `start` rejects `--no-http` and `--port` (#9536).
- `tm-epic` no longer presents a prefix-less state label as normal; it states that a model mixing prefixed and unprefixed state labels is refused (#9580).
- `tm-ticketing` and the `ticketing` agent: when `tm issue transition` refuses a target, report the refusal to the PM or owner; never author or edit a repo `issue-state.yaml` and never swap the status label by hand (#9580).
- The tm-capabilities CLI reference lists the `tm secrets exec` verb (#9621).
- `tm-capabilities` documents that `tm doctor --fix --yes` parks the duplicates `search_project_duplicate_index` flags (#9664).
- BASE-ENGINEER gains a "Scope Discipline" rule (state the assumption; stop only on competing readings) and drops "No dead code in deliverables"; the edit-scope and unused-item rules move to the code-production-process stage-implement reference.
- The tm-capabilities skill and its CLI and MCP tool references list `tm dbatg` and the `dbatg` tool, regenerated for #9758.
- The `tm-memory-import` skill states what the pm-guard allows under #9794 and owner ruling D61: the dry runs of steps 1, 2 and 4, the `review record|status|finish` forms and the step-5 `--review-ledger` import, as one-line plain-word commands with an optional `2>&1 | tee` into the review root or the session scratchpad. GO, not the guard, holds the step-5 import back. `--python`, `--help` and a `--from` store path stay denied.
- The tm-capabilities CLI reference lists `tm architect` and its verbs `classify`, `input-state`, `poll`, `quiet` and `self-ctx` (#9822).
- The tm-capabilities skill and its MCP tool reference list the `ask_owner` tool, regenerated for the P4 change (#9856).
- `tm-session-resume` 2.0.0 teaches the lean resume default: the resolved snapshot (`resolved_session`) plus live `git_status`, with the full digest behind `full: true` / `--full`; the watermarked digest and its guidance are removed. The memory section says `session_context_catchup` re-reads the launch digest's sources with `full: true`, and the tm-capabilities CLI and MCP references are regenerated (#9883).
- The tm-capabilities skill and its MCP tool reference list the `sessions_chat` tool, regenerated for #9979.
- The Architect skills `instructions/architect/skills/tm-fleet-check.md` and `tm-context-refresh.md`, the Architect `CLAUDE.md` template and `skills/tm-architect-setup.md` name the `local/` records layout instead of `records/` (#9997).
- `tm-fleet-check` reads `local/instance.md`, `local/user.md` and `local/goals.md` at pass start, verifies the observation channel every pass, and checks the live work against the goals. `tm-context-refresh` adds the pause and resume semantics and the Architect's own-session refresh. No context threshold changed (#9997).
- The Architect manual `instructions/architect/architect.md` names `tm-fleet-check` and `tm-context-refresh` instead of the retired `trusty-*` names, and drops the skill-index rows, `(details: …)` pointers and doc pointers for skills and files that do not ship. Its relay marker is `[<Name> Architect HH:MMZ]`, with `Architect answer:` and `Architect question:` (#9997).
- One context bar, 250k tokens by default and set in the Architect `CLAUDE.md`, replaces the 50% and 40% thresholds in the manual, `tm-context-refresh`, `tm-fleet-check` and the `CLAUDE.md` template. It applies to the watched sessions and to the Architect's own session (#9997).
- `tm-fleet-check` sends relays through `scripts/relay.sh` and checks a shared pane with `scripts/posted.sh`. `skills/tm-architect-setup.md` lists the relay scripts, the voice guide and the `architect-*` skills that `tm fleet init` writes (#9997).
- The `tm-capabilities` skill lists 76 `tm doctor` checks; its doctor reference describes the new `events` row for the trusty-events bus socket (#10042).
- `skills/tm-ticketing.md` is now a progressive skill: the entry file drops from 1,035 to under 220 lines and holds the entry rules, the "Every issue: milestone, relationships, state" section unchanged, the Labels/Project/Milestone standard, and a "Where Each Task Lives" table. Every other section moved, unchanged apart from cross-references, to eleven files under `skills/tm-ticketing/references/` that load only when read. A pointer naming a `tm-ticketing` section resolves through the table (#10051).
- `skills/api-design-patterns.md`, `skills/git-workflow.md`, `skills/model-context-builder.md`, `skills/env-manager.md`, `skills/tm-cli-operations.md` and `skills/webapp-testing.md` are now progressive skills. Each entry file is under 500 lines (870 → 184, 646 → 418, 643 → 321, 605 → 219, 518 → 469 and 511 → 440) and carries a "Where Each Task Lives" table that links every file under its `references/`. The moved sections sit in eleven new reference files and change only in cross-references; a pointer naming a moved section resolves through the table. git-workflow keeps "GitHub Actions Spend", "Pulling After a Merge" and the other Common Workflows recipes in the entry (#10055).
- `skills/tm-workflow.md` is now a progressive skill: the entry file drops from 1,090 to 399 lines and holds the ownership boundary and handoff, the delivery chain, One Outcome One PR, the git security review, branch protection, the nine-field PR body, squash-merge, shipped PR defaults, delegating the PR, the safety-core table and a "Where Each Task Lives" table. Every other section moved, unchanged apart from cross-references and heading levels, to eight files under `skills/tm-workflow/references/` that load only when read (#10055).
- `skills/tm-delegation-patterns.md` is now a progressive skill: the entry file drops from 717 to 321 lines and holds the routing tables, retries, the delegation brief, long-wait delegation, PM Re-Engagement, the engineer closing instruction, fixed agent scope and a "Where Each Task Lives" table. Every other section moved, unchanged apart from cross-references, to seven files under `skills/tm-delegation-patterns/references/`. A pointer naming a section of either skill resolves through its table (#10055).
- The `tm-capabilities` doctor reference describes the `events` row's heartbeat probe: `ok` only when the bus answers a subscription, `fail` when it is hung, and `warn` when it is too old to accept `heartbeat_ms` (#10071). A stale socket now names both causes, an exited daemon or a hung one with a full backlog, and the remedy `tctl restart trusty-events`.
- The `tm-capabilities` doctor reference describes `hooks_missing_tm_group` as covering seven lifecycle events, now that tm registers `PostToolUseFailure` (#10095).
- The `tm-capabilities` skill's MCP tool reference now says the owner answers an `ask_owner` question by picking one of the given options, and that free text is not delivered back to the caller (#10127).
- The `tm-capabilities` skill's MCP tool reference now documents the optional `options` argument of `ask_owner`: 2 to 6 answer labels, posted numbered from 1 (#10127).
- The Architect manual (`instructions/architect/architect.md`) renames the owner's "asks" to "actions" in rules r-039 and r-181, defines the D, A and Q id prefixes, sends every decision to the dashboard instead of chat, and has r-181 link a readable brief from each non-trivial dashboard item (#10161).
- Architect skills: in `architect-operating-rules`, owner "asks" become "actions" and D/A/Q are defined. `architect-workflow` and `tm-context-refresh` name the Intent Ledger digest as the Architect's handoff, through `scripts/self-clear.sh --live`. `architect-strategy` adds the factory issue-tracking rule (#10161).
- Architect core: the verified checkpoint for a session with an Intent Ledger is a fresh ledger digest. For any other session it is a verified `/tm-session-pause` snapshot (#10161).
- The `tm-capabilities` skill lists 79 `tm doctor` checks; its doctor reference describes the new `gh_account_dir_tokens` row, which flags a gh token stored in a per-account `hosts.yml` (#10037).
- The `tm-capabilities` skill lists 80 `tm doctor` checks; its doctor reference describes the new `claude_json_secrets` row, which flags a credential value in an `mcpServers.<server>.env` block of the managed `.claude.json` (#10041).
- The tm-capabilities skill and its CLI and MCP tool references list `tm architect dashboard` and the `architect_dashboard` tool, regenerated for #10063.
- The `architect-project-provision` skill tells the PM to write hook commands as `/bin/bash "<abs>/scripts/notify-supervisor.sh"` and `/bin/sh "<abs>/scripts/datg-hook.sh" <session>`, never a bare script path (D73).
- The `tm-capabilities` CLI reference lists `tm issue board` and its verbs `init`, `apply`, `check`, `show` and `add` (#10276).
- `tm-capabilities` CLI reference: `tm supervisor` is now described as a 24/7 observer only; auto-resume moved to the daemon.
- `tm-secrets` (1.5.0) and `secrets-manager` (0.3.0) describe the #10386 rule: an agent may use every in-scope key unless the operator denied it; `set`, `delete`, `copy` and removing a deny are operator-only, and `agent_use_refused` covers both cases.

### Documentation

- Review agents (`code-critic`, `code-analyzer`, `security`) and the `tm-secrets` skill state that a review never runs `tm secrets`, the `trusty-secrets` CLI or any live-socket call (#7524).
- The `tm-secrets` skill documents `tm secrets resolve <key>`: what it prints, the `TRUSTY_SECRETS_TOKEN_FD` grant token, and its exit-1 cases (#9072).
- The `tm-capabilities` CLI reference lists `tm fleet relaunch` (#9092).

## [2.2.1] — 2026-10-10

### Fixed

- The `research` agent returns its findings as text in its final response instead of saving them under `docs/research/`, which the harness refuses for subagents; it writes a file only when the PM brief names the path (#8566).

### Changed

- The tm-capabilities doctor reference says the trusty-search daemon binds no TCP port from 0.59.0, and that `tcp_listeners` fails on any trusty-search TCP listener (#9214, #9542).
- The tm-capabilities CLI reference lists the `tm memory rename` verb (trusty-memory's `palace_rename`, #9544).

## [2.2.0] — 2026-10-09

### Added

- `tm-capabilities` lists the `tm secrets` command and its `set`, `list`, `remove`, `import`, `copy` and `doctor` verbs; the top-level command count is 67 (#7521).
- `tm-capabilities` lists the `secrets_get_ref` MCP tool, its two arguments and its names-only answer; the MCP tool count is 36 (#7522).
- `tm-capabilities` documents `tm worktrees [--json] [--no-size]` and the `worktree_registry` row of `tm doctor` (#8994).
- `tm-session-management`, `tm-session-pause` and `tm-session-resume` tell the PM to record session resume state under `ws:<session>/resume` and PR state under `pr:<n>/state` with `tm memory remember --fact-key` (or the MCP tools' `fact_key`), so recall returns the current state instead of stale snapshots (#9142).
- The supervisor's decisions section states the question-id rule: two lowercase hex digits, next id is the last plus 1 in hex with `ff` wrapping to `01` and `00` never used, open ids skipped, and uppercase, one-digit and prefixed forms forbidden (#9289).
- The `tm-capabilities` CLI reference lists `tm sessions rebind <id-or-name> [--tmux <session>]` and `--all`, which re-bind a session record to its live tmux pane after a tmux server replacement (#9313).

### Changed

- `BASE-AGENT` credential rule names the owner-bound per-repo gh identity route, `docs/reference/environment-variables.md#per-repo-gh-identity` (#8557).
- `secrets-manager` no longer documents unbuilt `tm secrets` verbs or 1Password/Keeper backends. It works by vault and key name or `secret://` reference only, never calls `secrets.set` or hand-rolls a socket call, and reports `vault_out_of_scope`, `remote_host_unsupported` and `agent_use_refused` to the PM (#9068).
- The `tm-secrets` skill describes what `trusty-secrets` 0.1.0 ships (Keychain backend, own-vault scoping on github.com remotes, the names-only index, the `secrets.*` socket methods), lists each refusal kind with its action, and states the value rules as numbered imperatives (#9068).
- `local-ops`, `vercel-ops` and the `tm-secrets` skill list Vercel env var names only, filtered at the source with `vercel env ls <env> | awk 'NR>1{print $1}'`; `--json` and the unfiltered table are forbidden (#9158).

### Documentation

- The `tm-capabilities` doctor reference says the `content` row names the manual install (`gh release download <tag>`, then `tm content install --from <dir>/<tag>.tar.gz`) and never fetches the release itself (#9396).
- The `tm-capabilities` CLI reference says `tm memory forget` deletes a drawer by id or by slot (`--fact-key`) ([#9340](https://github.com/bobmatnyc/trusty-tools/issues/9340))
- The `tm-capabilities` CLI reference lists `tm memory forget` ([#9340](https://github.com/bobmatnyc/trusty-tools/issues/9340))
- The `tm-capabilities` CLI reference gives `tm services url` an example on port 7880, not trusty-search's retired 7878 ([#9543](https://github.com/bobmatnyc/trusty-tools/issues/9543))
- `tm-cli-operations` says `tm services` probes trusty-search by calling `search.health` on its Unix socket (`TRUSTY_SEARCH_SOCKET`, else the data directory): UP only when the socket answers, DOWN with the dial error otherwise. `tm services port|url trusty-search` exit 1 naming the socket, and an old static port-7878 `services.yaml` entry is read as the socket probe with one `WARN` ([#9543](https://github.com/bobmatnyc/trusty-tools/issues/9543))
- `tm-doctor` names the trusty-search check as reached over its Unix socket, not port 7878 ([#9543](https://github.com/bobmatnyc/trusty-tools/issues/9543))
- `tm-circuit-breaker` shows a raw socket probe, not a `curl` to port 7878, as the forbidden PM Bash example ([#9543](https://github.com/bobmatnyc/trusty-tools/issues/9543))
- `tm-epic`, `version-control`, `tm-workflow` and `tm-ticketing` state the phase PR title format `<type>(<scope>): [EPIC_N PHASE_M] <subject>`, with the tag after the conventional prefix, and that `tm pr open` applies the tag itself ([#9571](https://github.com/bobmatnyc/trusty-tools/issues/9571))

## [2.1.1] — 2026-10-05

### Added

- The 43 agent sources (including the five compose-only `BASE-*.md`) now live
  under `content/agents/`, and the four harness-understanding docs under
  `content/instructions/harness_understanding/`, moved from
  `crates/trusty-agents-common/src/assets/` (#9011).
- Every agent declares `metadata: {version: "0.1.0"}`; `content/manifest.toml`
  lists all 43 as members under bundle version 0.2.0 (#9011).
- trusty-mpm's skills now live under `content/skills/`, its PM instruction sections, supervisor sections, instruction package and schema under `content/instructions/`, its output styles under `content/instructions/output-styles/`, its SM instructions under `content/instructions/sm_instructions/`, and the two bundled docs under `content/instructions/docs/`, moved from `crates/trusty-mpm` (#9012). The bundle paths are unchanged except the new `instructions/docs/`.
- Every skill entry point declares `metadata: {version}` (its former top-level `version:` moved under `metadata:`; `tm-doctor` starts at 0.1.0); `content/manifest.toml` lists the 57 skills as members under bundle version 2.1.0, the highest member version (#9012).

### Fixed

- `code-critic` runs a consumer suite in the foreground with an explicit timeout and never backgrounds it to wait (#8320).
- `version-control`'s no-`tm` squash fallback passes the live PR title plus ` (#N)` as the subject and the live PR body as the message; a brief never overrides them (#8420).
- `vercel-ops` warns that a bare `vercel link` can create a stray project, and that `vercel env add --force` can leave a stale duplicate (#8321).
- `local-ops` checks the operator's egress IP against the allowlist before allowlist-dependent work (#8470).
- `version-control` and `tm-workflow` describe the `tm pr merge` checks gate: a failed check, no registered check, or a running check it would not wait for refuses the merge. A branch-caused failure is never waived, and `--allow-failing <check>` waives only a non-required check the brief names (#8614).
- `version-control`, `tm-workflow` and `git-workflow` no longer let a non-required pending check through, and no longer suggest `--admin` or raw `gh pr merge` around a refusal (#8614).
- The required-checks fallback read uses the branch endpoint's `.protection` field, because `/protection` answers 404 on an unprotected branch (#8614).
- `version-control` and `git-workflow` say that a passing `tm pr queue-check` clears the queue checks only; `tm pr merge` still applies the checks gate (#8614).
- `security` states the range's `git diff --name-only --diff-filter=d | wc -l` count and the files covered in a credential-scan report. Pure-rename, mode-only and binary files count as covered and are listed separately, binaries for manual review. A mismatch makes the report INCOMPLETE, never PASS (#8504).
- `version-control` reads `gh repo view --json autoMergeAllowed` before planning a merge and reports it up front. `tm-workflow` states that auto-merge is never assumed, and that a `false` answer means a direct merge once checks settle (#8640).

### Changed

- The PM prompt carries the owner's Completion Standard (ruling 2026-10-04): the four Done conditions, the fix bar (a finding blocks only for wrong behaviour, a security or credential exposure, data loss, a crash, hang or leaked process, a resource pileup, or a broken gate or CI), and the round limit (after the first review, at most one fix round and one delta review, then the Architect). Every other finding is one comment on the PR or issue, with no new round, issue or fix round. `tm-workflow` holds the full text.
- The round limit replaces "3+ review rounds is evidence to close and fold" in the PM prompt, `tm-workflow` and `tm-delegation-patterns`; a critic round counts against it.
- The PM prompt's Opportunistic Fixes rule now notes an easy fix in one comment instead of making it in the same work, and the QA gate and Fail-Open Check name the fix bar.
- `code-review-standards` makes `Parent` (one PR comment) the default disposition; `Fix here` is for fix-bar findings only.
- The supervisor (Architect) prompt carries the Architect's part: clear a PR when Done items 1-3 hold, never require a non-blocking MEDIUM or LOW fix, name over-polishing as drift, escalate only blocking findings.
- `code-analyzer` blocks only on a fix-bar class: Security is its own blocking priority, and Best Practices (SOLID, language idioms) is important, not blocking.
- The bundle's top level is the three content classes, `agents/`, `skills/` and `instructions/` (owner ruling 2026-10-01). Output styles, the session-manager instructions and the harness-understanding docs move from top-level `output-styles/`, `sm_instructions/` and `harness_understanding/` to `instructions/output-styles/`, `instructions/sm_instructions/` and `instructions/harness_understanding/`, with the same files. `bundle-manifest.toml` lists one `[[class]]` table per class and one `[[source]]` table per packaged source directory.
- The `tm-delegation-patterns` skill says who applies a local Terraform root whose state lives in the main checkout: the operator or `local-ops` dispatched there, while a worktree agent may run `terraform plan|apply -state=<that state>` (#8660).
- The four output styles, `BASE_SM.md` and `WHAT-IS-TRUSTY-MPM.md` point an agent inside the trusty-tools repo at `content/instructions/docs/WHAT-IS-TRUSTY-MPM.md`, and `mpm-skills-manager` names `content/skills/` as the bundled skill source, where #9012 moved them; the deployed path `~/.trusty-mpm/framework/docs/` is unchanged.
- `tm-capabilities` references regenerated: the bundled docs ship with the content, and the skill catalog is read from the content's `skills/` (#9012).


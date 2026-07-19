# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [Unreleased]

### Added

- `tm sessions rename <id-or-name> <new-name>` (and in-session `tm sessions rename <new-name>` via `$TM_MANAGED_SESSION_ID`) renames a managed session, updating the record's `tmux_name` and the live tmux session, with collision + invalid-name guards
- `tm sessions delete` now marks a session `--deleted--` (a new persisted `Deleted` state shown in the master list) instead of silently dropping the record; `tm sessions prune --state deleted` compacts the tombstones
- per-project gh account pinning via GH_TOKEN spawn-env injection (closes #3025) ([#3040](https://github.com/bobmatnyc/trusty-tools/pull/3040)) ([`53d0a54`](https://github.com/bobmatnyc/trusty-tools/commit/53d0a5433c9e42b2c49f11f943de8cfc2c7f4696))
- bridge custom remote+local MCP servers into fleet sessions at project/user scope ([#3033](https://github.com/bobmatnyc/trusty-tools/pull/3033)) ([`07cb73f`](https://github.com/bobmatnyc/trusty-tools/commit/07cb73f6bc236f753bb8e920ebab20b5cd9658c5))
- per-session asset staleness + tm sessions sync-assets ([#2980](https://github.com/bobmatnyc/trusty-tools/pull/2980)) ([`c8d137a`](https://github.com/bobmatnyc/trusty-tools/commit/c8d137a632275e85709e745176a39253e62d6e3b))
- daemon startup version banner + stale-daemon doctor check ([#2968](https://github.com/bobmatnyc/trusty-tools/pull/2968)) ([`abbcc45`](https://github.com/bobmatnyc/trusty-tools/commit/abbcc45c646cf821e8a9ed0828e66b11fc9b0a94))

### Fixed

- disclaim TCC responsibility on the TUI health screen's `[S]`-key `cargo run` spawn — the last undisclaimed spawn site in the TUI/session-launch call graph from the #2997 sweep; two remaining sites elsewhere in the crate tracked as #2997 part 6 (closes #3126)
- `tm ls` / the session picker no longer report running/attached sessions as `(stopped)`: the list handler now reconciles each session's displayed state against live tmux (`active`/`attached` when the tmux session exists, `stopped` only when it is truly gone) so the offered action (connect vs restart) matches reality
- disclaim TCC responsibility on session-launch spawns so Claude Code isn't attributed to trusty-mpm (closes #2997) ([#3037](https://github.com/bobmatnyc/trusty-tools/pull/3037)) ([`d481a0c`](https://github.com/bobmatnyc/trusty-tools/commit/d481a0cd7e264b20109e1c92122ba972db828222))
- teach user-scope MCP registration via tm mcp add in tm-cli-operations (closes #3020) ([#3021](https://github.com/bobmatnyc/trusty-tools/pull/3021)) ([`f6223aa`](https://github.com/bobmatnyc/trusty-tools/commit/f6223aacc1c030055440cea93c5ae52c1c4d231f))
- write_project_hooks uses write_json_atomic for torn-write parity (closes #2972) ([#3018](https://github.com/bobmatnyc/trusty-tools/pull/3018)) ([`32e805d`](https://github.com/bobmatnyc/trusty-tools/commit/32e805d609e15bbcc02ab4212f606e0e34b20293))
- tm doctor output_style content-drift + orphan detection ([#2976](https://github.com/bobmatnyc/trusty-tools/pull/2976)) ([`05a4a05`](https://github.com/bobmatnyc/trusty-tools/commit/05a4a059b1cd4d00f052e5a504141b8f2b621b84))
- explicit --url errors on unreachable daemon instead of silent fallback ([#2971](https://github.com/bobmatnyc/trusty-tools/pull/2971)) ([`c228ad9`](https://github.com/bobmatnyc/trusty-tools/commit/c228ad9a31093f79c27866d7ae15f0cb253e710f))
- entry-level hook matching for mixed groups + lifecycle triad in project-tier settings ([#2969](https://github.com/bobmatnyc/trusty-tools/pull/2969)) ([`de11649`](https://github.com/bobmatnyc/trusty-tools/commit/de11649700a79028771092cc22ea6343e4a8da84))

### Changed

- tm-adr skill v2.0.0 — formalize ADRs as first-class documentation artifact (opt-in → mandatory) ([#3172](https://github.com/bobmatnyc/trusty-tools/pull/3172))
- single shared tmux library; route trusty-mpm + trusty-agents through it ([#3017](https://github.com/bobmatnyc/trusty-tools/pull/3017)) ([`383b9f4`](https://github.com/bobmatnyc/trusty-tools/commit/383b9f475e781ef6049900f1630875e8ebf68264))

### Documentation

- in-flight issue/PR progress updates as standard PM workflow convention: bug diagnoses at triage time, progress comments at meaningful state changes, PR body freshness, and completion evidence (closes #3149) ([#3151](https://github.com/bobmatnyc/trusty-tools/pull/3151))
- long-wait protocol — disarm monitors on goal completion ([#2960](https://github.com/bobmatnyc/trusty-tools/pull/2960)) ([`e193414`](https://github.com/bobmatnyc/trusty-tools/commit/e1934140d38d6d9b847fef07676f29277683b220))

---
## [0.19.23] — 2026-07-17

### Added

- skill-port batch 1: 25 upstream `universal/` skills ported from `bobmatnyc/claude-mpm-skills` (main, 172 SKILL.md files) into trusty-mpm's bundled skill catalog (93 files, ~993 KB — 25 entry `SKILL.md` + 68 `references/*.md`), plus multi-file skill-directory deploy machinery: `bundle_all.rs` gains an `overwrite()` shorthand constructor so the ~164-entry `ALL` table stays under the SLOC cap; `skill_deployer.rs`'s `deploy_skills_filtered` now mirrors each stem's `references/*.md` files alongside its entry point via a shared `deploy_one_file` helper; `skill_source.rs`'s `materialize_skill_artifacts` writes nested rel_paths under a matching nested directory and a new recursive `prune_orphaned_skill_files` removes now-empty directories. Restores `skills:` frontmatter declarations on all 36 upstream-matched bundled agents, computed by unioning each agent's upstream `skills:` frontmatter (alias-resolved via the epic's 13-entry table) intersected with the 25 ported names (refs epic #2902, closes #2903)
- `tm generate capabilities[--check]` and the auto-generated `tm-capabilities` bundled skill: a dev-time subcommand walks the CLI command tree (clap introspection), the MCP tool catalog, the bundled agent roster, the bundled skill catalog, and a maintained-and-cross-checked doctor-check list, then writes a committed, byte-reproducible reference catalog (`skills/tm-capabilities.md` + 5 generated `references/*.md` files, plus one hand-authored `references/workflows.md` covering session launch / delegation / doctor triage / bug-report end-to-end flows). `--check` is the CI drift gate (`scripts/check_capabilities.sh`, wired into `.github/workflows/capabilities-drift.yml`) — a stale committed file fails the build instead of silently rotting. `BASE-AGENT.md` declares `skills: [tm-capabilities]` so every one of the 37 concrete agents picks it up via the existing DOC-42 union-across-chain compose merge; the existing `tm` skill gains one pointer line to it (complement, not supersede). Also fixes a latent bug in `materialize_skill_artifacts`'s mcp-shadow-guard stem computation that incorrectly widened the check to a multi-file skill's full nested `references/*.md` path instead of just the skill's own name (closes #2913)
- bundled `documentation-style` skill: a two-tier (`SKILL.md` + 6 `references/*.md`) SLD-grounded style guide covering the four-axis Why/What/Test + opt-in Spec-References inline doc model, per-artifact-type conventions (spec, README, file-level, class/module, method/function, block/inline), and context-economy guidance — defers to DOC-38 for the actual reference grammar rather than restating it (Annex B follow-up F2). `BASE-ENGINEER.md`'s Deliverables Checklist now points engineers at it, and declares `skills: [documentation-style]` in frontmatter so every engineer-family agent picks it up via the existing DOC-42 union-across-chain compose merge (closes #2911)

### Changed

- extract skills deploy/manifest/tiers machinery from trusty-mpm (refs #2892, #2818) ([#2916](https://github.com/bobmatnyc/trusty-tools/pull/2916)) ([`488602d`](https://github.com/bobmatnyc/trusty-tools/commit/488602dfa5cc75916f33c66b555832ce310b0025))
- extract agent compose/deploy/manifest machinery from trusty-mpm (refs #2892) ([#2909](https://github.com/bobmatnyc/trusty-tools/pull/2909)) ([`bb947ea`](https://github.com/bobmatnyc/trusty-tools/commit/bb947ead9e220a37b8902b1190d261295c23538b))

## [0.19.22] — 2026-07-17

### Added

- agent-bundled skills mechanism (DOC-42, [`docs/specs/agent-bundled-skills.md`](../../../docs/specs/agent-bundled-skills.md)): agents declare a `skills:` frontmatter list (block- and inline-style YAML both parse), the deploy pipeline co-deploys each referenced skill alongside its owning agent, `tm doctor` gains two new checks (`agent_skills` for dangling skill references, can Warn; `agent_skills_prose_hints`, always informational) bringing the doctor suite to 15 checks, and `tm agent list` / `tm agent show <agent>` surface each agent's resolved skill tier in the CLI. Per-agent deploy-failure isolation: `deploy_agents_filtered` no longer aborts the whole roster on one agent's compose failure — it logs the failure, records it in `DeployResult.failed`, and continues deploying the rest of the roster (closes [#2889](https://github.com/bobmatnyc/trusty-tools/issues/2889)) ([#2906](https://github.com/bobmatnyc/trusty-tools/pull/2906)) ([`4e4a2b9`](https://github.com/bobmatnyc/trusty-tools/commit/4e4a2b993547afef8b15bf29365163186a93f14f))

### Fixed

- restore code-critic's upstream review content as two bundled skills — `code-review-standards` (severity taxonomy, 80% confidence filter, APPROVE/WARN/BLOCK verdict protocol) and `contract-driven-testing` (three-level test pyramid derived from Code Contracts) — with the `code-critic` agent's `skills:` frontmatter updated to reference both (closes [#2890](https://github.com/bobmatnyc/trusty-tools/issues/2890)) ([#2900](https://github.com/bobmatnyc/trusty-tools/pull/2900)) ([`5214cd7`](https://github.com/bobmatnyc/trusty-tools/commit/5214cd700a48f4f688f8d596a40b822a37a6df4b))

### Fixed

- PM relays operator authority — scope injection-skepticism to untrusted content, not the dispatching PM ([#2844](https://github.com/bobmatnyc/trusty-tools/pull/2844)) ([`f0614b2`](https://github.com/bobmatnyc/trusty-tools/commit/f0614b293eb1fe0dbca6b20b093422b7cd608809))
- idle-park mitigation protocol for Agent-tool subagents ([#2835](https://github.com/bobmatnyc/trusty-tools/pull/2835)) ([`91f7c4d`](https://github.com/bobmatnyc/trusty-tools/commit/91f7c4d4d3fe6cc66c377b30ccff3a5425f96db5))

### Added

- idle-park mitigation protocol for in-conversation Agent-tool subagents — the layer below #2621's managed-session nudge, which cannot reach a subagent (no tmux pane to inject into). Amends the bundled prompt assets in their existing style: BASE-AGENT.md's "Foreground Execution" section gains a chunked-repoll subsection that also forbids the OPPOSITE failure (a 30-second blind-poll spam loop) — prefer the silent blocking `--watch`, size any manual sleep to the real wall-clock, message only on state change, one-shot `gh run view` diagnosis on overrun; the `version-control` and `local-ops` personas get a matching anti-spam bullet; PM_INSTRUCTIONS.md adds a "Parked-Subagent Detection & Nudge" protocol (recognize a parked stop by unmet-goal + backgrounded-wait language, `SendMessage` the SAME agent a resume nudge, prefer crate-scoped gates and blocking `--watch` to keep waits under the 10-min tool ceiling, never nudge a genuine human-wait); and `tm-delegation-patterns` documents the long-wait/chunked-repoll delegation pattern as first-class. Prompt/asset-only — the daemon-side hook for in-conversation subagents is deferred as a follow-up proposal (closes #2833)
- bundled `dotnet-engineer` agent — a modern C#/.NET 8+ specialist (ASP.NET Core, EF Core, xUnit, nullable reference types, minimal APIs, `dotnet` CLI workflow) with explicit legacy VB.NET awareness (reads/maintains `.vbproj`, WinForms/WebForms code; recommends interop over blind rewrites). Wired into per-project stack detection via new `*.sln`/`*.csproj`/`*.vbproj`/`global.json`/`Directory.Build.props` markers (extension-glob marker support added to `project_lang`), so the auto-derived `## Detected Project Stack` section and agent-roster scoping route C#/.NET/VB.NET projects to the specialist instead of the general-purpose fallback (closes #2831)
- project- and user-level custom skill tiers with precedence project-custom > user-custom > bundled: skills authored in `~/.trusty-mpm/skills/` deploy into every project and override same-named bundled skills, while a skill hand-placed in a project's `.claude/skills/` outranks both and is never overwritten on redeploy; collisions are logged. New `core::skill_tiers` (pure `plan_skill_tiers` planner + `deploy_all_skill_tiers` orchestrator) and `FrameworkPaths::user_skill_source_dir()`; `tm catalog apply`, `tm install`, and both tm-global `CLAUDE_CONFIG_DIR` bootstrap paths (`managed_config`, `standalone::global_config`) now route through the same multi-tier deploy so a user-tier override survives a catalog refresh, a routine reinstall, or a session-config rebuild — every raw single-tier `deploy_skills`/`deploy_skills_filtered` call site outside `skill_deployer.rs` itself has been migrated; documented in `PM_INSTRUCTIONS.md` and the `mpm-skills-manager` agent (closes #2816)
- auto-nudge idle-parked managed sessions ([#2781](https://github.com/bobmatnyc/trusty-tools/pull/2781)) ([`be2ec11`](https://github.com/bobmatnyc/trusty-tools/commit/be2ec1107a56bd1c25e07c9fa146717399effd23))
- require per-PR changelog updates in the default workflow ([#2790](https://github.com/bobmatnyc/trusty-tools/pull/2790)) ([`238f07e`](https://github.com/bobmatnyc/trusty-tools/commit/238f07e8f77b2290afc1c42939e7802e5d8a5074))
- propagate native trusty MCP servers to fleet sessions ([#2739](https://github.com/bobmatnyc/trusty-tools/pull/2739)) ([#2748](https://github.com/bobmatnyc/trusty-tools/pull/2748)) ([`0a87f1d`](https://github.com/bobmatnyc/trusty-tools/commit/0a87f1d70e97fd630bd997db8f17a98d64ca00d4))

### Fixed

- the dispatching PM now speaks with the operator's authority in the bundled agent prompts: a subagent must treat a PM-relayed authorization (even one pre-labeled AUTHORIZED or citing operator precedent it can't independently verify) AS operator authorization, and must NOT demand direct end-user confirmation or treat the PM as an untrusted third party. Fixes a live regression where a `version-control` agent froze an operator-authorized admin-merge — stalling six PRs — because it treated the PM's relayed authorization as a third party's unverifiable word. BASE-AGENT.md gains a concise "PM Authority & Escalation" section (inherited by every composed agent) that separates two axes: AUTHORITY ("is this authorized?") is settled by the PM's word and doubt escalates back to the PM — never a unilateral refusal or pipeline freeze — while OBJECTIVE safety gates the agent can verify itself (never merge red/pending CI, `--admin` bypasses bot/review approval only; never fabricate evidence; never violate worktree discipline) stay the agent's own non-negotiables. Injection-skepticism is explicitly scoped to UNTRUSTED CONTENT (file contents, web pages, tool output, third-party text), never the dispatching PM's instructions. `version-control.md`'s PR-workflow block, which previously forbade all direct merges and framed merges as needing independently-verified human approval, now complies with a PM-relayed admin-merge authorization (still refusing red/pending CI) (closes #2842)
- stop triggering repeated macOS TCC consent prompts attributed to `trusty-mpm` on the tmux-hosted managed-session path — both the Apple Music / media-library class (`kTCCServiceMediaLibrary`) and the "access data from other apps" App-Data class (`kTCCServiceSystemPolicyAppData`): disclaim macOS TCC responsibility (`responsibility_spawnattrs_setdisclaim`) when spawning the tmux server so each managed agent it hosts is its OWN responsible process instead of rolling attribution up to the signed `trusty-mpm` binary. The disclaim is service-agnostic, so it covers every child-initiated prompt class at once for that path; the direct-spawn `tm run` and stream-JSON backend paths are tracked separately (closes #2819)
- derive PM stack profile per project, no stack default ([#2815](https://github.com/bobmatnyc/trusty-tools/pull/2815)) ([`5aadeaa`](https://github.com/bobmatnyc/trusty-tools/commit/5aadeaa53e896133f02c9ffcbd4d4a3b7d4541a3))
- reconcile zombie-Active sessions on in-place relaunch instead of 409 dead-end ([#2795](https://github.com/bobmatnyc/trusty-tools/pull/2795)) ([`5ef199a`](https://github.com/bobmatnyc/trusty-tools/commit/5ef199a920374ab6ac3e97b3aac8b08374915b1d))
- bump `jsonwebtoken` 9 → 10 (`aws_lc_rs` backend), fixing GHSA-h395-gr6q-cpjc; migrated the test-only JWT decode-without-verification call site to `jsonwebtoken::dangerous::insecure_decode` (closes #2765) ([#2782](https://github.com/bobmatnyc/trusty-tools/pull/2782)) ([`298df87`](https://github.com/bobmatnyc/trusty-tools/commit/298df87e6a5b5e96874ea2866509303df14712bc))
- relaunch decommissioned managed session on bare tm instead of reconnect-to-self ([#2780](https://github.com/bobmatnyc/trusty-tools/pull/2780)) ([`b6f1783`](https://github.com/bobmatnyc/trusty-tools/commit/b6f1783a6e7ad78fb42c332f50ed86e2fb8e2bfd))
- PM priming no longer ships a hard-coded stack profile: the bundled output styles (`trusty-mpm`, `-research`, `-teacher`) are now stack-neutral — the "Rust workspace" declaration, `rust-engineer`-only delegation map, and `cargo`/`make check` quality gate are replaced with detect-first guidance — and every per-project PM prompt now carries an auto-derived `## Detected Project Stack` section (new `core::stack_profile`, reusing the `project_lang` marker detection) that routes to the project's actual language engineer(s) or, when nothing is detected, a neutral profile mandating a Research phase and forbidding any default. Fixes a Rust profile leaking into a Next.js/TypeScript project (ai-power-rankings); skills-side precedent #2005/#2006 (closes #1971)
- resolve managed MCP registry from real home in fleet-session injector ([#2761](https://github.com/bobmatnyc/trusty-tools/pull/2761)) ([`7c04f1f`](https://github.com/bobmatnyc/trusty-tools/commit/7c04f1f845432edd6b1fb488103a1aba9939fb3c))
- never traverse $HOME/TCC-protected folders (closes #2759) ([#2760](https://github.com/bobmatnyc/trusty-tools/pull/2760)) ([`74fe7f3`](https://github.com/bobmatnyc/trusty-tools/commit/74fe7f3c682cb5b0c648b63585c44ee47aa6d652))
- positive stdio field allowlist for fleet native-MCP injection ([#2739](https://github.com/bobmatnyc/trusty-tools/pull/2739)) ([#2755](https://github.com/bobmatnyc/trusty-tools/pull/2755)) ([`e3a5c68`](https://github.com/bobmatnyc/trusty-tools/commit/e3a5c684cc25028202f6ece1a262f515fa3b43da))

### Changed

- collapse stacked Unreleased headings (trusty-mpm, trusty-common) ([`95b16c8`](https://github.com/bobmatnyc/trusty-tools/commit/95b16c8c468084ce0e2a46eac8a4acf99cfab823))

### Documentation

- correct stale session_launch test comments ([#2779](https://github.com/bobmatnyc/trusty-tools/pull/2779)) ([`3371828`](https://github.com/bobmatnyc/trusty-tools/commit/33718282bc3e3da4da6f84ff6681d84cd2859a26))

### Added


### Added

- implement Slack send + read tools; extract SlackFormatter to trusty-common ([#2722](https://github.com/bobmatnyc/trusty-tools/pull/2722)) ([`847a0c3`](https://github.com/bobmatnyc/trusty-tools/commit/847a0c334e1a8822a7d31696b48946e538aca7cc))

### Fixed

- reconcile zombie-active sessions on in-place relaunch (closes #2743) ([#2744](https://github.com/bobmatnyc/trusty-tools/pull/2744)) ([`f1552c7`](https://github.com/bobmatnyc/trusty-tools/commit/f1552c72e4f20afda74466f97865845660d18a30))
- pm_guard recognizes git global flags and stops scanning quoted content ([#2741](https://github.com/bobmatnyc/trusty-tools/pull/2741)) ([`6e29e25`](https://github.com/bobmatnyc/trusty-tools/commit/6e29e253c073cb889bdbfce5e99d05d16e6d1fe2))
- framework workflow conventions — per-session session log, issue/PR assignee+label defaults, trusty-mpm attribution footer ([#2737](https://github.com/bobmatnyc/trusty-tools/pull/2737)) ([`3089600`](https://github.com/bobmatnyc/trusty-tools/commit/3089600b475cc464329132e5f7523536bb730797))

### Added

- shared ensure-project-indexed helper; wire tcode task start ([#2701](https://github.com/bobmatnyc/trusty-tools/pull/2701)) ([`28a8d11`](https://github.com/bobmatnyc/trusty-tools/commit/28a8d11d4a5eac21921c3ceeef8707f71cf35459))

### Changed

- DispositionReason enum replaces stringly-typed disposition reasons ([#2705](https://github.com/bobmatnyc/trusty-tools/pull/2705)) ([`e7970c8`](https://github.com/bobmatnyc/trusty-tools/commit/e7970c87541d774aa25c315f27da7fee656be492))
- convert closed-set literals to typed constructs (PR 1: zero-behavior batch) ([#2704](https://github.com/bobmatnyc/trusty-tools/pull/2704)) ([`3b65103`](https://github.com/bobmatnyc/trusty-tools/commit/3b651033f92e619c65bb1aaa77168213e3306b4b))

### Fixed

- picker launch-new switches client safely and exits instead of hanging ([#2680](https://github.com/bobmatnyc/trusty-tools/pull/2680)) ([`3de0647`](https://github.com/bobmatnyc/trusty-tools/commit/3de0647452f0e77a67aa20c52c8d9610474b8de8))
- pm_guard allows read-only sed/awk pipe segments ([#2677](https://github.com/bobmatnyc/trusty-tools/pull/2677)) ([`3881639`](https://github.com/bobmatnyc/trusty-tools/commit/38816393f9d836b79e08c8efd4642fd21f3a6e5f))

### Fixed

- bound the manager ChatStore conversation-key set with an LRU cap ([#2648](https://github.com/bobmatnyc/trusty-tools/pull/2648)) ([`0d776f9`](https://github.com/bobmatnyc/trusty-tools/commit/0d776f9e8de429b76ac00a9da355bd76fc75b933))
- exclude dead sessions (missing workspace) from resume/restart offers ([#2652](https://github.com/bobmatnyc/trusty-tools/pull/2652)) ([`4a8870f`](https://github.com/bobmatnyc/trusty-tools/commit/4a8870fb2990ca604f84da4ed7d46ac2203e11e4))
- tm sessions resume hands terminal to the resumed session's tmux window ([#2656](https://github.com/bobmatnyc/trusty-tools/pull/2656)) ([`6f260cd`](https://github.com/bobmatnyc/trusty-tools/commit/6f260cde3e58849278b00a6160b3ed1e3a116a33))
- sync session worktrees on resume + de-duplicate PM mandate injection ([#2653](https://github.com/bobmatnyc/trusty-tools/pull/2653)) ([`364c71c`](https://github.com/bobmatnyc/trusty-tools/commit/364c71cadb8ec0adc85ea36ea2ae70ef56a436a5))

### Changed

- split bin/tm/cli.rs into cli/ modules ([#2650](https://github.com/bobmatnyc/trusty-tools/pull/2650)) ([`53eb4a6`](https://github.com/bobmatnyc/trusty-tools/commit/53eb4a67f3cec9ba4f9d23ddf65243d20974d5ec))

### Added

- tm-manager phase 2 — route-task, proposal-and-confirm, route CLI (#2585 #2586 #2587) ([#2615](https://github.com/bobmatnyc/trusty-tools/pull/2615)) ([`87e4d1d`](https://github.com/bobmatnyc/trusty-tools/commit/87e4d1d4851b218bc651a2bb35ae681923633dfd))

### Fixed

- harden agents against idle parking — never end a turn to wait (closes #2610) ([#2620](https://github.com/bobmatnyc/trusty-tools/pull/2620)) ([`cbcfd17`](https://github.com/bobmatnyc/trusty-tools/commit/cbcfd17e797c6db80074e1254dfefb5f1f8fb3c1))

### Added

- async managed-spawn provisioning with live progress poll route (closes #2605) ([#2607](https://github.com/bobmatnyc/trusty-tools/pull/2607)) ([`f440192`](https://github.com/bobmatnyc/trusty-tools/commit/f44019266928e96973d16ab83cc618a4ac0894ff))
- tm manager status|digest|chat CLI (TMMGR phase 1) ([#2600](https://github.com/bobmatnyc/trusty-tools/pull/2600)) ([`ed9390a`](https://github.com/bobmatnyc/trusty-tools/commit/ed9390a8a0f3e491f2aa6416cf68b5603975536a))
- tm manager phase-1b — digest, read-only chat, hermetic suite ([#2601](https://github.com/bobmatnyc/trusty-tools/pull/2601)) ([`6f80f36`](https://github.com/bobmatnyc/trusty-tools/commit/6f80f36999c66ff679f194fe3e2d3bd9b3214c79))

### Fixed

- pm_guard permits PM single-file writes to non-source paths ([#2606](https://github.com/bobmatnyc/trusty-tools/pull/2606)) ([`3c6f9f7`](https://github.com/bobmatnyc/trusty-tools/commit/3c6f9f705760f1e94a37f6b9d966b45bf483c2ed))

### Added

- tm manager phase-1a — scaffold, status rollup, portfolio palace ([#2598](https://github.com/bobmatnyc/trusty-tools/pull/2598)) ([`3084a3c`](https://github.com/bobmatnyc/trusty-tools/commit/3084a3c406c2aa935c4c736950b81954c47fea39))

### Fixed

- session restart returns 500 for stopped session whose workspace is gone (closes #2577) ([#2594](https://github.com/bobmatnyc/trusty-tools/pull/2594)) ([`f2270f1`](https://github.com/bobmatnyc/trusty-tools/commit/f2270f1f2effb417aaf460f21b56c311d3403b34))

### Added

- task-injection delivery observability + readiness-probe hardening (closes #2364) ([#2568](https://github.com/bobmatnyc/trusty-tools/pull/2568)) ([`d0fe72b`](https://github.com/bobmatnyc/trusty-tools/commit/d0fe72b943434a4adab735e1973c1446f067f1a0))
- wire Slack onto the channel-agnostic SessionProxy (closes #2549) ([#2565](https://github.com/bobmatnyc/trusty-tools/pull/2565)) ([`26aa1ae`](https://github.com/bobmatnyc/trusty-tools/commit/26aa1ae5e6a4d890221874d181037180bb9cfde2))
- expose SessionProxy focus/inject/summarize as MCP tools ([#2562](https://github.com/bobmatnyc/trusty-tools/pull/2562)) ([`4edcc3b`](https://github.com/bobmatnyc/trusty-tools/commit/4edcc3bba3a848e9dba4d97401ee370423edf92f))

### Changed

- retire duplicate Bedrock ports onto trusty-common::inference ([#2567](https://github.com/bobmatnyc/trusty-tools/pull/2567)) ([`31c9fc0`](https://github.com/bobmatnyc/trusty-tools/commit/31c9fc0467adfda65b702c7433ceded01e0cf884))

### Fixed

- reconcile agent roster drift across deploy destinations ([#2547](https://github.com/bobmatnyc/trusty-tools/pull/2547)) ([`8d8b042`](https://github.com/bobmatnyc/trusty-tools/commit/8d8b04230a646f8c7941754c7cd4684f57d32089))
- pane-scope session capture reads to the recorded harness pane ([#2545](https://github.com/bobmatnyc/trusty-tools/pull/2545)) ([`18bf5b2`](https://github.com/bobmatnyc/trusty-tools/commit/18bf5b28f8ee47491c3660593195b5ca02ec8883))

### Fixed

- guided-default cwd's own repo wins over an ancestor ([#2542](https://github.com/bobmatnyc/trusty-tools/pull/2542)) ([#2543](https://github.com/bobmatnyc/trusty-tools/pull/2543)) ([`51f5041`](https://github.com/bobmatnyc/trusty-tools/commit/51f50417e23c76a4bea4beabc854c4e75cacd526))

### Added

- Test-pointer lint gate (closes #2458) ([#2529](https://github.com/bobmatnyc/trusty-tools/pull/2529)) ([`9876b44`](https://github.com/bobmatnyc/trusty-tools/commit/9876b44d72d2a68dd9df213e027d2f15a119e2b9))
- deterministic project configurator — tm projects config CLI + TUI form ([#2484](https://github.com/bobmatnyc/trusty-tools/pull/2484)) ([`bfb58a6`](https://github.com/bobmatnyc/trusty-tools/commit/bfb58a61db392eea868828492b532dde9bf41750))
- PATCH /api/v1/projects/{name} — registry-B field-level config update ([#2481](https://github.com/bobmatnyc/trusty-tools/pull/2481)) ([`6e2cc38`](https://github.com/bobmatnyc/trusty-tools/commit/6e2cc38d2fb91dddc001de5c2b7dccd470564481))
- TUI Deliverable glyph + read-only Deliverable/Milestone view ([#2473](https://github.com/bobmatnyc/trusty-tools/pull/2473)) ([`fed11da`](https://github.com/bobmatnyc/trusty-tools/commit/fed11da19b3e05935363228882c6885dd014d00f))
- TUI live-refresh + activity-pane wiring ([#2469](https://github.com/bobmatnyc/trusty-tools/pull/2469)) ([`7430bd8`](https://github.com/bobmatnyc/trusty-tools/commit/7430bd877e8d692af2bdbc9be1f695e5433652e5))
- multipane TUI skeleton — 4-pane projects control plane ([#2465](https://github.com/bobmatnyc/trusty-tools/pull/2465)) ([`ea89558`](https://github.com/bobmatnyc/trusty-tools/commit/ea8955859b36b6182bc0d047c1e465584a9a6049))
- SessionRecord.deliverable_id + --deliverable launch wiring (WI-13, closes #2379) ([#2439](https://github.com/bobmatnyc/trusty-tools/pull/2439)) ([`0c4799b`](https://github.com/bobmatnyc/trusty-tools/commit/0c4799be8914098d9d60063b3dfaf95c27d0fdd7))
- deliverable/milestone status histograms on project status endpoint ([#2429](https://github.com/bobmatnyc/trusty-tools/pull/2429)) ([`d92dd67`](https://github.com/bobmatnyc/trusty-tools/commit/d92dd67b013756316fe4b03e24ea3c6a3c9f881b))
- tm projects CLI — list/register/show/status + deliverables/milestones subtrees (closes #2115, #2381) ([#2428](https://github.com/bobmatnyc/trusty-tools/pull/2428)) ([`cca7216`](https://github.com/bobmatnyc/trusty-tools/commit/cca72166d9eaeabc24b8097fdebdd7589ace0e64))
- common tmux entry point + generous scrollback ([#2399](https://github.com/bobmatnyc/trusty-tools/pull/2399)) ([`2651fb3`](https://github.com/bobmatnyc/trusty-tools/commit/2651fb33622a077fa4323238daf5ad24484105bc))
- Deliverable/Milestone data model, central stores, CRUD API + state-machine enforcement (closes #2378, #2380) ([#2395](https://github.com/bobmatnyc/trusty-tools/pull/2395)) ([`359fb79`](https://github.com/bobmatnyc/trusty-tools/commit/359fb797d1fe73917c34f8d5be869c54aa3d22a0))
- canonical tm sessions namespace with deprecated tm session alias ([#2394](https://github.com/bobmatnyc/trusty-tools/pull/2394)) ([`9b4ba83`](https://github.com/bobmatnyc/trusty-tools/commit/9b4ba83a7aec7bbb2dee5a8960e8aacf65761670))
- deterministic project status-aggregation endpoint (closes #2117) ([#2396](https://github.com/bobmatnyc/trusty-tools/pull/2396)) ([`8ec8898`](https://github.com/bobmatnyc/trusty-tools/commit/8ec88983695583230e7d4cff82ba118bfda1e0de))
- tm-issues-prune skill organizes, prioritizes, and suggests next tasks ([#2393](https://github.com/bobmatnyc/trusty-tools/pull/2393)) ([`1266097`](https://github.com/bobmatnyc/trusty-tools/commit/1266097f2c70417270aa15c58599a7e42b20cd48))
- Telegram focused-session free-text routing (TELUI-6, #1440) ([#2372](https://github.com/bobmatnyc/trusty-tools/pull/2372)) ([`362cb72`](https://github.com/bobmatnyc/trusty-tools/commit/362cb72af874f7783fd84f105eec55574b6e6db3))
- align PM identity string to tm-<project>-<n> naming (closes #2325) ([#2328](https://github.com/bobmatnyc/trusty-tools/pull/2328)) ([`e20b878`](https://github.com/bobmatnyc/trusty-tools/commit/e20b878f6cb7f6a590a0aed92bd94d29dc97b14c))
- bundled tm CLI operations skill incl. MCP management (closes #2321) ([#2323](https://github.com/bobmatnyc/trusty-tools/pull/2323)) ([`970ebde`](https://github.com/bobmatnyc/trusty-tools/commit/970ebdea59cc29a0f900ad5dff1883cc59fb8dd3))
- tm mcp test verifies MCP servers via stdio handshake (closes #2311) ([#2316](https://github.com/bobmatnyc/trusty-tools/pull/2316)) ([`e1eb9a6`](https://github.com/bobmatnyc/trusty-tools/commit/e1eb9a657c077d41835d06c676755263fce48375))
- add delete action to interactive tm ls session picker (closes #2304) ([#2310](https://github.com/bobmatnyc/trusty-tools/pull/2310)) ([`5e0af0a`](https://github.com/bobmatnyc/trusty-tools/commit/5e0af0a8227cec3d24fbb694e702dcc63977a968))
- inject CLAUDE_CODE_OAUTH_TOKEN into managed sessions — fix login loop (closes #2246) ([#2256](https://github.com/bobmatnyc/trusty-tools/pull/2256)) ([`07a9085`](https://github.com/bobmatnyc/trusty-tools/commit/07a90857da5eba20e356326b14fac371098d4647))
- tm ls becomes session connector; alias list moves to tm ls --projects/-p ([#2297](https://github.com/bobmatnyc/trusty-tools/pull/2297)) ([`0365a1b`](https://github.com/bobmatnyc/trusty-tools/commit/0365a1bd1133a68911ec0a61ff03f76a05f18082))
- tm mcp add|remove|list for user-level MCP servers in tm config dir ([#2286](https://github.com/bobmatnyc/trusty-tools/pull/2286)) ([`a2cf543`](https://github.com/bobmatnyc/trusty-tools/commit/a2cf543feae45700b4bab8a6e6a0af64122ea2d5))

### Fixed

- guided-default no longer inherits an ancestor repo's project when cwd is untracked ([#2535](https://github.com/bobmatnyc/trusty-tools/pull/2535)) ([`9bba5ee`](https://github.com/bobmatnyc/trusty-tools/commit/9bba5eecc7a1a2b65368e4a743dca2f9436919bf))
- route tm CLI top-level client through bounded config (closes #2517) ([#2524](https://github.com/bobmatnyc/trusty-tools/pull/2524)) ([`9200bc5`](https://github.com/bobmatnyc/trusty-tools/commit/9200bc5e605070780dd78e5026dcd2d758d40bce))
- non-zero exit on session spawn failure (closes #2457) ([#2521](https://github.com/bobmatnyc/trusty-tools/pull/2521)) ([`74f95c7`](https://github.com/bobmatnyc/trusty-tools/commit/74f95c78653bc3a1ae5b220e2e8ff6b8d9877450))
- SessionEnd gate aggregates any-pane-live like the runtime reaper ([#2516](https://github.com/bobmatnyc/trusty-tools/pull/2516)) ([`e30c686`](https://github.com/bobmatnyc/trusty-tools/commit/e30c68665cca7185c5198186526eb8a84e7a56c8))
- harden inject/observe/dashboard-restart against active-pane ambiguity (closes #2468) ([#2514](https://github.com/bobmatnyc/trusty-tools/pull/2514)) ([`47b8387`](https://github.com/bobmatnyc/trusty-tools/commit/47b8387b23a8cdf2121aaf3619bcfd63be39005d))
- per-request DaemonClient timeouts + TUI input protection (closes #2471) ([#2512](https://github.com/bobmatnyc/trusty-tools/pull/2512)) ([`8ccb867`](https://github.com/bobmatnyc/trusty-tools/commit/8ccb867a152016858bd7d2643d11a606da7978c4))
- TUI activity pane — explicit unavailable state instead of perpetual loading ([#2513](https://github.com/bobmatnyc/trusty-tools/pull/2513)) ([`99ee122`](https://github.com/bobmatnyc/trusty-tools/commit/99ee122fcc680bd168c057a49f2cad01057bd31c))
- agent-manifest adoption + tm install --reset-agents ([#2505](https://github.com/bobmatnyc/trusty-tools/pull/2505)) ([`2d78f8e`](https://github.com/bobmatnyc/trusty-tools/commit/2d78f8e6ff251ff8d0f1bcf88b419410c0d65b0c))
- bundled agent guidance — foreground execution + PM-directive scoping (closes #2501, #2502) ([#2503](https://github.com/bobmatnyc/trusty-tools/pull/2503)) ([`d37aaf9`](https://github.com/bobmatnyc/trusty-tools/commit/d37aaf9300e52bfba262a0d684d6b8811f33b522))
- uniform TRUSTY_MPM_URL resolution across tm CLI client construction ([#2499](https://github.com/bobmatnyc/trusty-tools/pull/2499)) ([`8a985df`](https://github.com/bobmatnyc/trusty-tools/commit/8a985dff46117465bd38f574580095aea8978a20))
- drop decommissioned sessions from TUI Sessions pane ([#2497](https://github.com/bobmatnyc/trusty-tools/pull/2497)) ([`13cbb2b`](https://github.com/bobmatnyc/trusty-tools/commit/13cbb2b68d7a1196dadb76559fc2e6ada0466c26))
- surface server error bodies in DaemonClient PATCH/mutation errors ([#2496](https://github.com/bobmatnyc/trusty-tools/pull/2496)) ([`9ae5388`](https://github.com/bobmatnyc/trusty-tools/commit/9ae5388cc3d4356b76bdaf243193fb845502f368))
- launchd-aware bridge no-spawn + /health supervised flag (closes #2486) ([#2491](https://github.com/bobmatnyc/trusty-tools/pull/2491)) ([`e993c18`](https://github.com/bobmatnyc/trusty-tools/commit/e993c18ace1fe9a86f4b5315be7887ed767da710))
- target stored pane_id on resume/restart respawn ([#2467](https://github.com/bobmatnyc/trusty-tools/pull/2467)) ([`10fd418`](https://github.com/bobmatnyc/trusty-tools/commit/10fd4187309dee58a2cef689704eb38ff4b95ed0))
- reconcile stale-Active session before refusing in-place relaunch ([#2456](https://github.com/bobmatnyc/trusty-tools/pull/2456)) ([`16d4365`](https://github.com/bobmatnyc/trusty-tools/commit/16d4365dd963fa26b9fb2799714993e7b01fdf3b))
- SessionEnd hook uses non-destructive pane-preserving stop ([#2455](https://github.com/bobmatnyc/trusty-tools/pull/2455)) ([`d8c70a9`](https://github.com/bobmatnyc/trusty-tools/commit/d8c70a9449dfdc1c6315cb7c7cdd344e3939bba2))
- force_new opt-out so the picker's "launch new session" never adopts a live session (closes #2450) ([#2451](https://github.com/bobmatnyc/trusty-tools/pull/2451)) ([`b07b1ba`](https://github.com/bobmatnyc/trusty-tools/commit/b07b1babeca5701769ff5908ebfc6cbd39ca2801))
- SM context-engine round loss under concurrency + graceful no-memory degradation ([#2360](https://github.com/bobmatnyc/trusty-tools/pull/2360)) ([`ccc028a`](https://github.com/bobmatnyc/trusty-tools/commit/ccc028a9b7b8ae571bc05842b09e0375fbec0f3f))
- inject --task into spawned session pane (turnkey execution) ([#2361](https://github.com/bobmatnyc/trusty-tools/pull/2361)) ([`f95f044`](https://github.com/bobmatnyc/trusty-tools/commit/f95f04423d4fa46664621ad7d39dadf5161bf504))
- dedup stale duplicate session records per project in reconcile_on_boot (closes #2306) ([#2338](https://github.com/bobmatnyc/trusty-tools/pull/2338)) ([`a8b040c`](https://github.com/bobmatnyc/trusty-tools/commit/a8b040cdd1e5e4cf7c68d68a4475f05fde92af03))
- strip leading -- separator from tm mcp args at write and read (closes #2326) ([#2329](https://github.com/bobmatnyc/trusty-tools/pull/2329)) ([`df981bf`](https://github.com/bobmatnyc/trusty-tools/commit/df981bf29a9e2fcbe0f82560e784e872c8a7f8f2))
- strip stale-hash mpm hook entries so managed hook-merge is idempotent (closes #2235) ([#2301](https://github.com/bobmatnyc/trusty-tools/pull/2301)) ([`c2ba862`](https://github.com/bobmatnyc/trusty-tools/commit/c2ba862b65dc32511054312472da8a1e87d469c8))
- resolve clippy 1.97.0 lint regressions blocking CI ([#2284](https://github.com/bobmatnyc/trusty-tools/pull/2284)) ([`8b50ac2`](https://github.com/bobmatnyc/trusty-tools/commit/8b50ac25837d5d95244fe94ed913bc3093a2ce86))

### Changed

- mount config command on all 10 primary binaries ([#2528](https://github.com/bobmatnyc/trusty-tools/pull/2528)) ([`a58ea52`](https://github.com/bobmatnyc/trusty-tools/commit/a58ea5223167553f0d90fb5258d582d510dca316))
- migrate root CLAUDE.md to .trusty-mpm/INSTRUCTIONS.md ([#2300](https://github.com/bobmatnyc/trusty-tools/pull/2300)) ([`d484967`](https://github.com/bobmatnyc/trusty-tools/commit/d484967e50c6585f02dee2893ab325ac59cc71ee))

### Documentation

- add trusty-mpm package metadata + repoint trusty-search CI badge to monorepo ([#2292](https://github.com/bobmatnyc/trusty-tools/pull/2292)) ([`cba43a5`](https://github.com/bobmatnyc/trusty-tools/commit/cba43a5698c03ea611f731b6a5bef0809547a93f))


### Fixed

- probe live tmux/process for session liveness, not the stored state field ([#2275](https://github.com/bobmatnyc/trusty-tools/pull/2275)) ([`b5f8b6b`](https://github.com/bobmatnyc/trusty-tools/commit/b5f8b6b7b941b5f8939918f592374e0b4bd76d28))

### Added

- capture tmux window id at session pause and reattach on resume ([#2269](https://github.com/bobmatnyc/trusty-tools/pull/2269)) ([`1959738`](https://github.com/bobmatnyc/trusty-tools/commit/1959738450d9a1b7adaf707d9d5980d6365fe4a7))

### Fixed

- correct banner two_panel right-column no-inner-border rendering ([#2258](https://github.com/bobmatnyc/trusty-tools/pull/2258)) ([`6b9ae4a`](https://github.com/bobmatnyc/trusty-tools/commit/6b9ae4ab17ce0a07b2f69bfb14d3b477c8db6fbe))
- keep resumed panes rooted at the workspace, not $HOME ([#2254](https://github.com/bobmatnyc/trusty-tools/pull/2254)) ([`d2c1edf`](https://github.com/bobmatnyc/trusty-tools/commit/d2c1edf4431b0cba3e94f558fc13fc9c40786307))

### Added

- opt-in deny-by-default pm_guard + warn-only carrier self-check ([#2237](https://github.com/bobmatnyc/trusty-tools/pull/2237)) ([`c3328b6`](https://github.com/bobmatnyc/trusty-tools/commit/c3328b62ffc8f26a5e476d2d18c39a931cfb8a11))

### Fixed

- delegation persona reaches resume path + harden tm connect ([#2239](https://github.com/bobmatnyc/trusty-tools/pull/2239)) ([`df09b16`](https://github.com/bobmatnyc/trusty-tools/commit/df09b1655e9b316396ecbda47cd29395a057cf18))
- resolve stable binary for hooks/statusLine + self-heal stale paths (closes #2229) ([#2234](https://github.com/bobmatnyc/trusty-tools/pull/2234)) ([`ae87bbb`](https://github.com/bobmatnyc/trusty-tools/commit/ae87bbb3827b9dfc9fccae4732c66104af6912b7))

### Changed

- re-cut as 0.19.1: depends on trusty-common 0.22.2 (0.19.0 was git-tagged but
  never published to crates.io, so republishing the same content under a new
  patch version rather than moving an existing tag). Ships #2214/#2229/#2230/#2231.

### Added

- seed outputStyle/statusLine into tm-owned config dir ([#2215](https://github.com/bobmatnyc/trusty-tools/pull/2215)) ([`2ebb6e1`](https://github.com/bobmatnyc/trusty-tools/commit/2ebb6e19993524aae4df038041c4c1d1cdc64dd0))

### Added

- statusline shows session + weekly account-usage % ([#2141](https://github.com/bobmatnyc/trusty-tools/pull/2141)) ([`18a7942`](https://github.com/bobmatnyc/trusty-tools/commit/18a79426bd9b618910fbc3869d1929f98de19aec))
- statusline renders context >50% in red ([#2099](https://github.com/bobmatnyc/trusty-tools/pull/2099)) ([`a821b9d`](https://github.com/bobmatnyc/trusty-tools/commit/a821b9da89d7c076dfb4557094f632859b53642e))
- manage trusty-search index lifecycle for session worktrees ([#2094](https://github.com/bobmatnyc/trusty-tools/pull/2094)) ([`299e993`](https://github.com/bobmatnyc/trusty-tools/commit/299e9931edd2b7faf9236bd5ccc3b6ddb038329d))
- project config stores preferred gh user; default gh ops to it ([#2087](https://github.com/bobmatnyc/trusty-tools/pull/2087)) ([`d49e385`](https://github.com/bobmatnyc/trusty-tools/commit/d49e385826c0befb0d4cece01a032c71e67e1aa3))
- name managed-session worktrees by tmux session name, not UUID ([#2076](https://github.com/bobmatnyc/trusty-tools/pull/2076)) ([`39abd2e`](https://github.com/bobmatnyc/trusty-tools/commit/39abd2e02b47e5b4e739ad30de57c33cfdf4dc54))

### Fixed

- guarantee PM delegation persona loads (CLAUDE.md carrier + project-tier output-style + daemon-adapter system-prompt injection) ([#2129](https://github.com/bobmatnyc/trusty-tools/pull/2129)) ([`3a071a0`](https://github.com/bobmatnyc/trusty-tools/commit/3a071a0a2897911279433522d2564cb532f310be))
- pm_guard exempts native delegated sub-agent edits; retire global UNRESTRICTED bypass ([#2107](https://github.com/bobmatnyc/trusty-tools/pull/2107)) ([`0f413d0`](https://github.com/bobmatnyc/trusty-tools/commit/0f413d0c5d611178dbd5344475ae61ba5fa21213))
- PM summarizes orchestration in prose instead of raw pm_summary JSON ([#2045](https://github.com/bobmatnyc/trusty-tools/pull/2045)) ([`53731f4`](https://github.com/bobmatnyc/trusty-tools/commit/53731f4849436607cdef55237680a230c798d94f))
- statusline shows tmux session name instead of worktree UUID branch ([#2035](https://github.com/bobmatnyc/trusty-tools/pull/2035)) ([`bbafad1`](https://github.com/bobmatnyc/trusty-tools/commit/bbafad11063accc2631be4fd97cb610c13152faa))
- reach trusty-memory over discovered JSON-RPC, never a hardcoded port ([#2040](https://github.com/bobmatnyc/trusty-tools/pull/2040)) ([`e0f41c5`](https://github.com/bobmatnyc/trusty-tools/commit/e0f41c51f1baa7ddf0e427cb5c7e86cbe9bba5fa))

### Changed

- bump version 0.16.0 -> 0.16.1 ([#2139](https://github.com/bobmatnyc/trusty-tools/pull/2139)) ([`931917c`](https://github.com/bobmatnyc/trusty-tools/commit/931917cdc2606f563be7c711a5adb2842e7ce229))

### Documentation

- bundled architecture doc for memory-MCP, session/worktree, search-index ([#2096](https://github.com/bobmatnyc/trusty-tools/pull/2096)) ([`d82e15a`](https://github.com/bobmatnyc/trusty-tools/commit/d82e15ae9367a4c17cdd8fb714e053eb6da9cab7))

### Added


### Fixed


### Documentation


### Added

- in-place relaunch current session + on-exit hint (#2023 C+D) ([#2027](https://github.com/bobmatnyc/trusty-tools/pull/2027)) ([`dba2195`](https://github.com/bobmatnyc/trusty-tools/commit/dba21950daedb48e909ffa2aefd9c12b0d47537a))
- export TM_MANAGED_SESSION_ID into managed pane shell ([#2025](https://github.com/bobmatnyc/trusty-tools/pull/2025)) ([`879343b`](https://github.com/bobmatnyc/trusty-tools/commit/879343b93c720c69b6c706535de7d2d8e0db8021))
- non-destructive stop for runtime-exited sessions (#2023 A) ([#2024](https://github.com/bobmatnyc/trusty-tools/pull/2024)) ([`bca3685`](https://github.com/bobmatnyc/trusty-tools/commit/bca3685d665e67bcc0e1ba2f862410521b22c6fd))
- add tm session delete <id> [--force] ([#2021](https://github.com/bobmatnyc/trusty-tools/pull/2021)) ([`72cf975`](https://github.com/bobmatnyc/trusty-tools/commit/72cf9754cf507bf5ee5271e01412a9bb41b1147a))
- reformat tm statusline ([#2018](https://github.com/bobmatnyc/trusty-tools/pull/2018)) ([`138b0fb`](https://github.com/bobmatnyc/trusty-tools/commit/138b0fb8b76bf5fe22f56ddb74435fe1db7202ed))
- managed sessions launch with tm-owned CLAUDE_CONFIG_DIR + full roster (DOC-34, #1996) ([#2002](https://github.com/bobmatnyc/trusty-tools/pull/2002)) ([`7c2d13d`](https://github.com/bobmatnyc/trusty-tools/commit/7c2d13dc7c1d39e747b65a9e6b23a7be45ec4fdc))
- add /tm-init project-initialization skill ([#1997](https://github.com/bobmatnyc/trusty-tools/pull/1997)) ([`4c7332b`](https://github.com/bobmatnyc/trusty-tools/commit/4c7332b1abb8ab13ca8c9700625382823f57f7af))
- native gh-account awareness — statusline @login + doctor check ([#1994](https://github.com/bobmatnyc/trusty-tools/pull/1994)) ([`521f696`](https://github.com/bobmatnyc/trusty-tools/commit/521f696f037092cd60493dfc748eeea5749ec867))
- extract managed-session naming to trusty-common; trusty-agents adopts it (DOC-33 Phase 1, SPEC-ONESM-01) ([#1989](https://github.com/bobmatnyc/trusty-tools/pull/1989)) ([`025941d`](https://github.com/bobmatnyc/trusty-tools/commit/025941dfe68b7806f1f7f6b82bc06923d1cd5b9e))
- add /tm-session-pause and /tm-session-resume skills ([#1986](https://github.com/bobmatnyc/trusty-tools/pull/1986)) ([`0829711`](https://github.com/bobmatnyc/trusty-tools/commit/0829711037723085f701363b93c2de67214425e7))
- enforce PM delegation prohibitions via PreToolUse guard ([#1985](https://github.com/bobmatnyc/trusty-tools/pull/1985)) ([`ea14e69`](https://github.com/bobmatnyc/trusty-tools/commit/ea14e6981e40b1dac5d7faaf3d26476d0351932d))

### Fixed

- route Claude spawn through shared spawn_command builder ([#2017](https://github.com/bobmatnyc/trusty-tools/pull/2017)) ([`96b1ff5`](https://github.com/bobmatnyc/trusty-tools/commit/96b1ff5ef030e2cbafbca7fac9c3bcc5de746ea8))
- stop managed-hook duplicate accumulation in shared settings.json ([#2019](https://github.com/bobmatnyc/trusty-tools/pull/2019)) ([`7706bf7`](https://github.com/bobmatnyc/trusty-tools/commit/7706bf72f17afabc9df306b884a993faa31da3e4))
- resilient resume — existence-check before --resume ([#2016](https://github.com/bobmatnyc/trusty-tools/pull/2016)) ([`85197db`](https://github.com/bobmatnyc/trusty-tools/commit/85197db401499139d3a71069eb122ae4685fdcf4))
- correct env arg order in managed spawn (-u before CLAUDE_CONFIG_DIR) ([#2009](https://github.com/bobmatnyc/trusty-tools/pull/2009)) ([`13a949d`](https://github.com/bobmatnyc/trusty-tools/commit/13a949da42f4d12bf3e535bb638d4ee77d030820))
- make bundled PM skill examples stack-neutral (closes #2005) ([#2006](https://github.com/bobmatnyc/trusty-tools/pull/2006)) ([`53c3cdc`](https://github.com/bobmatnyc/trusty-tools/commit/53c3cdce3b99e2635565a58a1e722e3abcffc405))
- auto-reconcile zombie sessions on guided resume instead of erroring (closes #2001) ([#2004](https://github.com/bobmatnyc/trusty-tools/pull/2004)) ([`f635216`](https://github.com/bobmatnyc/trusty-tools/commit/f635216c4b1e6edf7463b2202283c2b39e0e180e))
- statusline shows user/repo, drops duplicate session-id branch ([#1991](https://github.com/bobmatnyc/trusty-tools/pull/1991)) ([`58f5b1f`](https://github.com/bobmatnyc/trusty-tools/commit/58f5b1f9df62dfae183389f5e279d8579f07de13))
- graceful CLI session termination + managed-session delegation gating ([#1979](https://github.com/bobmatnyc/trusty-tools/pull/1979)) ([`139bde2`](https://github.com/bobmatnyc/trusty-tools/commit/139bde2938f8edf33cd71df1d0a2b2a09ef1580c))

### Added

- compact splash art update, supersede giant-robot banner ([#1973](https://github.com/bobmatnyc/trusty-tools/pull/1973)) ([`9dd65fb`](https://github.com/bobmatnyc/trusty-tools/commit/9dd65fb875585cb41aa6a6affe7aae9a01df578a))
- switch session naming to tm-<leaf>-NN pattern ([#1966](https://github.com/bobmatnyc/trusty-tools/pull/1966)) ([`5c20a3d`](https://github.com/bobmatnyc/trusty-tools/commit/5c20a3dc3a0be4ea93ec298d7ee04ecabff0fb46))
- giant-robot «TRUSTY» banner art + stale banner.txt seed refresh ([#1933](https://github.com/bobmatnyc/trusty-tools/pull/1933)) ([`25cd224`](https://github.com/bobmatnyc/trusty-tools/commit/25cd22434d2bce527c6b0b4d7cb123c73d167928))
- extend per-stage provisioning progress to in-project and local-path spawns (closes #1919) ([#1923](https://github.com/bobmatnyc/trusty-tools/pull/1923)) ([`211b7ba`](https://github.com/bobmatnyc/trusty-tools/commit/211b7ba211873b9505090b8ee84ad521ebf97f19))
- unify session start with protected-path routing, rename sessions->session, bare tm shortcut (closes #1916) ([#1920](https://github.com/bobmatnyc/trusty-tools/pull/1920)) ([`0f40c01`](https://github.com/bobmatnyc/trusty-tools/commit/0f40c01085d15d6ec5f7f2424593640ad11da23e))
- add Trusty MPM v0.12.0 wordmark to splash banner (closes #1907) ([#1912](https://github.com/bobmatnyc/trusty-tools/pull/1912)) ([`d02d75c`](https://github.com/bobmatnyc/trusty-tools/commit/d02d75c26fd7d134c29a4f90d7434a2c094039ef))
- add tm meta-harness self-awareness text to BASE_PM identity (closes #1906) ([#1911](https://github.com/bobmatnyc/trusty-tools/pull/1911)) ([`076dfd3`](https://github.com/bobmatnyc/trusty-tools/commit/076dfd318740404a650cae4d52d613995a4c4459))
- complete mpm-* to tm-* skill fork, fork-safe one-time cleanup migration ([#1910](https://github.com/bobmatnyc/trusty-tools/pull/1910)) ([`0d3d93b`](https://github.com/bobmatnyc/trusty-tools/commit/0d3d93b68939ab672088affb0744070e945032da))
- stream real per-stage provisioning progress + trigger index reindex on session launch ([#1909](https://github.com/bobmatnyc/trusty-tools/pull/1909)) ([`287a49e`](https://github.com/bobmatnyc/trusty-tools/commit/287a49eeb896d56e51334fdabc02ecfc6b885e10))
- add report-only dry-run gate to idle auto-teardown (closes #1783) ([#1899](https://github.com/bobmatnyc/trusty-tools/pull/1899)) ([`3a7adf3`](https://github.com/bobmatnyc/trusty-tools/commit/3a7adf33c9512893881b8ba4c7e5ca54e6661f2a))
- rebuild PM skills as /tm- portfolio + fix skill-deploy bugs ([#1872](https://github.com/bobmatnyc/trusty-tools/pull/1872)) ([`2a245e8`](https://github.com/bobmatnyc/trusty-tools/commit/2a245e8724959ef7f38cb95e59c0367e36932cf8))
- auto-seed R3 identity prompt-fact at provision (DOC-28 Phase 2, epic #1855) ([`1b553c1`](https://github.com/bobmatnyc/trusty-tools/commit/1b553c17d5a51fcd324dd538ce5ed42cda3bdb97))

### Fixed

- clarify tool-output compression scope, ZTK/RTK status (closes #1944) ([#1952](https://github.com/bobmatnyc/trusty-tools/pull/1952)) ([`7b0a5bd`](https://github.com/bobmatnyc/trusty-tools/commit/7b0a5bd1f839ac531ce01e45db0bef9166aa5739))
- correct stale PM instruction template content (closes #1943) ([#1951](https://github.com/bobmatnyc/trusty-tools/pull/1951)) ([`c735452`](https://github.com/bobmatnyc/trusty-tools/commit/c735452c6b21df7ff24ee8a5925d131da8feb8cc))
- register provisioned native sessions so session_list finds them (closes #1946) ([#1949](https://github.com/bobmatnyc/trusty-tools/pull/1949)) ([`b8ff3cc`](https://github.com/bobmatnyc/trusty-tools/commit/b8ff3cc5d280378f0853ac3634b358291e613124))
- reconcile manifest drift, scope roster by project language, fix catalog sync (closes #1940, closes #1941, closes #1947) ([#1950](https://github.com/bobmatnyc/trusty-tools/pull/1950)) ([`e136d6c`](https://github.com/bobmatnyc/trusty-tools/commit/e136d6cd0b141071200b89b244a38ca6549b2cf9))
- clarify agent_delegate is a tracking gate, not the delegation path (closes #1942) ([#1948](https://github.com/bobmatnyc/trusty-tools/pull/1948)) ([`d38359f`](https://github.com/bobmatnyc/trusty-tools/commit/d38359fe29c40302c1482c8a1e3949c42e705154))
- palace-level alias resolution for claude-mpm parity (owner-repo -> bare palace) ([#1945](https://github.com/bobmatnyc/trusty-tools/pull/1945)) ([`af7f904`](https://github.com/bobmatnyc/trusty-tools/commit/af7f90499402971ac65aed5b104cde251e182599))
- address review follow-ups from #1936 (closes #1937) ([#1938](https://github.com/bobmatnyc/trusty-tools/pull/1938)) ([`1c0afa3`](https://github.com/bobmatnyc/trusty-tools/commit/1c0afa3c985680f671919811991c7eb5ef51d21c))
- use shared base checkout + git worktree per session (closes #1935) ([#1936](https://github.com/bobmatnyc/trusty-tools/pull/1936)) ([`40b9e3e`](https://github.com/bobmatnyc/trusty-tools/commit/40b9e3e2bfdef0d31ae432b6dbe43e885422a13a))
- daemon managed-spawn deploys agents/skills to worktree, not real ~/.claude (closes #1931) ([#1934](https://github.com/bobmatnyc/trusty-tools/pull/1934)) ([`d0c655d`](https://github.com/bobmatnyc/trusty-tools/commit/d0c655d142c8f2e043d09546c5905e7d165c4399))
- standalone load_alias deploys to isolated config, not real ~/.claude (closes #1927) ([#1930](https://github.com/bobmatnyc/trusty-tools/pull/1930)) ([`81e1924`](https://github.com/bobmatnyc/trusty-tools/commit/81e1924f662f1cee5b013d2db3c3840ee480bb98))
- resolve tm statusline to absolute path, heal stale bare entries (closes #1914) ([#1925](https://github.com/bobmatnyc/trusty-tools/pull/1925)) ([`104b4bf`](https://github.com/bobmatnyc/trusty-tools/commit/104b4bfb649315594cbad7a36931bf136b6a08e3))
- auto-refresh stale skill source dir, log skill deployment counts (closes #1917) ([#1922](https://github.com/bobmatnyc/trusty-tools/pull/1922)) ([`a124a2e`](https://github.com/bobmatnyc/trusty-tools/commit/a124a2e659a1eb620b2bdaeba36d492c099dc5cf))
- prevent orphan-GC from killing legitimate sessions across daemon restart (closes #1918) ([#1921](https://github.com/bobmatnyc/trusty-tools/pull/1921)) ([`1c90c11`](https://github.com/bobmatnyc/trusty-tools/commit/1c90c11aff433c1bdfc0ccc0bf94a14050858cdc))
- call prepare_session in spawn_managed_inproject, add statusline self-heal on resume (closes #1913) ([#1915](https://github.com/bobmatnyc/trusty-tools/pull/1915)) ([`7c3b052`](https://github.com/bobmatnyc/trusty-tools/commit/7c3b05222deaa06146b16b8ca7b0d0a9fab1c3f9))
- correct launchd plist detection in guided autostart ([#1901](https://github.com/bobmatnyc/trusty-tools/pull/1901)) ([`aa5da12`](https://github.com/bobmatnyc/trusty-tools/commit/aa5da12ce413b8a186d6afeb251ca48b758fd70e))
- auto-transition sessions to stopped when runtime process exits (closes #1814) ([#1894](https://github.com/bobmatnyc/trusty-tools/pull/1894)) ([`b4bee16`](https://github.com/bobmatnyc/trusty-tools/commit/b4bee163541f444b58de396f3637faae36db65e1))
- clean up managed-clone worktree dirs on decommission + reap orphans (closes #1838) ([#1895](https://github.com/bobmatnyc/trusty-tools/pull/1895)) ([`8843f9c`](https://github.com/bobmatnyc/trusty-tools/commit/8843f9c1ef2e2421cc06b32cb6c233ea9e37988b))
- unify workspace-root resolution + handle pre-existing old-layout dirs (closes #1805, closes #1807) ([#1896](https://github.com/bobmatnyc/trusty-tools/pull/1896)) ([`875d8e3`](https://github.com/bobmatnyc/trusty-tools/commit/875d8e310a2c0f0487b1bc0cd9e2e33b8e1ddc00))
- require opt-in before MCP-triggered session spawn for unregistered repos (#1836, #1837) ([`2a6252f`](https://github.com/bobmatnyc/trusty-tools/commit/2a6252fc06552e08976cf813f0b2de1af64a1e56))
- session attach uses switch-client inside tmux (nested-tmux fix) ([#1875](https://github.com/bobmatnyc/trusty-tools/pull/1875)) ([`93324c2`](https://github.com/bobmatnyc/trusty-tools/commit/93324c201ce9c18d6aa5a634da9361580c787e91))
- output-style resolution + test-isolation hygiene (#1860, #1863, #1858) ([`8a9a4f9`](https://github.com/bobmatnyc/trusty-tools/commit/8a9a4f9db79025d59fc1263e023984492d0af3e1))

### Changed

- hoist compress::tool_output from trusty-agents ([#1959](https://github.com/bobmatnyc/trusty-tools/pull/1959)) ([#1968](https://github.com/bobmatnyc/trusty-tools/pull/1968)) ([`7cf93b9`](https://github.com/bobmatnyc/trusty-tools/commit/7cf93b9ab3918aff316238bdfe540a4053aa971d))
- split inproject.rs to satisfy 500-SLOC production cap ([#1898](https://github.com/bobmatnyc/trusty-tools/pull/1898)) ([`43066c2`](https://github.com/bobmatnyc/trusty-tools/commit/43066c2a30cd9bdc6513554caff9ad2da54defd2))

### Changed: CLI command group `session` → `sessions` (issue #1394)

The top-level CLI command group was renamed from the singular `session` to the
plural **`sessions`** to match the `/api/v1/sessions/*` HTTP API surface. Every
subcommand is now invoked under the plural name, e.g. `tm sessions tui`,
`tm sessions ls`, `tm sessions new`.

- The singular `session` spelling is **removed entirely** — it is not retained
  as an alias. Invoking `tm session …` now fails with an
  unrecognized-subcommand error. Update any scripts or muscle memory to
  `tm sessions …`.
- This is a CLI-only change; the HTTP API (already `/api/v1/sessions/*`) and the
  separate `session-manager` / `sm` coordinator command are unaffected.

### Deprecated: verbose managed session-lifecycle verbs (issue #1205)

The managed session-lifecycle CLI verbs were renamed to the cleaner, symmetric
`stop` / `resume` / `decommission` family. The old verbose verbs still work but
now emit a one-line deprecation notice to **stderr** on every invocation and
will be removed in a future release.

| Deprecated verb | Use instead | Behavior |
|-----------------|-------------|----------|
| `tm sessions runtime-stop <id>` | `tm sessions stop <id>` | Stop the runtime, keep the workspace (resumable) |
| `tm sessions managed-stop <id>` | `tm sessions stop <id>` | Same as `runtime-stop` |
| `tm sessions managed-resume <id>` | `tm sessions resume <id>` | Re-spawn the runtime in the existing workspace |

- The deprecated verbs are hidden from `tm sessions --help` but continue to parse
  for backward compatibility.
- Each deprecated invocation prints `warning: '<old>' is deprecated; use '<new>'`
  to stderr; stdout stays clean for scripts.
- `tm sessions decommission <id>` (terminal teardown: remove workspace from disk)
  is unchanged.

## [0.19.4] — 2026-07-09

### Changed

- Add crates.io package metadata (keywords/categories/homepage/readme).

## [0.14.0] — 2026-07-01

### Added

DOC-28 self-awareness (R1-R4 of `docs/specs/trusty-mpm-self-awareness.md`), closing the
gaps behind the "self-awareness incident" where a session conflated this Rust
`trusty-mpm` with the unrelated Python `claude-mpm`, and no mechanism detected
that its instructions never loaded ([#1855](https://github.com/bobmatnyc/trusty-tools/pull/1855)) ([#1859](https://github.com/bobmatnyc/trusty-tools/pull/1859)) ([`5708d95`](https://github.com/bobmatnyc/trusty-tools/commit/5708d95310dd02c903502d812990c8c6d9d743e8)):

- **R1 — canonical self-description doc**: new bundled
  `crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md`, deployed via
  `bundle_all.rs::ALL` with `InstallPolicy::Overwrite` so `tm install` installs
  it to `~/.trusty-mpm/framework/docs/`.
- **R2 — identity protocol in instructions**: "Identity & Self-Awareness
  Protocol (Non-Overridable)" section added verbatim to `BASE_SM.md`'s
  non-overridable floor and to all three bundled output styles
  (`trusty-mpm.md`, `trusty-mpm-teacher.md`, `trusty-mpm-research.md`),
  routing identity questions through memory + the R1 doc and forbidding
  shell-probing (`pip3 show`, `which claude-mpm`).
- **R3 — manual identity-fact seeding**: documents the `kg_assert` identity-fact
  seed step in the R1 doc (automatic seeding from `prepare_session` is deferred
  future work per spec §7 Phase 2/§10).
- **R4 — `tm doctor` output-style check + load marker**: new
  `daemon/doctor_output_style.rs` probe validates that the effective
  `outputStyle` setting resolves to a real bundled style file (Ok/Warn/Fail per
  spec, reproducing the exact incident condition as a Fail), plus the
  greppable `<!-- trusty-mpm-instructions-loaded: v1 -->` load marker at the
  top of each floor section.

## [0.13.0] — 2026-06-30

### Added

- route tm CLI through console gateway with direct fallback ([#1852](https://github.com/bobmatnyc/trusty-tools/pull/1852)) ([`b1b58bc`](https://github.com/bobmatnyc/trusty-tools/commit/b1b58bc30a423db98d21d5254c3d4d8ac71ab8a5))
- wire trusty-mpm into console reverse proxy ([#1850](https://github.com/bobmatnyc/trusty-tools/pull/1850)) ([`970d297`](https://github.com/bobmatnyc/trusty-tools/commit/970d297bf9448cf74b3117445401524bd17b20e4))
- idle auto-suspend + scrollback snapshot + resume restoration (opt-in) ([#1816](https://github.com/bobmatnyc/trusty-tools/pull/1816)) ([#1822](https://github.com/bobmatnyc/trusty-tools/pull/1822)) ([`5e28313`](https://github.com/bobmatnyc/trusty-tools/commit/5e283135fa0ea959aed720573a1302c822c02fb9))
- runtime-editable splash art + block-robot default banner ([#1825](https://github.com/bobmatnyc/trusty-tools/pull/1825)) ([#1829](https://github.com/bobmatnyc/trusty-tools/pull/1829)) ([`bd4a72a`](https://github.com/bobmatnyc/trusty-tools/commit/bd4a72a9d1dce02d27db14b894dbc725394247d5))
- auto-register git project alias on tm launch + tm ls shows local paths ([#1819](https://github.com/bobmatnyc/trusty-tools/pull/1819)) ([`436f7c9`](https://github.com/bobmatnyc/trusty-tools/commit/436f7c9194077822fb0d44de9cff4fd1f4862909))
- kawaii row-of-three pixel-bots replace scary ASCII art ([#1811](https://github.com/bobmatnyc/trusty-tools/pull/1811)) ([#1812](https://github.com/bobmatnyc/trusty-tools/pull/1812)) ([`d8e2111`](https://github.com/bobmatnyc/trusty-tools/commit/d8e2111d882c7237c77214dd122d6337d6d8fde3))
- unify daily tm banner with tm banner + hide decommissioned tombstones (#1808, #1809) ([#1810](https://github.com/bobmatnyc/trusty-tools/pull/1810)) ([`fba0160`](https://github.com/bobmatnyc/trusty-tools/commit/fba0160a2d12132ef36c2c069502b7acd9be3a5e))
- unify managed clone on shared base + per-session .worktrees/ ([#1803](https://github.com/bobmatnyc/trusty-tools/pull/1803)) ([#1804](https://github.com/bobmatnyc/trusty-tools/pull/1804)) ([`c32fc0c`](https://github.com/bobmatnyc/trusty-tools/commit/c32fc0c0623a16757c9d0a4d65d8150a29e6c2d0))
- refine tm robot banner — clip art, dedupe version, owner/repo + managed path ([#1794](https://github.com/bobmatnyc/trusty-tools/pull/1794)) ([`9183e11`](https://github.com/bobmatnyc/trusty-tools/commit/9183e11d52ee8a1076ed09c894c746af1e493ab7))
- redirect tm launch + spawn_managed_local to managed clone ([#1590](https://github.com/bobmatnyc/trusty-tools/pull/1590)) ([#1796](https://github.com/bobmatnyc/trusty-tools/pull/1796)) ([`6c0af3c`](https://github.com/bobmatnyc/trusty-tools/commit/6c0af3c798686a0eb62cdb734b0febc39d0d265c))
- detach returns to tm picker + daemon/clone cwd hardening ([#1795](https://github.com/bobmatnyc/trusty-tools/pull/1795)) ([`3b0e723`](https://github.com/bobmatnyc/trusty-tools/commit/3b0e7231e85ca8fbc53dbd55bb4968d4d96e811c))
- include repo name in managed tmux session names ([#1789](https://github.com/bobmatnyc/trusty-tools/pull/1789)) ([#1791](https://github.com/bobmatnyc/trusty-tools/pull/1791)) ([`c1887de`](https://github.com/bobmatnyc/trusty-tools/commit/c1887defa898e32ee4a424869a810b40567aa979))
- session-manager daily QoL fixes — ls --source-id, info fallback, honest decommission ([#1787](https://github.com/bobmatnyc/trusty-tools/pull/1787)) ([#1788](https://github.com/bobmatnyc/trusty-tools/pull/1788)) ([`9e9c795`](https://github.com/bobmatnyc/trusty-tools/commit/9e9c795ed07399a6e252e2071d8bc0c161dba1ff))
- add context compaction efficiency segment ([#1774](https://github.com/bobmatnyc/trusty-tools/pull/1774)) ([`0594d8f`](https://github.com/bobmatnyc/trusty-tools/commit/0594d8f10e68d485ee190f7b76f072709eb18158))
- tm guided-default auto-start daemon + github-* SSH alias support ([#1775](https://github.com/bobmatnyc/trusty-tools/pull/1775)) ([#1776](https://github.com/bobmatnyc/trusty-tools/pull/1776)) ([`20374a2`](https://github.com/bobmatnyc/trusty-tools/commit/20374a26e1b445f11dfabe6a46ce69325decd5f8))
- DOC-28 cutover catch-up runtime — watermark + git/palace + auto-inject (PR2/3/4, #1762) ([`e7e23ea`](https://github.com/bobmatnyc/trusty-tools/commit/e7e23ea2ae1a679e285391ea452272ec5bbbfee2))
- DOC-28 cutover bridge core — tm sessions catchup (PR1, #1762) ([`d66a989`](https://github.com/bobmatnyc/trusty-tools/commit/d66a989a2f6cd4e208fd1d69092d7f644da3e23e))

### Fixed

- session-worktree prune/decommission hardening ([#1845](https://github.com/bobmatnyc/trusty-tools/pull/1845)) ([#1853](https://github.com/bobmatnyc/trusty-tools/pull/1853)) ([`ff970ed`](https://github.com/bobmatnyc/trusty-tools/commit/ff970ed88549f2536f47c00bc232e70dde2561bb))
- normalise lock-file URL before TCP probe in banner ([#1847](https://github.com/bobmatnyc/trusty-tools/pull/1847)) ([#1848](https://github.com/bobmatnyc/trusty-tools/pull/1848)) ([`df5c330`](https://github.com/bobmatnyc/trusty-tools/commit/df5c330d14fd43bfd0f8174582ef52a74adf0b7c))
- CLI ergonomics fixes for tm sessions ([#1846](https://github.com/bobmatnyc/trusty-tools/pull/1846)) ([`9e4f4b9`](https://github.com/bobmatnyc/trusty-tools/commit/9e4f4b98f6e236ec6fa106f713258fba5a03ba2a))
- managed-session lifecycle correctness ([#1840](https://github.com/bobmatnyc/trusty-tools/pull/1840)) ([#1844](https://github.com/bobmatnyc/trusty-tools/pull/1844)) ([`78f2bc2`](https://github.com/bobmatnyc/trusty-tools/commit/78f2bc29a9ddb14a2bd7b9c23e1b36e740ee36f3))
- banner/health/entry UX fixes ([#1839](https://github.com/bobmatnyc/trusty-tools/pull/1839)) ([#1843](https://github.com/bobmatnyc/trusty-tools/pull/1843)) ([`1863c22`](https://github.com/bobmatnyc/trusty-tools/commit/1863c22692b85621c3e15ef30aa7436b5b0883da))
- skip worktree checkouts in auto-registration + document TRUSTY_MPM_BANNER_FILE ([#1835](https://github.com/bobmatnyc/trusty-tools/pull/1835)) ([`d2e1ab8`](https://github.com/bobmatnyc/trusty-tools/commit/d2e1ab84a8f0a37dac6e14e7d03cbc02b566b630))
- orphan-GC log-spam reduction + key-match regression tests (closes #1813) ([#1823](https://github.com/bobmatnyc/trusty-tools/pull/1823)) ([`836f393`](https://github.com/bobmatnyc/trusty-tools/commit/836f3934e005a3436d1f775480b511a23017218e))
- RAII guard kills leaked tmux sessions after test/error ([#1815](https://github.com/bobmatnyc/trusty-tools/pull/1815)) ([#1821](https://github.com/bobmatnyc/trusty-tools/pull/1821)) ([`af67403`](https://github.com/bobmatnyc/trusty-tools/commit/af67403f3c5fe016cc03c532faafc70d19e3796e))
- stop session-manager tests leaking real tmux sessions into production store ([#1790](https://github.com/bobmatnyc/trusty-tools/pull/1790)) ([#1793](https://github.com/bobmatnyc/trusty-tools/pull/1793)) ([`b3410e4`](https://github.com/bobmatnyc/trusty-tools/commit/b3410e4fa5373a7df6759a369e3ccc38d99b4a24))
- session-manager on-ramp blockers — source_id backfill + first-run clone feedback ([#1780](https://github.com/bobmatnyc/trusty-tools/pull/1780)) ([#1781](https://github.com/bobmatnyc/trusty-tools/pull/1781)) ([`313b962`](https://github.com/bobmatnyc/trusty-tools/commit/313b962c00e92b036fec76ad09d8ca72256ce367))
- non-GitHub remote refusal no longer blames daemon ([#1777](https://github.com/bobmatnyc/trusty-tools/pull/1777)) ([#1778](https://github.com/bobmatnyc/trusty-tools/pull/1778)) ([`cc9d152`](https://github.com/bobmatnyc/trusty-tools/commit/cc9d152b81991bba15c35553ec95fcfd596213a8))

### Changed

- extract DOC-28 catch-up engine behind catchup feature (PR1, #1762) ([`addfdbb`](https://github.com/bobmatnyc/trusty-tools/commit/addfdbb04ed78028887a0e782afe7cfe83c10b46))

---

## [0.12.0] — 2026-06-27

### Added

- two-panel full-width banner (robot left, info right, natural height) ([#1759](https://github.com/bobmatnyc/trusty-tools/pull/1759)) ([`37f3810`](https://github.com/bobmatnyc/trusty-tools/commit/37f3810e3dd238b4b3509c0b65d48393d355e934))
- full-screen rust robot banner + bypass-permissions launch ([#1755](https://github.com/bobmatnyc/trusty-tools/pull/1755)) ([`0924589`](https://github.com/bobmatnyc/trusty-tools/commit/092458947d3c5487e188ba260744754cfd486f37))
- ungraceful-exit handling + --resume conversation continuity (closes #1744) ([#1748](https://github.com/bobmatnyc/trusty-tools/pull/1748)) ([`40989bd`](https://github.com/bobmatnyc/trusty-tools/commit/40989bd30f2e35f9b365cdb7a877348505f9e8c1))
- expanded pre-launch welcome panel — recent commits, service status, TM commands (closes #1743) ([#1747](https://github.com/bobmatnyc/trusty-tools/pull/1747)) ([`689a9be`](https://github.com/bobmatnyc/trusty-tools/commit/689a9bed62b39640f099f038c453617a3d16d73c))
- tm welcome banner box + rich Claude Code statusline + tmux detach hint ([#1740](https://github.com/bobmatnyc/trusty-tools/pull/1740)) ([`db0a115`](https://github.com/bobmatnyc/trusty-tools/commit/db0a11553da9e81a1ee8f36b4204ee0f768f0a41))
- guided-default session picker when tm run from a repo ([#1705](https://github.com/bobmatnyc/trusty-tools/pull/1705)) ([#1729](https://github.com/bobmatnyc/trusty-tools/pull/1729)) ([`40ec125`](https://github.com/bobmatnyc/trusty-tools/commit/40ec1252d44cb29c3540b69b70bc052a935851e0))
- chat session manager MVP — force flag, turn tools, palace_dream, Task drawer (closes #1719 #1720 #1721 #1722) ([#1723](https://github.com/bobmatnyc/trusty-tools/pull/1723)) ([`7b22f28`](https://github.com/bobmatnyc/trusty-tools/commit/7b22f28e2c4f256eda0678a01fac16bd1584685b))
- in-project protected workspace + claude-mpm parity (epic #1590) ([#1715](https://github.com/bobmatnyc/trusty-tools/pull/1715)) ([`abd9914`](https://github.com/bobmatnyc/trusty-tools/commit/abd991451ba84a771ff91fc06e86e390de30ac32))
- usability sprint 1 — lock-file URL, startup prompts, TASK.md, offline swagger ([#1697](https://github.com/bobmatnyc/trusty-tools/pull/1697)) ([`d5e7e37`](https://github.com/bobmatnyc/trusty-tools/commit/d5e7e3776852d353b407d04d8623376f98298f56))
- WI-5 follow-ups — OpenRouter classifier call + auth-timeout auto-stop (closes #1648, closes #1649) ([#1656](https://github.com/bobmatnyc/trusty-tools/pull/1656)) ([`6c71d64`](https://github.com/bobmatnyc/trusty-tools/commit/6c71d646aba04e3e530b081150166518ec827dd3))
- pin palace slug in standalone MCP injection (closes #1651) ([#1655](https://github.com/bobmatnyc/trusty-tools/pull/1655)) ([`663c9ea`](https://github.com/bobmatnyc/trusty-tools/commit/663c9eab5680830157a864484f01985eaabf0dba))
- pin trusty-memory palace slug in managed-session MCP injection (closes #1605) ([#1652](https://github.com/bobmatnyc/trusty-tools/pull/1652)) ([`d15c96d`](https://github.com/bobmatnyc/trusty-tools/commit/d15c96dc846e805f2ddf6549d157d2719afd4e9a))
- SESSCTL WI-5 auth + cost model (closes #1596) ([#1647](https://github.com/bobmatnyc/trusty-tools/pull/1647)) ([`c51a5f6`](https://github.com/bobmatnyc/trusty-tools/commit/c51a5f6ae68cee071d320a92b23b168cb7c4e441))

### Fixed

- absolute-path + project-scope + opt-out for Claude hooks (fail-open hardening) ([#1756](https://github.com/bobmatnyc/trusty-tools/pull/1756)) ([`e382abb`](https://github.com/bobmatnyc/trusty-tools/commit/e382abb5c335d1b2429934dc240651cb0d608235))
- idempotent catalog sync — update existing checkout instead of failing on re-clone (closes #1751) ([#1752](https://github.com/bobmatnyc/trusty-tools/pull/1752)) ([`8a70a30`](https://github.com/bobmatnyc/trusty-tools/commit/8a70a3048bd9f699261387a213a10ce67f542a19))
- guided resume restarts a stopped session instead of raw-attaching a dead tmux session (closes #1742) ([#1745](https://github.com/bobmatnyc/trusty-tools/pull/1745)) ([`83f30ba`](https://github.com/bobmatnyc/trusty-tools/commit/83f30ba43e66baefe3715da20281a756997bc7ab))
- hermetic test isolation for managed-session & prune-idle tests (closes #1734) ([#1736](https://github.com/bobmatnyc/trusty-tools/pull/1736)) ([`d0be201`](https://github.com/bobmatnyc/trusty-tools/commit/d0be201928ab9e7c1b7e80c1d23ecb741d38536f))
- include source_id in record_to_json to match record_to_summary (closes #1733) ([#1735](https://github.com/bobmatnyc/trusty-tools/pull/1735)) ([`a901a18`](https://github.com/bobmatnyc/trusty-tools/commit/a901a18a420a31eab0a80f9d6a0c6ccaf5355e0d))
- client source_id field + daemon URL resolution probing for guided tm (closes #1730, closes #1731) ([`2f1eef5`](https://github.com/bobmatnyc/trusty-tools/commit/2f1eef59b04f104bd7444b9fbe1a11837e44cb83))
- redirect guided-default fallback to managed clone, never live checkout ([#1724](https://github.com/bobmatnyc/trusty-tools/pull/1724)) ([#1728](https://github.com/bobmatnyc/trusty-tools/pull/1728)) ([`5a7d9f1`](https://github.com/bobmatnyc/trusty-tools/commit/5a7d9f18844fee4677d0fadfe50fb0946373bd5f))

### Changed

- publish trusty-agents-common 0.1.3 + trusty-mpm 0.11.0 to crates.io ([#1750](https://github.com/bobmatnyc/trusty-tools/pull/1750)) ([`70194ec`](https://github.com/bobmatnyc/trusty-tools/commit/70194ec1788fed2e71016912dae4e062baade139))

---

## [0.11.0] — 2026-06-24

### Added

- orphan-GC PID registry + PR A nits ([#1595](https://github.com/bobmatnyc/trusty-tools/pull/1595)) ([#1637](https://github.com/bobmatnyc/trusty-tools/pull/1637)) ([`3886d33`](https://github.com/bobmatnyc/trusty-tools/commit/3886d33e077240bac3e5417427818c41a66e5d8b))
- WI-4 PR A — graceful shutdown hardening (refs #1595) ([#1617](https://github.com/bobmatnyc/trusty-tools/pull/1617)) ([`7f5ed43`](https://github.com/bobmatnyc/trusty-tools/commit/7f5ed43a646fbb1c67f2eb176703a11f696e0712))
- WI-3 SESSCTL Phase 3 — activity observability (closes #1594) ([#1600](https://github.com/bobmatnyc/trusty-tools/pull/1600)) ([`36aebaf`](https://github.com/bobmatnyc/trusty-tools/commit/36aebaf9a43d117dc7441253a52d0b648f00487e))
- WI-2 SESSCTL Phase 2 — sessctl command surface + daemon HTTP endpoints (closes #1593) ([#1599](https://github.com/bobmatnyc/trusty-tools/pull/1599)) ([`3647649`](https://github.com/bobmatnyc/trusty-tools/commit/3647649eb824ff26cdc3524ac89a1004b9e1f9f4))
- WI-1 SESSCTL Phase 1 — backend trait + SessionActor + registry foundation (closes #1592) ([#1598](https://github.com/bobmatnyc/trusty-tools/pull/1598)) ([`68d102a`](https://github.com/bobmatnyc/trusty-tools/commit/68d102a9dc8706a59120923c55bbcbca13e0dae6))
- WI-B group /fleet output by project ([#1588](https://github.com/bobmatnyc/trusty-tools/pull/1588)) ([`9d88c33`](https://github.com/bobmatnyc/trusty-tools/commit/9d88c33880f51d2f857b92f6fad7526bc4ea3c1d))
- WI-A thread repo_url/ref_ through LaunchParams + sessions.launch ([#1587](https://github.com/bobmatnyc/trusty-tools/pull/1587)) ([`64ba815`](https://github.com/bobmatnyc/trusty-tools/commit/64ba815aaf3976e87c686a4e49c8c8ff26833ccb))
- WI-1 isolation regression-guard with version capture (closes #1582, refs #1548) ([#1583](https://github.com/bobmatnyc/trusty-tools/pull/1583)) ([`23d846c`](https://github.com/bobmatnyc/trusty-tools/commit/23d846c35058128d953f71c9aaf4933e1630d67d))
- output-style filesystem deployer for managed config (closes #1553) ([#1580](https://github.com/bobmatnyc/trusty-tools/pull/1580)) ([`46e6f40`](https://github.com/bobmatnyc/trusty-tools/commit/46e6f4092606c98aed2f32459b99cbb28cc5557b))
- tm update + tm rm standalone lifecycle subcommands ([#1578](https://github.com/bobmatnyc/trusty-tools/pull/1578)) ([`b5e4b20`](https://github.com/bobmatnyc/trusty-tools/commit/b5e4b20378d122318bb8bb690911b8c51512d571))
- configurable managed-root via --root / TRUSTY_MPM_ROOT / config.toml ([#1567](https://github.com/bobmatnyc/trusty-tools/pull/1567)) ([`7f781e0`](https://github.com/bobmatnyc/trusty-tools/commit/7f781e0de2e8f9bec2863998f1c0b664c1393c37))
- WI-8 wire trusty-review MCP into tm-global managed config (refs #1548) ([#1563](https://github.com/bobmatnyc/trusty-tools/pull/1563)) ([`a4f1805`](https://github.com/bobmatnyc/trusty-tools/commit/a4f18051a8a64851b37244afda2bbb2fa002c1f0))
- WI-3 managed-session hook-clean + trust-seed + MCP-enable (refs #1548) ([#1555](https://github.com/bobmatnyc/trusty-tools/pull/1555)) ([`66ee38a`](https://github.com/bobmatnyc/trusty-tools/commit/66ee38a58d25e915d7eb98448961b45b42eca390))
- WI-2 deploy bundled agents+skills into managed CLAUDE_CONFIG_DIR (refs #1548) ([#1552](https://github.com/bobmatnyc/trusty-tools/pull/1552)) ([`bce34bd`](https://github.com/bobmatnyc/trusty-tools/commit/bce34bdec7d522d47985104bc0509ced681fbfe1))
- WI-10 managed-session auth — tm login (keychain) + ANTHROPIC_API_KEY/--bare fallback (refs #1548) ([#1551](https://github.com/bobmatnyc/trusty-tools/pull/1551)) ([`539c94a`](https://github.com/bobmatnyc/trusty-tools/commit/539c94ac89a0f38a23b0db9aee9af902d6f89690))
- MVP standalone managed driver — register/load/run with CLAUDE_CONFIG_DIR isolation (refs #1548) ([#1549](https://github.com/bobmatnyc/trusty-tools/pull/1549)) ([`81ca1b0`](https://github.com/bobmatnyc/trusty-tools/commit/81ca1b0dda406113469336f3c492933d07f3bf94))
- NL->repo resolver (WI-5, refs #1517) ([#1535](https://github.com/bobmatnyc/trusty-tools/pull/1535)) ([`222d638`](https://github.com/bobmatnyc/trusty-tools/commit/222d638e6a3f2a11b5d1939299c5a31a3b063904))
- project registry + MCP tools (closes #1519) ([#1520](https://github.com/bobmatnyc/trusty-tools/pull/1520)) ([`53f95c2`](https://github.com/bobmatnyc/trusty-tools/commit/53f95c2d61c1522adfec7e90171187d39523e578))
- wire decommission/inject verbs + graceful no-creds path in action coordinator (closes #1524) ([#1525](https://github.com/bobmatnyc/trusty-tools/pull/1525)) ([`53c49dc`](https://github.com/bobmatnyc/trusty-tools/commit/53c49dc6936c6afa8443e480c1adbc1a399f431b))
- harness-understanding instructions in trusty-agents-common + DOC-21 (closes #1510) ([#1513](https://github.com/bobmatnyc/trusty-tools/pull/1513)) ([`737cddb`](https://github.com/bobmatnyc/trusty-tools/commit/737cddbb6e8908a268604f74a41f361e13f431fc))
- track & tear down ephemeral managed sessions (closes #1508) ([#1509](https://github.com/bobmatnyc/trusty-tools/pull/1509)) ([`3b7d0c9`](https://github.com/bobmatnyc/trusty-tools/commit/3b7d0c9f1225b989687f869b7158bd83b653fc70))
- Slack adapter on the chat-core seam (#1294, epic #1433) ([#1504](https://github.com/bobmatnyc/trusty-tools/pull/1504)) ([`330a2d9`](https://github.com/bobmatnyc/trusty-tools/commit/330a2d918f597b7b91da5aec9d0a9879ec5e5aef))
- web adapter on chat-core seam (refs #1433, #1295, #926) ([#1503](https://github.com/bobmatnyc/trusty-tools/pull/1503)) ([`1c0ccdf`](https://github.com/bobmatnyc/trusty-tools/commit/1c0ccdf93269cd45482a8cb76268c35bfee3247f))
- adopt existing tmux sessions + local-path managed spawn (refs #1433) ([#1502](https://github.com/bobmatnyc/trusty-tools/pull/1502)) ([`be25fff`](https://github.com/bobmatnyc/trusty-tools/commit/be25fff3aa2effdd1f462d2b670ae84e70daf973))
- drive managed fleet from Telegram — free-text→action chat + managed slash commands ([#1501](https://github.com/bobmatnyc/trusty-tools/pull/1501)) ([`5bd5c55`](https://github.com/bobmatnyc/trusty-tools/commit/5bd5c55e985df25049967334b8d3c9cfd0828540))
- add health verb to chat-core catalog + action loop (refs #1433) ([#1498](https://github.com/bobmatnyc/trusty-tools/pull/1498)) ([`5f7b526`](https://github.com/bobmatnyc/trusty-tools/commit/5f7b526982738cc6776792037ac83f9f70be7e72))
- action-capable coordinator chat — self-aware inline verb execution ([#1496](https://github.com/bobmatnyc/trusty-tools/pull/1496)) ([`5c792e7`](https://github.com/bobmatnyc/trusty-tools/commit/5c792e7e8637bd003dbb7bfcc277f263ff4d4008))
- wire STUI slash-dispatch + free-text routing through chat-core (refs #1272, #1276) ([#1494](https://github.com/bobmatnyc/trusty-tools/pull/1494)) ([`62bb3c1`](https://github.com/bobmatnyc/trusty-tools/commit/62bb3c15cf26ee2a6fc3ae2c3d7a0d9df4899273))
- route tm CLI session verbs through chat-core; drop duplicate resolvers (refs #1283) ([#1493](https://github.com/bobmatnyc/trusty-tools/pull/1493)) ([`586f2eb`](https://github.com/bobmatnyc/trusty-tools/commit/586f2eb3032979923869441e1335e0c49a82cdd0))
- chat-core nucleus — shared command layer for session-manager adapters ([#1492](https://github.com/bobmatnyc/trusty-tools/pull/1492)) ([`baaf568`](https://github.com/bobmatnyc/trusty-tools/commit/baaf5689603be72fe7625747c76fddc488d33958))
- typed DaemonClient managed-session methods + refactor tm managed cmds ([#1491](https://github.com/bobmatnyc/trusty-tools/pull/1491)) ([`c5287af`](https://github.com/bobmatnyc/trusty-tools/commit/c5287af26a98b62376914445b1853052f2c0cd6b))
- meta run launches a real Claude Code session + verifies demo artifact (closes #1049, closes #1051) ([#1489](https://github.com/bobmatnyc/trusty-tools/pull/1489)) ([`26fbf15`](https://github.com/bobmatnyc/trusty-tools/commit/26fbf15284136569190796b390597342f9afa717))
- custom-instruction loading for the metaharness (closes #1048) ([#1485](https://github.com/bobmatnyc/trusty-tools/pull/1485)) ([`ee2c498`](https://github.com/bobmatnyc/trusty-tools/commit/ee2c498253f0ec5e9f9d9a25c5f89e9a14d35d9a))
- STUI-1 numbered scrollable session list + keybindings + state preservation (refs #1278) ([#1482](https://github.com/bobmatnyc/trusty-tools/pull/1482)) ([`5bf0009`](https://github.com/bobmatnyc/trusty-tools/commit/5bf00095aef20380f6b32aeb4604ceadbbe41e18))
- standard harness-agnostic inject_text/observe/summarize on SessionControl ([#1461](https://github.com/bobmatnyc/trusty-tools/pull/1461)) ([#1463](https://github.com/bobmatnyc/trusty-tools/pull/1463)) ([`967c892`](https://github.com/bobmatnyc/trusty-tools/commit/967c892236105928ab03b25080cd293392673299))
- periodic + startup orphan-GC reconciling registries vs tmux ls ([#1458](https://github.com/bobmatnyc/trusty-tools/pull/1458)) ([#1462](https://github.com/bobmatnyc/trusty-tools/pull/1462)) ([`aa1c8f8`](https://github.com/bobmatnyc/trusty-tools/commit/aa1c8f8cee680648f8cd5231778f5bbb87a5308d))
- owning tmux Session guard with RAII Drop reaper + test teardown guards (refs #1453, #1459, epic #1452) ([#1460](https://github.com/bobmatnyc/trusty-tools/pull/1460)) ([`ff25808`](https://github.com/bobmatnyc/trusty-tools/commit/ff25808cf8fcb682851d6ba376501cece5674e6f))
- sessions TUI startup banner + service probes (STUI-0) ([#1431](https://github.com/bobmatnyc/trusty-tools/pull/1431)) ([`734b5e5`](https://github.com/bobmatnyc/trusty-tools/commit/734b5e54f2688052cadb2f5e3c26f8ab2d09b139))
- coordinator-context last_summary + summarizing flag (STUI-4) ([#1432](https://github.com/bobmatnyc/trusty-tools/pull/1432)) ([`0fda534`](https://github.com/bobmatnyc/trusty-tools/commit/0fda534e6359d8963ad59044421d4a6a82822afd))
- catalog update-check + rebuild/apply (closes #1408) ([#1429](https://github.com/bobmatnyc/trusty-tools/pull/1429)) ([`41b312a`](https://github.com/bobmatnyc/trusty-tools/commit/41b312a5cb78c469e9c3b0968107c2fd90340203))
- manifest-driven harness provisioning (HR-2, #1407) ([#1427](https://github.com/bobmatnyc/trusty-tools/pull/1427)) ([`f012658`](https://github.com/bobmatnyc/trusty-tools/commit/f012658d26560735b2659a4649cabff41b4ebac8))
- multi-style output + version-fallback injection (HR-4) ([#1412](https://github.com/bobmatnyc/trusty-tools/pull/1412)) ([`77ad339`](https://github.com/bobmatnyc/trusty-tools/commit/77ad33964556158f9816cf9e0fd7de7967ee9114))
- BASE agent content parity + initialPrompt/tier-model injection ([#1411](https://github.com/bobmatnyc/trusty-tools/pull/1411)) ([`0a28b24`](https://github.com/bobmatnyc/trusty-tools/commit/0a28b24550e041139b59c79daead1da3671d1f29))
- wire in-process AgentRunner into meta run orchestrator (closes #1030) ([#1396](https://github.com/bobmatnyc/trusty-tools/pull/1396)) ([`19b972d`](https://github.com/bobmatnyc/trusty-tools/commit/19b972d6e6158ebbc0013a59561c724226d2213a))
- coordinator TUI live session-list polling (Child #2, refs #1274) ([#1386](https://github.com/bobmatnyc/trusty-tools/pull/1386)) ([`44345df`](https://github.com/bobmatnyc/trusty-tools/commit/44345df721f9af648b0b9ac5bdc22d2ee1bcc5dc))
- coordinator TUI skeleton screen + tm coordinator-tui subcommand (Child #1, refs #1272) ([#1383](https://github.com/bobmatnyc/trusty-tools/pull/1383)) ([`2dab8d4`](https://github.com/bobmatnyc/trusty-tools/commit/2dab8d4588a139b7179716f25d8e657548cc5a72))
- wire trusty-code ToolRegistry into tm meta run (WI-2, refs #1045) ([#1384](https://github.com/bobmatnyc/trusty-tools/pull/1384)) ([`0684a7d`](https://github.com/bobmatnyc/trusty-tools/commit/0684a7d2477716904077a7381745f220b5e5c1ed))
- bootstrap tm meta run subcommand (WI-1, refs #1045) ([#1382](https://github.com/bobmatnyc/trusty-tools/pull/1382)) ([`cdaa6c7`](https://github.com/bobmatnyc/trusty-tools/commit/cdaa6c728a7d491984c32fbd27c33e64163b92c9))

### Fixed

- repair daemon::state overseer tests and daemon::api blocking-client panic (closes #1571, closes #1523) ([#1581](https://github.com/bobmatnyc/trusty-tools/pull/1581)) ([`216ee11`](https://github.com/bobmatnyc/trusty-tools/commit/216ee1144ff3173ed5fb59b37383cef10daa30c7))
- managed MVP polish — atomic-save cleanup, credential direction, idempotent .mcp.json ([#1579](https://github.com/bobmatnyc/trusty-tools/pull/1579)) ([`e1721ad`](https://github.com/bobmatnyc/trusty-tools/commit/e1721ad6943f6a32a52a7839aad16c01634cd1de))
- atomic confirm_pair_code with crash-safe claim cleanup (closes #1506) ([#1547](https://github.com/bobmatnyc/trusty-tools/pull/1547)) ([`731d915`](https://github.com/bobmatnyc/trusty-tools/commit/731d91511327124eb795fea5c6ffdfc31db18427))
- stop dropping tokio Runtime in async test context (closes #1521) ([#1522](https://github.com/bobmatnyc/trusty-tools/pull/1522)) ([`ac9365b`](https://github.com/bobmatnyc/trusty-tools/commit/ac9365b3a511ea72fd796ccb6c4e6aafb5dbd25c))
- HTML-escape Telegram command replies (closes #1514) ([#1515](https://github.com/bobmatnyc/trusty-tools/pull/1515)) ([`0e577e3`](https://github.com/bobmatnyc/trusty-tools/commit/0e577e3ab7ff7424e5a5a6242c4094e017170309))
- guard decommission against deleting non-owned workspaces (P0, closes #1511) ([#1512](https://github.com/bobmatnyc/trusty-tools/pull/1512)) ([`435a962`](https://github.com/bobmatnyc/trusty-tools/commit/435a962fc5ae631d5d11afdfc3c771fb9c2d653b))
- supervise Telegram bot + unify pairing-code store (closes #1499, closes #1500) ([#1505](https://github.com/bobmatnyc/trusty-tools/pull/1505)) ([`b5507a3`](https://github.com/bobmatnyc/trusty-tools/commit/b5507a3e59849f56e174af07c415ec448b4a7ee7))
- make telegram bot username configurable via TELEGRAM_BOT_USERNAME (default t_sess_bot) (refs #1433) ([#1497](https://github.com/bobmatnyc/trusty-tools/pull/1497)) ([`897df90`](https://github.com/bobmatnyc/trusty-tools/commit/897df90bdc25156aa8d190e7a4fdb0b5cd1a83c6))
- tmux lifecycle rollbacks — spawn send_line + registry upsert (#1456, #1457) ([#1468](https://github.com/bobmatnyc/trusty-tools/pull/1468)) ([`e471ee2`](https://github.com/bobmatnyc/trusty-tools/commit/e471ee2ead502b013f41ea59e40e75c13475ba45))
- tmux lifecycle — DELETE kills session + graceful-shutdown reaper (#1454, #1455) ([#1466](https://github.com/bobmatnyc/trusty-tools/pull/1466)) ([`4c53699`](https://github.com/bobmatnyc/trusty-tools/commit/4c5369923e592288e9b0e5d41dec028ec345078b))

### Changed

- WI-2 review nits — single missing-source hint + document deploy layout (refs #1548) ([#1556](https://github.com/bobmatnyc/trusty-tools/pull/1556)) ([`404b874`](https://github.com/bobmatnyc/trusty-tools/commit/404b8744e72ffeec07a2f1e12cb3eeeb217b38b1))
- rename CLI 'session' command group to 'sessions' (closes #1394) ([#1395](https://github.com/bobmatnyc/trusty-tools/pull/1395)) ([`864b006`](https://github.com/bobmatnyc/trusty-tools/commit/864b0062bfd0a3d913855c45fa9d246bca13634f))
- rename `tm coordinator-tui` → `tm session tui` + move coordinator API under /api/v1/sessions ([#1393](https://github.com/bobmatnyc/trusty-tools/pull/1393)) ([`749e7dd`](https://github.com/bobmatnyc/trusty-tools/commit/749e7dd4bad08ff8f93c04bb7c3d36991221f79b))

---

## [0.10.0] — 2026-06-17

### Fixed (closes #1373)

- **Sessions now register + pin their own project's trusty-search index.** At
  session launch `prepare_session` derives the project's canonical index id
  (git-root basename, via the shared `trusty_common::derive_index_id`),
  best-effort find-or-creates it in the running trusty-search daemon
  (`POST /indexes`), and injects the `trusty-search` MCP stub **pinned** to that
  id (`serve --index <id>`). A bare `search`/`grep` therefore resolves to the
  session's own project index instead of letting the LLM guess — which
  routinely picked the wrong (usually persistent `claude-mpm`) index. The
  daemon-unreachable case is graceful: it logs a warning and still pins the
  stub (the index is created on first reindex); an empty derived id falls back
  to the unpinned `serve` stub. Either way the session always launches.

## [0.9.0] — 2026-06-16

### Release

- **First monorepo publish.** This is the first `trusty-mpm` release published
  from the unified `trusty-tools` workspace. It supersedes the stale `0.8.1`
  on crates.io, which was published from the now-archived standalone repo.

### Fixed

- **Standalone build break:** `daemon/mcp_console.rs` imports
  `trusty_common::console_metrics` unconditionally, but that module is gated
  behind trusty-common's `console-metrics` feature. trusty-mpm's main
  `trusty-common` dependency now enables `console-metrics`, so
  `cargo check -p trusty-mpm` and `cargo publish` no longer fail to resolve
  the module. (Workspace feature-unification previously masked this under
  `cargo test`.)

## [0.8.2] — 2026-06-16

### Changed (closes part of #1318)

- **De-bundled `trusty-console`.** Removed the bundled `trusty-console`
  `[[bin]]` shim and dependency. `cargo install trusty-mpm` now produces
  `tm` and `trusty-mpm` only. Install the console with
  `cargo install trusty-console`. This is part of the single-owner-per-binary
  fix for the cargo binary-ownership collisions (#1262).

## [0.5.0] — 2026-05-28

### Added: `tm services` — canonical service-discovery CLI (issue #339)

**New subcommand**: `tm services <action>` — replaces ad-hoc `lsof`/`curl`/`ps`
patterns for discovering the port, health, and status of every trusty-* daemon.

#### Subcommands

| Command | Description |
|---------|-------------|
| `tm services list [--json]` | Table of all declared services with running/down status, port, version, and health |
| `tm services status <name> [--json]` | Detailed block for one service |
| `tm services port <name>` | Print just the port number (scriptable) |
| `tm services url <name>` | Print the full base URL |
| `tm services health <name>` | Probe the `/health` endpoint; exit 0 if healthy |
| `tm services log <name>` | Print the log file path if it exists |
| `tm services init [--force]` | Write the default manifest to `~/.claude-mpm/services.yaml` |
| `tm services restart <name>` | Execute the manifest `restart_cmd` |

#### Manifest

Default manifest embedded in the binary covers 6 services:

- `trusty-search` — port 7878, `/health` confirmed
- `trusty-analyze` — port 7879, `/health` confirmed
- `trusty-mpm-daemon` — port 7880, `/health` confirmed at `daemon/api.rs:74`
- `trusty-memory` — dynamic port (7070-7079) via `~/.trusty-memory/http_addr`
- `trusty-embedderd` — UDS sidecar, pgrep-only (no HTTP surface)
- `trusty-bm25-daemon` — UDS sidecar, pgrep-only (no HTTP surface)

Custom manifests can be placed at `~/.claude-mpm/services.yaml` (use `tm services init`).

#### Exit codes

| Code | Meaning |
|------|---------|
| 0 | Running/healthy (list always exits 0) |
| 1 | Service declared but down, or health probe failed |
| 2 | Service name not in manifest |

#### Scriptable usage

```bash
PORT=$(tm services port trusty-search)
URL=$(tm services url trusty-search)
tail -f $(tm services log trusty-search)
```

#### Architecture

- `crates/trusty-mpm/src/services/manifest.rs` — `ServicesManifest`, `ServiceDecl`,
  `PortDiscovery` enum, `ManifestValidationError` (thiserror)
- `crates/trusty-mpm/src/services/discoverer.rs` — `Discoverer` with 5-second
  TTL cache; `ProcessProber`/`PortProber`/`HttpProber`/`VersionRunner` trait
  seams for unit testing
- `crates/trusty-mpm/assets/default-services.yaml` — embedded default manifest

**Tests**: 21 new unit tests (8 manifest + 13 discoverer, all mocked) + 11 CLI
parse tests + 2 ignore-gated integration smoke tests.

---

## [consolidation] — 2026-05-26

**Combined 7 trusty-mpm-\* sub-crates into one crate with feature-gated `[[bin]]` targets.**

### Summary

The following sub-crates have been merged into this unified `trusty-mpm` crate:

| Former crate | Now lives in |
|---|---|
| `trusty-mpm-core` | `crates/trusty-mpm/src/core/` |
| `trusty-mpm-client` | `crates/trusty-mpm/src/client/` |
| `trusty-mpm-mcp` | `crates/trusty-mpm/src/mcp/` (feature: `mcp`) |
| `trusty-mpm-daemon` | `crates/trusty-mpm/src/daemon/` (feature: `daemon`) |
| `trusty-mpm-cli` | `crates/trusty-mpm/src/bin/tm.rs` (feature: `cli`) |
| `trusty-mpm-tui` | `crates/trusty-mpm/src/tui/` (feature: `tui`) |
| `trusty-mpm-telegram` | `crates/trusty-mpm/src/telegram/` (feature: `telegram`) |

The Tauri desktop GUI (`trusty-mpm-gui`) remains as a separate crate because
it owns `build.rs` (invoking `tauri_build::build()`) and `tauri.conf.json` — files
that cannot co-exist with a generic Cargo crate build system. The `gui` feature of
this crate wraps it as an optional path dependency.

### Workspace crate count
- Removed: 7 crates (`trusty-mpm-core`, `trusty-mpm-mcp`, `trusty-mpm-daemon`,
  `trusty-mpm-client`, `trusty-mpm-cli`, `trusty-mpm-tui`, `trusty-mpm-telegram`)
- Added: 1 crate (`trusty-mpm`)
- Net change: 28 → 22 workspace members

### Feature flags

| Feature | What it enables |
|---|---|
| `default` | `cli` + `daemon` (the common install path) |
| `cli` | `tm` / `trusty-mpm` CLI binary (implies `daemon`, `tui`, `telegram`) |
| `daemon` | `trusty-mpmd` daemon binary + daemon library module (implies `mcp`) |
| `mcp` | MCP server library module |
| `tui` | `trusty-mpm-tui` shim binary + TUI library module |
| `telegram` | `trusty-mpm-telegram` shim binary + Telegram library module |
| `gui` | `trusty-mpm-gui` shim binary (wraps the separate `trusty-mpm-gui` crate) |

### Public API surface

All public types, traits, and functions are preserved. The only change is the
import path: code that previously imported from `trusty_mpm_core`, `trusty_mpm_client`,
etc. should now import from the corresponding submodule of `trusty_mpm`:

```rust
// Before
use trusty_mpm_core::session::{Session, SessionId};
use trusty_mpm_client::DaemonClient;

// After
use trusty_mpm::core::session::{Session, SessionId};
use trusty_mpm::client::DaemonClient;
```

### Deprecation notes

The following crate names are no longer published:
- `trusty-mpm-core`
- `trusty-mpm-mcp`
- `trusty-mpm-daemon`
- `trusty-mpm-client`
- `trusty-mpm-cli`
- `trusty-mpm-tui`
- `trusty-mpm-telegram`

All functionality is available under `trusty-mpm` with the appropriate feature flags.

## [0.4.0] and prior

See the individual crate changelogs in the former sub-crate directories (available
in git history as `crates/trusty-mpm-{core,client,mcp,daemon,cli,tui,telegram}/`).

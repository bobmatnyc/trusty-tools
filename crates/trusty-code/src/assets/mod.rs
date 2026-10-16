//! Built-in default agents & skills for trusty-code (#2895), read from
//! instructional content at runtime since #9013.
//!
//! Why: A fresh project has no `.claude/agents/` or `.claude/skills/` yet, so
//! `tcode` would otherwise start with zero agents and zero skills — a cold,
//! unusable default. A built-in default set (parsed in-memory, without
//! trusty-mpm's disk-materialize install step) gives every project a usable
//! harness out of the box while disk-based `.claude/agents/` and
//! `.claude/skills/` always take precedence when present (see
//! `agents::load_all_agents` and `skills::discover_skill_metadata`'s
//! fallback branches). #9013 (ADR-0064): nothing here is compiled in any more.
//! These tables name the defaults; their texts live in the content tree —
//! tcode's own agents under `content/agents/trusty-code/`, its skill catalog
//! under `content/instructions/trusty-code/skills/` — and [`content`] reads
//! them through the shared resolver. No content means no defaults, under one
//! error naming `tm content update` / `tm content install --from`.
//! What: [`EmbeddedAgent`]/[`DEFAULT_AGENTS`] — the 34-agent dispatchable
//! roster (Slice E3, #2958, plus `pm` added for #3437, `ticketing` for
//! #4027, and the four delivery-workflow agents for #8129): tcode's own 8
//! `Direct` defaults (`engineer`, `qa-agent`, `code-reviewer`, `pm`,
//! `ticketing`, `version-control`, `local-ops`, `documentation` — no
//! `extends:` chain, projected via
//! `agents::md_loader::project_embedded_md`) plus the 26 tm agents (`extends:`-chained
//! through the 5 `BASE-*` templates — projected via
//! `agents::md_loader::project_embedded_md_with_extends`, which resolves
//! against the tm agent catalog). Authored as Markdown+frontmatter
//! (`.md`) as of #2897 Slice C (previously native TOML; Slice D subsequently
//! retired the TOML loader for USER `.claude/agents/*.toml` configs entirely
//! — see `agents::mod`'s docs). Both projection paths share one
//! frontmatter->`AgentConfig` mapping with the disk `.md` loader
//! (`agents::md_loader::load_md_agent`) — see that module's docs.
//! [`DEFAULT_SKILL_NAMES`] — tcode's copy of trusty-mpm's universal skill set
//! (format-identical `SKILL.md` files), reused per Bob's reuse directive; the
//! `tm-*` orchestration skills are excluded because they drive trusty-mpm MCP
//! tools tcode does not have. The copies have drifted from `content/skills/`;
//! #9013 moved them as they were.
//!
//! ## Tools-restriction deviation (Slice E3, #2958, Bob's 2026-07-18 ruling)
//!
//! Four of the 26 roster agents — `qa`, `code-critic`, `code-analyzer`,
//! `web-qa` — carry an explicit restrictive `tools:` override in their tcode
//! FORK (`content/agents/trusty-code/{qa,code-critic,code-analyzer,web-qa}.md`) that is NOT
//! present in the shared agent they were derived from (`content/agents`,
//! read at runtime since #9011). These four are the only agent `.md` files
//! trusty-code keeps a second copy of: every other roster agent is read
//! from instructional content through the tm agent catalog, so it cannot
//! drift. (#8129's four
//! delivery-workflow agents also live in `content/agents/trusty-code/`, but they are
//! tcode-AUTHORED, not copies — see the "Delivery-workflow agents" section
//! below.) This is a
//! deliberate, Bob-approved deviation, not drift: those four are
//! reviewer-intent agents and get
//! the same read-only tool allowlist tcode's own `code-reviewer` default uses
//! (no `write_file`/`edit`/`bash`). `research` stays byte-identical and
//! unrestricted per Bob's explicit ruling — it builds research reports, not
//! verdicts. `documentation` was covered by that same ruling until #8129,
//! which replaced the shared body with a tcode-native one carrying its own
//! `tcode_tools:` grant; see the "Delivery-workflow agents" section below for
//! its real status — that grant is strictly WIDER than the reviewer-intent
//! four get, so it is not a reversal of the ruling. NOTE for the file itself: the
//! shared frontmatter parser
//! (`trusty_agents_common::agents::builder::split_frontmatter`) does NOT
//! tolerate `#`-comment lines inside a frontmatter block (verified
//! empirically — a bare `#`-prefixed line there is a hard `FrontmatterParse`
//! error, not a tolerated comment), so no inline marker could be added to
//! those four files without breaking the compose step; this doc comment is
//! the marker of record. The deferred E4 CI staleness guard (diffing tcode's
//! copies against trusty-mpm's source) MUST whitelist the `tools:` line in
//! exactly these four files rather than flagging them as drift.
//!
//! ## Prose deviation follow-up (Slice E3 review round, #3041)
//!
//! Adding the `tools:` override alone left three of the four files
//! internally contradictory: their prose still instructed write/bash actions
//! the new allowlist denies (`web-qa.md`'s "Technical Testing Protocol" told
//! the agent to create test scripts, run `CI=true npm test`, and shell out to
//! `ps aux`; `qa.md`'s methodology told it to "Implement"/"Execute" test
//! suites directly; `code-analyzer.md`'s "Large-Volume Analysis" told it to
//! generate-and-run a Python script). A code-critic review of PR #3041 (WARN,
//! two HIGH + one MEDIUM findings) caught this. `web-qa.md`, `qa.md`, and
//! `code-analyzer.md` were reworded in the SAME PR to a genuinely read-only
//! frame: findings plus concrete, ready-to-run recommendations (drafted
//! scenarios, specified test cases, recommended commands) handed off to an
//! engineer/ops/CI to execute, never executed by the agent itself.
//! `code-critic.md` needed no prose change at the time — its review-only body
//! had no execute-oriented instructions. This is an ADDITIONAL deviation from
//! trusty-mpm byte-parity beyond the `tools:` line alone; the deferred E4
//! staleness guard must whitelist the reworded prose sections in these three
//! files too, not just the `tools:` line.
//!
//! `code-critic.md` gained its own reworded section later (owner ruling
//! 2026-08-12, PR #5596): trusty-mpm's upstream copy started instructing the
//! agent to post its verdict via `gh pr review --comment` directly. tcode's
//! copy has no `bash`/`gh` tool, so the "Posting the Verdict" section was
//! reworded to the same read-only frame as the three files above — it
//! specifies the exact command for the caller to run, never runs it itself.
//! `code-critic.md` now joins `web-qa.md`/`qa.md`/`code-analyzer.md` as a
//! four-of-four reworded-prose deviation, not three-of-four; the E4 staleness
//! guard whitelists it the same way (pinned-hash reconciliation, not
//! byte-parity).
//!
//! ## Non-coding cross-product roster addition (#4027, epic #4021)
//!
//! `ticketing` joined this roster (same treatment `research` got) so
//! trusty-agents' widened `dispatch_task` bridge (#4026) can reach ONE roster
//! instead of growing a second dispatch leg into trusty-mpm — the owner's OQ-4
//! ruling. It was embedded directly from the shared crate until #8129, which
//! replaced the body with a tcode-native one declaring its own `tcode_tools:`;
//! the dispatch NAME the bridge resolves is unchanged, which is all #4026
//! depends on. Its non-coding property is enforced where the
//! owner's OQ-7 ruling put enforcement — the BRIDGE's fail-closed
//! `NON_CODING_TARGETS` floor in
//! `crates/trusty-agents/src/tools/cross_product.rs` — not by an asset-level
//! allowlist that a direct `tcode run-task` invocation would bypass anyway.
//!
//! ## Delivery-workflow agents (#8129, epic #8127)
//!
//! `ticketing`, `version-control`, `local-ops` and `documentation` are the
//! four roles trusty-mpm's PM delegates the issue -> branch -> build -> PR
//! -> changelog steps to. tcode's PM could not reach any of them: three
//! (`ticketing`, `local-ops`, `documentation`) were `Composed` entries
//! sourced byte-identically from the shared crate, and `version-control` was
//! absent from the roster entirely. The shared bodies are written for
//! trusty-mpm's harness (`Skill(...)`, `tm` CLI verbs, MCP servers tcode does
//! not host) and their `tools:` frontmatter names Claude Code's vocabulary,
//! which this runtime ignores (#7683) — so each one projected to
//! `tools.allowed == None`, i.e. EVERY tool allowed, the opposite of the
//! grant its author wrote.
//!
//! All four are therefore tcode-NATIVE [`EmbeddedAgent::Direct`] agents in
//! `content/agents/trusty-code/`, authored the way `pm.md`/`engineer.md` are: a short body
//! scoped to tcode's actual tool surface, with an explicit `tcode_tools:`
//! allowlist that carries `bash` (for `git`/`gh`/`cargo`) and `finish_task`.
//! They are NOT forks of the same-named shared assets and are NOT pinned
//! deviations — there is no upstream content to reconcile against, only an
//! upstream ROLE. `scripts/check_agent_assets.sh` therefore lists them in
//! `$TCODE_ONLY`, never in `$DEVIATED_FILES`. The three shared sources they
//! replace are absent from [`SHARED_TM_AGENT_FILES`], so no caller can compose
//! the trusty-mpm body under the same dispatch name.
//!
//! ## E4 guard (issue #2958, `scripts/check_agent_assets.sh`)
//!
//! The byte-parity half of this gate is gone because what it compared is gone:
//! 30 duplicated `.md` files became one shared copy each (`content/agents`
//! since #9011), and a file cannot drift from itself. What the gate still checks is what the compiler cannot
//! see — it pins the 4 deviated files' SHARED source hash
//! (`scripts/agent-asset-pins.tsv`) so an edit behind one of them fails for
//! deliberate reconciliation, and it rejects any `.md` appearing in this
//! directory that is neither a pinned deviation nor a declared tcode-only
//! default, which is how a deleted duplicate stays deleted. See that script's
//! header and `.github/workflows/agent-assets.yml`.
//!
//! Test: `assets::tests::*` — every embedded agent `.md` parses and projects
//! to a field-identical `AgentConfig` vs. the retired TOML fixtures (the
//! original 3), every roster name resolves through the extends composer with
//! no BASE template dispatchable and no name collision, the four restricted
//! agents' composed `AgentConfig` carries the read-only tool list, and
//! `documentation`/`research` remain unrestricted. Every skill name is unique
//! and every skill's frontmatter `name:` matches its table key.
//!
//! [`EmbeddedAgent`]: crate::assets::EmbeddedAgent
//! [`EmbeddedAgent::Direct`]: crate::assets::EmbeddedAgent::Direct
//! [`DEFAULT_AGENTS`]: crate::assets::DEFAULT_AGENTS
//! [`SHARED_TM_AGENT_FILES`]: crate::assets::SHARED_TM_AGENT_FILES
//! [`content`]: crate::assets::content
//! [`DEFAULT_SKILL_NAMES`]: crate::assets::DEFAULT_SKILL_NAMES

/// One built-in default agent: either self-contained (tcode's own 8
/// defaults, no `extends:` chain) or a roster agent whose `extends:` chain is
/// resolved at load time against the tm agent catalog (Slice E3, #2958).
///
/// Why: `agents::mod`'s embedded-fallback needs a single ordered table
/// ([`DEFAULT_AGENTS`]) covering both projection strategies tcode's embedded
/// agents need: tcode's own 3 defaults are flat `.md` strings with no
/// inheritance (projected via `agents::md_loader::project_embedded_md`); the
/// 28 tm roster agents inherit from `BASE-*` templates and must be composed
/// in-memory (`agents::md_loader::project_embedded_md_with_extends`) before
/// projection. One enum keeps both variants in the same ordered slice rather
/// than two parallel tables that could silently drift out of sync in count
/// or ordering.
/// What: both variants carry only the dispatch name (#9013). A
/// [`EmbeddedAgent::Direct`] body is `content/agents/trusty-code/<name>.md`,
/// read by [`content::AgentSources`]; a [`EmbeddedAgent::Composed`] body is
/// composed from the catalog [`content::AgentSources::catalog`] holds.
/// [`EmbeddedAgent::name`] reads the name back out of either variant.
/// Test: `assets::tests::default_agents_parse_and_names_match`.
#[derive(Debug, Clone, Copy)]
pub enum EmbeddedAgent {
    /// A self-contained agent, no `extends:` chain; its `.md` is read from
    /// content (#9013) and projected via `agents::md_loader::project_embedded_md`.
    Direct {
        /// Dispatch key, matching the frontmatter `name:` inside the `.md`
        /// and its file stem.
        name: &'static str,
    },
    /// A roster agent resolved via `agents::md_loader::project_embedded_md_with_extends`
    /// against the tm agent catalog at load time.
    Composed {
        /// Dispatch key, and the lookup key into the catalog (after its own
        /// case-folding).
        name: &'static str,
    },
}

impl EmbeddedAgent {
    /// The dispatch key, regardless of variant.
    ///
    /// Why: callers (the embedded-fallback loader, tests) usually only need
    /// the name and would otherwise have to `match` on the variant just to
    /// read one shared field.
    /// What: returns `name` from either `Direct` or `Composed`.
    /// Test: `assets::tests::default_agents_parse_and_names_match`.
    pub fn name(&self) -> &'static str {
        match self {
            EmbeddedAgent::Direct { name } => name,
            EmbeddedAgent::Composed { name } => name,
        }
    }
}

/// Opening marker of `pm.md`'s contiguous agent-routing block (#8287).
///
/// Why: DOC-75 §4b row 3 ([#8293](https://github.com/bobmatnyc/trusty-tools/issues/8293))
/// will lift the routing table out of this crate into one shared source neither
/// product crate owns, behind a drift check. A marked, contiguous block is what
/// makes that a move rather than a rewrite, and it is also the span the routing
/// tests parse — so the lift boundary and the test boundary can never disagree.
/// What: the exact Markdown-comment line opening the block. Matched literally,
/// with no trailing prose on the marker line, so [`pm_routing_block`] needs no
/// line-scanning heuristic.
/// Test: `assets::tests::pm_routing_block_names_only_delegable_roster_agents`.
pub const PM_ROUTING_BLOCK_BEGIN: &str = "<!-- pm-routing-table:begin -->";

/// Closing marker of `pm.md`'s routing block.
///
/// Why/What/Test: as [`PM_ROUTING_BLOCK_BEGIN`].
pub const PM_ROUTING_BLOCK_END: &str = "<!-- pm-routing-table:end -->";

/// The delegation targets a delegate-mode coding task passes through, in order
/// (#8287).
///
/// Why: DOC-75 §1's delegate run is "research, then engineer, then qa" — the
/// ORDER is the behaviour, not just the membership, so it is declared here
/// rather than re-read from the card's prose by each test. `qa-agent` and not
/// `qa` because DOC-75 §6 requires real test output in the transcript and
/// tcode's `qa` fork carries no `bash` (see this module's "Tools-restriction
/// deviation" section) — it recommends commands it cannot run.
/// What: three roster names, each an [`EmbeddedAgent`] entry in
/// [`DEFAULT_AGENTS`], in the order the card's routing block must name them.
/// Test: `assets::tests::pm_routing_block_names_only_delegable_roster_agents`.
pub const PM_ROUTING_ORDER: &[&str] = &["research", "engineer", "qa-agent"];

/// `pm.md`'s size before #8287 added the routing block, in bytes.
///
/// Why: DOC-75 §4b caps tcode's resident PM prompt at 2x its 2026-09-19 size —
/// token budget is a first-class axis for tcode. A hardcoded baseline is what
/// lets a test enforce that cap; deriving it from `PM_MD.len()` would make the
/// assertion vacuous.
/// What: `wc -c` of `crates/trusty-code/src/assets/agents/pm.md` at commit
/// `3473119`, the tip of `main` when #8287 landed.
/// Test: `assets::tests::pm_card_stays_within_the_doc_75_size_cap`.
pub const PM_CARD_BASELINE_BYTES: usize = 2863;

/// The routing block's inner text, or `None` when the markers are absent.
///
/// Why: two callers need the same span — the routing tests, and
/// [#8293](https://github.com/bobmatnyc/trusty-tools/issues/8293)'s shared-source
/// lift, which has to read the block out of whichever card it is migrating.
/// Returning `None` rather than the whole prompt is deliberate: a card that
/// lost its markers must fail the routing test, not silently pass by matching
/// agent names elsewhere in the body.
/// What: the text between [`PM_ROUTING_BLOCK_BEGIN`] and
/// [`PM_ROUTING_BLOCK_END`], exclusive of both markers, trimmed. `None` when
/// either marker is missing or they appear out of order.
/// Test: `assets::tests::pm_routing_block_names_only_delegable_roster_agents`,
/// `prompt::tests::delegate_mode_prompt_carries_the_routing_table`.
pub fn pm_routing_block(card: &str) -> Option<&str> {
    let after_begin = card.split_once(PM_ROUTING_BLOCK_BEGIN)?.1;
    Some(after_begin.split_once(PM_ROUTING_BLOCK_END)?.0.trim())
}

/// The 34-agent dispatchable default roster, its texts read from content (#9013)
/// (Slice E3, #2958, plus `pm` added for #3437, `ticketing` for #4027, and
/// the four #8129 delivery-workflow agents):
/// tcode's 8 own `Direct` defaults plus the 26 tm roster agents. The 5
/// `BASE-*` extends templates in the tm agent catalog are
/// deliberately NOT entries here — they are extends-sources only, never
/// dispatchable — and trusty-mpm's own `engineer` agent is excluded from the
/// roster upstream (#2958's roster decision) precisely because it would
/// collide with tcode's `engineer` below;
/// `assets::tests::no_name_collisions_across_the_33_agent_roster` pins that
/// no collision exists in the final table.
///
/// Why: gives `agents::load_all_agents`'s embedded-fallback branch a fixed,
/// ordered table to parse when the disk `.claude/agents/` directory is empty
/// or absent. `pm` specifically fixes #3437: `task::protocol::task_run`
/// defaults an omitted `agent_name` to the literal `"pm"`
/// (`task/protocol.rs`), which resolved against NEITHER a disk
/// `~/.claude/agents/pm.md` NOR this table before #3437 — so every
/// daemon-default (including every GUI-initiated, agent-name-omitting) run
/// failed agent resolution before a single turn executed.
/// What: `engineer` (general implementation, full read/write tool set),
/// `qa-agent` (verification — read/inspect/run only, no `write_file`/`edit`,
/// hands bugs back to the engineer rather than fixing them), `code-reviewer`
/// (adversarial, read-only review, no `bash`), `pm` (orchestrator/default —
/// delegates when `delegate_to_agent` is available, executes directly
/// otherwise; see `content/agents/trusty-code/pm.md`), plus the four #8129
/// delivery-workflow agents `ticketing` (issues via `gh`), `version-control`
/// (branch/commit/push/PR via `git`+`gh`), `local-ops` (build, test, version
/// bump, changelog) and `documentation` (prose) — all eight
/// [`EmbeddedAgent::Direct`], interleaved alphabetically from
/// `documentation` onward. Then the 26 [`EmbeddedAgent::Composed`] roster
/// agents, alphabetical, matching the tm agent catalog's roster
/// ordering. Four of them (`qa`, `code-critic`, `code-analyzer`, `web-qa`)
/// carry a tcode-only restrictive `tcode_tools:` override in their `.md`
/// source — see this module's "Tools-restriction deviation" doc section above.
/// Test: `assets::tests::default_agents_parse_and_names_match`,
/// `assets::tests::base_templates_are_never_dispatchable`,
/// `assets::tests::no_name_collisions_across_the_34_agent_roster`,
/// `assets::tests::ticketing_is_dispatchable_for_cross_product_delegation`,
/// `assets::tests::restricted_reviewer_agents_carry_read_only_tools`,
/// `assets::tests::delivery_workflow_agents_are_dispatchable_with_bash`,
/// `assets::tests::research_remains_unrestricted`,
/// `assets::tests::default_task_run_agent_resolves_against_default_agents`,
/// `assets::tests::every_embedded_agent_model_normalizes_to_a_valid_slug`.
pub const DEFAULT_AGENTS: &[EmbeddedAgent] = &[
    EmbeddedAgent::Direct { name: "engineer" },
    EmbeddedAgent::Direct { name: "qa-agent" },
    EmbeddedAgent::Direct {
        name: "code-reviewer",
    },
    // #8184: `pm` is the default agent of an INTERACTIVE session, which runs
    // SOLO since #8184 — it edits and runs commands itself, in the user's real
    // project root, instead of delegating. Its card is therefore the only
    // bundled one carrying a `permissions:` block: every mutating tool asks
    // first (#3422), reads stay unprompted. A headless run has nobody to ask,
    // so an `ask` there denies (#8100) unless TCODE_PERMISSION_MODE=allow-asks.
    // #8287: in DELEGATE mode the same card is the only routing text the PM has,
    // so its contiguous routing block (see [`PM_ROUTING_BLOCK_BEGIN`]) is what
    // decides whether `research`/`engineer`/`qa-agent` get dispatched at all.
    EmbeddedAgent::Direct { name: "pm" },
    EmbeddedAgent::Composed { name: "api-qa" },
    EmbeddedAgent::Composed {
        name: "code-analyzer",
    },
    EmbeddedAgent::Composed {
        name: "code-critic",
    },
    EmbeddedAgent::Composed {
        name: "dart-engineer",
    },
    EmbeddedAgent::Composed {
        name: "data-engineer",
    },
    // #8129: tcode-native, not the shared roster's `documentation.md`.
    EmbeddedAgent::Direct {
        name: "documentation",
    },
    EmbeddedAgent::Composed {
        name: "golang-engineer",
    },
    EmbeddedAgent::Composed {
        name: "java-engineer",
    },
    EmbeddedAgent::Composed {
        name: "javascript-engineer",
    },
    // #8129: tcode-native build/test/version-bump/changelog agent.
    EmbeddedAgent::Direct { name: "local-ops" },
    EmbeddedAgent::Composed {
        name: "nextjs-engineer",
    },
    EmbeddedAgent::Composed {
        name: "elixir-engineer",
    },
    EmbeddedAgent::Composed {
        name: "phoenix-engineer",
    },
    EmbeddedAgent::Composed {
        name: "php-engineer",
    },
    EmbeddedAgent::Composed {
        name: "prompt-engineer",
    },
    EmbeddedAgent::Composed {
        name: "python-engineer",
    },
    EmbeddedAgent::Composed { name: "qa" },
    EmbeddedAgent::Composed {
        name: "react-engineer",
    },
    EmbeddedAgent::Composed {
        name: "refactoring-engineer",
    },
    EmbeddedAgent::Composed { name: "research" },
    EmbeddedAgent::Composed {
        name: "ruby-engineer",
    },
    EmbeddedAgent::Composed {
        name: "rust-engineer",
    },
    EmbeddedAgent::Composed { name: "security" },
    EmbeddedAgent::Composed {
        name: "svelte-engineer",
    },
    EmbeddedAgent::Composed {
        name: "tauri-engineer",
    },
    // #4027: non-coding ticketing specialist, reachable from trusty-agents'
    // widened cross-product bridge (#4026). See this module's "Non-coding
    // cross-product roster addition" doc section. #8129 replaced the shared
    // copy with a tcode-native one carrying a `tcode_tools:` allowlist.
    EmbeddedAgent::Direct { name: "ticketing" },
    EmbeddedAgent::Composed {
        name: "typescript-engineer",
    },
    // #8129: the branch/commit/push/PR half of the delivery workflow.
    EmbeddedAgent::Direct {
        name: "version-control",
    },
    EmbeddedAgent::Composed { name: "web-qa" },
    EmbeddedAgent::Composed {
        name: "web-ui-engineer",
    },
];

// -- Slice E2 (#2958): embedded tm agent catalog, for `md_loader`'s in-memory
// extends-composer. Slice E3 wires the 26 roster names above into
// `DEFAULT_AGENTS` as `EmbeddedAgent::Composed` entries; this table remains
// their content source, resolved at load time via
// `agents::md_loader::project_embedded_md_with_extends`. --

/// The 5 `BASE-*` extends-template names — composition bases only, never
/// meant to be dispatched directly (issue #3465 follow-up: the Agents
/// catalog listing was surfacing these to end users).
///
/// Why: single source of truth for "is this name a composition base"
/// (issue #3449's `agents.list`) instead of a scattered `starts_with`
/// check re-implemented per call site — `assets::tests::
/// base_templates_are_never_dispatchable` (this table never leaks into
/// [`DEFAULT_AGENTS`]) and `agents::protocol::is_base_agent` (the disk-tier
/// catalog listing never surfaces one of these names even when a project's
/// `.claude/agents/` dir has real `BASE-*.md` files on disk, as trusty-mpm's
/// own bundle installs them — see `crates/trusty-mpm/src/core/bundle_all.rs`)
/// both key off this list rather than duplicating it. Matched
/// case-insensitively against a lowercased candidate — the on-disk filename
/// convention is `BASE-QA.md` while frontmatter `name:`/`extends:` values
/// are lowercase `base-qa`.
/// Test: `assets::tests::base_templates_are_never_dispatchable`,
/// `agents::protocol::tests::list_excludes_base_agents_but_resolve_agent_still_finds_them`.
pub const BASE_AGENT_NAMES: &[&str] = &[
    "base-agent",
    "base-engineer",
    "base-ops",
    "base-qa",
    "base-research",
];

/// The 27 shared agent files tcode composes from: the 5 `BASE-*` extends
/// templates plus 22 roster agents (#9011).
///
/// Why: these texts are instructional content (ADR-0064), read at runtime
/// from the same source trusty-mpm reads — a trusty-tools checkout, else the
/// bundle `tm content install` pinned — instead of being compiled in.
/// Keyed by each file's name in `agents/` (`"BASE-QA.md"`,
/// `"rust-engineer.md"`), which
/// `trusty_agents_common::agents::builder_in_memory::InMemorySources::insert`
/// lowercases and strips, so `extends: base-qa` resolves with no extra work.
/// What: file names only; [`content::catalog_from`] reads them from a roster.
/// Test: `assets::tests::embedded_tm_agent_sources_has_31_entries_and_unique_keys`.
pub const SHARED_TM_AGENT_FILES: &[&str] = &[
    "BASE-AGENT.md",
    "BASE-ENGINEER.md",
    "BASE-OPS.md",
    "BASE-QA.md",
    "BASE-RESEARCH.md",
    "api-qa.md",
    "dart-engineer.md",
    "data-engineer.md",
    "golang-engineer.md",
    "java-engineer.md",
    "javascript-engineer.md",
    "nextjs-engineer.md",
    "elixir-engineer.md",
    "phoenix-engineer.md",
    "php-engineer.md",
    "prompt-engineer.md",
    "python-engineer.md",
    "react-engineer.md",
    "refactoring-engineer.md",
    "research.md",
    "ruby-engineer.md",
    "rust-engineer.md",
    "security.md",
    "svelte-engineer.md",
    "tauri-engineer.md",
    "typescript-engineer.md",
    "web-ui-engineer.md",
];

/// tcode's 4 read-only forks of shared roster agents, read from
/// `content/agents/trusty-code/` (#9013) — see this module's
/// "Tools-restriction deviation" section. Each shadows the shared file of the
/// same name in the catalog.
pub const LOCAL_TM_AGENT_FORKS: &[&str] =
    &["code-analyzer.md", "code-critic.md", "qa.md", "web-qa.md"];

/// [`content::AgentSources::catalog`] over the content resolved for this
/// process: the trusty-tools checkout above the cwd, else the installed
/// content bundle. Every failure names `tm content update` or
/// `tm content install --from`.
pub fn load_tm_agent_sources()
-> Result<Vec<(String, String)>, trusty_agents_common::agent_content::AgentContentError> {
    content::catalog_from(&content::resolve_for_process()?)
}

/// tcode's default skill catalog, by name (#9013: the texts live in
/// `content/instructions/trusty-code/skills/<name>/SKILL.md`).
///
/// Why: gives `skills::discover_skill_metadata`'s fallback branch a fixed,
/// ordered list of the skills content must supply when the disk
/// `.claude/skills/` directory is empty or absent; a listed skill the content
/// lacks is an error, never a shorter catalog.
/// What: 28 names, sorted; [`content::default_skills_from`] reads them.
/// Test: `assets::tests::default_skills_names_are_unique`.
pub const DEFAULT_SKILL_NAMES: &[&str] = &[
    "api-design-patterns",
    "api-documentation",
    "artifacts-builder",
    "brainstorming",
    "code-production-process",
    "code-review-standards",
    "condition-based-waiting",
    "contract-driven-testing",
    "database-migration",
    "documentation-style",
    "env-manager",
    "git-workflow",
    "internal-comms",
    "json-data-handling",
    "model-context-builder",
    "requesting-code-review",
    "root-cause-tracing",
    "security-scanning",
    "software-patterns",
    "systematic-debugging",
    "test-driven-development",
    "test-quality-inspector",
    "testing-anti-patterns",
    "verification-before-completion",
    "web-performance-optimization",
    "webapp-testing",
    "writing-plans",
    "xlsx",
];

pub mod content;

// -- Tests --------------------------------------------------------------------
// Split into `tests.rs` (not inlined) to keep this include-table file thin;
// see `tests.rs` module docs.

#[cfg(test)]
mod tests;

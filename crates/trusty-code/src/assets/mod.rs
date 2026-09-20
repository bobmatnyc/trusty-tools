//! Embedded default agents & skills for trusty-code (#2895).
//!
//! Why: A fresh project has no `.claude/agents/` or `.claude/skills/` yet, so
//! `tcode` would otherwise start with zero agents and zero skills — a cold,
//! unusable default. Bundling a working default set at compile time (mirroring
//! the embed pattern `trusty-mpm` uses in `crates/trusty-mpm/src/core/bundle.rs`,
//! but without trusty-mpm's disk-materialize install step — these are parsed
//! in-memory only) gives every project a usable harness out of the box while
//! disk-based `.claude/agents/` and `.claude/skills/` always take precedence
//! when present (see `agents::load_all_agents` and
//! `skills::discover_skill_metadata`'s embedded-fallback branches).
//! What: [`EmbeddedAgent`]/[`DEFAULT_AGENTS`] — the 34-agent dispatchable
//! roster (Slice E3, #2958, plus `pm` added for #3437, `ticketing` for
//! #4027, and the four delivery-workflow agents for #8129): tcode's own 8
//! `Direct` defaults (`engineer`, `qa-agent`, `code-reviewer`, `pm`,
//! `ticketing`, `version-control`, `local-ops`, `documentation` — no
//! `extends:` chain, projected via
//! `agents::md_loader::project_embedded_md`) plus the 26 tm agents (`extends:`-chained
//! through the 5 `BASE-*` templates — projected via
//! `agents::md_loader::project_embedded_md_with_extends`, which resolves
//! against [`EMBEDDED_TM_AGENT_SOURCES`]). Authored as Markdown+frontmatter
//! (`.md`) as of #2897 Slice C (previously native TOML; Slice D subsequently
//! retired the TOML loader for USER `.claude/agents/*.toml` configs entirely
//! — see `agents::mod`'s docs). Both projection paths share one
//! frontmatter->`AgentConfig` mapping with the disk `.md` loader
//! (`agents::md_loader::load_md_agent`) — see that module's docs.
//! [`EmbeddedSkill`]/[`DEFAULT_SKILLS`] — trusty-mpm's universal skill set
//! (format-identical `SKILL.md` files), reused verbatim per Bob's reuse
//! directive; the `tm-*` orchestration skills are excluded because they drive
//! trusty-mpm MCP tools tcode does not have.
//!
//! ## Tools-restriction deviation (Slice E3, #2958, Bob's 2026-07-18 ruling)
//!
//! Four of the 26 roster agents — `qa`, `code-critic`, `code-analyzer`,
//! `web-qa` — carry an explicit restrictive `tools:` override in their tcode
//! FORK (`assets/agents/{qa,code-critic,code-analyzer,web-qa}.md`) that is NOT
//! present in the shared asset they were derived from
//! (`trusty_agents_common::agent_assets`). These four are the only agent `.md`
//! files trusty-code still keeps a second copy of: every other roster agent is
//! embedded straight from the shared crate, so it cannot drift. (#8129's four
//! delivery-workflow agents also live in `assets/agents/`, but they are
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
//! `assets/agents/`, authored the way `pm.md`/`engineer.md` are: a short body
//! scoped to tcode's actual tool surface, with an explicit `tcode_tools:`
//! allowlist that carries `bash` (for `git`/`gh`/`cargo`) and `finish_task`.
//! They are NOT forks of the same-named shared assets and are NOT pinned
//! deviations — there is no upstream content to reconcile against, only an
//! upstream ROLE. `scripts/check_agent_assets.sh` therefore lists them in
//! `$TCODE_ONLY`, never in `$DEVIATED_FILES`. The three shared sources they
//! replace were removed from [`EMBEDDED_TM_AGENT_SOURCES`] in the same change
//! so no caller can compose the trusty-mpm body under the same dispatch name.
//!
//! ## E4 guard (issue #2958, `scripts/check_agent_assets.sh`)
//!
//! The byte-parity half of this gate is gone because what it compared is gone:
//! 30 duplicated `.md` files became one shared copy each, and a file cannot
//! drift from itself. What the gate still checks is what the compiler cannot
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
//! [`EMBEDDED_TM_AGENT_SOURCES`]: crate::assets::EMBEDDED_TM_AGENT_SOURCES
//! [`EmbeddedSkill`]: crate::assets::EmbeddedSkill
//! [`DEFAULT_SKILLS`]: crate::assets::DEFAULT_SKILLS

/// One embedded default agent: either self-contained (tcode's own 3
/// defaults, no `extends:` chain) or a roster agent whose `extends:` chain is
/// resolved at load time against [`EMBEDDED_TM_AGENT_SOURCES`] (Slice E3,
/// #2958).
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
/// What: [`EmbeddedAgent::Direct`] carries the dispatch name and the raw
/// `.md` source (frontmatter fence + prose body) verbatim. [`EmbeddedAgent::Composed`]
/// carries only the dispatch name — its content is resolved from
/// [`EMBEDDED_TM_AGENT_SOURCES`] at load time, since the whole point of the
/// in-memory composer is that no second copy of the raw bytes is needed here.
/// [`EmbeddedAgent::name`] reads the name back out of either variant.
/// Test: `assets::tests::default_agents_parse_and_names_match`.
#[derive(Debug, Clone, Copy)]
pub enum EmbeddedAgent {
    /// A self-contained agent: raw `.md` source, no `extends:` chain.
    Direct {
        /// Dispatch key, matching the frontmatter `name:` inside `md`.
        name: &'static str,
        /// Raw `.md` source (frontmatter fence + prose body), projectable
        /// via `agents::md_loader::project_embedded_md`.
        md: &'static str,
    },
    /// A roster agent resolved via `agents::md_loader::project_embedded_md_with_extends`
    /// against [`EMBEDDED_TM_AGENT_SOURCES`] at load time.
    Composed {
        /// Dispatch key, and the lookup key into `EMBEDDED_TM_AGENT_SOURCES`
        /// (after that table's own case-folding).
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
            EmbeddedAgent::Direct { name, .. } => name,
            EmbeddedAgent::Composed { name } => name,
        }
    }
}

/// One embedded default skill: its catalog name and raw `SKILL.md` source.
///
/// Why: `skills::mod`'s embedded-fallback needs both the name (for the
/// `SkillMetadata` catalog) and the raw Markdown (frontmatter + body, parsed
/// the same way a disk-based `SKILL.md` is).
/// What: `name` matches the frontmatter `name:` inside `skill_md`; `skill_md`
/// is the verbatim embedded file contents (frontmatter fence + body).
/// Test: `assets::tests::default_skills_names_are_unique`.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddedSkill {
    /// Catalog name, matching the frontmatter `name:` inside `skill_md`.
    pub name: &'static str,
    /// Raw `SKILL.md` source (frontmatter fence + Markdown body).
    pub skill_md: &'static str,
}

const ENGINEER_MD: &str = include_str!("agents/engineer.md");
const QA_AGENT_MD: &str = include_str!("agents/qa-agent.md");
const CODE_REVIEWER_MD: &str = include_str!("agents/code-reviewer.md");
/// The PM card AS AUTHORED — its routing table and pipeline chain are still
/// placeholders (#8293). [`pm_card`] is the card every consumer reads.
const PM_CARD_TEMPLATE: &str = include_str!("agents/pm.md");
// #8129: the four delivery-workflow agents. tcode-NATIVE, not forks of the
// shared roster's same-named files — see this module's "Delivery-workflow
// agents" doc section.
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
/// ORDER is the behaviour, not just the membership, so it is declared once
/// rather than re-read from the card's prose by each test. `qa-agent` and not
/// `qa` because DOC-75 §6 requires real test output in the transcript and
/// tcode's `qa` fork carries no `bash` (see this module's "Tools-restriction
/// deviation" section) — it recommends commands it cannot run.
/// What: #8293 moved the declaration itself into the shared routing rows, which
/// render the same three names into the card's pipeline sentence; this is a
/// re-export, so a reordering there cannot leave tcode's tests asserting the old
/// order. Each name is an [`EmbeddedAgent`] entry in [`DEFAULT_AGENTS`].
/// Test: `assets::tests::pm_routing_block_names_only_delegable_roster_agents`.
pub const PM_ROUTING_ORDER: &[&str] = trusty_agents_common::pm_routing::TCODE_PIPELINE;

/// `pm.md`'s size before #8287 added the routing block, in bytes.
///
/// Why: DOC-75 §4b caps tcode's resident PM prompt at 2x its 2026-09-19 size —
/// token budget is a first-class axis for tcode. A hardcoded baseline is what
/// lets a test enforce that cap; deriving it from the card's own length would
/// make the assertion vacuous.
/// What: `wc -c` of `crates/trusty-code/src/assets/agents/pm.md` at commit
/// `3473119`, the tip of `main` when #8287 landed.
/// Test: `assets::tests::pm_card_stays_within_the_doc_75_size_cap`.
pub const PM_CARD_BASELINE_BYTES: usize = 2863;

/// The PM card as every consumer reads it: the authored template with the
/// shared routing table and pipeline chain rendered in (#8293).
///
/// Why: the routing rows are owned by `trusty_agents_common::pm_routing`, which
/// trusty-mpm's PM instructions render the same rows from. Filling once, here,
/// puts the generated text inside [`DEFAULT_AGENTS`] itself, so every path that
/// already reads the card — `agents::resolve_agent`, `agents::load_all_agents`,
/// `agents::deploy` — gets the rendered card with no call-site change and no
/// second place to forget.
/// What: [`PM_CARD_TEMPLATE`] with
/// `trusty_agents_common::pm_routing::fill` applied for
/// [`trusty_agents_common::pm_routing::Consumer::Tcode`], computed once and
/// leaked to keep the `&'static str` shape [`EmbeddedAgent::Direct`] requires.
/// Test: `assets::tests::pm_card_routing_block_is_rendered_from_the_shared_rows`.
pub fn pm_card() -> &'static str {
    static FILLED: std::sync::LazyLock<&'static str> = std::sync::LazyLock::new(|| {
        let rendered = trusty_agents_common::pm_routing::fill(
            PM_CARD_TEMPLATE,
            trusty_agents_common::pm_routing::Consumer::Tcode,
        );
        // Leaked deliberately: one ~4 KB allocation for the process lifetime,
        // in exchange for the `&'static str` the embedded roster is built from.
        Box::leak(rendered.into_boxed_str())
    });
    &FILLED
}

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

const TICKETING_MD: &str = include_str!("agents/ticketing.md");
const VERSION_CONTROL_MD: &str = include_str!("agents/version-control.md");
const LOCAL_OPS_MD: &str = include_str!("agents/local-ops.md");
const DOCUMENTATION_MD: &str = include_str!("agents/documentation.md");

/// The 34-agent dispatchable default roster, embedded at compile time
/// (Slice E3, #2958, plus `pm` added for #3437, `ticketing` for #4027, and
/// the four #8129 delivery-workflow agents):
/// tcode's 8 own `Direct` defaults plus the 26 tm roster agents. The 5
/// `BASE-*` extends templates in [`EMBEDDED_TM_AGENT_SOURCES`] are
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
/// otherwise; see `assets/agents/pm.md`), plus the four #8129
/// delivery-workflow agents `ticketing` (issues via `gh`), `version-control`
/// (branch/commit/push/PR via `git`+`gh`), `local-ops` (build, test, version
/// bump, changelog) and `documentation` (prose) — all eight
/// [`EmbeddedAgent::Direct`], interleaved alphabetically from
/// `documentation` onward. Then the 26 [`EmbeddedAgent::Composed`] roster
/// agents, alphabetical, matching [`EMBEDDED_TM_AGENT_SOURCES`]'s roster
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
///
/// #8293: a `LazyLock` rather than a const, because the `pm` entry's card is
/// rendered from the shared routing rows at first use ([`pm_card`]). Every
/// existing caller reaches it through `.iter()`, which `LazyLock` derefs to
/// unchanged.
pub static DEFAULT_AGENTS: std::sync::LazyLock<Vec<EmbeddedAgent>> =
    std::sync::LazyLock::new(|| {
        vec![
            EmbeddedAgent::Direct {
                name: "engineer",
                md: ENGINEER_MD,
            },
            EmbeddedAgent::Direct {
                name: "qa-agent",
                md: QA_AGENT_MD,
            },
            EmbeddedAgent::Direct {
                name: "code-reviewer",
                md: CODE_REVIEWER_MD,
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
            EmbeddedAgent::Direct {
                name: "pm",
                md: pm_card(),
            },
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
                md: DOCUMENTATION_MD,
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
            EmbeddedAgent::Direct {
                name: "local-ops",
                md: LOCAL_OPS_MD,
            },
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
            EmbeddedAgent::Direct {
                name: "ticketing",
                md: TICKETING_MD,
            },
            EmbeddedAgent::Composed {
                name: "typescript-engineer",
            },
            // #8129: the branch/commit/push/PR half of the delivery workflow.
            EmbeddedAgent::Direct {
                name: "version-control",
                md: VERSION_CONTROL_MD,
            },
            EmbeddedAgent::Composed { name: "web-qa" },
            EmbeddedAgent::Composed {
                name: "web-ui-engineer",
            },
        ]
    });

// -- Slice E2 (#2958): embedded tm agent catalog, for `md_loader`'s in-memory
// extends-composer. Slice E3 wires the 26 roster names above into
// `DEFAULT_AGENTS` as `EmbeddedAgent::Composed` entries; this table remains
// their content source, resolved at load time via
// `agents::md_loader::project_embedded_md_with_extends`. --

const BASE_AGENT_MD: &str = trusty_agents_common::agent_assets::BASE_AGENT;
const BASE_ENGINEER_MD: &str = trusty_agents_common::agent_assets::BASE_ENGINEER;
const BASE_OPS_MD: &str = trusty_agents_common::agent_assets::BASE_OPS;
const BASE_QA_MD: &str = trusty_agents_common::agent_assets::BASE_QA;
const BASE_RESEARCH_MD: &str = trusty_agents_common::agent_assets::BASE_RESEARCH;

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

const API_QA_MD: &str = trusty_agents_common::agent_assets::API_QA;
const CODE_ANALYZER_MD: &str = include_str!("agents/code-analyzer.md");
const CODE_CRITIC_MD: &str = include_str!("agents/code-critic.md");
const DART_ENGINEER_MD: &str = trusty_agents_common::agent_assets::DART_ENGINEER;
const DATA_ENGINEER_MD: &str = trusty_agents_common::agent_assets::DATA_ENGINEER;
const GOLANG_ENGINEER_MD: &str = trusty_agents_common::agent_assets::GOLANG_ENGINEER;
const JAVA_ENGINEER_MD: &str = trusty_agents_common::agent_assets::JAVA_ENGINEER;
const JAVASCRIPT_ENGINEER_MD: &str = trusty_agents_common::agent_assets::JAVASCRIPT_ENGINEER;
const NEXTJS_ENGINEER_MD: &str = trusty_agents_common::agent_assets::NEXTJS_ENGINEER;
const ELIXIR_ENGINEER_MD: &str = trusty_agents_common::agent_assets::ELIXIR_ENGINEER;
const PHOENIX_ENGINEER_MD: &str = trusty_agents_common::agent_assets::PHOENIX_ENGINEER;
const PHP_ENGINEER_MD: &str = trusty_agents_common::agent_assets::PHP_ENGINEER;
const PROMPT_ENGINEER_MD: &str = trusty_agents_common::agent_assets::PROMPT_ENGINEER;
const PYTHON_ENGINEER_MD: &str = trusty_agents_common::agent_assets::PYTHON_ENGINEER;
const QA_MD: &str = include_str!("agents/qa.md");
const REACT_ENGINEER_MD: &str = trusty_agents_common::agent_assets::REACT_ENGINEER;
const REFACTORING_ENGINEER_MD: &str = trusty_agents_common::agent_assets::REFACTORING_ENGINEER;
const RESEARCH_MD: &str = trusty_agents_common::agent_assets::RESEARCH;
const RUBY_ENGINEER_MD: &str = trusty_agents_common::agent_assets::RUBY_ENGINEER;
const RUST_ENGINEER_MD: &str = trusty_agents_common::agent_assets::RUST_ENGINEER;
const SECURITY_MD: &str = trusty_agents_common::agent_assets::SECURITY;
const SVELTE_ENGINEER_MD: &str = trusty_agents_common::agent_assets::SVELTE_ENGINEER;
const TAURI_ENGINEER_MD: &str = trusty_agents_common::agent_assets::TAURI_ENGINEER;
const TYPESCRIPT_ENGINEER_MD: &str = trusty_agents_common::agent_assets::TYPESCRIPT_ENGINEER;
const WEB_QA_MD: &str = include_str!("agents/web-qa.md");
const WEB_UI_ENGINEER_MD: &str = trusty_agents_common::agent_assets::WEB_UI_ENGINEER;

/// The embedded tm agent catalog's raw sources (Slice E2, #2958): the 5
/// `BASE-*` extends templates plus the 26 `Composed` roster agents
/// (mpm/memory/cloud-vendor agents excluded -- see the issue's roster
/// decision; `ticketing`, `local-ops` and `documentation` left this table in
/// #8129, when tcode-native `Direct` agents took over those three dispatch
/// names -- see this module's "Delivery-workflow agents" doc section).
/// Keyed by each asset's ORIGINAL embedded
/// filename (`"BASE-QA.md"`, `"rust-engineer.md"`, ...) rather than a
/// pre-lowercased bare name, because
/// `trusty_agents_common::agents::builder_in_memory::InMemorySources::insert`
/// already lowercases and strips a trailing `.md` on both insert and lookup
/// (Slice E1, PR #3013) -- so `("BASE-QA.md", ...)` resolves an
/// `extends: base-qa` reference with no extra normalisation needed here.
///
/// Why: `agents::md_loader::project_embedded_md_with_extends` needs a single
/// batch source for `build_in_memory_source_map` instead of 31 individual
/// `insert` calls; this table is that source. Kept separate from
/// [`DEFAULT_AGENTS`] (rather than folded into it) because this table's key
/// space includes the 5 `BASE-*` templates, which must remain resolvable as
/// `extends:` targets while staying permanently non-dispatchable -- merging
/// the two tables would require a third state ("resolvable but not listed")
/// that the current `Direct`/`Composed` enum has no need to express.
/// What: 31 `(original_filename, raw_md_content)` pairs: 5 `BASE-*` templates
/// (never dispatchable) plus the 26 roster agents (dispatchable as of Slice
/// E3 via [`DEFAULT_AGENTS`]'s `EmbeddedAgent::Composed` entries, which
/// resolve against this table at load time).
/// Test: `assets::tests::embedded_tm_agent_sources_has_31_entries_and_unique_keys`,
/// `assets::tests::base_templates_are_never_dispatchable`,
/// `md_loader::tests::project_embedded_md_with_extends_resolves_rust_engineer_from_base_engineer`.
pub const EMBEDDED_TM_AGENT_SOURCES: &[(&str, &str)] = &[
    ("BASE-AGENT.md", BASE_AGENT_MD),
    ("BASE-ENGINEER.md", BASE_ENGINEER_MD),
    ("BASE-OPS.md", BASE_OPS_MD),
    ("BASE-QA.md", BASE_QA_MD),
    ("BASE-RESEARCH.md", BASE_RESEARCH_MD),
    ("api-qa.md", API_QA_MD),
    ("code-analyzer.md", CODE_ANALYZER_MD),
    ("code-critic.md", CODE_CRITIC_MD),
    ("dart-engineer.md", DART_ENGINEER_MD),
    ("data-engineer.md", DATA_ENGINEER_MD),
    ("golang-engineer.md", GOLANG_ENGINEER_MD),
    ("java-engineer.md", JAVA_ENGINEER_MD),
    ("javascript-engineer.md", JAVASCRIPT_ENGINEER_MD),
    ("nextjs-engineer.md", NEXTJS_ENGINEER_MD),
    ("elixir-engineer.md", ELIXIR_ENGINEER_MD),
    ("phoenix-engineer.md", PHOENIX_ENGINEER_MD),
    ("php-engineer.md", PHP_ENGINEER_MD),
    ("prompt-engineer.md", PROMPT_ENGINEER_MD),
    ("python-engineer.md", PYTHON_ENGINEER_MD),
    ("qa.md", QA_MD),
    ("react-engineer.md", REACT_ENGINEER_MD),
    ("refactoring-engineer.md", REFACTORING_ENGINEER_MD),
    ("research.md", RESEARCH_MD),
    ("ruby-engineer.md", RUBY_ENGINEER_MD),
    ("rust-engineer.md", RUST_ENGINEER_MD),
    ("security.md", SECURITY_MD),
    ("svelte-engineer.md", SVELTE_ENGINEER_MD),
    ("tauri-engineer.md", TAURI_ENGINEER_MD),
    ("typescript-engineer.md", TYPESCRIPT_ENGINEER_MD),
    ("web-qa.md", WEB_QA_MD),
    ("web-ui-engineer.md", WEB_UI_ENGINEER_MD),
];

const API_DESIGN_PATTERNS_SKILL: &str = include_str!("skills/api-design-patterns/SKILL.md");
const API_DOCUMENTATION_SKILL: &str = include_str!("skills/api-documentation/SKILL.md");
const ARTIFACTS_BUILDER_SKILL: &str = include_str!("skills/artifacts-builder/SKILL.md");
const BRAINSTORMING_SKILL: &str = include_str!("skills/brainstorming/SKILL.md");
const CODE_PRODUCTION_PROCESS_SKILL: &str = include_str!("skills/code-production-process/SKILL.md");
const CODE_REVIEW_STANDARDS_SKILL: &str = include_str!("skills/code-review-standards/SKILL.md");
const CONDITION_BASED_WAITING_SKILL: &str = include_str!("skills/condition-based-waiting/SKILL.md");
const CONTRACT_DRIVEN_TESTING_SKILL: &str = include_str!("skills/contract-driven-testing/SKILL.md");
const DATABASE_MIGRATION_SKILL: &str = include_str!("skills/database-migration/SKILL.md");
const DOCUMENTATION_STYLE_SKILL: &str = include_str!("skills/documentation-style/SKILL.md");
const ENV_MANAGER_SKILL: &str = include_str!("skills/env-manager/SKILL.md");
const GIT_WORKFLOW_SKILL: &str = include_str!("skills/git-workflow/SKILL.md");
const INTERNAL_COMMS_SKILL: &str = include_str!("skills/internal-comms/SKILL.md");
const JSON_DATA_HANDLING_SKILL: &str = include_str!("skills/json-data-handling/SKILL.md");
const MODEL_CONTEXT_BUILDER_SKILL: &str = include_str!("skills/model-context-builder/SKILL.md");
const REQUESTING_CODE_REVIEW_SKILL: &str = include_str!("skills/requesting-code-review/SKILL.md");
const ROOT_CAUSE_TRACING_SKILL: &str = include_str!("skills/root-cause-tracing/SKILL.md");
const SECURITY_SCANNING_SKILL: &str = include_str!("skills/security-scanning/SKILL.md");
const SOFTWARE_PATTERNS_SKILL: &str = include_str!("skills/software-patterns/SKILL.md");
const SYSTEMATIC_DEBUGGING_SKILL: &str = include_str!("skills/systematic-debugging/SKILL.md");
const TEST_DRIVEN_DEVELOPMENT_SKILL: &str = include_str!("skills/test-driven-development/SKILL.md");
const TEST_QUALITY_INSPECTOR_SKILL: &str = include_str!("skills/test-quality-inspector/SKILL.md");
const TESTING_ANTI_PATTERNS_SKILL: &str = include_str!("skills/testing-anti-patterns/SKILL.md");
const VERIFICATION_BEFORE_COMPLETION_SKILL: &str =
    include_str!("skills/verification-before-completion/SKILL.md");
const WEB_PERFORMANCE_OPTIMIZATION_SKILL: &str =
    include_str!("skills/web-performance-optimization/SKILL.md");
const WEBAPP_TESTING_SKILL: &str = include_str!("skills/webapp-testing/SKILL.md");
const WRITING_PLANS_SKILL: &str = include_str!("skills/writing-plans/SKILL.md");
const XLSX_SKILL: &str = include_str!("skills/xlsx/SKILL.md");

/// trusty-mpm's universal skill set, embedded at compile time (`tm-*`
/// orchestration skills excluded — see module docs).
///
/// Why: gives `skills::discover_skill_metadata`'s embedded-fallback branch a
/// fixed, ordered table to build a `SkillMetadata` catalog from when the disk
/// `.claude/skills/` directory is empty or absent.
/// What: 28 skills, sorted by name, each an `EmbeddedSkill { name, skill_md }`.
/// Test: `assets::tests::default_skills_names_are_unique`.
pub const DEFAULT_SKILLS: &[EmbeddedSkill] = &[
    EmbeddedSkill {
        name: "api-design-patterns",
        skill_md: API_DESIGN_PATTERNS_SKILL,
    },
    EmbeddedSkill {
        name: "api-documentation",
        skill_md: API_DOCUMENTATION_SKILL,
    },
    EmbeddedSkill {
        name: "artifacts-builder",
        skill_md: ARTIFACTS_BUILDER_SKILL,
    },
    EmbeddedSkill {
        name: "brainstorming",
        skill_md: BRAINSTORMING_SKILL,
    },
    EmbeddedSkill {
        name: "code-production-process",
        skill_md: CODE_PRODUCTION_PROCESS_SKILL,
    },
    EmbeddedSkill {
        name: "code-review-standards",
        skill_md: CODE_REVIEW_STANDARDS_SKILL,
    },
    EmbeddedSkill {
        name: "condition-based-waiting",
        skill_md: CONDITION_BASED_WAITING_SKILL,
    },
    EmbeddedSkill {
        name: "contract-driven-testing",
        skill_md: CONTRACT_DRIVEN_TESTING_SKILL,
    },
    EmbeddedSkill {
        name: "database-migration",
        skill_md: DATABASE_MIGRATION_SKILL,
    },
    EmbeddedSkill {
        name: "documentation-style",
        skill_md: DOCUMENTATION_STYLE_SKILL,
    },
    EmbeddedSkill {
        name: "env-manager",
        skill_md: ENV_MANAGER_SKILL,
    },
    EmbeddedSkill {
        name: "git-workflow",
        skill_md: GIT_WORKFLOW_SKILL,
    },
    EmbeddedSkill {
        name: "internal-comms",
        skill_md: INTERNAL_COMMS_SKILL,
    },
    EmbeddedSkill {
        name: "json-data-handling",
        skill_md: JSON_DATA_HANDLING_SKILL,
    },
    EmbeddedSkill {
        name: "model-context-builder",
        skill_md: MODEL_CONTEXT_BUILDER_SKILL,
    },
    EmbeddedSkill {
        name: "requesting-code-review",
        skill_md: REQUESTING_CODE_REVIEW_SKILL,
    },
    EmbeddedSkill {
        name: "root-cause-tracing",
        skill_md: ROOT_CAUSE_TRACING_SKILL,
    },
    EmbeddedSkill {
        name: "security-scanning",
        skill_md: SECURITY_SCANNING_SKILL,
    },
    EmbeddedSkill {
        name: "software-patterns",
        skill_md: SOFTWARE_PATTERNS_SKILL,
    },
    EmbeddedSkill {
        name: "systematic-debugging",
        skill_md: SYSTEMATIC_DEBUGGING_SKILL,
    },
    EmbeddedSkill {
        name: "test-driven-development",
        skill_md: TEST_DRIVEN_DEVELOPMENT_SKILL,
    },
    EmbeddedSkill {
        name: "test-quality-inspector",
        skill_md: TEST_QUALITY_INSPECTOR_SKILL,
    },
    EmbeddedSkill {
        name: "testing-anti-patterns",
        skill_md: TESTING_ANTI_PATTERNS_SKILL,
    },
    EmbeddedSkill {
        name: "verification-before-completion",
        skill_md: VERIFICATION_BEFORE_COMPLETION_SKILL,
    },
    EmbeddedSkill {
        name: "web-performance-optimization",
        skill_md: WEB_PERFORMANCE_OPTIMIZATION_SKILL,
    },
    EmbeddedSkill {
        name: "webapp-testing",
        skill_md: WEBAPP_TESTING_SKILL,
    },
    EmbeddedSkill {
        name: "writing-plans",
        skill_md: WRITING_PLANS_SKILL,
    },
    EmbeddedSkill {
        name: "xlsx",
        skill_md: XLSX_SKILL,
    },
];

// -- Tests --------------------------------------------------------------------
// Split into `tests.rs` (not inlined) to keep this include-table file thin;
// see `tests.rs` module docs.

#[cfg(test)]
mod tests;

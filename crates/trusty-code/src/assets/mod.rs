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
//! What: [`EmbeddedAgent`]/[`DEFAULT_AGENTS`] — the 32-agent dispatchable
//! roster (Slice E3, #2958, plus `pm` added for #3437): tcode's own 4
//! defaults (`engineer`, `qa-agent`, `code-reviewer`, `pm`, no `extends:`
//! chain — projected via `agents::md_loader::project_embedded_md`) plus the
//! 28 coding-relevant tm agents Bob selected in #2958 (`extends:`-chained
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
//! Four of the 28 roster agents — `qa`, `code-critic`, `code-analyzer`,
//! `web-qa` — carry an explicit restrictive `tools:` override in their tcode
//! copy (`assets/agents/{qa,code-critic,code-analyzer,web-qa}.md`) that is
//! NOT present in the trusty-mpm source files they were copied from
//! byte-for-byte in Slice E2. This is a deliberate, Bob-approved deviation
//! from byte-parity, not drift: those four are reviewer-intent agents and get
//! the same read-only tool allowlist tcode's own `code-reviewer` default uses
//! (no `write_file`/`edit`/`bash`). `documentation` and `research` stay
//! byte-identical and unrestricted per Bob's explicit ruling — they build
//! docs and research reports, not verdicts. NOTE for the file itself: the
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
//! `code-critic.md` needed no prose change — its review-only body already had
//! no execute-oriented instructions. This is an ADDITIONAL deviation from
//! trusty-mpm byte-parity beyond the `tools:` line alone; the deferred E4
//! staleness guard must whitelist the reworded prose sections in these three
//! files too, not just the `tools:` line.
//!
//! ## E4 staleness guard (issue #2958, `scripts/check_agent_assets.sh`)
//!
//! CI enforcement of everything above: byte-compares the 29 non-deviated
//! embedded copies against their trusty-mpm source, and pins the 4 deviated
//! files' upstream SOURCE hash (`scripts/agent-asset-pins.tsv`) so an
//! upstream edit behind one of them fails the gate for deliberate
//! reconciliation rather than drifting unnoticed. See that script's header
//! for the full design and `.github/workflows/agent-assets.yml` for the gate.
//!
//! Test: `assets::tests::*` — every embedded agent `.md` parses and projects
//! to a field-identical `AgentConfig` vs. the retired TOML fixtures (the
//! original 3), every roster name resolves through the extends composer with
//! no BASE template dispatchable and no name collision, the four restricted
//! agents' composed `AgentConfig` carries the read-only tool list, and
//! `documentation`/`research` remain unrestricted. Every skill name is unique
//! and every skill's frontmatter `name:` matches its table key.

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
const PM_MD: &str = include_str!("agents/pm.md");

/// The 32-agent dispatchable default roster, embedded at compile time
/// (Slice E3, #2958, plus `pm` added for #3437): tcode's original 3
/// defaults plus `pm` plus the 28 coding-relevant tm roster agents. The 5
/// `BASE-*` extends templates in [`EMBEDDED_TM_AGENT_SOURCES`] are
/// deliberately NOT entries here — they are extends-sources only, never
/// dispatchable — and trusty-mpm's own `engineer` agent is excluded from the
/// roster upstream (#2958's roster decision) precisely because it would
/// collide with tcode's `engineer` below;
/// `assets::tests::no_name_collisions_across_the_32_agent_roster` pins that
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
/// otherwise; see `assets/agents/pm.md`) — all four [`EmbeddedAgent::Direct`].
/// Then the 28 [`EmbeddedAgent::Composed`] roster agents, alphabetical,
/// matching [`EMBEDDED_TM_AGENT_SOURCES`]'s roster ordering. Four of them
/// (`qa`, `code-critic`, `code-analyzer`, `web-qa`) carry a tcode-only
/// restrictive `tools:` override in their `.md` source — see this module's
/// "Tools-restriction deviation" doc section above.
/// Test: `assets::tests::default_agents_parse_and_names_match`,
/// `assets::tests::base_templates_are_never_dispatchable`,
/// `assets::tests::no_name_collisions_across_the_32_agent_roster`,
/// `assets::tests::restricted_reviewer_agents_carry_read_only_tools`,
/// `assets::tests::documentation_and_research_remain_unrestricted`,
/// `assets::tests::default_task_run_agent_resolves_against_default_agents`,
/// `assets::tests::every_embedded_agent_model_normalizes_to_a_valid_slug`.
pub const DEFAULT_AGENTS: &[EmbeddedAgent] = &[
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
    EmbeddedAgent::Direct {
        name: "pm",
        md: PM_MD,
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
    EmbeddedAgent::Composed {
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
    EmbeddedAgent::Composed { name: "local-ops" },
    EmbeddedAgent::Composed {
        name: "nextjs-engineer",
    },
    EmbeddedAgent::Composed { name: "ops" },
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
    EmbeddedAgent::Composed {
        name: "typescript-engineer",
    },
    EmbeddedAgent::Composed { name: "web-qa" },
    EmbeddedAgent::Composed {
        name: "web-ui-engineer",
    },
];

// -- Slice E2 (#2958): embedded tm agent catalog, for `md_loader`'s in-memory
// extends-composer. Slice E3 wires the 28 roster names above into
// `DEFAULT_AGENTS` as `EmbeddedAgent::Composed` entries; this table remains
// their content source, resolved at load time via
// `agents::md_loader::project_embedded_md_with_extends`. --

const BASE_AGENT_MD: &str = include_str!("agents/BASE-AGENT.md");
const BASE_ENGINEER_MD: &str = include_str!("agents/BASE-ENGINEER.md");
const BASE_OPS_MD: &str = include_str!("agents/BASE-OPS.md");
const BASE_QA_MD: &str = include_str!("agents/BASE-QA.md");
const BASE_RESEARCH_MD: &str = include_str!("agents/BASE-RESEARCH.md");

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

const API_QA_MD: &str = include_str!("agents/api-qa.md");
const CODE_ANALYZER_MD: &str = include_str!("agents/code-analyzer.md");
const CODE_CRITIC_MD: &str = include_str!("agents/code-critic.md");
const DART_ENGINEER_MD: &str = include_str!("agents/dart-engineer.md");
const DATA_ENGINEER_MD: &str = include_str!("agents/data-engineer.md");
const DOCUMENTATION_MD: &str = include_str!("agents/documentation.md");
const GOLANG_ENGINEER_MD: &str = include_str!("agents/golang-engineer.md");
const JAVA_ENGINEER_MD: &str = include_str!("agents/java-engineer.md");
const JAVASCRIPT_ENGINEER_MD: &str = include_str!("agents/javascript-engineer.md");
const LOCAL_OPS_MD: &str = include_str!("agents/local-ops.md");
const NEXTJS_ENGINEER_MD: &str = include_str!("agents/nextjs-engineer.md");
const OPS_MD: &str = include_str!("agents/ops.md");
const PHOENIX_ENGINEER_MD: &str = include_str!("agents/phoenix-engineer.md");
const PHP_ENGINEER_MD: &str = include_str!("agents/php-engineer.md");
const PROMPT_ENGINEER_MD: &str = include_str!("agents/prompt-engineer.md");
const PYTHON_ENGINEER_MD: &str = include_str!("agents/python-engineer.md");
const QA_MD: &str = include_str!("agents/qa.md");
const REACT_ENGINEER_MD: &str = include_str!("agents/react-engineer.md");
const REFACTORING_ENGINEER_MD: &str = include_str!("agents/refactoring-engineer.md");
const RESEARCH_MD: &str = include_str!("agents/research.md");
const RUBY_ENGINEER_MD: &str = include_str!("agents/ruby-engineer.md");
const RUST_ENGINEER_MD: &str = include_str!("agents/rust-engineer.md");
const SECURITY_MD: &str = include_str!("agents/security.md");
const SVELTE_ENGINEER_MD: &str = include_str!("agents/svelte-engineer.md");
const TAURI_ENGINEER_MD: &str = include_str!("agents/tauri-engineer.md");
const TYPESCRIPT_ENGINEER_MD: &str = include_str!("agents/typescript-engineer.md");
const WEB_QA_MD: &str = include_str!("agents/web-qa.md");
const WEB_UI_ENGINEER_MD: &str = include_str!("agents/web-ui-engineer.md");

/// The embedded tm agent catalog's raw sources (Slice E2, #2958): the 5
/// `BASE-*` extends templates plus the 28 coding-relevant roster agents Bob
/// selected in #2958 (mpm/memory/cloud-vendor agents excluded -- see the
/// issue's roster decision). Keyed by each asset's ORIGINAL embedded
/// filename (`"BASE-QA.md"`, `"rust-engineer.md"`, ...) rather than a
/// pre-lowercased bare name, because
/// `trusty_agents_common::agents::builder_in_memory::InMemorySources::insert`
/// already lowercases and strips a trailing `.md` on both insert and lookup
/// (Slice E1, PR #3013) -- so `("BASE-QA.md", ...)` resolves an
/// `extends: base-qa` reference with no extra normalisation needed here.
///
/// Why: `agents::md_loader::project_embedded_md_with_extends` needs a single
/// batch source for `build_in_memory_source_map` instead of 33 individual
/// `insert` calls; this table is that source. Kept separate from
/// [`DEFAULT_AGENTS`] (rather than folded into it) because this table's key
/// space includes the 5 `BASE-*` templates, which must remain resolvable as
/// `extends:` targets while staying permanently non-dispatchable -- merging
/// the two tables would require a third state ("resolvable but not listed")
/// that the current `Direct`/`Composed` enum has no need to express.
/// What: 33 `(original_filename, raw_md_content)` pairs: 5 `BASE-*` templates
/// (never dispatchable) plus the 28 roster agents (dispatchable as of Slice
/// E3 via [`DEFAULT_AGENTS`]'s `EmbeddedAgent::Composed` entries, which
/// resolve against this table at load time).
/// Test: `assets::tests::embedded_tm_agent_sources_has_33_entries_and_unique_keys`,
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
    ("documentation.md", DOCUMENTATION_MD),
    ("golang-engineer.md", GOLANG_ENGINEER_MD),
    ("java-engineer.md", JAVA_ENGINEER_MD),
    ("javascript-engineer.md", JAVASCRIPT_ENGINEER_MD),
    ("local-ops.md", LOCAL_OPS_MD),
    ("nextjs-engineer.md", NEXTJS_ENGINEER_MD),
    ("ops.md", OPS_MD),
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

//! The `tm-capabilities` auto-generated harness capability catalog —
//! constants embedding the entry skill and its `references/*.md` siblings
//! (issue #2913).
//!
//! Why: five of these six files (everything except
//! [`TM_CAPABILITIES_WORKFLOWS`]) are produced by `tm generate capabilities`
//! (`crates/trusty-mpm/src/bin/tm/generate/`) from the harness's own
//! in-process data — the CLI command tree, the MCP tool catalog, the bundled
//! agent roster, the bundled skill catalog, and a maintained doctor-check
//! list. They are committed like any other bundled skill (not generated at
//! build time or deploy time — see the design-research brief on issue #2913
//! for why: build.rs and deploy-time-dynamic generation both recreate the
//! #2900 unwired-asset bug class) and gated by a CI drift check
//! (`scripts/check_capabilities.sh`). `TM_CAPABILITIES_WORKFLOWS` is the one
//! hand-authored exception — see its own doc comment.
//! What: `pub const` strings for the entry file plus five `references/*.md`
//! files, embedded at compile time via `include_str!`. Re-exported by
//! `bundle.rs`, registered in `bundle_all.rs::ALL` under the multi-file skill
//! layout (`skills/tm-capabilities.md` + `skills/tm-capabilities/references/
//! *.md`) the skill-port batch 1 (#2903) established.
//! Test: `bundle_tests.rs` — `tm_capabilities_is_in_bundle`,
//! `tm_capabilities_has_frontmatter`.

/// The `tm-capabilities` entry skill file.
///
/// Why: the file every agent loads first — states its own provenance
/// (auto-generated, do not hand-edit) and routes the reader to the right
/// `references/*.md` file rather than dumping everything inline.
/// What: embedded markdown deployed to `skills/tm-capabilities.md`. Produced
/// by `generate::entry::render`.
/// Test: `tm_capabilities_is_in_bundle`, `tm_capabilities_has_frontmatter`.
pub const TM_CAPABILITIES: &str = include_str!("../assets/skills/tm-capabilities.md");

/// Generated CLI command-tree reference (source #1, issue #2913).
///
/// Why: walks `Cli::command()` via clap introspection — the first use of
/// that API in the workspace — so it can never drift from what `tm` actually
/// parses (unlike scraping `--help` text).
/// What: embedded markdown deployed to
/// `skills/tm-capabilities/references/cli.md`. Produced by
/// `generate::cli_tree::render`.
/// Test: `tm_capabilities_is_in_bundle`.
pub const TM_CAPABILITIES_CLI: &str =
    include_str!("../assets/skills/tm-capabilities/references/cli.md");

/// Generated MCP tool catalog reference (source #2, issue #2913).
///
/// Why: `mcp::tools::tool_catalog()` is the single canonical, structured,
/// in-memory table already used to build `tools/list` responses — reading it
/// directly needs no parsing and can never drift.
/// What: embedded markdown deployed to
/// `skills/tm-capabilities/references/mcp-tools.md`. Produced by
/// `generate::mcp_tools::render`.
/// Test: `tm_capabilities_is_in_bundle`.
pub const TM_CAPABILITIES_MCP_TOOLS: &str =
    include_str!("../assets/skills/tm-capabilities/references/mcp-tools.md");

/// Generated agent roster reference (source #3, issue #2913).
///
/// Why: `bundle::ALL` filtered to `agents/*.md` plus
/// `agent_metadata::agent_metadata_from_str` reuse the exact frontmatter
/// grammar `agent_builder` uses at compose time, so this can never drift
/// from what actually deploys.
/// What: embedded markdown deployed to
/// `skills/tm-capabilities/references/agents.md`. Produced by
/// `generate::agents::render`.
/// Test: `tm_capabilities_is_in_bundle`.
pub const TM_CAPABILITIES_AGENTS: &str =
    include_str!("../assets/skills/tm-capabilities/references/agents.md");

/// Generated skill catalog reference (source #4, issue #2913).
///
/// Why: same `bundle::ALL` table [`TM_CAPABILITIES_AGENTS`] reads, filtered
/// to top-level `skills/*.md` entries (nested `references/*.md` siblings
/// excluded) and parsed with the shared frontmatter line parser.
/// What: embedded markdown deployed to
/// `skills/tm-capabilities/references/skills.md`. Produced by
/// `generate::skills::render`.
/// Test: `tm_capabilities_is_in_bundle`.
pub const TM_CAPABILITIES_SKILLS: &str =
    include_str!("../assets/skills/tm-capabilities/references/skills.md");

/// Generated doctor-check reference (source #5, issue #2913).
///
/// Why: doctor checks have no single data table in the source — this is a
/// maintained literal list (`generate::doctor::DOCTOR_CHECKS`) cross-checked
/// by a unit test (`doctor_checks_match_run_doctor_names`) against
/// `run_doctor`'s real output, so a drift fails the test suite, not just this
/// doc.
/// What: embedded markdown deployed to
/// `skills/tm-capabilities/references/doctor.md`. Produced by
/// `generate::doctor::render`.
/// Test: `tm_capabilities_is_in_bundle`.
pub const TM_CAPABILITIES_DOCTOR: &str =
    include_str!("../assets/skills/tm-capabilities/references/doctor.md");

/// Hand-authored canonical end-to-end workflows (issue #2913 brief §E).
///
/// Why: the design-research brief explicitly calls for one hand-authored
/// file among the otherwise-generated set — synthesis (how the CLI/MCP/
/// agent/skill pieces combine into an actual flow) is editorial judgment,
/// not mechanical enumeration, and mechanically regenerating it would
/// either lose that judgment or require teaching the generator to write
/// prose. Never touched by `tm generate capabilities` or its `--check` drift
/// gate.
/// What: embedded markdown deployed to
/// `skills/tm-capabilities/references/workflows.md`. Covers session launch,
/// delegation, doctor triage, and the bug-report pipeline.
/// Test: `tm_capabilities_is_in_bundle`.
pub const TM_CAPABILITIES_WORKFLOWS: &str =
    include_str!("../assets/skills/tm-capabilities/references/workflows.md");

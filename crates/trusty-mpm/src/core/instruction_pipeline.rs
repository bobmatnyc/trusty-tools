//! Instruction merge pipeline — compose a session's launch instructions.
//!
//! Why: every Claude Code session trusty-mpm starts must receive the same
//! framework instructions and the dynamic delegation routing context; doing
//! that merge ad-hoc at each launch site invites drift in ordering and
//! content.
//! What: [`build_instructions`] loads `INSTRUCTIONS.md`, generates the
//! delegation authority section from the deployed agents, and concatenates
//! the two sections in the fixed order 3 → 4 (framework → delegation). It
//! also seeds a project `CLAUDE.md` stub as a side effect (so a fresh
//! workspace always has a place for project notes), but — as of the #382 fix
//! — that project-notes content was never actually part of the text
//! delivered to `claude --append-system-prompt-file` (the real launch prompt
//! comes from [`crate::core::instruction_overrides::resolve_pm_prompt`] /
//! [`crate::core::session_launch::build_system_prompt_for`], neither of which
//! reads `PipelineOutput::merged`). Concatenating `CLAUDE.md` into `merged`
//! was therefore dead code that only risked a FUTURE accidental re-wiring of
//! a duplicate prompt payload (Claude Code already memory-loads `CLAUDE.md`
//! natively); it has been removed so `merged` cannot silently regrow that
//! duplication.
//! Test: `cargo test -p trusty-mpm-core instruction_pipeline` covers the
//! merge, a missing `INSTRUCTIONS.md`, and stub creation.

use std::fmt;
use std::path::PathBuf;

use crate::core::delegation_authority::{generate_authority, scan_agents};
use crate::core::instruction_package::SectionId;

/// Separator placed between merged instruction sections.
///
/// Why: the override resolver in [`crate::core::instruction_overrides`] must use
/// the identical rule so the bundled and override-resolved prompts never
/// visually diverge.
/// What: the `\n\n---\n\n` Markdown horizontal rule used between every section.
/// Test: `separators_are_consistent` in `instruction_overrides`.
pub(crate) const SECTION_SEPARATOR: &str = "\n\n---\n\n";

// ---------------------------------------------------------------------------
// Bundled system-prompt assembly
//
// Why: trusty-mpm must own its own PM instructions rather than reading from a
// `~/.claude-mpm/` install at runtime. The section assets below are embedded at
// compile time and assembled into a single `INSTRUCTIONS.md` that is passed to
// `claude --append-system-prompt-file` on every session launch.
//
// SOURCE OF TRUTH (#4183): one markdown file per [`SectionId`], under
// `assets/instructions/sections/`. The four monolithic assets this crate used to
// embed (`PM_INSTRUCTIONS.md`, `WORKFLOW.md`, `AGENT_DELEGATION.md`,
// `BASE_PM.md`) are gone; `pm_instructions()` and `base_pm()` reconstitute the
// two that spanned several sections so the legacy override assembly keeps its
// exact shape. Nothing is duplicated between the two models, so the composed
// package and the legacy assembly cannot drift apart in content — the property
// that makes `composed_package_is_byte_identical_to_the_legacy_bundled_fallback`
// meaningful rather than a check of two copies of the same paste.
// ---------------------------------------------------------------------------

/// Absorbed BASE_PM `## Identity` — who the PM is. Floor, tier `fixed`.
pub(crate) const SECTION_IDENTITY: &str =
    include_str!("../assets/instructions/sections/identity.md");
/// The PM's core operating instructions. Tier `project`.
pub(crate) const SECTION_CORE: &str = include_str!("../assets/instructions/sections/core.md");
/// Memory (context-first) protocol guidance. Tier `project`.
pub(crate) const SECTION_MEMORY: &str = include_str!("../assets/instructions/sections/memory.md");
/// Code/architecture search protocol guidance. Tier `project`.
pub(crate) const SECTION_SEARCH: &str = include_str!("../assets/instructions/sections/search.md");
/// 5-phase workflow execution details, including the sprint/harden doctrine.
///
/// `pub(crate)` so the override resolver can use it when no `WORKFLOW.md`
/// override is present.
pub(crate) const WORKFLOW: &str = include_str!("../assets/instructions/sections/workflow.md");
/// Agent delegation routing doctrine (the live roster is appended at compose
/// time, never authored here).
///
/// `pub(crate)` so the override resolver can use it when no
/// `AGENT_DELEGATION.md` override is present.
pub(crate) const AGENT_DELEGATION: &str =
    include_str!("../assets/instructions/sections/agent-delegation.md");
/// The canonical Prohibitions and Circuit Breakers tables. Floor, tier `fixed`.
///
/// Split out of `core.md` by #4573: both tables sat inside the `project`-tier
/// core section, so a three-line `CORE` block in a project's `CLAUDE.md` deleted
/// the PM's entire delegation-enforcement authority and still validated.
pub(crate) const SECTION_ENFORCEMENT: &str =
    include_str!("../assets/instructions/sections/enforcement.md");
/// Absorbed BASE_PM non-overridable rules, the customization contract, and the
/// Trusty tool-priority mandate. Floor, tier `fixed`.
pub(crate) const SECTION_NON_OVERRIDABLE_RULES: &str =
    include_str!("../assets/instructions/sections/non-overridable-rules.md");
/// Absorbed BASE_PM framework-guaranteed conventions. Floor, tier `fixed`.
pub(crate) const SECTION_FRAMEWORK_CONVENTIONS: &str =
    include_str!("../assets/instructions/sections/framework-guaranteed-conventions.md");

/// The compile-time table a schema-v2 `file` body resolves through.
///
/// Why: the instruction manifest (#4318) names its prose by path
/// (`{"kind":"file","path":"sections/core.md"}`) so the bulk of the instructions
/// keeps living in reviewable markdown rather than becoming one 23 KB JSON line
/// — but a path resolved at *runtime* would put the delivered system prompt at
/// the mercy of the filesystem and would let a renamed section ship as a silent
/// content drop. Every entry here is an `include_str!` of a constant declared
/// above, so the build stays hermetic and a missing section file is a compile
/// error rather than a launch-time surprise.
/// What: the nine canonical section sources, keyed by the path form the manifest
/// uses — relative to `assets/instructions/`. Table order is irrelevant; the
/// manifest's `blocks` array alone decides emission order.
/// Test: `every_section_source_resolves`, `unknown_file_source_is_rejected`.
pub(crate) const SECTION_SOURCES: [(&str, &str); 9] = [
    ("sections/identity.md", SECTION_IDENTITY),
    ("sections/core.md", SECTION_CORE),
    ("sections/memory.md", SECTION_MEMORY),
    ("sections/search.md", SECTION_SEARCH),
    ("sections/workflow.md", WORKFLOW),
    ("sections/agent-delegation.md", AGENT_DELEGATION),
    ("sections/enforcement.md", SECTION_ENFORCEMENT),
    (
        "sections/non-overridable-rules.md",
        SECTION_NON_OVERRIDABLE_RULES,
    ),
    (
        "sections/framework-guaranteed-conventions.md",
        SECTION_FRAMEWORK_CONVENTIONS,
    ),
];

/// Resolve a manifest `file` body path to its embedded source.
///
/// Why: one lookup point means a path typo in the manifest becomes a named
/// [`crate::core::instruction_package::ValidationError::UnknownFileSource`]
/// instead of an empty block.
/// What: a linear scan of [`SECTION_SOURCES`] — eight entries, called a handful
/// of times per process, so a map would buy nothing and would reintroduce the
/// iteration-order hazard the package format exists to avoid.
/// Test: `every_section_source_resolves`, `unknown_file_source_is_rejected`.
pub(crate) fn section_source(path: &str) -> Option<&'static str> {
    SECTION_SOURCES
        .iter()
        .find(|(key, _)| *key == path)
        .map(|(_, body)| *body)
}

/// The former `PM_INSTRUCTIONS.md` body, rebuilt from its three sections.
///
/// Why: the legacy override assembly
/// ([`crate::core::instruction_overrides::assemble_sections`]) treats the PM body
/// as one section it may be fully replaced by `PM_INSTRUCTIONS_DEPLOYED.md`. It
/// still needs that single string, but the *authored* source is now three files.
/// Reconstituting here — rather than keeping a fourth copy on disk — is what
/// stops the legacy path and the packaged path from delivering different
/// content once a section is edited (#4183).
/// What: Core, Memory and Search joined with a paragraph break, in that order,
/// with the trailing newline a file would have carried. The paragraph break is
/// deliberately [`crate::core::instruction_package::Join::Blank`]'s literal, so
/// this string is byte-identical to what the packaged composer emits for the
/// same three blocks.
/// Test: `pm_instructions_is_its_three_sections`,
/// `composed_package_is_byte_identical_to_the_legacy_bundled_fallback`.
pub(crate) fn pm_instructions() -> &'static str {
    static JOINED: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let run = manifest_run(&[SectionId::Core, SectionId::Memory, SectionId::Search])
            .unwrap_or_else(|| {
                format!(
                    "{}\n\n{}\n\n{}",
                    SECTION_CORE.trim(),
                    SECTION_MEMORY.trim(),
                    SECTION_SEARCH.trim()
                )
            });
        format!("{run}\n")
    });
    &JOINED
}

/// Project a run of sections out of the bundled manifest.
///
/// Why: since #4318 the manifest may author a rule inline rather than in a
/// section file, so rebuilding these strings from the `include_str!` constants
/// would deliver that rule to package-composed sessions and silently withhold it
/// from the legacy assembly and from [`assemble_system_prompt`]. Projecting the
/// manifest keeps one source of truth for the *content*, while the constants stay
/// as the retained fallback for the case where the manifest itself is unreadable.
/// What: [`crate::core::bundled_pm_package::authored_run`], or `None` when the
/// manifest failed to parse or validate.
/// Test: `pm_instructions_is_its_three_sections`, `base_pm_is_its_three_sections`.
fn manifest_run(sections: &[SectionId]) -> Option<String> {
    crate::core::bundled_pm_package::authored_run(sections).filter(|run| !run.trim().is_empty())
}

/// The bundled workflow section, as authored in the manifest.
///
/// Why: the workflow section is no longer only `sections/workflow.md` — the
/// manifest appends the opportunistic-fix rule as its own block. Every consumer
/// must therefore ask the manifest, not the constant, or a project with a
/// `WORKFLOW.md` override would be the only one to notice the difference.
/// What: the authored workflow blocks joined as declared, trimmed; the raw
/// `WORKFLOW` constant when the manifest is unreadable.
/// Test: `workflow_section_carries_the_opportunistic_fix_rule`,
/// `assemble_system_prompt_contains_all_sections`.
pub(crate) fn workflow_section() -> &'static str {
    static JOINED: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        manifest_run(&[SectionId::Workflow]).unwrap_or_else(|| WORKFLOW.trim().to_string())
    });
    &JOINED
}

/// The bundled delegation doctrine plus the roster-precedence note.
///
/// Why: #4318 moved the note's prose out of a Rust constant and into the manifest,
/// where its position between the doctrine and the live roster is declared rather
/// than formatted in by hand at two call sites.
/// What: the authored `agent-delegation` blocks joined as declared — the section
/// source followed by the note — trimmed. Falls back to the doctrine alone if the
/// manifest is unreadable, which is the pre-#4069 shape.
/// Test: `bundled_delegation_appends_deployed_roster`,
/// `composed_prompt_carries_the_live_roster_and_the_precedence_note`.
pub(crate) fn delegation_doctrine() -> &'static str {
    static JOINED: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        manifest_run(&[SectionId::AgentDelegation])
            .unwrap_or_else(|| AGENT_DELEGATION.trim().to_string())
    });
    &JOINED
}

/// The non-overridable framework floor, rebuilt from its three sections.
///
/// Why: the floor is appended last under *every* override branch, including full
/// PM replacement, so the resolver needs it as one opaque string. Same
/// no-duplicate-copy argument as [`pm_instructions`].
/// What: Identity, Enforcement (the Prohibitions and Circuit Breakers tables),
/// Non-Overridable Rules (which now carries the Trusty tool-priority mandate) and
/// Framework-Guaranteed Conventions, joined with a paragraph break.
///
/// #4573: `Enforcement` joins the floor here so the two authority tables reach
/// EVERY legacy branch too — including the `PM_INSTRUCTIONS_DEPLOYED.md` full
/// replacement, which discards every body section and appends only this string.
/// That is the second of the two wholesale-deletion paths #4573 names, and it is
/// closed by this list, not by the tier declaration alone.
///
/// ORDER CHANGE, recorded because it is the one floor reordering #4183 makes:
/// `BASE_PM.md` used to place `## Trusty Tool Priority (Non-Overridable)` *after*
/// `## Framework-Guaranteed Conventions`. It is a non-overridable rule, so it now
/// travels with the other non-overridable rules and consequently precedes the
/// conventions. Position only — not one word of either block changed, and both
/// remain inside the floor, so nothing about what is overridable moved.
/// Test: `base_pm_is_its_three_sections`, `floor_carries_the_tool_priority_mandate`.
pub(crate) fn base_pm() -> &'static str {
    static JOINED: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let run = manifest_run(&[
            SectionId::Identity,
            SectionId::Enforcement,
            SectionId::NonOverridableRules,
            SectionId::FrameworkGuaranteedConventions,
        ])
        .unwrap_or_else(|| {
            format!(
                "{}\n\n{}\n\n{}\n\n{}",
                SECTION_IDENTITY.trim(),
                SECTION_ENFORCEMENT.trim(),
                SECTION_NON_OVERRIDABLE_RULES.trim(),
                SECTION_FRAMEWORK_CONVENTIONS.trim()
            )
        });
        format!("{run}\n")
    });
    &JOINED
}

/// Assemble the full system prompt from bundled source components.
///
/// Why: a launched `claude` session must receive identical, version-controlled
/// PM instructions every time; embedding the sources and joining them here
/// removes any dependency on an external `~/.claude-mpm/` install.
/// What: concatenates the bundled sections in the fixed order
/// PM body → WORKFLOW → AGENT_DELEGATION → floor, separated by a `---` rule. The
/// floor comes last as the non-overridable framework floor; it carries the
/// Trusty MCP tool-priority block.
/// Test: `assemble_system_prompt_contains_all_sections`.
pub fn assemble_system_prompt() -> String {
    [
        pm_instructions(),
        workflow_section(),
        AGENT_DELEGATION,
        base_pm(),
    ]
    .join(SECTION_SEPARATOR)
}

/// Write the assembled system prompt to an explicit path.
///
/// Why: `tm install` must be testable against a temp directory rather than the
/// real `~/.trusty-mpm`; extracting the path-parameterised write here lets both
/// the production path ([`install_system_prompt`]) and the install handler use
/// the same assembly logic without touching the real home during unit tests.
/// What: creates parent directories if absent, then writes
/// [`assemble_system_prompt`] to `dest`.
/// Test: `install_system_prompt_to_writes_assembled`, `install_writes_assembled_prompt`.
pub fn install_system_prompt_to(dest: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, assemble_system_prompt())
}

/// Write the assembled system prompt to the trusty-mpm framework directory.
///
/// Why: `tm launch` passes `~/.trusty-mpm/framework/instructions/INSTRUCTIONS.md`
/// to `claude --append-system-prompt-file`; this regenerates that file from the
/// bundled assets so it always reflects the current trusty-mpm build.
/// What: creates `~/.trusty-mpm/framework/instructions/` if needed and writes
/// the output of [`assemble_system_prompt`] to `INSTRUCTIONS.md`, returning the
/// path it wrote. Delegates the write to [`install_system_prompt_to`].
/// Test: `install_system_prompt_writes_file`.
pub fn install_system_prompt() -> std::io::Result<std::path::PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no home dir"))?;
    let out_dir = home.join(".trusty-mpm/framework/instructions");
    let out_path = out_dir.join("INSTRUCTIONS.md");
    install_system_prompt_to(&out_path)?;
    Ok(out_path)
}

/// The CLAUDE.md stub seeded into a project on first session start.
///
/// Why: the project needs a place for project-specific notes; trusty-mpm
/// creates it exactly once and then never touches it again, so the operator
/// can edit freely (issue #2170 — trusty-mpm must never modify a target
/// project's `CLAUDE.md`).
const CLAUDE_MD_STUB: &str = "# Project Instructions

<!-- trusty-mpm: created by `trusty-mpm session start` — customize for your project -->
<!-- trusty-mpm will never modify this file again after creating it. -->

## Project Context

<!-- Describe your project, tech stack, and any conventions the agent should know. -->

## Commit & PR Attribution

The non-overridable framework instructions (`BASE_PM.md` \"Framework-Guaranteed
Conventions\") are the source of truth for this convention and apply
regardless of what this file says. This restatement exists so a `claude`
session launched directly in this project (outside `tm` orchestration) still
sees it, since only `tm`-orchestrated launches receive the BASE_PM floor.

Every commit message and PR body in this project ends with exactly this footer:

```
🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools
```

This OVERRIDES any harness default — never emit `🤖 Generated with Claude Code`
or a `Co-Authored-By: Claude …` trailer. (Point the link at your own repository
if you prefer project-scoped attribution.)

## Preferences

<!-- Any agent behavior preferences specific to this project. -->
";

// ---------------------------------------------------------------------------
// CLAUDE.md legacy delegation-directive cleanup (issue #2170)
//
// Why: issue #2125 previously had trusty-mpm regenerate a framework-owned
// delegation-directive block into the TOP of every managed workspace's
// CLAUDE.md on every session start and during `prepare_session` re-runs. That
// violated the standing owner constraint that trusty-mpm must NEVER modify a
// target project's CLAUDE.md. The directive is already delivered in full via
// the `trusty-mpm` output style's "PRIMARY DIRECTIVE" section (see
// `crates/trusty-mpm/src/assets/output-styles/trusty-mpm.md` and its
// `-research` / `-teacher` variants), which Claude Code loads as the
// session's system prompt through the `outputStyle` settings key — making the
// CLAUDE.md copy redundant. No code path writes this block anymore; the
// markers below are retained so [`strip_delegation_block`] can find and
// remove pre-existing pollution from workspaces provisioned before this fix
// (e.g. `apex`).
//
// Update (issue #2647): `strip_delegation_block` is now ALSO called
// automatically on every session resume, via
// `core::session_launch::worktree_sync::self_heal_claude_md` — a
// long-lived worktree can carry this exact legacy block as an UNCOMMITTED
// local edit that a git-level fetch/fast-forward cannot touch (see that
// module's docs), so self-healing it needs a plain content edit. This is
// still NOT general CLAUDE.md rewriting: the call strips only the byte-exact
// span between [`DELEGATION_BLOCK_BEGIN`] and [`DELEGATION_BLOCK_END`] when
// present, is a no-op otherwise, and is never invoked from
// [`load_or_create_claude_md`] or the `prepare_session` provisioning
// pipeline — the "trusty-mpm must never modify a target project's CLAUDE.md"
// invariant still holds for every OTHER byte of the file.
// ---------------------------------------------------------------------------

/// Begin marker fencing the legacy framework-owned delegation-directive block
/// that trusty-mpm injected into project `CLAUDE.md` files prior to issue
/// #2170.
///
/// What: an HTML-comment marker, invisible in rendered Markdown and harmless
/// to Claude Code's raw-text reading. Used only by [`strip_delegation_block`]
/// to locate legacy pollution — nothing writes it anymore.
/// Test: `strip_delegation_block_removes_legacy_block`.
const DELEGATION_BLOCK_BEGIN: &str = "<!-- trusty-mpm:delegation-directive:begin \
(framework-owned — do not edit; regenerated on every session start, see issue #2125) -->";

/// End marker paired with [`DELEGATION_BLOCK_BEGIN`].
const DELEGATION_BLOCK_END: &str = "<!-- trusty-mpm:delegation-directive:end -->";

/// One-time cleanup helper: remove a legacy trusty-mpm delegation-directive
/// block from `content`, if present.
///
/// Why: workspaces provisioned before issue #2170 landed may still carry the
/// block trusty-mpm used to inject, fenced by [`DELEGATION_BLOCK_BEGIN`] /
/// [`DELEGATION_BLOCK_END`]. This function is still NOT called from
/// [`load_or_create_claude_md`] or the `prepare_session` provisioning
/// pipeline — trusty-mpm must never rewrite a project's `CLAUDE.md` as part
/// of normal provisioning. As of issue #2647 it IS called from one other
/// site: `core::session_launch::worktree_sync::self_heal_claude_md`, on
/// every session resume — a targeted, anchored self-heal (this exact fenced
/// span only) rather than the general rewriting the invariant above still
/// forbids. It is also available for an operator (or a future explicit,
/// opt-in cleanup command) to call on demand.
/// What: if both fence markers are present (in order), removes the span
/// between them (inclusive) plus the blank line that follows it, leaving
/// every other byte of `content` untouched. Returns `content` unchanged when
/// no fenced block is found.
/// Test: `strip_delegation_block_removes_legacy_block`,
/// `strip_delegation_block_noop_when_absent`.
pub fn strip_delegation_block(content: &str) -> String {
    match (
        content.find(DELEGATION_BLOCK_BEGIN),
        content.find(DELEGATION_BLOCK_END),
    ) {
        (Some(start), Some(end)) if end > start => {
            let end = end + DELEGATION_BLOCK_END.len();
            let remainder = content[end..].trim_start_matches('\n');
            format!("{}{}", &content[..start], remainder)
        }
        _ => content.to_string(),
    }
}

/// Inputs to the instruction merge pipeline.
///
/// Why: bundles the three source locations so callers pass one value instead
/// of three loosely-related paths.
/// What: the framework `INSTRUCTIONS.md`, the deployed agents directory, and
/// the project `CLAUDE.md`.
/// Test: every `pipeline_*` test constructs one of these.
#[derive(Debug, Clone)]
pub struct PipelineInput {
    /// Path to the framework `INSTRUCTIONS.md`.
    pub framework_instructions_path: PathBuf,
    /// Directory of deployed agents (`~/.claude/agents/`).
    pub agents_dir: PathBuf,
    /// Path to the project `CLAUDE.md`.
    pub claude_md_path: PathBuf,
}

/// Result of a successful instruction merge.
///
/// Why: callers need both the composed text and a few flags describing what
/// happened (was a stub created? was `INSTRUCTIONS.md` present?) so they can
/// report it to the operator.
/// What: the merged instruction text plus per-source status flags.
/// Test: asserted by every `pipeline_*` test.
#[derive(Debug, Clone)]
pub struct PipelineOutput {
    /// The composed framework + delegation-authority instruction text.
    ///
    /// Deliberately excludes the project `CLAUDE.md` body (removed as dead
    /// code — see the module docs): Claude Code loads `CLAUDE.md` natively,
    /// and the actual launch prompt is built by
    /// [`crate::core::instruction_overrides::resolve_pm_prompt`], not this
    /// field.
    pub merged: String,
    /// False if `INSTRUCTIONS.md` was missing (treated as an empty section).
    pub instructions_loaded: bool,
    /// How many delegatable agents were found.
    pub agent_count: usize,
    /// True if the `CLAUDE.md` stub was created during this run.
    pub claude_md_created: bool,
}

/// A failure raised while composing session launch instructions.
///
/// Why: the pipeline performs filesystem I/O (reading `CLAUDE.md`, creating
/// the stub and its parent directory); callers need a typed failure surface.
/// What: wraps the underlying IO error with the path that triggered it.
/// Test: not exercised by the happy-path tests; surfaced if a path is invalid.
#[derive(Debug)]
pub enum PipelineError {
    /// A filesystem operation failed; payload is the offending path.
    Io {
        /// The path the failed operation targeted.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
}

impl fmt::Display for PipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "io error for {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for PipelineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
        }
    }
}

/// Compose the effective session launch instructions.
///
/// Why: every CC session needs consistent framework instructions + dynamic
/// routing context. It also needs the project `CLAUDE.md` to exist (Claude
/// Code loads it natively), so this still seeds the stub as a side effect.
///
/// What: loads INSTRUCTIONS.md (falls back to empty string if missing),
/// generates delegation authority from agents_dir, concatenates the two in
/// order 3→4, and returns the merged string. Separately ensures the project
/// `CLAUDE.md` exists (creating the stub if absent) purely for its side
/// effect — its content is intentionally NOT folded into `merged` (dead code
/// removed; see module docs).
///
/// Test: `pipeline_full`, `pipeline_missing_instructions`, `pipeline_creates_claude_md`
pub fn build_instructions(input: &PipelineInput) -> Result<PipelineOutput, PipelineError> {
    // Section 3: framework instructions. A missing file is not fatal — the
    // session can still launch with delegation context.
    let (framework, instructions_loaded) =
        match std::fs::read_to_string(&input.framework_instructions_path) {
            Ok(text) => (text, true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (String::new(), false),
            Err(err) => {
                return Err(PipelineError::Io {
                    path: input.framework_instructions_path.clone(),
                    source: err,
                });
            }
        };

    // Section 4: delegation authority, built fresh from the deployed agents.
    let agents = scan_agents(&input.agents_dir);
    let agent_count = agents.len();
    let authority = generate_authority(&agents);

    // Side effect only: ensure the project CLAUDE.md stub exists so a fresh
    // workspace always has a place for project notes. The content is
    // deliberately discarded here rather than folded into `merged` — Claude
    // Code already memory-loads `CLAUDE.md` natively, and the real launch
    // prompt is built by `resolve_pm_prompt`/`build_system_prompt_for`, not
    // this pipeline's `merged` output.
    let (_claude_md, claude_md_created) = load_or_create_claude_md(&input.claude_md_path)?;

    let merged = merge_sections(&framework, &authority);

    Ok(PipelineOutput {
        merged,
        instructions_loaded,
        agent_count,
        claude_md_created,
    })
}

/// Load `CLAUDE.md`, seeding the stub (and parent directories) if it does not
/// exist. A pre-existing `CLAUDE.md` is read back byte-identical and never
/// written to (issue #2170 — trusty-mpm must never modify a target project's
/// `CLAUDE.md`).
///
/// Why: section 5 is entirely project-owned; trusty-mpm creates the stub
/// exactly once, on first session start, so the operator always has a place
/// for project-specific notes, and never touches the file again afterward.
/// What: reads the existing file and returns it unchanged; if absent, writes
/// [`CLAUDE_MD_STUB`] (creating parent directories as needed) and returns
/// that. Returns the final content and whether this call created the file.
/// Test: `pipeline_creates_claude_md`, `pipeline_claude_md_left_byte_identical`.
fn load_or_create_claude_md(path: &PathBuf) -> Result<(String, bool), PipelineError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok((text, false)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent).map_err(|source| PipelineError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            std::fs::write(path, CLAUDE_MD_STUB).map_err(|source| PipelineError::Io {
                path: path.clone(),
                source,
            })?;
            Ok((CLAUDE_MD_STUB.to_string(), true))
        }
        Err(err) => Err(PipelineError::Io {
            path: path.clone(),
            source: err,
        }),
    }
}

/// Concatenate the two instruction sections in the fixed 3 → 4 order.
///
/// Why: the merge order is part of the framework contract; isolating it keeps
/// the ordering rule in one auditable place. A third "project CLAUDE.md"
/// section previously existed here (dead code removed — see module docs):
/// Claude Code loads `CLAUDE.md` natively and the real launch prompt never
/// read this function's output for that content.
/// What: joins framework and delegation authority with a `---` rule, skipping
/// empty sections so a missing `INSTRUCTIONS.md` does not leave a dangling
/// separator.
/// Test: `pipeline_full`, `pipeline_missing_instructions`.
fn merge_sections(framework: &str, authority: &str) -> String {
    let sections: Vec<&str> = [framework.trim(), authority.trim()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    let mut merged = sections.join(SECTION_SEPARATOR);
    if !merged.is_empty() {
        merged.push('\n');
    }
    merged
}

#[cfg(test)]
#[path = "instruction_pipeline_tests.rs"]
mod tests;

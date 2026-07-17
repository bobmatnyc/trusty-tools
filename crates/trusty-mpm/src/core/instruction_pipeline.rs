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
// `~/.claude-mpm/` install at runtime. The four source assets below are
// embedded at compile time and assembled into a single `INSTRUCTIONS.md` that
// is passed to `claude --append-system-prompt-file` on every session launch.
// ---------------------------------------------------------------------------

/// Token-optimized PM orchestration instructions (bundled at compile time).
///
/// `pub(crate)` so the override resolver in
/// [`crate::core::instruction_overrides`] can use it as the default when no
/// `PM_INSTRUCTIONS_DEPLOYED.md` override is present.
pub(crate) const PM_INSTRUCTIONS: &str = include_str!("../assets/instructions/PM_INSTRUCTIONS.md");
/// 5-phase workflow execution details (bundled at compile time).
///
/// `pub(crate)` so the override resolver can use it when no `WORKFLOW.md`
/// override is present.
pub(crate) const WORKFLOW: &str = include_str!("../assets/instructions/WORKFLOW.md");
/// Agent delegation routing table (bundled at compile time).
///
/// `pub(crate)` so the override resolver can use it when no
/// `AGENT_DELEGATION.md` override is present.
pub(crate) const AGENT_DELEGATION: &str =
    include_str!("../assets/instructions/AGENT_DELEGATION.md");
/// Non-overridable BASE_PM framework floor — includes the Trusty tool-priority
/// block — bundled at compile time. Placed last so it cannot be overridden.
///
/// `pub(crate)` so the override resolver can append it as the non-overridable
/// floor even under full PM replacement.
pub(crate) const BASE_PM: &str = include_str!("../assets/instructions/BASE_PM.md");

/// Assemble the full system prompt from bundled source components.
///
/// Why: a launched `claude` session must receive identical, version-controlled
/// PM instructions every time; embedding the sources and joining them here
/// removes any dependency on an external `~/.claude-mpm/` install.
/// What: concatenates the four bundled assets in the fixed order
/// PM_INSTRUCTIONS → WORKFLOW → AGENT_DELEGATION → BASE_PM, separated by a
/// `---` rule. BASE_PM comes last as the non-overridable framework floor; it
/// carries the Trusty MCP tool-priority block.
/// Test: `assemble_system_prompt_contains_all_sections`.
pub fn assemble_system_prompt() -> String {
    [PM_INSTRUCTIONS, WORKFLOW, AGENT_DELEGATION, BASE_PM].join("\n\n---\n\n")
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
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Write `<name>.md` into `dir` with the given raw content.
    fn write_file(path: &PathBuf, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, content).expect("write file");
    }

    /// Build a `PipelineInput` rooted at `tmp` with optional INSTRUCTIONS.md.
    fn input_in(tmp: &TempDir) -> PipelineInput {
        PipelineInput {
            framework_instructions_path: tmp.path().join("INSTRUCTIONS.md"),
            agents_dir: tmp.path().join("agents"),
            claude_md_path: tmp.path().join("project").join("CLAUDE.md"),
        }
    }

    #[test]
    fn pipeline_full() {
        // All inputs present → merged output contains the two framework
        // sections in the documented 3 → 4 order. The project CLAUDE.md is
        // deliberately NOT folded into `merged` (dead code removed — Claude
        // Code loads CLAUDE.md natively and the real launch prompt never
        // read this field for that content).
        let tmp = TempDir::new().unwrap();
        let input = input_in(&tmp);

        write_file(
            &input.framework_instructions_path,
            "# Framework\n\nFRAMEWORK SECTION\n",
        );
        fs::create_dir_all(&input.agents_dir).unwrap();
        write_file(
            &input.agents_dir.join("engineer.md"),
            "---\nname: engineer\nrole: engineer\ndescription: Builds things.\n---\n\n# Engineer\n",
        );
        write_file(&input.claude_md_path, "# Project\n\nPROJECT SECTION\n");

        let out = build_instructions(&input).unwrap();
        assert!(out.instructions_loaded);
        assert_eq!(out.agent_count, 1);
        assert!(
            !out.claude_md_created,
            "existing CLAUDE.md is not recreated"
        );

        let fw = out.merged.find("FRAMEWORK SECTION").expect("framework");
        let auth = out
            .merged
            .find("## Delegation Authority")
            .expect("authority");
        assert!(fw < auth, "framework precedes delegation authority");
        assert!(out.merged.contains("### engineer"));
        assert!(
            !out.merged.contains("PROJECT SECTION"),
            "project CLAUDE.md content must not be folded into merged: {}",
            out.merged
        );
    }

    #[test]
    fn pipeline_missing_instructions() {
        // INSTRUCTIONS.md absent → pipeline still succeeds, instructions_loaded
        // is false, and section 4 is still present.
        let tmp = TempDir::new().unwrap();
        let input = input_in(&tmp);
        // No INSTRUCTIONS.md written.
        fs::create_dir_all(&input.agents_dir).unwrap();
        write_file(
            &input.agents_dir.join("qa.md"),
            "---\nname: qa\nrole: qa\ndescription: Tests things.\n---\n\n# QA\n",
        );
        write_file(&input.claude_md_path, "# Project\n\nPROJECT NOTES\n");

        let out = build_instructions(&input).unwrap();
        assert!(!out.instructions_loaded);
        assert!(out.merged.contains("## Delegation Authority"));
        assert!(!out.merged.contains("PROJECT NOTES"));
        // No dangling separator at the very start.
        assert!(!out.merged.starts_with("---"));
    }

    #[test]
    fn pipeline_creates_claude_md() {
        // CLAUDE.md absent → it is created as a side effect, claude_md_created
        // is true, and the file on disk contains the stub — but the stub text
        // is NOT folded into `merged` (dead code removed).
        let tmp = TempDir::new().unwrap();
        let input = input_in(&tmp);
        write_file(&input.framework_instructions_path, "# Framework\n");
        fs::create_dir_all(&input.agents_dir).unwrap();

        assert!(!input.claude_md_path.exists());
        let out = build_instructions(&input).unwrap();
        assert!(out.claude_md_created);
        assert!(input.claude_md_path.exists());

        let on_disk = fs::read_to_string(&input.claude_md_path).unwrap();
        assert!(on_disk.contains("# Project Instructions"));
        assert!(on_disk.contains("trusty-mpm will never modify this file again"));
        // The seeded stub carries the attribution convention so every new
        // trusty-mpm project inherits the footer override at the framework
        // level (issue #2876) rather than relying on a per-project hand-edit.
        assert!(
            on_disk.contains("🤖🤖🤖 Generated with trusty-mpm"),
            "seeded stub must carry the attribution footer: {on_disk}"
        );
        assert!(
            !out.merged.contains("# Project Instructions"),
            "stub content must not be folded into merged: {}",
            out.merged
        );
    }

    #[test]
    fn pipeline_claude_md_left_byte_identical() {
        // Why (#2170): trusty-mpm must NEVER modify a target project's
        // CLAUDE.md. An existing file — including one that happens to
        // contain the literal delegation-block markers as ordinary operator
        // text — must come back out of `build_instructions` completely
        // untouched on disk, and its content must not leak into `merged`
        // (dead code removed).
        let tmp = TempDir::new().unwrap();
        let input = input_in(&tmp);
        write_file(&input.framework_instructions_path, "# Framework\n");
        fs::create_dir_all(&input.agents_dir).unwrap();
        let custom = "# My Project\n\nCUSTOM HAND-WRITTEN CONTENT\n";
        write_file(&input.claude_md_path, custom);

        let out = build_instructions(&input).unwrap();
        assert!(!out.claude_md_created);
        let on_disk = fs::read_to_string(&input.claude_md_path).unwrap();
        assert_eq!(
            on_disk, custom,
            "pre-existing CLAUDE.md must be left byte-identical: {on_disk}"
        );
        assert!(
            !on_disk.contains(DELEGATION_BLOCK_BEGIN),
            "no delegation-directive block may be injected: {on_disk}"
        );
        assert!(
            !out.merged.contains("CUSTOM HAND-WRITTEN CONTENT"),
            "project CLAUDE.md content must not be folded into merged: {}",
            out.merged
        );
    }

    #[test]
    fn strip_delegation_block_removes_legacy_block() {
        // Why (#2170 cleanup helper): a workspace polluted by the old #2125
        // injection must have the fenced block removed, leaving the
        // operator's own content untouched.
        let polluted = format!(
            "{DELEGATION_BLOCK_BEGIN}\n\nSome injected directive text.\n\n{DELEGATION_BLOCK_END}\n\n# My Project\n\nOperator notes.\n"
        );
        let cleaned = strip_delegation_block(&polluted);
        assert!(!cleaned.contains(DELEGATION_BLOCK_BEGIN));
        assert!(!cleaned.contains(DELEGATION_BLOCK_END));
        assert!(!cleaned.contains("Some injected directive text."));
        assert_eq!(cleaned, "# My Project\n\nOperator notes.\n");
    }

    #[test]
    fn strip_delegation_block_noop_when_absent() {
        // Why: a clean CLAUDE.md (the common case post-#2170) must be
        // returned byte-identical.
        let clean = "# My Project\n\nOperator notes.\n";
        assert_eq!(strip_delegation_block(clean), clean);
    }

    #[test]
    fn assemble_system_prompt_contains_all_sections() {
        // Why: the assembled prompt is the contract `claude` receives; every
        // bundled section must be present and joined with the `---` rule.
        let prompt = assemble_system_prompt();
        assert!(prompt.contains("# PM Agent -- Trusty MPM"));
        assert!(prompt.contains("# BASE_PM Framework Floor"));
        assert!(prompt.contains("# PM Workflow Configuration"));
        assert!(prompt.contains("# Agent Delegation Routing"));
        // The Trusty tool-priority block now lives inside the BASE_PM floor.
        assert!(prompt.contains("## Trusty Tool Priority (Non-Overridable)"));
        assert!(prompt.contains("\n\n---\n\n"));
        // BASE_PM is the non-overridable floor: it must come last.
        let base = prompt.find("# BASE_PM Framework Floor").expect("base_pm");
        let delegation = prompt
            .find("# Agent Delegation Routing")
            .expect("delegation");
        assert!(base > delegation, "BASE_PM floor must be appended last");
        // Ticketing-specific content was stripped from the bundled assets.
        assert!(!prompt.contains("mcp__mcp-ticketer__"));
        assert!(!prompt.contains("ticketing_agent"));
    }

    // Why serial + fake-HOME (extra sweep hit — review-gate report on top of
    // #2459/#2460/#2461): `install_system_prompt()` resolves `dirs::home_dir()`
    // internally and previously wrote straight into the REAL
    // `$HOME/.trusty-mpm/framework/instructions/` — both polluting the
    // developer's real home directory on every test run AND racing with any
    // other test in this binary that redirects `HOME` to a temp dir
    // concurrently (same class as `manifest_expands_tilde`, #2459). Redirect
    // `HOME` to an isolated temp dir for the duration of the test and join
    // the crate's shared `#[serial_test::serial]` lock so no other
    // HOME-mutating test can interleave.
    #[serial_test::serial]
    #[test]
    fn install_system_prompt_writes_file() {
        // Why: `tm launch` depends on `install_system_prompt` regenerating
        // INSTRUCTIONS.md; this asserts it writes a non-empty file under the
        // expected `~/.trusty-mpm/framework/instructions/` path.
        let fake_home = TempDir::new().expect("create fake home");
        let prev_home = std::env::var("HOME").ok();
        // SAFETY: serialized via `#[serial_test::serial]`, so no other test
        // thread observes or mutates HOME concurrently. Restored below
        // regardless of panics via the `HomeGuard` drop impl.
        unsafe { std::env::set_var("HOME", fake_home.path()) };
        struct HomeGuard(Option<String>);
        impl Drop for HomeGuard {
            fn drop(&mut self) {
                match &self.0 {
                    Some(p) => unsafe { std::env::set_var("HOME", p) },
                    None => unsafe { std::env::remove_var("HOME") },
                }
            }
        }
        let _guard = HomeGuard(prev_home);

        let out = install_system_prompt().expect("install succeeds");
        assert!(out.starts_with(fake_home.path()));
        assert!(out.ends_with("INSTRUCTIONS.md"));
        assert!(out.exists());
        let on_disk = fs::read_to_string(&out).unwrap();
        assert_eq!(on_disk, assemble_system_prompt());
        assert!(!on_disk.is_empty());
    }

    #[test]
    fn install_system_prompt_to_writes_assembled() {
        // Why: `tm install` calls `install_system_prompt_to` pointing at the
        // framework path so the assembled prompt (not the 4-line stub) is on
        // disk immediately after install — regression for issue #383.
        // What: asserts the file written by the path-parameterised helper
        // equals `assemble_system_prompt()` and contains key PM headings.
        // Test: call the helper against a temp path; read back and verify content.
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("instructions").join("INSTRUCTIONS.md");
        install_system_prompt_to(&dest).expect("write succeeds");
        assert!(
            dest.exists(),
            "INSTRUCTIONS.md must exist after install_system_prompt_to"
        );
        let on_disk = fs::read_to_string(&dest).unwrap();
        assert_eq!(
            on_disk,
            assemble_system_prompt(),
            "content must equal assembled prompt"
        );
        // The 4-line stub must NOT be present (regression guard for issue #383).
        assert!(
            !on_disk.trim().eq("# trusty-mpm Framework Instructions\n\nThis Claude Code instance is managed by trusty-mpm.\nDaemon endpoint: ${TRUSTY_MPM_URL:-http://localhost:7799}"),
            "stub content must not be written — full assembled prompt required"
        );
        // Real PM sections must be present.
        assert!(
            on_disk.contains("# PM Agent -- Trusty MPM"),
            "PM_INSTRUCTIONS section must be present"
        );
        assert!(
            on_disk.contains("# BASE_PM Framework Floor"),
            "BASE_PM floor must be present"
        );
    }

    #[test]
    fn primary_directive_mandate_not_duplicated_across_channels() {
        // Issue #2647 (R2): the FULL mandate table (Prohibitions, Circuit
        // Breakers, Delegation Map, PM Allowlist) must live in exactly ONE
        // channel — the appended system prompt (`assemble_system_prompt`,
        // always injected via `--append-system-prompt-file` on tm-driven
        // spawns). Before this fix, every bundled output-style file (loaded
        // independently via the Claude Code `outputStyle` settings key) ALSO
        // carried a full, near-identical restatement of that table —
        // duplicated tokens every session.
        //
        // Code-critic follow-up: `outputStyle` is deployed to
        // `<project>/.claude/output-styles/*.md` + `settings.json`
        // (`settings.rs`) and therefore applies to ANY `claude` launched in
        // that directory — including a manual `claude` invocation that never
        // receives `--append-system-prompt-file` at all
        // (`runtime/claude_code.rs`). A style with NOTHING but a pointer to
        // the appended prompt would leave such a launch with zero
        // enforcement. So each style keeps a short, SELF-CONTAINED minimal
        // mandate (PRIMARY DIRECTIVE statement, override-phrase list, a
        // prohibition summary) alongside the pointer to the full table —
        // this test asserts that block survived the dedup, not just that the
        // heading sentinel is de-duplicated.
        const SENTINEL: &str = "PRIMARY DIRECTIVE";
        let assembled = assemble_system_prompt();
        assert_eq!(
            assembled.matches(SENTINEL).count(),
            0,
            "the appended system prompt must not carry a literal PRIMARY \
             DIRECTIVE banner — the mandate lives there as Identity + \
             Prohibitions content"
        );

        // The appended prompt must still carry the substantive, canonical
        // mandate content — so the mandate can never silently vanish from
        // BOTH channels at once just because the banner text moved.
        assert!(
            assembled.contains("## Prohibitions"),
            "the appended prompt must carry the canonical Prohibitions table"
        );
        assert!(
            assembled.contains("## Circuit Breakers"),
            "the appended prompt must carry the canonical Circuit Breakers table"
        );
        assert!(
            assembled.contains("don't delegate"),
            "the appended prompt must carry at least one override phrase"
        );

        for style in crate::core::bundle::OUTPUT_STYLES {
            let combined =
                assembled.matches(SENTINEL).count() + style.content.matches(SENTINEL).count();
            assert_eq!(
                combined, 1,
                "{}: PRIMARY DIRECTIVE sentinel must appear exactly once across \
                 the appended prompt + this output style (one heading, never a \
                 full restatement of the mandate TABLE) — got {combined}",
                style.id
            );

            // Each style must still carry its OWN self-contained minimal
            // mandate — the override-phrase list and a prohibition summary —
            // so a manual `claude` launch (no --append-system-prompt-file)
            // in a tm-provisioned workspace is never left with zero
            // enforcement.
            assert!(
                style.content.contains("do this yourself"),
                "{}: must carry its own self-contained override-phrase list",
                style.id
            );
            assert!(
                style.content.contains("Minimum prohibitions"),
                "{}: must carry its own self-contained prohibition summary",
                style.id
            );
        }
    }

    #[test]
    fn pipeline_no_agents_still_succeeds() {
        // An empty agents dir yields a zero agent_count and the "no agents"
        // delegation section, but the pipeline still produces merged output.
        let tmp = TempDir::new().unwrap();
        let input = input_in(&tmp);
        write_file(&input.framework_instructions_path, "# Framework\n");
        fs::create_dir_all(&input.agents_dir).unwrap();

        let out = build_instructions(&input).unwrap();
        assert_eq!(out.agent_count, 0);
        assert!(out.merged.to_lowercase().contains("no delegatable agents"));
    }
}

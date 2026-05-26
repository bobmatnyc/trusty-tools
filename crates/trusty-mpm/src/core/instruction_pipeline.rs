//! Instruction merge pipeline — compose a session's launch instructions.
//!
//! Why: every Claude Code session trusty-mpm starts must receive the same
//! framework instructions, the dynamic delegation routing context, and any
//! project-specific notes; doing that merge ad-hoc at each launch site invites
//! drift in ordering and content.
//! What: [`build_instructions`] loads `INSTRUCTIONS.md`, generates the
//! delegation authority section from the deployed agents, loads or seeds a
//! project `CLAUDE.md` stub, and concatenates the three sections in the fixed
//! order 3 → 4 → 5 (framework → delegation → project).
//! Test: `cargo test -p trusty-mpm-core instruction_pipeline` covers the full
//! merge, a missing `INSTRUCTIONS.md`, stub creation, and stub preservation.

use std::fmt;
use std::path::PathBuf;

use crate::core::delegation_authority::{generate_authority, scan_agents};

/// Separator placed between the three merged instruction sections.
const SECTION_SEPARATOR: &str = "\n\n---\n\n";

// ---------------------------------------------------------------------------
// Bundled system-prompt assembly
//
// Why: trusty-mpm must own its own PM instructions rather than reading from a
// `~/.claude-mpm/` install at runtime. The four source assets below are
// embedded at compile time and assembled into a single `INSTRUCTIONS.md` that
// is passed to `claude --append-system-prompt-file` on every session launch.
// ---------------------------------------------------------------------------

/// Token-optimized PM orchestration instructions (bundled at compile time).
const PM_INSTRUCTIONS: &str = include_str!("../assets/instructions/PM_INSTRUCTIONS.md");
/// 5-phase workflow execution details (bundled at compile time).
const WORKFLOW: &str = include_str!("../assets/instructions/WORKFLOW.md");
/// Agent delegation routing table (bundled at compile time).
const AGENT_DELEGATION: &str = include_str!("../assets/instructions/AGENT_DELEGATION.md");
/// Non-overridable BASE_PM framework floor — includes the Trusty tool-priority
/// block — bundled at compile time. Placed last so it cannot be overridden.
const BASE_PM: &str = include_str!("../assets/instructions/BASE_PM.md");

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

/// Write the assembled system prompt to the trusty-mpm framework directory.
///
/// Why: `tm launch` passes `~/.trusty-mpm/framework/instructions/INSTRUCTIONS.md`
/// to `claude --append-system-prompt-file`; this regenerates that file from the
/// bundled assets so it always reflects the current trusty-mpm build.
/// What: creates `~/.trusty-mpm/framework/instructions/` if needed and writes
/// the output of [`assemble_system_prompt`] to `INSTRUCTIONS.md`, returning the
/// path it wrote.
/// Test: `install_system_prompt_writes_file`.
pub fn install_system_prompt() -> std::io::Result<std::path::PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no home dir"))?;
    let out_dir = home.join(".trusty-mpm/framework/instructions");
    std::fs::create_dir_all(&out_dir)?;
    let out_path = out_dir.join("INSTRUCTIONS.md");
    std::fs::write(&out_path, assemble_system_prompt())?;
    Ok(out_path)
}

/// The CLAUDE.md stub seeded into a project on first session start.
///
/// Why: the project needs a place for project-specific notes; trusty-mpm
/// creates it once and then never touches it, so the operator can edit freely.
const CLAUDE_MD_STUB: &str = "# Project Instructions

<!-- trusty-mpm: created by `trusty-mpm session start` — customize for your project -->
<!-- This file is yours: trusty-mpm will never overwrite it after creation. -->

## Project Context

<!-- Describe your project, tech stack, and any conventions the agent should know. -->

## Preferences

<!-- Any agent behavior preferences specific to this project. -->
";

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
    /// The full composed instruction text.
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
/// Why: every CC session needs consistent framework instructions +
/// dynamic routing context + project-specific notes.
///
/// What: loads INSTRUCTIONS.md (falls back to empty string if missing),
/// generates delegation authority from agents_dir, loads or creates
/// CLAUDE.md stub, concatenates in order 3→4→5, returns merged string.
///
/// Test: `pipeline_full`, `pipeline_missing_instructions`, `pipeline_creates_claude_md`
pub fn build_instructions(input: &PipelineInput) -> Result<PipelineOutput, PipelineError> {
    // Section 3: framework instructions. A missing file is not fatal — the
    // session can still launch with delegation context and project notes.
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

    // Section 5: the project CLAUDE.md, created from the stub if absent.
    let (claude_md, claude_md_created) = load_or_create_claude_md(&input.claude_md_path)?;

    let merged = merge_sections(&framework, &authority, &claude_md);

    Ok(PipelineOutput {
        merged,
        instructions_loaded,
        agent_count,
        claude_md_created,
    })
}

/// Load `CLAUDE.md`, seeding the stub (and parents) if it does not exist.
///
/// Why: section 5 is project-owned; trusty-mpm creates it exactly once and
/// then never overwrites it, so the operator's edits always survive.
/// What: returns the file's content and whether this call created it.
/// Test: `pipeline_creates_claude_md`, `pipeline_claude_md_not_overwritten`.
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

/// Concatenate the three instruction sections in the fixed 3 → 4 → 5 order.
///
/// Why: the merge order is part of the framework contract; isolating it keeps
/// the ordering rule in one auditable place.
/// What: joins framework, delegation authority, and project sections with a
/// `---` rule, skipping empty sections so a missing `INSTRUCTIONS.md` does not
/// leave a dangling separator.
/// Test: `pipeline_full`, `pipeline_missing_instructions`.
fn merge_sections(framework: &str, authority: &str, claude_md: &str) -> String {
    let sections: Vec<&str> = [framework.trim(), authority.trim(), claude_md.trim()]
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
        // All three inputs present → merged output contains all three sections
        // in the documented 3 → 4 → 5 order.
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
        let proj = out.merged.find("PROJECT SECTION").expect("project");
        assert!(fw < auth, "framework precedes delegation authority");
        assert!(auth < proj, "delegation authority precedes project notes");
        assert!(out.merged.contains("### engineer"));
    }

    #[test]
    fn pipeline_missing_instructions() {
        // INSTRUCTIONS.md absent → pipeline still succeeds, instructions_loaded
        // is false, and sections 4 + 5 are still present.
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
        assert!(out.merged.contains("PROJECT NOTES"));
        // No dangling separator at the very start.
        assert!(!out.merged.starts_with("---"));
    }

    #[test]
    fn pipeline_creates_claude_md() {
        // CLAUDE.md absent → it is created, claude_md_created is true, and the
        // file on disk contains the stub.
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
        assert!(on_disk.contains("trusty-mpm will never overwrite it"));
        assert!(out.merged.contains("# Project Instructions"));
    }

    #[test]
    fn pipeline_claude_md_not_overwritten() {
        // An existing CLAUDE.md with custom content must be preserved verbatim.
        let tmp = TempDir::new().unwrap();
        let input = input_in(&tmp);
        write_file(&input.framework_instructions_path, "# Framework\n");
        fs::create_dir_all(&input.agents_dir).unwrap();
        let custom = "# My Project\n\nCUSTOM HAND-WRITTEN CONTENT\n";
        write_file(&input.claude_md_path, custom);

        let out = build_instructions(&input).unwrap();
        assert!(!out.claude_md_created);
        let on_disk = fs::read_to_string(&input.claude_md_path).unwrap();
        assert_eq!(on_disk, custom, "custom CLAUDE.md must be untouched");
        assert!(out.merged.contains("CUSTOM HAND-WRITTEN CONTENT"));
    }

    #[test]
    fn assemble_system_prompt_contains_all_sections() {
        // Why: the assembled prompt is the contract `claude` receives; every
        // bundled section must be present and joined with the `---` rule.
        let prompt = assemble_system_prompt();
        assert!(prompt.contains("# PM Agent -- Claude MPM"));
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

    #[test]
    fn install_system_prompt_writes_file() {
        // Why: `tm launch` depends on `install_system_prompt` regenerating
        // INSTRUCTIONS.md; this asserts it writes a non-empty file under the
        // expected `~/.trusty-mpm/framework/instructions/` path.
        let out = install_system_prompt().expect("install succeeds");
        assert!(out.ends_with("INSTRUCTIONS.md"));
        assert!(out.exists());
        let on_disk = fs::read_to_string(&out).unwrap();
        assert_eq!(on_disk, assemble_system_prompt());
        assert!(!on_disk.is_empty());
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

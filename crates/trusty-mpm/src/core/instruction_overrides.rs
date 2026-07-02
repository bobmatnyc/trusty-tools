//! Project-level PM instruction overrides read from `<project>/.trusty-mpm/`.
//!
//! Why: `BASE_PM.md` advertises a "Customizing PM Behavior" table telling the PM
//! it can drop override files into the project's `.trusty-mpm/` directory and
//! that they take effect on the next session start. Historically *no code read
//! them* — the system prompt was a fixed compile-time concatenation, so every
//! advertised override was a silent no-op (issue #381). This module makes the
//! advertised behaviour real: it resolves the override files at session-prepare
//! time and layers them onto the bundled PM prompt per the documented
//! append/replace semantics.
//! What: [`resolve_pm_prompt`] reads up to five override files from
//! `<project>/.trusty-mpm/` and produces the effective PM prompt, *always*
//! appending the non-overridable `BASE_PM.md` floor last. The bundled assets are
//! the defaults for any section that has no override.
//! Test: the `tests` module exercises every documented file, the BASE_PM floor
//! invariant, and the robustness fallbacks (missing/empty/unreadable files).

use std::path::Path;

use crate::core::instruction_pipeline::{
    AGENT_DELEGATION, BASE_PM, PM_INSTRUCTIONS, SECTION_SEPARATOR, WORKFLOW,
};

/// Directory under the project root that holds the override files.
///
/// Why: the override files live alongside the inspectable instruction stash
/// (`last-instructions.md`) that `prepare_session` already writes, so they share
/// the project-local `.trusty-mpm/` directory documented in `BASE_PM.md`.
/// What: the literal directory name, joined onto the project root.
/// Test: every `resolve_*` test writes files under `<project>/.trusty-mpm/`.
pub const OVERRIDE_DIR_NAME: &str = ".trusty-mpm";

/// File name: full replacement of the PM instruction body (short-circuits).
pub const FILE_PM_DEPLOYED: &str = "PM_INSTRUCTIONS_DEPLOYED.md";
/// File name: replaces the bundled `AGENT_DELEGATION` section.
pub const FILE_AGENT_DELEGATION: &str = "AGENT_DELEGATION.md";
/// File name: replaces the bundled `WORKFLOW` section.
pub const FILE_WORKFLOW: &str = "WORKFLOW.md";
/// File name: replaces the memory guidance section.
pub const FILE_MEMORY: &str = "MEMORY.md";
/// File name: appended (additive) project rules.
pub const FILE_INSTRUCTIONS: &str = "INSTRUCTIONS.md";

/// Heading that delimits a `MEMORY.md` override block.
///
/// Why: there is no standalone bundled "memory" asset — the memory guidance is
/// the "Context-First Protocol (MANDATORY)" subsection inside `PM_INSTRUCTIONS`.
/// Rather than fragile surgical excision of that subsection, a `MEMORY.md`
/// override is slotted in as a clearly-delimited replacement block placed
/// immediately after `PM_INSTRUCTIONS` (and before `WORKFLOW`). The heading
/// makes it unambiguous to a reader — and to the launched PM — that the project
/// has overridden memory behaviour.
/// What: the Markdown heading prepended to the `MEMORY.md` body when slotted.
/// Test: `memory_override_is_slotted_after_pm_instructions`.
const MEMORY_OVERRIDE_HEADING: &str = "## Memory Behavior (project override)";

/// Read an override file, returning `Some(trimmed_contents)` only when it is
/// present and non-empty.
///
/// Why: the override semantics distinguish three states — absent, present but
/// empty, and present with content. Absent and empty both fall back to the
/// bundled default; an unreadable file (e.g. permission denied) also falls back.
/// Treating an empty file as "no override" (with a warning) avoids silently
/// blanking a whole section because someone `touch`ed a file. Robustness must
/// never hard-fail the launch.
/// What: joins `dir/name`; on a successful read of non-whitespace content
/// returns the trimmed body; on `NotFound` returns `None` silently; on an empty
/// file or any other IO error logs a `tracing::warn!` and returns `None`.
/// Test: `unreadable_override_falls_back`, `empty_override_falls_back`,
/// `missing_override_dir_uses_bundled`.
fn read_override(dir: &Path, name: &str) -> Option<String> {
    let path = dir.join(name);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                tracing::warn!(
                    path = %path.display(),
                    "instruction override file is empty; using bundled default"
                );
                None
            } else {
                tracing::info!(path = %path.display(), "applying instruction override");
                Some(trimmed.to_string())
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                %err,
                "instruction override file unreadable; using bundled default"
            );
            None
        }
    }
}

/// Resolve the effective PM prompt for `project_dir`, applying any overrides.
///
/// Why: this is the single source of truth #381 requires — both the live prompt
/// delivered to `claude` (`--append-system-prompt-file`) and the inspectable
/// stash (`tm session instructions` / `.trusty-mpm/last-instructions.md`) call
/// it, so the inspectable copy can never diverge from what the PM actually
/// received (the #382 concern).
///
/// What: reads override files from `<project_dir>/.trusty-mpm/` and assembles
/// the prompt per the `BASE_PM.md` "Customizing PM Behavior" table. Branch (1):
/// when `PM_INSTRUCTIONS_DEPLOYED.md` is present the body is *its* contents
/// (full replacement of PM_INSTRUCTIONS + WORKFLOW + AGENT_DELEGATION + MEMORY),
/// then `INSTRUCTIONS.md` (if present) is appended — **SHORT-CIRCUIT**. Branch
/// (2): otherwise PM_INSTRUCTIONS (bundled) → optional MEMORY override block →
/// WORKFLOW (override or bundled) → AGENT_DELEGATION (override or bundled), then
/// `INSTRUCTIONS.md` (if present) appended. In **both** branches the
/// non-overridable `BASE_PM.md` floor is appended **last** — it is never
/// replaceable, preserving the framework-floor guarantee `BASE_PM.md` itself
/// states. Sections are joined with [`SECTION_SEPARATOR`].
///
/// Robustness: a missing `.trusty-mpm/` directory, missing files, empty files,
/// and unreadable files all fall back to the bundled defaults without failing.
///
/// MEMORY.md slotting: there is no standalone bundled memory asset (the memory
/// guidance lives inside `PM_INSTRUCTIONS`). A `MEMORY.md` override is therefore
/// slotted as a clearly-delimited [`MEMORY_OVERRIDE_HEADING`] block placed
/// immediately after `PM_INSTRUCTIONS`, which the launched PM reads as the
/// authoritative, later-and-more-specific memory instruction.
///
/// Test: `no_overrides_uses_bundled`, `instructions_appended`,
/// `workflow_override_replaces`, `agent_delegation_override_replaces`,
/// `pm_deployed_replaces_body_but_keeps_base_floor`,
/// `memory_override_is_slotted_after_pm_instructions`, and the robustness tests.
pub fn resolve_pm_prompt(project_dir: &Path) -> String {
    let dir = project_dir.join(OVERRIDE_DIR_NAME);

    // Floor is always appended last and never replaceable.
    let floor = BASE_PM;

    // Branch 1: full replacement short-circuit. BASE_PM still floors it.
    if let Some(body) = read_override(&dir, FILE_PM_DEPLOYED) {
        let mut sections: Vec<String> = vec![body];
        if let Some(extra) = read_override(&dir, FILE_INSTRUCTIONS) {
            sections.push(extra);
        }
        sections.push(floor.trim().to_string());
        return join_sections(sections);
    }

    // Branch 2: section-by-section assembly with per-section overrides.
    let workflow =
        read_override(&dir, FILE_WORKFLOW).unwrap_or_else(|| WORKFLOW.trim().to_string());
    let delegation = read_override(&dir, FILE_AGENT_DELEGATION)
        .unwrap_or_else(|| AGENT_DELEGATION.trim().to_string());

    let mut sections: Vec<String> = vec![PM_INSTRUCTIONS.trim().to_string()];

    // MEMORY override slots in right after PM_INSTRUCTIONS as a delimited block.
    if let Some(memory) = read_override(&dir, FILE_MEMORY) {
        sections.push(format!("{MEMORY_OVERRIDE_HEADING}\n\n{memory}"));
    }

    sections.push(workflow);
    sections.push(delegation);

    // Additive project rules.
    if let Some(extra) = read_override(&dir, FILE_INSTRUCTIONS) {
        sections.push(extra);
    }

    // Non-overridable floor, always last.
    sections.push(floor.trim().to_string());

    join_sections(sections)
}

/// Join resolved sections with the framework separator, dropping empties.
///
/// Why: a defensive filter keeps a stray empty section from producing a dangling
/// `---` rule; centralizing the join keeps the separator consistent with the
/// bundled [`crate::core::instruction_pipeline::assemble_system_prompt`].
/// What: trims each section, drops empties, joins the rest with
/// [`SECTION_SEPARATOR`].
/// Test: exercised by every `resolve_*` test via the public entry point.
fn join_sections(sections: Vec<String>) -> String {
    sections
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(SECTION_SEPARATOR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Write `<project>/.trusty-mpm/<name>` with `content`, creating dirs.
    fn write_override(project: &Path, name: &str, content: &str) {
        let dir = project.join(OVERRIDE_DIR_NAME);
        fs::create_dir_all(&dir).expect("create .trusty-mpm");
        fs::write(dir.join(name), content).expect("write override");
    }

    #[test]
    fn no_overrides_uses_bundled() {
        // No `.trusty-mpm/` dir at all → the bundled four sections are present
        // and BASE_PM is the last section.
        let tmp = TempDir::new().unwrap();
        let prompt = resolve_pm_prompt(tmp.path());

        assert!(prompt.contains("# PM Agent -- Claude MPM"));
        assert!(prompt.contains("# PM Workflow Configuration"));
        assert!(prompt.contains("# Agent Delegation Routing"));
        assert!(prompt.contains("# BASE_PM Framework Floor"));

        let base = prompt.find("# BASE_PM Framework Floor").expect("base");
        let delegation = prompt.find("# Agent Delegation Routing").expect("deleg");
        assert!(base > delegation, "BASE_PM floor must be last");
    }

    #[test]
    fn instructions_appended() {
        // INSTRUCTIONS.md is additive: its content appears, the bundled sections
        // remain, and BASE_PM is still last.
        let tmp = TempDir::new().unwrap();
        write_override(
            tmp.path(),
            FILE_INSTRUCTIONS,
            "# Project Rules\n\nALWAYS_RUN_MAKE_CHECK\n",
        );
        let prompt = resolve_pm_prompt(tmp.path());

        assert!(prompt.contains("ALWAYS_RUN_MAKE_CHECK"));
        assert!(prompt.contains("# PM Agent -- Claude MPM"));
        assert!(prompt.contains("# Agent Delegation Routing"));

        let extra = prompt.find("ALWAYS_RUN_MAKE_CHECK").expect("extra");
        let base = prompt.find("# BASE_PM Framework Floor").expect("base");
        assert!(extra < base, "INSTRUCTIONS.md precedes the BASE_PM floor");
    }

    #[test]
    fn workflow_override_replaces() {
        // WORKFLOW.md replaces the bundled workflow section; other sections are
        // intact and the bundled workflow heading is gone.
        let tmp = TempDir::new().unwrap();
        write_override(
            tmp.path(),
            FILE_WORKFLOW,
            "# Custom Workflow\n\nTWO_PHASE_ONLY\n",
        );
        let prompt = resolve_pm_prompt(tmp.path());

        assert!(prompt.contains("TWO_PHASE_ONLY"));
        assert!(
            !prompt.contains("# PM Workflow Configuration"),
            "bundled workflow heading must be replaced"
        );
        // Other sections intact.
        assert!(prompt.contains("# PM Agent -- Claude MPM"));
        assert!(prompt.contains("# Agent Delegation Routing"));
        assert!(prompt.contains("# BASE_PM Framework Floor"));
    }

    #[test]
    fn agent_delegation_override_replaces() {
        // AGENT_DELEGATION.md replaces the bundled delegation section; others
        // intact.
        let tmp = TempDir::new().unwrap();
        write_override(
            tmp.path(),
            FILE_AGENT_DELEGATION,
            "# Custom Routing\n\nROUTE_ALL_TO_ENGINEER\n",
        );
        let prompt = resolve_pm_prompt(tmp.path());

        assert!(prompt.contains("ROUTE_ALL_TO_ENGINEER"));
        assert!(
            !prompt.contains("# Agent Delegation Routing"),
            "bundled delegation heading must be replaced"
        );
        assert!(prompt.contains("# PM Agent -- Claude MPM"));
        assert!(prompt.contains("# PM Workflow Configuration"));
        assert!(prompt.contains("# BASE_PM Framework Floor"));
    }

    #[test]
    fn memory_override_is_slotted_after_pm_instructions() {
        // MEMORY.md slots in as a delimited block right after PM_INSTRUCTIONS
        // and before the workflow section.
        let tmp = TempDir::new().unwrap();
        write_override(
            tmp.path(),
            FILE_MEMORY,
            "Recall from the `team` palace before any task.\n",
        );
        let prompt = resolve_pm_prompt(tmp.path());

        assert!(prompt.contains(MEMORY_OVERRIDE_HEADING));
        assert!(prompt.contains("Recall from the `team` palace"));

        let pm = prompt.find("# PM Agent -- Claude MPM").expect("pm");
        let mem = prompt.find(MEMORY_OVERRIDE_HEADING).expect("mem");
        let wf = prompt.find("# PM Workflow Configuration").expect("wf");
        assert!(pm < mem, "memory block follows PM_INSTRUCTIONS");
        assert!(mem < wf, "memory block precedes the workflow section");
        // Floor still last.
        let base = prompt.find("# BASE_PM Framework Floor").expect("base");
        assert!(wf < base);
    }

    #[test]
    fn pm_deployed_replaces_body_but_keeps_base_floor() {
        // PM_INSTRUCTIONS_DEPLOYED.md fully replaces the body, but the
        // non-overridable BASE_PM floor is STILL appended last, and the bundled
        // PM/workflow/delegation sections are gone.
        let tmp = TempDir::new().unwrap();
        write_override(
            tmp.path(),
            FILE_PM_DEPLOYED,
            "# Wholly Custom PM\n\nDO_EXACTLY_THIS\n",
        );
        let prompt = resolve_pm_prompt(tmp.path());

        assert!(prompt.contains("DO_EXACTLY_THIS"));
        // The non-overridable floor is preserved.
        assert!(
            prompt.contains("# BASE_PM Framework Floor"),
            "BASE_PM floor must always be appended"
        );
        assert!(prompt.contains("## Trusty Tool Priority (Non-Overridable)"));
        // Bundled body sections are replaced.
        assert!(!prompt.contains("# PM Agent -- Claude MPM"));
        assert!(!prompt.contains("# PM Workflow Configuration"));
        assert!(!prompt.contains("# Agent Delegation Routing"));

        // Floor is last.
        let body = prompt.find("DO_EXACTLY_THIS").expect("body");
        let base = prompt.find("# BASE_PM Framework Floor").expect("base");
        assert!(body < base, "BASE_PM floor must come after the custom body");
    }

    #[test]
    fn pm_deployed_still_appends_instructions() {
        // Even under full replacement, INSTRUCTIONS.md (additive) is appended
        // between the custom body and the BASE_PM floor.
        let tmp = TempDir::new().unwrap();
        write_override(tmp.path(), FILE_PM_DEPLOYED, "CUSTOM_BODY\n");
        write_override(tmp.path(), FILE_INSTRUCTIONS, "PROJECT_ADDENDUM\n");
        let prompt = resolve_pm_prompt(tmp.path());

        let body = prompt.find("CUSTOM_BODY").expect("body");
        let addendum = prompt.find("PROJECT_ADDENDUM").expect("addendum");
        let base = prompt.find("# BASE_PM Framework Floor").expect("base");
        assert!(body < addendum && addendum < base);
        // Bundled sections still absent under full replacement.
        assert!(!prompt.contains("# PM Workflow Configuration"));
    }

    #[test]
    fn missing_override_dir_uses_bundled() {
        // A `.trusty-mpm/` directory that does not exist is not an error.
        let tmp = TempDir::new().unwrap();
        assert!(!tmp.path().join(OVERRIDE_DIR_NAME).exists());
        let prompt = resolve_pm_prompt(tmp.path());
        assert!(prompt.contains("# PM Agent -- Claude MPM"));
        assert!(prompt.contains("# BASE_PM Framework Floor"));
    }

    #[test]
    fn empty_override_falls_back() {
        // An empty (whitespace-only) override file is treated as "no override":
        // the bundled default for that section is used (no silent blanking).
        let tmp = TempDir::new().unwrap();
        write_override(tmp.path(), FILE_WORKFLOW, "   \n\t\n");
        let prompt = resolve_pm_prompt(tmp.path());
        // Bundled workflow heading survives because the empty override is ignored.
        assert!(prompt.contains("# PM Workflow Configuration"));
        assert!(prompt.contains("# BASE_PM Framework Floor"));
    }

    #[test]
    fn unreadable_override_falls_back() {
        // A file that cannot be read (here: a directory in the file's place)
        // falls back to the bundled default rather than failing the launch.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(OVERRIDE_DIR_NAME);
        fs::create_dir_all(&dir).unwrap();
        // Create a *directory* named WORKFLOW.md so read_to_string errors with
        // something other than NotFound.
        fs::create_dir(dir.join(FILE_WORKFLOW)).unwrap();

        let prompt = resolve_pm_prompt(tmp.path());
        // Did not panic; bundled workflow is used.
        assert!(prompt.contains("# PM Workflow Configuration"));
        assert!(prompt.contains("# BASE_PM Framework Floor"));
    }

    #[test]
    fn separators_are_consistent() {
        // The resolved prompt uses the same `---` rule the bundled assembler
        // uses, so the two never visually diverge.
        let tmp = TempDir::new().unwrap();
        let prompt = resolve_pm_prompt(tmp.path());
        assert!(prompt.contains(SECTION_SEPARATOR));
    }
}

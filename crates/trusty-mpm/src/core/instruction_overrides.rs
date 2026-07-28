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
/// Per-project stack profile: in **both** branches an auto-derived
/// [`crate::core::stack_profile::stack_profile_section`] block is folded in right
/// after the PM body. It states the project's detected stack (or a neutral
/// detect-first profile when nothing is detected) so the PM is primed from the
/// project's real stack, never a hardcoded default (#1971). It is framework
/// context, not a user override, so it is not replaceable.
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
/// Delegation roster: the DEFAULT delegation section is composed by
/// [`delegation_with_roster`], which appends the LIVE deployed-agent roster to
/// the bundled asset (#4069). Like the stack profile it is auto-derived framework
/// context — but unlike it, it lives inside the overridable delegation section,
/// so an `AGENT_DELEGATION.md` override still replaces section and roster alike.
///
/// Composition source (#4183): the BUNDLED-FALLBACK configuration — no
/// delegation, workflow or memory override, and a roster present — is composed
/// through [`crate::core::bundled_pm_package`] and its typed
/// [`crate::core::instruction_package::InstructionPackage`], byte-identically to
/// the legacy assembly below. Every other configuration is deliberately still
/// assembled by [`assemble_sections`]; see the branch comment for why.
///
/// Test: `no_overrides_uses_bundled`, `instructions_appended`,
/// `workflow_override_replaces`, `agent_delegation_override_replaces`,
/// `bundled_delegation_appends_deployed_roster`,
/// `pm_deployed_replaces_body_but_keeps_base_floor`,
/// `memory_override_is_slotted_after_pm_instructions`,
/// `stack_profile_present_when_detected`, `stack_profile_neutral_when_undetected`,
/// and the robustness tests.
pub fn resolve_pm_prompt(project_dir: &Path) -> String {
    let dir = project_dir.join(OVERRIDE_DIR_NAME);

    // Floor is always appended last and never replaceable.
    let floor = BASE_PM;

    // Per-project stack profile derived from detected marker files (#1971). This
    // is auto-derived framework context, not a user override, so it is folded in
    // regardless of which assembly branch runs — every project's PM priming is
    // configured from that project's detected stack, never a hardcoded default.
    let stack = crate::core::stack_profile::stack_profile_section(project_dir);

    // Branch 1 (configuration 3): full replacement short-circuit. BASE_PM still
    // floors it. See #4183 — deliberately still on the legacy path: a deployed
    // body is opaque prose contributing no delegation section, so it fails both
    // `SectionWithoutBlocks` and `RosterNotConsumed`. Porting it needs schema
    // work tracked as follow-up on the epic; do not make it "schema-valid" by
    // weakening either check.
    if let Some(body) = read_override(&dir, FILE_PM_DEPLOYED) {
        let mut sections: Vec<String> = vec![body, stack];
        if let Some(extra) = read_override(&dir, FILE_INSTRUCTIONS) {
            sections.push(extra);
        }
        sections.push(floor.trim().to_string());
        return join_sections(sections);
    }

    // Branch 2: section-by-section assembly with per-section overrides.
    let workflow_override = read_override(&dir, FILE_WORKFLOW);
    let delegation_override = read_override(&dir, FILE_AGENT_DELEGATION);
    let memory_override = read_override(&dir, FILE_MEMORY);
    let addendum = read_override(&dir, FILE_INSTRUCTIONS);

    let delegation = match delegation_override {
        // See #4183 — configuration 2 is deliberately still on the legacy path:
        // an `AGENT_DELEGATION.md` override replaces the whole section and so
        // never consumes the computed roster, which `RosterNotConsumed` exists
        // to forbid. Follow-up on the epic, not a check to relax.
        Some(body) => body,
        None => {
            let roster = crate::core::delegation_authority::deployed_roster_section(project_dir);

            // #4183: configuration 1 (bundled fallback) composes through the
            // typed InstructionPackage. Gated on there being no section
            // override and a roster to consume — the only shape the schema can
            // express — and byte-identical to the assembly below.
            let bundled_fallback = roster
                .as_deref()
                .filter(|_| workflow_override.is_none() && memory_override.is_none());
            if let Some(roster) = bundled_fallback {
                match crate::core::bundled_pm_package::compose_bundled_fallback(
                    &stack,
                    roster,
                    addendum.as_deref(),
                ) {
                    Ok(prompt) => return prompt,
                    // Unreachable for the shipped assets — `shipped_assets_
                    // build_and_validate` proves it — so this is a loud last
                    // resort, never a routine fallback. The legacy assembly
                    // below is byte-identical, so degrading to it still
                    // delivers the right prompt; the error is what says the
                    // package model drifted from the assets.
                    Err(err) => tracing::error!(
                        %err,
                        "bundled PM instruction package failed to compose; \
                         falling back to the legacy assembly"
                    ),
                }
            }

            delegation_with_roster(roster.as_deref())
        }
    };

    let workflow = workflow_override.unwrap_or_else(|| WORKFLOW.trim().to_string());

    assemble_sections(stack, memory_override, workflow, delegation, addendum)
}

/// The legacy section-by-section assembly (branch 2).
///
/// Why: extracted so the configurations #4183 leaves on the legacy path share
/// one implementation with the byte-equality oracle the package path is tested
/// against — an oracle that is dead test code proves nothing.
/// What: `PM_INSTRUCTIONS` → stack profile → optional memory block → workflow →
/// delegation → optional addendum → the non-overridable floor, joined with
/// [`SECTION_SEPARATOR`] and with empty sections dropped.
/// Test: `no_overrides_uses_bundled`, `workflow_override_replaces`,
/// `memory_override_is_slotted_after_pm_instructions`, and
/// `composed_package_is_byte_identical_to_the_legacy_bundled_fallback` in
/// `bundled_pm_package_tests.rs`.
pub(crate) fn assemble_sections(
    stack: String,
    memory_override: Option<String>,
    workflow: String,
    delegation: String,
    addendum: Option<String>,
) -> String {
    let mut sections: Vec<String> = vec![PM_INSTRUCTIONS.trim().to_string(), stack];

    // MEMORY override slots in right after PM_INSTRUCTIONS as a delimited block.
    if let Some(memory) = memory_override {
        sections.push(format!("{MEMORY_OVERRIDE_HEADING}\n\n{memory}"));
    }

    sections.push(workflow);
    sections.push(delegation);

    // Additive project rules.
    if let Some(extra) = addendum {
        sections.push(extra);
    }

    // Non-overridable floor, always last.
    sections.push(BASE_PM.trim().to_string());

    join_sections(sections)
}

/// Note slotted between the bundled doctrine and the live roster.
///
/// Why: it resolves two ambiguities the append creates, both raised in review.
///
/// (1) PRECEDENCE. The retained asset contradicts the roster on concrete points
/// — it calls the generic `ops` agent DEPRECATED while the roster lists `ops`,
/// and it advertises agents the roster may not carry. Reconciling the asset is
/// #4183's job; until then the PM needs one unambiguous tie-break rule.
///
/// (2) LOADABILITY. The roster is the UNION of three tiers, but no launch mode
/// reads all three: the daemon managed-spawn passes `--setting-sources
/// project,local`, which excludes both user-level tiers. A narrower per-mode
/// tier set is not available here — `tm launch` / `tm connect` deploy into
/// `~/.claude/agents` (`FrameworkPaths::default()`) and then spawn with that
/// very flag, so the tier they populate is the tier the flag excludes. Passing
/// "the tier the flag reads" would render an EMPTY roster there and silently
/// revert those paths to the stale asset — reintroducing #4069. Passing "the
/// tier that was deployed to" would advertise unloadable agents anyway. Until
/// that deploy/read mismatch is fixed, the union plus an explicit re-route
/// instruction is the shape that cannot silently under-advertise; a failed
/// dispatch is self-correcting, a missing agent is not.
///
/// What: a Markdown blockquote stating the roster wins on WHICH agents exist,
/// and to re-route rather than retry on an unknown-agent-type error.
///
/// `pub(crate)` since #4183: [`crate::core::bundled_pm_package`] emits it as its
/// own delegation-section block, so both composers use the identical literal.
/// Test: `bundled_delegation_appends_deployed_roster`.
pub(crate) const ROSTER_PRECEDENCE_NOTE: &str = "\
> The live roster below is authoritative for WHICH agents exist and what each handles; the \
tables above are routing doctrine only. Where the two disagree, trust the roster.\n\
>\n\
> Depending on how this session was launched, a listed agent may not be loadable. If a \
dispatch fails with an unknown agent type, re-route to the closest listed alternative — do \
not retry the same agent.";

/// The DEFAULT delegation section: bundled routing doctrine + the live roster.
///
/// Why (#4069): the delegation section is meant to be constructed from what is
/// actually deployed, and `build_instructions` does compute that roster via
/// `generate_authority` — but its output lands in a `PipelineOutput` string no
/// prompt composer reads, so the fallback here shipped the static
/// `AGENT_DELEGATION.md` asset verbatim. That asset's agent table has been
/// hand-maintained since 2026-07-03 and names 8 agents; a real deployment
/// carries ~40, so agents like `ticketing` and `memory-manager` appeared **zero
/// times** in the delivered prompt and the PM could not route to them. The
/// bundled asset is still emitted in full because it carries routing doctrine
/// the roster does not (make/mise routing, keyword routing, the ops-agent
/// table) — the live roster is *appended*, never substituted, so no doctrine is
/// lost. Restructuring the asset itself is out of scope (epic #4183).
///
/// This is the FALLBACK only: a project/user `AGENT_DELEGATION.md` override
/// still replaces the whole section, unchanged, exactly as documented.
///
/// What: returns the trimmed bundled asset, with the rendered
/// `## Delegation Authority` block from
/// [`crate::core::delegation_authority::deployed_roster_section`] appended when
/// any agent is deployed. With no deployed agents the asset is returned alone
/// (pre-#4069 behaviour). Takes the already-rendered roster rather than scanning
/// itself (#4183) so `resolve_pm_prompt` scans the agent tiers exactly once
/// whichever composition path it takes.
/// Test: `bundled_delegation_appends_deployed_roster`,
/// `no_overrides_uses_bundled`, `agent_delegation_override_replaces`.
pub(crate) fn delegation_with_roster(roster: Option<&str>) -> String {
    let bundled = AGENT_DELEGATION.trim();
    match roster {
        Some(roster) => format!("{bundled}\n\n{ROSTER_PRECEDENCE_NOTE}\n\n{}", roster.trim()),
        None => bundled.to_string(),
    }
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

    /// Deploy a composed agent into the PROJECT tier (`<project>/.claude/agents`).
    ///
    /// Why: the project tier is the highest-precedence roster source and the one
    /// the daemon managed-spawn path actually deploys into, so a test that writes
    /// here asserts the real launch shape without depending on the machine's
    /// `~/.claude/agents` (which these tests must never require or forbid).
    fn deploy_agent(project: &Path, name: &str) {
        let dir = project.join(".claude").join("agents");
        fs::create_dir_all(&dir).expect("create .claude/agents");
        fs::write(
            dir.join(format!("{name}.md")),
            format!(
                "---\nname: {name}\nrole: {name}\ndescription: Handles {name} work.\n\
                 model: sonnet\n---\n\n# {name}\n"
            ),
        )
        .expect("write agent");
    }

    #[test]
    fn bundled_delegation_appends_deployed_roster() {
        // Issue #4069 REGRESSION GATE. The delegation section must describe the
        // agents that are actually deployed, not the static asset's
        // hand-maintained table. `ticketing` and `memory-manager` are deployed
        // in reality but appear NOWHERE in any bundled asset, so a rendered
        // `### <name>` roster entry for them can only come from the live scan.
        // Against the pre-fix code this assertion fails: `resolve_pm_prompt`
        // returned the static `AGENT_DELEGATION.md` verbatim.
        let tmp = TempDir::new().unwrap();
        deploy_agent(tmp.path(), "ticketing");
        deploy_agent(tmp.path(), "memory-manager");

        let prompt = resolve_pm_prompt(tmp.path());

        assert!(
            prompt.contains("## Delegation Authority"),
            "the live roster section must be present"
        );
        assert!(
            prompt.contains("### ticketing"),
            "a deployed agent absent from the static asset must reach the prompt"
        );
        assert!(
            prompt.contains("### memory-manager"),
            "a deployed agent absent from the static asset must reach the prompt"
        );
        assert!(
            prompt.contains("Handles ticketing work."),
            "the agent's own description must reach the prompt"
        );

        // The bundled routing doctrine is APPENDED to, never replaced: the
        // roster carries no make/mise or keyword routing rules.
        assert!(prompt.contains("# Agent Delegation Routing"));
        assert!(prompt.contains("## Make / Mise Command Routing"));

        // Review HIGH-2 / MEDIUM-2: the two blocks contradict each other on
        // concrete points, and the roster is a tier UNION that no single launch
        // mode fully loads. Both are resolved by the note between them, which
        // must be present whenever a roster is rendered.
        assert!(
            prompt.contains("trust the roster"),
            "the roster must be declared authoritative over the stale doctrine table"
        );
        assert!(
            prompt.contains("re-route to the closest listed alternative"),
            "an advertised-but-unloadable agent must carry a recovery instruction"
        );

        // Ordering: doctrine first, note, roster after, BASE_PM floor last.
        let doctrine = prompt.find("# Agent Delegation Routing").expect("doctrine");
        let note = prompt.find("trust the roster").expect("note");
        let roster = prompt.find("## Delegation Authority").expect("roster");
        let base = prompt.find("# BASE_PM Framework Floor").expect("base");
        assert!(doctrine < note, "doctrine precedes the note");
        assert!(note < roster, "the note precedes the roster it governs");
        assert!(roster < base, "BASE_PM floor stays last");
    }

    #[test]
    fn no_overrides_uses_bundled() {
        // No `.trusty-mpm/` dir at all → the bundled four sections are present
        // and BASE_PM is the last section.
        let tmp = TempDir::new().unwrap();
        let prompt = resolve_pm_prompt(tmp.path());

        assert!(prompt.contains("# PM Agent -- Trusty MPM"));
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
        assert!(prompt.contains("# PM Agent -- Trusty MPM"));
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
        assert!(prompt.contains("# PM Agent -- Trusty MPM"));
        assert!(prompt.contains("# Agent Delegation Routing"));
        assert!(prompt.contains("# BASE_PM Framework Floor"));
    }

    #[test]
    fn agent_delegation_override_replaces() {
        // AGENT_DELEGATION.md replaces the bundled delegation section; others
        // intact. Issue #4069 must not weaken this precedence: an agent is
        // deployed here, and the override still replaces the WHOLE section, so
        // neither the bundled doctrine nor the auto-generated roster is emitted.
        let tmp = TempDir::new().unwrap();
        deploy_agent(tmp.path(), "ticketing");
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
        assert!(
            !prompt.contains("### ticketing"),
            "an override replaces the section outright — the roster is not re-appended"
        );
        assert!(prompt.contains("# PM Agent -- Trusty MPM"));
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

        let pm = prompt.find("# PM Agent -- Trusty MPM").expect("pm");
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
        assert!(!prompt.contains("# PM Agent -- Trusty MPM"));
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
        assert!(prompt.contains("# PM Agent -- Trusty MPM"));
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

    #[test]
    fn stack_profile_present_when_detected() {
        // A detected project (Cargo.toml) folds in the auto-derived stack profile
        // right after PM_INSTRUCTIONS and before the BASE_PM floor, routing to the
        // detected engineer — never a hardcoded default (#1971).
        use crate::core::stack_profile::STACK_PROFILE_HEADING;
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();

        let prompt = resolve_pm_prompt(tmp.path());
        assert!(prompt.contains(STACK_PROFILE_HEADING));
        assert!(prompt.contains("`rust-engineer`"));

        let pm = prompt.find("# PM Agent -- Trusty MPM").expect("pm");
        let stack = prompt.find(STACK_PROFILE_HEADING).expect("stack");
        let base = prompt.find("# BASE_PM Framework Floor").expect("base");
        assert!(pm < stack, "stack profile follows PM_INSTRUCTIONS");
        assert!(stack < base, "stack profile precedes the BASE_PM floor");
    }

    #[test]
    fn stack_profile_neutral_when_undetected() {
        // An unknown project type yields a neutral, detect-first profile — the PM
        // is told NOT to assume any stack rather than inheriting a default.
        use crate::core::stack_profile::STACK_PROFILE_HEADING;
        let tmp = TempDir::new().unwrap();
        let prompt = resolve_pm_prompt(tmp.path());
        assert!(prompt.contains(STACK_PROFILE_HEADING));
        assert!(prompt.contains("Do NOT assume any stack"));
    }

    #[test]
    fn framework_guaranteed_conventions_survive_every_override_combination() {
        // Issue #3374 (mirrors `primary_directive_mandate_not_duplicated_across_
        // channels` in instruction_pipeline.rs): the commit/PR attribution
        // footer, the proportional-documentation policy, and the ticket-
        // attribution-at-change-site convention must live in the BASE_PM floor
        // and therefore survive EVERY override combination — including the
        // full-PM-replacement branch, where every bundled body section is
        // discarded but the floor is still appended.
        const MARKERS: &[&str] = &[
            "Generated with trusty-mpm",
            "Proportional documentation",
            "Ticket attribution at the change site",
        ];

        // No overrides at all: conventions present via the bundled floor.
        let tmp = TempDir::new().unwrap();
        let prompt = resolve_pm_prompt(tmp.path());
        for marker in MARKERS {
            assert!(
                prompt.contains(marker),
                "no-override prompt must carry the guaranteed convention {marker:?}"
            );
        }

        // Section-by-section overrides (workflow + delegation + instructions):
        // the floor is untouched by any of these, so the conventions persist.
        let tmp2 = TempDir::new().unwrap();
        write_override(tmp2.path(), FILE_WORKFLOW, "# Custom Workflow\n\nX\n");
        write_override(
            tmp2.path(),
            FILE_AGENT_DELEGATION,
            "# Custom Routing\n\nY\n",
        );
        write_override(tmp2.path(), FILE_INSTRUCTIONS, "# Project Rules\n\nZ\n");
        let prompt2 = resolve_pm_prompt(tmp2.path());
        for marker in MARKERS {
            assert!(
                prompt2.contains(marker),
                "section-override prompt must still carry {marker:?}"
            );
        }

        // Full-PM-replacement branch: every bundled body section is discarded,
        // but the non-overridable floor — and therefore these conventions —
        // must still be appended last.
        let tmp3 = TempDir::new().unwrap();
        write_override(
            tmp3.path(),
            FILE_PM_DEPLOYED,
            "# Wholly Custom PM\n\nDO_THIS\n",
        );
        let prompt3 = resolve_pm_prompt(tmp3.path());
        for marker in MARKERS {
            assert!(
                prompt3.contains(marker),
                "full-replacement prompt must still carry {marker:?} via the floor"
            );
        }
        let body = prompt3.find("DO_THIS").expect("custom body");
        let base = prompt3.find("# BASE_PM Framework Floor").expect("base");
        assert!(body < base, "floor (and its conventions) must come last");
    }
}

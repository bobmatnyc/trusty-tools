//! Composes the PM system prompt, and detects retired override files.
//!
//! Why: the project's customization surface is named sections in its root
//! `CLAUDE.md`, and nothing else (#4286). Five files under
//! `<project>/.trusty-mpm/` used to be a second, higher-precedence surface with
//! whole-document granularity. They are RETIRED: not read, not honoured. The
//! retirement had to be loud rather than silent — a project whose rules stop
//! reaching the PM with no error is the worst outcome available — so this module
//! also owns the detector both halves of that signal share.
//!
//! What: [`resolve_pm_prompt`] composes the effective PM prompt. It scans
//! `CLAUDE.md` for named-section overrides via
//! [`crate::core::claude_md_sections`], folds in the auto-derived stack profile
//! and the live agent roster, and *always* appends the non-overridable framework
//! floor last. Two composers exist and the roster alone selects between them: with
//! a roster the typed [`crate::core::instruction_package::InstructionPackage`]
//! composes every section; with none (no agent deployed in any tier) it degrades
//! to [`assemble_sections`], where only `WORKFLOW`, `MEMORY` and
//! `AGENT-DELEGATION` are independently addressable and any other named override
//! is reported by `warn_unapplied` rather than dropped in silence.
//!
//! [`detect_legacy_overrides`] reports leftover retired files;
//! [`warn_legacy_overrides`] logs them on every resolution, and the
//! `legacy_overrides` `tm doctor` check fails on them.
//!
//! Naming note: `base_pm()` and the `# Framework Instructions` heading are
//! historical labels. `BASE_PM.md` the FILE was deleted by #4183 and
//! `scripts/check_instruction_floor.sh` fails the build if it returns; the floor
//! is authored as four `fixed`-tier sections and reconstituted by that function.
//!
//! Test: the `tests` module inverts one gate per retired file (a retired file
//! must change nothing), pins the survival set across every reachable
//! configuration, and covers the detector; `claude_md_sections_tests.rs` covers
//! the named-section wiring end to end.

use std::path::Path;

use crate::core::claude_md_sections::ProjectOverrides;
use crate::core::instruction_package::SectionId;
use crate::core::instruction_pipeline::{
    AGENT_DELEGATION, SECTION_SEPARATOR, base_pm, pm_instructions,
};

/// Directory under the project root that holds the override files.
///
/// Why: the override files live alongside the inspectable instruction stash
/// (`last-instructions.md`) that `prepare_session` already writes, so they share
/// the project-local `.trusty-mpm/` directory documented in `BASE_PM.md`.
/// What: the literal directory name, joined onto the project root.
/// Test: every `resolve_*` test writes files under `<project>/.trusty-mpm/`.
pub const OVERRIDE_DIR_NAME: &str = ".trusty-mpm";

// RETIRED OVERRIDE FILE NAMES (#4286).
//
// These five names are no longer an override surface. Nothing below reads them
// as instruction content any more — they are retained ONLY so the retirement is
// DETECTABLE: [`detect_legacy_overrides`] reports a leftover file, and both the
// prompt resolver and the `legacy_overrides` doctor check turn that report into
// a loud, actionable signal. Deleting the constants would make a leftover file
// silent, which is the one outcome the retirement may not produce.

/// Retired: formerly a full replacement of the PM instruction body.
pub const FILE_PM_DEPLOYED: &str = "PM_INSTRUCTIONS_DEPLOYED.md";
/// Retired: formerly replaced the bundled `AGENT_DELEGATION` section.
pub const FILE_AGENT_DELEGATION: &str = "AGENT_DELEGATION.md";
/// Retired: formerly replaced the bundled `WORKFLOW` section.
pub const FILE_WORKFLOW: &str = "WORKFLOW.md";
/// Retired: formerly replaced the memory guidance section.
pub const FILE_MEMORY: &str = "MEMORY.md";
/// Retired: formerly appended (additive) project rules.
pub const FILE_INSTRUCTIONS: &str = "INSTRUCTIONS.md";

/// Every retired override file name, in former precedence order.
///
/// Why: the detector and the doctor check must agree on the set exactly. Two
/// hand-maintained lists would drift, and a name missing from one of them is a
/// file that gets silently ignored — the failure mode #4286 exists to end.
/// What: the five names, joined onto `<project>/.trusty-mpm/`.
/// Test: `legacy_override_files_are_the_five_retired_names`.
pub const LEGACY_OVERRIDE_FILES: [&str; 5] = [
    FILE_PM_DEPLOYED,
    FILE_AGENT_DELEGATION,
    FILE_WORKFLOW,
    FILE_MEMORY,
    FILE_INSTRUCTIONS,
];

/// The migration sentence every leftover-file signal must carry.
///
/// Why: a warning that says only "this file is ignored" leaves the operator
/// with a dead file and no next step. One shared constant keeps the resolver's
/// log line and the doctor check's message from drifting into two different
/// pieces of advice.
/// What: the one-line migration, naming `CLAUDE.md` and the marker grammar.
/// Test: `legacy_file_signal_names_the_migration`,
/// `legacy_overrides_fail_names_the_migration`.
pub const LEGACY_MIGRATION_HINT: &str = "move project facts into CLAUDE.md as plain prose, and section overrides into a \
     `<!-- TRUSTY-MPM: <SECTION> START v=1 -->` … `<!-- TRUSTY-MPM: <SECTION> END -->` \
     block in CLAUDE.md, then delete the file (#4286)";

/// Report which retired override files a project still carries.
///
/// Why: the hard cut (#4286) stops READING these files, and a silent stop would
/// delete a project's rules with no error — the unacceptable outcome. Detection
/// is therefore a first-class, testable function rather than an incidental log
/// line, and it is shared by the resolver and by `tm doctor` so the two can
/// never disagree about whether a project is affected.
/// What: the existing `<project>/.trusty-mpm/<name>` paths, in
/// [`LEGACY_OVERRIDE_FILES`] order. A missing `.trusty-mpm/` directory yields an
/// empty vector. Presence only — the contents are never read.
/// Test: `no_legacy_files_detected_in_a_clean_project`,
/// `every_retired_file_is_detected`.
pub fn detect_legacy_overrides(project_dir: &Path) -> Vec<std::path::PathBuf> {
    let dir = project_dir.join(OVERRIDE_DIR_NAME);
    LEGACY_OVERRIDE_FILES
        .iter()
        .map(|name| dir.join(name))
        .filter(|path| path.is_file())
        .collect()
}

/// Emit the resolver-side loud signal for any leftover retired override file.
///
/// Why: `tm doctor` only fires when someone runs it. The prompt resolver runs on
/// EVERY session launch and on every `tm session instructions`, so it is the one
/// place guaranteed to observe the condition at the moment it matters. `error!`
/// rather than `warn!` deliberately: the project's instructions are no longer
/// reaching the PM, which is a broken configuration, not a style note.
/// What: one `tracing::error!` per leftover file, naming the path and
/// [`LEGACY_MIGRATION_HINT`]. Silent when the project is clean.
/// Test: `legacy_file_signal_names_the_migration`.
fn warn_legacy_overrides(project_dir: &Path) {
    for path in detect_legacy_overrides(project_dir) {
        tracing::error!(
            path = %path.display(),
            migration = LEGACY_MIGRATION_HINT,
            "RETIRED override file is NO LONGER READ (#4286): its contents do not reach \
             the PM prompt"
        );
    }
}

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
/// #4399: within branch (2), "WORKFLOW (override or bundled)" and its two
/// siblings each additionally fall back to a CLAUDE.md named-section override
/// for that SAME section before reaching the bundled default — so a legacy
/// file that forces this branch for one section (say `AGENT_DELEGATION.md`)
/// can no longer shadow a CLAUDE.md override authored for a DIFFERENT,
/// unrelated section (say `WORKFLOW`). A legacy file still wins over a named
/// override for its OWN section — that collision is deliberate and unchanged.
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
/// the robustness tests, and (#4399, in `claude_md_sections_tests.rs`)
/// `an_unrelated_legacy_file_does_not_shadow_a_named_section_override`,
/// `a_same_section_legacy_file_still_wins_over_a_named_override_and_is_reported`,
/// `an_unrelated_legacy_file_does_not_shadow_a_named_memory_override`,
/// `an_unrelated_legacy_file_does_not_shadow_a_named_delegation_override`,
/// `identity_core_and_search_stay_reported_unapplied_on_the_legacy_path`.
pub fn resolve_pm_prompt(project_dir: &Path) -> String {
    let (prompt, source) = resolve_pm_prompt_with_source(project_dir);
    // `info!`, deliberately: this is the operator-visible record of WHICH
    // composer produced the session's prompt, and the two are byte-identical by
    // contract so nothing else can reveal it. `debug!` sits below the default
    // filter in both subscriber setups, which would make the claim false as
    // shipped. Matches `delegation_authority`'s analogous roster-source event.
    tracing::info!(?source, "resolved the PM system prompt");
    prompt
}

/// Which composer produced a resolved prompt.
///
/// Why: the two composers are byte-identical by contract (#4183), so the
/// delivered string cannot tell you which one ran. That is exactly the property
/// that let a call-site mutation survive a green suite — a test asserting
/// byte-equality through [`resolve_pm_prompt`] would keep passing even if the
/// package branch were deleted outright. Reporting the source turns "the
/// composed path was actually taken" into an assertable fact, and gives the
/// operator a log line saying which model produced their prompt.
/// What: [`PromptSource::Package`] for the re-sourced bundled fallback,
/// [`PromptSource::Legacy`] for the two override configurations and for the
/// compose-error degradation.
/// Test: `resolve_pm_prompt_takes_the_package_path_when_a_roster_is_deployed`,
/// `workflow_override_forces_the_legacy_path_even_with_a_roster`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptSource {
    /// Composed via [`crate::core::bundled_pm_package`] / `InstructionPackage`.
    Package,
    /// Assembled by the legacy string concatenation.
    Legacy,
}

/// [`resolve_pm_prompt`], additionally reporting which composer ran.
///
/// Why: see [`PromptSource`] — byte-identical outputs make the path
/// unobservable from the result alone, so the seam is what keeps the acceptance
/// gate anchored to the branch it claims to cover.
/// What: the resolved prompt plus its source, with the deployed-agent roster
/// scanned from the real tiers.
/// Test: `resolve_pm_prompt_takes_the_package_path_when_a_roster_is_deployed`,
/// `workflow_override_forces_the_legacy_path_even_with_a_roster`.
pub(crate) fn resolve_pm_prompt_with_source(project_dir: &Path) -> (String, PromptSource) {
    resolve_pm_prompt_with_roster(project_dir, || {
        crate::core::delegation_authority::deployed_roster_section(project_dir)
    })
}

/// [`resolve_pm_prompt_with_source`] with the agent roster supplied by the
/// caller.
///
/// Why: the roster is the one composition input this function cannot control
/// and cannot re-derive. [`crate::core::delegation_authority::deployed_roster_section`]
/// unions three tiers — the project tier, `$CLAUDE_CONFIG_DIR/agents` and
/// `~/.claude/agents` — on EVERY call, and the latter two are machine-global
/// mutable state that live `tm` sessions rewrite at launch; `scan_agents` also
/// drops a file silently on a transient read failure. Two successive scans can
/// therefore disagree. A byte-equality test that scans once per side is
/// consequently racing, and it fails with a message indistinguishable from a
/// genuine delivered-prompt regression — observed at ~1 red in 4 full-suite
/// runs on a provisioned workstation, and invisible in CI, where no ambient
/// tier exists. Injecting the roster gives both sides of that comparison ONE
/// value without touching `$HOME` (which would cost the serial `HOME_LOCK`) and
/// without altering the wiring under test.
///
/// What: identical to [`resolve_pm_prompt_with_source`] except that
/// `roster_source` supplies the rendered `## Delegation Authority` block. It is
/// a closure, not a value, so it stays LAZY: an `AGENT_DELEGATION.md` override
/// short-circuits before any scan, exactly as before.
/// Test: `resolve_pm_prompt_is_byte_identical_to_the_legacy_assembly`,
/// `resolve_pm_prompt_is_byte_identical_with_a_project_addendum`,
/// `gate_is_deterministic_across_repeated_runs`.
pub(crate) fn resolve_pm_prompt_with_roster(
    project_dir: &Path,
    roster_source: impl FnOnce() -> Option<String>,
) -> (String, PromptSource) {
    // #4286: named sections in `CLAUDE.md` are now the ONLY project override
    // surface. Scanned first because every branch below consumes the result.
    let named = crate::core::claude_md_sections::scan_project(project_dir);
    named.log();

    // #4286: the five `.trusty-mpm/` files are no longer read. A project that
    // still carries one gets a loud resolver-side error naming the migration —
    // never silence. `tm doctor`'s `legacy_overrides` check is the second half
    // of the same signal.
    warn_legacy_overrides(project_dir);

    // Per-project stack profile derived from detected marker files (#1971).
    // Auto-derived framework context, not a user override.
    let stack = crate::core::stack_profile::stack_profile_section(project_dir);

    let roster = roster_source();

    // The packaged composer is now the ordinary path for every project: with no
    // legacy file able to force the string assembly, the only remaining gate is
    // whether a roster exists for the `agent-roster` generator block to consume
    // (`RosterNotConsumed` forbids composing without one).
    if let Some(roster) = roster.as_deref() {
        let (composed, rejected) =
            crate::core::bundled_pm_package::compose_bundled_fallback_with_overrides(
                &stack,
                roster,
                // #4286: `.trusty-mpm/INSTRUCTIONS.md` was the only production
                // source for the optional `project-addendum` block. With it
                // retired the generator is never fed; the block is `optional`
                // in the manifest, so it simply does not emit.
                None,
                &named.overrides,
            );
        for rejection in &rejected {
            tracing::warn!(
                %rejection,
                "named-section override declined; keeping the bundled section"
            );
        }
        match composed {
            Ok(prompt) => return (prompt, PromptSource::Package),
            // Unreachable for the shipped assets — `shipped_assets_build_and_
            // validate` proves it — so this is a loud last resort, never a
            // routine fallback. The string assembly below composes the same
            // configuration, so degrading still delivers a correct prompt.
            Err(err) => tracing::error!(
                %err,
                "bundled PM instruction package failed to compose; \
                 falling back to the legacy assembly"
            ),
        }
    }

    // Roster-absent degradation (no agent deployed in any tier) and the
    // compose-error last resort. The string assembly can address `WORKFLOW`,
    // `MEMORY` and `AGENT-DELEGATION` independently, so named overrides for
    // those three still land here; `IDENTITY`/`CORE`/`SEARCH` live inside the
    // opaque `pm_instructions()` blob and are reported unapplied rather than
    // dropped in silence.
    let named_section = |id: SectionId| {
        named
            .overrides
            .iter()
            .find(|o| o.section == id)
            .map(|o| o.body.clone())
    };

    let delegation = delegation_with_named_override(
        named_section(SectionId::AgentDelegation).as_deref(),
        roster.as_deref(),
    );
    let workflow = named_section(SectionId::Workflow)
        .unwrap_or_else(|| crate::core::instruction_pipeline::workflow_section().to_string());
    let memory_override = named_section(SectionId::Memory);

    let unapplied = ProjectOverrides {
        overrides: named
            .overrides
            .iter()
            .filter(|o| {
                !matches!(
                    o.section,
                    SectionId::Workflow | SectionId::Memory | SectionId::AgentDelegation
                )
            })
            .cloned()
            .collect(),
        diagnostics: Vec::new(),
    };
    crate::core::claude_md_sections::warn_unapplied(&unapplied);
    (
        assemble_sections(stack, memory_override, workflow, delegation, None),
        PromptSource::Legacy,
    )
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
    let mut sections: Vec<String> = vec![pm_instructions().trim().to_string(), stack];

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
    sections.push(base_pm().trim().to_string());

    join_sections(sections)
}

/// The DEFAULT delegation section: bundled routing doctrine + the live roster.
///
/// THE ROSTER-PRECEDENCE NOTE, slotted between the doctrine and the roster,
/// resolves two ambiguities the append creates, both raised in review.
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
/// The note is a Markdown blockquote stating the roster wins on WHICH agents
/// exist, and to re-route rather than retry on an unknown-agent-type error. Since
/// #4318 its prose is authored in the instruction manifest
/// (`assets/instructions/pm-instruction-package.json`) as the delegation section's
/// second block, not as a Rust literal — so its position relative to the doctrine
/// and the roster is *declared*, and both composers read the same bytes without a
/// second copy. [`crate::core::instruction_pipeline::delegation_doctrine`]
/// projects doctrine-plus-note back out as one string.
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
/// What: returns the manifest's delegation doctrine (asset plus the precedence
/// note) with the rendered `## Delegation Authority` block from
/// [`crate::core::delegation_authority::deployed_roster_section`] appended when
/// any agent is deployed. With no deployed agents the asset is returned alone,
/// without the note — the note only makes sense above a roster (pre-#4069
/// behaviour). Takes the already-rendered roster rather than scanning itself
/// (#4183) so `resolve_pm_prompt` scans the agent tiers exactly once whichever
/// composition path it takes.
/// Test: `bundled_delegation_appends_deployed_roster`,
/// `no_overrides_uses_bundled`, `agent_delegation_override_replaces`.
pub(crate) fn delegation_with_roster(roster: Option<&str>) -> String {
    match roster {
        Some(roster) => format!(
            "{}\n\n{}",
            crate::core::instruction_pipeline::delegation_doctrine(),
            roster.trim()
        ),
        None => AGENT_DELEGATION.trim().to_string(),
    }
}

/// The AGENT-DELEGATION section body when no legacy `AGENT_DELEGATION.md` file
/// forced full replacement, honoring a CLAUDE.md named-section override if the
/// project authored one (#4399).
///
/// Why: an unrelated legacy file (`WORKFLOW.md`/`MEMORY.md`) can force
/// [`resolve_pm_prompt_with_roster`] onto this string-assembled branch even
/// though the project never touched `AGENT_DELEGATION.md`. Before #4399 that
/// meant a CLAUDE.md `AGENT-DELEGATION` override was silently dropped in favor
/// of [`delegation_with_roster`]'s bundled doctrine — the project's override
/// was declined for a reason that has nothing to do with delegation. This
/// mirrors [`crate::core::instruction_package::InstructionPackage::with_overrides`]'s
/// authored/generated split for the SAME section on the packaged path: the
/// override replaces the bundled doctrine (and its roster-precedence note),
/// while the live roster — host-computed, never authored by a project — still
/// follows when any agent is deployed.
///
/// What: `Some(body)` returns `body` (trimmed), followed by the roster when
/// `roster` is `Some`; `None` defers to [`delegation_with_roster`] unchanged.
/// Test: `an_unrelated_legacy_file_does_not_shadow_a_named_delegation_override`
/// (`claude_md_sections_tests.rs`).
fn delegation_with_named_override(named_override: Option<&str>, roster: Option<&str>) -> String {
    match named_override {
        Some(body) => match roster {
            Some(roster) => format!("{}\n\n{}", body.trim(), roster.trim()),
            None => body.trim().to_string(),
        },
        None => delegation_with_roster(roster),
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
        let base = prompt.find("# Framework Instructions").expect("base");
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
        assert!(prompt.contains("# Framework Instructions"));

        let base = prompt.find("# Framework Instructions").expect("base");
        let delegation = prompt.find("# Agent Delegation Routing").expect("deleg");
        assert!(base > delegation, "BASE_PM floor must be last");
    }

    // RETIREMENT GATES (#4286). Each of the six tests below was previously the
    // assertion that a `.trusty-mpm/` file DID override, and each is now
    // inverted to assert it does NOT. They are inverted rather than deleted on
    // purpose: an inverted test still fails if someone reinstates the read path,
    // whereas a deleted one would let the whole mechanism come back unnoticed.

    #[test]
    fn a_retired_instructions_file_no_longer_reaches_the_prompt() {
        // Was `instructions_appended`. This is the file that actually exists in
        // the field, so it is the retirement's highest-blast-radius case.
        let tmp = TempDir::new().unwrap();
        write_override(
            tmp.path(),
            FILE_INSTRUCTIONS,
            "# Project Rules\n\nALWAYS_RUN_MAKE_CHECK\n",
        );
        let prompt = resolve_pm_prompt(tmp.path());

        assert!(
            !prompt.contains("ALWAYS_RUN_MAKE_CHECK"),
            "a retired INSTRUCTIONS.md must not contribute to the prompt"
        );
        // The bundled sections are unaffected.
        assert!(prompt.contains("# PM Agent -- Trusty MPM"));
        assert!(prompt.contains("# Agent Delegation Routing"));
        assert!(prompt.contains("# Framework Instructions"));
    }

    #[test]
    fn a_retired_workflow_file_no_longer_replaces_the_workflow_section() {
        // Was `workflow_override_replaces`. The bundled workflow heading now
        // SURVIVES, which is the exact inversion.
        let tmp = TempDir::new().unwrap();
        write_override(
            tmp.path(),
            FILE_WORKFLOW,
            "# Custom Workflow\n\nTWO_PHASE_ONLY\n",
        );
        let prompt = resolve_pm_prompt(tmp.path());

        assert!(!prompt.contains("TWO_PHASE_ONLY"));
        assert!(
            prompt.contains("# PM Workflow Configuration"),
            "the bundled workflow section must survive a retired WORKFLOW.md"
        );
    }

    #[test]
    fn a_retired_delegation_file_no_longer_suppresses_the_live_roster() {
        // Was `agent_delegation_override_replaces`. This inversion also closes
        // the #4069 defect by construction: the retired file can no longer
        // suppress the live roster, because it is not read at all.
        let tmp = TempDir::new().unwrap();
        deploy_agent(tmp.path(), "ticketing");
        write_override(
            tmp.path(),
            FILE_AGENT_DELEGATION,
            "# Custom Routing\n\nROUTE_ALL_TO_ENGINEER\n",
        );
        let prompt = resolve_pm_prompt(tmp.path());

        assert!(!prompt.contains("ROUTE_ALL_TO_ENGINEER"));
        assert!(prompt.contains("# Agent Delegation Routing"));
        assert!(
            prompt.contains("### ticketing"),
            "the live roster must reach the prompt — a retired file cannot suppress it"
        );
    }

    #[test]
    fn a_retired_memory_file_no_longer_slots_a_memory_block() {
        // Was `memory_override_is_slotted_after_pm_instructions`.
        let tmp = TempDir::new().unwrap();
        write_override(
            tmp.path(),
            FILE_MEMORY,
            "Recall from the `team` palace before any task.\n",
        );
        let prompt = resolve_pm_prompt(tmp.path());

        assert!(!prompt.contains(MEMORY_OVERRIDE_HEADING));
        assert!(!prompt.contains("Recall from the `team` palace"));
        assert!(prompt.contains("# PM Agent -- Trusty MPM"));
    }

    #[test]
    fn a_retired_deployed_file_no_longer_replaces_the_body() {
        // Was `pm_deployed_replaces_body_but_keeps_base_floor`. This was the
        // most destructive of the five — a full-body replacement — so the
        // inversion asserts every bundled section is back.
        let tmp = TempDir::new().unwrap();
        write_override(
            tmp.path(),
            FILE_PM_DEPLOYED,
            "# Wholly Custom PM\n\nDO_EXACTLY_THIS\n",
        );
        let prompt = resolve_pm_prompt(tmp.path());

        assert!(!prompt.contains("DO_EXACTLY_THIS"));
        assert!(prompt.contains("# PM Agent -- Trusty MPM"));
        assert!(prompt.contains("# PM Workflow Configuration"));
        assert!(prompt.contains("# Agent Delegation Routing"));
        assert!(prompt.contains("# Framework Instructions"));
        assert!(prompt.contains("## Trusty Tool Priority (Non-Overridable)"));
    }

    #[test]
    fn every_retired_file_present_at_once_changes_nothing() {
        // Was `pm_deployed_still_appends_instructions`. The strongest form of
        // the retirement claim: a project carrying ALL FIVE files receives
        // byte-for-byte the prompt of a project carrying none.
        let with = TempDir::new().unwrap();
        for name in LEGACY_OVERRIDE_FILES {
            write_override(with.path(), name, &format!("CONTENT_OF_{name}\n"));
        }
        let without = TempDir::new().unwrap();

        let (with_prompt, _) =
            resolve_pm_prompt_with_roster(with.path(), || Some("## Delegation Authority".into()));
        let (without_prompt, _) = resolve_pm_prompt_with_roster(without.path(), || {
            Some("## Delegation Authority".into())
        });

        assert_eq!(
            with_prompt, without_prompt,
            "the five retired files must have zero effect on the composed prompt"
        );
    }

    #[test]
    fn legacy_override_files_are_the_five_retired_names() {
        // Pins the detector's set against the individual constants, so adding a
        // sixth name to one place and not the other is a red test rather than a
        // file that is silently never detected.
        assert_eq!(
            LEGACY_OVERRIDE_FILES,
            [
                "PM_INSTRUCTIONS_DEPLOYED.md",
                "AGENT_DELEGATION.md",
                "WORKFLOW.md",
                "MEMORY.md",
                "INSTRUCTIONS.md",
            ]
        );
    }

    #[test]
    fn no_legacy_files_detected_in_a_clean_project() {
        let tmp = TempDir::new().unwrap();
        assert!(detect_legacy_overrides(tmp.path()).is_empty());
        // A `.trusty-mpm/` directory holding only non-override state is clean.
        fs::create_dir_all(tmp.path().join(OVERRIDE_DIR_NAME)).unwrap();
        fs::write(
            tmp.path()
                .join(OVERRIDE_DIR_NAME)
                .join("last-instructions.md"),
            "composed",
        )
        .unwrap();
        assert!(detect_legacy_overrides(tmp.path()).is_empty());
    }

    #[test]
    fn every_retired_file_is_detected() {
        let tmp = TempDir::new().unwrap();
        for name in LEGACY_OVERRIDE_FILES {
            write_override(tmp.path(), name, "x\n");
        }
        let found = detect_legacy_overrides(tmp.path());
        assert_eq!(found.len(), LEGACY_OVERRIDE_FILES.len());
        for name in LEGACY_OVERRIDE_FILES {
            assert!(
                found.iter().any(|p| p.ends_with(name)),
                "detector missed {name}"
            );
        }
    }

    #[test]
    fn legacy_file_signal_names_the_migration() {
        // The migration hint is what turns a failure into an actionable one; it
        // must name the destination surface AND the marker grammar.
        assert!(LEGACY_MIGRATION_HINT.contains("CLAUDE.md"));
        assert!(LEGACY_MIGRATION_HINT.contains("TRUSTY-MPM:"));
        assert!(LEGACY_MIGRATION_HINT.contains("#4286"));
    }

    #[test]
    fn missing_override_dir_uses_bundled() {
        // A `.trusty-mpm/` directory that does not exist is not an error.
        let tmp = TempDir::new().unwrap();
        assert!(!tmp.path().join(OVERRIDE_DIR_NAME).exists());
        let prompt = resolve_pm_prompt(tmp.path());
        assert!(prompt.contains("# PM Agent -- Trusty MPM"));
        assert!(prompt.contains("# Framework Instructions"));
    }

    #[test]
    fn a_directory_in_a_retired_files_place_is_not_detected_and_not_fatal() {
        // Was `unreadable_override_falls_back`. The detector uses `is_file`, so
        // a directory sitting where a retired file would be is NOT reported —
        // reporting it would send the operator to migrate a directory. The
        // launch must also still succeed.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(OVERRIDE_DIR_NAME);
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir(dir.join(FILE_WORKFLOW)).unwrap();

        assert!(detect_legacy_overrides(tmp.path()).is_empty());
        let prompt = resolve_pm_prompt(tmp.path());
        assert!(prompt.contains("# PM Workflow Configuration"));
        assert!(prompt.contains("# Framework Instructions"));
    }

    #[test]
    fn an_empty_retired_file_is_still_detected() {
        // Was `empty_override_falls_back`. Emptiness used to mean "no override";
        // it now means nothing at all for composition, but the file must still
        // be REPORTED — an empty leftover is still a file the operator has to
        // delete, and staying silent about it leaves `tm doctor` green with a
        // retired file on disk.
        let tmp = TempDir::new().unwrap();
        write_override(tmp.path(), FILE_WORKFLOW, "   \n\t\n");

        assert_eq!(detect_legacy_overrides(tmp.path()).len(), 1);
        let prompt = resolve_pm_prompt(tmp.path());
        assert!(prompt.contains("# PM Workflow Configuration"));
        assert!(prompt.contains("# Framework Instructions"));
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
        let base = prompt.find("# Framework Instructions").expect("base");
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

        // CLAUDE.md named-section overrides of every PROJECT-tier section at
        // once — the maximal customization a project can now express. The floor
        // is `fixed` tier, so none of these can touch it. This replaces the
        // former "all five legacy files" and "full PM replacement" arms, which
        // no longer exist as reachable configurations (#4286).
        let tmp2 = TempDir::new().unwrap();
        std::fs::write(
            tmp2.path().join("CLAUDE.md"),
            "<!-- TRUSTY-MPM: CORE START v=1 -->\nCUSTOM_CORE\n<!-- TRUSTY-MPM: CORE END -->\n\n\
             <!-- TRUSTY-MPM: MEMORY START v=1 -->\nCUSTOM_MEMORY\n<!-- TRUSTY-MPM: MEMORY END -->\n\n\
             <!-- TRUSTY-MPM: SEARCH START v=1 -->\nCUSTOM_SEARCH\n<!-- TRUSTY-MPM: SEARCH END -->\n\n\
             <!-- TRUSTY-MPM: WORKFLOW START v=1 -->\nCUSTOM_WORKFLOW\n<!-- TRUSTY-MPM: WORKFLOW END -->\n\n\
             <!-- TRUSTY-MPM: AGENT-DELEGATION START v=1 -->\nCUSTOM_ROUTING\n<!-- TRUSTY-MPM: AGENT-DELEGATION END -->\n",
        )
        .unwrap();
        let (prompt2, _) =
            resolve_pm_prompt_with_roster(tmp2.path(), || Some("## Delegation Authority".into()));
        for marker in MARKERS {
            assert!(
                prompt2.contains(marker),
                "a fully-overridden prompt must still carry {marker:?} via the floor"
            );
        }
        // The overrides really did land — otherwise the assertions above would
        // pass vacuously against an unmodified bundled prompt.
        assert!(prompt2.contains("CUSTOM_WORKFLOW"));
        assert!(prompt2.contains("CUSTOM_ROUTING"));

        // A project that ALSO carries all five retired files must be no
        // different: the floor survives and the retired content never appears.
        let tmp3 = TempDir::new().unwrap();
        for name in LEGACY_OVERRIDE_FILES {
            write_override(tmp3.path(), name, "RETIRED_CONTENT\n");
        }
        let prompt3 = resolve_pm_prompt(tmp3.path());
        for marker in MARKERS {
            assert!(
                prompt3.contains(marker),
                "a prompt for a project with retired files must still carry {marker:?}"
            );
        }
        assert!(
            !prompt3.contains("RETIRED_CONTENT"),
            "retired files must not contribute"
        );
        // The other half of the survival set the floor guarantees.
        assert!(prompt3.contains("## Trusty Tool Priority (Non-Overridable)"));
    }
}

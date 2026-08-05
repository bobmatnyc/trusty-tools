//! Unit tests for [`super`] — the PM prompt composer and the retired-override
//! detector.
//!
//! Split out of `instruction_overrides.rs` by #4286: restoring coverage for the
//! roster-absent memory slot and the unapplied-section report pushed the parent
//! file to 538 SLOC, over the 500 production cap. Same `#[path]` split
//! `claude_md_sections.rs` already uses.

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
        prompt.contains("**Model:** sonnet"),
        "the roster's own value — the model hint — must reach the prompt"
    );
    assert!(
        !prompt.contains("Handles ticketing work."),
        "the agent `description` is already published by the harness's Agent-type \
         catalog; re-emitting it as `Handles:` made the roster a second copy"
    );

    // The bundled routing doctrine is APPENDED to, never replaced: the roster
    // carries no make/mise or keyword routing rules, which live in the single
    // Routing Table the six former routing surfaces were collapsed into.
    assert!(prompt.contains("# Agent Delegation Routing"));
    assert!(prompt.contains("## Routing Table"));
    assert!(prompt.contains("every `make` and `mise run` target"));

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
    let (without_prompt, _) =
        resolve_pm_prompt_with_roster(without.path(), || Some("## Delegation Authority".into()));

    assert_eq!(
        with_prompt, without_prompt,
        "the five retired files must have zero effect on the composed prompt"
    );
}

#[test]
fn a_named_memory_override_is_slotted_on_the_roster_absent_path() {
    // COVERAGE RESTORED (#4286). `MEMORY_OVERRIDE_HEADING` is still live —
    // `assemble_sections` slots a MEMORY override as a delimited block — but
    // its only test drove it through the retired `.trusty-mpm/MEMORY.md`
    // file. Retiring that file left the behaviour real and untested. The
    // surviving way to reach it is a CLAUDE.md `MEMORY` block on the
    // roster-absent path, which is what this covers.
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("CLAUDE.md"),
        "<!-- TRUSTY-MPM: MEMORY START v=1 -->\n\
         Recall from the `team` palace before any task.\n\
         <!-- TRUSTY-MPM: MEMORY END -->\n",
    )
    .unwrap();

    let (prompt, source) = resolve_pm_prompt_with_roster(tmp.path(), || None);
    assert_eq!(source, PromptSource::Legacy, "no roster -> string assembly");

    assert!(prompt.contains(MEMORY_OVERRIDE_HEADING));
    assert!(prompt.contains("Recall from the `team` palace"));

    let pm = prompt.find("# PM Agent -- Trusty MPM").expect("pm");
    let mem = prompt.find(MEMORY_OVERRIDE_HEADING).expect("mem");
    let wf = prompt
        .find("# PM Workflow Configuration")
        .expect("bundled workflow");
    assert!(pm < mem, "the memory block follows PM_INSTRUCTIONS");
    assert!(mem < wf, "and precedes the workflow section");
}

#[test]
fn unaddressable_sections_are_reported_unapplied_on_the_roster_absent_path() {
    // COVERAGE RESTORED (#4286). The string assembly can address only
    // WORKFLOW, MEMORY and AGENT-DELEGATION. Overrides for any other section
    // are handed to `warn_unapplied` rather than dropped in silence — that
    // call is still live, and its test went with the #4399 cluster.
    //
    // Asserted in its observable form: the override body must NOT appear
    // (it could not be applied), while the bundled section survives.
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("CLAUDE.md"),
        "<!-- TRUSTY-MPM: SEARCH START v=1 -->\n\
         UNAPPLIED_SEARCH_BODY\n\
         <!-- TRUSTY-MPM: SEARCH END -->\n\n\
         <!-- TRUSTY-MPM: WORKFLOW START v=1 -->\n\
         APPLIED_WORKFLOW_BODY\n\
         <!-- TRUSTY-MPM: WORKFLOW END -->\n",
    )
    .unwrap();

    let (prompt, source) = resolve_pm_prompt_with_roster(tmp.path(), || None);
    assert_eq!(source, PromptSource::Legacy);

    assert!(
        !prompt.contains("UNAPPLIED_SEARCH_BODY"),
        "SEARCH has no slot in the string assembly and must not land"
    );
    // ...and the section it aimed at is still delivered from the bundle.
    assert!(prompt.contains("## Code Search Protocol (Context-First)"));
    // The addressable sibling in the same file still applies, which is what
    // makes the negative above a real limitation rather than a dropped file.
    assert!(prompt.contains("APPLIED_WORKFLOW_BODY"));
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

#[test]
fn one_surface_rule_lives_in_core_and_survives_an_override_attempt() {
    // The one-surface doctrine has to sit in CORE, not in the floor section
    // that documents the mechanics: CORE is the only `fixed`-tier section, so
    // it is the only place a project cannot delete. Stated anywhere else, a
    // project could override away the rule telling it not to override
    // elsewhere.
    const RULE: &[&str] = &[
        "## Customization Surface (ONE surface, always-on)",
        // Half 1 — one surface, every other channel banned.
        "`CLAUDE.md` and NOWHERE else",
        ".trusty-mpm/` override files",
        // Half 2 — the every-prompt admission test, and its corollary.
        "Needed on every prompt",
        "Needed only sometimes",
    ];

    let tmp = TempDir::new().unwrap();
    let prompt = resolve_pm_prompt(tmp.path());
    for marker in RULE {
        assert!(
            prompt.contains(marker),
            "bundled prompt must carry the one-surface rule {marker:?}"
        );
    }

    // A project that tries to replace CORE wholesale: the marker is declined,
    // so the rule stays in force. WORKFLOW is overridden alongside it purely to
    // prove the override machinery ran at all.
    let tmp2 = TempDir::new().unwrap();
    std::fs::write(
        tmp2.path().join("CLAUDE.md"),
        "<!-- TRUSTY-MPM: CORE START v=1 -->\n\
         Put overrides wherever you like.\nCUSTOM_CORE\n\
         <!-- TRUSTY-MPM: CORE END -->\n\n\
         <!-- TRUSTY-MPM: WORKFLOW START v=1 -->\nCUSTOM_WORKFLOW\n\
         <!-- TRUSTY-MPM: WORKFLOW END -->\n",
    )
    .unwrap();
    let (prompt2, _) =
        resolve_pm_prompt_with_roster(tmp2.path(), || Some("## Delegation Authority".into()));

    assert!(
        prompt2.contains("CUSTOM_WORKFLOW"),
        "override machinery ran"
    );
    assert!(
        !prompt2.contains("CUSTOM_CORE"),
        "a CORE marker must be declined"
    );
    for marker in RULE {
        assert!(
            prompt2.contains(marker),
            "a CORE override attempt must not remove {marker:?}"
        );
    }
}

//! Tests for the instruction merge pipeline and bundled prompt assembly.
//!
//! Split out of `instruction_pipeline.rs` (#4318) so the production file stays
//! under the 500-SLOC cap enforced by `scripts/check_line_cap.sh`. The module is
//! included with `#[path]`, so `use super::*` still reaches the pipeline's items
//! exactly as it did inline; nothing about the assertions changed in the move.

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
fn pm_instructions_is_its_three_sections() {
    // #4183: the legacy PM body is RECONSTITUTED, never kept as a fourth copy
    // on disk. #4318 moved the source of that reconstitution from the
    // `include_str!` constants to the MANIFEST, because the manifest may now
    // author a rule inline — rebuilding from the constants would deliver such a
    // rule to the packaged composer and not to the legacy override assembly,
    // which is the split-brain this asserts against.
    let body = pm_instructions();
    let projected = crate::core::bundled_pm_package::authored_run(&[
        SectionId::Core,
        SectionId::Memory,
        SectionId::Search,
    ])
    .expect("the manifest is readable");
    assert_eq!(body, format!("{projected}\n"));

    // Every section source is still delivered in full, in order, and the
    // manifest-authored rules ride along.
    for expected in [
        SECTION_CORE.trim(),
        SECTION_MEMORY.trim(),
        SECTION_SEARCH.trim(),
    ] {
        assert!(body.contains(expected), "a section source went missing");
    }
    assert!(body.contains("### Clickable References"));
    assert!(body.contains("### Banned Word"));

    // The paragraph break is `Join::Blank`'s literal, which is what makes
    // this string byte-identical to what the packaged composer emits.
    assert!(body.contains("## Memory Protocol (Context-First)"));
    assert!(body.contains("## Code Search Protocol (Context-First)"));
    assert!(
        !body.contains("## Context-First Protocol\n"),
        "the merged Memory+Search block must not survive as a fourth copy"
    );
}

#[test]
fn base_pm_is_its_four_sections() {
    // Same no-second-copy property for the non-overridable floor, plus the
    // one reordering #4183 makes: the tool-priority mandate travels with the
    // other non-overridable rules and so now precedes the conventions.
    //
    // #4573 added `enforcement` (Prohibitions + Circuit Breakers) as the second
    // floor section. Asserting it HERE — over `base_pm()`, the string every
    // legacy branch appends, including the `PM_INSTRUCTIONS_DEPLOYED.md` full
    // replacement — is what proves the tier change alone did not leave the
    // legacy paths without the authority tables.
    let floor = base_pm();
    assert_eq!(
        floor,
        format!(
            "{}\n\n{}\n\n{}\n\n{}\n",
            SECTION_IDENTITY.trim(),
            SECTION_ENFORCEMENT.trim(),
            SECTION_NON_OVERRIDABLE_RULES.trim(),
            SECTION_FRAMEWORK_CONVENTIONS.trim()
        )
    );

    let tool = floor
        .find("## Trusty Tool Priority (Non-Overridable)")
        .expect("the tool-priority mandate stays in the floor");
    let conventions = floor
        .find("## Framework-Guaranteed Conventions (Non-Overridable)")
        .expect("the guaranteed conventions stay in the floor");
    assert!(
        tool < conventions,
        "tool priority now rides with the non-overridable rules it belongs to"
    );
}

#[test]
fn workflow_section_carries_the_opportunistic_fix_rule() {
    // The opportunistic-fix rule (owner amendment on #4183) is authored in the
    // manifest, not in `workflow.md`, so the constant alone does not carry it.
    // Every consumer must therefore read the section through the manifest —
    // otherwise a project with no override and a project on the legacy path
    // would receive different workflows.
    let section = workflow_section();
    assert!(section.starts_with(WORKFLOW.trim()));
    assert!(section.contains("## Opportunistic Fixes"));
    assert!(section.contains("noted on the CURRENT issue"));
    assert!(
        !WORKFLOW.contains("## Opportunistic Fixes"),
        "the rule is manifest-authored; finding it in workflow.md means it was \
         duplicated"
    );
}

#[test]
fn delegation_doctrine_carries_the_precedence_note() {
    // #4318 moved the roster-precedence note out of a Rust literal and into the
    // manifest. The doctrine string every composer uses must still be the asset
    // followed by that note, in that order.
    let doctrine = delegation_doctrine();
    assert!(doctrine.starts_with(AGENT_DELEGATION.trim()));
    assert!(doctrine.ends_with("do not retry the same agent."));
    assert!(doctrine.contains("trust the roster"));
}

#[test]
fn the_installed_prompt_carries_the_manifest_authored_rules() {
    // `assemble_system_prompt` writes
    // `~/.trusty-mpm/framework/instructions/INSTRUCTIONS.md`, which `tm install`
    // and `tm launch` hand to `claude --append-system-prompt-file`. It has no
    // agent roster, so it cannot use the package composer — which is exactly why
    // it has to project the manifest rather than concatenate the constants. A
    // manifest-authored rule missing here is a rule half the launch paths never
    // see.
    let prompt = assemble_system_prompt();
    for marker in [
        "### Clickable References",
        "### Banned Word",
        "## Opportunistic Fixes",
    ] {
        assert!(
            prompt.contains(marker),
            "the installed prompt must carry {marker:?}"
        );
    }
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

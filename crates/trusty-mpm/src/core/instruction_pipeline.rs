//! Instruction merge pipeline — compose a session's launch instructions.
//!
//! Why: every Claude Code session trusty-mpm starts must receive the same
//! framework instructions and the dynamic delegation routing context; doing
//! that merge ad-hoc at each launch site invites drift in ordering and
//! content.
//! What: [`build_instructions`] resolves the project's agent roster, seeds a
//! project `CLAUDE.md` stub when absent — unless git says the upstream default
//! branch already tracks one, in which case the absence is branch staleness and
//! the launch is refused rather than stubbed (#5228) — and reports what it found;
//! [`compiled_prompt_path`] resolves where a session's compiled prompt lands and
//! [`write_compiled_prompt_to`] is the one writer of it.
//!
//! #4832 removed this pipeline's last text output. `build_instructions` used to
//! read `<framework_root>/instructions/INSTRUCTIONS.md` and concatenate it with
//! the delegation-authority section into `PipelineOutput::merged` — a field with
//! zero call sites, because the real launch prompt comes from
//! [`crate::core::instruction_overrides::resolve_pm_prompt`] /
//! [`crate::core::session_launch::build_system_prompt_for`]. #4752 had already
//! retired the file: nothing writes it, and `tm install` deletes a stale copy.
//! What survived was a read whose content was never used but whose failure —
//! anything other than `NotFound`: a permissions error, a directory at that
//! path, invalid UTF-8 — became `PrepError::Instructions`, the one variant
//! `is_fatal()` returns true for. It could only ever kill a session over unread
//! bytes, so the read, the field, and the merge helper are gone. The fatal gate
//! on the compiled-prompt WRITE is deliberate (#4752) and stays.
//! Test: `cargo test -p trusty-mpm instruction_pipeline` covers roster
//! resolution, stub creation, and compiled-prompt path/write behaviour.

use std::fmt;
use std::path::PathBuf;

use crate::core::delegation_authority::resolve_roster;
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
/// What: a linear scan of [`SECTION_SOURCES`] — nine entries, called a handful
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
/// Test: `pm_instructions_is_its_three_sections`, `base_pm_is_its_four_sections`.
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
/// Test: `base_pm_is_its_four_sections`, `floor_carries_the_tool_priority_mandate`.
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

/// Filename of the compiled PM system prompt.
///
/// Why (#4752): the compiled prompt needs a path NO other writer targets. It
/// previously shared `instructions/INSTRUCTIONS.md` with a bundled 4-line stub
/// and with the pipeline's own input file, so the on-disk answer to "what
/// instructions is my session running?" depended on which writer ran last —
/// the regression #383 already fixed once and #4752 closes structurally.
/// What: `INSTRUCTIONS-COMPILED.md`, resolved per SESSION by
/// [`compiled_prompt_path`] — never under the global framework root.
/// Test: `compiled_prompt_path_is_project_local`,
/// `compiled_prompt_path_is_not_the_bundled_instructions_path`.
pub const COMPILED_PROMPT_FILE: &str = "INSTRUCTIONS-COMPILED.md";

/// Resolve a session's compiled-prompt path.
///
/// Why (#4752, then #4832): the compiled prompt answers "what instructions is
/// MY session running", so it is per-SESSION output, not per-project config.
/// #4752 moved it off the shared `~/.trusty-mpm/framework/` file, where every
/// project overwrote every other; #4832 finishes the job on both remaining
/// axes. Project-scoping still let two concurrent sessions in one project
/// overwrite each other, so the file now sits under `sessions/<id>/`. And the
/// project directory a caller hands in is frequently a WORKTREE — resolving
/// against it gave one project a `.trusty-mpm/` per branch, which the owner
/// ruled out (a worktree carries code, config and docs; harness state belongs
/// to the project). [`crate::core::harness_root::session_dir`] resolves both.
///
/// This is why the accessor does NOT live on `FrameworkPaths`: that type models
/// the global framework INSTALL layout, and this file is per-session state.
/// What: `<harness-root>/.trusty-mpm/sessions/<session_id>/INSTRUCTIONS-COMPILED.md`,
/// where the harness root is the checkout that owns `project_dir` — the main
/// checkout, never a worktree of it.
/// Test: `compiled_prompt_path_is_project_local`,
/// `compiled_prompt_path_is_per_session`,
/// `compiled_prompt_path_never_lands_inside_a_worktree`.
pub fn compiled_prompt_path(project_dir: &std::path::Path, session_id: &str) -> std::path::PathBuf {
    crate::core::harness_root::session_dir(project_dir, session_id).join(COMPILED_PROMPT_FILE)
}

/// The operator-facing explanation for a failed instruction write.
///
/// Why (#4752): establishing a session's instructions is fatal — it refuses a
/// launch — so the operator must be told what was refused, where, and why, not
/// handed a bare `io::Error`. One formatter keeps that message identical
/// wherever the refusal surfaces: the CLI (`tm session start`), the daemon
/// resume path, the bare-`tm` in-place relaunch, or the HTTP client.
///
/// Renamed from `instructions_failure_message` (round 4): it now also covers
/// a [`build_instructions`] failure, so wording specific to the compiled prompt
/// would have been wrong at half its call sites.
/// What: names the path, the underlying cause, and the remedy.
/// Test: `instructions_failure_message_names_the_path_and_a_remedy`.
pub fn instructions_failure_message(path: &std::path::Path, source: &std::io::Error) -> String {
    // #5228: "could not write" was a false claim for the stale-worktree refusal
    // below — nothing is attempted there, the write is declined on purpose — and
    // the permissions/free-space remedy was wrong for it too. The wording now
    // states the condition (instructions not established) and defers the remedy
    // to `source`, which carries its own where a specific one exists.
    format!(
        "could not establish the session instructions at {}: {source}\n\
         The session was NOT started: it depends on those instructions. Resolve \
         the cause above — for a path `tm` could not write, check permissions and \
         free space — then retry.",
        path.display()
    )
}

/// Write an already-composed prompt to a project's compiled-prompt path.
///
/// Why (#4752): every writer of `INSTRUCTIONS-COMPILED.md` goes through one
/// function so the file can never hold anything but a full compiled prompt. It
/// takes the prompt the composer ALREADY built for
/// `--append-system-prompt-file` rather than recomposing it, so the launch cost
/// is one `write`, not a second composition pass.
/// What: creates `dest`'s parent directory if absent, then writes `prompt`
/// verbatim. Returns the underlying IO error unchanged; pair it with
/// [`instructions_failure_message`] when surfacing to an operator.
/// Test: `write_compiled_prompt_to_creates_parent_dirs`.
pub fn write_compiled_prompt_to(dest: &std::path::Path, prompt: &str) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, prompt)
}

/// Compose a project's prompt and refresh its compiled prompt on disk — fatally.
///
/// Why (#4752): THREE entry points hand a runtime a prompt, and each must
/// guarantee the compiled copy is current first — fresh start
/// ([`crate::core::session_launch::prepare_session`]), daemon resume
/// (`managed_routes::lifecycle::resume_managed`), and the bare-`tm` in-place
/// relaunch (`tm::commands::guided_inplace::run_inplace_relaunch`). Round 3 of
/// this issue missed the third: it calls neither of the other two, so its only
/// compiled write was `build_prompt_file`'s best-effort refresh. A resumed
/// session could therefore run on a stale or missing compiled prompt exactly
/// where a fresh launch would have refused.
///
/// SCOPE — this helper serves TWO of those three, not all three. The two
/// resume-shaped paths (daemon resume, in-place relaunch) share it because
/// neither has an explicit style to apply, so composing with `None` is right for
/// both. `prepare_session` deliberately does NOT call it: it must compose with
/// the `effective_style` it just resolved (flag > config > manifest), which this
/// helper hardcodes to `None`, and routing it through here would silently drop
/// the operator's chosen output style from the compiled copy. All three still
/// share the actual write via [`write_compiled_prompt_to`] and the same fatal
/// policy; what differs is only which text they compose.
/// What: composes the project-resolved prompt through the same seam the launcher
/// uses, with no explicit output style, writes it to [`compiled_prompt_path`],
/// and on failure returns the operator-facing string from
/// [`instructions_failure_message`] — callers refuse the launch with it.
/// Test: `refresh_compiled_prompt_writes_the_project_local_file`,
/// `refresh_compiled_prompt_reports_an_actionable_failure`.
pub fn refresh_compiled_prompt(
    project_dir: &std::path::Path,
    session_id: &str,
) -> Result<(), String> {
    let native = crate::core::output_style::claude_supports_native_output_style();
    let prompt = crate::core::session_launch::build_system_prompt_for_with_style_and_native(
        project_dir,
        None,
        native,
    );
    let dest = compiled_prompt_path(project_dir, session_id);
    write_compiled_prompt_to(&dest, &prompt)
        .map_err(|source| instructions_failure_message(&dest, &source))
}

/// Delete a pre-#4832 per-project compiled prompt and its `framework/` dir.
///
/// Why: #4832 moved the compiled prompt from
/// `<project>/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md` to
/// `<harness-root>/.trusty-mpm/sessions/<id>/INSTRUCTIONS-COMPILED.md`. An
/// upgraded install keeps the old file forever with no writer left to refresh
/// it, and it is the file an operator inspects to answer "what is my session
/// running" — a stale answer there is worse than no answer. Every worktree of
/// an upgraded project also holds one, since the old path resolved against the
/// worktree. Nothing but tm ever wrote it, so removing it takes nothing from
/// the operator. `manifest.toml` — the one operator-authored file that now
/// lives in `framework/` — is deliberately NOT touched: the directory is only
/// removed when it is empty.
/// What: removes `<project_dir>/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md`
/// if present, and ONLY THEN removes the now-possibly-empty `framework/`
/// directory (ignoring the not-empty error). Returns whether a file was
/// removed. The gate matters because this runs on every launch while
/// `tm project init` seeds an empty `framework/`; ungated, init created the
/// directory and the next launch deleted it. Resolves against `project_dir`
/// VERBATIM, not the harness root: the point is to clean the worktree-local
/// copies the old path created.
/// Test: `migrate_removes_a_legacy_compiled_prompt`,
/// `migrate_keeps_a_sibling_manifest`,
/// `migrate_is_a_noop_when_absent`,
/// `migrate_keeps_an_empty_framework_dir_when_nothing_migrated`.
pub fn remove_legacy_compiled_prompt(project_dir: &std::path::Path) -> std::io::Result<bool> {
    let framework = project_dir
        .join(crate::core::harness_root::HARNESS_DIR)
        .join(crate::core::harness_root::FRAMEWORK_DIR);
    let removed = match std::fs::remove_file(framework.join(COMPILED_PROMPT_FILE)) {
        Ok(()) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => return Err(err),
    };
    // Only when this call actually migrated something. `remove_dir` refuses a
    // non-empty directory, which preserves a sibling `manifest.toml` — but it
    // does NOT distinguish "empty leftover" from "empty because `tm project
    // init` just created it", and that scaffolding runs on init while this runs
    // on every launch. Ungated, init created the directory and the next launch
    // deleted it (#4841 review).
    if removed {
        let _ = std::fs::remove_dir(&framework);
    }
    Ok(removed)
}

/// Delete a stale `instructions/INSTRUCTIONS.md` left by a pre-#4752 install.
///
/// Why: until #4752, `tm install` wrote the compiled prompt to
/// `framework/instructions/INSTRUCTIONS.md`. Nothing writes that path anymore —
/// but [`build_instructions`] still READS it as an optional framework section,
/// so an upgraded machine would keep folding a frozen copy of an OLD compiled
/// prompt into [`PipelineOutput::merged`] forever, with no writer left to
/// refresh it. A stale input nothing can update is worse than an absent one, and
/// the pipeline already treats absence as normal (`instructions_loaded: false`).
/// The file is framework-owned — trusty-mpm has always overwritten it on every
/// install — so removing it takes nothing from the operator.
/// What: removes `dest` if it exists; returns whether a file was removed. A
/// `NotFound` race is reported as "nothing removed", not an error, so a
/// concurrent install cannot fail this one.
/// Test: `remove_stale_bundled_instructions_deletes_a_leftover`,
/// `remove_stale_bundled_instructions_is_a_noop_when_absent`.
pub fn remove_stale_bundled_instructions(dest: &std::path::Path) -> std::io::Result<bool> {
    match std::fs::remove_file(dest) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

/// The CLAUDE.md stub seeded into a project on first session start.
///
/// Why: the project needs a place for project-specific notes; trusty-mpm
/// creates it exactly once and then never touches it again, so the operator
/// can edit freely (issue #2170 — trusty-mpm must never modify a target
/// project's `CLAUDE.md`).
pub(crate) const CLAUDE_MD_STUB: &str = "# Project Instructions

<!-- trusty-mpm: created by `trusty-mpm session start` — customize for your project -->
<!-- trusty-mpm will never modify this file again after creating it. -->

## Project Context

<!-- Describe your project, tech stack, and any conventions the agent should know. -->

## Framework Instructions

This file is the ONLY surface for customizing the PM's instructions. To replace
a section of the framework prompt, put the replacement between a marker pair —
`<!-- TRUSTY-MPM: WORKFLOW START v=1 -->` on its own line, your text, then
`<!-- TRUSTY-MPM: WORKFLOW END -->` on its own line.

(Those markers are shown inline deliberately. A marker alone on a line IS the
mechanism, so a worked example here would take effect as a real override — see
`seeded_claude_md_declares_no_overrides`.)

Tokens: `IDENTITY`, `MEMORY`, `SEARCH`, `WORKFLOW`, `AGENT-DELEGATION`,
`ENFORCEMENT`, `NON-OVERRIDABLE-RULES`, `FRAMEWORK-GUARANTEED-CONVENTIONS`.
`CORE` is the one token that is always declined. Prose outside the markers is
project context — Claude Code loads it natively, so it is never copied into the
composed prompt.

The instructions actually delivered to this session are written to
`.trusty-mpm/last-instructions.md` on every launch. Read that file to see what
the PM received, including which of your overrides applied:

```
tm session instructions          # print it, with applied/declined markers on stderr
cat .trusty-mpm/last-instructions.md
```

## Commit & PR Attribution

This restatement exists so a `claude` session launched directly in this project
(outside `tm` orchestration) still sees the convention.

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
/// Why: bundles the source locations so callers pass one value instead of
/// several loosely-related paths.
/// What: the project the roster is resolved for, and the project `CLAUDE.md`.
/// #4832 removed `framework_instructions_path` with the read that consumed it.
/// Test: every `pipeline_*` test constructs one of these.
#[derive(Debug, Clone)]
pub struct PipelineInput {
    /// The project whose agent roster this session will receive.
    ///
    /// #4588: this replaced a single `agents_dir`. Callers used to name one
    /// directory to scan, which is how the printed count came to describe a
    /// different set of agents than the PM was given. The roster is resolved
    /// from the project by
    /// [`crate::core::delegation_authority::resolve_roster`], so there is no
    /// longer a directory for a caller to get wrong.
    pub project_dir: PathBuf,
    /// Path to the project `CLAUDE.md`.
    pub claude_md_path: PathBuf,
}

/// Result of a successful instruction merge.
///
/// Why: callers need the flags describing what happened (how many agents will
/// the PM get? was a stub created?) so they can report it to the operator.
/// What: per-source status flags. #4832 removed the `merged` text field with
/// the dead read that produced it — see the module docs.
/// Test: asserted by every `pipeline_*` test.
#[derive(Debug, Clone)]
pub struct PipelineOutput {
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
/// What: resolves the project's agent roster via
/// [`crate::core::delegation_authority::resolve_roster`] for its count, and
/// ensures the project `CLAUDE.md` exists (creating the stub if absent) purely
/// for its side effect.
///
/// #4832: this no longer reads `<framework_root>/instructions/INSTRUCTIONS.md`.
/// Nothing has written that file since #4752, its content fed only the unread
/// `PipelineOutput::merged`, and any read error other than `NotFound` became
/// the one fatal `PrepError` variant — a session refused over bytes nobody
/// used. See the module docs.
///
/// #5228: the seed is now conditional. A `CLAUDE.md` the upstream default
/// branch tracks but this working tree lacks means the branch is stale, not
/// that the project is new, so this returns `Err` — which
/// [`crate::core::session_launch::prepare_session`] treats as fatal — instead
/// of writing a stub that would stand in for every project instruction.
///
/// Test: `pipeline_full`, `pipeline_creates_claude_md`,
/// `pipeline_does_not_read_the_retired_framework_instructions`,
/// `build_instructions_refuses_a_stale_worktree_rather_than_seeding_a_stub`,
/// `session_start_count_matches_the_delivered_delegation_roster`
pub fn build_instructions(input: &PipelineInput) -> Result<PipelineOutput, PipelineError> {
    // #4588: resolved by `resolve_roster` — the SAME function that renders the
    // delegation section delivered to the PM — so `agent_count`, which
    // `tm session start` prints verbatim, cannot describe a different set of
    // agents than the PM received.
    let agent_count = resolve_roster(&input.project_dir).len();

    // Side effect only: ensure the project CLAUDE.md stub exists so a fresh
    // workspace always has a place for project notes. Claude Code memory-loads
    // `CLAUDE.md` natively and the real launch prompt is built by
    // `resolve_pm_prompt`/`build_system_prompt_for`, so the content is read
    // back only to report whether this call created it.
    let (_claude_md, claude_md_created) = load_or_create_claude_md(&input.claude_md_path)?;

    Ok(PipelineOutput {
        agent_count,
        claude_md_created,
    })
}

/// An upstream ref that tracks a file this working tree does not have (#5228).
///
/// What: the ref carrying the file (`origin/main`) and the branch checked out
/// here, both quoted back to the operator so the diagnosis names the actual
/// staleness rather than describing it in the abstract.
struct UpstreamTracked {
    /// The remote-tracking ref that has the file, e.g. `origin/main`.
    reference: String,
    /// The branch this working tree has checked out.
    branch: String,
}

/// Ask git whether `path` is absent only because this branch predates the
/// commit that added it upstream (#5228).
///
/// Why: `NotFound` alone cannot distinguish a genuinely new project from a
/// worktree cut before `CLAUDE.md` became tracked, and the two need opposite
/// handling — seed the stub, or refuse. git already knows: the remote-tracking
/// ref is local (no network, no fetch) and either carries the file or does not.
/// What: returns `Some` only when `path`'s directory is inside a git work tree
/// AND one of the candidate upstream refs — the branch's own `@{upstream}`,
/// `origin/HEAD`, then `origin/main` / `origin/master` — has a blob at `path`'s
/// repo-relative location. Every uncertain answer is `None`, so the caller
/// keeps its pre-#5228 behaviour wherever git cannot positively confirm the
/// staleness. Ordinary launches never reach this: the caller only probes when
/// the file is already known to be missing.
/// Test: `load_or_create_claude_md_refuses_to_stub_a_branch_predating_the_tracked_file`,
/// `load_or_create_claude_md_still_seeds_when_upstream_has_no_claude_md`.
fn upstream_tracking(path: &std::path::Path) -> Option<UpstreamTracked> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty())?;
    let name = path.file_name()?.to_str()?;
    if git_stdout(dir, &["rev-parse", "--is-inside-work-tree"])? != "true" {
        return None;
    }
    let branch = git_stdout(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap_or_else(|| "HEAD".to_string());

    let tracked = git_stdout(
        dir,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    );
    // `origin/HEAD` is resolved to the branch it points at so the message names
    // `origin/main`, not a symref the operator would have to dereference.
    let default_head = git_stdout(dir, &["rev-parse", "--abbrev-ref", "origin/HEAD"]);
    let candidates = tracked.into_iter().chain(default_head).chain(
        ["origin/main", "origin/master"]
            .into_iter()
            .map(String::from),
    );

    for reference in candidates {
        // `<rev>:./<name>` resolves `name` relative to `dir` inside the repo, so
        // this stays correct for a project that is not the repository root.
        if git_succeeds(dir, &["cat-file", "-e", &format!("{reference}:./{name}")]) {
            return Some(UpstreamTracked { reference, branch });
        }
    }
    None
}

/// The operator-facing explanation for a refused stub (#5228).
///
/// Why: the refusal stops a launch, so it owes the operator the diagnosis AND
/// the command that fixes it — recovery is one fast-forward away, and a bare
/// "file missing" would send them looking for the wrong problem.
/// What: names the file, the branch, the ref that has it, and two recovery
/// commands (bring the branch current, or take just the file).
/// Test: `stale_claude_md_refusal_names_the_branch_ref_and_recovery`.
fn stale_claude_md_message(path: &std::path::Path, upstream: &UpstreamTracked) -> String {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "CLAUDE.md".to_string());
    let UpstreamTracked { reference, branch } = upstream;
    format!(
        "{name} is absent from this working tree but tracked at {reference} — \
         branch `{branch}` predates the commit that added it. No stub was \
         written: a stub would replace every project instruction with \
         boilerplate, and once on disk it is indistinguishable from an authored \
         file to every later session. Bring the real file in with either:\n  \
         git -C {dir} merge --ff-only {reference}\n  \
         git -C {dir} checkout {reference} -- {name}",
        dir = dir.display()
    )
}

/// Run `git -C <dir> <args>`, returning trimmed stdout or `None` on any failure.
fn git_stdout(dir: &std::path::Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// Whether `git -C <dir> <args>` exited zero.
fn git_succeeds(dir: &std::path::Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .is_ok_and(|out| out.status.success())
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
///
/// #5228 — absent is not the same as new. When git says the upstream default
/// branch already tracks this file, its absence here means the branch predates
/// the commit that added it, and seeding the stub would hand the session
/// boilerplate in place of the project's real instructions. That failure is
/// invisible (the session cannot tell) and PERMANENT (every later run finds a
/// file present and takes the read path), so this refuses instead. The refusal
/// is fatal by the caller's own contract — `prepare_session` maps a
/// [`PipelineError`] onto `PrepError::Instructions`, the one condition #4752
/// rules must stop a launch — and the message carries the recovery command.
/// Detection is conservative: anything git cannot answer (no git, not a work
/// tree, no upstream ref, upstream has no such file) falls through to the
/// ordinary stub, so a genuinely new project is unaffected.
/// Test: `pipeline_creates_claude_md`, `pipeline_claude_md_left_byte_identical`,
/// `load_or_create_claude_md_refuses_to_stub_a_branch_predating_the_tracked_file`,
/// `build_instructions_refuses_a_stale_worktree_rather_than_seeding_a_stub`,
/// `load_or_create_claude_md_still_seeds_when_upstream_has_no_claude_md`,
/// `load_or_create_claude_md_reads_a_present_file_in_a_git_worktree`.
fn load_or_create_claude_md(path: &PathBuf) -> Result<(String, bool), PipelineError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok((text, false)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // #5228: a stale branch's missing CLAUDE.md must never be stubbed over.
            if let Some(upstream) = upstream_tracking(path) {
                return Err(PipelineError::Io {
                    path: path.clone(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        stale_claude_md_message(path, &upstream),
                    ),
                });
            }
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

// #4832: `merge_sections` lived here. It joined the framework `INSTRUCTIONS.md`
// text with the delegation-authority section into `PipelineOutput::merged`,
// which had zero call sites — both the field and its only producer are gone.

#[cfg(test)]
#[path = "instruction_pipeline_tests.rs"]
mod tests;

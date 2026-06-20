//! Session Manager (SM) system-prompt assembly + override layering (DOC-14 §4).
//!
//! Why: the SM carries its own role-specific system prompt, composed and
//! delivered with the same discipline as the PM prompt
//! ([`crate::core::instruction_pipeline`] /
//! [`crate::core::instruction_overrides`]) but one level up. The five sections
//! define the SM's behavior: identity, prohibitions SP1-SP7, allowlist; the
//! canonical harness mental model (DOC-21); the 6-phase delegation loop; the
//! BLOCKING verification gate; the tool/verb surface; and the non-overridable
//! framework floor. Assembling them ad-hoc at each call site would invite the
//! same ordering/content drift the PM pipeline was built to prevent.
//! What: [`assemble_sm_prompt`] embeds the four bundled assets via `include_str!`
//! at compile time and joins them with the shared harness-understanding content
//! from `trusty_agents_common::harness_doc` in the fixed order
//! SM_INSTRUCTIONS -> SM_HARNESS -> SM_WORKFLOW -> SM_TOOLS -> BASE_SM,
//! BASE_SM **always last** as the non-overridable floor. [`resolve_sm_prompt`]
//! layers optional per-file overrides from an override directory
//! (`~/.trusty-mpm/sm/`, see [`sm_override_dir`]) onto the bundled defaults,
//! **always** appending the bundled BASE_SM floor last -- BASE_SM is never
//! overridable.
//! Test: the `tests` module mirrors `instruction_overrides::tests` -- bundled
//! content, per-file override replacement, the never-overridable BASE_SM floor
//! invariant, and the missing/empty/unreadable fallbacks.
//!
//! Crucial distinction from the PM (spec §4): the SM prompt is **not** delivered
//! via `claude --append-system-prompt-file` (the SM is the daemon-side brain,
//! not a spawned Claude Code session). It is supplied as the provider request's
//! `system` message later (SM-7). This module only exposes the assembled prompt.

use std::path::{Path, PathBuf};

use crate::core::instruction_pipeline::SECTION_SEPARATOR;
use trusty_agents_common::harness_doc;

// `pub(crate)` is deliberate: these bundled defaults are consumed only through
// `assemble_sm_prompt` / `resolve_sm_prompt`, which own the ordering and the
// BASE_SM-last floor invariant. Any caller needing the raw section content goes
// through those functions; exposing the constants beyond the crate would let a
// caller reassemble the prompt out of order and silently bypass the floor.

/// SM identity + Prohibitions table (SP1-SP7) + Allowlist (bundled at compile
/// time). Mirrors `PM_INSTRUCTIONS`.
pub(crate) const SM_INSTRUCTIONS: &str =
    include_str!("../../assets/sm_instructions/SM_INSTRUCTIONS.md");
/// The 6-phase delegation loop + BLOCKING verification gate (bundled). Mirrors
/// `WORKFLOW`.
pub(crate) const SM_WORKFLOW: &str = include_str!("../../assets/sm_instructions/SM_WORKFLOW.md");
/// The SM's tool/verb surface: session control + memory + goals (bundled).
/// Mirrors `AGENT_DELEGATION`.
pub(crate) const SM_TOOLS: &str = include_str!("../../assets/sm_instructions/SM_TOOLS.md");
/// Non-overridable SM framework floor (bundled). Placed last so it can never be
/// overridden. Mirrors `BASE_PM`.
pub(crate) const BASE_SM: &str = include_str!("../../assets/sm_instructions/BASE_SM.md");

/// Override-directory name segment under the trusty-mpm home (`~/.trusty-mpm`).
///
/// Why: the SM is daemon-side, not per-project, so its overrides live in a
/// home-anchored `~/.trusty-mpm/sm/` directory (the SM analogue of the PM's
/// project-local `.trusty-mpm/`), per the SM-3 task spec.
/// What: the `sm` subdirectory name joined onto `~/.trusty-mpm`.
/// Test: `sm_override_dir_under_home`.
pub const SM_OVERRIDE_SUBDIR: &str = "sm";

/// Override file: replaces the bundled `SM_INSTRUCTIONS` section.
pub const FILE_SM_INSTRUCTIONS: &str = "SM_INSTRUCTIONS.md";
/// Override file: replaces the bundled `SM_WORKFLOW` section.
pub const FILE_SM_WORKFLOW: &str = "SM_WORKFLOW.md";
/// Override file: replaces the bundled `SM_TOOLS` section.
pub const FILE_SM_TOOLS: &str = "SM_TOOLS.md";
/// Override file: replaces the bundled `SM_HARNESS` section (the shared
/// harness-understanding content from `trusty-agents-common`).
pub const FILE_SM_HARNESS: &str = "SM_HARNESS.md";

/// Assemble the SM system prompt from the four bundled assets plus the shared
/// harness-understanding content from `trusty_agents_common::harness_doc`.
///
/// Why: the SM's provider request needs an identical, version-controlled system
/// prompt every time; embedding the sources and joining them here removes any
/// runtime dependency on an external install and keeps the ordering rule in one
/// auditable place (mirrors [`assemble_system_prompt`]).
/// What: joins the five sections in the fixed order SM_INSTRUCTIONS ->
/// SM_HARNESS -> SM_WORKFLOW -> SM_TOOLS -> BASE_SM, separated by the `---`
/// rule. The SM_HARNESS section is the full harness-understanding doc from
/// `trusty_agents_common::harness_doc::harness_understanding()`. Each section
/// is trimmed before joining -- byte-for-byte the same treatment
/// [`resolve_sm_prompt`] applies -- so the two produce identical output when no
/// override is present. BASE_SM is **always last** as the non-overridable
/// framework floor.
/// Test: `assemble_sm_prompt_contains_all_sections`,
/// `assemble_sm_prompt_base_floor_is_last`,
/// `assemble_sm_prompt_contains_harness_section`,
/// `resolve_with_no_overrides_matches_assembled_sections` (exact equality).
///
/// [`assemble_system_prompt`]: crate::core::instruction_pipeline::assemble_system_prompt
pub fn assemble_sm_prompt() -> String {
    let harness = harness_doc::harness_understanding();
    let harness_trimmed = harness.trim();
    vec![
        SM_INSTRUCTIONS.trim(),
        harness_trimmed,
        SM_WORKFLOW.trim(),
        SM_TOOLS.trim(),
        BASE_SM.trim(),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(SECTION_SEPARATOR)
}

/// Resolve `~/.trusty-mpm/sm/`, the SM's override directory.
///
/// Why: [`resolve_sm_prompt`] takes an explicit directory so it is testable
/// against a temp dir; this convenience resolves the production location for the
/// daemon, mirroring how the PM pipeline anchors `~/.trusty-mpm`.
/// What: returns `<home>/.trusty-mpm/sm` when a home directory is resolvable,
/// else `None` (the caller then assembles the bundled prompt unconditionally).
/// Test: `sm_override_dir_under_home`.
pub fn sm_override_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".trusty-mpm").join(SM_OVERRIDE_SUBDIR))
}

/// Read an override file, returning `Some(trimmed)` only when present and
/// non-empty.
///
/// Why: override semantics distinguish absent, present-but-empty, and
/// present-with-content. Absent and empty both fall back to the bundled default;
/// an unreadable file (e.g. permission denied, or a directory in its place) also
/// falls back. Treating empty as "no override" avoids silently blanking a whole
/// section because someone `touch`ed a file. Robustness must never hard-fail
/// prompt assembly. Mirrors `instruction_overrides::read_override`.
/// What: joins `dir/name`; on non-whitespace content returns the trimmed body;
/// on `NotFound` returns `None` silently; on an empty file or any other IO error
/// logs a `tracing::warn!` and returns `None`.
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
                    "SM instruction override file is empty; using bundled default"
                );
                None
            } else {
                tracing::info!(path = %path.display(), "applying SM instruction override");
                Some(trimmed.to_string())
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                %err,
                "SM instruction override file unreadable; using bundled default"
            );
            None
        }
    }
}

/// Resolve the effective SM system prompt, applying any overrides in `dir`.
///
/// Why: the SM prompt must support per-file operator customization (the SM
/// analogue of `resolve_pm_prompt`) while keeping a non-overridable framework
/// floor. Both the live prompt (the provider `system` message, SM-7) and any
/// future inspectable stash call this one function so they can never diverge.
///
/// What: for each of `SM_INSTRUCTIONS`, `SM_HARNESS`, `SM_WORKFLOW`, and
/// `SM_TOOLS`, uses the override file from `dir` when present and non-empty,
/// else the bundled default (SM_HARNESS falls back to
/// `harness_doc::harness_understanding()`). The bundled `BASE_SM` floor is
/// **always** appended last and is **never** overridable -- even a `BASE_SM.md`
/// placed in `dir` is ignored; the bundled floor is used. Sections are joined
/// with [`SECTION_SEPARATOR`], the same rule [`assemble_sm_prompt`] uses, so
/// the two never visually diverge. Order: SM_INSTRUCTIONS -> SM_HARNESS ->
/// SM_WORKFLOW -> SM_TOOLS -> BASE_SM.
///
/// Robustness: a missing override directory, missing files, empty files, and
/// unreadable files all fall back to the bundled defaults without failing.
///
/// Test: `no_overrides_uses_bundled`, `sm_instructions_override_replaces`,
/// `harness_override_replaces`, `workflow_override_replaces`,
/// `tools_override_replaces`, `base_sm_floor_is_never_overridable`, and the
/// robustness tests.
pub fn resolve_sm_prompt(dir: &Path) -> String {
    let instructions = read_override(dir, FILE_SM_INSTRUCTIONS)
        .unwrap_or_else(|| SM_INSTRUCTIONS.trim().to_string());

    // The harness section uses the shared trusty-agents-common content, with
    // an optional override via SM_HARNESS.md in the override directory.
    let harness_default = harness_doc::harness_understanding();
    let harness =
        read_override(dir, FILE_SM_HARNESS).unwrap_or_else(|| harness_default.trim().to_string());

    let workflow =
        read_override(dir, FILE_SM_WORKFLOW).unwrap_or_else(|| SM_WORKFLOW.trim().to_string());
    let tools = read_override(dir, FILE_SM_TOOLS).unwrap_or_else(|| SM_TOOLS.trim().to_string());

    // BASE_SM is the non-overridable floor: always the bundled one, always last.
    // Order: SM_INSTRUCTIONS -> SM_HARNESS -> SM_WORKFLOW -> SM_TOOLS -> BASE_SM
    vec![
        instructions,
        harness,
        workflow,
        tools,
        BASE_SM.trim().to_string(),
    ]
    .into_iter()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(SECTION_SEPARATOR)
}

/// Resolve the effective SM prompt for the production `~/.trusty-mpm/sm/`
/// override directory.
///
/// Why: the daemon needs a zero-argument entry point that resolves the real
/// override location; [`resolve_sm_prompt`] stays path-parameterised for tests.
/// What: when [`sm_override_dir`] resolves a home, delegates to
/// [`resolve_sm_prompt`] with that directory (which tolerates a missing dir);
/// when no home is resolvable, returns the bundled [`assemble_sm_prompt`].
/// Test: side-effect-only over the real home; the layering logic is covered by
/// the `resolve_sm_prompt` tests against temp dirs.
pub fn resolve_sm_prompt_default() -> String {
    match sm_override_dir() {
        Some(dir) => resolve_sm_prompt(&dir),
        None => assemble_sm_prompt(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Write `<dir>/<name>` with `content`, creating `dir` if needed.
    fn write_override(dir: &Path, name: &str, content: &str) {
        fs::create_dir_all(dir).expect("create override dir");
        fs::write(dir.join(name), content).expect("write override");
    }

    #[test]
    fn assemble_sm_prompt_contains_all_sections() {
        // Why: the assembled prompt IS the SM's behavior contract; every bundled
        // section -- prohibitions, allowlist, harness model, 6-phase loop,
        // verification gate, and the BASE_SM floor -- must be present and joined
        // with the `---` rule.
        let prompt = assemble_sm_prompt();
        // Identity + prohibitions table (SP1-SP7) + allowlist.
        assert!(prompt.contains("# Session Manager (SM) -- trusty-mpm"));
        assert!(prompt.contains("| SP1 |"));
        assert!(prompt.contains("| SP7 |"));
        assert!(prompt.contains("You MAY do directly (Allowlist)"));
        // The harness-understanding section (DOC-21) canonical markers.
        assert!(prompt.contains('✻'), "harness ✻ glyph must be present");
        assert!(
            prompt.contains("__HARNESS_EVENT__"),
            "harness __HARNESS_EVENT__ must be present"
        );
        // The 6-phase loop + verification gate.
        assert!(prompt.contains("# SM Workflow -- the delegation loop"));
        assert!(prompt.contains("1. **INTAKE.**"));
        assert!(prompt.contains("6. **REPORT & PERSIST.**"));
        assert!(prompt.contains("Verification gate (BLOCKING)"));
        // The tool/verb surface.
        assert!(prompt.contains("# SM Tools -- the verbs you may call"));
        // The BASE_SM floor.
        assert!(prompt.contains("# BASE_SM Framework Floor"));
        // Joined with the framework separator.
        assert!(prompt.contains(SECTION_SEPARATOR));
    }

    #[test]
    fn assemble_sm_prompt_base_floor_is_last() {
        // Why: BASE_SM is the non-overridable floor; it must be the final
        // section so nothing can displace it.
        let prompt = assemble_sm_prompt();
        let base = prompt.find("# BASE_SM Framework Floor").expect("base_sm");
        let tools = prompt
            .find("# SM Tools -- the verbs you may call")
            .expect("tools");
        let workflow = prompt
            .find("# SM Workflow -- the delegation loop")
            .expect("workflow");
        assert!(workflow < tools, "workflow precedes tools");
        assert!(base > tools, "BASE_SM floor must be appended last");
        // Verify harness section appears before BASE_SM floor
        let harness_pos = prompt.find('✻').expect("harness ✻ glyph");
        assert!(
            harness_pos < base,
            "harness section must precede BASE_SM floor"
        );
    }

    #[test]
    fn assemble_sm_prompt_contains_harness_section() {
        let prompt = assemble_sm_prompt();
        // The harness understanding section contains the Claude Code working glyph
        // and the tcode event prefix as canonical markers (DOC-21).
        assert!(
            prompt.contains('✻'),
            "assembled SM prompt must contain ✻ (harness understanding glyph marker)"
        );
        assert!(
            prompt.contains("__HARNESS_EVENT__"),
            "assembled SM prompt must contain __HARNESS_EVENT__ (tcode event marker)"
        );
    }

    #[test]
    fn harness_override_replaces() {
        let tmp = TempDir::new().unwrap();
        write_override(
            tmp.path(),
            FILE_SM_HARNESS,
            "# Custom Harness Doc\n\nHARNESS_OVERRIDE_SENTINEL\n",
        );
        let prompt = resolve_sm_prompt(tmp.path());
        assert!(prompt.contains("HARNESS_OVERRIDE_SENTINEL"));
        assert!(prompt.contains("# Session Manager (SM) -- trusty-mpm"));
        assert!(prompt.contains("# SM Workflow -- the delegation loop"));
        assert!(prompt.contains("# SM Tools -- the verbs you may call"));
        assert!(prompt.contains("# BASE_SM Framework Floor"));
    }

    #[test]
    fn no_overrides_uses_bundled() {
        // No override dir at all → all four bundled sections present, BASE_SM
        // last.
        let tmp = TempDir::new().unwrap();
        let prompt = resolve_sm_prompt(tmp.path());

        assert!(prompt.contains("# Session Manager (SM) -- trusty-mpm"));
        assert!(prompt.contains("# SM Workflow -- the delegation loop"));
        assert!(prompt.contains("# SM Tools -- the verbs you may call"));
        assert!(prompt.contains("# BASE_SM Framework Floor"));

        let base = prompt.find("# BASE_SM Framework Floor").expect("base");
        let tools = prompt
            .find("# SM Tools -- the verbs you may call")
            .expect("tools");
        assert!(base > tools, "BASE_SM floor must be last");
    }

    #[test]
    fn sm_instructions_override_replaces() {
        // SM_INSTRUCTIONS.md replaces the bundled identity/prohibitions section;
        // the other bundled sections (and the floor) remain.
        let tmp = TempDir::new().unwrap();
        write_override(
            tmp.path(),
            FILE_SM_INSTRUCTIONS,
            "# Custom Identity\n\nDELEGATE_EVERYTHING_TO_ALICE\n",
        );
        let prompt = resolve_sm_prompt(tmp.path());

        assert!(prompt.contains("DELEGATE_EVERYTHING_TO_ALICE"));
        assert!(
            !prompt.contains("# Session Manager (SM) -- trusty-mpm"),
            "bundled SM_INSTRUCTIONS heading must be replaced"
        );
        // Other sections intact.
        assert!(prompt.contains("# SM Workflow -- the delegation loop"));
        assert!(prompt.contains("# SM Tools -- the verbs you may call"));
        assert!(prompt.contains("# BASE_SM Framework Floor"));
        // Floor still last.
        let body = prompt.find("DELEGATE_EVERYTHING_TO_ALICE").expect("body");
        let base = prompt.find("# BASE_SM Framework Floor").expect("base");
        assert!(body < base, "BASE_SM floor follows the override body");
    }

    #[test]
    fn workflow_override_replaces() {
        // SM_WORKFLOW.md replaces the bundled workflow section; others intact.
        let tmp = TempDir::new().unwrap();
        write_override(
            tmp.path(),
            FILE_SM_WORKFLOW,
            "# Custom Loop\n\nTWO_PHASE_ONLY\n",
        );
        let prompt = resolve_sm_prompt(tmp.path());

        assert!(prompt.contains("TWO_PHASE_ONLY"));
        assert!(
            !prompt.contains("# SM Workflow -- the delegation loop"),
            "bundled workflow heading must be replaced"
        );
        assert!(prompt.contains("# Session Manager (SM) -- trusty-mpm"));
        assert!(prompt.contains("# SM Tools -- the verbs you may call"));
        assert!(prompt.contains("# BASE_SM Framework Floor"));
    }

    #[test]
    fn tools_override_replaces() {
        // SM_TOOLS.md replaces the bundled tools section; others intact.
        let tmp = TempDir::new().unwrap();
        write_override(
            tmp.path(),
            FILE_SM_TOOLS,
            "# Custom Verbs\n\nONLY_LAUNCH_AND_STOP\n",
        );
        let prompt = resolve_sm_prompt(tmp.path());

        assert!(prompt.contains("ONLY_LAUNCH_AND_STOP"));
        assert!(
            !prompt.contains("# SM Tools -- the verbs you may call"),
            "bundled tools heading must be replaced"
        );
        assert!(prompt.contains("# Session Manager (SM) -- trusty-mpm"));
        assert!(prompt.contains("# SM Workflow -- the delegation loop"));
        assert!(prompt.contains("# BASE_SM Framework Floor"));
    }

    #[test]
    fn base_sm_floor_is_never_overridable() {
        // Even with a BASE_SM.md in the override dir, the BUNDLED floor is used
        // and appended last; the override-dir BASE_SM content must NOT appear.
        let tmp = TempDir::new().unwrap();
        write_override(
            tmp.path(),
            "BASE_SM.md",
            "# Fake Floor\n\nNO_PROHIBITIONS_ANYMORE\n",
        );
        // Also override an overridable section to prove layering still works.
        write_override(
            tmp.path(),
            FILE_SM_INSTRUCTIONS,
            "# Custom Identity\n\nCUSTOM_IDENTITY_BODY\n",
        );
        let prompt = resolve_sm_prompt(tmp.path());

        // The overridable section IS replaced.
        assert!(prompt.contains("CUSTOM_IDENTITY_BODY"));
        // The bundled floor is present and last.
        assert!(
            prompt.contains("# BASE_SM Framework Floor"),
            "bundled BASE_SM floor must always be appended"
        );
        assert!(prompt.contains("Trusty Tool Priority (Non-Overridable)"));
        // The fake floor from the override dir must NOT leak in.
        assert!(
            !prompt.contains("NO_PROHIBITIONS_ANYMORE"),
            "override-dir BASE_SM.md must be ignored"
        );
        assert!(!prompt.contains("# Fake Floor"));

        // Bundled floor is the last section.
        let body = prompt.find("CUSTOM_IDENTITY_BODY").expect("body");
        let base = prompt.find("# BASE_SM Framework Floor").expect("base");
        assert!(body < base, "bundled BASE_SM floor must come last");
    }

    #[test]
    fn missing_override_dir_uses_bundled() {
        // A non-existent override dir is not an error.
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist");
        assert!(!missing.exists());
        let prompt = resolve_sm_prompt(&missing);
        assert!(prompt.contains("# Session Manager (SM) -- trusty-mpm"));
        assert!(prompt.contains("# BASE_SM Framework Floor"));
    }

    #[test]
    fn empty_override_falls_back() {
        // An empty (whitespace-only) override is treated as "no override": the
        // bundled default for that section survives (no silent blanking).
        let tmp = TempDir::new().unwrap();
        write_override(tmp.path(), FILE_SM_WORKFLOW, "   \n\t\n");
        let prompt = resolve_sm_prompt(tmp.path());
        assert!(prompt.contains("# SM Workflow -- the delegation loop"));
        assert!(prompt.contains("# BASE_SM Framework Floor"));
    }

    #[test]
    fn unreadable_override_falls_back() {
        // A file that cannot be read (here: a directory in the file's place)
        // falls back to the bundled default rather than failing assembly.
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(FILE_SM_WORKFLOW)).unwrap();
        let prompt = resolve_sm_prompt(tmp.path());
        // Did not panic; bundled workflow is used.
        assert!(prompt.contains("# SM Workflow -- the delegation loop"));
        assert!(prompt.contains("# BASE_SM Framework Floor"));
    }

    #[test]
    fn separators_are_consistent() {
        // The resolved prompt uses the same `---` rule the bundled assembler
        // uses, so the two never visually diverge.
        let tmp = TempDir::new().unwrap();
        let prompt = resolve_sm_prompt(tmp.path());
        assert!(prompt.contains(SECTION_SEPARATOR));
    }

    #[test]
    fn resolve_with_no_overrides_matches_assembled_sections() {
        // With an empty override dir, resolve_sm_prompt is now byte-identical to
        // the bundled assemble_sm_prompt: both trim each section the same way
        // before joining with the `---` rule (Finding 3). Assert exact equality.
        let tmp = TempDir::new().unwrap();
        let resolved = resolve_sm_prompt(tmp.path());
        let assembled = assemble_sm_prompt();
        assert_eq!(
            resolved, assembled,
            "no-override resolve must be byte-identical to assemble"
        );
        // The harness block joins its four sub-sections with plain "\n\n" (not
        // "---"), so it does NOT embed SECTION_SEPARATOR internally. The assembled
        // SM prompt contains exactly 5 top-level sections separated by "---":
        // SM_INSTRUCTIONS → SM_HARNESS → SM_WORKFLOW → SM_TOOLS → BASE_SM.
        let section_count = assembled.split(SECTION_SEPARATOR).count();
        assert_eq!(
            section_count, 5,
            "exactly five top-level sections expected (got {section_count}); \
             the harness block must not embed the top-level separator"
        );
    }

    #[test]
    fn sm_override_dir_under_home() {
        // The production override dir resolves under ~/.trusty-mpm/sm. In the
        // normal case (CI and dev machines both have a home) `sm_override_dir`
        // returns `Some` and we assert the path shape. We tolerate the rare
        // home-less environment by handling `None` explicitly with a comment, so
        // a genuinely-skipped assertion can never masquerade as a green pass: if
        // a home IS present the path-shape checks run and must hold; if no home
        // resolves we record that the graceful-None fallback path was taken.
        match sm_override_dir() {
            Some(dir) => {
                assert!(dir.ends_with("sm"), "override dir must end with `sm`");
                assert!(
                    dir.to_string_lossy().contains(".trusty-mpm"),
                    "override dir must be anchored under `.trusty-mpm`"
                );
            }
            None => {
                // No home directory resolvable (e.g. a stripped sandbox). This is
                // the documented graceful fallback: `resolve_sm_prompt_default`
                // then assembles the bundled prompt unconditionally. Nothing to
                // assert about the path shape here -- the absence itself is the
                // contract -- but we flag it so the green pass is honest.
                eprintln!(
                    "sm_override_dir_under_home: no home resolved; \
                     graceful-None fallback exercised"
                );
            }
        }
    }
}

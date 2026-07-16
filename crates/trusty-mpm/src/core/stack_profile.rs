//! Per-project stack profile derived from detected language markers (#1971).
//!
//! Why: the PM must be primed with THIS project's stack, never a hardcoded
//! default. Historically the deployed output style declared a "Rust workspace"
//! for every project, so a Next.js/TypeScript project (ai-power-rankings) was
//! told to route to `rust-engineer` and run `cargo` — the PM had to notice and
//! self-correct. Bob's normative requirement: no default stack profile; each
//! project's PM priming is configured from that project's detected stack, and an
//! undetected stack yields a NEUTRAL profile that mandates detection before
//! routing. The bundled output styles are now stack-neutral (they carry no
//! default); this module supplies the concrete, per-project half: a
//! **Detected Project Stack** section folded into the per-project PM prompt at
//! session-prepare time.
//! What: [`stack_profile_section`] probes `project_dir` via the shared
//! [`crate::core::manifest::project_lang::detected_engineers`] marker detection
//! (single source of truth, also used for agent-roster scoping) and renders a
//! Markdown section — either the detected language engineers (route code work to
//! them, use the project's own quality gate) or a neutral "not auto-detected"
//! block that forbids defaulting and requires a Research pass. [`resolve_pm_prompt`]
//! ([`crate::core::instruction_overrides`]) slots the section into the prompt.
//! Test: `detected_rust_lists_rust_engineer`, `detected_nextjs_lists_ts_family`,
//! `detected_polyglot_lists_both_families`, `undetected_is_neutral_no_default`,
//! `heading_present_in_both_modes`.

use std::path::Path;

use crate::core::manifest::project_lang::detected_engineers;

/// Heading that delimits the auto-derived stack-profile block in the PM prompt.
///
/// Why: a stable, greppable heading lets the launched PM (and tests, and a human
/// reading `.trusty-mpm/last-instructions.md`) locate the authoritative per-project
/// stack statement, and lets [`crate::core::instruction_overrides`] assert it is
/// present.
/// What: the Markdown `##` heading prepended to the detected-stack body.
/// Test: `heading_present_in_both_modes`.
pub const STACK_PROFILE_HEADING: &str = "## Detected Project Stack (auto-derived)";

/// Render the per-project stack-profile section for `project_dir`.
///
/// Why: the PM prompt must state the project's ACTUAL stack rather than inherit a
/// default one. Deriving the section from the same marker detection used for
/// agent scoping keeps the delegation surface and the prose priming consistent,
/// and guarantees an unknown project gets a neutral, detect-first profile instead
/// of any language default (#1971).
/// What: probes `project_dir` for language markers via [`detected_engineers`].
/// When at least one matches, returns a section listing the matching
/// `<lang>-engineer` stems (sorted, de-duplicated by the `BTreeSet`) and pointing
/// the quality gate at the project's own configured checks. When none match,
/// returns a NEUTRAL section that forbids assuming any stack and mandates a
/// Research pass to detect it before routing. Pure and side-effect-free apart
/// from the filesystem `exists()` probes performed by [`detected_engineers`].
/// Test: `detected_rust_lists_rust_engineer`, `detected_nextjs_lists_ts_family`,
/// `detected_polyglot_lists_both_families`, `undetected_is_neutral_no_default`.
pub fn stack_profile_section(project_dir: &Path) -> String {
    let engineers = detected_engineers(project_dir);

    if engineers.is_empty() {
        return format!(
            "{STACK_PROFILE_HEADING}\n\n\
             No known language or framework marker files were found in this \
             project's root. **Do NOT assume any stack** — not Rust, not Python, \
             not Node/TypeScript. Begin with a **MANDATORY Research phase** to \
             detect the stack from the repository before routing any \
             implementation work, then delegate to the matching \
             `<lang>-engineer`. Never fall back to a default stack profile."
        );
    }

    let list = engineers
        .iter()
        .map(|stem| format!("- `{stem}`"))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "{STACK_PROFILE_HEADING}\n\n\
         trusty-mpm probed this project's root marker files and detected the \
         stack below. Route hands-on code work to the matching language \
         engineer(s) — prefer the most specific — and never a generic \
         `engineer` when one of these fits:\n\n\
         {list}\n\n\
         **Quality gate:** use THIS project's own configured checks (its \
         `Makefile` target, `package.json` scripts, or CI pipeline) — confirm \
         the real commands before citing them; do not assume `cargo`/`make \
         check` unless the project actually uses them. If a task clearly touches \
         a stack not listed above, run a Research pass to confirm before \
         routing — never fall back to a default stack profile."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Create an empty marker file `name` under `dir`, making parents as needed.
    fn touch(dir: &Path, name: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, "").unwrap();
    }

    /// A Cargo project routes to rust-engineer and cites the project's own gate.
    ///
    /// Why: the core positive case — a detected Rust project must produce a
    /// rust-engineer routing derived from detection, not a hardcoded default.
    /// What: writes `Cargo.toml`, asserts the section names `rust-engineer`, the
    /// heading, and the project-own quality-gate wording; and does NOT contain
    /// the neutral "do NOT assume" block.
    /// Test: this function IS the test.
    #[test]
    fn detected_rust_lists_rust_engineer() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "Cargo.toml");

        let section = stack_profile_section(tmp.path());
        assert!(section.contains(STACK_PROFILE_HEADING));
        assert!(
            section.contains("`rust-engineer`"),
            "must route to rust-engineer"
        );
        assert!(
            section.contains("own configured checks"),
            "must point at the project's own quality gate"
        );
        assert!(
            !section.contains("Do NOT assume any stack"),
            "a detected project must not emit the neutral block"
        );
    }

    /// A Next.js/TypeScript project lists the JS/TS family engineers.
    ///
    /// Why: this is the exact ai-power-rankings recurrence from #1971 — a
    /// package.json + tsconfig.json + next.config project must route to the
    /// nextjs/typescript engineers, never rust-engineer.
    /// What: writes `package.json`, `tsconfig.json`, `next.config.js`, asserts the
    /// section names `nextjs-engineer` and `typescript-engineer` and does NOT name
    /// `rust-engineer`.
    /// Test: this function IS the test.
    #[test]
    fn detected_nextjs_lists_ts_family() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "package.json");
        touch(tmp.path(), "tsconfig.json");
        touch(tmp.path(), "next.config.js");

        let section = stack_profile_section(tmp.path());
        assert!(
            section.contains("`nextjs-engineer`"),
            "must route to nextjs-engineer"
        );
        assert!(
            section.contains("`typescript-engineer`"),
            "must route to typescript-engineer"
        );
        assert!(
            !section.contains("`rust-engineer`"),
            "a Next.js project must NOT route to rust-engineer (the #1971 bug)"
        );
    }

    /// A .NET project (`*.csproj` glob marker) routes to dotnet-engineer.
    ///
    /// Why: #2831 — a C#/.NET/VB.NET project must get its specialist rather than
    /// the general-purpose fallback, so the derived stack section must name
    /// `dotnet-engineer` when a project/solution marker is present.
    /// What: writes `App.csproj`, asserts the section names `dotnet-engineer` and
    /// does not emit the neutral "Do NOT assume" block.
    /// Test: this function IS the test.
    #[test]
    fn detected_dotnet_lists_dotnet_engineer() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "App.csproj");

        let section = stack_profile_section(tmp.path());
        assert!(
            section.contains("`dotnet-engineer`"),
            "must route to dotnet-engineer"
        );
        assert!(
            !section.contains("Do NOT assume any stack"),
            "a detected .NET project must not emit the neutral block"
        );
    }

    /// An unknown project type yields a neutral, detect-first profile.
    ///
    /// Why: Bob's requirement — undetected stack must NEVER default to any stack;
    /// it must mandate detection before routing.
    /// What: a directory with only a README asserts the neutral block: the "Do
    /// NOT assume any stack" and "MANDATORY Research" wording, and the absence of
    /// any `<lang>-engineer` routing list.
    /// Test: this function IS the test.
    #[test]
    fn undetected_is_neutral_no_default() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "README.md");

        let section = stack_profile_section(tmp.path());
        assert!(section.contains(STACK_PROFILE_HEADING));
        assert!(section.contains("Do NOT assume any stack"));
        assert!(section.contains("MANDATORY Research"));
        assert!(
            !section.contains("rust-engineer") && !section.contains("python-engineer"),
            "the neutral block must not name any language engineer"
        );
    }

    /// A polyglot project (Rust + JS/TS) names both families, dropping neither.
    ///
    /// Why: mirrors `project_lang::polyglot_project_keeps_both` at the rendering
    /// layer — a repo with markers for two ecosystems must route to engineers
    /// from BOTH, not silently pick one, since a wrong exclusive pick is exactly
    /// the class of bug #1971 is about.
    /// What: writes both `Cargo.toml` and `package.json`, asserts the rendered
    /// section names `rust-engineer` AND a JS-family engineer stem
    /// (`javascript-engineer`), and does not emit the neutral "do NOT assume"
    /// block.
    /// Test: this function IS the test.
    #[test]
    fn detected_polyglot_lists_both_families() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "Cargo.toml");
        touch(tmp.path(), "package.json");

        let section = stack_profile_section(tmp.path());
        assert!(
            section.contains("`rust-engineer`"),
            "polyglot project must still route to rust-engineer"
        );
        assert!(
            section.contains("`javascript-engineer`"),
            "polyglot project must also route to the JS/TS family"
        );
        assert!(
            !section.contains("Do NOT assume any stack"),
            "a detected polyglot project must not emit the neutral block"
        );
    }

    /// The heading is present whether or not a stack is detected.
    ///
    /// Why: [`crate::core::instruction_overrides`] and human readers rely on the
    /// heading being present in both modes to locate the block.
    /// What: asserts the heading appears for both a detected (Cargo.toml) and an
    /// undetected (empty) project root.
    /// Test: this function IS the test.
    #[test]
    fn heading_present_in_both_modes() {
        let detected = TempDir::new().unwrap();
        touch(detected.path(), "go.mod");
        assert!(stack_profile_section(detected.path()).contains(STACK_PROFILE_HEADING));

        let undetected = TempDir::new().unwrap();
        assert!(stack_profile_section(undetected.path()).contains(STACK_PROFILE_HEADING));
    }
}

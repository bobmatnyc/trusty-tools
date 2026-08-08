//! Renders `references/framework.md` — how the harness itself is laid out
//! (issue #4946).
//!
//! Why: a session could answer "what commands exist?" from the other five
//! reference files but not "where does my skill actually deploy?" or "which
//! agent tier wins?" — those answers only existed in Rust source, so every
//! question ended in a hand investigation. The one place the compiled prompt
//! did state a tier precedence
//! (`assets/instructions/sections/core.md`) named `~/.trusty-mpm/agents/`, a
//! tier no code reads: hand-written prose about the framework's own mechanics
//! rots silently, and stale self-knowledge is worse than none. So this file is
//! generated from the same path constants and tier resolvers the runtime uses
//! — move a directory or reorder a tier and the committed skill drifts,
//! failing `scripts/check_capabilities.sh`.
//! What: [`render`] emits the install layout ([`FrameworkPaths`] rendered under
//! a literal `~` base), the managed `CLAUDE_CONFIG_DIR`, the agent tier order
//! ([`deployed_agent_dirs_from`]), the skill deploy tiers
//! ([`skill_deploy_tiers`]), the per-session state layout, an
//! existence-checked index of the authoritative docs (separating what ships in
//! the published crate from what is repo-only), and the commands that report
//! live state — each annotated with its real clap `about` string.
//! Test: `framework_render_lists_the_real_agent_tier_order`,
//! `framework_render_never_names_the_phantom_agent_tier`,
//! `framework_render_is_deterministic`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use clap::CommandFactory;

use crate::cli::Cli;
use trusty_mpm::core::delegation_authority::deployed_agent_dirs_from;
use trusty_mpm::core::harness_root::{HARNESS_DIR, SESSIONS_DIR};
use trusty_mpm::core::instruction_pipeline::COMPILED_PROMPT_FILE;
use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::core::skill_deploy_tiers::skill_deploy_tiers;
use trusty_mpm::core::trusty_tools_config::managed_claude_config_dir_at;

/// The home-directory stand-in every rendered path is resolved against.
///
/// Why: [`FrameworkPaths::default`] resolves against the real `dirs::home_dir()`,
/// which would bake `/Users/<someone>` into a committed file and fail the drift
/// gate on every other machine. `~` is both reproducible and the form an
/// operator recognises.
const HOME: &str = "~";

/// The project stand-in for tiers that are resolved per-project.
const PROJECT: &str = "<project>";

/// Docs that live OUTSIDE the published crate, checked for existence.
///
/// Why: these are the authoritative long-form references, and pointing at one
/// that has been moved or deleted is exactly the silent rot this file exists to
/// prevent. Each path is probed against the repo root at generation time, so a
/// rename flips its rendered row and fails the drift gate instead of shipping a
/// dead pointer.
/// What: `(repo-relative path, what it holds)`.
const REPO_DOCS: &[(&str, &str)] = &[
    (
        "docs/adr/INDEX.md",
        "Index of every architecture decision record — the ADR is the authority on why a mechanism is shaped the way it is.",
    ),
    (
        "docs/specs/README.md",
        "Index of the numbered DOC-* specs (behaviour contracts). A spec, not an ADR, is what a source file's `# Spec References` block links back to.",
    ),
    (
        "docs/reference/",
        "Operational reference: release workflow, worktree discipline, SLOC cap, environment variables, threat model.",
    ),
    (
        "docs/SUMMARY.md",
        "mdBook table of contents spanning every crate's docs.",
    ),
];

/// Commands that report the harness's LIVE state, rather than this catalog's
/// generated-at-build-time view.
///
/// Why: this file describes the layout a correctly-installed harness has. When
/// a session needs what its own machine actually resolved — which tiers exist,
/// what the prompt really says — it must run something, not read prose.
/// What: `(command path through the clap tree, why you would run it)`. The
/// `about` text is pulled from clap at render time, so a renamed command
/// changes the output.
const LIVE_STATE_COMMANDS: &[(&[&str], &str)] = &[
    (
        &["doctor"],
        "Whether each tier is actually populated on this machine, and which check failed.",
    ),
    (
        &["sessions", "instructions"],
        "The composed prompt this project's session would receive, verbatim — the ground truth when the prompt and this catalog disagree.",
    ),
    (
        &["install"],
        "Re-materialise the bundled agent/skill/hook artifacts into the layout above.",
    ),
];

/// Render the full framework-model reference.
///
/// Why: see the module doc — every path and tier below is computed, never
/// restated.
/// What: seven sections (layout, managed config dir, agent tiers, skill tiers,
/// per-session state, documentation index, live-state commands).
/// Test: `framework_render_lists_the_real_agent_tier_order`.
pub(crate) fn render() -> String {
    let home = Path::new(HOME);
    let paths = FrameworkPaths::under(home);
    let managed = managed_claude_config_dir_at(home);

    let mut out = String::new();
    out.push_str("# Framework Model — Layout, Tiers, and Where the Answers Live\n\n");
    out.push_str(
        "Generated by `tm generate capabilities` from the harness's own path \
         constants and tier resolvers (`core::paths::FrameworkPaths`, \
         `core::delegation_authority::deployed_agent_dirs_from`, \
         `core::skill_deploy_tiers::skill_deploy_tiers`, \
         `core::harness_root`). Nothing here is restated by hand: move a \
         directory or reorder a tier and this file drifts, failing \
         `scripts/check_capabilities.sh`. Paths are shown against `~` because \
         the real layout is home-relative.\n\n",
    );

    render_layout(&mut out, &paths, &managed);
    render_agent_tiers(&mut out, &paths, &managed);
    render_skill_tiers(&mut out, &paths);
    render_session_state(&mut out);
    render_docs(&mut out);
    render_live_state(&mut out);
    out
}

/// Install-layout table plus the `CLAUDE_CONFIG_DIR` explanation.
fn render_layout(out: &mut String, paths: &FrameworkPaths, managed: &Path) {
    out.push_str("## Install Layout\n\n");
    out.push_str("| Path | What lives there |\n|---|---|\n");
    let rows: &[(&PathBuf, &str)] = &[
        (&paths.root, "Framework install root."),
        (
            &paths.framework,
            "tm-owned, auto-materialised bundled tree. Never hand-edit — `tm install` and session prep overwrite it.",
        ),
        (
            &paths.agents,
            "Bundled agent SOURCE files (pre-composition).",
        ),
        (
            &paths.skills,
            "Bundled skill SOURCE files, re-materialised whenever the binary's embedded bundle fingerprint changes.",
        ),
        (
            &paths.user_skills,
            "USER-authored skill source. Sits outside `framework/` precisely because that tree is overwritten. Deployed into every session, outranking a same-named bundled skill.",
        ),
        (&paths.hooks, "Optimizer / overseer hook policy."),
        (
            &paths.instructions,
            "Framework instruction assets. The prompt a session actually ran with is NOT here — it is per-session (see below).",
        ),
        (&paths.registry, "Shared project registry."),
    ];
    for (path, what) in rows {
        let _ = writeln!(out, "| `{}` | {what} |", path.display());
    }
    let _ = writeln!(
        out,
        "| `{}` | The tm-managed `CLAUDE_CONFIG_DIR` (see below). |",
        managed.display()
    );
    let _ = writeln!(
        out,
        "| `{}` | The operator's OWN Claude Code install. tm reads it as the lowest agent tier and never writes framework-owned agents into it. |\n",
        paths.claude_home_dir().join(".claude").display()
    );

    out.push_str("## Why `CLAUDE_CONFIG_DIR` Is Relocated\n\n");
    let _ = writeln!(
        out,
        "Managed sessions launch `claude` with `CLAUDE_CONFIG_DIR` pointed at \
         `{}`, not the operator's `~/.claude`. Two things follow. Framework-owned \
         agents and skills land in a tm-controlled directory, so they cannot \
         contaminate — or be contaminated by — a Claude Code install the operator \
         maintains for their own use. And because the relocated directory is \
         global rather than per-workspace, one roster serves every session instead \
         of each project carrying a mutable copy that can silently shadow the real \
         agent.",
        managed.display()
    );
    out.push_str(
        "\nA session that has this variable set is reading the managed tier. A \
         session that does not is reading the operator's own.\n\n",
    );
}

/// Agent tier precedence, rendered from the real resolver.
fn render_agent_tiers(out: &mut String, paths: &FrameworkPaths, managed: &Path) {
    out.push_str("## Agent Tier Precedence\n\n");
    out.push_str(
        "Highest precedence first — on a name collision the earlier tier wins, \
         case-insensitively. Rendered from `deployed_agent_dirs_from`, the same \
         function the runtime resolves against.\n\n",
    );
    let dirs = deployed_agent_dirs_from(
        Path::new(PROJECT),
        Some(managed),
        &paths.claude_agents_dir(),
    );
    let notes = [
        "Hand-placed and project-custom agents only. tm does not deploy bundled agents here.",
        "Where every BUNDLED agent deploys. The `CLAUDE_CONFIG_DIR` env var overrides this path when set.",
        "The operator's own Claude Code agents. Read, never written by tm.",
    ];
    for (i, dir) in dirs.iter().enumerate() {
        let note = notes.get(i).copied().unwrap_or("");
        let _ = writeln!(out, "{}. `{}` — {note}", i + 1, dir.display());
    }
    out.push_str(
        "\nThere is no `~/.trusty-mpm/agents/` tier. No code reads that path; a \
         copy placed there is loaded by nothing.\n\n",
    );
}

/// Skill deploy tiers, rendered from the real enumerator.
fn render_skill_tiers(out: &mut String, paths: &FrameworkPaths) {
    out.push_str("## Skill Deploy Tiers\n\n");
    out.push_str(
        "Skills, unlike agents, have no single destination — a bundled skill is \
         deployed into each of these independently, so one can be stale while \
         another is current. Rendered from `skill_deploy_tiers`.\n\n",
    );
    out.push_str("| Tier | Directory |\n|---|---|\n");
    for tier in skill_deploy_tiers(paths, Some(Path::new(PROJECT))) {
        let _ = writeln!(out, "| {} | `{}` |", tier.label, tier.dir.display());
    }
    out.push_str(
        "\nSource precedence when the same skill name exists in more than one \
         source: project-custom, then user-custom (`~/.trusty-mpm/skills/`), then \
         bundled. A deployed skill you hand-edit is frozen — its checksum no \
         longer matches, so redeploy skips it rather than overwriting your edit. \
         A skill whose slug contains `mcp` is never deployed; it would shadow \
         Claude Code's built-in `/mcp`.\n\n",
    );
}

/// Per-session on-disk state.
fn render_session_state(out: &mut String) {
    out.push_str("## Per-Session State\n\n");
    let session_dir = Path::new(PROJECT)
        .join(HARNESS_DIR)
        .join(SESSIONS_DIR)
        .join("<session-id>");
    let _ = writeln!(
        out,
        "Session records live under `{}`, and the prompt a session was actually \
         launched with is `{}` inside it. Both are keyed by session id, so two \
         concurrent sessions in one project cannot overwrite each other.\n",
        session_dir.display(),
        COMPILED_PROMPT_FILE
    );
    let _ = writeln!(
        out,
        "The `{PROJECT}` here is the checkout that OWNS the project — the main \
         checkout, never one of its git worktrees. Harness state belongs to the \
         project; a worktree carries only code.\n"
    );
}

/// Documentation index, split by what actually ships.
fn render_docs(out: &mut String) {
    out.push_str("## Authoritative Documentation\n\n");
    out.push_str(
        "This catalog answers layout and tier questions. Design rationale, \
         behaviour contracts, and operational procedure live in documents — but \
         only some of them travel with the installed binary.\n\n",
    );

    out.push_str("### Ships in the published crate\n\n");
    let crate_docs = crate_doc_files();
    if crate_docs.is_empty() {
        out.push_str("- (none found under `crates/trusty-mpm/docs/`)\n");
    } else {
        for name in &crate_docs {
            let _ = writeln!(out, "- `crates/trusty-mpm/docs/{name}`");
        }
    }
    out.push_str("- `crates/trusty-mpm/README.md`, `crates/trusty-mpm/CHANGELOG.md`\n\n");

    out.push_str("### Repo-only — NOT in the published crate\n\n");
    out.push_str(
        "`cargo package` includes only files under `crates/trusty-mpm/`, so \
         everything below requires a checkout of \
         `github.com/bobmatnyc/trusty-tools`. A session running against an \
         installed binary alone cannot open these.\n\n",
    );
    out.push_str("| Path | Holds | Present in this checkout |\n|---|---|---|\n");
    let root = repo_root();
    for (rel, what) in REPO_DOCS {
        let present = if root.join(rel).exists() { "yes" } else { "NO" };
        let _ = writeln!(out, "| `{rel}` | {what} | {present} |");
    }
    out.push('\n');
}

/// Commands that report live state, annotated with clap's real `about` text.
fn render_live_state(out: &mut String) {
    out.push_str("## Resolving Live State\n\n");
    out.push_str(
        "This file describes the layout a correct install has. To learn what \
         THIS machine actually resolved, run one of these rather than \
         guessing — the `about` text is pulled from the CLI itself.\n\n",
    );
    for (path, why) in LIVE_STATE_COMMANDS {
        let about = command_about(path);
        let _ = writeln!(out, "- `tm {}` — {why}", path.join(" "));
        let _ = writeln!(out, "  - CLI: {about}");
    }
    out.push_str(
        "\nWhen the compiled prompt and this catalog disagree about a path, the \
         prompt is the thing the session is actually obeying and this catalog is \
         the thing derived from code. Report the disagreement; do not silently \
         pick one.\n",
    );
}

/// Look up a command's `about` string by walking the clap tree.
///
/// Why: hard-coding the description would reintroduce the hand-maintained prose
/// this whole file exists to eliminate. Resolving it live means a renamed or
/// removed command changes the rendered output and trips the drift gate.
/// What: walks `Cli::command()` one segment at a time, returning the `about`
/// text, or an explicit not-found marker.
/// Test: `framework_render_resolves_clap_about_text`.
fn command_about(path: &[&str]) -> String {
    let mut current = Cli::command();
    for segment in path {
        let next = current.find_subcommand(segment).cloned();
        match next {
            Some(sub) => current = sub,
            None => {
                return format!(
                    "(no such command — `tm {}` was renamed or removed)",
                    path.join(" ")
                );
            }
        }
    }
    current
        .get_about()
        .map(|about| about.to_string())
        .unwrap_or_else(|| "(no description)".to_string())
}

/// The repository root (`crates/trusty-mpm/../..`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Every `.md` file under `crates/trusty-mpm/docs/`, sorted.
///
/// Why: this is the set that actually ships in the published crate, so it is
/// read from disk rather than listed by hand — adding or removing a crate doc
/// changes the generated skill and is caught by the drift gate.
/// What: sorted file names; an unreadable directory yields an empty list.
/// Test: `framework_render_lists_crate_docs`.
fn crate_doc_files() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.ends_with(".md").then_some(name)
        })
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framework_render_lists_the_real_agent_tier_order() {
        let rendered = render();
        let project = rendered
            .find("1. `<project>/.claude/agents`")
            .expect("project tier ranks first");
        let managed = rendered
            .find("claude-config/agents`")
            .expect("managed tier is listed");
        assert!(project < managed, "project tier must precede managed tier");
    }

    #[test]
    fn framework_render_never_names_the_phantom_agent_tier() {
        // #4946: `~/.trusty-mpm/agents/` is the tier the compiled prompt used to
        // claim and no code reads. It may appear only in the sentence that
        // denies it.
        let rendered = render();
        let mentions = rendered.matches("~/.trusty-mpm/agents/").count();
        assert_eq!(
            mentions, 1,
            "the phantom tier may appear once, in the sentence refuting it"
        );
        assert!(
            rendered.contains("There is no `~/.trusty-mpm/agents/` tier"),
            "{rendered}"
        );
    }

    #[test]
    fn framework_render_resolves_clap_about_text() {
        let rendered = render();
        assert!(
            !rendered.contains("no such command"),
            "a documented live-state command was renamed or removed: {rendered}"
        );
    }

    #[test]
    fn framework_render_lists_crate_docs() {
        let docs = crate_doc_files();
        assert!(
            !docs.is_empty(),
            "crates/trusty-mpm/docs/ should hold the crate-shipped docs"
        );
        let rendered = render();
        for name in docs {
            assert!(rendered.contains(&name), "{name} missing from {rendered}");
        }
    }

    #[test]
    fn framework_render_marks_repo_only_docs_present() {
        // Every REPO_DOCS pointer must resolve in a real checkout; a `NO` row
        // means the pointer is dead and the generated file just said so.
        let rendered = render();
        assert!(
            !rendered.contains("| NO |"),
            "a repo-only doc pointer no longer resolves: {rendered}"
        );
    }

    #[test]
    fn framework_render_has_no_blank_line_runs() {
        // Sections are assembled by separate writers; a stray trailing newline
        // in one of them is invisible in source and obvious in the rendered file.
        let rendered = render();
        assert!(!rendered.contains("\n\n\n"), "{rendered}");
    }

    #[test]
    fn framework_render_is_deterministic() {
        assert_eq!(render(), render());
    }
}

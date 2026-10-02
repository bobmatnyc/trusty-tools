//! Generator engine for `tm generate capabilities` (issue #2913).
//!
//! Why: the `tm-capabilities` bundled skill (CLI tree, MCP tool catalog,
//! agent roster, skill catalog, doctor checks, framework layout) must never
//! be hand-maintained prose — every one of those six surfaces already exists
//! as machine-extractable, in-process data (clap's `Command` introspection,
//! `mcp::tools::tool_catalog()`, `bundle::ALL` + `agent_metadata`, a
//! maintained-and-cross-checked doctor-check list, the `FrameworkPaths` and
//! tier resolvers the runtime itself resolves against). This module is the
//! single place that walks each surface and turns it into deterministic
//! markdown, so `--check` (the CI drift gate) and the write path share
//! exactly the same generation logic and can never disagree about what
//! "up to date" means.
//! What: [`generate`] builds the full [`GeneratedSet`] (7 files); [`write`](crate::generate::write)
//! writes it to `crates/trusty-mpm/src/assets/skills/` of the checkout the
//! command runs in ([`resolve_skills_asset_dir`], #7776); [`diff`] compares it
//! against the committed copies without writing; [`run_capabilities`] is the
//! `tm generate capabilities[--check]` CLI entry point.
//! Test: `generated_set_has_seven_entries`, `generated_set_is_deterministic`,
//! plus each submodule's own render tests.

mod agents;
mod cli_tree;
mod doctor;
mod entry;
mod framework;
mod mcp_tools;
mod skills;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Relative path (under `src/assets/skills/`) -> generated file content.
///
/// Why: `&'static str` keys (string literals fixed at compile time, one per
/// generated file) avoid an owned-`String`-vs-literal mismatch between the
/// build side and the seven fixed call sites in [`generate`]; `BTreeMap` gives
/// deterministic iteration order for free (the write/diff loops don't
/// otherwise care about order, but stable iteration keeps `--check`'s printed
/// drift summary reproducible too).
pub(crate) type GeneratedSet = BTreeMap<&'static str, String>;

/// Build every generated file's content, keyed by its path relative to
/// `src/assets/skills/`.
///
/// Why: a single function is the one place that knows the full generated
/// file set — the entry point plus six references files — so [`write`](crate::generate::write) and
/// [`diff`] share it and can never disagree about which files exist.
/// What: returns 7 entries: `tm-capabilities.md` (the entry file) plus
/// `tm-capabilities/references/{cli,mcp-tools,agents,skills,doctor,framework}.md`.
/// `references/workflows.md` is deliberately absent from this set — it is
/// hand-authored (issue #2913 brief §E) and never regenerated or diffed.
/// Test: `generated_set_has_seven_entries`, `generated_set_is_deterministic`.
pub(crate) fn generate(roster: &trusty_mpm::core::content_source::AgentRoster) -> GeneratedSet {
    let mut set = GeneratedSet::new();
    set.insert("tm-capabilities.md", entry::render(roster));
    set.insert("tm-capabilities/references/cli.md", cli_tree::render());
    set.insert(
        "tm-capabilities/references/mcp-tools.md",
        mcp_tools::render(),
    );
    set.insert(
        "tm-capabilities/references/agents.md",
        agents::render(roster),
    );
    set.insert("tm-capabilities/references/skills.md", skills::render());
    set.insert("tm-capabilities/references/doctor.md", doctor::render());
    set.insert(
        "tm-capabilities/references/framework.md",
        framework::render(),
    );
    set
}

/// The skills asset directory, relative to a trusty-tools checkout root.
const SKILLS_ASSET_REL: &str = "crates/trusty-mpm/src/assets/skills";

/// The skills asset directory of the checkout whose git root holds `start`.
///
/// Why (#7776): an installed `tm` is usually built from a worktree, so the
/// compile-time `CARGO_MANIFEST_DIR` names the BUILD checkout. `--check` run
/// anywhere else diffed that tree and reported every file "(missing)" once it
/// was reclaimed, and the write path dirtied it.
/// What: the nearest ancestor of `start` holding a `.git` entry (a directory in
/// a main checkout, a file in a worktree) is the git root; its
/// `crates/trusty-mpm/src/assets/skills/` is returned. With no git root, or a
/// git root without that directory, it errors naming the path it tried — a
/// path-resolution error, never a drift report.
/// Test: `resolves_the_cwd_git_root_not_the_build_checkout_7776`,
/// `unresolvable_asset_root_is_a_path_error_7776`.
pub(crate) fn resolve_skills_asset_dir(start: &Path) -> anyhow::Result<PathBuf> {
    // #7776: resolve from the invocation's checkout, never env!("CARGO_MANIFEST_DIR").
    let Some(root) = start.ancestors().find(|d| d.join(".git").exists()) else {
        anyhow::bail!(
            "tm-capabilities: cannot resolve the asset directory: no git root at or above {}. \
             Run `tm generate capabilities` from inside a trusty-tools checkout or worktree.",
            start.display()
        );
    };
    let dir = root.join(SKILLS_ASSET_REL);
    anyhow::ensure!(
        dir.is_dir(),
        "tm-capabilities: cannot resolve the asset directory: {} does not exist, so the git root \
         {} is not a trusty-tools checkout.",
        dir.display(),
        root.display()
    );
    Ok(dir)
}

/// Write every generated file to disk under `root`.
///
/// Why: the non-`--check` path — regenerating and committing the output is
/// how a maintainer picks up a new CLI command, MCP tool, agent, or skill.
/// `root` is injected (rather than resolved here) so tests
/// can point this at a temp directory instead of mutating the real committed
/// assets on every `cargo test` run.
/// What: creates parent directories as needed (the `references/` subtree
/// does not exist until the first write) and overwrites each file.
/// Test: `write_then_diff_round_trips_clean` (against a temp dir).
pub(crate) fn write(set: &GeneratedSet, root: &Path) -> anyhow::Result<()> {
    for (rel_path, content) in set {
        let target = root.join(rel_path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, content)?;
    }
    Ok(())
}

/// Diff every generated file against the committed copy under `root`.
///
/// Why: the `--check` path (the CI drift gate) must never write — it only
/// reports what would change, so a stale committed file fails the build
/// instead of silently self-healing in CI. `root` is injected for the same
/// temp-dir-testability reason as [`write`](crate::generate::write).
/// What: returns the list of mismatched/missing relative paths, each
/// annotated with why it drifted; an empty vec means every generated file
/// matches its committed copy exactly (byte-for-byte).
/// Test: `diff_reports_missing_file_against_empty_dir`,
/// `write_then_diff_round_trips_clean`.
pub(crate) fn diff(set: &GeneratedSet, root: &Path) -> Vec<String> {
    let mut drifted = Vec::new();
    for (rel_path, content) in set {
        let target = root.join(rel_path);
        match std::fs::read_to_string(&target) {
            Ok(existing) if &existing == content => {}
            Ok(_) => drifted.push(format!("{rel_path} (content differs)")),
            // #7776: name the path read, so a wrong root is visible as one.
            Err(_) => drifted.push(format!("{rel_path} (not found at {})", target.display())),
        }
    }
    drifted
}

/// `tm generate capabilities[--check]` — the CLI entry point.
///
/// Why: `commands::generate::generate` (the thin CLI handler) delegates here
/// so the generation engine stays independently unit-testable without
/// clap/anyhow plumbing in every submodule.
/// What: resolves the asset directory from the current directory's git root
/// ([`resolve_skills_asset_dir`], #7776), then [`run_capabilities_in`].
/// Test: exercised end-to-end by `scripts/check_capabilities.sh` against the
/// committed output; unit coverage is per-submodule + [`diff`]/[`write`](crate::generate::write).
pub(crate) fn run_capabilities(check: bool) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("tm-capabilities: cannot read the current directory: {e}"))?;
    run_capabilities_in(&cwd, check)
}

/// [`run_capabilities`] against an explicit starting directory.
///
/// What: reads the agent roster from that same checkout's `content/` (#9011;
/// never the installed bundle), then, without `check`, writes the freshly
/// generated set under the checkout holding `start` and reports the file count. With `check`, diffs instead of
/// writing and returns an error (non-zero exit) listing every drifted file when
/// the set is not clean.
/// Test: `check_reads_the_checkout_holding_the_cwd_7776`.
pub(crate) fn run_capabilities_in(start: &Path, check: bool) -> anyhow::Result<()> {
    let root = resolve_skills_asset_dir(start)?;
    // `root` is `<checkout>/crates/trusty-mpm/src/assets/skills`.
    let checkout = root
        .ancestors()
        .find(|d| d.join(".git").exists())
        .unwrap_or(&root);
    let content = trusty_agents_common::agent_content::checkout_content(checkout)?;
    let roster = trusty_mpm::core::content_source::AgentRoster::load(&content)?;
    run_capabilities_at(&root, check, &roster)
}

/// [`run_capabilities_in`] with the asset directory and roster resolved.
pub(crate) fn run_capabilities_at(
    root: &Path,
    check: bool,
    roster: &trusty_mpm::core::content_source::AgentRoster,
) -> anyhow::Result<()> {
    let set = generate(roster);
    if check {
        let drifted = diff(&set, root);
        if drifted.is_empty() {
            println!(
                "tm-capabilities: up to date ({} generated files).",
                set.len()
            );
            Ok(())
        } else {
            eprintln!(
                "tm-capabilities: drift detected in {} file(s):",
                drifted.len()
            );
            for d in &drifted {
                eprintln!("  - {d}");
            }
            eprintln!(
                "\nRun `tm generate capabilities` (no --check) to regenerate, then commit the diff."
            );
            anyhow::bail!("tm-capabilities drift check failed");
        }
    } else {
        write(&set, root)?;
        println!(
            "tm-capabilities: wrote {} generated files under {}/tm-capabilities*",
            set.len(),
            root.display()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_set_has_seven_entries() {
        let set = generate(crate::commands::install::test_roster_ref());
        assert_eq!(set.len(), 7);
        assert!(set.contains_key("tm-capabilities.md"));
        assert!(set.contains_key("tm-capabilities/references/cli.md"));
        assert!(set.contains_key("tm-capabilities/references/mcp-tools.md"));
        assert!(set.contains_key("tm-capabilities/references/agents.md"));
        assert!(set.contains_key("tm-capabilities/references/skills.md"));
        assert!(set.contains_key("tm-capabilities/references/doctor.md"));
        assert!(set.contains_key("tm-capabilities/references/framework.md"));
    }

    #[test]
    fn generated_set_is_deterministic() {
        let a = generate(crate::commands::install::test_roster_ref());
        let b = generate(crate::commands::install::test_roster_ref());
        assert_eq!(a, b);
    }

    #[test]
    fn generated_set_no_content_is_empty() {
        for (path, content) in generate(crate::commands::install::test_roster_ref()) {
            assert!(!content.trim().is_empty(), "{path} generated empty content");
        }
    }

    #[test]
    fn diff_reports_missing_file_against_empty_dir() {
        // A generated set diffed against a fresh temp dir that certainly
        // doesn't contain it — proves `diff`'s "missing" branch fires. Never
        // touches the real committed assets.
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut set = GeneratedSet::new();
        set.insert("tm-capabilities/whatever.md", "content".to_string());
        let drifted = diff(&set, tmp.path());
        assert_eq!(drifted.len(), 1);
        assert!(drifted[0].contains("not found at"), "{drifted:?}");
    }

    /// A checkout root under `root`: a `.git` entry — a directory, or the file
    /// a worktree carries — plus the skills asset directory.
    fn fake_checkout(root: &Path, git_is_file: bool) {
        std::fs::create_dir_all(root.join(SKILLS_ASSET_REL)).expect("asset dir");
        if git_is_file {
            std::fs::write(root.join(".git"), "gitdir: /elsewhere\n").expect(".git file");
        } else {
            std::fs::create_dir_all(root.join(".git")).expect(".git dir");
        }
    }

    /// Why (#7776): a worktree nested in a main checkout, entered from a
    /// subdirectory, resolves to ITS OWN assets — not the enclosing checkout's
    /// and not the build checkout baked in at compile time.
    #[test]
    fn resolves_the_cwd_git_root_not_the_build_checkout_7776() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let main = tmp.path().join("trusty-tools");
        let worktree = main.join(".claude/worktrees/agent-x");
        fake_checkout(&main, false);
        fake_checkout(&worktree, true);

        let resolved =
            resolve_skills_asset_dir(&worktree.join("crates/trusty-mpm/src")).expect("worktree");
        assert_eq!(resolved, worktree.join(SKILLS_ASSET_REL));
        assert_eq!(
            resolve_skills_asset_dir(&main.join("crates")).expect("main checkout"),
            main.join(SKILLS_ASSET_REL)
        );
    }

    /// Why (#7776): `--check` must judge the checkout it runs in. An empty
    /// asset directory there fails, naming that path — the build checkout's
    /// clean files must not answer for it.
    #[test]
    fn check_reads_the_checkout_holding_the_cwd_7776() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let worktree = tmp.path().join("wt");
        fake_checkout(&worktree, true);

        // #9011: the fake checkout carries no `content/`; the roster is the
        // real one, and the asset root is still resolved from the cwd.
        let root = resolve_skills_asset_dir(&worktree).expect("worktree");
        let roster = crate::commands::install::test_roster_ref();
        let err =
            run_capabilities_at(&root, true, roster).expect_err("nothing generated there yet");
        assert!(err.to_string().contains("drift check failed"), "{err:#}");

        run_capabilities_at(&root, false, roster).expect("write into the worktree");
        run_capabilities_at(&root, true, roster).expect("a clean worktree passes --check");
    }

    /// Why (#7776): a directory the resolver cannot place is a path error that
    /// says so, never seven "(missing)" drift lines.
    #[test]
    fn unresolvable_asset_root_is_a_path_error_7776() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bare = tmp.path().join("not-a-repo");
        std::fs::create_dir_all(bare.join(".git")).expect(".git");
        let err = resolve_skills_asset_dir(&bare).expect_err("no asset dir");
        let msg = err.to_string();
        assert!(msg.contains("cannot resolve the asset directory"), "{msg}");
        assert!(msg.contains(SKILLS_ASSET_REL), "{msg}");
    }

    #[test]
    fn diff_reports_content_mismatch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("tm-capabilities.md"), "stale content").unwrap();
        let mut set = GeneratedSet::new();
        set.insert("tm-capabilities.md", "fresh content".to_string());
        let drifted = diff(&set, tmp.path());
        assert_eq!(drifted.len(), 1);
        assert!(drifted[0].contains("content differs"), "{drifted:?}");
    }

    #[test]
    fn write_then_diff_round_trips_clean() {
        // `write` + `diff` round-trip against an isolated temp dir: after
        // `write`, a fresh `diff` against the just-written files must report
        // no drift (since `generate(crate::commands::install::test_roster_ref())` is deterministic — see
        // `generated_set_is_deterministic`). Never touches the real
        // committed assets — see `scripts/check_capabilities.sh` for the
        // check that DOES compare against the real committed output.
        let tmp = tempfile::tempdir().expect("tempdir");
        let set = generate(crate::commands::install::test_roster_ref());
        write(&set, tmp.path()).expect("write succeeds");
        let drifted = diff(&set, tmp.path());
        assert!(
            drifted.is_empty(),
            "unexpected drift after write: {drifted:?}"
        );
    }
}

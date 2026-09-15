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
//! writes it to the invoking checkout's `crates/trusty-mpm/src/assets/skills/`
//! ([`resolve_skills_asset_dir`]); [`diff`] compares it
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
pub(crate) fn generate() -> GeneratedSet {
    let mut set = GeneratedSet::new();
    set.insert("tm-capabilities.md", entry::render());
    set.insert("tm-capabilities/references/cli.md", cli_tree::render());
    set.insert(
        "tm-capabilities/references/mcp-tools.md",
        mcp_tools::render(),
    );
    set.insert("tm-capabilities/references/agents.md", agents::render());
    set.insert("tm-capabilities/references/skills.md", skills::render());
    set.insert("tm-capabilities/references/doctor.md", doctor::render());
    set.insert(
        "tm-capabilities/references/framework.md",
        framework::render(),
    );
    set
}

/// Asset directory, relative to a trusty-tools checkout root.
const SKILLS_ASSET_REL: &str = "crates/trusty-mpm/src/assets/skills";

/// Resolve the skills asset directory of the trusty-tools checkout that
/// contains `start`.
///
/// Why: an installed `tm` is routinely built from a worktree, so the
/// compile-time `CARGO_MANIFEST_DIR` names the build checkout, not the one
/// the command runs in. Anchoring on it made `--check` false-report drift and
/// made the write path dirty an unrelated, finished worktree (#7776).
/// What: walks `start` and its ancestors; the nearest directory holding both
/// `Cargo.toml` and `crates/trusty-mpm/src/assets/skills/` is the checkout
/// root, and its asset directory is returned. Nearest wins, so a worktree
/// under `.claude/worktrees/` resolves to itself, not the enclosing main
/// checkout. With no match it errors naming what it looked for; it never
/// falls back to the build path, for writes or for `--check`.
/// Test: `resolves_nearest_checkout_from_nested_subdirectory`,
/// `errors_outside_any_checkout`.
pub(crate) fn resolve_skills_asset_dir(start: &Path) -> anyhow::Result<PathBuf> {
    // #7776: resolve from the invocation's checkout, never env!("CARGO_MANIFEST_DIR").
    start
        .ancestors()
        .find(|dir| dir.join("Cargo.toml").is_file() && dir.join(SKILLS_ASSET_REL).is_dir())
        .map(|root| root.join(SKILLS_ASSET_REL))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no trusty-tools checkout found at or above {}: looked for a directory \
                 containing both Cargo.toml and {SKILLS_ASSET_REL}/. Run `tm generate \
                 capabilities` from inside a trusty-tools checkout or worktree.",
                start.display()
            )
        })
}

/// Write every generated file to disk under `root`.
///
/// Why: the non-`--check` path — regenerating and committing the output is
/// how a maintainer picks up a new CLI command, MCP tool, agent, or skill.
/// `root` is injected (rather than hard-coding [`resolve_skills_asset_dir`]) so tests
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
            Err(_) => drifted.push(format!("{rel_path} (missing)")),
        }
    }
    drifted
}

/// `tm generate capabilities[--check]` — the CLI entry point.
///
/// Why: `commands::generate::generate` (the thin CLI handler) delegates here
/// so the generation engine stays independently unit-testable without
/// clap/anyhow plumbing in every submodule.
/// What: resolves the asset directory from the current working directory's
/// checkout ([`resolve_skills_asset_dir`]) and errors outside one. Without
/// `check`, writes the freshly generated set and reports the file count. With
/// `check`, diffs instead of writing and returns an error (non-zero exit)
/// listing every drifted file when the set is not clean.
/// Test: exercised end-to-end by `scripts/check_capabilities.sh` against the
/// committed output; unit coverage is per-submodule, [`diff`], [`write`](crate::generate::write),
/// and [`resolve_skills_asset_dir`].
pub(crate) fn run_capabilities(check: bool) -> anyhow::Result<()> {
    let set = generate();
    let cwd = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("cannot read the current directory: {e}"))?;
    let root = resolve_skills_asset_dir(&cwd)?;
    if check {
        let drifted = diff(&set, &root);
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
        write(&set, &root)?;
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
        let set = generate();
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
        let a = generate();
        let b = generate();
        assert_eq!(a, b);
    }

    #[test]
    fn generated_set_no_content_is_empty() {
        for (path, content) in generate() {
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
        assert!(drifted[0].contains("missing"), "{drifted:?}");
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

    /// Lay out a minimal checkout root under `root`: `Cargo.toml` plus the
    /// skills asset directory.
    fn fake_checkout(root: &Path) {
        std::fs::create_dir_all(root.join(SKILLS_ASSET_REL)).expect("asset dir");
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("Cargo.toml");
    }

    #[test]
    fn resolves_nearest_checkout_from_nested_subdirectory() {
        // #7776: a worktree nested inside a main checkout, started from a
        // subdirectory of the worktree, resolves to the worktree's assets.
        let tmp = tempfile::tempdir().expect("tempdir");
        let main = tmp.path().join("trusty-tools");
        let worktree = main.join(".claude/worktrees/agent-x");
        fake_checkout(&main);
        fake_checkout(&worktree);
        let start = worktree.join("crates/trusty-mpm/src");

        let resolved = resolve_skills_asset_dir(&start).expect("inside a checkout");

        assert_eq!(resolved, worktree.join(SKILLS_ASSET_REL));
        assert_ne!(
            resolved,
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src/assets/skills")
        );
        assert_eq!(
            resolve_skills_asset_dir(&main.join("crates")).expect("main checkout"),
            main.join(SKILLS_ASSET_REL)
        );
    }

    #[test]
    fn errors_outside_any_checkout() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let start = tmp.path().join("not/a/checkout");
        std::fs::create_dir_all(&start).expect("dirs");

        let err = resolve_skills_asset_dir(&start)
            .expect_err("no checkout above a bare temp dir")
            .to_string();

        assert!(err.contains("no trusty-tools checkout found"), "{err}");
        assert!(err.contains(SKILLS_ASSET_REL), "{err}");
    }

    #[test]
    fn write_then_diff_round_trips_clean() {
        // `write` + `diff` round-trip against an isolated temp dir: after
        // `write`, a fresh `diff` against the just-written files must report
        // no drift (since `generate()` is deterministic — see
        // `generated_set_is_deterministic`). Never touches the real
        // committed assets — see `scripts/check_capabilities.sh` for the
        // check that DOES compare against the real committed output.
        let tmp = tempfile::tempdir().expect("tempdir");
        let set = generate();
        write(&set, tmp.path()).expect("write succeeds");
        let drifted = diff(&set, tmp.path());
        assert!(
            drifted.is_empty(),
            "unexpected drift after write: {drifted:?}"
        );
    }
}

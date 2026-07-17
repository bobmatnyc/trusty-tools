//! Self-healing materialization of the bundled skill *source* directory
//! (`~/.trusty-mpm/framework/skills/`) — issue #1917.
//!
//! Why: [`crate::core::paths::FrameworkPaths::skill_source_dir`] falls back to
//! `~/.trusty-mpm/framework/skills/` whenever no `agents/skills` git submodule
//! is checked out — the normal case for every binary-only install. That
//! directory was previously populated ONLY by the separate, explicit
//! `tm install` command, which only ever ADDS files: an already-existing one
//! survives untouched unless the operator passes `--force`
//! (`install.rs::install_to`). So a skill rename (e.g. the mpm-*→tm-* rename,
//! #1905) left the OLD file sitting alongside the new one, and a machine that
//! never ran `tm install` under the current binary at all had an empty or
//! missing directory. Either way, `deploy_skills_filtered` silently deployed
//! zero (or stale) skills at session-launch time — no error, no warning,
//! nothing (#1917). Session preparation must not depend on a prior manual
//! `tm install` having populated this directory correctly.
//! What: [`skill_bundle_stamp`] fingerprints the compiled-in `skills/*` slice
//! of [`bundle::ALL`] via sha256 — `bundle_all.rs` has no existing
//! version/checksum concept, so this is the marker this module introduces.
//! [`materialize_skill_artifacts`] writes every bundled skill file into
//! `paths.skills`, pruning any `.md` file on disk the table no longer lists
//! (renamed/removed skills). Since issue #2903 this includes multi-file
//! skills' nested `<stem>/references/<file>.md` artifacts, materialized under
//! a matching nested directory (not flattened) and pruned recursively via
//! [`prune_orphaned_skill_files`] — the pre-#2903 top-level-only sweep never
//! descended into skill subdirectories. [`ensure_skill_source_fresh`] compares the
//! current stamp against a marker file written alongside the source
//! directory and re-materializes it only when they differ (including "never
//! materialized at all"), so callers can invoke it unconditionally and cheap
//! on every session-prep run. It deliberately never touches the `agents/skills`
//! git submodule path — that directory is git-tracked and authoritative on
//! its own.
//! Test: `skill_bundle_stamp_is_stable_across_calls`,
//! `materialize_skill_artifacts_writes_all_skills`,
//! `materialize_skill_artifacts_prunes_files_not_in_table`,
//! `materialize_skill_artifacts_writes_nested_reference_files`,
//! `materialize_skill_artifacts_prunes_stale_reference_files`,
//! `ensure_skill_source_fresh_materializes_when_missing`,
//! `ensure_skill_source_fresh_is_noop_when_current`,
//! `ensure_skill_source_fresh_prunes_renamed_files`,
//! `ensure_skill_source_fresh_skips_submodule_source`.

use std::collections::HashSet;

use crate::core::agent_manifest::{atomic_write, checksum};
use crate::core::bundle;
use crate::core::error::Result;
use crate::core::paths::FrameworkPaths;

/// Marker file recording the last-materialized bundle stamp, written
/// directly under the skill source directory (`paths.skills/.bundle-stamp`).
const STAMP_FILE_NAME: &str = ".bundle-stamp";

/// Compute a stable sha256 fingerprint over every `skills/*` entry in the
/// compiled-in [`bundle::ALL`] table.
///
/// Why: this fingerprint is how [`ensure_skill_source_fresh`] detects "this
/// binary embeds different skill content than what is on disk" without
/// depending on file mtimes, which a plain backup/restore can shuffle.
/// What: concatenates `<rel_path>\0<contents>\n` for every table entry whose
/// `rel_path` starts with `"skills/"`, in table order (stable — see
/// `bundle_tests::bundle_table_is_complete`), and returns the sha256 hex
/// digest via [`checksum`].
/// Test: `skill_bundle_stamp_is_stable_across_calls`.
pub fn skill_bundle_stamp() -> String {
    let mut buf = String::new();
    for artifact in bundle::ALL
        .iter()
        .filter(|a| a.rel_path.starts_with("skills/"))
    {
        buf.push_str(artifact.rel_path);
        buf.push('\0');
        buf.push_str(artifact.contents);
        buf.push('\n');
    }
    checksum(&buf)
}

/// Write every bundled `skills/*` artifact into `paths.skills`, pruning any
/// top-level `.md` file on disk the current table no longer lists.
///
/// Why: `paths.skills` is a framework-owned artifact directory — every entry
/// in it is written exclusively by trusty-mpm's own installer/self-heal path,
/// never hand-edited — so it is safe (and necessary, for the renamed-skill
/// case) to prune files the current binary no longer embeds, not just add new
/// ones.
/// What: creates `paths.skills` if absent, atomically (over)writes each
/// `skills/*` artifact's contents to `<paths.skills>/<basename>` (always,
/// regardless of prior content — this is the authoritative re-materialization
/// path, not the user-facing incremental `tm install`), then removes any
/// existing top-level `*.md` file whose name is not one of the artifacts just
/// written. Hidden files (leading `.`, e.g. the stamp marker) are never
/// touched. An artifact whose basename (stem, `.md`-stripped) contains "mcp"
/// (case-insensitive) is skipped entirely — never written, and any stale copy
/// already on disk is pruned by the sweep below (exercised indirectly by
/// `materialize_skill_artifacts_prunes_files_not_in_table`, which asserts the
/// prune sweep removes a file absent from `keep`) — as a defense-in-depth
/// mirror of the `skill_deployer` deploy guard (#2186): such a name would
/// shadow Claude Code's built-in `/mcp` command once deployed. The compiled-in
/// `bundle::ALL` table currently contains no such artifact, so this branch has
/// no dedicated fixture-backed unit test here; coverage lives in
/// `skill_deployer::tests::deploy_skips_mcp_named_skill`, which exercises the
/// identical stem-matching rule against an arbitrary source directory. Returns
/// the basenames written.
/// Test: `materialize_skill_artifacts_writes_all_skills`,
/// `materialize_skill_artifacts_prunes_files_not_in_table`.
pub fn materialize_skill_artifacts(paths: &FrameworkPaths) -> Result<Vec<String>> {
    std::fs::create_dir_all(&paths.skills)?;

    let mut written = Vec::new();
    let mut keep: HashSet<String> = HashSet::new();
    for artifact in bundle::ALL
        .iter()
        .filter(|a| a.rel_path.starts_with("skills/"))
    {
        let basename = artifact
            .rel_path
            .strip_prefix("skills/")
            .unwrap_or(artifact.rel_path);

        // A multi-file skill's `references/*.md` siblings (issue #2903) embed
        // as nested rel_paths (`<stem>/references/<file>.md`); the mcp-shadow
        // guard only ever applies to the skill's own name — the first path
        // segment — never to a reference filename.
        let stem = basename
            .split('/')
            .next()
            .unwrap_or(basename)
            .strip_suffix(".md")
            .unwrap_or(basename);

        // Defense-in-depth mirror of the `skill_deployer::deploy_skills_filtered`
        // guard (#2186): a bundled skill whose basename (stem, `.md`-stripped)
        // contains "mcp" would shadow Claude Code's built-in `/mcp` command
        // once deployed. Refuse to even materialize it into the framework
        // source dir, so a bad name never reaches the deploy step at all.
        if stem.to_lowercase().contains("mcp") {
            tracing::warn!(
                skill = %stem,
                "refusing to materialize bundled skill whose name contains \"mcp\" — it would shadow Claude Code's built-in /mcp command"
            );
            continue;
        }

        let dest = paths.skills.join(basename);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        atomic_write(&dest, artifact.contents)?;
        keep.insert(basename.to_string());
        written.push(basename.to_string());
    }

    prune_orphaned_skill_files(&paths.skills, &paths.skills, &keep)?;

    Ok(written)
}

/// Recursively remove any `.md` file under `dir` whose path (relative to
/// `root`, `/`-separated) is not in `keep`, then remove any directory left
/// empty by that pruning.
///
/// Why: [`materialize_skill_artifacts`] must prune stale entries for renamed
/// or removed skills (#1917) AND, since #2903 added nested
/// `<stem>/references/<file>.md` artifacts, stale reference files and their
/// now-empty `<stem>/references/` or `<stem>/` directories — a flat top-level
/// `read_dir` sweep (the pre-#2903 implementation) only ever saw top-level
/// `.md` files and never descended into skill subdirectories.
/// What: hidden entries (leading `.`, e.g. the bundle-stamp marker) are never
/// touched; every other file is removed unless its root-relative path is in
/// `keep`; every directory is recursed into first, then removed if left with
/// no entries.
/// Test: `materialize_skill_artifacts_prunes_files_not_in_table`,
/// `materialize_skill_artifacts_prunes_stale_reference_files`.
fn prune_orphaned_skill_files(
    dir: &std::path::Path,
    root: &std::path::Path,
    keep: &HashSet<String>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        if entry.file_type()?.is_dir() {
            prune_orphaned_skill_files(&path, root, keep)?;
            if std::fs::read_dir(&path)?.next().is_none() {
                std::fs::remove_dir(&path)?;
            }
        } else if name.ends_with(".md") {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            if !keep.contains(rel.as_str()) {
                std::fs::remove_file(&path)?;
            }
        }
    }
    Ok(())
}

/// Ensure `paths.skills` reflects the binary's currently-embedded skill
/// bundle, re-materializing it when missing or stale.
///
/// Why: this is the self-healing entry point session preparation calls
/// before deploying skills (#1917) — it removes the dependency on a prior,
/// separate `tm install` run having populated (or refreshed) the source
/// directory.
/// What: no-ops when [`FrameworkPaths::skill_source_dir`] would resolve to
/// the `agents/skills` git submodule instead of `paths.skills` (mirrors that
/// method's own submodule-precedence check) — a populated submodule is
/// git-tracked and authoritative on its own, so overwriting it here would be
/// destructive and wrong. Otherwise compares [`skill_bundle_stamp`] against
/// `<paths.skills>/.bundle-stamp`; when they differ (including "stamp file
/// absent", which covers a missing or never-materialized directory), calls
/// [`materialize_skill_artifacts`] and rewrites the stamp file. Returns
/// `true` when a refresh happened, `false` when the source was already
/// current or the submodule path is in play.
/// Test: `ensure_skill_source_fresh_materializes_when_missing`,
/// `ensure_skill_source_fresh_is_noop_when_current`,
/// `ensure_skill_source_fresh_prunes_renamed_files`,
/// `ensure_skill_source_fresh_skips_submodule_source`.
pub fn ensure_skill_source_fresh(paths: &FrameworkPaths) -> Result<bool> {
    if paths.skill_source_dir() != paths.skills {
        return Ok(false);
    }

    let stamp_path = paths.skills.join(STAMP_FILE_NAME);
    let current = skill_bundle_stamp();
    let on_disk = std::fs::read_to_string(&stamp_path).ok();
    if on_disk.as_deref() == Some(current.as_str()) {
        return Ok(false);
    }

    materialize_skill_artifacts(paths)?;
    atomic_write(&stamp_path, &current)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_bundle_stamp_is_stable_across_calls() {
        // The fingerprint is a pure function of the compiled-in table, so two
        // calls in the same process must agree.
        assert_eq!(skill_bundle_stamp(), skill_bundle_stamp());
    }

    #[test]
    fn materialize_skill_artifacts_writes_all_skills() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = FrameworkPaths::under(tmp.path());

        let written = materialize_skill_artifacts(&paths).unwrap();
        let expected: Vec<&str> = bundle::ALL
            .iter()
            .filter(|a| a.rel_path.starts_with("skills/"))
            .map(|a| a.rel_path.strip_prefix("skills/").unwrap())
            .collect();
        assert_eq!(written.len(), expected.len());
        for name in expected {
            let content = std::fs::read_to_string(paths.skills.join(name))
                .unwrap_or_else(|e| panic!("missing {name}: {e}"));
            let artifact = bundle::ALL
                .iter()
                .find(|a| a.rel_path == format!("skills/{name}"))
                .unwrap();
            assert_eq!(content, artifact.contents);
        }
    }

    #[test]
    fn materialize_skill_artifacts_prunes_files_not_in_table() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        std::fs::create_dir_all(&paths.skills).unwrap();
        // Simulate a leftover pre-rename skill file the current table no
        // longer lists (e.g. an old `mpm-*.md` under the framework source).
        std::fs::write(paths.skills.join("mpm-old-skill.md"), "stale\n").unwrap();

        materialize_skill_artifacts(&paths).unwrap();

        assert!(!paths.skills.join("mpm-old-skill.md").exists());
        assert!(paths.skills.join("tm-doctor.md").exists());
    }

    #[test]
    fn materialize_skill_artifacts_writes_nested_reference_files() {
        // Issue #2903: a multi-file skill's `skills/<stem>/references/<file>.md`
        // entries must materialize under a nested directory, not flattened —
        // proven here against a real batch-1 skill (`systematic-debugging`)
        // rather than a synthetic fixture, so the assertion pins the actual
        // production layout.
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = FrameworkPaths::under(tmp.path());

        materialize_skill_artifacts(&paths).unwrap();

        let workflow = paths
            .skills
            .join("systematic-debugging")
            .join("references")
            .join("workflow.md");
        assert!(
            workflow.is_file(),
            "expected nested reference file at {workflow:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&workflow).unwrap(),
            crate::core::bundle::SYSTEMATIC_DEBUGGING_WORKFLOW
        );
    }

    #[test]
    fn materialize_skill_artifacts_prunes_stale_reference_files() {
        // A reference file (and its now-empty parent directories) left over
        // from a renamed/removed skill must be pruned, not just top-level
        // `.md` files — the pre-#2903 flat `read_dir` sweep never descended
        // into skill subdirectories.
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        let stale_refs = paths.skills.join("removed-skill").join("references");
        std::fs::create_dir_all(&stale_refs).unwrap();
        std::fs::write(stale_refs.join("old.md"), "stale\n").unwrap();

        materialize_skill_artifacts(&paths).unwrap();

        assert!(
            !paths.skills.join("removed-skill").exists(),
            "the entire orphaned skill directory must be pruned, including now-empty dirs"
        );
    }

    #[test]
    fn ensure_skill_source_fresh_materializes_when_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        assert!(!paths.skills.exists());

        let refreshed = ensure_skill_source_fresh(&paths).unwrap();

        assert!(refreshed, "a missing source dir must trigger a refresh");
        assert!(paths.skills.join("tm-doctor.md").exists());
        assert!(paths.skills.join(STAMP_FILE_NAME).exists());
    }

    #[test]
    fn ensure_skill_source_fresh_is_noop_when_current() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        assert!(ensure_skill_source_fresh(&paths).unwrap());

        let marker = paths.skills.join("tm-doctor.md");
        let before = std::fs::metadata(&marker).unwrap().modified().unwrap();

        let refreshed = ensure_skill_source_fresh(&paths).unwrap();

        assert!(!refreshed, "an already-current source dir must be a no-op");
        let after = std::fs::metadata(&marker).unwrap().modified().unwrap();
        assert_eq!(before, after, "a no-op refresh must not rewrite files");
    }

    #[test]
    fn ensure_skill_source_fresh_prunes_renamed_files() {
        // Simulates the exact #1917 symptom: an operator's existing
        // `~/.trusty-mpm/framework/skills/` predates a skill rename and holds
        // a stale marker (or none at all) plus a leftover old file.
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        std::fs::create_dir_all(&paths.skills).unwrap();
        std::fs::write(paths.skills.join("mpm-old-skill.md"), "stale\n").unwrap();
        std::fs::write(paths.skills.join(STAMP_FILE_NAME), "stale-stamp").unwrap();

        let refreshed = ensure_skill_source_fresh(&paths).unwrap();

        assert!(refreshed, "a stale stamp must trigger a refresh");
        assert!(!paths.skills.join("mpm-old-skill.md").exists());
        assert!(paths.skills.join("tm-doctor.md").exists());
    }

    #[test]
    fn ensure_skill_source_fresh_skips_submodule_source() {
        // When the `agents/skills` git submodule is checked out,
        // `skill_source_dir()` prefers it over `paths.skills` — the self-heal
        // path must never materialize into (or even create) `paths.skills`
        // in that case.
        let tmp = tempfile::TempDir::new().unwrap();
        let submodule_root = tempfile::TempDir::new().unwrap();
        let submodule_skills = submodule_root.path().join("agents").join("skills");
        std::fs::create_dir_all(&submodule_skills).unwrap();

        let mut paths = FrameworkPaths::under(tmp.path());
        paths.trusty_mpm_root = Some(submodule_root.path().to_path_buf());
        assert_eq!(paths.skill_source_dir(), submodule_skills);

        let refreshed = ensure_skill_source_fresh(&paths).unwrap();

        assert!(!refreshed);
        assert!(
            !paths.skills.exists(),
            "must never create paths.skills when a submodule source is in play"
        );
    }
}

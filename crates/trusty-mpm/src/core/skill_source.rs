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
//! What: [`skill_bundle_stamp`] fingerprints the skills of the loaded content
//! (#9012: runtime content, no longer the compiled-in `bundle::ALL` slice) via
//! sha256 — the marker this module introduces.
//! [`materialize_skill_artifacts`] writes every content skill file into
//! `paths.skills`, pruning any `.md` file on disk the content no longer lists
//! (renamed/removed skills). Since issue #2903 this includes multi-file
//! skills' nested `<stem>/references/<file>.md` artifacts, materialized under
//! a matching nested directory (not flattened) and pruned recursively via
//! [`prune_orphaned_skill_files`] — the pre-#2903 top-level-only sweep never
//! descended into skill subdirectories. #10211: a skill's carried files are
//! any extension (`references/*.yml`, `scripts/*.sh`); `scripts/*` is written
//! `0o755`, and a `.materialized` ledger of what this module wrote is what lets
//! the prune remove a stale non-`.md` file without touching a file it never
//! wrote. [`ensure_skill_source_fresh`] compares the
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
//! `materialize_writes_scripts_executable_and_non_md_references`,
//! `materialize_prunes_an_orphaned_script_it_wrote`,
//! `materialize_keeps_a_file_it_never_wrote`,
//! `ensure_skill_source_fresh_materializes_when_missing`,
//! `ensure_skill_source_fresh_is_noop_when_current`,
//! `ensure_skill_source_fresh_prunes_renamed_files`,
//! `ensure_skill_source_fresh_skips_submodule_source`.

use std::collections::HashSet;
use std::path::Path;

use crate::core::agent_manifest::{atomic_write, atomic_write_executable, checksum};
use crate::core::error::Result;
use crate::core::framework_content::FrameworkContent;
use crate::core::paths::FrameworkPaths;
use crate::core::skill_deployer::is_executable_skill_file;

/// Marker file recording the last-materialized bundle stamp, written
/// directly under the skill source directory (`paths.skills/.bundle-stamp`).
///
/// #4873: `pub(crate)` so a test can pin a hand-seeded skill source as
/// already-current and stop [`ensure_skill_source_fresh`] materializing the
/// content's skills over it.
pub(crate) const STAMP_FILE_NAME: &str = ".bundle-stamp";

/// Ledger of every file [`materialize_skill_artifacts`] last wrote, one
/// root-relative path per line, under `paths.skills/.materialized` (#10211).
///
/// Why: the prune removes a non-`.md` file only when this ledger names it, so
/// a renamed skill's `scripts/*.sh` goes while a file tm never wrote stays.
pub(crate) const LEDGER_FILE_NAME: &str = ".materialized";

/// Compute a stable sha256 fingerprint over every skill file of `content`.
///
/// Why: this fingerprint is how [`ensure_skill_source_fresh`] detects "the
/// installed content carries different skills than what is on disk" without
/// depending on file mtimes, which a plain backup/restore can shuffle.
/// What: concatenates `<bundle path>\0<contents>\n` for every skill of
/// `content`, in bundle-path order (#9012: was the compiled-in table order),
/// and returns the sha256 hex digest via [`checksum`].
/// Test: `skill_bundle_stamp_is_stable_across_calls`.
pub fn skill_bundle_stamp(content: &FrameworkContent) -> String {
    let mut buf = String::new();
    for (rel_path, contents) in content.skills() {
        buf.push_str(rel_path);
        buf.push('\0');
        buf.push_str(contents);
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
/// path, not the user-facing incremental `tm install`), a `<stem>/scripts/*`
/// artifact as `0o755` (#10211, [`is_executable_skill_file`]), then removes
/// any existing `*.md` file, or any file the previous run's
/// [`LEDGER_FILE_NAME`] ledger names, whose path is not one of the artifacts
/// just written, and rewrites the ledger. Hidden files (leading `.`, e.g. the
/// stamp marker) are never
/// touched. An artifact whose OWN stem (the first path segment, `.md`-stripped
/// — never a nested `references/*.md` sibling's filename) contains "mcp"
/// (case-insensitive) is skipped entirely — never written, and any stale copy
/// already on disk is pruned by the sweep below (exercised indirectly by
/// `materialize_skill_artifacts_prunes_files_not_in_table`, which asserts the
/// prune sweep removes a file absent from `keep`) — as a defense-in-depth
/// mirror of the `skill_deployer` deploy guard (#2186): such a name would
/// shadow Claude Code's built-in `/mcp` command once deployed. Issue #2913
/// added the first `bundle::ALL` artifact that exercises the nested-path edge
/// of this guard (`tm-capabilities/references/mcp-tools.md` — a reference
/// filename containing "mcp" whose owning skill's stem, `tm-capabilities`,
/// does not); a prior version of the stem computation incorrectly widened the
/// guard to the full nested path in that case, silently refusing to
/// materialize the file. Returns the basenames written.
/// Test: `materialize_skill_artifacts_writes_all_skills`,
/// `materialize_skill_artifacts_prunes_files_not_in_table`,
/// `materialize_skill_artifacts_does_not_shadow_guard_on_reference_filename`,
/// `materialize_writes_scripts_executable_and_non_md_references`,
/// `materialize_prunes_an_orphaned_script_it_wrote`,
/// `materialize_keeps_a_file_it_never_wrote`.
pub fn materialize_skill_artifacts(
    paths: &FrameworkPaths,
    content: &FrameworkContent,
) -> Result<Vec<String>> {
    std::fs::create_dir_all(&paths.skills)?;

    let mut written = Vec::new();
    let mut keep: HashSet<String> = HashSet::new();
    // #9012: the skills are runtime content.
    for (rel_path, contents) in content.skills() {
        let basename = rel_path.strip_prefix("skills/").unwrap_or(rel_path);

        // A multi-file skill's `references/*.md` siblings (issue #2903) embed
        // as nested rel_paths (`<stem>/references/<file>.md`); the mcp-shadow
        // guard only ever applies to the skill's own name — the first path
        // segment — never to a reference filename.
        //
        // Bug fix (issue #2913): the previous chain's final `.unwrap_or(basename)`
        // fell back to the FULL nested path (not the first segment) whenever
        // that first segment didn't itself end in ".md" — which is always true
        // for a `references/*.md` sibling. That silently widened the mcp-shadow
        // check to the entire nested path, so a reference file merely NAMED
        // `mcp-*.md` (e.g. `tm-capabilities/references/mcp-tools.md`) tripped
        // the guard even though the owning skill's own stem (`tm-capabilities`)
        // never contained "mcp". Binding the first segment once and stripping
        // `.md` from THAT (not from `basename`) fixes both the single-file case
        // (`foo.md` -> `foo`) and the nested case (`foo/references/bar.md` ->
        // `foo`) correctly.
        let first_segment = basename.split('/').next().unwrap_or(basename);
        let stem = first_segment.strip_suffix(".md").unwrap_or(first_segment);

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
        // #10211: `<stem>/scripts/*` lands executable; the tarball's `0644`
        // cannot carry the bit, so the path is the contract.
        if is_executable_artifact(basename) {
            atomic_write_executable(&dest, contents)?;
        } else {
            atomic_write(&dest, contents)?;
        }
        keep.insert(basename.to_string());
        written.push(basename.to_string());
    }

    // #10211: what the previous run wrote is what this run may prune beyond
    // the legacy `.md` rule. An unreadable ledger is an error, never "nothing
    // owned": the prune would then keep stale scripts while the stamp advanced.
    let ledger_path = paths.skills.join(LEDGER_FILE_NAME);
    let owned = read_ledger(&ledger_path)?;
    prune_orphaned_skill_files(&paths.skills, &paths.skills, &keep, &owned)?;
    let mut ledger: Vec<&str> = keep.iter().map(String::as_str).collect();
    ledger.sort_unstable();
    atomic_write(&ledger_path, &ledger.join("\n"))?;

    Ok(written)
}

/// Whether a `skills/`-relative artifact path names a file written `0o755`.
///
/// What: `<stem>/<rel>` with [`is_executable_skill_file`] true for `<rel>`.
/// Test: `materialize_writes_scripts_executable_and_non_md_references`.
fn is_executable_artifact(basename: &str) -> bool {
    basename
        .split_once('/')
        .is_some_and(|(_, rel)| is_executable_skill_file(rel))
}

/// The root-relative paths the previous materialization wrote (#10211).
///
/// What: one path per non-empty line of `ledger_path`; an absent ledger is the
/// empty set (a source dir no run of this version has written yet); any other
/// read error propagates.
/// Test: `materialize_prunes_an_orphaned_script_it_wrote`.
fn read_ledger(ledger_path: &Path) -> Result<HashSet<String>> {
    match std::fs::read_to_string(ledger_path) {
        Ok(text) => Ok(text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashSet::new()),
        Err(e) => Err(e.into()),
    }
}

/// Recursively remove any `.md` file, or any file in `owned`, under `dir`
/// whose path (relative to `root`, `/`-separated) is not in `keep`, then
/// remove any directory left empty by that pruning.
///
/// Why: [`materialize_skill_artifacts`] must prune stale entries for renamed
/// or removed skills (#1917) AND, since #2903 added nested
/// `<stem>/references/<file>.md` artifacts, stale reference files and their
/// now-empty `<stem>/references/` or `<stem>/` directories — a flat top-level
/// `read_dir` sweep (the pre-#2903 implementation) only ever saw top-level
/// `.md` files and never descended into skill subdirectories. #10211: a
/// skill's `scripts/*.sh` or `references/*.yml` is pruned too, but only when
/// the previous run's ledger (`owned`) says this module wrote it — a file tm
/// never wrote is never deleted.
/// What: hidden entries (leading `.`, e.g. the bundle-stamp marker and the
/// ledger) are never touched; a `.md` file, or a file named in `owned`, is
/// removed unless its root-relative path is in `keep`; any other file stays;
/// every directory is recursed into first, then removed if left with no
/// entries. Any read or remove error propagates, so the caller never advances
/// its stamp over a half-pruned tree.
/// Test: `materialize_skill_artifacts_prunes_files_not_in_table`,
/// `materialize_skill_artifacts_prunes_stale_reference_files`,
/// `materialize_prunes_an_orphaned_script_it_wrote`,
/// `materialize_keeps_a_file_it_never_wrote`.
fn prune_orphaned_skill_files(
    dir: &Path,
    root: &Path,
    keep: &HashSet<String>,
    owned: &HashSet<String>,
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
            prune_orphaned_skill_files(&path, root, keep, owned)?;
            if std::fs::read_dir(&path)?.next().is_none() {
                std::fs::remove_dir(&path)?;
            }
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        if keep.contains(rel.as_str()) {
            continue;
        }
        // #10211: non-`.md` files are pruned by ledger, never by extension.
        if name.ends_with(".md") || owned.contains(rel.as_str()) {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Ensure `paths.skills` reflects the skills of `content`, re-materializing
/// it when missing or stale.
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
pub fn ensure_skill_source_fresh(
    paths: &FrameworkPaths,
    content: &FrameworkContent,
) -> Result<bool> {
    if paths.skill_source_dir() != paths.skills {
        return Ok(false);
    }
    // #8545: the refresh below writes the framework source; fenced in tests.
    crate::core::home_write_fence::check(&paths.skills);

    let stamp_path = paths.skills.join(STAMP_FILE_NAME);
    let current = skill_bundle_stamp(content);
    let on_disk = std::fs::read_to_string(&stamp_path).ok();
    if on_disk.as_deref() == Some(current.as_str()) {
        return Ok(false);
    }

    materialize_skill_artifacts(paths, content)?;
    atomic_write(&stamp_path, &current)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::content_source::test_support::repo_content;

    #[test]
    fn skill_bundle_stamp_is_stable_across_calls() {
        // The fingerprint is a pure function of the content's skills, so two
        // calls over the same content must agree.
        assert_eq!(
            skill_bundle_stamp(&repo_content()),
            skill_bundle_stamp(&repo_content())
        );
    }

    #[test]
    fn materialize_skill_artifacts_writes_all_skills() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = FrameworkPaths::under(tmp.path());

        let repo = repo_content();
        let written = materialize_skill_artifacts(&paths, &repo).unwrap();
        let expected: Vec<(&str, &str)> = repo
            .skills()
            .map(|(rel, body)| (rel.strip_prefix("skills/").unwrap(), body))
            .collect();
        assert_eq!(written.len(), expected.len());
        for (name, body) in expected {
            let content = std::fs::read_to_string(paths.skills.join(name))
                .unwrap_or_else(|e| panic!("missing {name}: {e}"));
            assert_eq!(content, body);
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

        materialize_skill_artifacts(&paths, &repo_content()).unwrap();

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

        materialize_skill_artifacts(&paths, &repo_content()).unwrap();

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
            repo_content()
                .skill("skills/systematic-debugging/references/workflow.md")
                .unwrap()
        );
    }

    #[test]
    fn materialize_skill_artifacts_does_not_shadow_guard_on_reference_filename() {
        // Regression (issue #2913): a bug in the old stem computation fell
        // back to the FULL nested rel_path (not just the first path segment)
        // whenever that segment didn't itself end in ".md" — which is always
        // true for a `references/*.md` sibling. That silently widened the
        // mcp-shadow guard (#2186) to the entire nested path, so a reference
        // file merely NAMED `mcp-*.md` (e.g.
        // `tm-capabilities/references/mcp-tools.md`) tripped the guard even
        // though the owning skill's own stem (`tm-capabilities`) never
        // contains "mcp". Pins the real production artifact so a regression
        // fails immediately.
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = FrameworkPaths::under(tmp.path());

        materialize_skill_artifacts(&paths, &repo_content()).unwrap();

        let mcp_tools_ref = paths
            .skills
            .join("tm-capabilities")
            .join("references")
            .join("mcp-tools.md");
        assert!(
            mcp_tools_ref.is_file(),
            "expected tm-capabilities/references/mcp-tools.md to materialize \
             despite its filename containing \"mcp\" — only the skill's own \
             stem should be checked, at {mcp_tools_ref:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&mcp_tools_ref).unwrap(),
            repo_content()
                .skill("skills/tm-capabilities/references/mcp-tools.md")
                .unwrap()
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

        materialize_skill_artifacts(&paths, &repo_content()).unwrap();

        assert!(
            !paths.skills.join("removed-skill").exists(),
            "the entire orphaned skill directory must be pruned, including now-empty dirs"
        );
    }

    /// A fixture skill carrying a script and a non-`.md` reference (#10211).
    const FIXTURE_SKILL: &[(&str, &str)] = &[
        ("tm-fixture.md", "---\nname: tm-fixture\n---\nbody\n"),
        ("tm-fixture/references/guide.md", "# Guide\n"),
        ("tm-fixture/references/adr-check.yml", "rules: []\n"),
        ("tm-fixture/scripts/check.sh", "#!/bin/sh\nexit 0\n"),
    ];

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// #10211: every fixture file materializes byte-identical; `scripts/*` is
    /// `0o755`, every reference is not executable. Fails on the pre-fix code,
    /// which wrote the script `0644`.
    #[cfg(unix)]
    #[test]
    fn materialize_writes_scripts_executable_and_non_md_references() {
        use crate::core::framework_content::tests::content_with_skill_files;
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = FrameworkPaths::under(tmp.path());

        materialize_skill_artifacts(&paths, &content_with_skill_files(FIXTURE_SKILL)).unwrap();

        for (rel, body) in FIXTURE_SKILL {
            let path = paths.skills.join(rel);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), *body, "{rel}");
            let executable = mode_of(&path) & 0o111 != 0;
            assert_eq!(executable, rel.contains("/scripts/"), "{rel} mode");
        }
        assert_eq!(
            mode_of(&paths.skills.join("tm-fixture/scripts/check.sh")),
            0o755
        );
    }

    /// #10211: a non-`.md` file a previous run wrote (named in the ledger) and
    /// the current content no longer lists is pruned, directory included.
    /// Fails on the pre-fix code, whose sweep removed `.md` files only.
    #[test]
    fn materialize_prunes_an_orphaned_script_it_wrote() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        let stale = paths.skills.join("removed-skill").join("scripts");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("old.sh"), "stale\n").unwrap();
        std::fs::write(
            paths.skills.join(LEDGER_FILE_NAME),
            "removed-skill/scripts/old.sh\n",
        )
        .unwrap();

        materialize_skill_artifacts(&paths, &repo_content()).unwrap();

        assert!(
            !paths.skills.join("removed-skill").exists(),
            "an orphaned script this module wrote must be pruned with its directory"
        );
        let ledger = std::fs::read_to_string(paths.skills.join(LEDGER_FILE_NAME)).unwrap();
        assert!(ledger.lines().any(|l| l == "tm-doctor.md"), "{ledger}");
        assert!(!ledger.contains("removed-skill"), "{ledger}");
    }

    /// #10211: a non-`.md` file absent from the ledger was not written by this
    /// module, so the prune leaves it — beside a managed skill or at the root.
    #[test]
    fn materialize_keeps_a_file_it_never_wrote() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        let user_dir = paths.skills.join("tm-doctor").join("scripts");
        std::fs::create_dir_all(&user_dir).unwrap();
        let beside = user_dir.join("mine.sh");
        let root_level = paths.skills.join("notes.txt");
        std::fs::write(&beside, "user\n").unwrap();
        std::fs::write(&root_level, "user\n").unwrap();

        materialize_skill_artifacts(&paths, &repo_content()).unwrap();

        assert!(
            beside.is_file(),
            "a user script beside a managed skill stays"
        );
        assert!(root_level.is_file(), "a user file at the root stays");
        assert!(paths.skills.join("tm-doctor.md").is_file());
    }

    /// #10211: an unreadable ledger stops the run before any prune, so the
    /// caller never writes a stamp over a tree it could not reconcile.
    #[cfg(unix)]
    #[test]
    fn materialize_fails_closed_on_an_unreadable_ledger() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        std::fs::create_dir_all(&paths.skills).unwrap();
        // A directory where the ledger file belongs is unreadable as a file.
        std::fs::create_dir_all(paths.skills.join(LEDGER_FILE_NAME)).unwrap();

        let err = materialize_skill_artifacts(&paths, &repo_content());

        assert!(err.is_err(), "an unreadable ledger must not read as empty");
    }

    #[test]
    fn ensure_skill_source_fresh_materializes_when_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        assert!(!paths.skills.exists());

        let refreshed = ensure_skill_source_fresh(&paths, &repo_content()).unwrap();

        assert!(refreshed, "a missing source dir must trigger a refresh");
        assert!(paths.skills.join("tm-doctor.md").exists());
        assert!(paths.skills.join(STAMP_FILE_NAME).exists());
    }

    #[test]
    fn ensure_skill_source_fresh_is_noop_when_current() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        assert!(ensure_skill_source_fresh(&paths, &repo_content()).unwrap());

        let marker = paths.skills.join("tm-doctor.md");
        let before = std::fs::metadata(&marker).unwrap().modified().unwrap();

        let refreshed = ensure_skill_source_fresh(&paths, &repo_content()).unwrap();

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

        let refreshed = ensure_skill_source_fresh(&paths, &repo_content()).unwrap();

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

        let refreshed = ensure_skill_source_fresh(&paths, &repo_content()).unwrap();

        assert!(!refreshed);
        assert!(
            !paths.skills.exists(),
            "must never create paths.skills when a submodule source is in play"
        );
    }
}

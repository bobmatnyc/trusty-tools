//! Skill deployment — writes skill directories into `~/.claude/skills/`.
//!
//! Why: Claude Code discovers skills from `~/.claude/skills/<name>/SKILL.md`
//! (directory per skill, entry-point file named `SKILL.md`). trusty-mpm must
//! keep that directory populated with up-to-date skills in that format, while
//! never destroying files the user owns or has hand-edited. Skills carry no
//! inheritance, so — unlike agents — deployment is a plain content copy, but
//! the manifest-based ownership tracking is identical.
//! What: [`deploy_skills`] reads every `*.md` file from a source directory,
//! derives the skill name by stripping the `.md` extension, and writes each
//! one as `~/.claude/skills/<name>/SKILL.md`. It consults the
//! [`SkillManifest`] to classify each target file and writes only the files it
//! safely may. It returns a [`DeployStats`] summarising what happened.
//! Test: `cargo test -p trusty-mpm skill_deployer` covers a new deploy, a
//! skipped user-modified file, an unchanged file, and a user-owned file.

use std::path::Path;

use crate::core::agent_manifest::{atomic_write, checksum};
use crate::core::error::Error;
use crate::core::skill_manifest::{SkillManifest, SkillManifestEntry};

/// Summary of one [`deploy_skills`] run.
///
/// Why: callers print per-file status; they need the file lists split by
/// outcome to render that summary and to know whether any work was skipped.
/// What: filenames grouped into freshly written, skipped (user-owned or
/// user-modified), and unchanged (checksum already current).
/// Test: every `deploy_*` test asserts on these vectors.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeployStats {
    /// Filenames successfully (re)written this run.
    pub deployed: Vec<String>,
    /// Filenames skipped because the user owns or modified them, or because
    /// the name collides with a Claude Code built-in slash command (e.g.
    /// contains "mcp" — see [`deploy_skills_filtered`]).
    pub skipped: Vec<String>,
    /// Filenames left untouched because their checksum already matched.
    pub unchanged: Vec<String>,
}

/// Whether a source filename names a skill file to deploy.
///
/// Why: the source directory holds `.md` files; only those should be deployed,
/// and any manifest file or hidden file must be ignored. Exported (PR #2818
/// review, MEDIUM) so `core::skill_tiers::list_source_stems` enumerates
/// exactly the same file set this deployer will act on — a second,
/// independently-written filter here would risk drifting out of sync.
/// What: returns `true` for `*.md` files that do not start with `.`.
/// Test: covered indirectly by `deploy_new_skill`;
/// `crate::core::skill_tiers::tests::list_source_stems_reads_md_files` exercises
/// it via the shared helper.
pub(crate) fn is_skill_file(name: &str) -> bool {
    !name.starts_with('.') && name.ends_with(".md")
}

/// Derive the skill name (manifest key and target directory name) from a
/// source filename.
///
/// Why: sources are flat `<name>.md` files but the deploy target is
/// `<dest>/<name>/SKILL.md`. Stripping `.md` gives the shared name. Exported
/// (PR #2818 review, MEDIUM) so `core::skill_tiers::list_source_stems` derives
/// stems with the IDENTICAL single-strip semantics this deployer uses — a
/// second `trim_end_matches(".md")`-based implementation would repeat-strip a
/// pathological `foo.md.md` filename to `foo`, while this deployer's own
/// `deploy_skills_filtered` (via [`is_skill_file`] + this function) treats it
/// as stem `foo.md`; the mismatch meant the tier planner would greenlight a
/// stem the deployer's `select` predicate then rejected, so the skill silently
/// never deployed.
/// What: returns the filename without its `.md` suffix (single strip).
/// Test: covered indirectly by every `deploy_*` test;
/// `crate::core::skill_tiers::tests::list_source_stems_reads_md_files` exercises
/// it via the shared helper.
pub(crate) fn skill_stem(filename: &str) -> &str {
    filename.strip_suffix(".md").unwrap_or(filename)
}

/// Deploy all skills from `source` to `dest`.
///
/// Why: ensures `~/.claude/skills/` has up-to-date skill files without
/// clobbering user-owned or user-modified files.
///
/// Rules:
///   - Name (stem) contains "mcp" (case-insensitive) → collides with Claude
///     Code's built-in `/mcp` command → skip, never written (#2186)
///   - Not in manifest, file exists → user-owned → skip silently
///   - In manifest, checksum matches deployed copy → overwrite when stale
///   - In manifest, checksum differs → user-modified → skip
///   - New trusty-mpm skill → write + add to manifest
///
/// Test: `deploy_new_skill`, `deploy_skips_user_modified`,
/// `deploy_unchanged_no_write`, `deploy_user_owned_skipped`,
/// `deploy_skips_mcp_named_skill`.
pub fn deploy_skills(source: &Path, dest: &Path) -> Result<DeployStats, Error> {
    // Default policy: deploy every skill in the source directory.
    deploy_skills_filtered(source, dest, |_name| true)
}

/// Deploy skills from `source`, restricting to those `select` accepts.
///
/// Why: HR-2 manifests describe a skill *set* (include/exclude globs); the
/// session-launch path must deploy a subset of the source skills without copying
/// the whole directory. Factoring the selection into a predicate keeps the
/// manifest logic in `session_launch` while reusing the identical
/// ownership/atomic-write machinery here.
/// What: identical to [`deploy_skills`] except a source skill whose stem (the
/// `.md`-stripped name) `select` rejects is skipped before any write.
/// [`deploy_skills`] delegates here with an accept-all predicate, so existing
/// behavior is unchanged. Deselecting a skill does not remove a previously
/// deployed copy (an HR-3 concern).
/// Test: `deploy_filtered_respects_predicate`.
pub fn deploy_skills_filtered(
    source: &Path,
    dest: &Path,
    select: impl Fn(&str) -> bool,
) -> Result<DeployStats, Error> {
    let mut stats = DeployStats::default();

    // No source directory means nothing to deploy — an empty result, not an
    // error, so a fresh install with no skills still succeeds. This is a
    // legitimate state for a fresh checkout, but it is also indistinguishable
    // from a misconfigured `skill_source_dir()` (e.g. a submodule that failed
    // to initialise) unless we log it — silently returning an empty
    // `DeployStats` previously left operators with no signal that zero
    // skills were ever considered (A2, tm-skills-portfolio epic).
    if !source.is_dir() {
        tracing::warn!(
            source = %source.display(),
            "skill source directory missing — no skills will be deployed"
        );
        return Ok(stats);
    }

    let mut manifest = SkillManifest::load(dest);
    let now = chrono::Utc::now().to_rfc3339();

    // Collect skill filenames deterministically so output and tests are stable.
    let mut names: Vec<String> = Vec::new();
    let mut source_file_count = 0usize;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if entry.file_type()?.is_file() && is_skill_file(name) {
            source_file_count += 1;
            // Honour the manifest's skill-set selection (HR-2). The stem is the
            // `.md`-stripped name used as the skill id and target dir name.
            if select(skill_stem(name)) {
                names.push(name.to_string());
            }
        }
    }
    names.sort_unstable();

    // An existing but empty source directory is just as silent a failure mode
    // as a missing one — e.g. a populated `agents/skills/` submodule checked
    // out shallow, or a source dir pointed at the wrong path (A2).
    if source_file_count == 0 {
        tracing::warn!(
            source = %source.display(),
            "skill source directory is empty — no skills will be deployed"
        );
    }

    for filename in names {
        let stem = skill_stem(&filename).to_string();

        // Claude Code ships a built-in `/mcp` slash command. A deployed skill
        // whose invocable name (the stem, i.e. the directory Claude Code
        // exposes as `/<stem>`) contains "mcp" shadows that built-in and
        // makes `/mcp` unreachable for the user (#2186). Reject before any
        // file I/O or manifest write so a bad name never lands on disk.
        if stem.to_lowercase().contains("mcp") {
            tracing::warn!(
                skill = %stem,
                "refusing to deploy skill whose name contains \"mcp\" — it would shadow Claude Code's built-in /mcp command"
            );
            stats.skipped.push(stem);
            continue;
        }

        let source_path = source.join(&filename);
        let content = std::fs::read_to_string(&source_path)?;
        // Claude Code discovers skills from <dest>/<name>/SKILL.md.
        let skill_dir = dest.join(&stem);
        let target_path = skill_dir.join("SKILL.md");
        deploy_one_file(
            &mut manifest,
            &stem,
            &target_path,
            &content,
            &now,
            &mut stats,
        )?;

        // Mirror any reference files a multi-file skill carries alongside its
        // SKILL.md (issue #2903 — upstream progressive-disclosure skills ship
        // a lean entry point plus a `references/*.md` subtree loaded on
        // demand). This runs independent of whether the entry point itself
        // changed this pass, so a skill that gains a new reference file on a
        // later deploy still picks it up even when its SKILL.md content is
        // unchanged (`stats.unchanged`).
        let refs_source_dir = source.join(&stem).join("references");
        if refs_source_dir.is_dir() {
            let mut ref_names: Vec<String> = std::fs::read_dir(&refs_source_dir)?
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_str()?.to_string();
                    (e.file_type().ok()?.is_file() && is_skill_file(&name)).then_some(name)
                })
                .collect();
            ref_names.sort_unstable();

            for ref_name in ref_names {
                let ref_content = std::fs::read_to_string(refs_source_dir.join(&ref_name))?;
                let ref_key = format!("{stem}/references/{ref_name}");
                let ref_target = skill_dir.join("references").join(&ref_name);
                deploy_one_file(
                    &mut manifest,
                    &ref_key,
                    &ref_target,
                    &ref_content,
                    &now,
                    &mut stats,
                )?;
            }
        }
    }

    manifest.save(dest)?;

    Ok(stats)
}

/// Deploy one managed file (a skill's `SKILL.md` or one of its
/// `references/*.md` siblings) under the shared ownership rules.
///
/// Why: [`deploy_skills_filtered`] applies the IDENTICAL
/// managed/unmanaged/unchanged/stale classification to both the entry-point
/// `SKILL.md` (keyed by the bare stem) and each multi-file skill's
/// `references/*.md` siblings (keyed by `<stem>/references/<file>`, issue
/// #2903) — factoring it out here is what keeps those two call sites from
/// silently diverging on the ownership rule.
/// What: writes `content` to `target_path` (creating parent directories as
/// needed) via [`atomic_write`] and records `key` in `manifest` UNLESS the
/// target exists and is either unmanaged (user-owned — skip) or managed with
/// a checksum mismatch (user-modified — skip), or is managed with a matching
/// checksum and `content` is unchanged (unchanged — no-op). Every outcome is
/// pushed to the matching `stats` vector under `key`.
/// Test: `deploy_new_skill`, `deploy_skips_user_modified`,
/// `deploy_unchanged_no_write`, `deploy_user_owned_skipped`, plus the
/// reference-file tests below (`deploy_reference_files_land_alongside_skill`,
/// `deploy_reference_files_skip_user_modified`,
/// `deploy_reference_files_sync_even_when_entry_unchanged`).
fn deploy_one_file(
    manifest: &mut SkillManifest,
    key: &str,
    target_path: &Path,
    content: &str,
    now: &str,
    stats: &mut DeployStats,
) -> Result<(), Error> {
    if target_path.exists() {
        if !manifest.is_managed(key) {
            // User dropped their own file here — never touch it.
            stats.skipped.push(key.to_string());
            return Ok(());
        }
        let current = std::fs::read_to_string(target_path)?;
        if manifest.checksum_matches(key, &current) {
            if checksum(content) == checksum(&current) {
                // Deployed copy is already the latest content.
                stats.unchanged.push(key.to_string());
                return Ok(());
            }
            // Managed and unmodified by the user → safe to refresh.
        } else {
            // Managed but the user edited it → preserve their changes.
            stats.skipped.push(key.to_string());
            return Ok(());
        }
    }

    // Write (new file, or safe refresh of a managed file) atomically.
    // Create the parent directory if needed, then write via
    // write-temp-then-rename so a crash between the content write and the
    // subsequent manifest save leaves the old file intact.
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write(target_path, content)?;
    manifest.managed.insert(
        key.to_string(),
        SkillManifestEntry {
            checksum: checksum(content),
            deployed_at: now.to_string(),
        },
    );
    stats.deployed.push(key.to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// A two-file skill source set.
    fn write_sources(dir: &Path) {
        fs::write(
            dir.join("tm-doctor.md"),
            "---\nname: tm-doctor\n---\n\n# Doctor\n\nDiagnostic skill.\n",
        )
        .unwrap();
        fs::write(
            dir.join("example-skill.md"),
            "---\nname: example-skill\n---\n\n# Example\n\nExample skill.\n",
        )
        .unwrap();
    }

    #[test]
    fn deploy_new_skill() {
        // A first-ever deploy must write every skill as <dest>/<name>/SKILL.md
        // and record the skill name (stem) in the manifest.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        let stats = deploy_skills(src.path(), tgt.path()).unwrap();
        assert_eq!(stats.deployed.len(), 2);
        // Stats report stems, not filenames.
        assert!(stats.deployed.contains(&"tm-doctor".to_string()));
        assert!(stats.skipped.is_empty());
        assert!(stats.unchanged.is_empty());

        // Each skill lands at <dest>/<name>/SKILL.md — not a flat .md file.
        let doctor = fs::read_to_string(tgt.path().join("tm-doctor").join("SKILL.md")).unwrap();
        assert!(doctor.contains("Diagnostic skill."));

        let manifest = SkillManifest::load(tgt.path());
        assert!(manifest.is_managed("tm-doctor"));
    }

    #[test]
    fn deploy_filtered_respects_predicate() {
        // HR-2: a selection predicate restricts which source skills deploy.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path()); // tm-doctor.md + example-skill.md

        let stats =
            deploy_skills_filtered(src.path(), tgt.path(), |name| name == "example-skill").unwrap();

        assert!(stats.deployed.contains(&"example-skill".to_string()));
        assert!(!stats.deployed.contains(&"tm-doctor".to_string()));
        assert!(tgt.path().join("example-skill").join("SKILL.md").exists());
        assert!(!tgt.path().join("tm-doctor").exists());
    }

    #[test]
    fn deploy_skips_user_modified() {
        // A managed file the user edited must be skipped, not overwritten.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        deploy_skills(src.path(), tgt.path()).unwrap();

        // Simulate the user editing the deployed SKILL.md.
        fs::write(
            tgt.path().join("tm-doctor").join("SKILL.md"),
            "---\nname: tm-doctor\n---\n\nUSER HAND-EDIT\n",
        )
        .unwrap();

        let stats = deploy_skills(src.path(), tgt.path()).unwrap();
        assert!(stats.skipped.contains(&"tm-doctor".to_string()));
        assert!(!stats.deployed.contains(&"tm-doctor".to_string()));

        let still = fs::read_to_string(tgt.path().join("tm-doctor").join("SKILL.md")).unwrap();
        assert!(still.contains("USER HAND-EDIT"));
    }

    #[test]
    fn deploy_unchanged_no_write() {
        // A second deploy with no source changes must report files unchanged
        // and not rewrite them.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        deploy_skills(src.path(), tgt.path()).unwrap();
        let skill_md = tgt.path().join("tm-doctor").join("SKILL.md");
        let before = fs::metadata(&skill_md).unwrap().modified().unwrap();

        let stats = deploy_skills(src.path(), tgt.path()).unwrap();
        assert!(stats.unchanged.contains(&"tm-doctor".to_string()));
        assert!(stats.deployed.is_empty());

        let after = fs::metadata(&skill_md).unwrap().modified().unwrap();
        assert_eq!(before, after, "unchanged file must not be rewritten");
    }

    #[test]
    fn deploy_user_owned_skipped() {
        // A SKILL.md in the target that trusty-mpm never deployed (absent from
        // the manifest) must be left completely untouched.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        // Pre-create a user-owned skill directory for tm-doctor.
        let user_dir = tgt.path().join("tm-doctor");
        fs::create_dir_all(&user_dir).unwrap();
        fs::write(user_dir.join("SKILL.md"), "USER OWNED — not trusty-mpm's\n").unwrap();

        let stats = deploy_skills(src.path(), tgt.path()).unwrap();
        assert!(stats.skipped.contains(&"tm-doctor".to_string()));

        let content = fs::read_to_string(user_dir.join("SKILL.md")).unwrap();
        assert_eq!(content, "USER OWNED — not trusty-mpm's\n");

        // example-skill had no conflict, so it deploys normally.
        assert!(stats.deployed.contains(&"example-skill".to_string()));
    }

    #[test]
    fn deploy_refreshes_stale_managed_skill() {
        // A managed, unmodified file whose source changed must be refreshed.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        deploy_skills(src.path(), tgt.path()).unwrap();

        // The framework updates the source skill.
        fs::write(
            src.path().join("tm-doctor.md"),
            "---\nname: tm-doctor\n---\n\n# Doctor v2\n",
        )
        .unwrap();

        let stats = deploy_skills(src.path(), tgt.path()).unwrap();
        assert!(stats.deployed.contains(&"tm-doctor".to_string()));
        let refreshed = fs::read_to_string(tgt.path().join("tm-doctor").join("SKILL.md")).unwrap();
        assert!(refreshed.contains("Doctor v2"));
    }

    #[test]
    fn deploy_skips_mcp_named_skill() {
        // #2186: a skill whose name (stem) contains "mcp" must never be
        // deployed — Claude Code's built-in `/mcp` command would be shadowed
        // by `/toolchains-ai-protocols-mcp` (or any other mcp-containing
        // name). It must be recorded as skipped, not deployed, and must not
        // block sibling skills in the same batch from deploying normally.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path()); // tm-doctor.md + example-skill.md
        fs::write(
            src.path().join("toolchains-ai-protocols-mcp.md"),
            "---\nname: toolchains-ai-protocols-mcp\n---\n\n# AI Protocols\n\nMCP guidance.\n",
        )
        .unwrap();

        let stats = deploy_skills(src.path(), tgt.path()).unwrap();

        assert!(
            stats
                .skipped
                .contains(&"toolchains-ai-protocols-mcp".to_string())
        );
        assert!(
            !stats
                .deployed
                .contains(&"toolchains-ai-protocols-mcp".to_string())
        );
        assert!(
            !tgt.path()
                .join("toolchains-ai-protocols-mcp")
                .join("SKILL.md")
                .exists(),
            "an mcp-named skill must never be written to disk"
        );

        // Sibling non-mcp skills in the same batch must still deploy.
        assert!(stats.deployed.contains(&"tm-doctor".to_string()));
        assert!(stats.deployed.contains(&"example-skill".to_string()));
    }

    #[test]
    fn deploy_skips_mcp_named_skill_case_insensitive() {
        // The guard must match "mcp" regardless of case (e.g. `Foo-MCP.md`).
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path()); // tm-doctor.md + example-skill.md
        fs::write(
            src.path().join("Foo-MCP.md"),
            "---\nname: Foo-MCP\n---\n\n# Foo MCP\n\nSomething.\n",
        )
        .unwrap();

        let stats = deploy_skills(src.path(), tgt.path()).unwrap();

        assert!(stats.skipped.contains(&"Foo-MCP".to_string()));
        assert!(!stats.deployed.contains(&"Foo-MCP".to_string()));
        assert!(!tgt.path().join("Foo-MCP").exists());
    }

    #[test]
    fn deploy_missing_source_dir_is_empty_result() {
        // Deploying from a non-existent source directory is a no-op success.
        let tgt = TempDir::new().unwrap();
        let stats = deploy_skills(Path::new("/nonexistent/trusty-mpm/skills"), tgt.path()).unwrap();
        assert_eq!(stats, DeployStats::default());
    }

    /// Write a multi-file skill: `<dir>/<stem>.md` plus `<dir>/<stem>/references/*.md`.
    fn write_multi_file_skill(dir: &Path, stem: &str, refs: &[(&str, &str)]) {
        fs::write(
            dir.join(format!("{stem}.md")),
            format!("---\nname: {stem}\n---\n\nEntry point.\n"),
        )
        .unwrap();
        let refs_dir = dir.join(stem).join("references");
        fs::create_dir_all(&refs_dir).unwrap();
        for (name, body) in refs {
            fs::write(refs_dir.join(name), body).unwrap();
        }
    }

    #[test]
    fn deploy_reference_files_land_alongside_skill() {
        // Issue #2903: a multi-file skill's references/*.md siblings must
        // deploy to <dest>/<stem>/references/<file> alongside SKILL.md.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_multi_file_skill(
            src.path(),
            "systematic-debugging",
            &[("workflow.md", "WORKFLOW"), ("examples.md", "EXAMPLES")],
        );

        let stats = deploy_skills(src.path(), tgt.path()).unwrap();

        assert!(stats.deployed.contains(&"systematic-debugging".to_string()));
        assert!(
            stats
                .deployed
                .contains(&"systematic-debugging/references/workflow.md".to_string())
        );
        assert!(
            stats
                .deployed
                .contains(&"systematic-debugging/references/examples.md".to_string())
        );

        let workflow = fs::read_to_string(
            tgt.path()
                .join("systematic-debugging")
                .join("references")
                .join("workflow.md"),
        )
        .unwrap();
        assert_eq!(workflow, "WORKFLOW");
        let examples = fs::read_to_string(
            tgt.path()
                .join("systematic-debugging")
                .join("references")
                .join("examples.md"),
        )
        .unwrap();
        assert_eq!(examples, "EXAMPLES");
    }

    #[test]
    fn deploy_reference_files_skip_user_modified() {
        // A user-edited reference file must be preserved, not overwritten,
        // exactly like a user-edited SKILL.md.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_multi_file_skill(src.path(), "systematic-debugging", &[("workflow.md", "V1")]);
        deploy_skills(src.path(), tgt.path()).unwrap();

        let ref_path = tgt
            .path()
            .join("systematic-debugging")
            .join("references")
            .join("workflow.md");
        fs::write(&ref_path, "USER HAND-EDIT").unwrap();

        // Source changes; user's edit must survive the redeploy.
        fs::write(
            src.path()
                .join("systematic-debugging")
                .join("references")
                .join("workflow.md"),
            "V2",
        )
        .unwrap();
        let stats = deploy_skills(src.path(), tgt.path()).unwrap();

        assert!(
            stats
                .skipped
                .contains(&"systematic-debugging/references/workflow.md".to_string())
        );
        assert_eq!(fs::read_to_string(&ref_path).unwrap(), "USER HAND-EDIT");
    }

    #[test]
    fn deploy_reference_files_sync_even_when_entry_unchanged() {
        // A new reference file added to a skill whose SKILL.md content did
        // NOT change must still deploy — reference sync is independent of
        // whether the entry point itself changed this pass.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_multi_file_skill(src.path(), "systematic-debugging", &[]);
        deploy_skills(src.path(), tgt.path()).unwrap();

        // SKILL.md is untouched, but a reference file is added afterward.
        let refs_dir = src.path().join("systematic-debugging").join("references");
        fs::write(refs_dir.join("new-ref.md"), "NEW REF").unwrap();

        let stats = deploy_skills(src.path(), tgt.path()).unwrap();
        assert!(
            stats
                .unchanged
                .contains(&"systematic-debugging".to_string()),
            "entry point content is unchanged: {stats:?}"
        );
        assert!(
            stats
                .deployed
                .contains(&"systematic-debugging/references/new-ref.md".to_string())
        );
        assert!(
            tgt.path()
                .join("systematic-debugging")
                .join("references")
                .join("new-ref.md")
                .is_file()
        );
    }

    #[test]
    fn deploy_single_file_skill_has_no_references_dir() {
        // A skill with no references/ subtree must not create an empty
        // references/ directory under the deployed skill.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path()); // tm-doctor.md + example-skill.md, no refs

        deploy_skills(src.path(), tgt.path()).unwrap();

        assert!(!tgt.path().join("tm-doctor").join("references").exists());
    }
}

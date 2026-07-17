//! Skill deployment — writes skill directories into `~/.claude/skills/`.
//!
//! Why: Claude Code discovers skills from `~/.claude/skills/<name>/SKILL.md`
//! (directory per skill, entry-point file named `SKILL.md`). trusty-mpm must
//! keep that directory populated with up-to-date skills in that format, while
//! never destroying files the user owns or has hand-edited. Skills carry no
//! inheritance, so — unlike agents — deployment is a plain content copy, but
//! the manifest-based ownership tracking is identical. Moved to
//! trusty-agents-common (#2892, #2818) from `trusty-mpm::core::skill_deployer`
//! so a second harness (trusty-code) can reuse the same deploy pipeline;
//! `trusty-mpm` re-exports every item here from `core::skill_deployer` for
//! source compatibility. [`is_skill_file`] and [`skill_stem`] widen from
//! `pub(crate)` to `pub` (mirroring `agents::deployer::is_agent_file`, #2892)
//! so trusty-mpm's `skill_staleness` module can keep calling them cross-crate.
//! What: [`deploy_skills`] reads every `*.md` file from a source directory,
//! derives the skill name by stripping the `.md` extension, and writes each
//! one as `~/.claude/skills/<name>/SKILL.md`. It consults the
//! [`SkillManifest`] to classify each target file and writes only the files it
//! safely may. It also mirrors any `references/*.md` sibling files a
//! multi-file skill carries alongside its entry point (issue #2903, ported
//! from `trusty-mpm` PR #2915) under the identical ownership rule, keyed as
//! `<stem>/references/<file>`. It returns a [`DeployStats`] summarising what
//! happened.
//! Test: `cargo test -p trusty-agents-common skills::deployer` covers a new
//! deploy, a skipped user-modified file, an unchanged file, a user-owned
//! file, and the reference-file mirroring cases.

use std::path::Path;

use crate::agents::manifest::{Result, atomic_write, checksum};
use crate::skills::manifest::{SkillManifest, SkillManifestEntry};

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
/// review, MEDIUM) so `skills::tiers::list_source_stems` enumerates
/// exactly the same file set this deployer will act on — a second,
/// independently-written filter here would risk drifting out of sync.
/// What: returns `true` for `*.md` files that do not start with `.`.
/// Test: covered indirectly by `deploy_new_skill`;
/// `crate::skills::tiers::tests::list_source_stems_reads_md_files` exercises
/// it via the shared helper.
pub fn is_skill_file(name: &str) -> bool {
    !name.starts_with('.') && name.ends_with(".md")
}

/// Derive the skill name (manifest key and target directory name) from a
/// source filename.
///
/// Why: sources are flat `<name>.md` files but the deploy target is
/// `<dest>/<name>/SKILL.md`. Stripping `.md` gives the shared name. Exported
/// (PR #2818 review, MEDIUM) so `skills::tiers::list_source_stems` derives
/// stems with the IDENTICAL single-strip semantics this deployer uses — a
/// second `trim_end_matches(".md")`-based implementation would repeat-strip a
/// pathological `foo.md.md` filename to `foo`, while this deployer's own
/// `deploy_skills_filtered` (via [`is_skill_file`] + this function) treats it
/// as stem `foo.md`; the mismatch meant the tier planner would greenlight a
/// stem the deployer's `select` predicate then rejected, so the skill silently
/// never deployed.
/// What: returns the filename without its `.md` suffix (single strip).
/// Test: covered indirectly by every `deploy_*` test;
/// `crate::skills::tiers::tests::list_source_stems_reads_md_files` exercises
/// it via the shared helper.
pub fn skill_stem(filename: &str) -> &str {
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
pub fn deploy_skills(source: &Path, dest: &Path) -> Result<DeployStats> {
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
) -> Result<DeployStats> {
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
        // SKILL.md (issue #2903, ported from `trusty-mpm` PR #2915 —
        // upstream progressive-disclosure skills ship a lean entry point plus
        // a `references/*.md` subtree loaded on demand). This runs
        // independent of whether the entry point itself changed this pass,
        // so a skill that gains a new reference file on a later deploy still
        // picks it up even when its SKILL.md content is unchanged
        // (`stats.unchanged`).
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
/// #2903, ported from `trusty-mpm` PR #2915) — factoring it out here is what
/// keeps those two call sites from silently diverging on the ownership rule.
/// What: writes `content` to `target_path` (creating parent directories as
/// needed) via [`atomic_write`] and records `key` in `manifest` UNLESS the
/// target exists and is either unmanaged (user-owned — skip) or managed with
/// a checksum mismatch (user-modified — skip), or is managed with a matching
/// checksum and `content` is unchanged (unchanged — no-op). Every outcome is
/// pushed to the matching `stats` vector under `key`.
/// Test: `deploy_new_skill`, `deploy_skips_user_modified`,
/// `deploy_unchanged_no_write`, `deploy_user_owned_skipped`, plus the
/// reference-file tests in `deployer_tests.rs`
/// (`deploy_reference_files_land_alongside_skill`,
/// `deploy_reference_files_skip_user_modified`,
/// `deploy_reference_files_sync_even_when_entry_unchanged`).
fn deploy_one_file(
    manifest: &mut SkillManifest,
    key: &str,
    target_path: &Path,
    content: &str,
    now: &str,
    stats: &mut DeployStats,
) -> Result<()> {
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
#[path = "deployer_tests.rs"]
mod tests;

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
//! What: [`deploy_skills`] enumerates the source directory's skills through
//! [`crate::skills::source_scan::scan_skill_sources`] — which accepts BOTH a
//! flat `<stem>.md` and a directory-shaped `<stem>/SKILL.md` (#4949) — and
//! writes each one as `~/.claude/skills/<name>/SKILL.md`. It consults the
//! [`SkillManifest`] to classify each target file and writes only the files it
//! safely may. Every file a multi-file skill carries alongside its entry point
//! travels with it under the identical ownership rule, keyed as
//! `<stem>/<relative-path>` (issue #2903 introduced this for
//! `references/*.md`; #4949 widened it to `metadata.json`, `scripts/`, and any
//! other carried file). It returns a [`DeployStats`] summarising what
//! happened, including — since #4949 — the names it could not deploy.
//! Test: `cargo test -p trusty-agents-common skills::deployer` covers a new
//! deploy, a skipped user-modified file, an unchanged file, a user-owned
//! file, the reference-file mirroring cases, and the directory-shaped-skill
//! cases (`deploy_directory_shaped_skill` and siblings).

use std::path::Path;

use crate::agents::manifest::{Result, atomic_write, checksum};
use crate::skills::manifest::{
    SkillManifest, SkillManifestEntry, SkillManifestSave, with_skill_manifest_lock,
};
use crate::skills::source_scan::{SourceSkill, scan_skill_sources};

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
    // No source directory means nothing to deploy — an empty result, not an
    // error, so a fresh install with no skills still succeeds. This is a
    // legitimate state for a fresh checkout, but it is also indistinguishable
    // from a misconfigured `skill_source_dir()` (e.g. a submodule that failed
    // to initialise) unless we log it — silently returning an empty
    // `DeployStats` previously left operators with no signal that zero
    // skills were ever considered (A2, tm-skills-portfolio epic). Checked
    // BEFORE taking the ledger lock so a no-op deploy neither blocks on a
    // concurrent writer nor creates a lock sidecar in a directory it will not
    // touch.
    if !source.is_dir() {
        tracing::warn!(
            source = %source.display(),
            "skill source directory missing — no skills will be deployed"
        );
        return Ok(DeployStats::default());
    }

    // #4881: the ENTIRE load-modify-save cycle runs under the directory's
    // exclusive ledger lock. Four writers share this target — a session
    // launch's deploy, `sync-assets`, `tm catalog apply`, and (since #4882) the
    // project-tier deploy on a version change — and without the lock two of
    // them that both load before either saves silently drop each other's
    // entries, after which the files those entries described read as
    // user-edited and are skipped forever. See
    // `manifest::with_skill_manifest_lock`.
    with_skill_manifest_lock(dest, || deploy_skills_locked(source, dest, select))
}

/// The body of [`deploy_skills_filtered`], run while holding the ledger lock.
///
/// Why (#4881): split out so the critical section is a single expression the
/// lock helper wraps, and so the lock's scope is impossible to misread — every
/// manifest load, file write, and manifest save in this function happens with
/// the lock held. Mirrors `agents::deployer::deploy_agents_locked`.
/// What: the classify/write/save pipeline documented on
/// [`deploy_skills_filtered`]. Never call it directly; it is unsafe against
/// concurrent writers by construction.
/// Test: covered by every `deploy_*` test through the public wrapper, plus
/// `deploy_blocks_while_the_skill_ledger_lock_is_held`.
fn deploy_skills_locked(
    source: &Path,
    dest: &Path,
    select: impl Fn(&str) -> bool,
) -> Result<DeployStats> {
    let mut stats = DeployStats::default();

    // #5626: a ledger that could not be READ must stop the deploy — the empty
    // default reclassifies every managed skill as user-owned, and the files this
    // run writes then carry no ledger entry, which is the #4881 freeze.
    let mut manifest = SkillManifest::load_checked(dest)?;
    // #4881: the snapshot the compare-and-swap save is checked against.
    let base = manifest.clone();
    let now = chrono::Utc::now().to_rfc3339();

    // #4949: ONE shared scan answers "what is a skill here?", covering both the
    // flat `<stem>.md` layout and the directory `<stem>/SKILL.md` layout. The
    // loop this replaced tested `entry.file_type()?.is_file()`, so a
    // directory-shaped skill failed it and was dropped with no warning on every
    // deploy. `skills::tiers::list_source_stems` calls the same scan, so the
    // planner and this deployer cannot disagree about which stems exist.
    let scan = scan_skill_sources(source)?;

    // #4949: a directory the scan could not turn into a skill (no `SKILL.md`)
    // rides `stats.skipped` — the channel callers already summarise for the
    // user (`managed_config::skill_skip_summary`, `tm sync-assets`) — plus a
    // warn line, matching how the `mcp`-name rejection below is surfaced.
    for name in &scan.rejected {
        tracing::warn!(
            skill = %name,
            source = %source.display(),
            "skill directory has no SKILL.md — it cannot be deployed"
        );
        stats.skipped.push(name.clone());
    }

    // An existing but empty source directory is just as silent a failure mode
    // as a missing one — e.g. a populated `agents/skills/` submodule checked
    // out shallow, or a source dir pointed at the wrong path (A2). Judged
    // BEFORE `select` runs, as it always was: a filtered deploy that legitimately
    // selects nothing (every stem shadowed by a higher tier) is not an empty
    // source. A directory holding only malformed skills is not empty either —
    // those were just reported by name.
    if scan.skills.is_empty() && scan.rejected.is_empty() {
        tracing::warn!(
            source = %source.display(),
            "skill source directory is empty — no skills will be deployed"
        );
    }

    // Honour the manifest's skill-set selection (HR-2). The scan is already
    // sorted by stem, so output and tests stay deterministic.
    let selected: Vec<SourceSkill> = scan
        .skills
        .into_iter()
        .filter(|skill| select(&skill.stem))
        .collect();

    // #4881: the write loop's failure sites (an unreadable source file, a failed
    // `atomic_write`) can return AFTER earlier `SKILL.md` files are already on
    // disk. Propagating straight out would skip the save below, leaving those
    // files unrecorded — bytes newer than any recorded checksum, which the
    // classifier reads as a hand-edit and skips forever. So the loop's error is
    // held, the ledger is saved either way, and the error propagates after.
    // Pre-existing (it predates this issue's lock work) but fixed here because
    // it violates the same invariant.
    let write_result = deploy_selected(dest, &selected, &now, &mut manifest, &mut stats);

    let save_result = manifest.save_merging(dest, &base);

    // The write error comes first: it is the more specific failure, and the save
    // has already run by this point regardless of which one is reported.
    write_result?;
    if save_result? == SkillManifestSave::Merged {
        tracing::warn!(
            dest = %dest.display(),
            "the skill manifest changed during this deploy — a writer bypassed the \
             ledger lock; its entries were merged rather than dropped"
        );
    }

    Ok(stats)
}

/// Write every selected skill and record it, stopping at the first I/O failure.
///
/// Why (#4881): split out of [`deploy_skills_locked`] so its `?` sites cannot
/// bypass the manifest save. Files this function has already written are on disk
/// whether it returns `Ok` or `Err`, so the caller must save the ledger on BOTH
/// paths — a shape that is easy to get wrong while the loop and the save share
/// one function body, and was wrong here before this change.
/// What: for each skill, writes `<dest>/<stem>/SKILL.md` plus every file the
/// skill carries through [`deploy_one_file`], recording each in `manifest` and
/// `stats`. Skills whose stem contains `mcp` are rejected before any I/O
/// (#2186). Returns the first I/O error; `manifest` and `stats` still describe
/// everything written up to that point.
/// Test: `deploy_records_files_written_before_a_mid_loop_failure`,
/// `deploy_directory_shaped_skill`, plus every `deploy_*` test through the
/// public entry point.
fn deploy_selected(
    dest: &Path,
    skills: &[SourceSkill],
    now: &str,
    manifest: &mut SkillManifest,
    stats: &mut DeployStats,
) -> Result<()> {
    for skill in skills {
        let stem = skill.stem.clone();

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

        let content = std::fs::read_to_string(&skill.entry)?;
        // Claude Code discovers skills from <dest>/<name>/SKILL.md.
        let skill_dir = dest.join(&stem);
        let target_path = skill_dir.join("SKILL.md");

        deploy_one_file(manifest, &stem, &target_path, &content, now, stats)?;

        // Mirror every file the skill carries alongside its entry point — the
        // `references/*.md` subtree a progressive-disclosure skill ships (issue
        // #2903), and since #4949 also its `metadata.json`, `scripts/`, and
        // anything else at any depth. This runs independent of whether the
        // entry point itself changed this pass, so a skill that gains a file on
        // a later deploy still picks it up even when its SKILL.md content is
        // unchanged (`stats.unchanged`).
        for extra in &skill.extras {
            // A non-UTF-8 file (an image, a compiled asset) cannot ride the
            // manifest's text checksum. Report it and carry on — aborting the
            // whole deploy over one carried asset would be a worse failure than
            // the one #4949 fixed.
            let extra_content = match std::fs::read_to_string(&extra.path) {
                Ok(content) => content,
                Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
                    let key = format!("{stem}/{}", extra.rel);
                    tracing::warn!(
                        file = %key,
                        "skill file is not UTF-8 — it cannot be deployed"
                    );
                    stats.skipped.push(key);
                    continue;
                }
                Err(err) => return Err(err.into()),
            };
            let key = format!("{stem}/{}", extra.rel);
            let mut extra_target = skill_dir.clone();
            for segment in extra.rel.split('/') {
                extra_target.push(segment);
            }
            deploy_one_file(manifest, &key, &extra_target, &extra_content, now, stats)?;
        }
    }

    Ok(())
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

//! The files `tm fleet init` writes into the Architect project (#8436, P4).
//!
//! Why: the Architect profile reads its fleet specifics from "this project's
//! `CLAUDE.md`", runs the deterministic poller and the relay scripts from
//! `scripts/`, keeps its records in `local/` (#9997, superseding ruling C),
//! and runs the Architect-only skills (`tm-fleet-check`, `tm-context-refresh`
//! and, since #9997, the `architect-*` skills its manual indexes; rulings D,
//! E), and reads its instance settings from `local/architect.json` (#10161).
//! Without them a fresh Architect has instructions that point at files
//! that do not exist.
//! What: [`files`] lists each file in write order. The `CLAUDE.md` template
//! and the skills come from an [`ArchitectSeed`] read from content at
//! runtime (#9132, ADR-0064); the scripts, the voice guide and the other
//! templates are still embedded; every file under
//! `src/assets/architect/templates/` is a `template!` row. The canonical
//! copies live in `python/trusty-architect/` and
//! `scripts/check_architect_subproject.sh` fails when a shipped copy drifts.
//! The skills are not in the framework bundle (`core::bundle::ALL`): that
//! bundle deploys to every session's user tier, and these must reach the
//! Architect only. [`deploy`] writes each file when it is absent, and since
//! #10571 replaces one whose bytes are an earlier shipped version
//! (`seed_history.tsv`, see [`refresh`]). A file with any other bytes is
//! someone's edit and is reported skipped, never overwritten. The skills' bytes are the ones every Architect launch also
//! deploys (#9540).
//! Test: `a_first_run_seeds_the_architect_project`,
//! `every_template_asset_is_seeded`, `every_script_asset_is_seeded`,
//! `every_schema_asset_is_seeded`, `every_dashboard_asset_is_seeded`,
//! `an_edited_seed_or_script_is_never_overwritten`,
//! `every_path_a_ported_skill_names_exists_after_init`,
//! `the_relay_scripts_and_the_voice_guide_are_seeded`,
//! `every_script_a_seeded_script_loads_by_path_is_seeded`,
//! `the_architect_config_loader_and_seed_are_seeded`,
//! `a_seed_that_does_not_resolve_writes_nothing`.

use std::path::Path;

use anyhow::{Context, bail};
use trusty_mpm::core::project_skill_tier::architect_seed::ArchitectSeed;

use super::Step;

/// One file `tm fleet init` writes, relative to the Architect directory.
pub(crate) struct Seeded<'a> {
    /// Destination below the Architect directory.
    pub(crate) dest: String,
    /// The shipped bytes.
    pub(crate) contents: &'a str,
    /// Whether the file is written with mode `0755`.
    pub(crate) executable: bool,
}

/// The shipped bytes of the compiled-in row seeded at `dest`, if any.
pub(crate) fn embedded_contents(dest: &str) -> Option<&'static str> {
    EMBEDDED
        .iter()
        .find(|(d, _, _)| *d == dest)
        .map(|&(_, c, _)| c)
}

#[path = "seed_refresh.rs"]
pub(crate) mod refresh;
#[path = "seed_table.rs"]
mod table;
use table::EMBEDDED;

/// Every file `tm fleet init` writes, in write order.
///
/// Why: #9132 — the template and the skills are read from content, so the
/// table is built from `seed` instead of being a compiled-in constant.
/// What: `CLAUDE.md` from `seed`, then [`EMBEDDED`], then each seed skill at
/// `.claude/skills/<stem>/SKILL.md`.
/// Test: `a_first_run_seeds_the_architect_project`.
pub(crate) fn files(seed: &ArchitectSeed) -> Vec<Seeded<'_>> {
    let template = Seeded {
        dest: "CLAUDE.md".to_owned(),
        contents: &seed.claude_md,
        executable: false,
    };
    let embedded = EMBEDDED.iter().map(|&(dest, contents, executable)| Seeded {
        dest: dest.to_owned(),
        contents,
        executable,
    });
    // #9540: the skill bytes are the ones the launch deploys.
    let skills = seed.skills.iter().map(|skill| Seeded {
        dest: format!(".claude/skills/{}/SKILL.md", skill.stem),
        contents: &skill.contents,
        executable: false,
    });
    std::iter::once(template)
        .chain(embedded)
        .chain(skills)
        .collect()
}

/// Directories `tm fleet init` guarantees, created when absent.
// #9997: per-project records live in `local/projects/`; the skills write
// other users' profiles to `local/users/` and briefs to `local/briefs/`.
// #10161: `architect-improvements` keeps its ledger in `local/improvements/`.
pub(crate) const DIRS: &[&str] = &[
    "local/projects",
    "local/users",
    "local/briefs",
    "local/improvements",
];

/// Write every [`files`] entry and [`DIRS`] entry under `dir`; one step each.
///
/// Why/What: see the module doc. `dir` is the preflight-checked path; `seed`
/// was read before any write, so a missing seed file never leaves a partial
/// project (#9132).
/// Test: `a_first_run_seeds_the_architect_project`,
/// `an_edited_seed_or_script_is_never_overwritten`.
pub(crate) fn deploy(dir: &Path, seed: &ArchitectSeed) -> anyhow::Result<Vec<Step>> {
    deploy_with(dir, seed, &refresh::shipped_before)
}

/// The predicate [`deploy_with`] asks whether live bytes are an earlier shipped copy.
pub(crate) type ShippedBefore<'a> = &'a dyn Fn(&str, &[u8]) -> bool;

/// [`deploy`] with the shipped-history predicate injected (#10571).
///
/// Why: a test needs an earlier shipped copy whose bytes it knows; the real
/// history holds only hashes. What: as [`deploy`], `shipped_before` decides
/// which differing files are refreshed.
/// Test: `a_refresh_updates_an_unedited_earlier_copy`.
pub(crate) fn deploy_with(
    dir: &Path,
    seed: &ArchitectSeed,
    shipped_before: ShippedBefore<'_>,
) -> anyhow::Result<Vec<Step>> {
    let files = files(seed);
    let mut steps = Vec::with_capacity(files.len() + DIRS.len() + 2);
    // #9997: an Architect that still keeps `records/` gets no `local/` rows.
    let records_era = dir.join(super::status::STALE_RECORDS_DIR).is_dir();
    for file in &files {
        if records_era && file.dest.starts_with("local/") {
            continue;
        }
        steps.push(deploy_file(dir, file, shipped_before)?);
    }
    // #10571: name the prototype commit and content this binary's bundle came from.
    if let Some(line) = files
        .iter()
        .find(|f| f.dest == refresh::PROVENANCE_DEST)
        .and_then(|f| refresh::provenance_line(f.contents))
    {
        steps.push(Step::Unchanged(line));
    }
    for rel in DIRS {
        if records_era {
            continue;
        }
        let path = dir.join(rel);
        steps.push(if path.is_dir() {
            Step::Unchanged(format!("{rel}/"))
        } else {
            std::fs::create_dir_all(&path)
                .with_context(|| format!("cannot create {}", path.display()))?;
            Step::Changed(format!("created {rel}/"))
        });
    }
    if records_era {
        steps.extend(records_era_steps(dir));
    }
    Ok(steps)
}

/// The report lines for a project that still keeps `records/`.
///
/// Why: #9997. A re-init must not seed `local/` beside `records/`; a `local/`
/// left from an earlier run is the owner's to merge, so it is named, not removed.
/// What: one `Skipped` line saying the `local/` templates were not seeded, plus
/// a warning line naming `local/` when it exists. Reads only.
/// Test: `a_reinit_beside_records_writes_no_local_templates`,
/// `a_reinit_warns_about_a_stale_local_beside_records`.
fn records_era_steps(dir: &Path) -> Vec<Step> {
    let mut steps = vec![Step::Skipped(
        "local/ templates: not seeded, because records/ exists".to_owned(),
    )];
    let local = dir.join("local");
    if local.exists() {
        steps.push(Step::Skipped(format!(
            "WARNING stale `local/` directory at {} beside `records/`: move what you keep              into one of them and delete the other yourself (tm deletes nothing)",
            local.display()
        )));
    }
    steps
}

/// Write one file when absent or an unedited earlier shipped copy; never
/// replace bytes that someone edited.
///
/// Why: #10571, an existing install must receive updated scripts it never
/// edited, and keep and report the ones its owner edited.
/// What: equal bytes are unchanged; bytes `shipped_before` lists
/// for this destination are replaced (mode kept for a script); any other
/// bytes are an edit, reported skipped and left as is; absent is written.
/// Test: `a_refresh_updates_an_unedited_earlier_copy`,
/// `a_refresh_keeps_an_edited_copy_and_reports_it`,
/// `an_edited_seed_or_script_is_never_overwritten`.
fn deploy_file(
    dir: &Path,
    file: &Seeded<'_>,
    shipped_before: ShippedBefore<'_>,
) -> anyhow::Result<Step> {
    let path = dir.join(&file.dest);
    match std::fs::read(&path) {
        Ok(bytes) if bytes == file.contents.as_bytes() => Ok(Step::Unchanged(file.dest.clone())),
        Ok(bytes) if shipped_before(&file.dest, &bytes) => {
            write_seed(&path, file)?;
            Ok(Step::Changed(format!(
                "updated {} (an unedited earlier shipped copy)",
                file.dest
            )))
        }
        Ok(_) => Ok(Step::Skipped(format!(
            "{}: differs from the shipped copy (edited), so it is left as is; \
             custom copy kept, not refreshed",
            file.dest
        ))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            write_seed(&path, file)?;
            Ok(Step::Changed(format!("wrote {}", file.dest)))
        }
        Err(err) if err.kind() == std::io::ErrorKind::IsADirectory => {
            bail!(
                "{} is a directory, not the file tm fleet init writes",
                path.display()
            )
        }
        Err(err) => Err(err).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Write `file`'s shipped bytes to `path` atomically; mode `0755` for a script.
fn write_seed(path: &Path, file: &Seeded<'_>) -> anyhow::Result<()> {
    trusty_common::atomic_file::write_atomic(path, file.contents.as_bytes())
        .with_context(|| format!("cannot write {}", path.display()))?;
    if file.executable {
        make_executable(path)?;
    }
    Ok(())
}

/// Set mode `0755` on a script tm just wrote.
#[cfg(unix)]
fn make_executable(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("cannot make {} executable", path.display()))
}

/// No mode bits to set off Unix.
#[cfg(not(unix))]
fn make_executable(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

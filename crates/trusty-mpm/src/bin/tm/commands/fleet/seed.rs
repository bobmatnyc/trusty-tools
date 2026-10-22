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
//! Architect only. [`deploy`] writes each file only when it is absent. A file
//! with other bytes is someone's edit and is reported skipped, never
//! overwritten. The skills' bytes are the ones every Architect launch also
//! deploys (#9540).
//! Test: `a_first_run_seeds_the_architect_project`,
//! `every_template_asset_is_seeded`, `every_script_asset_is_seeded`,
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

/// One embedded [`files`] row: destination, bytes, executable.
type Embedded = (&'static str, &'static str, bool);

/// Build one [`EMBEDDED`] row from a path under `src/assets/architect/`.
macro_rules! embedded {
    ($dest:literal, $asset:literal, $exec:literal) => {
        (
            $dest,
            include_str!(concat!("../../../../assets/architect/", $asset)),
            $exec,
        )
    };
}

/// Build one [`EMBEDDED`] row for a template seeded at the same relative path
/// under the Architect directory as under `src/assets/architect/templates/`.
macro_rules! template {
    ($rel:literal) => {
        (
            $rel,
            include_str!(concat!("../../../../assets/architect/templates/", $rel)),
            false,
        )
    };
}

/// The files still compiled in, in write order, after `CLAUDE.md`.
const EMBEDDED: &[Embedded] = &[
    embedded!(".gitignore", "templates/gitignore", false),
    // #9997: the records layout is `local/`, never the old directory.
    template!("local/state.md"),
    template!("local/actions.md"),
    template!("local/goals.md"),
    template!("local/user.md"),
    template!("local/instance.md"),
    template!("local/projects/_template.md"),
    template!("local/owner/README.md"),
    template!("local/owner/business.md"),
    template!("local/owner/process.md"),
    template!("local/owner/decision-making.md"),
    template!("local/owner/risk-tolerance.md"),
    template!("local/owner/detail-and-communication.md"),
    template!("local/owner/contradictions.md"),
    template!("local/owner/dream-log.md"),
    template!("local/mandates/README.md"),
    template!("local/mandates/example.toml"),
    // See #10161: the instance config `scripts/architect_config.py` reads.
    template!("local/architect.json"),
    embedded!("scripts/fleet-poll.py", "scripts/fleet-poll.py", true),
    // #8891: fleet-poll.py loads this sibling by path; it must ship beside it.
    embedded!(
        "scripts/fleet-classify.py",
        "scripts/fleet-classify.py",
        false
    ),
    embedded!("scripts/input-state.py", "scripts/input-state.py", true),
    embedded!(
        "scripts/quiet-sessions.py",
        "scripts/quiet-sessions.py",
        false
    ),
    embedded!("scripts/self-ctx.py", "scripts/self-ctx.py", false),
    // See #10161: the poller's alert triage and Architect watchdog (P1b-0).
    embedded!("scripts/alert_triage.py", "scripts/alert_triage.py", false),
    embedded!(
        "scripts/architect-watchdog.py",
        "scripts/architect-watchdog.py",
        false
    ),
    // See #10161: PM intake, its digest and the Notification hook pair (P2).
    embedded!("scripts/pm_intake.py", "scripts/pm_intake.py", false),
    embedded!("scripts/pm-digest.py", "scripts/pm-digest.py", true),
    embedded!("scripts/pm-notify.sh", "scripts/pm-notify.sh", true),
    embedded!(
        "scripts/notify-supervisor.sh",
        "scripts/notify-supervisor.sh",
        true
    ),
    // See #10161: the Intent Ledger set (P3). datg.py imports its four
    // datg_* modules from its own directory, so all five ship together.
    embedded!("scripts/datg.py", "scripts/datg.py", true),
    embedded!("scripts/datg_store.py", "scripts/datg_store.py", false),
    embedded!("scripts/datg_ops.py", "scripts/datg_ops.py", false),
    embedded!("scripts/datg_log.py", "scripts/datg_log.py", false),
    embedded!("scripts/datg_read.py", "scripts/datg_read.py", false),
    embedded!("scripts/datg-hook.sh", "scripts/datg-hook.sh", true),
    embedded!("scripts/ctx-watch.py", "scripts/ctx-watch.py", false),
    embedded!("scripts/il-cron.py", "scripts/il-cron.py", false),
    embedded!("scripts/self-clear.sh", "scripts/self-clear.sh", true),
    embedded!("scripts/state-archive.py", "scripts/state-archive.py", true),
    embedded!(
        "scripts/architect-idle.py",
        "scripts/architect-idle.py",
        true
    ),
    embedded!(
        "scripts/start-fleet-poll.sh",
        "scripts/start-fleet-poll.sh",
        true
    ),
    // #9997: the two-step relay send and the double-post guard the manual names.
    embedded!("scripts/relay.sh", "scripts/relay.sh", true),
    embedded!("scripts/posted.sh", "scripts/posted.sh", true),
    // posted.sh runs this sibling by path.
    embedded!("scripts/relay-marker.py", "scripts/relay-marker.py", false),
    // See #10161: the `local/architect.json` loader; a module, not a command.
    embedded!(
        "scripts/architect_config.py",
        "scripts/architect_config.py",
        false
    ),
    // #9997: the voice guide the manual and `architect-operating-rules` cite.
    embedded!("docs/architect/voice.md", "docs/voice.md", false),
];

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
pub(crate) const DIRS: &[&str] = &["local/projects", "local/users", "local/briefs"];

/// Write every [`files`] entry and [`DIRS`] entry under `dir`; one step each.
///
/// Why/What: see the module doc. `dir` is the preflight-checked path; `seed`
/// was read before any write, so a missing seed file never leaves a partial
/// project (#9132).
/// Test: `a_first_run_seeds_the_architect_project`,
/// `an_edited_seed_or_script_is_never_overwritten`.
pub(crate) fn deploy(dir: &Path, seed: &ArchitectSeed) -> anyhow::Result<Vec<Step>> {
    let files = files(seed);
    let mut steps = Vec::with_capacity(files.len() + DIRS.len() + 2);
    // #9997: an Architect that still keeps `records/` gets no `local/` rows.
    let records_era = dir.join(super::status::STALE_RECORDS_DIR).is_dir();
    for file in &files {
        if records_era && file.dest.starts_with("local/") {
            continue;
        }
        steps.push(deploy_file(dir, file)?);
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

/// Write one file when absent; never replace bytes that differ.
fn deploy_file(dir: &Path, file: &Seeded<'_>) -> anyhow::Result<Step> {
    let path = dir.join(&file.dest);
    match std::fs::read(&path) {
        Ok(bytes) if bytes == file.contents.as_bytes() => Ok(Step::Unchanged(file.dest.clone())),
        Ok(_) => Ok(Step::Skipped(format!(
            "{}: differs from the shipped copy (edited), so it is left as is",
            file.dest
        ))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            trusty_common::atomic_file::write_atomic(&path, file.contents.as_bytes())
                .with_context(|| format!("cannot write {}", path.display()))?;
            if file.executable {
                make_executable(&path)?;
            }
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

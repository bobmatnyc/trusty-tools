//! The files `tm fleet init` writes into the Architect project (#8436, P4).
//!
//! Why: the Architect profile reads its fleet specifics from "this project's
//! `CLAUDE.md`", runs the deterministic poller from `scripts/`, keeps its pass
//! records in `records/` (ruling C), and runs the Architect-only skills
//! `tm-fleet-check` and `tm-context-refresh` (rulings D, E). Without them a
//! fresh Architect has instructions that point at files that do not exist.
//! What: [`files`] lists each file in write order. The `CLAUDE.md` template
//! and the two skills come from an [`ArchitectSeed`] read from content at
//! runtime (#9132, ADR-0064); the scripts and the other templates are still
//! embedded. The canonical copies live in `python/trusty-architect/` and
//! `scripts/check_architect_subproject.sh` fails when a shipped copy drifts.
//! The two skills are not in the framework bundle (`core::bundle::ALL`): that
//! bundle deploys to every session's user tier, and these must reach the
//! Architect only. [`deploy`] writes each file only when it is absent. A file
//! with other bytes is someone's edit and is reported skipped, never
//! overwritten. The two skills' bytes are the ones every Architect launch
//! also deploys (#9540).
//! Test: `a_first_run_seeds_the_architect_project`,
//! `an_edited_seed_or_script_is_never_overwritten`,
//! `every_path_a_ported_skill_names_exists_after_init`,
//! `every_script_a_seeded_script_loads_by_path_is_seeded`,
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

/// The files still compiled in, in write order, after `CLAUDE.md`.
const EMBEDDED: &[Embedded] = &[
    embedded!(".gitignore", "templates/gitignore", false),
    embedded!("records/state.md", "templates/records/state.md", false),
    embedded!("records/actions.md", "templates/records/actions.md", false),
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
    embedded!(
        "scripts/start-fleet-poll.sh",
        "scripts/start-fleet-poll.sh",
        true
    ),
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

/// Directories `tm fleet init` creates that start with no file in them.
pub(crate) const DIRS: &[&str] = &["records/projects"];

/// Write every [`files`] entry and [`DIRS`] entry under `dir`; one step each.
///
/// Why/What: see the module doc. `dir` is the preflight-checked path; `seed`
/// was read before any write, so a missing seed file never leaves a partial
/// project (#9132).
/// Test: `a_first_run_seeds_the_architect_project`,
/// `an_edited_seed_or_script_is_never_overwritten`.
pub(crate) fn deploy(dir: &Path, seed: &ArchitectSeed) -> anyhow::Result<Vec<Step>> {
    let files = files(seed);
    let mut steps = Vec::with_capacity(files.len() + DIRS.len());
    for file in &files {
        steps.push(deploy_file(dir, file)?);
    }
    for rel in DIRS {
        let path = dir.join(rel);
        steps.push(if path.is_dir() {
            Step::Unchanged(format!("{rel}/"))
        } else {
            std::fs::create_dir_all(&path)
                .with_context(|| format!("cannot create {}", path.display()))?;
            Step::Changed(format!("created {rel}/"))
        });
    }
    Ok(steps)
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

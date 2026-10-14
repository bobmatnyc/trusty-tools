//! The files `tm fleet init` writes into the Architect project (#8436, P4, PR C).
//!
//! Why: the Architect's instructions point at files in its project — the
//! records in `local/`, the scripts, the templates, the docs, the agents and
//! the skills — so a fresh Architect needs every one of them. PR C seeds the
//! source tree's own layout, the one `scripts/init.sh` builds by hand, so the
//! two paths cannot drift (`init_sh_and_tm_fleet_init_produce_the_same_tree`).
//! What: [`files`] walks the embedded `src/assets/architect/` tree (a
//! byte-equal mirror of `python/trusty-architect/`, enforced by
//! `scripts/check_architect_subproject.sh`): `agents/` lands in
//! `.claude/agents/`, every other file at its own path, and [`PROMOTED`]
//! templates also at the root or in `local/`. Every `*.sh` and `scripts/*.py`
//! is written `0755`. The skills come from `architect_skills::SKILLS`, the one
//! table every Architect launch also deploys (#9540), never from a second
//! embed. [`deploy`] writes each file only when it is absent; a file with
//! other bytes is someone's edit and is reported skipped, never overwritten.
//! It then creates the [`DIRS`] skeleton and the empty [`LEDGERS`]. An existing
//! install keeps its `records/`; nothing removes it.
//! Test: `a_first_run_seeds_the_architect_project`,
//! `an_edited_seed_or_script_is_never_overwritten`,
//! `every_path_a_ported_skill_names_exists_after_init`,
//! `every_script_a_seeded_script_loads_by_path_is_seeded`,
//! `init_sh_and_tm_fleet_init_produce_the_same_tree`.

use std::borrow::Cow;
use std::path::Path;

use anyhow::{Context, bail};
use trusty_mpm::core::project_skill_tier::architect_skills;

use super::Step;

/// The embedded mirror of the Architect source tree.
#[derive(rust_embed::RustEmbed)]
#[folder = "src/assets/architect/"]
// #9540: the skills ship through `architect_skills::SKILLS`, not this walk.
#[exclude = "skills/**"]
#[exclude = "*.DS_Store"]
struct Assets;

/// One file `tm fleet init` writes, relative to the Architect directory.
pub(crate) struct Seeded {
    /// Destination below the Architect directory.
    pub(crate) dest: String,
    /// The shipped bytes.
    pub(crate) contents: Cow<'static, [u8]>,
    /// Whether the file is written with mode `0755`.
    pub(crate) executable: bool,
}

#[cfg(test)]
impl Seeded {
    /// The shipped bytes as text; every seeded file is UTF-8.
    pub(crate) fn text(&self) -> &str {
        std::str::from_utf8(&self.contents).unwrap_or_default()
    }
}

/// Templates also written at another path: `(asset, destination)`.
pub(crate) const PROMOTED: &[(&str, &str)] = &[
    ("templates/CLAUDE.md", "CLAUDE.md"),
    ("templates/AGENTS.md", "AGENTS.md"),
    ("templates/gitignore", ".gitignore"),
    ("templates/state.md", "local/state.md"),
    ("templates/goals.md", "local/goals.md"),
    ("templates/actions.md", "local/actions.md"),
    ("templates/local-gitignore", "local/.gitignore"),
];

/// Directories `tm fleet init` creates that start with no file in them.
pub(crate) const DIRS: &[&str] = &[
    "local/briefs",
    "local/archive",
    "local/mandates",
    "local/improvements/runs",
    "local/usage",
];

/// Ledgers that start empty.
pub(crate) const LEDGERS: &[&str] = &[
    "local/improvements/ledger.jsonl",
    "local/usage/ledger.jsonl",
];

/// Where asset `path` lands below the Architect directory.
fn dest_of(path: &str) -> String {
    match path.strip_prefix("agents/") {
        Some(rest) => format!(".claude/agents/{rest}"),
        None => path.to_owned(),
    }
}

/// Whether the file at `dest` is written `0755` (accepted layout, 10-09).
fn executable(dest: &str) -> bool {
    dest.ends_with(".sh") || (dest.starts_with("scripts/") && dest.ends_with(".py"))
}

/// Every file `tm fleet init` writes, in write order: the asset walk, the
/// promoted templates, the empty ledgers, then the skills.
pub(crate) fn files() -> Vec<Seeded> {
    let asset = |path: &str| Assets::get(path).map(|f| f.data);
    let mut out: Vec<Seeded> = Assets::iter()
        .filter_map(|path| {
            let contents = asset(&path)?;
            let dest = dest_of(&path);
            Some(Seeded {
                executable: executable(&dest),
                dest,
                contents,
            })
        })
        .collect();
    out.sort_by(|a, b| a.dest.cmp(&b.dest));
    for (from, dest) in PROMOTED {
        if let Some(contents) = asset(from) {
            out.push(Seeded {
                dest: (*dest).to_owned(),
                contents,
                executable: false,
            });
        }
    }
    out.extend(LEDGERS.iter().map(|dest| Seeded {
        dest: (*dest).to_owned(),
        contents: Cow::Borrowed(&[]),
        executable: false,
    }));
    out.extend(architect_skills::SKILLS.iter().map(|skill| Seeded {
        dest: format!(".claude/skills/{}/SKILL.md", skill.stem),
        contents: Cow::Borrowed(skill.contents.as_bytes()),
        executable: false,
    }));
    out
}

/// Write every [`files`] entry and [`DIRS`] entry under `dir`; one step each.
///
/// Why/What: see the module doc. `dir` is the preflight-checked path.
/// Test: `a_first_run_seeds_the_architect_project`,
/// `an_edited_seed_or_script_is_never_overwritten`.
pub(crate) fn deploy(dir: &Path) -> anyhow::Result<Vec<Step>> {
    let files = files();
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
fn deploy_file(dir: &Path, file: &Seeded) -> anyhow::Result<Step> {
    let path = dir.join(&file.dest);
    match std::fs::read(&path) {
        Ok(bytes) if bytes == *file.contents => Ok(Step::Unchanged(file.dest.clone())),
        Ok(_) => Ok(Step::Skipped(format!(
            "{}: differs from the shipped copy (edited), so it is left as is",
            file.dest
        ))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("cannot create {}", parent.display()))?;
            }
            trusty_common::atomic_file::write_atomic(&path, &file.contents)
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

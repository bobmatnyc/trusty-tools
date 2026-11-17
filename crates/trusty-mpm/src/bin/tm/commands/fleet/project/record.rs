//! A watched project's record, `local/projects/<key>.md` (#10895).
//!
//! Why: `add` starts the record the Architect keeps per project (issue scope),
//! and `remove` keeps it, marked removed (plan D5).
//! What: [`template`] picks the Architect's own `_template.md`, else the
//! embedded copy; [`render`] fills the placeholders the registry answers;
//! [`create`] writes a new record and never replaces one; [`mark_removed`]
//! adds the `- Watch: removed <time>` line under the first heading.
//! Test: `record_render_fills_known_placeholders_and_keeps_the_rest`,
//! `record_create_never_overwrites`,
//! `record_mark_removed_appends_after_the_first_bullet_block`.

use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};

use super::super::seed;

/// The record template's path below the Architect directory.
pub(crate) const TEMPLATE: &str = "local/projects/_template.md";

/// The template's copy instruction, which a rendered record drops.
const COPY_NOTE: &str =
    "Copy this file to `<project>.md` in this directory for each watched project.\n\n";

/// What [`render`] fills in.
pub(crate) struct Fields<'a> {
    /// The registry key: the record's title.
    pub(crate) key: &'a str,
    /// `owner/repo`, when the registry's URL names one.
    pub(crate) repo: Option<String>,
    /// The project's directory, when known.
    pub(crate) path: Option<&'a Path>,
    /// The tmux session stem, `tm-<leaf>`.
    pub(crate) session: String,
}

/// `<dir>/local/projects/<key>.md`; a key that is not one plain file-name
/// component is refused.
pub(crate) fn path(dir: &Path, key: &str) -> anyhow::Result<PathBuf> {
    let mut parts = Path::new(key).components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(_)), None) if !key.starts_with('.') => {}
        _ => bail!("registry key {key:?} cannot name a record file"),
    }
    Ok(dir.join("local/projects").join(format!("{key}.md")))
}

/// The template text: the Architect's own copy when present, else the one
/// `tm fleet init` seeds.
pub(crate) fn template(dir: &Path) -> anyhow::Result<String> {
    let local = dir.join(TEMPLATE);
    match std::fs::read_to_string(&local) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => seed::embedded_contents(TEMPLATE)
            .map(str::to_owned)
            .with_context(|| format!("no embedded {TEMPLATE}")),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", local.display())),
    }
}

/// Fill the placeholders `fields` answers; leave every other one as written.
pub(crate) fn render(template: &str, fields: &Fields<'_>) -> String {
    let mut text = template
        .replacen(COPY_NOTE, "", 1)
        .replace("<Project name>", fields.key)
        .replace("<tm-name>", &fields.session);
    if let Some(repo) = &fields.repo {
        text = text.replace("<owner/repo>", repo);
    }
    if let Some(path) = fields.path {
        text = text.replace("<path>", &path.display().to_string());
    }
    text
}

/// Write `text` to `path` unless a record is already there.
///
/// Why: a record is the Architect's history; `add` must never replace one.
/// What: writes a sibling temp file and hard-links it into place, so the
/// record appears whole or not at all and an existing one wins. `Ok(false)`
/// when the record already existed.
/// Test: `record_create_never_overwrites`.
pub(crate) fn create(path: &Path, text: &str) -> std::io::Result<bool> {
    if path.try_exists()? {
        return Ok(false);
    }
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".watch-record-{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        match std::fs::hard_link(&tmp, path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

/// `text` with `- Watch: removed <stamp>` under its first heading.
///
/// Why: AC5; ruling 7 of the brief fixes where the line goes.
/// What: after the first `#` line, past blank lines, a run of `- ` bullets
/// (with indented continuation lines) gets the new line at its end; with no
/// bullets there the line goes directly after the heading. With no heading it
/// goes first.
/// Test: `record_mark_removed_appends_after_the_first_bullet_block`.
pub(crate) fn mark_removed(text: &str, stamp: &str) -> String {
    let line = format!("- Watch: removed {stamp}\n");
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let Some(heading) = lines.iter().position(|l| l.starts_with('#')) else {
        return format!("{line}{text}");
    };
    let mut at = heading + 1;
    let first = lines[at..].iter().position(|l| !l.trim().is_empty());
    if let Some(offset) = first
        && lines[at + offset].starts_with("- ")
    {
        let mut end = at + offset;
        while end < lines.len()
            && (lines[end].starts_with("- ")
                || (lines[end].starts_with(' ') && !lines[end].trim().is_empty()))
        {
            end += 1;
        }
        at = end;
    }
    let mut out = String::with_capacity(text.len() + line.len() + 1);
    for (i, l) in lines.iter().enumerate() {
        if i == at {
            out.push_str(&line);
        }
        out.push_str(l);
    }
    if at == lines.len() {
        // A last line with no newline would run into the inserted one.
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&line);
    }
    out
}

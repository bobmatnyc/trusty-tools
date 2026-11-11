//! The one-time import of the `CLAUDE.md` `## Watch set` lines (plan D9).
//!
//! Why: before #10895 the owner listed watched projects by hand in the
//! Architect's `CLAUDE.md`. An upgraded Architect must keep supervising them
//! with no manual step (plan O3).
//! What: [`plan`] reads the section's `` - `tm-<project>`: `<path>` `` lines
//! (a trailing goal is ignored) and maps each to a registry key: by its path,
//! else by the session name without `tm-`. It never writes; `CLAUDE.md` stays
//! byte-identical, and the Architect replaces the section with the pointer.
//! Test: `migration_parses_lines_with_and_without_a_goal`,
//! `migration_imports_registered_lines_and_skips_the_rest`,
//! `migration_notes_every_bullet_shape_and_a_missing_section`.

use std::path::Path;

use anyhow::{Context, bail};

use super::registry::Registry;

/// The section heading the import reads.
const HEADING: &str = "## Watch set";

/// What the import would add, and one note per line it read.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    /// Registry keys to add, deduplicated, in `CLAUDE.md` order.
    pub(crate) keys: Vec<String>,
    /// One line per imported or skipped list line.
    pub(crate) notes: Vec<String>,
}

/// Whether `line` is the `## Watch set` heading, ignoring ASCII case and
/// the amount of whitespace around and between its words.
fn is_heading(line: &str) -> bool {
    let mut got = line.split_whitespace();
    HEADING
        .split_whitespace()
        .all(|w| got.next().is_some_and(|g| g.eq_ignore_ascii_case(w)))
        && got.next().is_none()
}

/// Whether `line` is a Markdown bullet at any indentation: `-`, `*` or `+`
/// followed by whitespace or the end of the line.
fn is_bullet(line: &str) -> bool {
    let mut chars = line.trim_start().chars();
    matches!(chars.next(), Some('-' | '*' | '+')) && chars.next().is_none_or(char::is_whitespace)
}

/// The bullet lines of `text`'s watch-set section; `None` with no section.
///
/// #10895 review: an indented or `*`/`+` bullet is kept, so [`plan`] can
/// import it or name it in a skip note; none is dropped unseen.
pub(crate) fn section_lines(text: &str) -> Option<Vec<&str>> {
    let mut lines = text.lines();
    lines.find(|l| is_heading(l))?;
    Some(
        lines
            .take_while(|l| !l.starts_with("# ") && !l.starts_with("## "))
            .filter(|l| is_bullet(l))
            .collect(),
    )
}

/// The `(session, path)` of a list line; `None` for any other bullet.
pub(crate) fn parse_line(line: &str) -> Option<(&str, &str)> {
    let rest = line.trim_start().get(1..)?.trim_start();
    let (session, rest) = rest.strip_prefix('`')?.split_once('`')?;
    let (path, _goal) = rest.strip_prefix(": `")?.split_once('`')?;
    (is_bullet(line) && !session.is_empty() && !path.is_empty()).then_some((session, path))
}

/// The template's own example line, which the import passes over silently.
fn is_placeholder(session: &str, path: &str) -> bool {
    session.contains('<') || path.starts_with('<')
}

/// Map the `CLAUDE.md` list in `dir` onto registry keys.
///
/// Why: see the module doc.
/// What: an absent `CLAUDE.md` is an empty plan; an unreadable one is an
/// error. A `CLAUDE.md` with no watch-set heading is an empty plan with one
/// note saying so. Each list line maps by [`Registry::key_for_path`], else by
/// the session name with `tm-` stripped matched to a key exactly, then
/// ignoring ASCII case. Unmatched and unparseable bullets get a `skipped`
/// note saying why; the template's placeholder line gets none. With no
/// registry file and at least one project line, the plan is an error naming
/// the registry path and `TRUSTY_MPM_ROOT`: importing then would use up the
/// import and drop every line.
/// Test: `migration_imports_registered_lines_and_skips_the_rest`,
/// `migration_notes_every_bullet_shape_and_a_missing_section`,
/// `migration_refuses_without_a_registry_file_and_writes_nothing`.
pub(crate) fn plan(dir: &Path, registry: &Registry) -> anyhow::Result<Plan> {
    let path = dir.join("CLAUDE.md");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Plan::default()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let mut plan = Plan::default();
    let Some(lines) = section_lines(&text) else {
        plan.notes.push(format!(
            "watch-set import: {} has no \"{HEADING}\" section; nothing imported",
            path.display()
        ));
        return Ok(plan);
    };
    let projects = lines
        .iter()
        .filter_map(|line| parse_line(line))
        .filter(|(s, p)| !is_placeholder(s, p))
        .count();
    if registry.is_absent() && projects > 0 {
        // #10895 review: an absent registry would skip every line and the
        // written list would use the import up; refuse before any write.
        bail!(
            "watch-set import refused: the project registry {} does not exist, and {} lists \
             {} project(s) under \"{HEADING}\". Check TRUSTY_MPM_ROOT, which selects the \
             managed root the registry is read from, then run the command again; nothing was \
             written",
            registry.source().display(),
            path.display(),
            projects
        );
    }
    for line in lines {
        let Some((session, checkout)) = parse_line(line) else {
            plan.notes.push(format!(
                "watch-set import: skipped {line:?}: not a \"- `tm-<project>`: `<path>`\" line"
            ));
            continue;
        };
        if is_placeholder(session, checkout) {
            continue;
        }
        let name = session.strip_prefix("tm-").unwrap_or(session);
        let key = registry
            .key_for_path(Path::new(checkout))
            .or_else(|| registry.key_named(name));
        match key {
            Some(key) => {
                plan.notes
                    .push(format!("watch-set import: added {key} (from {line:?})"));
                if !plan.keys.iter().any(|k| k == key) {
                    plan.keys.push(key.to_owned());
                }
            }
            None => plan.notes.push(format!(
                "watch-set import: skipped {line:?}: no registered project has path {checkout} \
                 or key {name}; register it with `tm projects register`, then run \
                 `tm fleet project add`"
            )),
        }
    }
    Ok(plan)
}

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
//! `migration_imports_registered_lines_and_skips_the_rest`.

use std::path::Path;

use anyhow::Context;

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

/// The bullet lines of `text`'s `## Watch set` section.
pub(crate) fn section_lines(text: &str) -> Vec<&str> {
    text.lines()
        .skip_while(|l| l.trim_end() != HEADING)
        .skip(1)
        .take_while(|l| !l.starts_with("# ") && !l.starts_with("## "))
        .filter(|l| l.starts_with("- "))
        .collect()
}

/// The `(session, path)` of a list line; `None` for any other bullet.
pub(crate) fn parse_line(line: &str) -> Option<(&str, &str)> {
    let (session, rest) = line.strip_prefix("- `")?.split_once('`')?;
    let (path, _goal) = rest.strip_prefix(": `")?.split_once('`')?;
    (!session.is_empty() && !path.is_empty()).then_some((session, path))
}

/// The template's own example line, which the import passes over silently.
fn is_placeholder(session: &str, path: &str) -> bool {
    session.contains('<') || path.starts_with('<')
}

/// Map the `CLAUDE.md` list in `dir` onto registry keys.
///
/// Why: see the module doc.
/// What: an absent `CLAUDE.md` or section is an empty plan; an unreadable one
/// is an error. Each list line maps by [`Registry::key_for_path`], else by
/// the session name with `tm-` stripped matched to a key exactly, then
/// ignoring ASCII case. Unmatched and unparseable bullets get a `skipped`
/// note saying why; the template's placeholder line gets none.
/// Test: `migration_imports_registered_lines_and_skips_the_rest`.
pub(crate) fn plan(dir: &Path, registry: &Registry) -> anyhow::Result<Plan> {
    let path = dir.join("CLAUDE.md");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Plan::default()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let mut plan = Plan::default();
    for line in section_lines(&text) {
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

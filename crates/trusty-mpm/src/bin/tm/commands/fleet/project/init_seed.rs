//! `tm fleet init` seeds `local/watch.json` (#11012, epic #10894 phase 3).
//!
//! Why: the owner asked that installing the Architect creates its watch list
//! (2026-11-09). Before this, a new Architect had no list until the first
//! `tm fleet project` command, whose `CLAUDE.md` import found nothing to
//! import.
//! What: [`steps`] creates the list only when it is absent, from the live
//! `tm-*` tmux sessions mapped to registry keys ([`key_for_session`]) plus
//! the keys the one-time `CLAUDE.md` import ([`migrate::plan`]) would add, so
//! a re-run on an upgraded Architect drops no hand-listed project. Each
//! key's record is written before the list, as `tm fleet project add` does.
//! An existing list, even an empty one, is never changed, and a malformed one
//! is a failed step that leaves it byte-identical.
//! Test: `init_seed_writes_the_mapped_projects_and_their_records`,
//! `init_seed_rerun_leaves_the_list_byte_identical`.

use std::path::{Path, PathBuf};

use anyhow::Context;

use super::super::Step;
use super::list::count_sessions;
use super::registry::Registry;
use super::{migrate, record, render_record, watch};

/// One live tmux session: its name and its start directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveSession {
    /// The session name, e.g. `tm-alpha-02`.
    pub(crate) name: String,
    /// `#{session_path}`; `None` when tmux printed none.
    pub(crate) path: Option<PathBuf>,
}

/// What `init` seeds the list from; [`super::init_watch`] reads it.
pub(crate) struct InitWatch {
    /// The registry from disk; an error becomes a FAILED step.
    pub(crate) registry: anyhow::Result<Registry>,
    /// The live sessions; `None` when tmux could not be read.
    pub(crate) sessions: Option<Vec<LiveSession>>,
}

/// The registry key session `session` belongs to, or why it has none.
///
/// Why: a wrong mapping silently drops or adds a watched project (issue
/// risk), so a session maps only when exactly one project claims it.
/// What: the projects whose session alias names it — the stem rule `tm fleet
/// project list` counts sessions by ([`count_sessions`]) — and, when none
/// does, the project whose directory is the session's path
/// ([`Registry::key_for_path`]). Two alias matches are ambiguous: `Err`.
/// Test: `init_seed_writes_the_mapped_projects_and_their_records`.
pub(crate) fn key_for_session<'r>(
    registry: &'r Registry,
    session: &LiveSession,
) -> Result<&'r str, String> {
    let name = std::slice::from_ref(&session.name);
    let by_alias: Vec<&str> = registry
        .projects()
        .iter()
        .filter(|p| count_sessions(name, registry.session_alias(p)) == 1)
        .map(|p| p.name.as_str())
        .collect();
    match by_alias.as_slice() {
        [key] => Ok(key),
        [] => session
            .path
            .as_deref()
            .and_then(|path| registry.key_for_path(path))
            .ok_or_else(|| {
                let path = session
                    .path
                    .as_deref()
                    .map_or_else(|| "no path".to_owned(), |p| p.display().to_string());
                format!("no registered project has its session alias or its path ({path})")
            }),
        many => Err(format!(
            "its name matches more than one project ({})",
            many.join(", ")
        )),
    }
}

/// The `init` report lines for the list in `dir`; see the module doc.
///
/// Why: issue #11012 scope and acceptance criteria 1-5.
/// What: a list that exists and parses is `Unchanged`, naming its entry
/// count. A list that does not parse, a registry that did not load, and a
/// record or list write that failed are each one `Failed` line naming the
/// file, so `init` exits 1. With no registry file the list is not written
/// (`Skipped`), leaving the seed for a later run. Otherwise one `Skipped`
/// line per live `tm-*` session that maps to no key (the `own` Architect
/// sessions excluded) and per import line the `CLAUDE.md` import skipped,
/// then `Changed` naming the keys written. No tmux, or no `tm-*` session,
/// writes an empty list and says so.
/// Test: `init_seed_writes_the_mapped_projects_and_their_records`,
/// `init_seed_rerun_leaves_the_list_byte_identical`,
/// `init_seed_never_edits_an_owner_changed_list`,
/// `init_seed_malformed_list_fails_and_is_left_unchanged`,
/// `init_seed_write_failure_is_a_failed_step`,
/// `init_seed_without_tmux_writes_an_empty_list`,
/// `init_seed_import_key_without_a_live_session_survives_init_reruns`,
/// `init_seed_without_a_registry_writes_nothing`.
pub(crate) fn steps(dir: &Path, watch: &InitWatch, own: &[&str]) -> Vec<Step> {
    let path = watch::path(dir);
    match watch::read(&path) {
        Ok(Some(file)) => {
            return vec![existing(&path, &file)];
        }
        Ok(None) => {}
        Err(e) => {
            return vec![Step::Failed(format!(
                "watch list: {e:#}; init left it unchanged — fix or delete it, then run \
                 `tm fleet init` again"
            ))];
        }
    }
    let registry = match &watch.registry {
        Ok(registry) => registry,
        Err(e) => {
            return vec![Step::Failed(format!(
                "watch list {}: cannot read the project registry: {e:#}",
                path.display()
            ))];
        }
    };
    if registry.is_absent() {
        return vec![Step::Skipped(format!(
            "watch list {}: the project registry {} does not exist, so no session maps to a \
             project; check TRUSTY_MPM_ROOT, then run `tm fleet init` again",
            path.display(),
            registry.source().display()
        ))];
    }
    let mut out = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    let mut from_sessions = 0;
    let live: Vec<&LiveSession> = watch
        .sessions
        .iter()
        .flatten()
        .filter(|s| s.name.starts_with("tm-") && !own.contains(&s.name.as_str()))
        .collect();
    for session in &live {
        match key_for_session(registry, session) {
            Ok(key) if keys.iter().any(|k| k == key) => {}
            Ok(key) => {
                keys.push(key.to_owned());
                from_sessions += 1;
            }
            Err(why) => out.push(Step::Skipped(format!(
                "watch list: tmux session {} not watched: {why}",
                session.name
            ))),
        }
    }
    match seed(dir, registry, &path, keys) {
        Ok(Some((keys, notes))) => {
            out.extend(
                notes
                    .into_iter()
                    .filter(|n| n.contains(": skipped "))
                    .map(Step::Skipped),
            );
            out.push(Step::Changed(written(
                &path,
                &keys,
                from_sessions,
                watch.sessions.is_none(),
            )));
        }
        // #11012: another command created the list while this one ran.
        Ok(None) => match watch::read(&path) {
            Ok(Some(file)) => out.push(existing(&path, &file)),
            Ok(None) => out.push(Step::Failed(format!(
                "watch list {}: vanished",
                path.display()
            ))),
            Err(e) => out.push(Step::Failed(format!("watch list: {e:#}"))),
        },
        Err(e) => out.push(Step::Failed(format!(
            "watch list {}: not written: {e:#}",
            path.display()
        ))),
    }
    out
}

/// Under the list's lock: append the import's keys, write every record,
/// then the list. `None` when a list appeared meanwhile.
fn seed(
    dir: &Path,
    registry: &Registry,
    path: &Path,
    mut keys: Vec<String>,
) -> anyhow::Result<Option<(Vec<String>, Vec<String>)>> {
    watch::create_if_absent(path, || {
        let plan = migrate::plan(dir, registry)?;
        for key in plan.keys {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        for key in &keys {
            // #11012: record first, then the list, as `add` does.
            let record_path = record::path(dir, key)?;
            let text = render_record(dir, registry, key)?;
            record::create(&record_path, &text)
                .with_context(|| format!("cannot write the record {}", record_path.display()))?;
        }
        Ok((watch::WatchFile::of(&keys), (keys, plan.notes)))
    })
}

/// The line for a list that already exists.
fn existing(path: &Path, file: &watch::WatchFile) -> Step {
    Step::Unchanged(format!(
        "watch list {} exists with {} project(s); init leaves it as is — change it with \
         `tm fleet project add|remove`",
        path.display(),
        file.projects.len()
    ))
}

/// The line for a list this run wrote.
fn written(path: &Path, keys: &[String], from_sessions: usize, no_tmux: bool) -> String {
    if keys.is_empty() {
        let why = if no_tmux {
            "tmux could not be read"
        } else {
            "no live tm-* session maps to a registered project"
        };
        return format!(
            "wrote an empty watch list {} ({why}); an empty list watches every tm-* session. \
             Add projects with `tm fleet project add <key|dir>`",
            path.display()
        );
    }
    format!(
        "wrote watch list {} with {} project(s), {from_sessions} from live tm-* sessions: {}",
        path.display(),
        keys.len(),
        keys.join(", ")
    )
}

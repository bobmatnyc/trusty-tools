//! `tm fleet project add|remove|list` — the Architect's watch list (#10895).
//!
//! Why: the owner used to edit the Architect's `CLAUDE.md` by hand to change
//! which projects it supervises. Epic #10894 moves that list into
//! `local/watch.json`, changed only through these commands (plan D1, D6).
//! What: [`run`] resolves the Architect directory, the managed root (so
//! `TRUSTY_MPM_ROOT` is honoured), the registry from disk and the live tmux
//! session names, then calls [`add`], [`remove`] or [`list()`]. Those take
//! every input as an argument, so the unit tests never reach the operator's
//! home, root or tmux. A refused command writes nothing: it is validated
//! before the one-time `CLAUDE.md` import ([`watch::ensure`]) runs.
//! Test: `commands::fleet::project::tests`, `tests/tm_fleet.rs`
//! (`fleet_project_*`).

mod list;
mod migrate;
mod record;
mod registry;
mod watch;

use std::path::Path;

use anyhow::{Context, bail};

use self::registry::Registry;
use crate::cli::FleetProjectAction;

/// What a command did: import notes for stderr, the result for stdout.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    /// One line per `CLAUDE.md` line the one-time import read.
    pub(crate) notes: Vec<String>,
    /// The command's own report.
    pub(crate) message: String,
}

/// Dispatch `tm fleet project <action>`.
///
/// Why: the CLI entry point; it is the only place that reads the process
/// environment and tmux.
/// What: `--dir` else the recorded Architect else the default
/// ([`super::resolve_dir`]); the root from `resolve_managed_paths(None)`;
/// [`Registry::load`]; tmux `list-sessions` for `list`. Notes go to stderr,
/// so `list --json` stays parseable.
/// Test: `tests/tm_fleet.rs` (`fleet_project_*`).
pub(crate) async fn run(action: FleetProjectAction, home: &Path) -> anyhow::Result<()> {
    let root = crate::commands::managed_root::resolve_managed_paths(None)?.root;
    let registry = Registry::load(&root).await?;
    let (outcome, rows, json) = match action {
        FleetProjectAction::Add { project, dir } => {
            let dir = architect_dir(dir.as_deref(), home)?;
            (add(&dir, &registry, &project)?, None, false)
        }
        FleetProjectAction::Remove { key, dir } => {
            let dir = architect_dir(dir.as_deref(), home)?;
            let stamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
            (remove(&dir, &registry, &key, &stamp)?, None, false)
        }
        FleetProjectAction::List { json, dir } => {
            let dir = architect_dir(dir.as_deref(), home)?;
            let names = live_session_names();
            let (outcome, rows) = list(&dir, &registry, names.as_deref())?;
            (outcome, Some(rows), json)
        }
    };
    for note in &outcome.notes {
        eprintln!("{note}");
    }
    match rows {
        Some(rows) if json => println!("{}", serde_json::to_string_pretty(&rows)?),
        _ => print!("{}", outcome.message),
    }
    Ok(())
}

/// The Architect directory; it must exist, or there is no list to edit.
fn architect_dir(dir: Option<&str>, home: &Path) -> anyhow::Result<std::path::PathBuf> {
    let dir = super::resolve_dir(dir, home)?;
    if !dir.is_dir() {
        bail!(
            "no Architect directory at {}; run `tm fleet init` first, or pass --dir",
            dir.display()
        );
    }
    Ok(dir)
}

/// Watch the project `target` names: record first, then the list entry.
///
/// Why: issue scope and AC1, AC2, AC4. The record is written before the entry
/// so a listed project always has one, and a failure leaves the list as it
/// was.
/// What: `target` resolves to a key ([`Registry::resolve`]) before anything
/// is written; an unknown project is an error naming the registry and the
/// command that registers it. Then the one-time import runs, and under the
/// list's lock the record is created when absent and the key appended when
/// absent. A record failure returns `Err` from the closure, so the list is not
/// published; a list-publish failure after the record is an error, and a
/// re-run finds the record and adds the entry. A listed key whose record
/// exists changes nothing and says so.
/// Test: `add_of_an_unregistered_project_writes_nothing`,
/// `add_twice_leaves_one_entry_and_reports_unchanged`,
/// `add_record_write_failure_leaves_the_list_unchanged`,
/// `add_list_write_failure_after_the_record_fails_and_a_rerun_completes`.
pub(crate) fn add(dir: &Path, registry: &Registry, target: &str) -> anyhow::Result<Outcome> {
    let Some(key) = registry.resolve(target) else {
        bail!(
            "{target} is not a registered project: {} holds no such key or checkout path. \
             Register it with `tm projects register <name> --repo-url <url>`, then run \
             `tm fleet project add` again",
            registry.source().display()
        );
    };
    let record_path = record::path(dir, key)?;
    let text = render_record(dir, registry, key)?;
    let notes = watch::ensure(dir, registry)?;
    let list_path = watch::path(dir);
    let listed = watch::read(&list_path)?.is_some_and(|f| f.contains(key));
    if listed && record_path.exists() {
        let message = format!("unchanged: {key} is already watched\n");
        return Ok(Outcome { notes, message });
    }
    let (created, added) = watch::update(&list_path, |file| {
        // #10895: the record lands before the entry; a failure here is an
        // `Err`, so the list is left byte-identical.
        let created = record::create(&record_path, &text)
            .with_context(|| format!("cannot write the record {}", record_path.display()))?;
        let added = !file.contains(key);
        if added {
            file.projects.push(watch::Entry {
                key: key.to_owned(),
            });
        }
        Ok((created, added))
    })
    .with_context(|| format!("cannot add {key} to {}", list_path.display()))?;
    let mut message = String::new();
    if created {
        message.push_str(&format!("wrote {}\n", record_path.display()));
    }
    message.push_str(&if added {
        format!("added {key} to the watch list\n")
    } else {
        format!("unchanged: {key} is already watched\n")
    });
    Ok(Outcome { notes, message })
}

/// The record `add` writes for `key`, from the template.
fn render_record(dir: &Path, registry: &Registry, key: &str) -> anyhow::Result<String> {
    let project = registry
        .get(key)
        .with_context(|| format!("{key} left the registry"))?;
    let alias = registry.alias_for(project);
    let path = registry.path_for(project);
    let fields = record::Fields {
        key,
        repo: trusty_common::github_path::parse_github_path(&project.repo_url)
            .filter(|gh| gh.owner != trusty_common::github_path::UNKNOWN_OWNER)
            .map(|gh| format!("{}/{}", gh.owner, gh.repo)),
        path: path.as_deref(),
        session: list::session_stem(alias.map_or(key, |a| a.alias.as_str())),
    };
    Ok(record::render(&record::template(dir)?, &fields))
}

/// Stop watching `key`: mark its record removed, then drop the entry.
///
/// Why: AC5 and plan D5 — the record is kept as history.
/// What: a key the list does not hold is an error that writes nothing; with
/// no list yet, the import's plan decides that, unwritten. Then the import
/// runs, and under the list's lock the record gets `- Watch: removed
/// <stamp>` ([`record::mark_removed`]) and the entry is dropped. A missing
/// record is reported, not an error.
/// Test: `remove_marks_the_record_and_drops_the_entry`,
/// `remove_of_an_unlisted_key_writes_nothing`,
/// `remove_record_mark_failure_keeps_the_entry`.
pub(crate) fn remove(
    dir: &Path,
    registry: &Registry,
    key: &str,
    stamp: &str,
) -> anyhow::Result<Outcome> {
    let list_path = watch::path(dir);
    let listed = match watch::read(&list_path)? {
        Some(file) => file.contains(key),
        None => migrate::plan(dir, registry)?.keys.iter().any(|k| k == key),
    };
    if !listed {
        bail!("{key} is not on the watch list; `tm fleet project list` shows the list");
    }
    let record_path = record::path(dir, key)?;
    let notes = watch::ensure(dir, registry)?;
    let marked = watch::update(&list_path, |file| {
        if !file.contains(key) {
            bail!("{key} left the watch list while this command waited for it");
        }
        let marked = match std::fs::read_to_string(&record_path) {
            Ok(text) => {
                let text = record::mark_removed(&text, stamp);
                trusty_common::atomic_file::write_atomic(&record_path, text.as_bytes())
                    .with_context(|| format!("cannot mark {}", record_path.display()))?;
                true
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                return Err(e).with_context(|| format!("cannot read {}", record_path.display()));
            }
        };
        file.projects.retain(|e| e.key != key);
        Ok(marked)
    })
    .with_context(|| format!("cannot remove {key} from {}", list_path.display()))?;
    let record_line = if marked {
        format!("marked {} removed\n", record_path.display())
    } else {
        format!("no record at {}; nothing to mark\n", record_path.display())
    };
    let message = format!("{record_line}removed {key} from the watch list\n");
    Ok(Outcome { notes, message })
}

/// The watch list's rows, after the one-time import.
///
/// Why: AC6 and AC7.
/// What: [`watch::ensure`], then [`list::rows`] over the list. The message
/// is the human table.
/// Test: `list_rows_carry_the_eight_fields`,
/// `migration_runs_once_and_an_emptied_list_stays_empty`.
pub(crate) fn list(
    dir: &Path,
    registry: &Registry,
    sessions: Option<&[String]>,
) -> anyhow::Result<(Outcome, Vec<list::Row>)> {
    let notes = watch::ensure(dir, registry)?;
    let list_path = watch::path(dir);
    let file = watch::read(&list_path)?.unwrap_or_default();
    let rows = list::rows(dir, &file, registry, sessions);
    let message = list::render(&rows, &list_path);
    Ok((Outcome { notes, message }, rows))
}

/// The live tmux session names, or `None` when tmux cannot be read.
///
/// "No server" is zero sessions, not an unknown count.
fn live_session_names() -> Option<Vec<String>> {
    use trusty_mpm::core::tmux::{TmuxCommand, run_tmux};
    let out = run_tmux(&TmuxCommand::ListSessions).ok()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let no_server = err.contains("no server running") || err.contains("error connecting");
        return no_server.then(Vec::new);
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.split(':').next())
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

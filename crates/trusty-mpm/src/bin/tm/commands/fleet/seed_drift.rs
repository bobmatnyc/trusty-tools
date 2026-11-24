//! Read-only drift report over the Architect's managed files (#11773).
//!
//! Why: `tm fleet init` keeps an edited file and refreshes an unedited one,
//! but nothing told the owner which files differ from this binary's bundle
//! and why. #11773 asks for a read-only report first, and an opt-in
//! `tm architect up --strict` that refuses to start over an edited script.
//! What: [`classify`] compares every file `tm fleet init` manages
//! ([`super::seed::files`] plus the Architect agents) with its bytes on disk,
//! the current seed and the shipped history (`seed_history.tsv`, through
//! [`refresh::shipped_before`]), and adds the installed provenance sha
//! ([`refresh::PROVENANCE_DEST`]) where one maps. Each path gets one
//! [`State`]. It reads only; it never writes, locks or fetches.
//! Test: `seed_drift_tests.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Context;
use serde::Serialize;
use trusty_mpm::core::project_skill_tier::architect_seed::ArchitectSeed;
use trusty_mpm::core::project_skill_tier::architect_skills::RECORD_FILE;

use super::seed::{self, SHIPPED_SUFFIX, ShippedBefore, refresh};

/// How one path under the Architect directory relates to the shipped bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum State {
    /// Bytes equal the current seed.
    Current,
    /// Bytes equal an earlier shipped copy; the next init refreshes them.
    Stale,
    /// Bytes match no shipped copy: someone edited the file.
    Edited,
    /// A managed path with no file.
    Absent,
    /// A file under a managed directory that tm does not seed, or an orphaned
    /// `*.shipped` sibling.
    Extra,
    /// The owner's own file (`AGENTS.md`); never edited or stale.
    Owner,
    /// An instance-owned `local/` file that differs from the template
    /// ([`refresh::instance_owned`], #10608); never edited.
    Instance,
}

impl State {
    /// Every state, in report order.
    pub(crate) const ALL: [State; 7] = [
        State::Current,
        State::Stale,
        State::Edited,
        State::Absent,
        State::Extra,
        State::Owner,
        State::Instance,
    ];

    /// The lowercase name the report and `--json` print.
    pub(crate) fn name(self) -> &'static str {
        match self {
            State::Current => "current",
            State::Stale => "stale",
            State::Edited => "edited",
            State::Absent => "absent",
            State::Extra => "extra",
            State::Owner => "owner",
            State::Instance => "instance",
        }
    }
}

/// One path in the report, with the three shas it was judged by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Entry {
    /// Path relative to the Architect directory, `/`-separated.
    pub(crate) path: String,
    /// The verdict.
    pub(crate) state: State,
    /// sha256 of the bytes on disk; `None` when absent.
    #[serde(rename = "disk_sha256")]
    pub(crate) disk: Option<String>,
    /// sha256 of the bytes this binary's seed ships; `None` for an unmanaged path.
    #[serde(rename = "embedded_sha256")]
    pub(crate) embedded: Option<String>,
    /// sha256 the installed `bundle-provenance.json` records for this path.
    #[serde(rename = "provenance_sha256")]
    pub(crate) provenance: Option<String>,
}

/// The whole drift report for one Architect directory.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Report {
    /// The Architect directory that was read.
    pub(crate) dir: String,
    /// One entry per path, sorted by path.
    pub(crate) files: Vec<Entry>,
}

impl Report {
    /// How many entries are in `state`.
    pub(crate) fn count(&self, state: State) -> usize {
        self.files.iter().filter(|e| e.state == state).count()
    }

    /// The edited managed `scripts/*` paths, the set `up --strict` refuses on.
    ///
    /// Why: #11773 ruling — only an edited script fails `--strict`; an edited
    /// skill, `CLAUDE.md` or schema is reported by `drift` but starts.
    /// Test: `strict_refuses_an_edited_script_and_writes_nothing`.
    pub(crate) fn edited_scripts(&self) -> Vec<&str> {
        self.files
            .iter()
            .filter(|e| e.state == State::Edited && e.path.starts_with("scripts/"))
            .map(|e| e.path.as_str())
            .collect()
    }

    /// `drift`'s exit code: 1 when any file is edited, else 0.
    pub(crate) fn exit(&self) -> i32 {
        i32::from(self.count(State::Edited) > 0)
    }
}

/// Files the owner writes; tm seeds none of them (#11773: `AGENTS.md`).
pub(crate) const OWNER_FILES: &[&str] = &["AGENTS.md"];

/// Directories whose unseeded files are `extra`.
const MANAGED_DIRS: &[&str] = &["scripts", ".claude/skills"];

/// Directory names the extra walk never enters (interpreter caches).
const SKIP_DIRS: &[&str] = &["__pycache__"];

/// [`classify_with`] against the compiled-in shipped history.
///
/// Why/What: see the module doc.
/// Test: `an_untouched_init_reads_current`.
pub(crate) fn classify(dir: &Path, seed: &ArchitectSeed) -> anyhow::Result<Report> {
    classify_with(dir, seed, &refresh::shipped_before)
}

/// The drift report for `dir`, with the shipped-history predicate injected.
///
/// Why: a test needs a stale copy whose bytes it knows; the real history
/// holds only hashes (as [`seed::deploy_with`]).
/// What: per managed path — absent; `local/*` equal to the seed is current,
/// else `instance`; equal to the seed is current; `shipped_before` is stale;
/// any other bytes are edited. Then `AGENTS.md` as `owner`, then `extra`.
/// A `records/`-era directory skips the `local/` rows, as init does.
/// # Errors
/// A managed path that exists but cannot be read (not `NotFound`).
/// Test: `each_state_is_classified`, `a_stale_history_copy_reads_stale`,
/// `extras_and_orphaned_shipped_siblings_read_extra`.
pub(crate) fn classify_with(
    dir: &Path,
    seed: &ArchitectSeed,
    shipped_before: ShippedBefore<'_>,
) -> anyhow::Result<Report> {
    let provenance = installed_provenance(dir);
    let records_era = dir.join(super::status::STALE_RECORDS_DIR).is_dir();
    let mut managed: Vec<(String, &str)> = seed::files(seed)
        .into_iter()
        .map(|f| (f.dest, f.contents))
        .collect();
    // #11773: the agents are seeded by seed_agents.rs, outside `files`.
    managed.extend(
        seed.agents
            .iter()
            .map(|a| (format!(".claude/agents/{}.md", a.stem), a.contents.as_str())),
    );
    let mut files = Vec::with_capacity(managed.len() + 8);
    for (dest, contents) in &managed {
        if records_era && refresh::instance_owned(dest) {
            continue;
        }
        let disk = read_optional(&dir.join(dest))?;
        let state = match disk.as_deref() {
            None => State::Absent,
            Some(b) if b == contents.as_bytes() => State::Current,
            // #11773: instance-owned files are a class of their own, never edited.
            Some(_) if refresh::instance_owned(dest) => State::Instance,
            Some(b) if shipped_before(dest, b) => State::Stale,
            Some(_) => State::Edited,
        };
        files.push(Entry {
            path: dest.clone(),
            state,
            disk: disk.as_deref().map(refresh::sha256_hex),
            embedded: Some(refresh::sha256_hex(contents.as_bytes())),
            provenance: provenance_key(dest).and_then(|k| provenance.get(&k).cloned()),
        });
    }
    for owner in OWNER_FILES {
        if let Some(bytes) = read_optional(&dir.join(owner))? {
            files.push(unmanaged(owner, State::Owner, &bytes));
        }
    }
    let edited: BTreeSet<&str> = files
        .iter()
        .filter(|e| e.state == State::Edited)
        .map(|e| e.path.as_str())
        .collect();
    let known: BTreeSet<&str> = managed.iter().map(|(d, _)| d.as_str()).collect();
    let mut extras = BTreeSet::new();
    for rel in MANAGED_DIRS {
        walk(dir, rel, &mut extras);
    }
    // #11773: a `.shipped` sibling outside the walked directories.
    for dest in &known {
        let sibling = format!("{dest}{SHIPPED_SUFFIX}");
        if dir.join(&sibling).is_file() {
            extras.insert(sibling);
        }
    }
    let mut extra_entries = Vec::new();
    for rel in extras {
        if known.contains(rel.as_str()) || rel.ends_with(RECORD_FILE) {
            continue;
        }
        // A sibling beside a kept edit is #11245's, not an orphan.
        if let Some(base) = rel.strip_suffix(SHIPPED_SUFFIX)
            && edited.contains(base)
        {
            continue;
        }
        let bytes = std::fs::read(dir.join(&rel)).unwrap_or_default();
        extra_entries.push(unmanaged(&rel, State::Extra, &bytes));
    }
    files.extend(extra_entries);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Report {
        dir: dir.display().to_string(),
        files,
    })
}

/// An entry for a path the seed does not ship.
fn unmanaged(path: &str, state: State, bytes: &[u8]) -> Entry {
    Entry {
        path: path.to_owned(),
        state,
        disk: Some(refresh::sha256_hex(bytes)),
        embedded: None,
        provenance: None,
    }
}

/// The file's bytes, `None` when it does not exist.
fn read_optional(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Every regular file below `dir/rel`, as `/`-separated paths relative to `dir`.
fn walk(dir: &Path, rel: &str, out: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir.join(rel)) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let child = format!("{rel}/{name}");
        match entry.file_type() {
            Ok(t) if t.is_dir() => {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    walk(dir, &child, out);
                }
            }
            Ok(_) => {
                out.insert(child);
            }
            Err(_) => {}
        }
    }
}

/// The `files` map of the installed provenance stamp; empty when absent or malformed.
fn installed_provenance(dir: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(dir.join(refresh::PROVENANCE_DEST))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("files").cloned())
        .and_then(|f| serde_json::from_value(f).ok())
        .unwrap_or_default()
}

/// The provenance `files` key (a `python/trusty-architect/` path) for `dest`.
///
/// Why: provenance names the sub-project's pair sources, not the seeded
/// destinations. What: the fixed renames of the seed table; `None` for the
/// provenance stamp itself. Test: `the_json_report_names_three_shas_per_path`.
pub(crate) fn provenance_key(dest: &str) -> Option<String> {
    if dest == refresh::PROVENANCE_DEST {
        return None;
    }
    if let Some(rest) = dest.strip_prefix(".claude/skills/") {
        return rest
            .strip_suffix("/SKILL.md")
            .map(|s| format!("skills/{s}.md"));
    }
    if let Some(rest) = dest.strip_prefix(".claude/agents/") {
        return Some(format!("agents/{rest}"));
    }
    Some(match dest {
        "CLAUDE.md" => "templates/CLAUDE.md".to_owned(),
        ".gitignore" => "templates/gitignore".to_owned(),
        "docs/architect/voice.md" => "docs/voice.md".to_owned(),
        _ if dest.starts_with("local/") => format!("templates/{dest}"),
        _ => dest.to_owned(),
    })
}

#[cfg(test)]
#[path = "seed_drift_tests.rs"]
mod tests;

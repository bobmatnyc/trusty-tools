//! Preset versions, `tm il config diff` and `tm il config update` (#11002).
//!
//! Why: ADR-0077 D6. A new preset never overwrites an edited file, and
//! `tm fleet init` keeps every `local/` file as is (#10608), so a preset change
//! never reached an existing install and an edited file could not see what
//! changed.
//! What: [`PRESETS`] holds the preset each `schema_version` shipped (index =
//! version; absent = 0). [`diff`] classifies each difference between the file
//! and the current preset by a three-way comparison against the preset of the
//! file's own version: a key still at its old preset value is a preset change
//! ([`DeltaKind::Preset`]), any other difference is a user edit
//! ([`DeltaKind::Edit`]). [`upgrade`] runs step k -> k+1 for each older
//! version in order (ADR-0067 D3 rule 2): the same merge takes preset changes
//! and keeps edits, then the file is stamped. It backs the file up first and
//! aborts when the backup fails (rule 3). A newer file is refused (rule 1).
//! Test: `core::il_schedule::migrate_tests`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::{IlScheduleError, Outcome, PRESET, Schedule, locked};

/// The schema version this binary writes: the current preset's.
pub const SCHEMA_VERSION: u64 = 1;

/// The preset each schema version shipped; `PRESETS[k]` is version `k`.
///
/// Version 0 is the preset `tm fleet init` seeded before `schema_version`
/// existed (`seed_history.tsv` sha256 `e477725c…`). A step is never removed
/// while a file at its version may exist (ADR-0067 D7).
const PRESETS: [&str; 2] = [include_str!("presets/v0.json"), PRESET];

// #11002: a version bump without its preset snapshot fails the build.
const _: () = assert!(PRESETS.len() == SCHEMA_VERSION as usize + 1);

/// How [`upgrade`] treats one difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaKind {
    /// The file still holds the old preset value: an update takes the new one.
    Preset,
    /// The user changed the value: an update keeps it.
    Edit,
}

/// One key path where the file and the current preset differ.
#[derive(Debug, Clone, PartialEq)]
pub struct Delta {
    /// Dotted key path, e.g. `thresholds.floor_ctx`.
    pub path: String,
    /// The file's value; `None` when the key is absent.
    pub file: Option<Value>,
    /// The current preset's value; `None` when the key is absent.
    pub preset: Option<Value>,
    /// Whether an update applies the preset value or keeps the file's.
    pub kind: DeltaKind,
}

/// What [`diff`] found.
#[derive(Debug)]
pub struct Diff {
    /// Whether the schedule file exists; absent means the preset applies.
    pub from_file: bool,
    /// The file's `schema_version` (absent = 0).
    pub file_version: u64,
    /// Every differing key path, `schema_version` excluded.
    pub deltas: Vec<Delta>,
}

impl Diff {
    /// The report `tm il config diff` prints: header, one line per delta, summary.
    ///
    /// Test: `diff_lists_preset_changes_and_edits`.
    pub fn render(&self, path: &Path) -> String {
        let mut out = String::new();
        if self.from_file {
            let _ = writeln!(
                out,
                "file: {} (schema_version {})",
                path.display(),
                self.file_version
            );
        } else {
            let _ = writeln!(
                out,
                "file: {} is absent; the preset applies",
                path.display()
            );
        }
        let _ = writeln!(out, "preset: built in (schema_version {SCHEMA_VERSION})");
        for d in &self.deltas {
            let (file, preset) = (show(d.file.as_ref()), show(d.preset.as_ref()));
            let _ = match d.kind {
                DeltaKind::Preset => writeln!(out, "apply {}: {file} -> {preset}", d.path),
                DeltaKind::Edit => writeln!(out, "edit  {}: {file} (preset: {preset})", d.path),
            };
        }
        let applies = self.deltas.iter().filter(|d| d.kind == DeltaKind::Preset);
        let applies = applies.count();
        let edits = self.deltas.len() - applies;
        if self.file_version < SCHEMA_VERSION {
            let _ = writeln!(
                out,
                "schema_version {} -> {SCHEMA_VERSION}: `tm il config update` migrates the file",
                self.file_version
            );
        }
        if self.deltas.is_empty() {
            out.push_str("no differences from the current preset\n");
        } else {
            let _ = writeln!(
                out,
                "{applies} preset change(s) `tm il config update` applies; {edits} edit(s) it keeps"
            );
        }
        out
    }
}

/// A delta value as compact JSON, or `absent`.
fn show(value: Option<&Value>) -> String {
    value.map_or_else(|| "absent".to_owned(), Value::to_string)
}

/// Compare the schedule at `path` with the current preset; reads only.
///
/// Why: issue #11002 AC2. An edited file must be able to see what a new
/// preset changed, and which of its own values an update would keep.
/// What: [`super::load`] (absent file = the preset), refuse a newer version,
/// then the three-way comparison of the module doc. With one step between the
/// file and the current preset it lists exactly what [`upgrade`] does.
/// Test: `diff_lists_preset_changes_and_edits`, `diff_of_the_current_preset_is_empty`,
/// `a_newer_schema_is_refused_and_the_file_is_unchanged`.
pub fn diff(path: &Path) -> Result<Diff, IlScheduleError> {
    let loaded = super::load(path)?;
    let file_version = loaded.schedule.schema_version.unwrap_or(0);
    refuse_newer(path, file_version)?;
    let file = body(path, &loaded.text)?;
    let mut deltas = Vec::new();
    merge_maps(
        "",
        &preset_body(file_version),
        &file,
        &preset_body(SCHEMA_VERSION),
        &mut deltas,
    );
    Ok(Diff {
        from_file: loaded.from_file,
        file_version,
        deltas,
    })
}

/// Migrate the schedule at `path` to [`SCHEMA_VERSION`], keeping every edit.
///
/// Why: issue #11002 AC3. A preset change must reach an existing install
/// without overwriting what its owner changed.
/// What: under the schedule lock. Absent file: writes [`PRESET`]. Current
/// version: unchanged. Newer: [`IlScheduleError::SchemaTooNew`]. Older: copies
/// the bytes to `<file>.v<from>.bak` and verifies the copy (a failure aborts
/// before any write), runs each step k -> k+1, stamps `schema_version`, checks
/// the result parses as a [`Schedule`], and publishes it atomically.
/// Test: `an_edited_v0_file_survives_an_update_and_takes_the_preset_changes`,
/// `an_update_is_idempotent`, `an_update_of_an_absent_file_writes_the_preset`,
/// `a_failed_backup_aborts_the_update_and_the_file_is_unchanged`.
pub fn upgrade(path: &Path) -> Result<Outcome, IlScheduleError> {
    let write_err = |source| IlScheduleError::Write {
        path: path.to_path_buf(),
        source,
    };
    locked(path, || {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                trusty_common::atomic_file::write_atomic(path, PRESET.as_bytes())
                    .map_err(write_err)?;
                return Ok(Outcome::changed(format!(
                    "wrote the preset (schema_version {SCHEMA_VERSION})"
                )));
            }
            Err(source) => {
                return Err(IlScheduleError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let from = parse::<Schedule>(path, &text)?.schema_version.unwrap_or(0);
        refuse_newer(path, from)?;
        if from == SCHEMA_VERSION {
            return Ok(Outcome::unchanged(format!(
                "unchanged: schema_version {from} is current; `tm il config diff` lists your edits"
            )));
        }
        let backup = backup(path, from, &text)?;
        let mut doc = body(path, &text)?;
        let (mut applied, mut kept) = (0usize, Vec::<String>::new());
        for k in from..SCHEMA_VERSION {
            let mut deltas = Vec::new();
            doc = merge_maps("", &preset_body(k), &doc, &preset_body(k + 1), &mut deltas);
            for d in deltas {
                match d.kind {
                    DeltaKind::Preset => applied += 1,
                    DeltaKind::Edit if !kept.contains(&d.path) => kept.push(d.path),
                    DeltaKind::Edit => {}
                }
            }
        }
        let mut schedule: Schedule =
            serde_json::from_value(Value::Object(doc)).map_err(|source| {
                IlScheduleError::Parse {
                    path: path.to_path_buf(),
                    source,
                }
            })?;
        schedule.schema_version = Some(SCHEMA_VERSION);
        let out = schedule
            .to_text()
            .map_err(|e| write_err(std::io::Error::other(e)))?;
        trusty_common::atomic_file::write_atomic(path, out.as_bytes()).map_err(write_err)?;
        let kept_list = if kept.is_empty() {
            String::new()
        } else {
            format!(": {}", kept.join(", "))
        };
        Ok(Outcome::changed(format!(
            "updated: schema_version {from} -> {SCHEMA_VERSION}; applied {applied} preset \
             change(s); kept {} edit(s){kept_list}; backup at {}",
            kept.len(),
            backup.display()
        )))
    })
}

/// [`IlScheduleError::SchemaTooNew`] when `found` is above [`SCHEMA_VERSION`].
pub(super) fn refuse_newer(path: &Path, found: u64) -> Result<(), IlScheduleError> {
    if found > SCHEMA_VERSION {
        return Err(IlScheduleError::SchemaTooNew {
            path: path.to_path_buf(),
            found,
            supported: SCHEMA_VERSION,
        });
    }
    Ok(())
}

/// Copy `text` to `<path>.v<from>.bak` and read it back; any failure aborts.
fn backup(path: &Path, from: u64, text: &str) -> Result<PathBuf, IlScheduleError> {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".v{from}.bak"));
    let bak = PathBuf::from(name);
    let fail = |source| IlScheduleError::Write {
        path: bak.clone(),
        source,
    };
    trusty_common::atomic_file::write_atomic(&bak, text.as_bytes()).map_err(fail)?;
    if std::fs::read(&bak).map_err(fail)? != text.as_bytes() {
        return Err(fail(std::io::Error::other(
            "the backup does not match the file",
        )));
    }
    Ok(bak)
}

/// `text` parsed as `T`, a failure being [`IlScheduleError::Parse`].
fn parse<T: serde::de::DeserializeOwned>(path: &Path, text: &str) -> Result<T, IlScheduleError> {
    serde_json::from_str(text).map_err(|source| IlScheduleError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

/// The top-level object of `text` without `schema_version`.
fn body(path: &Path, text: &str) -> Result<Map<String, Value>, IlScheduleError> {
    let mut map: Map<String, Value> = parse(path, text)?;
    map.remove("schema_version");
    Ok(map)
}

/// [`body`] of `PRESETS[version]`.
fn preset_body(version: u64) -> Map<String, Value> {
    let text = usize::try_from(version)
        .ok()
        .and_then(|k| PRESETS.get(k))
        .expect("callers pass a version at most SCHEMA_VERSION");
    // #11002: compiled in; `every_preset_version_parses_and_names_its_version` proves it.
    body(Path::new("preset"), text).expect("every embedded preset parses")
}

/// Three-way merge of `file` against `base` (old preset) and `new` (new preset).
///
/// Why: an update must take preset changes and keep edits, key by key.
/// What: per key, file order first, then keys only `new` has. Equal file and
/// new: kept, no delta. All three objects: recurse. Otherwise the file equal
/// to `base` (both may be absent) takes `new`, a [`DeltaKind::Preset`] delta;
/// any other value is kept, a [`DeltaKind::Edit`] delta. Arrays are atoms, so
/// an enrolled `sessions` list is one edit.
/// Test: `merge_takes_preset_changes_and_keeps_edits`.
pub(super) fn merge_maps(
    prefix: &str,
    base: &Map<String, Value>,
    file: &Map<String, Value>,
    new: &Map<String, Value>,
    out: &mut Vec<Delta>,
) -> Map<String, Value> {
    let keys: Vec<&String> = file
        .keys()
        .chain(new.keys().filter(|k| !file.contains_key(*k)))
        .collect();
    let mut merged = Map::new();
    for key in keys {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if let Some(v) = merge_value(&path, base.get(key), file.get(key), new.get(key), out) {
            merged.insert(key.clone(), v);
        }
    }
    merged
}

/// One key of [`merge_maps`]; `None` means the key is absent from the result.
fn merge_value(
    path: &str,
    base: Option<&Value>,
    file: Option<&Value>,
    new: Option<&Value>,
    out: &mut Vec<Delta>,
) -> Option<Value> {
    if file == new {
        return file.cloned();
    }
    if let (Some(Value::Object(b)), Some(Value::Object(f)), Some(Value::Object(n))) =
        (base, file, new)
    {
        return Some(Value::Object(merge_maps(path, b, f, n, out)));
    }
    let kind = if file == base {
        DeltaKind::Preset
    } else {
        DeltaKind::Edit
    };
    out.push(Delta {
        path: path.to_owned(),
        file: file.cloned(),
        preset: new.cloned(),
        kind,
    });
    match kind {
        DeltaKind::Preset => new.cloned(),
        DeltaKind::Edit => file.cloned(),
    }
}

//! Take cargo's record of the `trusty-embedderd-py` launcher over after a
//! prebuilt install (#9974).
//!
//! Why: the prebuilt path writes binaries into `$CARGO_HOME/bin` without
//! cargo. On a host that once ran `cargo install trusty-embedderd-py` 0.1.x,
//! cargo's install metadata kept naming that standalone package as the owner
//! of the `trusty-embedderd-py` binary after trusty-search's prebuilt replaced
//! it with its own launcher. A later `cargo uninstall trusty-embedderd-py`
//! then deleted the launcher trusty-search needs, and a later
//! `cargo install trusty-search --locked` refused over the foreign owner.
//!
//! The record is TAKEN OVER, not cleared. Clearing would leave the three
//! binaries on disk with no owner, and cargo refuses to install over an
//! untracked binary as firmly as over a foreign one ("binary already exists
//! in destination"), so `cargo install trusty-search --locked` would still
//! fail. A trusty-search record owning the binaries lets that install treat
//! them as its own and lets `cargo uninstall trusty-search` remove them.
//!
//! What: [`take_over_launcher`] runs only when the crate just placed the
//! launcher into a `<root>/bin` dir and `<root>/.crates.toml` or
//! `<root>/.crates2.json` records the standalone package as the launcher's
//! owner. In each such file it drops the launcher from the standalone record,
//! drops any older record of the installing crate, and adds one crates.io
//! record for the installing crate owning every placed binary no third package
//! owns. Every other record, and every file without the standalone record, is
//! left as it is. A file is replaced by a same-directory temp file and a
//! rename. A file that does not parse is never rewritten.
//!
//! Test: `download::cargo_record_tests`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::commands::standalone_launcher::{LAUNCHER_BIN, STANDALONE_PACKAGE};

/// Cargo's source id for crates.io, the source every prebuilt is cut from.
const CRATES_IO_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// The two metadata files cargo keeps in an install root, v1 first.
const METADATA_FILES: [&str; 2] = [".crates.toml", ".crates2.json"];

/// A prebuilt install placed the launcher, but cargo's metadata could not be
/// brought in line with it.
///
/// Why (#9974): the binaries work, but the stale record breaks a later,
/// unrelated `cargo uninstall`. This is the only moment the operator can tie
/// the cause to the fix, so the install fails rather than warns: a non-zero
/// exit cannot be scrolled past.
/// What: names the file, the reason, and the manual fix.
/// Test: `cargo_record_tests::malformed_metadata_is_reported_and_left_unchanged`,
/// `cargo_record_tests::error_text_names_the_file_and_the_manual_fix`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct CargoRecordError {
    /// The crate whose prebuilt was installed.
    pub crate_name: String,
    /// The version that was installed.
    pub version: String,
    /// The metadata file that could not be read, parsed or replaced.
    pub file: PathBuf,
    /// Why it could not.
    pub reason: String,
}

impl std::fmt::Display for CargoRecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            crate_name,
            version,
            file,
            reason,
        } = self;
        let file = file.display();
        write!(
            f,
            "{crate_name} {version} was installed, but cargo's install metadata {file} \
             could not be updated: {reason}.\n\
             It may still record the standalone {STANDALONE_PACKAGE} package as the owner \
             of the `{LAUNCHER_BIN}` binary {crate_name} now ships, so \
             `cargo uninstall {STANDALONE_PACKAGE}` would delete that binary.\n\
             Fix: repair {file} (restore it if it is corrupt; make it writable), then rerun \
             `tctl install {crate_name}` or run `cargo install {crate_name} --locked --force`. \
             Do not run `cargo uninstall {STANDALONE_PACKAGE}` before that."
        )
    }
}

impl std::error::Error for CargoRecordError {}

/// `name` as cargo records it on this platform.
fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// Whether package-id key `key` (`<name> <version> (<source>)`) names `name`.
fn names(key: &str, name: &str) -> bool {
    key.split_whitespace().next() == Some(name)
}

/// What one metadata file's records turn into.
struct Edits {
    /// Records to rewrite with these bins; an empty list removes the record.
    set: Vec<(String, Vec<String>)>,
    /// Bins the new record of the installing crate owns, sorted.
    claim: Vec<String>,
}

/// Plan the takeover over one file's `(key, bins)` records.
///
/// What: `None` when no standalone record owns the launcher. Otherwise the
/// standalone records lose the launcher, records of `crate_name` are removed,
/// and `crate_name` claims each placed bin no remaining record owns.
fn plan(records: &[(String, Vec<String>)], crate_name: &str, placed: &[String]) -> Option<Edits> {
    let launcher = exe(LAUNCHER_BIN);
    let standalone = |k: &str, b: &[String]| names(k, STANDALONE_PACKAGE) && b.contains(&launcher);
    if !records.iter().any(|(k, b)| standalone(k, b)) {
        return None;
    }
    let mut set = Vec::new();
    let mut kept: Vec<&String> = Vec::new();
    for (k, b) in records {
        if standalone(k, b) {
            let rest: Vec<String> = b.iter().filter(|x| **x != launcher).cloned().collect();
            set.push((k.clone(), rest));
            kept.extend(b.iter().filter(|x| **x != launcher));
        } else if names(k, crate_name) {
            set.push((k.clone(), Vec::new()));
        } else {
            kept.extend(b);
        }
    }
    let mut claim: Vec<String> = placed
        .iter()
        .filter(|b| !kept.contains(b))
        .cloned()
        .collect();
    claim.sort();
    claim.dedup();
    Some(Edits { set, claim })
}

/// Rewrite `.crates.toml` text; `Ok(None)` when it needs no change.
///
/// What: format-preserving (`toml_edit`), so untouched records keep their
/// exact text. `Err` names what did not parse.
fn take_over_v1(
    text: &str,
    key: &str,
    crate_name: &str,
    placed: &[String],
) -> Result<Option<String>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("not TOML: {e}"))?;
    let Some(item) = doc.get_mut("v1") else {
        return Ok(None);
    };
    let table = item
        .as_table_like_mut()
        .ok_or("its `v1` entry is not a table")?;
    let mut records = Vec::new();
    for (k, v) in table.iter() {
        let bins = v
            .as_array()
            .and_then(|a| a.iter().map(|b| b.as_str().map(str::to_owned)).collect())
            .ok_or_else(|| format!("record `{k}` is not a list of binary names"))?;
        records.push((k.to_owned(), bins));
    }
    let Some(edits) = plan(&records, crate_name, placed) else {
        return Ok(None);
    };
    for (k, bins) in edits.set {
        if bins.is_empty() {
            table.remove(&k);
        } else {
            table.insert(
                &k,
                toml_edit::value(bins.into_iter().collect::<toml_edit::Array>()),
            );
        }
    }
    if !edits.claim.is_empty() {
        let bins: toml_edit::Array = edits.claim.into_iter().collect();
        table.insert(key, toml_edit::value(bins));
    }
    Ok(Some(doc.to_string()))
}

/// Rewrite `.crates2.json` text; `Ok(None)` when it needs no change.
///
/// What: the new record carries the fields cargo itself gives a record it
/// knows only from `.crates.toml` (`target` and `rustc` unknown). `Err`
/// names what did not parse.
fn take_over_v2(
    text: &str,
    key: &str,
    crate_name: &str,
    placed: &[String],
) -> Result<Option<String>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let mut doc: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let Some(installs) = doc.get_mut("installs") else {
        return Ok(None);
    };
    let installs = installs
        .as_object_mut()
        .ok_or("its `installs` entry is not an object")?;
    let mut records = Vec::new();
    for (k, v) in installs.iter() {
        let bins = v
            .get("bins")
            .and_then(Value::as_array)
            .and_then(|a| a.iter().map(|b| b.as_str().map(str::to_owned)).collect())
            .ok_or_else(|| format!("record `{k}` has no list of binary names"))?;
        records.push((k.clone(), bins));
    }
    let Some(edits) = plan(&records, crate_name, placed) else {
        return Ok(None);
    };
    for (k, bins) in edits.set {
        if bins.is_empty() {
            installs.remove(&k);
        } else if let Some(record) = installs.get_mut(&k) {
            record["bins"] = json!(bins);
        }
    }
    if !edits.claim.is_empty() {
        installs.insert(
            key.to_owned(),
            json!({
                "version_req": null, "bins": edits.claim, "features": [],
                "all_features": false, "no_default_features": false,
                "profile": "release", "target": null, "rustc": null
            }),
        );
    }
    serde_json::to_string(&doc)
        .map(Some)
        .map_err(|e| format!("could not serialise it: {e}"))
}

/// Replace `path` with `contents` crash-safely, keeping its permissions.
fn replace_file(path: &Path, contents: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let perms = std::fs::metadata(path)?.permissions();
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(contents.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.as_file().set_permissions(perms)?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// After a prebuilt install, take cargo's launcher record over from a
/// standalone `trusty-embedderd-py` install.
///
/// Why: see the module doc (#9974).
/// What: a no-op unless `crate_name` is not the standalone package, `placed`
/// includes the launcher, and `bin_dir` is a cargo root's `bin` dir. Reads and
/// plans both metadata files before writing either, so a parse failure in one
/// leaves both untouched. A missing file is skipped and never created.
/// Test: `cargo_record_tests::prebuilt_install_takes_the_standalone_launcher_record_over`,
/// `cargo_record_tests::a_root_without_the_standalone_record_is_untouched`,
/// `cargo_record_tests::a_root_without_metadata_files_gets_none`,
/// `cargo_record_tests::malformed_metadata_is_reported_and_left_unchanged`,
/// `cargo_record_tests::real_cargo_accepts_the_takeover` (ignored).
///
/// # Errors
/// [`CargoRecordError`] when a metadata file cannot be read, parsed or
/// replaced.
pub(crate) fn take_over_launcher(
    crate_name: &str,
    version: &str,
    bin_dir: &Path,
    placed: &[PathBuf],
) -> Result<(), CargoRecordError> {
    let placed: Vec<String> = placed
        .iter()
        .filter_map(|p| p.file_name()?.to_str().map(str::to_owned))
        .collect();
    if crate_name == STANDALONE_PACKAGE || !placed.contains(&exe(LAUNCHER_BIN)) {
        return Ok(());
    }
    // Cargo keeps its metadata in the install root, the parent of `<root>/bin`.
    let root = match bin_dir.parent() {
        Some(root) if bin_dir.file_name().is_some_and(|n| n == "bin") => root,
        _ => return Ok(()),
    };
    let fail = |file: &Path, reason: String| CargoRecordError {
        crate_name: crate_name.to_owned(),
        version: version.to_owned(),
        file: file.to_owned(),
        reason,
    };
    let key = format!("{crate_name} {version} ({CRATES_IO_SOURCE})");
    let mut writes = Vec::new();
    for name in METADATA_FILES {
        let path = root.join(name);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(fail(&path, format!("could not read it: {e}"))),
        };
        let rewrite = if name == METADATA_FILES[0] {
            take_over_v1
        } else {
            take_over_v2
        };
        if let Some(new) = rewrite(&text, &key, crate_name, &placed).map_err(|r| fail(&path, r))? {
            writes.push((path, new));
        }
    }
    // `.crates.toml` is written first: cargo rebuilds `.crates2.json`'s
    // ownership from it, so a crash between the two writes still shows cargo
    // the takeover.
    for (path, new) in writes {
        replace_file(&path, &new).map_err(|e| fail(&path, format!("could not replace it: {e}")))?;
        tracing::info!(file = %path.display(), crate_name, "took the {LAUNCHER_BIN} record over from standalone {STANDALONE_PACKAGE}");
    }
    Ok(())
}

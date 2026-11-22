//! Read compatibility with the pre-rename locations, and the one-time move
//! out of them (#11632, MB1516 §11).
//!
//! Why: the crate was `trusty-agents` until #11632. Three names are baked
//! into existing machines: the `TAGENT_ASSISTANTS_DIR` variable, the dotless
//! homes root `~/trusty-agents/<id>/`, and the dotted app directory
//! `~/.trusty-agents`. MB1516 §11 keeps all three readable for one release
//! and moves the two directories to their new names once.
//! What:
//! - [`resolve_assistants_root`] and [`app_dir`] pick the new directory when
//!   it exists, else the old one when it exists, else the new one. A library
//!   caller that never runs the move still finds existing data.
//! - [`migrate_legacy_locations`] moves `~/.trusty-agents` to
//!   `~/.trusty-assistants` and each `~/trusty-agents/<id>/` to
//!   `~/trusty-assistants/<id>/`. It never overwrites a destination that
//!   exists, and leaves a symlink at the old path so absolute paths recorded
//!   elsewhere (a trusty-search index root, a log path in another crate)
//!   still resolve. A home's knowledge state records its OKG root as an
//!   absolute path, so the move rewrites that one field.
//! - Every step that fails undoes the steps before it and returns
//!   [`AssistantError::Relocate`]; a failed entry is left exactly as found.
//!
//! The variable `TAGENT_ASSISTANTS_DIR` names a directory the operator chose,
//! so it is read as-is and never moved; only its name is deprecated.
//! Test: `super::tests::relocate_tests` — the whole module.

use std::path::{Path, PathBuf};

use super::error::AssistantError;

/// The pre-rename name of [`super::ASSISTANTS_DIR_ENV`]. Read for one release.
/// Test: `super::tests::relocate_tests::new_variable_wins_and_the_old_one_is_still_read`.
pub const LEGACY_ASSISTANTS_DIR_ENV: &str = "TAGENT_ASSISTANTS_DIR";

/// The pre-rename dotless homes root under the user's home.
/// Test: `super::tests::relocate_tests::legacy_homes_root_is_read_until_the_new_one_exists`.
pub const LEGACY_ASSISTANTS_DIR_NAME: &str = "trusty-agents";

/// The app-private directory under the user's home (config, state, logs).
/// Test: `super::tests::relocate_tests::legacy_app_dir_is_read_until_the_new_one_exists`.
pub const APP_DIR_NAME: &str = ".trusty-assistants";

/// The pre-rename name of [`APP_DIR_NAME`]. Read for one release.
/// Test: `super::tests::relocate_tests::legacy_app_dir_is_read_until_the_new_one_exists`.
pub const LEGACY_APP_DIR_NAME: &str = ".trusty-agents";

/// Where a home keeps the knowledge state that records an absolute OKG root.
const KNOWLEDGE_STATE: &str = "stores/knowledge-pipeline/state.json";

/// The homes root under `home`, preferring the new name.
///
/// Why: read compatibility for a process that has not run the move.
/// What: `<home>/trusty-assistants` when it is a directory, else
/// `<home>/trusty-agents` when that is a real directory, else the new path.
/// Test: `super::tests::relocate_tests::legacy_homes_root_is_read_until_the_new_one_exists`.
pub fn resolve_assistants_root(home: &Path) -> PathBuf {
    prefer_new(
        home.join(super::ASSISTANTS_DIR_NAME),
        home.join(LEGACY_ASSISTANTS_DIR_NAME),
    )
}

/// The app-private directory under `home`, preferring the new name.
///
/// Why: config, provider policy, the user profile and per-user state all
/// lived under `~/.trusty-agents`; one resolver keeps them in one place.
/// What: `<home>/.trusty-assistants` when it is a directory, else
/// `<home>/.trusty-agents` when that is a real directory, else the new path.
/// Test: `super::tests::relocate_tests::legacy_app_dir_is_read_until_the_new_one_exists`.
pub fn app_dir(home: &Path) -> PathBuf {
    prefer_new(home.join(APP_DIR_NAME), home.join(LEGACY_APP_DIR_NAME))
}

fn prefer_new(new: PathBuf, legacy: PathBuf) -> PathBuf {
    if new.is_dir() || !is_real_dir(&legacy) {
        new
    } else {
        legacy
    }
}

/// A directory that is not a symlink. A symlink at a legacy path is the
/// pointer this module left behind, so it never counts as unmoved data.
fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// What one [`migrate_legacy_locations`] call did.
///
/// Why: the caller prints each move and each refusal to stderr, so a user
/// sees that their data changed place, or why it did not.
/// Test: `super::tests::relocate_tests::moves_each_pilot_home_once`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RelocationReport {
    /// `(from, to)` for each directory moved by this call.
    pub moved: Vec<(PathBuf, PathBuf)>,
    /// `(legacy, new)` for each directory left in place because the new
    /// location already existed.
    pub kept_both: Vec<(PathBuf, PathBuf)>,
}

impl RelocationReport {
    /// One human-readable line per entry, moves first.
    /// Test: `super::tests::relocate_tests::both_homes_present_keeps_both_untouched`.
    pub fn lines(&self) -> Vec<String> {
        let moved = self.moved.iter().map(|(from, to)| {
            format!(
                "moved {} to {} (#11632); the old path is now a symlink",
                from.display(),
                to.display()
            )
        });
        let kept = self.kept_both.iter().map(|(legacy, new)| {
            format!(
                "both {} and {} exist; using the new one and leaving the old one untouched \
                 — merge or remove it by hand",
                legacy.display(),
                new.display()
            )
        });
        moved.chain(kept).collect()
    }
}

/// Move the pre-rename directories under `home` to their new names, once.
///
/// Why: #11632 — the second release drops the read compatibility above, so
/// the data has to be under the new names by then. Running the move here,
/// before a command reads state, means a user never edits a home that is
/// about to move.
/// What: moves `<home>/.trusty-agents` to `<home>/.trusty-assistants`, then,
/// unless an assistants-root variable is set, each real directory in
/// `<home>/trusty-agents` to `<home>/trusty-assistants/<name>`. A
/// destination that exists is never overwritten: the pair goes in
/// [`RelocationReport::kept_both`] and both stay as they are. Each move
/// renames the directory, rewrites a home's recorded OKG root, and leaves a
/// symlink at the old path; a step that fails undoes the earlier steps of
/// that entry and returns the error. Entries moved before the failure stay
/// moved. A second call finds only symlinks and does nothing.
/// Test: `super::tests::relocate_tests::moves_each_pilot_home_once`,
/// `super::tests::relocate_tests::both_homes_present_keeps_both_untouched`,
/// `super::tests::relocate_tests::failed_move_leaves_the_legacy_home_in_place`,
/// `super::tests::relocate_tests::unreadable_knowledge_state_refuses_the_move`.
pub fn migrate_legacy_locations(home: &Path) -> Result<RelocationReport, AssistantError> {
    let mut report = RelocationReport::default();
    move_one(
        &home.join(LEGACY_APP_DIR_NAME),
        &home.join(APP_DIR_NAME),
        &mut report,
    )?;
    if root_override_is_set() {
        return Ok(report);
    }
    let legacy_root = home.join(LEGACY_ASSISTANTS_DIR_NAME);
    if !is_real_dir(&legacy_root) {
        return Ok(report);
    }
    let new_root = home.join(super::ASSISTANTS_DIR_NAME);
    std::fs::create_dir_all(&new_root).map_err(|e| relocate_error(&legacy_root, &new_root, e))?;
    let mut entries = std::fs::read_dir(&legacy_root)
        .map_err(|e| relocate_error(&legacy_root, &new_root, e))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_real_dir(path))
        .collect::<Vec<_>>();
    entries.sort();
    for from in entries {
        let Some(name) = from.file_name() else {
            continue;
        };
        let to = new_root.join(name);
        move_one(&from, &to, &mut report)?;
    }
    Ok(report)
}

/// Whether either assistants-root variable names a directory, in which case
/// the default homes root is not in use and is left alone.
fn root_override_is_set() -> bool {
    [super::ASSISTANTS_DIR_ENV, LEGACY_ASSISTANTS_DIR_ENV]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|v| !v.is_empty()))
}

/// Move one directory, or record why it stayed. See [`migrate_legacy_locations`].
fn move_one(from: &Path, to: &Path, report: &mut RelocationReport) -> Result<(), AssistantError> {
    if !is_real_dir(from) {
        return Ok(());
    }
    if std::fs::symlink_metadata(to).is_ok() {
        report.kept_both.push((from.to_path_buf(), to.to_path_buf()));
        return Ok(());
    }
    // Read and rewrite the knowledge state BEFORE anything moves, so a state
    // file this code cannot parse refuses the move with nothing changed.
    let state = rebased_state(from, to)?;
    std::fs::rename(from, to).map_err(|e| relocate_error(from, to, e))?;
    if let Some((_, rebased)) = &state
        && let Err(e) = write_state(to, rebased)
    {
        return Err(undo(from, to, None, e));
    }
    if let Err(e) = link_back(to, from) {
        let original = state.as_ref().map(|(original, _)| original.as_slice());
        return Err(undo(from, to, original, e));
    }
    report.moved.push((from.to_path_buf(), to.to_path_buf()));
    Ok(())
}

/// The original and rewritten bytes of the home's knowledge state, or `None`
/// when the home has none.
///
/// What: replaces the `from` prefix of `store.root` with `to`. Every other
/// field is left byte-for-byte as parsed, including `revision`.
fn rebased_state(from: &Path, to: &Path) -> Result<Option<(Vec<u8>, Vec<u8>)>, AssistantError> {
    let path = from.join(KNOWLEDGE_STATE);
    let original = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(relocate_error(from, to, e)),
    };
    let invalid = |reason: String| AssistantError::Relocate {
        from: from.to_path_buf(),
        to: to.to_path_buf(),
        reason: format!("{}: {reason}", path.display()),
    };
    let mut state: serde_json::Value =
        serde_json::from_slice(&original).map_err(|e| invalid(e.to_string()))?;
    let root = state
        .pointer_mut("/store/root")
        .ok_or_else(|| invalid("has no `store.root`".to_string()))?;
    let recorded = PathBuf::from(
        root.as_str()
            .ok_or_else(|| invalid("`store.root` is not a string".to_string()))?,
    );
    // The state may record the canonical spelling of the home (`/private/var`
    // for `/var` on macOS), so try both.
    let canonical = std::fs::canonicalize(from).map_err(|e| relocate_error(from, to, e))?;
    let Some(rest) = recorded
        .strip_prefix(from)
        .or_else(|_| recorded.strip_prefix(&canonical))
        .ok()
        .map(Path::to_path_buf)
    else {
        // A root outside the home is refused by the knowledge store anyway;
        // moving the home does not change what it records.
        return Ok(None);
    };
    *root = serde_json::Value::String(to.join(&rest).to_string_lossy().into_owned());
    let rebased = serde_json::to_vec_pretty(&state).map_err(|e| invalid(e.to_string()))?;
    Ok(Some((original, rebased)))
}

/// Atomically replace the knowledge state of the home now at `home`.
///
/// What: a 0600 temp file in the same directory, synced, then renamed over
/// the state file, so a failure leaves the previous bytes in place.
fn write_state(home: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let path = home.join(KNOWLEDGE_STATE);
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("knowledge state has no parent directory"))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tmp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(&path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(unix)]
fn link_back(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn link_back(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Put a half-done move back: the original state bytes, then the directory.
///
/// Why: the Fail-Open Check for #11632 — a failed move must leave the entry
/// where the read path above still finds it, not half-moved.
/// What: returns [`AssistantError::Relocate`] naming the failure, and the
/// undo failure too when the undo itself fails.
fn undo(
    from: &Path,
    to: &Path,
    original_state: Option<&[u8]>,
    cause: std::io::Error,
) -> AssistantError {
    let restore_state = original_state.map_or(Ok(()), |bytes| write_state(to, bytes));
    let restore_dir = restore_state.and_then(|()| std::fs::rename(to, from));
    let reason = match restore_dir {
        Ok(()) => format!("{cause}; the move was undone"),
        Err(undo) => format!(
            "{cause}; undoing the move also failed ({undo}), so the data is at {}",
            to.display()
        ),
    };
    AssistantError::Relocate {
        from: from.to_path_buf(),
        to: to.to_path_buf(),
        reason,
    }
}

fn relocate_error(from: &Path, to: &Path, source: std::io::Error) -> AssistantError {
    AssistantError::Relocate {
        from: from.to_path_buf(),
        to: to.to_path_buf(),
        reason: source.to_string(),
    }
}

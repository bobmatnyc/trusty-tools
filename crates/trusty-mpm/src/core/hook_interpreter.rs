//! Bare-script hook commands: find them, and add the interpreter (#10263).
//!
//! Why: a hook command that is a bare `.sh` path runs only while the script
//! keeps its exec bit and its shebang resolves (D73). PR1 (#10265) made the
//! seeded commands name their interpreter, but settings files written before it
//! still carry the bare form, and nothing rewrote them.
//! What: [`classify_command`] decides one command string against the fixed
//! [`INTERPRETER_TABLE`]; [`scan_settings_file`] runs it over every
//! `hooks.*[].hooks[].command` and `statusLine.command` in one settings file;
//! [`repair_settings_file`] is the same pass with the rewrite written back
//! (snapshot first, atomic publish, under the settings lock). Every other JSON
//! key is left as parsed. The doctor row and `--fix` steps built on these live
//! in [`crate::core::hook_interpreter_doctor`].
//! Test: `hook_interpreter_tests.rs`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::core::standalone::hooks::backup::{HOOK_SETTINGS_SNAPSHOTS_KEPT, snapshot_then_prune};

/// Script basename → the interpreter `--fix` writes in front of it.
///
/// Why: the only scripts tm can prove it seeded are these two; any other `.sh`
/// may need a different interpreter, so it is reported and left alone.
/// Test: `a_bare_table_script_is_rewritten_with_its_interpreter`.
pub const INTERPRETER_TABLE: &[(&str, &str)] = &[
    ("notify-supervisor.sh", "/bin/bash"),
    ("datg-hook.sh", "/bin/sh"),
];

/// The seeded `Notification` receiver whose bytes the asset row compares.
pub const NOTIFY_SUPERVISOR: &str = "notify-supervisor.sh";

/// The DATG hook, whose first argument is the PM session it filters on.
const DATG_HOOK: &str = "datg-hook.sh";

/// What [`classify_command`] decided about one command string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Not a bare-script command (already has an interpreter, or no `.sh`).
    Clean,
    /// A bare table script: `new` is the interpreter-prefixed command.
    /// `missing_session` is set for a `datg-hook.sh` entry with no argument.
    Rewrite { new: String, missing_session: bool },
    /// A bare-script shape tm will not rewrite; the string says why.
    Refused(String),
}

/// Classify one hook or `statusLine` command.
///
/// Why (#10263): the rewrite must be exact for the two seeded scripts and must
/// never guess for anything else, so every shape it cannot prove safe is a
/// refusal the operator sees rather than a silent skip.
/// What: `Clean` unless the first token ends in `.sh` (or an env assignment
/// precedes a `.sh` token). Refused: env-var prefix, `~` or relative path,
/// shell metacharacters in the path or arguments, a basename outside
/// [`INTERPRETER_TABLE`], a script missing on disk. Otherwise `Rewrite` to
/// `<interp> "<abs>" <args…>`, arguments preserved.
/// Test: `a_bare_table_script_is_rewritten_with_its_interpreter`,
/// `every_unsafe_shape_is_refused`, `interpreter_shaped_commands_are_clean`,
/// `a_datg_hook_without_a_session_is_flagged`.
pub fn classify_command(cmd: &str) -> Verdict {
    let trimmed = cmd.trim();
    if !trimmed.contains(".sh") {
        return Verdict::Clean;
    }
    let (first, rest) = match split_first(trimmed) {
        Ok(split) => split,
        Err(why) => return Verdict::Refused(why),
    };
    if is_env_assignment(&first) {
        return Verdict::Refused(
            "an env-var prefix precedes the script — rewrite it by hand".to_string(),
        );
    }
    if !first.ends_with(".sh") {
        return Verdict::Clean;
    }
    if first.starts_with('~') {
        return Verdict::Refused("a `~` path is not rewritten — spell it absolute".to_string());
    }
    let script = Path::new(&first);
    if !script.is_absolute() {
        return Verdict::Refused("a relative script path is not rewritten".to_string());
    }
    let args: Vec<&str> = rest.split_whitespace().collect();
    if first.chars().any(is_path_metachar) || args.iter().any(|a| !a.chars().all(is_plain_arg)) {
        return Verdict::Refused("the command carries shell metacharacters".to_string());
    }
    let base = script.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let Some(interp) = interpreter_for(base) else {
        return Verdict::Refused(format!(
            "`{base}` is outside the interpreter table (notify-supervisor.sh, datg-hook.sh)"
        ));
    };
    if !script.is_file() {
        return Verdict::Refused("the script is missing on disk".to_string());
    }
    let mut new = format!("{interp} \"{first}\"");
    for arg in &args {
        new.push(' ');
        new.push_str(arg);
    }
    Verdict::Rewrite {
        new,
        missing_session: base == DATG_HOOK && args.is_empty(),
    }
}

/// The interpreter [`INTERPRETER_TABLE`] maps `basename` to, if any.
fn interpreter_for(basename: &str) -> Option<&'static str> {
    INTERPRETER_TABLE
        .iter()
        .find(|(name, _)| *name == basename)
        .map(|(_, interp)| *interp)
}

/// Split off the first token, honouring one leading double-quoted path.
fn split_first(cmd: &str) -> Result<(String, &str), String> {
    if let Some(after) = cmd.strip_prefix('"') {
        let Some(end) = after.find('"') else {
            return Err("the command has an unbalanced quote".to_string());
        };
        let rest = &after[end + 1..];
        if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
            return Err("the command carries shell metacharacters".to_string());
        }
        return Ok((after[..end].to_string(), rest));
    }
    let end = cmd.find(char::is_whitespace).unwrap_or(cmd.len());
    Ok((cmd[..end].to_string(), &cmd[end..]))
}

/// `NAME=value` — a shell env assignment in command position.
fn is_env_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A character that cannot sit inside the `"<abs>"` the rewrite writes.
fn is_path_metachar(c: char) -> bool {
    matches!(c, '"' | '$' | '`' | '\\' | '\n' | '\r') || (c.is_whitespace() && c != ' ')
}

/// A character an unquoted trailing argument may carry unchanged.
fn is_plain_arg(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':' | '@' | '%' | '+' | ',')
}

/// The absolute `basename` script a command names, in either shape.
///
/// Why: the asset row compares the seeded `notify-supervisor.sh` a hook runs,
/// whether that hook is bare or already interpreter-shaped.
/// Test: `scan_collects_the_notify_script_in_either_shape`.
pub fn referenced_script(cmd: &str, basename: &str) -> Option<PathBuf> {
    cmd.split_whitespace()
        .map(|t| t.trim_matches(|c| c == '"' || c == '\''))
        .map(Path::new)
        .find(|p| p.is_absolute() && p.file_name().and_then(|n| n.to_str()) == Some(basename))
        .map(Path::to_path_buf)
}

/// What one settings file holds, after the rewrite was computed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileFindings {
    /// `(old, new)` for every command the table rewrites.
    pub rewrites: Vec<(String, String)>,
    /// `(command, reason)` for every bare-script shape refused.
    pub refusals: Vec<(String, String)>,
    /// The rewritten `datg-hook.sh` commands that carry no session argument.
    pub missing_session: Vec<String>,
    /// Every absolute `notify-supervisor.sh` a command names.
    pub notify_scripts: Vec<PathBuf>,
}

/// Why a settings file could not be scanned or repaired.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FileError {
    /// The file exists and could not be read.
    #[error("cannot read: {0}")]
    Unreadable(String),
    /// Not JSON, or not a JSON object — never rewritten.
    #[error("not a JSON object settings file ({0}) — never rewritten")]
    Unparseable(String),
    /// The snapshot, lock, or publish failed; the file is unchanged.
    #[error("cannot write: {0}")]
    Unwritable(String),
}

/// Scan one settings file without writing. `Ok(None)` when it does not exist.
///
/// Test: `scan_reports_without_writing`, `an_unparseable_file_is_an_error`.
pub fn scan_settings_file(path: &Path) -> Result<Option<FileFindings>, FileError> {
    if !path.exists() {
        return Ok(None);
    }
    read_and_rewrite(path).map(|(_, findings)| Some(findings))
}

/// Scan one settings file and, when `apply`, write the rewrite back.
///
/// Why (#10263): the apply arm follows the build-tree repoint's write path —
/// snapshot, then atomic publish, under the settings lock — so a backup always
/// precedes the write and a crash leaves the original intact.
/// What: `Ok(None)` for an absent file. Otherwise the findings and, when
/// `apply` wrote anything, the snapshot path. No rewrite: nothing is written,
/// not even a snapshot or a lock sidecar, which makes a second run a no-op.
/// The file is re-read under the lock before the write.
/// Test: `repair_applies_backs_up_and_is_idempotent`,
/// `repair_dry_run_writes_nothing`.
pub fn repair_settings_file(
    path: &Path,
    apply: bool,
) -> Result<Option<(FileFindings, Option<PathBuf>)>, FileError> {
    if !path.exists() {
        return Ok(None);
    }
    // A file with nothing to rewrite is never locked, so no lock sidecar is
    // left beside every settings file the machine-wide sweep reads.
    let (_, preview) = read_and_rewrite(path)?;
    if !apply || preview.rewrites.is_empty() {
        return Ok(Some((preview, None)));
    }
    crate::core::settings_lock::with_settings_lock(path, || {
        let (val, findings) = read_and_rewrite(path)?;
        if findings.rewrites.is_empty() {
            return Ok(Some((findings, None)));
        }
        let backup = snapshot_then_prune(path, HOOK_SETTINGS_SNAPSHOTS_KEPT)
            .map_err(|e| FileError::Unwritable(format!("snapshot: {e}")))?;
        crate::core::settings_lock::publish(path, &val)
            .map_err(|e| FileError::Unwritable(e.to_string()))?;
        Ok(Some((findings, backup)))
    })
    .map_err(|e| FileError::Unwritable(format!("settings lock: {e}")))?
}

/// Read, parse, and compute the rewrite in memory.
fn read_and_rewrite(path: &Path) -> Result<(Value, FileFindings), FileError> {
    let text = std::fs::read_to_string(path).map_err(|e| FileError::Unreadable(e.to_string()))?;
    let mut val: Value =
        serde_json::from_str(&text).map_err(|e| FileError::Unparseable(e.to_string()))?;
    if !val.is_object() {
        return Err(FileError::Unparseable(
            "valid JSON, not an object".to_string(),
        ));
    }
    let mut findings = FileFindings::default();
    for slot in command_slots(&mut val) {
        let Some(cmd) = slot.as_str().map(str::to_string) else {
            continue;
        };
        if let Some(script) = referenced_script(&cmd, NOTIFY_SUPERVISOR) {
            findings.notify_scripts.push(script);
        }
        match classify_command(&cmd) {
            Verdict::Clean => {}
            Verdict::Refused(why) => findings.refusals.push((cmd, why)),
            Verdict::Rewrite {
                new,
                missing_session,
            } => {
                if missing_session {
                    findings.missing_session.push(new.clone());
                }
                *slot = Value::String(new.clone());
                findings.rewrites.push((cmd, new));
            }
        }
    }
    Ok((val, findings))
}

/// Every `hooks.<event>[].hooks[].command` and `statusLine.command` string.
///
/// Why: one walk serves both the scan and the rewrite; a level whose JSON
/// shape is not the expected object/array is skipped, as the build-tree
/// repoint's walker skips it.
fn command_slots(val: &mut Value) -> Vec<&mut Value> {
    let mut out = Vec::new();
    let Some(root) = val.as_object_mut() else {
        return out;
    };
    for (key, v) in root.iter_mut() {
        match key.as_str() {
            "hooks" => hook_command_slots(v, &mut out),
            "statusLine" => {
                if let Some(cmd) = v.get_mut("command").filter(|c| c.is_string()) {
                    out.push(cmd);
                }
            }
            _ => {}
        }
    }
    out
}

/// The `hooks` half of [`command_slots`].
fn hook_command_slots<'a>(hooks: &'a mut Value, out: &mut Vec<&'a mut Value>) {
    let Some(events) = hooks.as_object_mut() else {
        return;
    };
    let groups = events
        .values_mut()
        .filter_map(Value::as_array_mut)
        .flatten();
    let entries = groups
        .filter_map(|g| g.get_mut("hooks").and_then(Value::as_array_mut))
        .flatten();
    for entry in entries {
        if let Some(cmd) = entry.get_mut("command").filter(|c| c.is_string()) {
            out.push(cmd);
        }
    }
}

#[cfg(test)]
#[path = "hook_interpreter_tests.rs"]
mod tests;

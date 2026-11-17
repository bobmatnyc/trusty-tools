//! The #8878 presence-key Keychain floor applied to the BODY of a script a
//! Bash command runs.
//!
//! Why: a write of the key item moved into `bash plant.sh` or `python3
//! plant.py` names nothing the inline floor reads. The #8879 script walk
//! already locates every script a command runs, so this reuses it.
//! What: [`evaluate_script_body_presence_key`] locates each script with
//! `walk::each_script`, reads it with `read_script`, and judges the whole
//! body, in any language, with `evaluate_presence_key_write`; a shell body's
//! own script runs are followed to `MAX_SCRIPT_DEPTH`.
//! Residual: a script the walk cannot locate or read is the #8879 rule's
//! refusal, not this one's; scripts a non-shell body runs are not followed.
//! Test: `a_presence_key_write_in_a_script_body_is_denied_8878`.

use std::path::{Path, PathBuf};

use super::walk::each_script;
use super::{Body, Family, MAX_SCRIPT_DEPTH, family_of, read_script, shebang_interpreter};
use crate::commands::pm_guard_presence_key::evaluate_presence_key_write;
use crate::commands::pm_guard_secret_script_self::{ToplevelRisk, resolve_self_paths_with};

/// The deny for a Bash command that runs a script whose body writes the
/// presence key item, or `None`.
pub(crate) fn evaluate_script_body_presence_key(command: &str, cwd: &Path) -> Option<String> {
    let bases = [cwd.to_path_buf()];
    let mut seen = Vec::new();
    each_script(command, &bases, |word, found, interpreter, seg_bases| {
        let why = judge(&found.ok()?, interpreter, seg_bases, 0, &mut seen)?;
        Some(format!(
            "{why} Found in the script `{word}` this command runs."
        ))
    })
}

/// Judge one script body, then the scripts a shell body runs.
fn judge(
    path: &Path,
    interpreter: Option<&str>,
    bases: &[PathBuf],
    depth: usize,
    seen: &mut Vec<PathBuf>,
) -> Option<String> {
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if seen.contains(&key) || depth >= MAX_SCRIPT_DEPTH {
        return None;
    }
    seen.push(key);
    let Ok(Body::Script(body)) = read_script(path) else {
        return None;
    };
    if let Some(why) = evaluate_presence_key_write(&body) {
        return Some(why);
    }
    let interpreter = interpreter
        .map(str::to_string)
        .unwrap_or_else(|| shebang_interpreter(&body));
    if family_of(&interpreter) != Some(Family::Shell) {
        return None;
    }
    let risk = ToplevelRisk::scan(&body, false, interpreter.contains("zsh"));
    let nested = resolve_self_paths_with(&body, path, risk);
    each_script(&nested, bases, |_, found, inner, inner_bases| {
        judge(&found.ok()?, inner, inner_bases, depth + 1, seen)
    })
}

//! Files that hold a supervised process's ENVIRONMENT: a pm2 dump and a
//! launchd plist (#8523).
//!
//! Why: a research agent told "key names only" printed plaintext credentials
//! with `python3 json.load` on `~/.pm2/dump.pm2` and `cat` on a LaunchAgent
//! plist. Neither name is in the #7266 secret-bearing class: a pm2 dump holds
//! every managed process's full `env`, and a plist's `EnvironmentVariables`
//! dict holds whatever its installer put there.
//! What: [`names_a_process_manager_dump`] adds the pm2 dump to the #7266 NAME
//! class. [`evaluate_env_plist_read`] judges a plist by CONTENT, because most
//! plists hold nothing secret: it refuses a Bash command or a
//! `Read`/`Grep`/`Edit`/`MultiEdit`/`Write` call that names a plist whose
//! `EnvironmentVariables` carries a credential-keyed entry
//! (`trusty_common::launchd_secrets::plist_credential_env_keys`). A plist it
//! cannot read, parse or even locate fails CLOSED when it could hold that
//! dict. `ls`, `stat`, `file`, `test` and `rm` never print bytes and are
//! skipped.
//! Test: `names_a_pm2_dump_and_a_glob_over_its_home`,
//! `refuses_a_plist_whose_environment_carries_a_credential`,
//! `allows_a_plist_with_no_credential_and_a_safe_verb`,
//! `fails_closed_on_a_plist_it_cannot_judge`.

use std::path::{Path, PathBuf};

use trusty_common::launchd_secrets::{is_binary_plist, plist_credential_env_keys};

use crate::commands::hook_rewrite::strip_wrapper_prefix;
use crate::commands::pm_guard_bash::{split_heredoc_bodies, split_shell_segments, tokenize};
use crate::commands::pm_guard_secret_read::{NESTED_COMMAND_MARKERS, command_basename};
use crate::commands::pm_guard_secret_words::is_path_byte;

/// The key whose dict holds a launchd job's environment.
const ENV_KEY: &str = "EnvironmentVariables";

/// Largest plist the guard reads; a larger one fails closed.
const MAX_PLIST_BYTES: u64 = 1 << 20;

/// Programs that report on or delete a file without printing its bytes.
const NON_PRINTING_VERBS: &[&str] = &["ls", "stat", "rm", "test", "[", "file"];

/// Whether `path` names a pm2 process dump, or a glob that can select one.
///
/// Why: `~/.pm2/dump.pm2` (and its `.bak`) is pm2's `save` file — the full
/// `env` of every process it supervises (#8523).
/// What: a basename starting `dump.pm2`, case-insensitively, a wildcard
/// basename directly under a `.pm2` directory, or the `.pm2` directory itself
/// (#8523 critic CRITICAL 2: a content `Grep` over it recurses into the dump).
/// Test: `names_a_pm2_dump_and_a_glob_over_its_home`,
/// `denies_a_grep_over_the_pm2_home_8523`.
pub(crate) fn names_a_process_manager_dump(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let lower = lower.trim_end_matches('/');
    let mut parts = lower.rsplit('/');
    let base = parts.next().unwrap_or_default();
    base == ".pm2"
        || base.starts_with("dump.pm2")
        || (parts.next() == Some(".pm2") && base.contains(['*', '?', '[']))
}

/// Refuse a tool call that would print a credential-bearing launchd plist
/// (#8523): `Some(reason)` denies, `None` allows.
///
/// Why: see the module doc. The plist's CONTENT decides, so relative paths
/// resolve against `cwd`, the hook's working directory.
/// What: collects every candidate path the call names, then asks
/// [`judge_plist`] of each; the first refusal wins. A launchd directory named
/// by a `Grep` or by a printing Bash command is refused.
/// Test: `refuses_a_plist_whose_environment_carries_a_credential`,
/// `fails_closed_on_a_plist_it_cannot_judge`,
/// `refuses_a_bash_content_search_over_a_launchd_directory`.
pub(crate) fn evaluate_env_plist_read(
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
    cwd: &Path,
) -> Option<String> {
    let field = |key: &str| {
        tool_input
            .and_then(|v| v.get(key))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    };
    let (candidates, has_cd) = match tool_name {
        "Bash" => bash_candidates(field("command")?),
        "Read" | "Edit" | "MultiEdit" | "Write" => (vec![field("file_path")?.to_string()], false),
        "Grep" => (vec![field("path")?.to_string()], false),
        _ => return None,
    };
    // #8523 critic CRITICAL 1: a `Grep` prints the lines of every file under a
    // directory, so a directory it names is judged, not waved through.
    // #8523 round 3: so does `grep -r`/`rg`/`find -exec cat` through Bash, and
    // `bash_candidates` already dropped every non-printing verb (`ls`, `stat`,
    // …), so every Bash candidate left is judged the same way.
    let searches_directories = matches!(tool_name, "Grep" | "Bash");
    candidates
        .iter()
        .find_map(|word| judge_plist(word, cwd, has_cd, searches_directories).err())
}

/// Candidate plist words in a Bash command, and whether it changes directory.
fn bash_candidates(command: &str) -> (Vec<String>, bool) {
    let home = dirs::home_dir().map(|h| h.display().to_string());
    let command = match &home {
        Some(h) => command.replace("${HOME}", h).replace("$HOME", h),
        None => command.to_string(),
    };
    let (argv_text, bodies) = split_heredoc_bodies(&command);
    let mut out = Vec::new();
    let mut has_cd = false;
    for segment in split_shell_segments(&argv_text) {
        let lexed = tokenize(&segment).ok();
        let program = lexed.as_ref().and_then(|argv| {
            let start = strip_wrapper_prefix(argv)?;
            argv.get(start).map(|t| command_basename(t))
        });
        has_cd |= matches!(program.as_deref(), Some("cd" | "pushd"));
        let nested = NESTED_COMMAND_MARKERS.iter().any(|m| segment.contains(m));
        if !nested && program.is_some_and(|p| NON_PRINTING_VERBS.contains(&p.as_str())) {
            continue;
        }
        push_candidates(&segment, &mut out);
        lexed
            .iter()
            .flatten()
            .for_each(|t| push_candidates(t, &mut out));
    }
    bodies.iter().for_each(|b| push_candidates(b, &mut out));
    (out, has_cd)
}

/// Append every path word of `text` that could name a launchd plist.
fn push_candidates(text: &str, out: &mut Vec<String>) {
    for word in text.split(|c: char| !is_path_byte(c)) {
        let lower = word.to_ascii_lowercase();
        let in_launchd_dir = lower.contains("launchagents") || lower.contains("launchdaemons");
        if (lower.ends_with(".plist") || in_launchd_dir) && !out.iter().any(|w| w == word) {
            out.push(word.to_string());
        }
    }
}

/// `Ok(())` allows `word`; `Err(reason)` refuses it.
///
/// What: a word naming no existing plist allows, unless it is relative and
/// unverifiable (missing, or the command `cd`s first) or sits in a launchd
/// directory behind a glob — both fail closed. A launchd directory, or one
/// under it, is refused to a content search (`searches_directories`) and
/// allowed otherwise. An existing file is read and handed to
/// [`judge_plist_bytes`].
fn judge_plist(
    word: &str,
    cwd: &Path,
    has_cd: bool,
    searches_directories: bool,
) -> Result<(), String> {
    let lower = word.to_ascii_lowercase();
    let in_launchd_dir = lower.contains("launchagents") || lower.contains("launchdaemons");
    let is_plist = lower.ends_with(".plist");
    if !(is_plist || in_launchd_dir) {
        return Ok(());
    }
    if word.contains(['*', '?', '[', '{']) {
        // #8523: a glob over a launchd directory may select a credential plist.
        return Err(deny_reason(
            word,
            "names plists by a pattern the guard cannot resolve",
        ));
    }
    let (mut path, relative) = resolve(word, cwd);
    if path.is_dir() {
        // #8523 critic CRITICAL 1: a content search over a launchd directory
        // prints every plist under it, as the glob arm above already refuses.
        if searches_directories && in_launchd_dir {
            return Err(deny_reason(
                word,
                "is a launchd directory a content search would print every plist of",
            ));
        }
        return Ok(());
    }
    // #8523: `defaults read ~/Library/LaunchAgents/com.x` reads `com.x.plist`.
    let with_extension = PathBuf::from(format!("{}.plist", path.display()));
    if !is_plist && !path.exists() && with_extension.is_file() {
        path = with_extension;
    }
    if !path.exists() {
        // #8523: a relative plist the hook cannot find may exist where the
        // shell runs; an absolute one that is absent has nothing to print.
        if relative && is_plist {
            return Err(deny_reason(word, "could not be located to check"));
        }
        return Ok(());
    }
    if relative && has_cd {
        return Err(deny_reason(
            word,
            "is relative to a directory the command changes",
        ));
    }
    let bytes = read_bounded(&path).map_err(|why| deny_reason(word, &why))?;
    judge_plist_bytes(&bytes).map_err(|why| deny_reason(word, &why))
}

/// Judge a plist's bytes: `Err(why)` when its environment may hold a credential.
///
/// What: no `EnvironmentVariables` key allows. A binary plist carrying one
/// fails closed (it is not parsed here); an XML one is parsed and refused when
/// any key is credential-shaped, or when the parse fails.
fn judge_plist_bytes(bytes: &[u8]) -> Result<(), String> {
    if !bytes
        .windows(ENV_KEY.len())
        .any(|w| w == ENV_KEY.as_bytes())
    {
        return Ok(());
    }
    if is_binary_plist(bytes) {
        return Err("is a binary plist carrying `EnvironmentVariables`".to_string());
    }
    let xml = std::str::from_utf8(bytes)
        .map_err(|_| "carries `EnvironmentVariables` but is not UTF-8".to_string())?;
    match plist_credential_env_keys(xml) {
        Ok(keys) if keys.is_empty() => Ok(()),
        Ok(keys) => Err(format!(
            "carries credential keys in `EnvironmentVariables` (`{}`)",
            keys.join("`, `")
        )),
        Err(e) => Err(format!("carries `EnvironmentVariables` but {e}")),
    }
}

/// Resolve `~`, absolute and relative spellings; `true` when relative.
fn resolve(word: &str, cwd: &Path) -> (PathBuf, bool) {
    if let Some(rest) = word.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return (home.join(rest), false);
    }
    let path = Path::new(word);
    if path.is_absolute() {
        (path.to_path_buf(), false)
    } else {
        (cwd.join(path), true)
    }
}

/// Read at most [`MAX_PLIST_BYTES`]; a larger, unreadable or non-regular file
/// is an error.
///
/// Why: #8523 critic HIGH 2 — a FIFO or device reports length 0, so a size
/// check alone passed it and the read blocked the PreToolUse hook forever.
/// What: refuses anything whose (symlink-followed) type is not a regular file,
/// opens with `O_NONBLOCK` so a FIFO swapped in afterwards cannot block the
/// open, re-checks the OPENED handle's type, and reads through
/// `take(MAX_PLIST_BYTES + 1)` so no size report is trusted.
/// Test: `refuses_a_non_regular_plist_without_blocking`,
/// `fails_closed_on_a_plist_it_cannot_judge`.
fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let not_regular = || "is not a regular file".to_string();
    let meta = std::fs::metadata(path).map_err(|e| format!("could not be inspected ({e})"))?;
    if !meta.file_type().is_file() {
        return Err(not_regular());
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| format!("could not be opened to check ({e})"))?;
    let opened = file
        .metadata()
        .map_err(|e| format!("could not be inspected ({e})"))?;
    if !opened.file_type().is_file() {
        return Err(not_regular());
    }
    let mut bytes = Vec::new();
    file.take(MAX_PLIST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("could not be read to check ({e})"))?;
    if bytes.len() as u64 > MAX_PLIST_BYTES {
        return Err(format!("is larger than {MAX_PLIST_BYTES} bytes"));
    }
    Ok(bytes)
}

/// The refusal, naming the plist and why it could not be allowed.
fn deny_reason(word: &str, why: &str) -> String {
    format!(
        "naming the launchd plist `{word}` is refused (issue #8523) — it {why}, and printing \
         a plist prints its `EnvironmentVariables` values. `tm doctor` (the `launchd_secrets` \
         row) names a trusty plist's credential KEYS without their values, and \
         `tm doctor --fix-launchd-secrets` moves them into the credential store; for any \
         other plist, ask the operator."
    )
}

#[cfg(test)]
#[path = "pm_guard_secret_env_files_tests.rs"]
mod tests;

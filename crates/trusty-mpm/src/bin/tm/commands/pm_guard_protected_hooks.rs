//! Hook-derived inputs of the #8928 step 7 protected-path check.
//!
//! Why: a hook runs outside the sandbox, so the files a hook command executes
//! and the frontmatter `hooks:` of skill and agent Markdown are ways to plant
//! code that escapes it. Split from `pm_guard_protected_paths.rs` to keep
//! both under the SLOC cap.
//! What: [`hook_named_paths`] lists every path a `hooks` or `statusLine`
//! command runs, from the project and user settings files and from skill and
//! agent frontmatter `hooks:`; [`markdown_hook_verdict`] judges a write to
//! skill or agent Markdown by the frontmatter `hooks` key its result carries.
//! Test: `pm_guard_protected_paths_tests.rs`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::commands::pm_guard_protected_paths::{ProtectedEnv, normalize};
use crate::commands::pm_guard_trust_anchor_paths::{Resolved, eq_fold, resolve};

/// The characters that end a shell word, so `true&&prog` names `prog`
/// (#10107).
const SHELL_OPERATORS: [char; 8] = [';', '&', '|', '(', ')', '<', '>', '\n'];

/// Every path a hook or `statusLine` command runs.
///
/// What: reads `settings.json` and `settings.local.json` under each project
/// root's `.claude/` and in each user config directory, and the frontmatter
/// `hooks:` of skill and agent Markdown there (#8928 fix round). Each command
/// is located by [`command_paths`]. A missing file names nothing;
/// `Err(file)` when one cannot be read or does not parse, and (#10107)
/// `Err(entry)` when a `PATH` entry cannot be placed (fail-closed).
/// Test: `hook_and_status_line_paths_are_listed`,
/// `every_fail_closed_arm_denies`, `relative_hook_paths_resolve_against_the_project`,
/// `frontmatter_hook_commands_are_protected`, `a_bare_hook_program_is_resolved_on_path`,
/// `a_hook_program_cannot_be_shadowed_earlier_on_path`,
/// `an_unresolvable_path_entry_is_an_error`.
pub(crate) fn hook_named_paths(
    env: &ProtectedEnv,
    cwd: Option<&Path>,
) -> Result<Vec<PathBuf>, PathBuf> {
    let roots = env.project_roots(cwd);
    let project_dir = env.project_dir.as_deref().or(cwd);
    let mut files: Vec<(PathBuf, Option<&Path>)> = Vec::new();
    for root in &roots {
        for name in ["settings.json", "settings.local.json"] {
            files.push((root.join(".claude").join(name), Some(root.as_path())));
        }
    }
    for dir in env.user_dirs() {
        for name in ["settings.json", "settings.local.json"] {
            files.push((dir.join(name), project_dir));
        }
    }
    let mut out = Vec::new();
    let paths = |command: &str, project: Option<&Path>| {
        command_paths(command, env.home.as_deref(), project, env.path.as_deref())
    };
    for (file, project) in files {
        let Some(settings) = read_settings(&file)? else {
            continue;
        };
        for command in commands(&settings) {
            out.extend(paths(command, project)?);
        }
    }
    // #8928 fix round: skill and agent frontmatter `hooks:` run commands too.
    let mut bases: Vec<(PathBuf, Option<&Path>)> = roots
        .iter()
        .map(|root| (root.join(".claude"), Some(root.as_path())))
        .collect();
    bases.extend(env.user_dirs().into_iter().map(|dir| (dir, project_dir)));
    for (base, project) in bases {
        for hooks in frontmatter_hook_values(&base)? {
            let wrapped = json!({ "hooks": hooks });
            for command in commands(&wrapped) {
                out.extend(paths(command, project)?);
            }
        }
    }
    Ok(out)
}

/// `file` as a JSON object; `Ok(None)` when absent, `Err(file)` when
/// unreadable or not an object.
fn read_settings(file: &Path) -> Result<Option<Value>, PathBuf> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(file.to_path_buf()),
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(value) if value.is_object() => Ok(Some(value)),
        _ => Err(file.to_path_buf()),
    }
}

/// Every `command` string under `hooks.<event>[].hooks[]`, and `statusLine`.
fn commands(settings: &Value) -> Vec<&str> {
    let mut out = Vec::new();
    if let Some(events) = settings.get("hooks").and_then(Value::as_object) {
        for groups in events.values().filter_map(Value::as_array) {
            for group in groups {
                let hooks = group.get("hooks").and_then(Value::as_array);
                for hook in hooks.into_iter().flatten() {
                    out.extend(hook.get("command").and_then(Value::as_str));
                }
            }
        }
    }
    out.extend(
        settings
            .get("statusLine")
            .and_then(|s| s.get("command"))
            .and_then(Value::as_str),
    );
    out
}

/// The paths one command runs, as spelled and resolved.
///
/// What: the command is split as a shell would, and each word again at the
/// shell operators in [`SHELL_OPERATORS`] (#10107: `true&&prog`). Each word,
/// each operator-split piece, and the value of a `NAME=value` word is located
/// by [`locate`]. `Err(entry)` when a `PATH` entry cannot be placed
/// (fail-closed).
/// Test: `hook_and_status_line_paths_are_listed`,
/// `a_hook_program_cannot_be_shadowed_earlier_on_path`,
/// `an_unresolvable_path_entry_is_an_error`.
fn command_paths(
    command: &str,
    home: Option<&Path>,
    project: Option<&Path>,
    path_env: Option<&OsStr>,
) -> Result<Vec<PathBuf>, PathBuf> {
    let words = shlex::split(command)
        .unwrap_or_else(|| command.split_whitespace().map(str::to_owned).collect());
    let mut out = Vec::new();
    for word in &words {
        // #10107: the whole word too, so a quoted path holding `(` is kept.
        let pieces = std::iter::once(word.as_str()).chain(word.split(SHELL_OPERATORS));
        for piece in pieces.filter(|p| !p.is_empty()) {
            let value = piece.split_once('=').map(|(_, v)| v);
            for spelling in std::iter::once(piece).chain(value) {
                for path in locate(spelling, home, project, path_env)? {
                    let path = normalize(&path);
                    if let Resolved::Path(resolved) = resolve(&path) {
                        out.push(resolved);
                    }
                    out.push(path);
                }
            }
        }
    }
    Ok(out)
}

/// Where one command word may point (see [`command_paths`]).
///
/// Why: #10107 — the program position is lost after `env -i`, `nice -n 10`,
/// `timeout 5` or a newline, so every bare word is a program candidate.
/// What: `~`, `$HOME` and `$CLAUDE_PROJECT_DIR` are expanded and an absolute
/// path is kept; a flag names nothing; a relative path with a `/` joins the
/// project root; a bare word is the project-root file of that name when one
/// exists, plus its [`path_candidates`].
/// Test: `a_hook_program_cannot_be_shadowed_earlier_on_path`,
/// `relative_hook_paths_resolve_against_the_project`.
fn locate(
    word: &str,
    home: Option<&Path>,
    project: Option<&Path>,
    path_env: Option<&OsStr>,
) -> Result<Vec<PathBuf>, PathBuf> {
    if let Some(path) = expand(word, home, project) {
        return Ok(vec![path]);
    }
    if word.is_empty() || word.starts_with('-') {
        return Ok(Vec::new());
    }
    if word.contains('/') {
        return Ok(project.map(|root| root.join(word)).into_iter().collect());
    }
    let mut out: Vec<PathBuf> = project
        .map(|root| root.join(word))
        .filter(|candidate| candidate.is_file())
        .into_iter()
        .collect();
    out.extend(path_candidates(word, project, path_env)?);
    Ok(out)
}

/// `dir/word` for each `PATH` dir up to and including the first one holding
/// an executable `word`, or for every dir when none does.
///
/// Why: #10107 — protecting only the first existing match let a new file in
/// an earlier `PATH` dir shadow the hook program, and a program found nowhere
/// be planted anywhere.
/// What: a relative entry (an empty one included) joins the project root, as
/// the hook runs there. Fail-closed: `Err` for a relative entry with no
/// project root, and for an unset `PATH`, whose search the shell does not
/// define here.
/// Test: `a_hook_program_cannot_be_shadowed_earlier_on_path`,
/// `an_unresolvable_path_entry_is_an_error`.
fn path_candidates(
    word: &str,
    project: Option<&Path>,
    path_env: Option<&OsStr>,
) -> Result<Vec<PathBuf>, PathBuf> {
    let Some(path_env) = path_env else {
        return Err(PathBuf::from("$PATH"));
    };
    let mut out = Vec::new();
    for dir in std::env::split_paths(path_env) {
        let dir = if dir.is_absolute() {
            dir
        } else {
            project.ok_or_else(|| dir.clone())?.join(dir)
        };
        let candidate = dir.join(word);
        let found = is_executable(&candidate);
        out.push(candidate);
        if found {
            break;
        }
    }
    Ok(out)
}

/// Whether `path` is a file with an execute bit, as `PATH` lookup requires.
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The frontmatter `hooks` values of the skill and agent Markdown under
/// `base` (`skills/` and `agents/`, one directory level deep).
///
/// What: a missing directory or file has none; `Err(path)` for one that
/// cannot be read, or frontmatter that mentions `hooks` and does not parse
/// (fail-closed, #8928 fix round).
fn frontmatter_hook_values(base: &Path) -> Result<Vec<Value>, PathBuf> {
    let mut out = Vec::new();
    for kind in ["skills", "agents"] {
        for file in markdown_files(&base.join(kind), 1)? {
            let text = match std::fs::read_to_string(&file) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(file),
            };
            if !text.contains("hooks") {
                continue;
            }
            match frontmatter_hooks(&text) {
                Ok(None) => {}
                Ok(Some(hooks)) => out.push(serde_json::to_value(hooks).map_err(|_| file.clone())?),
                Err(()) => return Err(file),
            }
        }
    }
    Ok(out)
}

/// The `.md` files in `dir`, and in its subdirectories `depth` levels down.
fn markdown_files(dir: &Path, depth: u8) -> Result<Vec<PathBuf>, PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(dir.to_path_buf()),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry.map_err(|_| dir.to_path_buf())?.path();
        if path.is_dir() {
            if depth > 0 {
                out.extend(markdown_files(&path, depth - 1)?);
            }
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| eq_fold(e, "md"))
        {
            out.push(path);
        }
    }
    Ok(out)
}

/// `word` as an absolute path after `~`, `$HOME` and `$CLAUDE_PROJECT_DIR`.
fn expand(word: &str, home: Option<&Path>, project: Option<&Path>) -> Option<PathBuf> {
    let prefixes: [(&str, Option<&Path>); 5] = [
        ("~", home),
        ("$HOME", home),
        ("${HOME}", home),
        ("$CLAUDE_PROJECT_DIR", project),
        ("${CLAUDE_PROJECT_DIR}", project),
    ];
    for (prefix, base) in prefixes {
        if let Some(rest) = word.strip_prefix(prefix)
            && (rest.is_empty() || rest.starts_with('/'))
        {
            return base.map(|b| b.join(rest.trim_start_matches('/')));
        }
    }
    Path::new(word).is_absolute().then(|| PathBuf::from(word))
}

/// The deny reason for a skill or agent Markdown write, or `None`.
///
/// What: the result is the Write's `content`, or the Edit / MultiEdit edits
/// applied to the file as read. Denied when the result's frontmatter carries
/// a `hooks` key that differs from the file's current one (added or changed).
/// Fail-closed: a file that cannot be read, a result that cannot be
/// reproduced, and frontmatter that does not parse but mentions `hooks` deny.
/// Test: `a_markdown_hook_is_judged_by_the_result`,
/// `every_fail_closed_arm_denies`.
pub(crate) fn markdown_hook_verdict(input: Option<&Value>, file: &Path) -> Option<String> {
    let current = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Some(format!("skill or agent Markdown that cannot be read ({e})")),
    };
    let Some(result) = input.and_then(|i| edited(i, &current)) else {
        return Some("skill or agent Markdown whose edit result cannot be reproduced".to_owned());
    };
    let after = match frontmatter_hooks(&result) {
        Ok(None) => return None,
        Ok(Some(hooks)) => hooks,
        Err(()) => {
            return Some(
                "skill or agent Markdown whose frontmatter mentions `hooks` but does not parse"
                    .to_owned(),
            );
        }
    };
    match frontmatter_hooks(&current) {
        Ok(Some(before)) if before == after => None,
        _ => Some("skill or agent Markdown gaining a frontmatter `hooks:` key".to_owned()),
    }
}

/// The text an edit tool would leave, or `None` when it cannot be reproduced.
fn edited(input: &Value, current: &str) -> Option<String> {
    if let Some(content) = input.get("content") {
        return content.as_str().map(str::to_owned);
    }
    let apply = |text: String, edit: &Value| {
        let old = edit.get("old_string")?.as_str()?;
        let new = edit.get("new_string")?.as_str()?;
        if old.is_empty() || !text.contains(old) {
            return None;
        }
        let all = edit.get("replace_all").and_then(Value::as_bool) == Some(true);
        Some(if all {
            text.replace(old, new)
        } else {
            text.replacen(old, new, 1)
        })
    };
    match input.get("edits").and_then(Value::as_array) {
        Some(edits) => edits.iter().try_fold(current.to_owned(), &apply),
        None => apply(current.to_owned(), input),
    }
}

/// The frontmatter `hooks` value of `text`: `Ok(None)` with no frontmatter or
/// no key; `Err(())` when the block does not parse and mentions `hooks`.
pub(crate) fn frontmatter_hooks(text: &str) -> Result<Option<serde_yaml::Value>, ()> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Ok(None);
    }
    let block: Vec<&str> = lines.take_while(|l| l.trim_end() != "---").collect();
    let block = block.join("\n");
    let mentions = block.to_ascii_lowercase().contains("hooks");
    match serde_yaml::from_str::<serde_yaml::Value>(&block) {
        Ok(serde_yaml::Value::Mapping(map)) => Ok(map.get("hooks").cloned()),
        _ if mentions => Err(()),
        _ => Ok(None),
    }
}

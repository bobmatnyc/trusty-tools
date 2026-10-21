//! What else in a command can change where a `git push` goes (#8878 S5).
//!
//! Why: the push rule probes the repository at hook time, before the command
//! runs. Anything the same command does first — a `-c` value, a git
//! environment variable, a `git remote` / `git config` write, a config-file
//! write, a new `HOME` — can make the probed answer wrong, so it fails closed.
//! Split out of `force_push.rs` to keep that file under the 500-SLOC cap.
//! What: [`git_globals`] reads git's global options; [`git_env_assignment`]
//! and [`git_reconfiguration`] scan the whole command; [`is_builtin`] says
//! whether a subcommand can be an alias.
//! #8878 S5 round 2 (owner D8d): both lists are allowlists — any `git config`
//! write but `user.*`, and any `GIT_*` variable but tracing and the
//! interactive helpers, denies with a push; a config-file write is found by
//! its target's file name, so spelling, quoting, a glob or an expansion
//! cannot hide it.
//! Test: `git_config_before_a_push_fails_closed`,
//! `a_git_environment_or_relocated_repository_push_is_denied`,
//! `a_push_after_a_git_reconfiguration_is_denied`,
//! `a_config_write_outside_the_allowlist_before_a_push_is_denied`,
//! `a_config_file_write_in_any_spelling_is_denied`,
//! `a_git_variable_outside_the_allowlist_with_a_push_is_denied`.

use super::super::floor_d4::{program_positions, segments};
use super::super::shell_write_targets;

/// `git --list-cmds=builtins` (git 2.54): a subcommand git runs itself, which
/// no alias can shadow. Any other subcommand may be an alias for `push`.
const GIT_BUILTINS: &str = "\
     add am annotate apply archive backfill bisect blame branch bugreport bundle cat-file \
     check-attr check-ignore check-mailmap check-ref-format checkout checkout--worker \
     checkout-index cherry cherry-pick clean clone column commit commit-graph commit-tree \
     config count-objects credential credential-cache credential-cache--daemon \
     credential-store describe diagnose diff diff-files diff-index diff-pairs diff-tree \
     difftool fast-export fast-import fetch fetch-pack fmt-merge-msg for-each-ref \
     for-each-repo format-patch fsck fsck-objects fsmonitor--daemon gc get-tar-commit-id grep \
     hash-object help history hook index-pack init init-db interpret-trailers last-modified \
     log ls-files ls-remote ls-tree mailinfo mailsplit maintenance merge merge-base \
     merge-file merge-index merge-ours merge-recursive merge-recursive-ours \
     merge-recursive-theirs merge-subtree merge-tree mktag mktree multi-pack-index mv \
     name-rev notes pack-objects pack-redundant pack-refs patch-id pickaxe prune prune-packed \
     pull push range-diff read-tree rebase receive-pack reflog refs remote remote-ext \
     remote-fd repack replace replay repo rerere reset restore rev-list rev-parse revert rm \
     send-pack shortlog show show-branch show-index show-ref sparse-checkout stage stash \
     status stripspace submodule--helper switch symbolic-ref tag unpack-file unpack-objects \
     update-index update-ref update-server-info upload-archive upload-archive--writer \
     upload-pack var verify-commit verify-pack verify-tag version whatchanged worktree \
     write-tree";

/// Text that names a config file a push reads: a write to one is not parsed.
const CONFIG_FILES: &[&str] = &[".gitconfig", "git/config", ".ssh/config"];

/// The file names a push reads configuration from: `.git/config`, its
/// `config.worktree`, XDG `git/config`, `~/.gitconfig` and `~/.ssh/config`.
const CONFIG_NAMES: &[&str] = &["config", "config.worktree", ".gitconfig"];

/// Directories whose `config` a push reads; a copy into one lands there.
const CONFIG_DIRS: &[&str] = &[".git", "git", ".ssh"];

/// Stands for text the shell builds at run time, which may hold a `/`.
const RUNTIME: char = '\u{1}';

/// `git remote` verbs that add or redirect a remote.
const REMOTE_WRITES: &[&str] = &["add", "set-url", "rename"];

/// `git config` flags that make it a read.
const CONFIG_READ_FLAGS: &[&str] = &[
    "--get",
    "--get-all",
    "--get-regexp",
    "--get-urlmatch",
    "--get-color",
    "--get-colorbool",
    "-l",
    "--list",
];

/// `git config` flags that make it a write.
const CONFIG_WRITE_FLAGS: &[&str] = &[
    "--add",
    "--unset",
    "--unset-all",
    "--replace-all",
    "--rename-section",
    "--remove-section",
    "-e",
    "--edit",
];

/// `git config` options whose value is the next word.
const CONFIG_VALUE_OPTS: &[&str] = &[
    "-f",
    "--file",
    "--blob",
    "--type",
    "--default",
    "--comment",
    "--value",
];

/// `GIT_*` variables a push may carry: tracing (`GIT_TRACE*`), the
/// interactive helpers, and the commit identity, none of which moves a push.
const GIT_ENV_ALLOWED: &[&str] = &[
    "GIT_PAGER",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_TERMINAL_PROMPT",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
];

/// Variables that move the global git config.
const CONFIG_HOMES: &[&str] = &["HOME", "XDG_CONFIG_HOME"];

/// Builtins that set a variable named as a bare word.
const SETTERS: &[&str] = &["export", "declare", "typeset", "readonly", "local", "read"];

/// Whether `sub` is a git builtin, which no alias can shadow.
pub(super) fn is_builtin(sub: &str) -> bool {
    GIT_BUILTINS.split_whitespace().any(|b| b == sub)
}

/// The `-c` / `--config-env` values among git's global options, the index of
/// the subcommand (`None` when only options follow `git` at `g`), and whether
/// `--git-dir` or `--work-tree` relocates the repository.
pub(super) fn git_globals(argv: &[String], g: usize) -> (Vec<String>, Option<usize>, bool) {
    let mut configs = Vec::new();
    let mut redirected = false;
    let mut i = g + 1;
    while let Some(tok) = argv.get(i) {
        let tok = tok.as_str();
        match tok {
            "-c" | "--config-env" => {
                configs.push(argv.get(i + 1).cloned().unwrap_or_default());
                i += 2;
            }
            "--git-dir" | "--work-tree" => {
                redirected = true;
                i += 2;
            }
            t if t.starts_with("--git-dir=") || t.starts_with("--work-tree=") => {
                redirected = true;
                i += 1;
            }
            "-C" | "--namespace" => i += 2,
            t if t.starts_with("--config-env=") => {
                configs.push(t["--config-env=".len()..].to_string());
                i += 1;
            }
            t if t.starts_with("-c") => {
                configs.push(t[2..].to_string());
                i += 1;
            }
            t if t.starts_with('-') => i += 1,
            _ => return (configs, Some(i), redirected),
        }
    }
    (configs, None, redirected)
}

/// The words of `command`, cut on whitespace and shell separators, each
/// shlex-unquoted — or, when it does not lex, stripped of quotes.
fn loose_words(command: &str) -> Vec<String> {
    command
        .split(|c: char| c.is_whitespace() || ";&|()`".contains(c))
        .flat_map(|piece| {
            shlex::split(piece).unwrap_or_else(|| vec![piece.replace(['\'', '"', '\\'], "")])
        })
        .collect()
}

/// The leading identifier of `word`.
fn identifier(word: &str) -> &str {
    let end = word
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(word.len());
    &word[..end]
}

/// The first `GIT_*` variable outside [`GIT_ENV_ALLOWED`] and `GIT_TRACE*`
/// named anywhere in `command` — an env prefix, `env`, an `export`, or a `+=`.
///
/// What: over [`loose_words`] (`'GIT_DIR'=x` is `GIT_DIR=x`), a word's name is
/// its leading identifier after any `$`/`{`, so `GIT_DIR+=x`, `export GIT_DIR`
/// and `${GIT_DIR:=x}` all name `GIT_DIR`; over-matching an `echo` is the safe
/// direction.
/// #8878 S5 round 2: an allowlist, not a list of known variables —
/// `GIT_SSH_COMMAND`, `GIT_PROXY_COMMAND` and `GIT_EXEC_PATH` passed the old one.
/// Test: `a_git_environment_or_relocated_repository_push_is_denied`,
/// `a_quoted_or_appended_git_variable_is_denied`,
/// `a_git_variable_outside_the_allowlist_with_a_push_is_denied`.
pub(super) fn git_env_assignment(command: &str) -> Option<String> {
    loose_words(command)
        .iter()
        .map(|tok| identifier(tok.trim_start_matches(['$', '{'])).to_string())
        .find(|name| {
            name.starts_with("GIT_")
                && !name.starts_with("GIT_TRACE")
                && !GIT_ENV_ALLOWED.contains(&name.as_str())
        })
}

/// What in `command` rewrites the git configuration a push reads, or `None`.
///
/// Why: #8878 S5 critic round 1, HIGH 1 — the remote probe runs before the
/// command, so a remote the same command adds, re-points or renames, a
/// `remote.*.url` / `pushurl`, `url.*.insteadOf` / `pushInsteadOf` or
/// `alias.*` it sets, a config file it writes, or a `HOME` /
/// `XDG_CONFIG_HOME` it sets passed the probe and then changed the push.
/// What: a config-file write ([`config_file_write`]); an assignment to a
/// [`CONFIG_HOMES`] variable (`NAME=`, `NAME+=`, `${NAME:=…}`, or the bare name
/// after a [`SETTERS`] word); and, per segment, a `git remote` [`REMOTE_WRITES`]
/// verb or a `git config` write of any key but `user.*` ([`config_write`]).
/// Test: `a_push_after_a_git_reconfiguration_is_denied`,
/// `a_config_write_outside_the_allowlist_before_a_push_is_denied`.
pub(super) fn git_reconfiguration(command: &str) -> Option<String> {
    if let Some(what) = config_file_write(command) {
        return Some(what);
    }
    if let Some(name) = config_home_assignment(command) {
        return Some(format!("`{name}`"));
    }
    segments(command).iter().find_map(|seg| {
        let argv = shlex::split(&seg.text)?;
        program_positions(&argv)
            .into_iter()
            .filter(|(_, program)| program == "git")
            .find_map(|(g, _)| git_config_write(&argv, g))
    })
}

/// The `git remote` / `git config` write the git at `argv[g]` makes.
fn git_config_write(argv: &[String], g: usize) -> Option<String> {
    let (_, sub, _) = git_globals(argv, g);
    let sub = sub?;
    let rest = &argv[sub + 1..];
    match argv[sub].as_str() {
        "remote" => rest
            .iter()
            .find(|w| !w.starts_with('-'))
            .filter(|verb| REMOTE_WRITES.contains(&verb.as_str()))
            .map(|verb| format!("`git remote {verb}`")),
        "config" => config_write(rest)
            .filter(|key| !allowed_config_key(key))
            .map(|key| format!("`git config {key}`")),
        _ => None,
    }
}

/// Whether a `git config` write of `key` leaves every push alone: `user.*`,
/// spelled literally.
fn allowed_config_key(key: &str) -> bool {
    key.to_ascii_lowercase().starts_with("user.") && !key.contains(['$', '`'])
}

/// The key a `git config` with arguments `rest` writes, or `None` for a read.
///
/// Why: #8878 S5 round 2, row 1 — a list of push-redirecting keys missed
/// `branch.<b>.pushRemote`, `remote.pushDefault`, `remote.<r>.push`,
/// `remote.<r>.mirror` and `include.path`, so any write is judged instead.
/// What: a write flag or a `set` / `unset` verb names its key; a section
/// rename or removal and `--edit` name the flag or verb, which no allowlist
/// admits. Otherwise a read flag, a `get` / `list` verb or a lone key is a
/// read (`git config remote.origin.url`), and a key with a value is a write.
/// Test: `a_config_write_outside_the_allowlist_before_a_push_is_denied`.
fn config_write(rest: &[String]) -> Option<String> {
    let (mut flag, mut read, mut words) = (None, false, Vec::new());
    let mut i = 0;
    while let Some(word) = rest.get(i).map(String::as_str) {
        i += 1;
        if word == "--" {
            words.extend(rest[i..].iter().map(String::as_str));
            break;
        }
        if CONFIG_WRITE_FLAGS.contains(&word) {
            flag = Some(word);
        } else if CONFIG_READ_FLAGS.contains(&word) {
            read = true;
        } else if CONFIG_VALUE_OPTS.contains(&word) {
            i += 1;
        } else if !word.starts_with('-') {
            words.push(word);
        }
    }
    let first = words.first().copied();
    match first {
        Some(verb @ ("set" | "unset")) => return Some(words.get(1).unwrap_or(&verb).to_string()),
        Some(verb @ ("rename-section" | "remove-section" | "edit")) => return Some(verb.into()),
        Some("get" | "list") => return None,
        _ => {}
    }
    match flag {
        Some(f @ ("--add" | "--unset" | "--unset-all" | "--replace-all")) => {
            Some(first.unwrap_or(f).to_string())
        }
        Some(f) => Some(f.to_string()),
        None => (!read && words.len() >= 2).then(|| words[0].to_string()),
    }
}

/// The config-file write in `command`, or `None`.
///
/// Why: #8878 S5 round 2, row 2 — a literal substring missed `.git//config`,
/// a quoted or escaped name, a glob, a path built at run time, and the
/// `~/.ssh/config` a `HostName` line redirects a push through.
/// What: any [`CONFIG_FILES`] name in the command once quotes and backslashes
/// are dropped and `//` / `/./` collapsed; then every write target
/// ([`shell_write_targets`]) whose file name may be a [`CONFIG_NAMES`] name
/// ([`may_be_config`]). A write the guard cannot place denies (fail closed).
/// Test: `a_config_file_write_in_any_spelling_is_denied`.
fn config_file_write(command: &str) -> Option<String> {
    let flat = collapse(&command.replace(['\'', '"', '\\'], ""));
    if let Some(file) = CONFIG_FILES.iter().find(|f| flat.contains(**f)) {
        return Some(format!("a write naming `{file}`"));
    }
    let into_config_dir = |path: &str| {
        let path = path.trim_end_matches('/');
        CONFIG_DIRS.contains(&path.rsplit('/').next().unwrap_or(path))
    };
    match shell_write_targets(command, &into_config_dir) {
        Err(why) => Some(format!("a write the guard cannot place ({why})")),
        Ok(targets) => targets
            .into_iter()
            .find(|target| may_be_config(target))
            .map(|target| format!("a write to `{target}`")),
    }
}

/// `path` with every `//` and `/./` collapsed to `/`.
fn collapse(path: &str) -> String {
    let mut out = path.to_string();
    loop {
        let next = out.replace("//", "/").replace("/./", "/");
        if next == out {
            return out;
        }
        out = next;
    }
}

/// Whether the file `target` names may be a [`CONFIG_NAMES`] file.
///
/// What: each expansion (`$X`, `${…}`, `$(…)`, a backtick) becomes
/// [`RUNTIME`], which may hold a `/`. The file name is the text after the last
/// `/` past the last expansion; when an expansion is inside the file name,
/// only the literal text after it is known (`*` before it). That name is
/// matched as a shell glob, so `conf*` and `c?nfig` match `config`.
fn may_be_config(target: &str) -> bool {
    // A redirect target can arrive still quoted (`>> "$D/config"`).
    let path = collapse(&runtime_marked(&target.replace(['\'', '"', '\\'], "")));
    let after = path.rsplit(RUNTIME).next().unwrap_or(&path);
    let name = match after.rsplit_once('/') {
        Some((_, name)) => name.to_string(),
        None if path.contains(RUNTIME) => format!("*{after}"),
        None => after.to_string(),
    };
    CONFIG_NAMES
        .iter()
        .any(|config| glob_matches(name.as_bytes(), config.as_bytes()))
}

/// `word` with each shell expansion replaced by one [`RUNTIME`]; an
/// unterminated one runs to the end of the word.
fn runtime_marked(word: &str) -> String {
    let bytes = word.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let end = match (b, bytes.get(i + 1)) {
            (b'`', _) => closing(bytes, i + 1, b'`', b'`'),
            (b'$', Some(b'(')) => closing(bytes, i + 2, b'(', b')'),
            (b'$', Some(b'{')) => closing(bytes, i + 2, b'{', b'}'),
            (b'$', Some(c)) if c.is_ascii_alphabetic() || *c == b'_' => {
                let tail = &bytes[i + 1..];
                i + 1
                    + tail
                        .iter()
                        .position(|c| !(c.is_ascii_alphanumeric() || *c == b'_'))
                        .unwrap_or(tail.len())
            }
            (b'$', Some(_)) => i + 2,
            (b'$', None) => i + 1,
            _ => {
                let len = word[i..].chars().next().map_or(1, char::len_utf8);
                out.push_str(&word[i..i + len]);
                i += len;
                continue;
            }
        };
        out.push(RUNTIME);
        i = end.min(bytes.len());
    }
    out
}

/// The index just past the `close` that balances an `open` before `from`.
fn closing(bytes: &[u8], from: usize, open: u8, close: u8) -> usize {
    let mut depth = 1;
    for (at, &b) in bytes.iter().enumerate().skip(from) {
        if b == close {
            depth -= 1;
            if depth == 0 {
                return at + 1;
            }
        } else if b == open && open != close {
            depth += 1;
        }
    }
    bytes.len()
}

/// Whether shell glob `pattern` matches `name`; a bracket class matches any
/// one byte, the fail-closed over-match.
fn glob_matches(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|k| glob_matches(rest, &name[k..])),
        Some((b'?', rest)) => !name.is_empty() && glob_matches(rest, &name[1..]),
        Some((b'[', rest)) => match rest.iter().position(|&b| b == b']') {
            Some(close) => !name.is_empty() && glob_matches(&rest[close + 1..], &name[1..]),
            None => name.first() == Some(&b'[') && glob_matches(rest, &name[1..]),
        },
        Some((b, rest)) => name.first() == Some(b) && glob_matches(rest, &name[1..]),
    }
}

/// The first [`CONFIG_HOMES`] variable `command` assigns.
fn config_home_assignment(command: &str) -> Option<String> {
    let words = loose_words(command);
    words.iter().enumerate().find_map(|(i, word)| {
        let (braced, body) = match word.strip_prefix("${") {
            Some(body) => (true, body),
            None if word.starts_with('$') => return None,
            None => (false, word.as_str()),
        };
        let name = identifier(body);
        if !CONFIG_HOMES.contains(&name) {
            return None;
        }
        let after = &body[name.len()..];
        let assigns = if braced {
            after.starts_with(":=") || after.starts_with('=')
        } else {
            after.starts_with('=')
                || after.starts_with("+=")
                || (after.is_empty() && i > 0 && SETTERS.contains(&words[i - 1].as_str()))
        };
        assigns.then(|| name.to_string())
    })
}

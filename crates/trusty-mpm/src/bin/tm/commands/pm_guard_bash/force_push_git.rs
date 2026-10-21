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
//! Test: `git_config_before_a_push_fails_closed`,
//! `a_git_environment_or_relocated_repository_push_is_denied`,
//! `a_push_after_a_git_reconfiguration_is_denied`.

use super::super::floor_d4::{program_positions, segments};

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

/// Text that names a git config file: a write to one is not parsed.
const CONFIG_FILES: &[&str] = &[".gitconfig", "git/config"];

/// `git remote` verbs that add or redirect a remote.
const REMOTE_WRITES: &[&str] = &["add", "set-url", "rename"];

/// `git config` words that make it a read, never a write.
const CONFIG_READS: &[&str] = &["--get", "--get-all", "--get-regexp", "-l", "--list", "get"];

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

/// The first `GIT_CONFIG*`, `GIT_DIR`, `GIT_COMMON_DIR` or `GIT_WORK_TREE`
/// named anywhere in `command` — an env prefix, `env`, an `export`, or a `+=`.
///
/// What: over [`loose_words`] (`'GIT_DIR'=x` is `GIT_DIR=x`), a word's name is
/// its leading identifier after any `$`/`{`, so `GIT_DIR+=x`, `export GIT_DIR`
/// and `${GIT_DIR:=x}` all name `GIT_DIR`; over-matching an `echo` is the safe
/// direction.
/// Test: `a_git_environment_or_relocated_repository_push_is_denied`,
/// `a_quoted_or_appended_git_variable_is_denied`.
pub(super) fn git_env_assignment(command: &str) -> Option<String> {
    loose_words(command)
        .iter()
        .map(|tok| identifier(tok.trim_start_matches(['$', '{'])).to_string())
        .find(|name| {
            name.starts_with("GIT_CONFIG")
                || ["GIT_DIR", "GIT_COMMON_DIR", "GIT_WORK_TREE"].contains(&name.as_str())
        })
}

/// What in `command` rewrites the git configuration a push reads, or `None`.
///
/// Why: #8878 S5 critic round 1, HIGH 1 — the remote probe runs before the
/// command, so a remote the same command adds, re-points or renames, a
/// `remote.*.url` / `pushurl`, `url.*.insteadOf` / `pushInsteadOf` or
/// `alias.*` it sets, a config file it writes, or a `HOME` /
/// `XDG_CONFIG_HOME` it sets passed the probe and then changed the push.
/// What: any text naming [`CONFIG_FILES`]; an assignment to a
/// [`CONFIG_HOMES`] variable (`NAME=`, `NAME+=`, `${NAME:=…}`, or the bare name
/// after a [`SETTERS`] word); and, per segment, a `git remote` [`REMOTE_WRITES`]
/// verb or a `git config` word naming one of the keys above.
/// Test: `a_push_after_a_git_reconfiguration_is_denied`.
pub(super) fn git_reconfiguration(command: &str) -> Option<String> {
    if let Some(file) = CONFIG_FILES.iter().find(|f| command.contains(**f)) {
        return Some(format!("a write naming `{file}`"));
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
        "config" if !rest.iter().any(|w| CONFIG_READS.contains(&w.as_str())) => rest
            .iter()
            .find(|w| rewrites_a_push(w))
            .map(|key| format!("`git config {key}`")),
        _ => None,
    }
}

/// Whether a config key redirects or aliases a push.
fn rewrites_a_push(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    (key.starts_with("remote.") && (key.ends_with(".url") || key.ends_with(".pushurl")))
        || (key.starts_with("url.")
            && (key.ends_with(".insteadof") || key.ends_with(".pushinsteadof")))
        || key.starts_with("alias.")
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

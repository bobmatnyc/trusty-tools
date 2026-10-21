//! The force-push-to-the-default-branch deny of the #8878 D4-remainder floor.
//!
//! Why: the #8878 deny-set (comment 5889247312) names `--force`, `-f`,
//! `--force-with-lease`, a `+ref` refspec and `--mirror` aimed at the default
//! branch; when the default cannot be determined, `main` and `master` are.
//! What: [`force_push_reason`] reads one segment's `git push` argv. `--mirror`
//! always denies. Otherwise every forced destination — every refspec under a
//! force flag, a `+` refspec alone, `--all`/`--branches`, or the current branch
//! when no refspec is given — is compared with the default branch.
//! FAIL-CLOSED: a destination that needs the current branch denies when the
//! branch cannot be read, including after a `cd`; a wildcard destination
//! denies.
//! #8878 follow-on: a push to a URL, a path or an unconfigured name denies
//! whether forced or not, and any `-c`/`--config-env` on a push, or any
//! `-c alias.*`, denies. #8878 PR 2c r2: so does `-c` or a git variable on a
//! subcommand that is not a git builtin, which may be a `push` alias.
//! Residual: aliases from the user's git config, and a push made by a script.
//! Test: `a_force_push_to_the_default_branch_is_denied`,
//! `a_force_push_elsewhere_passes`, `an_unknown_current_branch_denies`.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::floor_d4::{D4_REMEDY, program_positions};

/// The names treated as the default branch when it cannot be determined.
const FALLBACK_DEFAULTS: &[&str] = &["main", "master"];

/// `git push` options whose value is a separate token.
// #8878 follow-on: `--recurse-submodules check` takes its value as a token too.
const PUSH_VALUE_OPTS: &[&str] = &[
    "push-option",
    "repo",
    "receive-pack",
    "exec",
    "recurse-submodules",
];

/// Every `git push` long option (git-push(1)), for unique-prefix resolution.
const PUSH_LONG_OPTS: &[&str] = &[
    "all",
    "branches",
    "prune",
    "mirror",
    "dry-run",
    "porcelain",
    "delete",
    "tags",
    "follow-tags",
    "no-follow-tags",
    "signed",
    "no-signed",
    "atomic",
    "no-atomic",
    "push-option",
    "receive-pack",
    "exec",
    "force-with-lease",
    "no-force-with-lease",
    "force",
    "no-force",
    "force-if-includes",
    "no-force-if-includes",
    "repo",
    "set-upstream",
    "thin",
    "no-thin",
    "quiet",
    "verbose",
    "progress",
    "no-progress",
    "recurse-submodules",
    "no-recurse-submodules",
    "verify",
    "no-verify",
    "ipv4",
    "ipv6",
];

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

/// The `git push` long option `name` names: exact, or its one unique-prefix
/// match. `None` when it matches none or several.
/// Test: `a_force_push_to_the_default_branch_is_denied`.
fn resolve_long(name: &str) -> Option<&'static str> {
    if let Some(exact) = PUSH_LONG_OPTS.iter().find(|o| **o == name) {
        return Some(exact);
    }
    let mut matches = PUSH_LONG_OPTS.iter().filter(|o| o.starts_with(name));
    match (matches.next(), matches.next()) {
        (Some(only), None) if !name.is_empty() => Some(only),
        _ => None,
    }
}

/// The repository facts the rule needs, injectable for tests.
pub(crate) trait GitProbe {
    /// The remote's default branch (`refs/remotes/<remote>/HEAD`), if known.
    fn default_branch(&self, dir: &Path, remote: &str) -> Option<String>;
    /// The checked-out branch in `dir`, if on one.
    fn current_branch(&self, dir: &Path) -> Option<String>;
    /// Whether `name` is a configured remote in `dir`; `None` when unreadable.
    fn has_remote(&self, dir: &Path, name: &str) -> Option<bool>;
}

/// The live repository, read with `git`.
pub(crate) struct LiveGit;

impl LiveGit {
    fn read(dir: &Path, args: &[&str]) -> Option<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .ok()?;
        let text = String::from_utf8(out.stdout).ok()?;
        let text = text.trim();
        (out.status.success() && !text.is_empty()).then(|| text.to_string())
    }
}

impl GitProbe for LiveGit {
    fn default_branch(&self, dir: &Path, remote: &str) -> Option<String> {
        let head = format!("refs/remotes/{remote}/HEAD");
        let full = Self::read(dir, &["symbolic-ref", "--quiet", &head])?;
        full.strip_prefix(&format!("refs/remotes/{remote}/"))
            .map(str::to_string)
    }

    fn current_branch(&self, dir: &Path) -> Option<String> {
        Self::read(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"])
    }

    fn has_remote(&self, dir: &Path, name: &str) -> Option<bool> {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .arg("remote")
            .output()
            .ok()?;
        let text = String::from_utf8(out.stdout).ok()?;
        out.status
            .success()
            .then(|| text.lines().any(|line| line.trim() == name))
    }
}

/// The parsed shape of one `git push`.
#[derive(Default)]
struct Push {
    force: bool,
    /// `--force-with-lease`, tracked apart: `--no-force` does not clear it.
    lease: bool,
    /// An unknown or ambiguous long option, which could be a force.
    unknown: bool,
    /// The `--repo` value, the repository when no operand names one.
    repo: Option<String>,
    mirror: bool,
    all: bool,
    operands: Vec<String>,
}

/// The force-push deny for one segment's argv, or `None`.
///
/// What: finds `git … push` through wrappers; `dir` is the directory the git
/// command runs in (`None` after a `cd`), extended by any `-C` before `push`.
pub(super) fn force_push_reason(
    argv: &[String],
    dir: Option<&Path>,
    git: &dyn GitProbe,
    command: &str,
) -> Option<String> {
    for (g, program) in program_positions(argv) {
        if program != "git" {
            continue;
        }
        // #8878 follow-on: `-c`/`--config-env` can alias or reconfigure a push.
        let (configs, sub, redirected) = git_globals(argv, g);
        let aliased = configs
            .iter()
            .any(|c| c.to_ascii_lowercase().starts_with("alias."));
        // #8878 PR 2c r2: a subcommand git does not build in can be an alias
        // for `push`, which the configuration below can define.
        let may_push = sub.is_some_and(|i| {
            argv[i] == "push" || !GIT_BUILTINS.split_whitespace().any(|b| b == argv[i])
        });
        if aliased || (may_push && !configs.is_empty()) {
            return Some(format!(
                "Hard-floor deny (#8878 D4): this `git` command sets `-c`/`--config-env` \
                 (`{}`), which can alias or reconfigure a push, so the guard cannot tell what \
                 it pushes where. Run the push without it. {D4_REMEDY}",
                configs.join(" ")
            ));
        }
        // #8878 PR 2c: a `GIT_CONFIG_*` / `GIT_DIR` / `GIT_WORK_TREE` in the
        // command reconfigures or relocates the push the same way `-c` does.
        if let Some(name) = git_env_assignment(command).filter(|_| may_push) {
            return Some(format!(
                "Hard-floor deny (#8878 D4): this command sets `{name}`, which can reconfigure \
                 or relocate a `git push`, so the guard cannot tell what it pushes where. Run \
                 the push without it. {D4_REMEDY}"
            ));
        }
        let Some(p) = (g + 1..argv.len()).find(|&i| argv[i] == "push") else {
            continue;
        };
        // #8878 PR 2c: `--git-dir` / `--work-tree` move the repository the
        // remote is read from; the guard does not follow them.
        let dir = dir
            .filter(|_| !redirected)
            .map(|d| git_dir(d, &argv[g + 1..p]));
        let push = parse(&argv[p + 1..]);
        if let Some(remote) = unconfigured_remote(&push, dir.as_deref(), git) {
            return Some(format!(
                "Hard-floor deny (#8878 D4): this `git push` sends to `{remote}`, a URL, a \
                 path or a name that is not a configured remote (or cannot be checked) — \
                 network exfiltration. Push to a configured remote such as `origin`. \
                 {D4_REMEDY}"
            ));
        }
        if let Some(branch) = forced_default(&push, dir.as_deref(), git) {
            return Some(format!(
                "Hard-floor deny (#8878 D4): this `git push` force-updates `{branch}`, the \
                 default branch (or cannot rule it out). Push to a feature branch, or push \
                 without force. {D4_REMEDY}"
            ));
        }
    }
    None
}

/// The `-c` / `--config-env` values among git's global options, the index of
/// the subcommand (`None` when only options follow `git` at `g`), and whether
/// `--git-dir` or `--work-tree` relocates the repository.
fn git_globals(argv: &[String], g: usize) -> (Vec<String>, Option<usize>, bool) {
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

/// The first `GIT_CONFIG*`, `GIT_DIR`, `GIT_COMMON_DIR` or `GIT_WORK_TREE`
/// named anywhere in `command` — an env prefix, `env`, an `export`, or a `+=`.
///
/// What: the command is cut on whitespace and shell separators, and each
/// piece is shlex-unquoted (#8878 PR 2c r2: `'GIT_DIR'=x` is `GIT_DIR=x`) —
/// or, when it does not lex, stripped of quotes. A piece's name is its leading
/// identifier after any `$`/`{`, so `GIT_DIR+=x`, `export GIT_DIR` and
/// `${GIT_DIR:=x}` all name `GIT_DIR`; over-matching an `echo` is the safe
/// direction.
/// Test: `a_git_environment_or_relocated_repository_push_is_denied`,
/// `a_quoted_or_appended_git_variable_is_denied`.
fn git_env_assignment(command: &str) -> Option<String> {
    let pieces = command.split(|c: char| c.is_whitespace() || ";&|()`".contains(c));
    let tokens = pieces.flat_map(|piece| {
        shlex::split(piece).unwrap_or_else(|| vec![piece.replace(['\'', '"', '\\'], "")])
    });
    tokens
        .map(|tok| {
            let tok = tok.trim_start_matches(['$', '{']);
            let end = tok
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(tok.len());
            tok[..end].to_string()
        })
        .find(|name| {
            name.starts_with("GIT_CONFIG")
                || ["GIT_DIR", "GIT_COMMON_DIR", "GIT_WORK_TREE"].contains(&name.as_str())
        })
}

/// The repository a push names when it is not a configured remote: a URL, a
/// path, a bare name the repository does not know, or any name whose remote
/// list cannot be read. `None` for a configured remote or no repository operand.
// #8878 PR 2c: the probe runs before the command, so a repository the command
// itself creates reads as unreadable — only a positive answer allows.
fn unconfigured_remote(push: &Push, dir: Option<&Path>, git: &dyn GitProbe) -> Option<String> {
    let remote = push.operands.first().or(push.repo.as_ref())?;
    let configured =
        is_remote_name(remote) && dir.is_some_and(|d| git.has_remote(d, remote) == Some(true));
    (!configured).then(|| remote.clone())
}

/// `dir` extended by each `-C <path>` among git's global options.
fn git_dir(dir: &Path, globals: &[String]) -> PathBuf {
    let mut out = dir.to_path_buf();
    for pair in globals.windows(2) {
        if pair[0] == "-C" {
            out = out.join(&pair[1]);
        }
    }
    out
}

/// Parse the argv after `push`.
fn parse(tail: &[String]) -> Push {
    let mut push = Push::default();
    let mut i = 0;
    while i < tail.len() {
        let tok = tail[i].as_str();
        i += 1;
        if tok == "--" {
            push.operands.extend(tail[i..].iter().cloned());
            break;
        }
        if let Some(long) = tok.strip_prefix("--") {
            let (name, attached) = long
                .split_once('=')
                .map_or((long, false), |(n, _)| (n, true));
            // #8878 fix round: git takes any unique prefix of a long option.
            let value = long.split_once('=').map(|(_, v)| v.to_string());
            match resolve_long(name) {
                Some("force") => push.force = true,
                // #8878 follow-on: `--no-force` does not clear a lease.
                Some("force-with-lease") => push.lease = true,
                Some("no-force-with-lease") => push.lease = false,
                Some("no-force") => push.force = false,
                Some("repo") => {
                    push.repo = value.or_else(|| tail.get(i).cloned());
                    i += usize::from(!attached);
                }
                Some("mirror") => push.mirror = true,
                Some("all" | "branches") => push.all = true,
                Some(opt) if !attached && PUSH_VALUE_OPTS.contains(&opt) => i += 1,
                Some(_) => {}
                // Unknown or ambiguous: it could be a force — fail closed.
                None => push.unknown = true,
            }
            continue;
        }
        match tok {
            "-o" => i += 1,
            t if t.starts_with('-') && t.len() > 1 => {
                // A short cluster: `-f` anywhere before `-o`, whose value ends it.
                let cluster = &t[1..];
                let before_o = cluster.split('o').next().unwrap_or_default();
                push.force |= before_o.contains('f');
                if cluster.ends_with('o') {
                    i += 1;
                }
            }
            _ => push.operands.push(tok.to_string()),
        }
    }
    push
}

/// The default branch this push force-updates, or `None` when it does not.
fn forced_default(push: &Push, dir: Option<&Path>, git: &dyn GitProbe) -> Option<String> {
    let remote = push.operands.first().map_or("origin", String::as_str);
    let defaults: Vec<String> = dir
        .filter(|_| is_remote_name(remote))
        .and_then(|d| git.default_branch(d, remote))
        .map_or_else(
            || FALLBACK_DEFAULTS.iter().map(|s| s.to_string()).collect(),
            |b| vec![b],
        );
    let first_default = || defaults.first().cloned().unwrap_or_default();
    let forced = push.force || push.lease || push.unknown;
    if push.mirror || (forced && push.all) {
        return Some(first_default());
    }
    let current = || dir.and_then(|d| git.current_branch(d));
    let refspecs = push.operands.get(1..).unwrap_or_default();
    if refspecs.is_empty() {
        if !forced {
            return None;
        }
        // #8878: the implicit destination is the current branch; unknown denies.
        return match current() {
            Some(branch) if !defaults.contains(&branch) => None,
            Some(branch) => Some(branch),
            None => Some(first_default()),
        };
    }
    for spec in refspecs {
        let plus = spec.starts_with('+');
        if !(plus || forced) {
            continue;
        }
        let spec = spec.trim_start_matches('+');
        let dst = spec.rsplit_once(':').map_or(spec, |(_, dst)| dst);
        let dst = dst.strip_prefix("refs/heads/").unwrap_or(dst);
        if dst.contains('*') {
            return Some(first_default());
        }
        let dst = if dst == "HEAD" || dst == "@" || dst.is_empty() {
            match current() {
                Some(branch) => branch,
                None => return Some(first_default()),
            }
        } else {
            dst.to_string()
        };
        if defaults.contains(&dst) {
            return Some(dst);
        }
    }
    None
}

/// Whether a push's first operand is a remote name rather than a URL or path.
fn is_remote_name(remote: &str) -> bool {
    !remote.is_empty() && !remote.contains(['/', ':', '\\']) && !remote.starts_with(['.', '~'])
}

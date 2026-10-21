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
//! #8878 S5 (owner ruling 2026-09-29 14:06Z): a push to a URL, a path or an
//! unconfigured name denies whether forced or not — after a `cd` the remote
//! cannot be checked, so it denies too; any `-c`/`--config-env` on a push, or
//! any `-c alias.*`, denies, as does `-c` or a git variable on a subcommand
//! that is not a git builtin, which may be a `push` alias. A shell redirect is
//! never a remote or the only refspec.
//! #8878 S5 critic round 1: so does a push after the same command rewrites
//! git config or a remote, a push with no remote operand in a repository the
//! guard cannot read, a push `xargs` runs, and `git send-pack`.
//! Residual: aliases from the user's git config, a push made by a script, and
//! a push with no remote operand whose `pushRemote` / `push.default=upstream`
//! sends it to another branch.
//! Test: `a_force_push_to_the_default_branch_is_denied`,
//! `a_force_push_elsewhere_passes`, `an_unknown_current_branch_denies`,
//! `a_push_to_an_unconfigured_remote_is_denied`, `a_redirect_is_not_a_refspec`.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::floor_d4::{D4_REMEDY, program_positions};
use git_config::{git_env_assignment, git_globals, git_reconfiguration, is_builtin};
use parse::{Push, parse};

// #8878 S5 critic round 1: split out to keep this file under the SLOC cap.
#[path = "force_push_git.rs"]
mod git_config;
#[path = "force_push_parse.rs"]
mod parse;

/// The names treated as the default branch when it cannot be determined.
const FALLBACK_DEFAULTS: &[&str] = &["main", "master"];

/// How to push so the guard can check the remote (#8878 S5 critic round 1:
/// a variable or `~` in `-C` is joined unexpanded, so the probe fails).
const PUSH_ADVICE: &str = "Push to a configured remote such as `origin`, from the repository \
     or with `git -C` and a literal absolute path: the guard never follows a `cd`, and does \
     not expand a variable or `~` in `-C`.";

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

/// A D4 deny reason with the shared remedy appended.
fn deny(what: String) -> Option<String> {
    Some(format!("Hard-floor deny (#8878 D4): {what} {D4_REMEDY}"))
}

/// The force-push deny for one segment's argv, or `None`.
///
/// What: finds `git … push` (and `git send-pack`) through wrappers; `dir` is
/// the directory the git command runs in (`None` after a `cd`), extended by
/// any `-C` before `push`. `command` is the whole command, scanned for what
/// else reconfigures git; `appended` says `xargs` may add operands.
/// Test: `a_push_after_a_git_reconfiguration_is_denied`,
/// `an_operand_less_push_in_an_unknown_repository_is_denied`,
/// `a_push_run_by_xargs_is_denied`, `git_send_pack_is_denied`.
pub(super) fn force_push_reason(
    argv: &[String],
    dir: Option<&Path>,
    git: &dyn GitProbe,
    command: &str,
    appended: bool,
) -> Option<String> {
    for (g, program) in program_positions(argv) {
        // #8878 S5 critic round 1: `send-pack` pushes without the word `push`.
        let send_pack = || {
            deny(
                "`git send-pack` pushes refs to any repository, a push the guard cannot \
                  judge. Use `git push` to a configured remote."
                    .to_string(),
            )
        };
        if program == "git-send-pack" {
            return send_pack();
        }
        if program != "git" {
            continue;
        }
        // #8878 S5: `-c`/`--config-env` can alias or reconfigure a push.
        let (configs, sub, redirected) = git_globals(argv, g);
        if sub.is_some_and(|i| argv[i] == "send-pack") {
            return send_pack();
        }
        let aliased = configs
            .iter()
            .any(|c| c.to_ascii_lowercase().starts_with("alias."));
        // #8878 S5: a subcommand git does not build in can be an alias for
        // `push`, which the configuration below can define.
        let may_push = sub.is_some_and(|i| argv[i] == "push" || !is_builtin(&argv[i]));
        if aliased || (may_push && !configs.is_empty()) {
            return deny(format!(
                "this `git` command sets `-c`/`--config-env` (`{}`), which can alias or \
                 reconfigure a push, so the guard cannot tell what it pushes where. Run the \
                 push without it.",
                configs.join(" ")
            ));
        }
        // #8878 S5: a `GIT_CONFIG_*` / `GIT_DIR` / `GIT_WORK_TREE` in the
        // command reconfigures or relocates the push the same way `-c` does.
        if let Some(name) = git_env_assignment(command).filter(|_| may_push) {
            return deny(format!(
                "this command sets `{name}`, which can reconfigure or relocate a `git push`, \
                 so the guard cannot tell what it pushes where. Run the push without it."
            ));
        }
        // #8878 S5 critic round 1: the probe runs before the command, so a
        // remote or config the same command rewrites is unseen.
        if let Some(what) = git_reconfiguration(command).filter(|_| may_push) {
            return deny(format!(
                "this command changes the git configuration a push reads ({what}) before \
                 it pushes, so the guard cannot tell where the push goes. Make the change \
                 and the push in separate commands."
            ));
        }
        let Some(p) = (g + 1..argv.len()).find(|&i| argv[i] == "push") else {
            continue;
        };
        // #8878 S5 critic round 1: `xargs` appends operands the guard never
        // sees — a remote, a refspec, or a `+` refspec.
        if appended {
            return deny(
                "this `git push` runs under `xargs`, which appends remotes or refspecs the \
                 guard cannot see. Name the remote and refspecs in the command."
                    .to_string(),
            );
        }
        // #8878 S5: `--git-dir` / `--work-tree` move the repository the
        // remote is read from; the guard does not follow them.
        let dir = dir
            .filter(|_| !redirected)
            .map(|d| git_dir(d, &argv[g + 1..p]));
        let push = parse(&argv[p + 1..]);
        if let Some(remote) = unconfigured_remote(&push, dir.as_deref(), git) {
            return deny(format!(
                "this `git push` sends to `{remote}`, a URL, a path or a name that is not a \
                 configured remote (or cannot be checked) — network exfiltration. \
                 {PUSH_ADVICE}"
            ));
        }
        // #8878 S5 critic round 1: with no remote operand the repository picks
        // the remote, so it must be one the guard can read.
        let readable = dir
            .as_deref()
            .is_some_and(|d| git.has_remote(d, "origin").is_some());
        if push.remote().is_none() && !readable {
            return deny(format!(
                "this `git push` names no remote, and the guard cannot read the repository \
                 that picks one (after a `cd`, under `--git-dir`/`--work-tree`, or a `-C` \
                 directory that is not a repository yet). {PUSH_ADVICE}"
            ));
        }
        if let Some(branch) = forced_default(&push, dir.as_deref(), git) {
            return deny(format!(
                "this `git push` force-updates `{branch}`, the default branch (or cannot rule \
                 it out). Push to a feature branch, or push without force."
            ));
        }
    }
    None
}

/// The repository a push names when it is not a configured remote: a URL, a
/// path, a bare name the repository does not know, or any name whose remote
/// list cannot be read. `None` for a configured remote or no repository operand.
// #8878 S5: the probe runs before the command, so a repository the command
// itself creates reads as unreadable — only a positive answer allows.
fn unconfigured_remote(push: &Push, dir: Option<&Path>, git: &dyn GitProbe) -> Option<String> {
    let remote = push.remote()?;
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
    if refspecs.is_empty() && forced {
        // #8878: the implicit destination is the current branch; unknown denies.
        match current() {
            Some(branch) if !defaults.contains(&branch) => {}
            Some(branch) => return Some(branch),
            None => return Some(first_default()),
        }
    }
    // #8878 S5: shell words are judged as refspecs too — the fail-closed
    // reading of a quoted refspec that only looks like a redirect or a `)`.
    for spec in refspecs.iter().chain(&push.shell_words) {
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

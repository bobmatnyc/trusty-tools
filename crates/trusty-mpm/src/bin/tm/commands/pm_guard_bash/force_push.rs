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
//! Residual: git aliases, `-c alias.*`, and a push made by a script.
//! Test: `a_force_push_to_the_default_branch_is_denied`,
//! `a_force_push_elsewhere_passes`, `an_unknown_current_branch_denies`.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::floor_d4::{D4_REMEDY, program_positions};

/// The names treated as the default branch when it cannot be determined.
const FALLBACK_DEFAULTS: &[&str] = &["main", "master"];

/// `git push` options whose value is a separate token.
const PUSH_VALUE_OPTS: &[&str] = &["-o", "--push-option", "--repo", "--receive-pack", "--exec"];

/// The repository facts the rule needs, injectable for tests.
pub(crate) trait GitProbe {
    /// The remote's default branch (`refs/remotes/<remote>/HEAD`), if known.
    fn default_branch(&self, dir: &Path, remote: &str) -> Option<String>;
    /// The checked-out branch in `dir`, if on one.
    fn current_branch(&self, dir: &Path) -> Option<String>;
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
}

/// The parsed shape of one `git push`.
#[derive(Default)]
struct Push {
    force: bool,
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
) -> Option<String> {
    for (g, program) in program_positions(argv) {
        if program != "git" {
            continue;
        }
        let Some(p) = (g + 1..argv.len()).find(|&i| argv[i] == "push") else {
            continue;
        };
        let dir = dir.map(|d| git_dir(d, &argv[g + 1..p]));
        let push = parse(&argv[p + 1..]);
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
        match tok {
            "--force" => push.force = true,
            "--no-force" => push.force = false,
            "--mirror" => push.mirror = true,
            "--all" | "--branches" => push.all = true,
            t if t.starts_with("--force-with-lease") => push.force = true,
            t if PUSH_VALUE_OPTS.contains(&t) => i += 1,
            t if t.starts_with("--") => {}
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
    if push.mirror || (push.force && push.all) {
        return Some(first_default());
    }
    let current = || dir.and_then(|d| git.current_branch(d));
    let refspecs = push.operands.get(1..).unwrap_or_default();
    if refspecs.is_empty() {
        if !push.force {
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
        if !(plus || push.force) {
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
    !remote.is_empty() && !remote.contains(['/', ':', '\\']) && !remote.starts_with('.')
}

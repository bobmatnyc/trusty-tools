//! Which directories a command may be standing in at each step of the
//! git-verb walk (#9146).
//!
//! Why: the main-checkout rules tracked a `cd` as if it always ran and always
//! succeeded. Run from a main checkout, `cd <removed-worktree>; git reset
//! --hard` was judged against the worktree, while the shell, its `cd` failed,
//! reset the main checkout. The same held for a `cd` the shell skips
//! (`false && cd <wt>; git commit -a`) and for one that runs in a subshell
//! (`cd <wt> | true`, `cd <wt> &`).
//! What: [`CwdTracker`] reads the [`Step`]s of
//! [`crate::commands::pm_guard_bash::shell_groups::grouped_steps`] and keeps a SET of candidate
//! directories. A `cd` replaces the set only when it is sure to have run and
//! succeeded for the commands that follow: either every operator from the
//! `cd` to them is `&&`, or the `cd` runs unconditionally (first in its
//! `&&`/`||` list) into a directory that exists now. Otherwise the set gains
//! the destination and keeps the directories it had. A `cd` that is a pipeline
//! stage or backgrounded leaves the set alone. The rules judge a git verb
//! against every candidate, so an uncertain `cd` can only add a deny.
//! The `&&` narrowing needs the `cd` to head its and-or list: not the right
//! operand of `||`, not negated by `!`. A `cd` in a brace group opened after an
//! operator is never sure, and the set is capped at [`MAX_CANDIDATES`]; past
//! the cap it is one unresolved stand-in that every rule refuses (#9146).
//! Residual: a `cd` behind `then`/`do` counts as unconditional, so `if false;
//! then cd <existing-wt>; fi; git reset --hard` is still judged against the
//! worktree; `pushd`/`popd` are not tracked.
//! Test: `a_cd_that_may_not_run_or_fail_keeps_the_old_directory_9146`,
//! `a_cd_that_surely_ran_still_moves_the_directory_9146`,
//! `a_cd_in_a_shell_heredoc_body_does_not_leak_9146`,
//! `cd_tracker_candidates_per_operator_9146`, `cd_tracker_bounds_the_candidate_set_9146`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::commands::hook_rewrite::first_command_token;
use crate::commands::pm_guard_bash::shell_groups::{Sep, Step};
use crate::commands::pm_guard_bash::{PathEnv, resolve_target_path};

/// The most directories the tracker keeps as candidates (#9146).
const MAX_CANDIDATES: usize = 8;

/// The one candidate that stands for a set grown past [`MAX_CANDIDATES`].
///
/// An unexpanded `$NAME` component: [`crate::commands::pm_guard_bash::unresolved_target`]
/// refuses any verb judged against it, and the name says why.
const OVERFLOW_DIR: &str = "$TM_PM_GUARD_CD_CANDIDATES_OVERFLOW";

/// A `cd` whose following operator has not been read yet.
struct PendingCd {
    before: Vec<PathBuf>,
    after: Vec<PathBuf>,
    /// Ran unconditionally into directories that exist now.
    sure: bool,
    /// Neither the right operand of `||` nor negated: when `&&` follows, the
    /// commands after it ran only if this `cd` did.
    head: bool,
}

/// The state a child shell restores when it ends.
struct Frame {
    cwds: Vec<PathBuf>,
    broken: Option<Vec<PathBuf>>,
    prev: Option<Sep>,
}

/// The candidate working directories across a walk of [`Step`]s (#9146).
pub(super) struct CwdTracker<'a> {
    env: &'a PathEnv,
    cwds: Vec<PathBuf>,
    /// Where a later command may stand if the `&&` chain after an uncertain
    /// `cd` breaks; merged into `cwds` at the first other operator.
    broken: Option<Vec<PathBuf>>,
    /// The operator before the current command; `None` at a list's start.
    prev: Option<Sep>,
    pending: Option<PendingCd>,
    stack: Vec<Frame>,
    /// A `!` was read for the next command.
    negated: bool,
    /// One entry per open brace group: whether it may not run at all.
    groups: Vec<bool>,
}

impl<'a> CwdTracker<'a> {
    /// A tracker standing in `cwd`.
    pub(super) fn new(cwd: &Path, env: &'a PathEnv) -> Self {
        Self {
            env,
            cwds: vec![cwd.to_path_buf()],
            broken: None,
            prev: None,
            pending: None,
            stack: Vec::new(),
            negated: false,
            groups: Vec::new(),
        }
    }

    /// Every directory the next command may run in; never empty.
    pub(super) fn cwds(&self) -> &[PathBuf] {
        &self.cwds
    }

    /// Read one step. A command other than `cd` comes back for the caller to
    /// judge against [`Self::cwds`]; everything else is absorbed.
    pub(super) fn step(&mut self, step: Step) -> Option<String> {
        match step {
            Step::Enter => {
                self.settle(None);
                self.stack.push(Frame {
                    cwds: self.cwds.clone(),
                    broken: self.broken.take(),
                    prev: self.prev.take(),
                });
                None
            }
            Step::Leave => {
                self.settle(None);
                if let Some(frame) = self.stack.pop() {
                    self.cwds = frame.cwds;
                    self.broken = frame.broken;
                    self.prev = frame.prev;
                }
                None
            }
            Step::Then(sep) => {
                self.settle(Some(sep));
                self.prev = Some(sep);
                None
            }
            // #9146: `! cd <dir> && git …` runs the verb when the `cd` failed.
            Step::Not => {
                self.negated = true;
                None
            }
            // #9146: a group opened after an operator may be skipped whole, so
            // no `cd` inside it is sure to have run.
            Step::OpenGroup => {
                let skippable = self.may_not_run() || self.prev.is_some_and(|s| s != Sep::Seq);
                self.groups.push(skippable);
                None
            }
            Step::CloseGroup => {
                self.groups.pop();
                None
            }
            Step::Command(text) => {
                self.settle(None);
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    return None;
                }
                if first_command_token(trimmed).as_deref() == Some("cd") {
                    self.cd(trimmed);
                    return None;
                }
                self.negated = false;
                Some(text)
            }
        }
    }

    /// Record a `cd`; its effect is decided by the operator after it.
    fn cd(&mut self, segment: &str) {
        // #9146: a pipeline stage runs in a subshell, so its `cd` ends there.
        if self.prev == Some(Sep::Pipe) {
            return;
        }
        let Some(dest) = cd_destination(segment) else {
            return;
        };
        let negated = std::mem::take(&mut self.negated);
        // #9146: an overflowed set stays overflowed; `..` must not cancel it.
        let after = merge(self.cwds.iter().map(|cwd| {
            if is_overflow(cwd) {
                cwd.clone()
            } else {
                resolve_target_path(&dest, cwd, self.env)
            }
        }));
        let unconditional = !self.may_not_run() && !matches!(self.prev, Some(Sep::And | Sep::Or));
        let sure =
            unconditional && !negated && after.iter().all(|dir| !is_overflow(dir) && dir.is_dir());
        self.pending = Some(PendingCd {
            before: self.cwds.clone(),
            after,
            sure,
            head: !negated && self.prev != Some(Sep::Or),
        });
    }

    /// Whether the current command sits in a brace group that may be skipped.
    fn may_not_run(&self) -> bool {
        self.groups.last().copied().unwrap_or(false)
    }

    /// Apply a pending `cd` now that the operator after it is known (`None`
    /// when the walk moved on without one), then end a broken `&&` chain.
    fn settle(&mut self, sep: Option<Sep>) {
        if let Some(cd) = self.pending.take() {
            self.cwds = match sep {
                // #9146: a backgrounded or piped `cd` runs in a subshell.
                Some(Sep::Pipe | Sep::Background) => cd.before,
                _ if cd.sure => cd.after,
                // The commands after `&&` run only once the `cd` succeeded;
                // the first other operator brings the old directory back.
                // #9146: not when `||` or `!` precedes the `cd`: then
                // `true || cd <wt> && git …` skips it and runs the verb.
                Some(Sep::And) if cd.head => {
                    let broken = self.broken.take().unwrap_or_default();
                    self.broken = Some(merge(broken.into_iter().chain(cd.before)));
                    cd.after
                }
                // #9146: it may have been skipped or failed.
                _ => merge(cd.after.into_iter().chain(cd.before)),
            };
        }
        if sep.is_some_and(|sep| sep != Sep::And)
            && let Some(broken) = self.broken.take()
        {
            self.cwds = merge(self.cwds.drain(..).chain(broken));
        }
    }
}

/// The directory operand of a `cd` segment, past its options; `None` for a
/// bare `cd` or a segment `shlex` cannot split.
fn cd_destination(segment: &str) -> Option<String> {
    let argv = shlex::split(segment)?;
    let mut args = argv.into_iter().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--" {
            return args.next();
        }
        // `-P`, `-L`, `-e`, `-@`; a lone `-` is `$OLDPWD`, an operand.
        if arg.len() > 1 && arg.starts_with('-') {
            continue;
        }
        return Some(arg);
    }
    None
}

/// `paths` in order, each once, and no more than [`MAX_CANDIDATES`].
///
/// Why: every uncertain `cd` doubles the set, so a run of `cd a; cd b; …` over
/// directories that do not exist made it 2^n and the hook could hang (#9146).
/// What: past the bound the set is the single [`OVERFLOW_DIR`], which no verb
/// is allowed to run against (fail closed). Linear in `paths`.
fn merge(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut out: Vec<PathBuf> = Vec::new();
    for path in paths {
        if seen.insert(path.clone()) {
            out.push(path);
        }
    }
    if out.len() > MAX_CANDIDATES {
        return vec![PathBuf::from(OVERFLOW_DIR)];
    }
    out
}

/// Whether `path` is the [`OVERFLOW_DIR`] stand-in.
pub(super) fn is_overflow(path: &Path) -> bool {
    path == Path::new(OVERFLOW_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::pm_guard_bash::shell_groups::grouped_steps;

    /// The candidate set each non-`cd` command of `command` is judged against.
    fn candidates(command: &str, cwd: &Path) -> Vec<(String, Vec<PathBuf>)> {
        let env = PathEnv::from_process();
        let mut tracker = CwdTracker::new(cwd, &env);
        let mut out = Vec::new();
        for step in grouped_steps(command).steps {
            if let Some(text) = tracker.step(step) {
                out.push((text, tracker.cwds().to_vec()));
            }
        }
        out
    }

    #[test]
    fn cd_tracker_candidates_per_operator_9146() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().to_path_buf();
        let real = home.join("real");
        std::fs::create_dir(&real).expect("mkdir real");
        let gone = home.join("gone");
        let (r, g) = (real.display(), gone.display());
        let rows: Vec<(String, Vec<&PathBuf>)> = vec![
            // Sure: unconditional into an existing directory, or `&&`.
            (format!("cd {r}; git x"), vec![&real]),
            (format!("cd {g} && git x"), vec![&gone]),
            (format!("cd -P -- {r}; git x"), vec![&real]),
            // Failed, skipped, piped, backgrounded.
            (format!("cd {g}; git x"), vec![&gone, &home]),
            (format!("cd {g} || true; git x"), vec![&gone, &home]),
            (format!("true && cd {r}; git x"), vec![&real, &home]),
            (format!("cd {g} && true; git x"), vec![&gone, &home]),
            (format!("cd {r} | git x"), vec![&home]),
            (format!("true | cd {r}; git x"), vec![&home]),
            (format!("cd {r} & git x"), vec![&home]),
            // #9146: a `cd` behind `||` or `!` can be skipped under `&&` too.
            (format!("true || cd {r} && git x"), vec![&real, &home]),
            (format!("! cd {g} && git x"), vec![&gone, &home]),
            (format!("! cd {r}; git x"), vec![&real, &home]),
            // #9146: a newline after `&&`/`||` continues the list.
            (format!("false &&\ncd {r}; git x"), vec![&real, &home]),
            (format!("false ||\ncd {r}; git x"), vec![&real, &home]),
            (format!("true |\ncd {r}; git x"), vec![&home]),
            // #9146: a group opened under an operator may be skipped whole.
            (
                format!("false && {{ true; cd {r}; }}; git x"),
                vec![&real, &home],
            ),
            (
                format!("false || {{ true; cd {r}; }}; git x"),
                vec![&real, &home],
            ),
            (format!("{{ true; cd {r}; }}; git x"), vec![&real]),
        ];
        for (command, expected) in rows {
            let found = candidates(&command, &home);
            let last = found.last().expect("a command");
            let expected: Vec<PathBuf> = expected.into_iter().cloned().collect();
            assert_eq!(last.1, expected, "{command}");
        }
    }

    /// #9146: a run of uncertain `cd`s collapses to the one overflow candidate
    /// instead of doubling the set each time.
    #[test]
    fn cd_tracker_bounds_the_candidate_set_9146() {
        let home = tempfile::tempdir().expect("tempdir");
        let command = (0..20)
            .map(|i| format!("cd nowhere{i}"))
            .chain(["git x".to_string()])
            .collect::<Vec<_>>()
            .join("; ");
        let found = candidates(&command, home.path());
        let set = &found.last().expect("a command").1;
        assert_eq!(set.len(), 1, "{set:?}");
        assert!(is_overflow(&set[0]), "{set:?}");
        // A `..` after the overflow cannot cancel it.
        let found = candidates(&format!("{command}; cd ..; git y"), home.path());
        assert!(is_overflow(&found.last().expect("a command").1[0]));
    }
}

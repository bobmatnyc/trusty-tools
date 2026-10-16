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
//! Residual: a `cd` behind `then`/`do` counts as unconditional, so `if false;
//! then cd <existing-wt>; fi; git reset --hard` is still judged against the
//! worktree; `pushd`/`popd` are not tracked.
//! Test: `a_cd_that_may_not_run_or_fail_keeps_the_old_directory_9146`,
//! `a_cd_that_surely_ran_still_moves_the_directory_9146`,
//! `a_cd_in_a_shell_heredoc_body_does_not_leak_9146`,
//! `cd_tracker_candidates_per_operator_9146`.

use std::path::{Path, PathBuf};

use crate::commands::hook_rewrite::first_command_token;
use crate::commands::pm_guard_bash::shell_groups::{Sep, Step};
use crate::commands::pm_guard_bash::{PathEnv, resolve_target_path};

/// A `cd` whose following operator has not been read yet.
struct PendingCd {
    before: Vec<PathBuf>,
    after: Vec<PathBuf>,
    /// Ran unconditionally into directories that exist now.
    sure: bool,
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
        let after = dedup(
            self.cwds
                .iter()
                .map(|cwd| resolve_target_path(&dest, cwd, self.env)),
        );
        let unconditional = !matches!(self.prev, Some(Sep::And | Sep::Or));
        let sure = unconditional && after.iter().all(|dir| dir.is_dir());
        self.pending = Some(PendingCd {
            before: self.cwds.clone(),
            after,
            sure,
        });
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
                Some(Sep::And) => {
                    let broken = self.broken.take().unwrap_or_default();
                    self.broken = Some(dedup(broken.into_iter().chain(cd.before)));
                    cd.after
                }
                // #9146: it may have been skipped or failed.
                _ => dedup(cd.after.into_iter().chain(cd.before)),
            };
        }
        if sep.is_some_and(|sep| sep != Sep::And)
            && let Some(broken) = self.broken.take()
        {
            self.cwds = dedup(self.cwds.drain(..).chain(broken));
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

/// `paths` in order, each once.
fn dedup(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for path in paths {
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
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
        ];
        for (command, expected) in rows {
            let found = candidates(&command, &home);
            let last = found.last().expect("a command");
            let expected: Vec<PathBuf> = expected.into_iter().cloned().collect();
            assert_eq!(last.1, expected, "{command}");
        }
    }
}

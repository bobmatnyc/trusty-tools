//! Turning a path token a Bash command wrote into a directory the guard can
//! reason about.
//!
//! Why: every rule in this module tree — the worktree-add denylist, the
//! destructive-delete denylist, the ADR-0037 main-checkout rule, the ADR-0057
//! removal grant — decides on a DIRECTORY, and what the command actually says
//! is a TOKEN: `~/scratch/wt`, `$TMPDIR/wt`, `$PWD/..`, `$MAIN/.claude/…`.
//! Resolving that in one place is what keeps the rules agreeing about where a
//! command stands; a second copy would drift on `~` expansion, on `..`
//! collapsing, or on which variables the guard can expand at all. Split out of
//! `mod.rs` when #7098/#7100 added the "what did NOT expand" half and pushed
//! that file over the 500-SLOC cap.
//!
//! What: [`PathEnv`] carries the three environment values as data rather than
//! reading `std::env` per use, [`resolve_target_path`] expands and normalizes a
//! token against a base directory, and [`unresolved_target`] reports what the
//! expansion could not reach — the one question a rule must ask before stating
//! anything about the directory it got back.
//!
//! Test: `evaluate_worktree_add_command_expands_tmpdir_and_home`,
//! `unexpanded_shell_variable_finds_both_spellings`,
//! `unexpanded_shell_variable_is_none_for_ordinary_paths`,
//! `unresolved_target_reports_a_surviving_tilde_as_written`,
//! `unresolved_target_is_none_once_home_expands_the_tilde` in the sibling
//! `tests` module.

use std::path::{Path, PathBuf};

/// The environment values [`resolve_target_path`] expands `$TMPDIR`/`$TMP`/`~`
/// from, captured as data instead of read from `std::env` at each use.
///
/// Why: the expansion rules can only be tested by controlling those variables,
/// and the obvious way to do that — `std::env::set_var` in the test — mutates
/// PROCESS-GLOBAL state that every other test in the `tm` test binary sees for
/// as long as it is set. `cargo test` runs tests as threads in one process, so
/// a restore-on-drop guard bounds the leak's lifetime but not its visibility:
/// concurrent siblings still read the mutated value. That is not hypothetical —
/// a `TMPDIR` pinned to a macOS-only scratch path reddened five
/// `pm_guard_budget` tests on a Linux CI runner (PR #4914, run 31023632348)
/// because `tempfile` honors `$TMPDIR` and the path does not exist there.
/// Passing the values in removes the global mutation rather than scheduling
/// around it.
/// What: the three variables [`resolve_target_path`] expands, each `None` when
/// unset. [`PathEnv::from_process`] is the one place that reads the real
/// environment, so production behavior is unchanged — a `Bash` tool call
/// inherits the guard process's environment, and the guard expands against it.
/// Test: `evaluate_worktree_add_command_expands_tmpdir_and_home` builds one
/// directly; every other caller goes through [`PathEnv::from_process`].
pub(crate) struct PathEnv {
    pub(crate) tmpdir: Option<String>,
    pub(crate) tmp: Option<String>,
    pub(crate) home: Option<String>,
}

impl PathEnv {
    /// Read `$TMPDIR`, `$TMP`, and `$HOME` from the guard process.
    pub(crate) fn from_process() -> Self {
        Self {
            tmpdir: std::env::var("TMPDIR").ok(),
            tmp: std::env::var("TMP").ok(),
            home: std::env::var("HOME").ok(),
        }
    }
}

/// Expand a leading `~`, `$TMPDIR`/`${TMPDIR}`, `$TMP`/`${TMP}`,
/// `$HOME`/`${HOME}`, and `$PWD`/`${PWD}` in a path token using `env`/`base`,
/// then resolve it against `base` if still relative, then lexically
/// normalize (collapse `.`/`..` components WITHOUT touching the filesystem).
///
/// Why: `shlex::split` does not perform shell variable expansion, so a target
/// argument like `$TMPDIR/wt-foo`, `~/scratch/wt-foo`, a literal `$HOME`
/// (issue #4031 — an agent typing `rm -rf $HOME` verbatim), or `$PWD` (issue
/// #4031 review — `rm -rf $PWD` from inside a session's own worktree root
/// reached this function unexpanded, so the guard saw a literal `"$PWD"`
/// token that matched nothing) reaches this function as literal text; the
/// guard must expand it itself to see where it really points. `$PWD` expands
/// to `base` — the SAME cwd this function's own `.`/`..` resolution already
/// treats as "here" — rather than a second read of the process environment's
/// `PWD`, so a command composed with a preceding `cd` (which updates `base`
/// via the caller's tracking, never the process env) still resolves `$PWD`
/// against where the command actually stands. Filesystem-touching resolution
/// (`fs::canonicalize`) is deliberately avoided — the worktree target usually
/// does not exist yet, and a `PreToolUse` hook must stay fast and
/// side-effect-free.
/// What: string-replaces the env-var forms (`$PWD`/`${PWD}` against `base`,
/// the rest against `env`), expands a `~`/`~/…` prefix via `$HOME`, joins onto
/// `base` if the result is still relative, then normalizes. The env values
/// arrive via [`PathEnv`] rather than being read here, so a test can pin them
/// without mutating process-global state — see [`PathEnv`] for the CI failure
/// that motivated the seam. In production [`PathEnv::from_process`] supplies
/// the guard process's own environment, which a `Bash` tool call inherits
/// unchanged. Anything it could NOT expand survives as a literal path
/// component; [`unresolved_target`] is how a caller finds out.
pub(crate) fn resolve_target_path(token: &str, base: &Path, env: &PathEnv) -> PathBuf {
    let mut expanded = token.to_string();
    if let Some(tmpdir) = env.tmpdir.as_deref() {
        expanded = expanded
            .replace("${TMPDIR}", tmpdir)
            .replace("$TMPDIR", tmpdir);
    }
    if let Some(tmp) = env.tmp.as_deref() {
        expanded = expanded.replace("${TMP}", tmp).replace("$TMP", tmp);
    }
    if let Some(home) = env.home.as_deref() {
        expanded = expanded.replace("${HOME}", home).replace("$HOME", home);
    }
    let pwd = base.to_string_lossy();
    expanded = expanded.replace("${PWD}", &pwd).replace("$PWD", &pwd);
    if expanded == "~" {
        if let Some(home) = env.home.as_deref() {
            expanded = home.to_string();
        }
    } else if let Some(rest) = expanded.strip_prefix("~/")
        && let Some(home) = env.home.as_deref()
    {
        expanded = format!("{home}/{rest}");
    }
    let path = Path::new(&expanded);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    normalize_lexically(&joined)
}

/// Collapse `.`/`..` path components without touching the filesystem.
///
/// Why: [`resolve_target_path`] must not call `fs::canonicalize` (the target
/// usually doesn't exist yet), but a purely textual join like
/// `/repo/../tmp/wt` must still be recognized as resolving under `/tmp`.
/// What: walks `path`'s components, popping the accumulator on `..` and
/// dropping `.`, otherwise appending. Does not consult the filesystem, so it
/// cannot see through symlinks (see the residual-bypass note on
/// `super::evaluate_worktree_add_command`).
fn normalize_lexically(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The first unexpanded shell variable left in `path`, if any.
///
/// Why: [`resolve_target_path`] expands `$TMPDIR`, `$TMP`, `$HOME` and `$PWD`
/// and nothing else, then JOINS whatever is left onto the base — so a
/// `$MAIN`-style variable survives as a literal path COMPONENT and two rules
/// then state something false about the directory it names. The
/// worktree-removal re-checks probed
/// `<repo>/$MAIN/$MAIN/.claude/worktrees/agent-…` and reported the resulting
/// `git status` failure as a dirty tree (#7098); the ADR-0037 destructive rule
/// walked `<repo>/$WT` up to the nearest `.git` and called that directory a
/// main checkout (#7100). Both callers still DENY — a directory the guard
/// cannot resolve is never cleared, and an empty `$WT` would run the command in
/// the checkout itself — but each now names what it could not establish instead
/// of asserting something untrue.
/// What: `Some("$MAIN")` / `Some("${MAIN}")` for the first `$name` or
/// `${name}` in the path's text, `None` when nothing is left to expand. A `$`
/// that opens no name — a directory literally called `$` — is not a variable
/// and answers `None`.
/// Test: `unexpanded_shell_variable_finds_both_spellings`,
/// `unexpanded_shell_variable_is_none_for_ordinary_paths`.
pub(super) fn unexpanded_shell_variable(path: &Path) -> Option<String> {
    let text = path.to_string_lossy();
    for (i, _) in text.match_indices('$') {
        let rest = &text[i + 1..];
        if let Some(inner) = rest.strip_prefix('{') {
            if let Some(end) = inner.find('}')
                && !inner[..end].is_empty()
                && inner[..end].chars().all(is_shell_name_char)
            {
                return Some(format!("${{{name}}}", name = &inner[..end]));
            }
            continue;
        }
        let name: String = rest
            .chars()
            .take_while(|c| is_shell_name_char(*c))
            .collect();
        if !name.is_empty() {
            return Some(format!("${name}"));
        }
    }
    None
}

/// Whether `c` may appear in a shell variable name.
fn is_shell_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// What [`resolve_target_path`] could not expand, and the path a refusal
/// should quote because of it.
///
/// Why: two expansions survive resolution and they need DIFFERENT paths in the
/// refusal. A `$WT` the guard does not expand is joined onto the directory the
/// command actually stands in, so `/repo/$WT` is a true statement about where
/// an empty `$WT` would land and the whole path is worth showing (#7100). A
/// `~` that survived because `$HOME` was unset is not: a shell never joins `~`
/// onto the working directory, so `<launch-dir>/~/scratch/repo` names nothing
/// that exists — and quoting it is how the guard came to call an agent's
/// launch directory a main checkout (#7234). For that case the honest path is
/// the suffix from the tilde on, which is the token as the command wrote it.
/// What: `token` is the expansion itself (`$WT`, `${WT}`, `~`, `~user`);
/// `shown` is the path to name in the refusal.
/// Test: `unresolved_target_reports_a_surviving_tilde_as_written`,
/// `unresolved_target_is_none_once_home_expands_the_tilde`.
pub(super) struct UnresolvedTarget {
    pub(super) token: String,
    pub(super) shown: std::path::PathBuf,
}

/// The first expansion [`resolve_target_path`] could not perform in `path`.
///
/// Why: every rule in this module tree denies a directory it cannot resolve,
/// and each was asking only [`unexpanded_shell_variable`] — which scans for a
/// `$`-prefixed token and nothing else. A leading `~` reaches
/// [`resolve_target_path`] as literal text whenever `$HOME` is unset, survives
/// as a path COMPONENT, and then reads as an ordinary relative path: it is
/// joined onto the base, `main_checkout_root` walks up into the base's `.git`,
/// and the refusal names a checkout the command never addressed (#7234).
/// Routing both spellings through one detector is what keeps a rule added
/// later from inheriting only half the answer.
/// What: the `$NAME` answer first, then a path COMPONENT beginning with `~`.
/// Fails CLOSED, in the direction this module tree already takes: a directory
/// genuinely named `~backup` is reported unresolved and the caller refuses
/// rather than clears it.
/// Test: `unresolved_target_reports_a_surviving_tilde_as_written`,
/// `unresolved_target_is_none_once_home_expands_the_tilde`,
/// `unexpanded_shell_variable_finds_both_spellings`.
pub(super) fn unresolved_target(path: &Path) -> Option<UnresolvedTarget> {
    if let Some(token) = unexpanded_shell_variable(path) {
        return Some(UnresolvedTarget {
            token,
            shown: path.to_path_buf(),
        });
    }
    let components: Vec<_> = path.components().collect();
    let at = components
        .iter()
        .position(|c| c.as_os_str().to_string_lossy().starts_with('~'))?;
    Some(UnresolvedTarget {
        token: components[at].as_os_str().to_string_lossy().into_owned(),
        shown: components[at..].iter().collect(),
    })
}

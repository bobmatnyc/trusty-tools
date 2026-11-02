//! The whole-server kill floor of `tm hook --pm-guard` (#10665).
//!
//! Why: on 2026-11-01 a scratch script ran `TMUX_TMPDIR=$T tmux kill-server`.
//! An inherited `$TMUX` wins over `TMUX_TMPDIR`, so the command killed the
//! fleet's tmux server and every session on it. `-L` does override an
//! inherited `$TMUX` (verified on tmux 3.6b), and `-S` always does.
//! What: [`evaluate_tmux_kill_server`] refuses a `tmux kill-server` or
//! `kill-session -a` unless it reaches a scratch server, read from its
//! `-L`/`-S` words ([`scratch_server`]): an absolute `-S` path that is not the
//! socket `$TMUX` names and not in the hook's tmux socket directory, or an
//! `-L` label other than `default` and the label of the `$TMUX` socket. An
//! `env -u TMUX` or `env -i` that runs tmux directly with `TMUX_TMPDIR` set
//! to an absolute scratch directory (not `/tmp`) reaches that directory's
//! default socket, and an `-S "$VAR"` the command assigns once from a literal
//! or `mktemp` is read as that path (`socket`). A `kill-session` whose target
//! names a fleet session (`tm-…`) on the default or `$TMUX` server refuses
//! too. Every caller is bound, the Architect included.
//! FAIL-CLOSED (`walk`): every kill word the command spells must sit in a
//! tmux command judged here or in an argument of a known-inert program
//! (`echo`, `printf`, `grep`/`rg`, `sed -n`, `cat`, `git … -m`) that prints
//! to stdout; anything else — a script written and run in the same command,
//! `awk`, `ssh`, an alias, inline evaluator code — refuses. So does a
//! command the guard cannot classify.
//! Residual, out of #10665's scope: `pkill tmux`, `killall tmux`,
//! `kill -TERM <server pid>`, a symlink to the default socket created in the
//! same command, and `tmux source-file` of a file holding `kill-server`.
//! Test: `tmux_kill_server_tests.rs`; end to end in
//! `tests/tm_hook_pm_guard_tmux_kill_server_10665.rs`.

use std::path::{Path, PathBuf};

use super::super::architect_pane_env::{ENV_RESET, SERVER_ENV};
use super::super::architect_pane_parse::{Hit, Target, explicit_socket, tmux_hits};
use super::super::unclassifiable_command;
use trusty_mpm::core::tmux_server::socket_dir;

// #10665 r3: the env-cleared and variable `-S` forms, and the fail-closed walk.
#[path = "tmux_kill_server_socket.rs"]
mod socket;
#[path = "tmux_kill_server_walk.rs"]
mod walk;

/// The rule name recorded with a whole-server kill deny.
pub(crate) const TMUX_KILL_SERVER_RULE: &str = "tmux-kill-server";

/// The safe forms every refusal names.
const SAFE_FORM: &str = "the safe form is `tmux -S <absolute scratch socket> kill-server` \
     (or `kill-session -a`) with a socket outside the tmux socket directory, \
     `tmux -L <label> kill-server` with a label other than `default` and this session's own, \
     or `env -u TMUX TMUX_TMPDIR=<absolute scratch dir> tmux kill-server`";

/// What decides which socket a bare `tmux` reaches: the hook's environment.
#[derive(Debug, Clone, Default)]
pub(crate) struct ServerEnv {
    /// `$TMUX`, `<socket>,<pid>,<session>`, when set.
    pub(crate) tmux: Option<String>,
    /// `$TMUX_TMPDIR`, when set.
    pub(crate) tmux_tmpdir: Option<PathBuf>,
    /// The real uid, which names the `tmux-<uid>` socket directory.
    pub(crate) uid: u32,
}

impl ServerEnv {
    /// This process's `$TMUX`, `$TMUX_TMPDIR` and real uid.
    pub(crate) fn from_process() -> Self {
        Self {
            tmux: std::env::var("TMUX").ok().filter(|v| !v.is_empty()),
            tmux_tmpdir: std::env::var_os("TMUX_TMPDIR").map(PathBuf::from),
            uid: nix::unistd::getuid().as_raw(),
        }
    }

    /// The tmux socket directories a bare `tmux` or `-L` reaches: the one
    /// `$TMUX_TMPDIR` names and `/tmp`'s, symlinks followed.
    fn socket_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![socket_dir(None, self.uid)];
        if let Some(tmpdir) = self.tmux_tmpdir.as_deref() {
            dirs.push(socket_dir(Some(tmpdir), self.uid));
        }
        dirs
    }

    /// The socket `$TMUX` names, its first comma field.
    fn inherited_socket(&self) -> Option<PathBuf> {
        let tmux = self.tmux.as_deref()?;
        let socket = tmux.split(',').next().filter(|s| !s.is_empty())?;
        Some(canonical(Path::new(socket)))
    }
}

/// Classify a Bash command against the whole-server kill floor:
/// `Some(reason)` denies.
///
/// Why: the one entry point `pm_guard_floor` calls for a Bash command (#10665).
/// What: see the module doc. A kill the text spells is judged by `walk`; a
/// hit the text does not spell (a decoded `$'…'`), and every fleet-session
/// `kill-session`, by the hits of the whole command.
/// Test: `the_incident_shape_is_refused_with_tmux_inherited`,
/// `an_explicit_scratch_socket_is_allowed`,
/// `a_default_socket_or_label_is_refused`,
/// `wrapped_nested_and_chained_kills_are_refused`,
/// `an_unreadable_kill_fails_closed`,
/// `ordinary_tmux_commands_are_unaffected`,
/// `env_cleared_scratch_tmpdir_kills_are_allowed`,
/// `a_kill_word_no_tmux_hit_accounts_for_is_refused`,
/// `a_kill_word_in_an_inert_argument_is_allowed`,
/// `a_fleet_session_kill_needs_a_scratch_server`.
pub(crate) fn evaluate_tmux_kill_server(command: &str, env: &ServerEnv) -> Option<String> {
    let mentions_kill = names_whole_server_kill(command);
    // #10665: fail closed — a command the guard cannot classify that names a
    // whole-server kill is refused, whatever tmux_hits made of it.
    if mentions_kill && unclassifiable_command(command).is_some() {
        return Some(refusal("the guard cannot read the command"));
    }
    // #10665 r3: a kill word no judged tmux command accounts for refuses.
    if mentions_kill && let Err(why) = walk::judge(command, command, 0, env) {
        return Some(refusal(&why));
    }
    for hit in tmux_hits(command) {
        let spelled = mentions_kill && hit.targets.contains(&Target::Server);
        if hit.opaque.is_some() && hit.targets.is_empty() || spelled {
            continue;
        }
        if let Err(why) = judge_hit(&hit, env) {
            return Some(refusal(&why));
        }
    }
    None
}

/// `Err` naming why `hit` — a whole-server kill, or a `kill-session` of a
/// fleet session — does not reach a scratch server.
fn judge_hit(hit: &Hit, env: &ServerEnv) -> Result<(), String> {
    if hit.targets.contains(&Target::Server) {
        let verb = if hit.verb == "kill-server" {
            "`tmux kill-server`"
        } else {
            "`tmux kill-session -a`"
        };
        return scratch_server(&hit.server, env).map_err(|why| format!("{verb} {why}"));
    }
    // #10665 r3: a `tm-` session lives on the fleet's server.
    let fleet = hit.targets.iter().find_map(|t| match t {
        Target::Literal(t) if t.strip_prefix('=').unwrap_or(t).starts_with("tm-") => Some(t),
        _ => None,
    });
    match fleet {
        Some(target) if hit.verb == "kill-session" => {
            scratch_server(&hit.server, env).map_err(|why| {
                format!("`tmux kill-session -t {target}` kills a fleet session and {why}")
            })
        }
        _ => Ok(()),
    }
}

/// Whether the #9001 and #8902 floors leave `hit` to this one (#10665 r3):
/// a whole-server kill whose only unreadable part is a moved or reset
/// environment, which [`evaluate_tmux_kill_server`] has already judged.
pub(in super::super) fn owned_by_kill_floor(hit: &Hit) -> bool {
    matches!(hit.opaque, Some(why) if why == SERVER_ENV || why == ENV_RESET)
        && !hit.targets.is_empty()
        && hit.targets.iter().all(|t| *t == Target::Server)
}

/// `Ok` when `server`, a hit's `-L`/`-S` words, selects a scratch server;
/// `Err` names why it does not.
fn scratch_server(server: &[String], env: &ServerEnv) -> Result<(), &'static str> {
    let Some(socket) = explicit_socket(server) else {
        // #10665 r3: `-L` overrides an inherited `$TMUX` (tmux 3.6b).
        let label = server
            .chunks(2)
            .filter(|pair| pair.first().is_some_and(|f| f == "-L"))
            .filter_map(|pair| pair.get(1))
            .next_back();
        let Some(label) = label else {
            return Err(
                "gives no `-S <socket>` or `-L <label>`, so it reaches the server \
                 `$TMUX` names or the default one; `TMUX_TMPDIR` does not override an \
                 inherited `$TMUX`",
            );
        };
        if label == "default" {
            return Err("names `-L default`, the default server");
        }
        let own = env.inherited_socket();
        if own.as_deref().and_then(Path::file_name) == Some(label.as_ref()) {
            return Err("names the `-L` label of the server `$TMUX` names: this session's");
        }
        return Ok(());
    };
    let path = Path::new(socket);
    if !path.is_absolute() {
        return Err("gives a relative `-S` path, which the guard cannot resolve");
    }
    let path = canonical(path);
    if env.inherited_socket().is_some_and(|s| s == path) {
        return Err("names the socket `$TMUX` names: the server this session runs on");
    }
    if path
        .parent()
        .is_some_and(|dir| env.socket_dirs().iter().any(|d| d == dir))
    {
        return Err(
            "names a socket in the tmux socket directory, where the default \
             server and every `-L` server live",
        );
    }
    Ok(())
}

/// `path` with its symlinks followed: its longest existing ancestor
/// canonicalized, the rest joined back (`/tmp/tmux-1/x` with no
/// `/tmp/tmux-1` yet is `/private/tmp/tmux-1/x` on macOS).
fn canonical(path: &Path) -> PathBuf {
    for base in path.ancestors() {
        if let Ok(real) = std::fs::canonicalize(base)
            && let Ok(rest) = path.strip_prefix(base)
        {
            return if rest.as_os_str().is_empty() {
                real
            } else {
                real.join(rest)
            };
        }
    }
    path.to_path_buf()
}

/// Whether `command` names a whole-server kill as text: a `kill-server` word
/// or a unique prefix of it (`kill-ser`), or a `kill-session` word (or
/// prefix) with an `-a…` flag word anywhere after it; also once quotes are
/// removed (`kill-""server`, #10665 r3).
pub(crate) fn names_whole_server_kill(command: &str) -> bool {
    let unquoted: String = command.chars().filter(|c| !"'\"\\".contains(*c)).collect();
    spells_kill(command) || spells_kill(&unquoted)
}

/// The text half of [`names_whole_server_kill`].
fn spells_kill(text: &str) -> bool {
    let words: Vec<&str> = text
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|w| !w.is_empty())
        .collect();
    // tmux takes a unique prefix; `kill-s` is ambiguous, `kill-se` too.
    let prefix_of = |w: &str, full: &str| w.len() >= 8 && full.starts_with(&w.to_ascii_lowercase());
    words.iter().enumerate().any(|(i, w)| {
        prefix_of(w, "kill-server")
            || (prefix_of(w, "kill-session")
                && words[i + 1..]
                    .iter()
                    .any(|f| f.starts_with('-') && !f.starts_with("--") && f.contains('a')))
    })
}

/// The refusal text for `why`, naming the rule and the safe forms.
pub(crate) fn refusal(why: &str) -> String {
    format!(
        "Refused: a whole-server tmux kill (#10665): {why}. An inherited `$TMUX` once \
         killed the fleet's tmux server this way; {SAFE_FORM}."
    )
}

#[cfg(test)]
#[path = "tmux_kill_server_tests.rs"]
mod tests;

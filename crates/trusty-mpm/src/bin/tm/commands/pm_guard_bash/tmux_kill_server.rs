//! The whole-server kill floor of `tm hook --pm-guard` (#10665).
//!
//! Why: on 2026-11-01 a scratch script ran `TMUX_TMPDIR=$T tmux kill-server`.
//! An inherited `$TMUX` wins over `TMUX_TMPDIR` (and over `-L`), so the
//! command killed the fleet's tmux server and every session on it. Owner
//! ruling (#10665): `tmux kill-server` and `tmux kill-session -a` run only
//! with an explicit `-S <socket>` naming a scratch socket.
//! What: [`evaluate_tmux_kill_server`] takes the tmux commands
//! `architect_pane_parse::tmux_hits` finds — the parser that already reads
//! env prefixes, `env`/`command` and other wrappers, `sh -c`, `$(…)` and
//! `;`/`&&` chains — and refuses each whole-server hit
//! (`Target::Server`) whose server words carry no absolute `-S` path, or an
//! `-S` path that is the default server's socket: the socket `$TMUX` names,
//! or any socket in the hook's tmux socket directory (where `-L` and the
//! default server live). Every caller is bound, the Architect included.
//! FAIL-CLOSED: when the guard cannot read the command — an opaque hit with
//! no readable verb, or a command it cannot classify — a `kill-server` word,
//! or a `kill-session` word beside an `-a` flag, refuses. So does inline
//! code a non-shell evaluator runs (`python3 -c`, `perl -e`, `node -e`,
//! `osascript -e`, …) that names one: the guard cannot read it as shell.
//! Residual, out of #10665's scope: `pkill tmux`, `killall tmux`,
//! `kill -TERM <server pid>`, a shell `alias`, a symlink to the default
//! socket created in the same command, `tmux source-file` of a file holding
//! `kill-server`, and a script written and run in the same command.
//! Test: `tmux_kill_server_tests.rs`; end to end in
//! `tests/tm_hook_pm_guard_tmux_kill_server_10665.rs`.

use std::path::{Path, PathBuf};

use super::super::architect_pane_parse::{Target, explicit_socket, tmux_hits};
use super::super::credential_print::{code_operands, evaluator_name};
use super::super::floor_d4::{program_positions, segments};
use super::super::shell_lex::DASH_C_SHELLS;
use super::super::unclassifiable_command;
use trusty_mpm::core::tmux_server::socket_dir;

/// The rule name recorded with a whole-server kill deny.
pub(crate) const TMUX_KILL_SERVER_RULE: &str = "tmux-kill-server";

/// The safe form every refusal names.
const SAFE_FORM: &str = "the safe form is `tmux -S <absolute scratch socket> kill-server` \
     (or `kill-session -a`), with a socket path outside the tmux socket directory";

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
/// What: see the module doc. The last `-S` word of a hit is the socket tmux
/// uses (`-S` overrides `-L`, a later `-S` an earlier one).
/// Test: `the_incident_shape_is_refused_with_tmux_inherited`,
/// `an_explicit_scratch_socket_is_allowed`,
/// `a_default_socket_or_label_is_refused`,
/// `wrapped_nested_and_chained_kills_are_refused`,
/// `an_unreadable_kill_fails_closed`,
/// `ordinary_tmux_commands_are_unaffected`.
pub(crate) fn evaluate_tmux_kill_server(command: &str, env: &ServerEnv) -> Option<String> {
    let mentions_kill = names_whole_server_kill(command);
    // #10665: fail closed — a command the guard cannot classify that names a
    // whole-server kill is refused, whatever tmux_hits made of it.
    if mentions_kill && unclassifiable_command(command).is_some() {
        return Some(refusal("the guard cannot read the command"));
    }
    // #10665 r2: evaluator code is not shell, so `tmux_hits` never reads it.
    if mentions_kill && evaluator_code_names_kill(command) {
        return Some(refusal(
            "inline code an evaluator runs names one, and the guard cannot read it as shell",
        ));
    }
    for hit in tmux_hits(command) {
        if let Some(why) = hit.opaque
            && hit.targets.is_empty()
        {
            if mentions_kill {
                return Some(refusal(why));
            }
            continue;
        }
        if !hit.targets.contains(&Target::Server) {
            continue;
        }
        let verb = if hit.verb == "kill-server" {
            "`tmux kill-server`"
        } else {
            "`tmux kill-session -a`"
        };
        if let Err(why) = scratch_socket(&hit.server, env) {
            return Some(refusal(&format!("{verb} {why}")));
        }
    }
    None
}

/// Whether a segment of `command`, wrapper bodies included, runs a non-shell
/// evaluator (`python3 -c`, `perl -e`, `node -e`, `osascript -e`) whose
/// inline code names a whole-server kill. Shells, `eval` and `source` are
/// read by `tmux_hits`; `ssh` runs on another host.
fn evaluator_code_names_kill(command: &str) -> bool {
    let read_as_shell = |name: &str| {
        DASH_C_SHELLS.contains(&name) || matches!(name, "eval" | "source" | "." | "ssh")
    };
    segments(command).iter().any(|seg| {
        let Some(argv) = shlex::split(&seg.text) else {
            return false;
        };
        program_positions(&argv).into_iter().any(|(at, program)| {
            let program = program.to_ascii_lowercase();
            evaluator_name(&program).is_some_and(|name| {
                !read_as_shell(name)
                    && code_operands(name, argv.get(at + 1..).unwrap_or_default())
                        .into_iter()
                        .any(names_whole_server_kill)
            })
        })
    })
}

/// `Ok` when `server`, a hit's `-L`/`-S` words, selects an explicit scratch
/// socket; `Err` names why it does not.
fn scratch_socket(server: &[String], env: &ServerEnv) -> Result<(), &'static str> {
    let Some(socket) = explicit_socket(server) else {
        return Err(
            "gives no `-S <socket>`, so it reaches the server `$TMUX` names or the \
             default one; `-L` and `TMUX_TMPDIR` do not override an inherited `$TMUX`",
        );
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

/// Whether `command` names a whole-server kill as text: a `kill-server` word,
/// or a `kill-session` word with an `-a…` flag word anywhere after it.
pub(crate) fn names_whole_server_kill(command: &str) -> bool {
    let words: Vec<&str> = command
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|w| !w.is_empty())
        .collect();
    words.iter().enumerate().any(|(i, w)| {
        w.eq_ignore_ascii_case("kill-server")
            || (w.eq_ignore_ascii_case("kill-session")
                && words[i + 1..]
                    .iter()
                    .any(|f| f.starts_with('-') && !f.starts_with("--") && f.contains('a')))
    })
}

/// The refusal text for `why`, naming the rule and the safe form.
pub(crate) fn refusal(why: &str) -> String {
    format!(
        "Refused: a whole-server tmux kill (#10665): {why}. An inherited `$TMUX` once \
         killed the fleet's tmux server this way; {SAFE_FORM}."
    )
}

#[cfg(test)]
#[path = "tmux_kill_server_tests.rs"]
mod tests;

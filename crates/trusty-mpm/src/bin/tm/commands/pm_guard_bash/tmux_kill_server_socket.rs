//! The socket a tmux kill reaches when its argv does not spell it (#10665 r3).
//!
//! Why: Architect ruling df-D77. `env -u TMUX TMUX_TMPDIR=<dir> tmux
//! kill-server` reaches `<dir>/tmux-<uid>/default`, and the sandbox scripts
//! stop their server with `env -i … tmux -S "$SOCK" kill-server`. Neither
//! spells a literal `-S`, so the floor would refuse both.
//! What: [`explicit_argv`] writes the socket into the tmux argv as a literal
//! `-S`, which `scratch_server` then judges: the default socket of a scratch
//! `TMUX_TMPDIR` that `env` sets while it removes `TMUX` (`-u TMUX` or `-i`),
//! when `env` runs tmux directly and nothing else in the command sets `TMUX`;
//! and for `-S "$VAR"`, the one literal absolute path the command assigns to
//! `VAR`, or [`FRESH_SCRATCH`] for a `mktemp` value or, under `env -i`, any
//! value. Anything else is left as written, and refuses.
//! Test: `env_cleared_scratch_tmpdir_kills_are_allowed`,
//! `a_socket_variable_is_read_from_its_one_assignment`.

use std::path::Path;

use super::super::super::architect_pane_env::moves_server_env;
use super::super::super::floor_d4::{grammar_prefix, segments};
use super::{ServerEnv, canonical};
use crate::commands::hook_rewrite::is_env_assignment;
use trusty_mpm::core::tmux_server::socket_dir;

/// A path no tmux server listens on, outside every socket directory: the
/// socket of a server the command creates fresh (`mktemp`, `env -i`).
const FRESH_SCRATCH: &str = "/nonexistent/tm-guard-fresh-scratch.sock";

/// `argv` with the socket its tmux (at `pos`) reaches written as a literal
/// `-S` where the rules in the module doc place it. `script` is the whole
/// command or script body, which holds the assignments.
pub(super) fn explicit_argv(
    argv: &[String],
    pos: usize,
    script: &str,
    env: &ServerEnv,
) -> Vec<String> {
    let mut argv = argv.to_vec();
    let prefix = env_prefix(&argv, pos);
    let reset = prefix.as_ref().is_some_and(|p| p.reset);
    let mut has_server = false;
    let mut i = pos + 1;
    while let Some(word) = argv.get(i) {
        if word == "--" || !word.starts_with('-') || word == "-" {
            break;
        }
        let option = word
            .char_indices()
            .skip(1)
            .find(|(_, c)| "cfLST".contains(*c))
            .map(|(k, c)| (c, word[k + c.len_utf8()..].is_empty()));
        if let Some((c, separate)) = option {
            has_server |= c == 'L' || c == 'S';
            if c == 'S'
                && separate
                && let Some(name) = argv.get(i + 1).and_then(|v| variable(v))
                && let Some(path) =
                    assigned_path(name, script).or_else(|| reset.then(|| FRESH_SCRATCH.into()))
            {
                argv[i + 1] = path;
            }
            i += usize::from(separate);
        }
        i += 1;
    }
    let tmpdir = prefix.and_then(|p| p.cleared_tmpdir());
    if !has_server && let Some(dir) = tmpdir.filter(|_| !sets_tmux_elsewhere(script)) {
        let socket = socket_dir(Some(Path::new(&dir)), env.uid).join("default");
        argv.splice(
            pos + 1..pos + 1,
            ["-S".to_string(), socket.display().to_string()],
        );
    }
    argv
}

/// What an `env` that runs tmux directly does to its environment.
struct EnvPrefix {
    /// `-i`, `-` or `--ignore-environment`: nothing is inherited.
    reset: bool,
    /// `-u TMUX` (or `--unset`).
    unsets_tmux: bool,
    /// The last `TMUX_TMPDIR=` it sets.
    tmpdir: Option<String>,
}

impl EnvPrefix {
    /// The scratch directory tmux finds its server in: `TMUX` removed and
    /// `TMUX_TMPDIR` a literal absolute directory other than `/tmp`.
    fn cleared_tmpdir(self) -> Option<String> {
        let dir = self.tmpdir.filter(|_| self.reset || self.unsets_tmux)?;
        let literal = dir.starts_with('/') && !dir.contains(['$', '`', '~']);
        let real = canonical(Path::new(&dir));
        let tmp = real == Path::new("/tmp") || real == Path::new("/private/tmp");
        (literal && !tmp).then_some(dir)
    }
}

/// The `env` that runs the tmux at `pos` directly — `env`, then only its
/// options and assignments — or `None`. An option this reader does not know,
/// or a `TMUX=` assignment, is `None` too.
fn env_prefix(argv: &[String], pos: usize) -> Option<EnvPrefix> {
    let words = argv.get(grammar_prefix(argv)..pos)?;
    let (first, rest) = words.split_first()?;
    let base = first.rsplit('/').next().unwrap_or(first);
    if base != "env" && base != "genv" {
        return None;
    }
    let mut out = EnvPrefix {
        reset: false,
        unsets_tmux: false,
        tmpdir: None,
    };
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        let unset = match word.as_str() {
            "-i" | "-" | "--ignore-environment" => {
                out.reset = true;
                continue;
            }
            "-u" | "--unset" => words.next().map(String::as_str),
            w if w.starts_with("--unset=") => w.strip_prefix("--unset="),
            w if w.starts_with("-u") => w.strip_prefix("-u"),
            w if is_env_assignment(w) => {
                let (name, value) = w.split_once('=')?;
                match name {
                    "TMUX" => return None,
                    "TMUX_TMPDIR" => out.tmpdir = Some(value.to_string()),
                    _ => {}
                }
                continue;
            }
            _ => return None,
        };
        out.unsets_tmux |= unset? == "TMUX";
    }
    Some(out)
}

/// Whether the command sets `TMUX` anywhere but an `env` option that removes
/// it: the clearing words and `TMUX_TMPDIR=` are dropped, then
/// `moves_server_env` reads the rest, so a `TMUX_TMPDIR` change elsewhere
/// counts too (fail closed).
fn sets_tmux_elsewhere(script: &str) -> bool {
    let mut rest = script.to_string();
    for clearing in [
        "--unset=TMUX",
        "--unset TMUX",
        "-u TMUX",
        "-uTMUX",
        "TMUX_TMPDIR=",
    ] {
        rest = rest.replace(clearing, " ");
    }
    moves_server_env(&rest)
}

/// The variable a `$NAME` or `${NAME}` word reads, or `None`.
fn variable(word: &str) -> Option<&str> {
    let name = word.strip_prefix('$')?;
    let name = name
        .strip_prefix('{')
        .and_then(|n| n.strip_suffix('}'))
        .unwrap_or(name);
    let mut chars = name.chars();
    let head = chars.next()?;
    let ok = (head.is_ascii_alphabetic() || head == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    ok.then_some(name)
}

/// The path `script` gives `name`: its one assignment, when `name` is
/// written nowhere else, a literal absolute path or a `mktemp` value.
fn assigned_path(name: &str, script: &str) -> Option<String> {
    let writes = script.match_indices(name).filter(|(at, _)| {
        let before = script[..*at].chars().next_back();
        let after = script[at + name.len()..].chars().next();
        let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        !word(before) && !word(after) && !matches!(before, Some('$' | '{' | '#' | '!' | '-'))
    });
    if writes.count() != 1 {
        return None;
    }
    let prefix = format!("{name}=");
    let mut values = segments(script)
        .into_iter()
        .filter_map(|seg| shlex::split(&seg.text))
        .flatten()
        .filter_map(|w| w.strip_prefix(&prefix).map(str::to_string));
    let value = values.next()?;
    if value.starts_with("$(mktemp") || value.starts_with("`mktemp") {
        return Some(FRESH_SCRATCH.into());
    }
    let literal = value.starts_with('/') && !value.contains(['$', '`', '~', '*', '?', '[']);
    literal.then_some(value)
}

//! Which tmux server a command reaches, for the Architect pane floor (#8902).
//!
//! Why: the floor lists the server the hook process sees. A command that
//! changes the environment tmux finds its server in, or that renames or
//! creates a session, reaches panes that list does not describe.
//! What: the reasons such a hit carries, [`moves_server_env`] and
//! [`resets_env`].
//! Test: `a_server_the_command_selects_differently_denies`,
//! `a_nested_invocation_inherits_the_outer_server`.

/// Why a hit denies when the command changes `TMUX` or `TMUX_TMPDIR`.
pub(super) const SERVER_ENV: &str = "the command sets or unsets `TMUX` or `TMUX_TMPDIR`, so its \
     tmux server may not be the one the guard lists";

/// Why a hit denies behind `sudo`, `doas`, `env -i` or `exec -c`.
pub(super) const ENV_RESET: &str = "`sudo`, `doas`, `env -i` or `exec -c` changes the \
     environment tmux finds its server in";

/// Why a hit denies under a relative `-S`: the probe cannot know its directory.
pub(super) const RELATIVE_SOCKET: &str = "a relative `-S` socket path resolves against a \
     directory the guard does not know";

/// Why a hit on a server holding the Architect denies beside a session rename
/// or creation.
pub(super) const RETARGET: &str = "the command also renames or creates a session, and targets \
     resolve against the sessions that exist before it runs";

/// Whether `command` names `TMUX` or `TMUX_TMPDIR` other than in a `$`
/// expansion that only reads it: an assignment, `unset`, `env -u`,
/// `export -n`, and `${TMUX=…}` / `${TMUX:=…}`, which assign when unset.
pub(super) fn moves_server_env(command: &str) -> bool {
    command.match_indices("TMUX").any(|(at, _)| {
        let after = &command[at + 4..];
        let after = after.strip_prefix("_TMPDIR").unwrap_or(after);
        let whole = !after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_');
        let before = &command[..at];
        let expanded = before.trim_end_matches(['{', '#', '!']).ends_with('$');
        // #8902: `${TMUX:=x}` assigns `TMUX` when it is unset or empty.
        let assigns = before.ends_with("${") && (after.starts_with('=') || after.starts_with(":="));
        whole && (!expanded || assigns)
    })
}

/// Whether the words before a `tmux` program word reset its environment:
/// `sudo`, `doas`, `env` with `-i`, `-` or `--ignore-environment`, or
/// `exec -c`.
pub(super) fn resets_env(before: &[String]) -> bool {
    let (mut in_env, mut in_exec) = (false, false);
    for word in before {
        let tok = word.strip_prefix('\\').unwrap_or(word);
        let short =
            |flag: char| tok.starts_with('-') && !tok.starts_with("--") && tok.contains(flag);
        match tok.rsplit('/').next().unwrap_or(tok) {
            "sudo" | "doas" => return true,
            "env" | "genv" => in_env = true,
            "exec" => in_exec = true,
            _ if in_env && (tok == "-" || tok == "--ignore-environment" || short('i')) => {
                return true;
            }
            // #8902: `exec -c` runs the command with an empty environment.
            _ if in_exec && short('c') => return true,
            _ => {}
        }
    }
    false
}

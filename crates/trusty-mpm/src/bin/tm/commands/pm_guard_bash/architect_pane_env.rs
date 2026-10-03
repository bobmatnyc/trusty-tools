//! Which tmux server a command reaches, for the Architect pane floor (#8902).
//!
//! Why: the floor lists the server the hook process sees. A command that
//! changes the environment tmux finds its server in, or that renames or
//! creates a session, reaches panes that list does not describe.
//! What: the reasons such a hit carries, [`moves_server_env`],
//! [`assigns_dynamic_name`] and [`resets_env`].
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

/// Why an omitted target of a `tmux` typed into a pane on another server
/// cannot be resolved (#8902 follow-up LOW).
pub(super) const PICKED: &str = "a nested command with no target, on another server than the \
     keys go to, acts on a pane tmux picks";

/// Why a hit on a server holding the Architect denies beside a session rename
/// or creation.
pub(super) const RETARGET: &str = "the command also renames or creates a session, and targets \
     resolve against the sessions that exist before it runs";

/// Whether `command` may change `TMUX` or `TMUX_TMPDIR`.
///
/// What: the name outside a `$` expansion that only reads it — an assignment,
/// `unset`, `env -u`, `export -n`, `${TMUX=…}`, `${TMUX:=…}` and zsh's
/// `${TMUX::=…}` — also once quotes are removed (`export TM''UX=…`), and any
/// assigning expansion whose name the guard cannot read
/// ([`assigns_unread_name`]).
/// Test: `a_server_the_command_selects_differently_denies`,
/// `a_tmux_assignment_inside_a_word_or_through_an_unread_name_denies`,
/// `an_unreadable_expansion_or_dynamic_name_counts_as_a_server_move`.
pub(super) fn moves_server_env(command: &str) -> bool {
    // #8902: quote removal joins a split name, `TM''UX=` or `"TM"UX=`.
    let unquoted: String = command.chars().filter(|c| !"'\"\\".contains(*c)).collect();
    names_server_env(command) || names_server_env(&unquoted) || assigns_unread_name(command)
}

/// The literal-name half of [`moves_server_env`].
fn names_server_env(command: &str) -> bool {
    command.match_indices("TMUX").any(|(at, _)| {
        let after = &command[at + 4..];
        let after = after.strip_prefix("_TMPDIR").unwrap_or(after);
        let whole = !after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_');
        let before = &command[..at];
        let expanded = before.trim_end_matches(['{', '#', '!']).ends_with('$');
        // #8902: `${TMUX:=x}` assigns `TMUX` when it is unset or empty; zsh's
        // `${TMUX::=x}` always does.
        let assigns = before.ends_with("${") && starts_assigning(after);
        whole && (!expanded || assigns)
    })
}

/// Whether `op`, the text after a parameter name, opens `=`, `:=` or `::=`.
fn starts_assigning(op: &str) -> bool {
    ["=", ":=", "::="].iter().any(|a| op.starts_with(a))
}

/// Whether a `${…}` expansion may assign a variable the guard cannot name:
/// zsh's `(P)` flag, bash's `${!name…}`, or a nested expansion as the name,
/// with an `=` in the expansion. A `${` with no closing brace counts.
///
/// Why: #8902 follow-up — `n=TM; n+=UX; : ${(P)n::=…}` assigns `TMUX` with no
/// `TMUX` in the text, so an unread name counts as `TMUX` (fail closed).
/// What: one pass pairs every brace and counts `=` and `P`, so each `${` is
/// judged in constant time.
/// Test: `a_tmux_assignment_inside_a_word_or_through_an_unread_name_denies`.
fn assigns_unread_name(command: &str) -> bool {
    if !command.contains("${") {
        return false;
    }
    // #8902: a rescan per `${` made a deeply nested `${a${a…}}` quadratic, and
    // a hook that times out lets the command run.
    let bytes = command.as_bytes();
    let n = bytes.len();
    let mut close: Vec<Option<usize>> = vec![None; n];
    let mut open = Vec::new();
    let (mut eq, mut p) = (vec![0usize; n + 1], vec![0usize; n + 1]);
    for (i, b) in bytes.iter().enumerate() {
        match b {
            b'{' => open.push(i),
            b'}' => {
                if let Some(o) = open.pop() {
                    close[o] = Some(i);
                }
            }
            _ => {}
        }
        eq[i + 1] = eq[i] + usize::from(*b == b'=');
        p[i + 1] = p[i] + usize::from(*b == b'P');
    }
    let mut next_paren = vec![n; n + 1];
    for i in (0..n).rev() {
        next_paren[i] = if bytes[i] == b')' {
            i
        } else {
            next_paren[i + 1]
        };
    }
    command.match_indices("${").any(|(at, _)| {
        let Some(end) = close[at + 1] else {
            return true;
        };
        let start = at + 2;
        // `(flags)name`: the flags run to the first `)` in the body.
        let (flags, name) = if bytes.get(start) == Some(&b'(') {
            let shut = next_paren[start + 1].min(end);
            (start + 1..shut, (shut + 1).min(end))
        } else {
            (start..start, start)
        };
        let unread =
            p[flags.end] > p[flags.start] || (name < end && matches!(bytes[name], b'!' | b'$'));
        unread && eq[end] > eq[start]
    })
}

/// Whether one segment's argv runs an assignment builtin on a name the shell
/// expands: `export "${n}UX=…"`, `typeset $n=…`, `unset "$n"`, `read "$n"`,
/// `printf -v "$n"`, or `eval` of expanded text.
///
/// Why: #8902 follow-up — the expanded name may be `TMUX`, which the guard
/// cannot see, so it counts as one (fail closed). A value the shell expands
/// (`export PATH="$PATH:/x"`) is not a name and does not count.
/// Test: `a_tmux_assignment_inside_a_word_or_through_an_unread_name_denies`.
pub(super) fn assigns_dynamic_name(argv: &[String]) -> bool {
    let Some((program, args)) = argv.split_first() else {
        return false;
    };
    let dynamic = |w: &String| w.contains(['$', '`']);
    let name = |w: &String| w.split('=').next().unwrap_or_default().to_owned();
    match program.as_str() {
        "export" | "declare" | "typeset" | "local" | "readonly" | "integer" | "float" | "unset"
        | "read" | "let" | "getopts" => args
            .iter()
            .filter(|w| !w.starts_with('-'))
            .any(|w| dynamic(&name(w))),
        "eval" => args.iter().any(dynamic),
        "printf" => args
            .windows(2)
            .any(|pair| pair[0] == "-v" && dynamic(&pair[1])),
        _ => false,
    }
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

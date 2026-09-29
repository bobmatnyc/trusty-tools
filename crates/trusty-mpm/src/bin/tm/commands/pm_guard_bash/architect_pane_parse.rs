//! The tmux commands a Bash command runs, for the Architect pane floor (#8902).
//!
//! Why: the floor judges tmux verbs and their targets, so it needs every tmux
//! command a Bash command can run, and each target exactly as tmux reads it.
//! What: [`tmux_hits`] walks the command's segments (through `sh -c`, `env -S`,
//! `xargs` and `eval` wrappers) and its command substitutions, finds each
//! `tmux` program word, and parses the global options and each `;`-separated
//! command. A verb in [`DENY_VERBS`] yields a [`Hit`] with its targets. Every
//! argument of a known command is also read as a shell command, as a tmux
//! command string, and as the start of a tmux argv — the forms `run-shell`,
//! `if-shell`, `bind-key`, `new-window` and typed keys carry.
//! FAIL-CLOSED: see [`Hit::opaque`].
//! Test: `architect_pane_tests.rs`.

use super::architect_pane_verbs::{DENY_VERBS, Resolved, Verb, resolve};
use super::floor_d4::{program_positions, segments};
use super::shell_lex::QuoteScan;
use super::{command_substitutions, unclassifiable_command};

/// Nesting depth past which arguments are no longer read as commands.
const MAX_DEPTH: usize = 3;

/// Words of one argv read as the start of a nested tmux argv.
const MAX_SUFFIXES: usize = 64;

/// One shell word and whether the shell expands it (`$…`, a backtick, `~`).
#[derive(Debug, Clone)]
pub(super) struct Word {
    pub(super) text: String,
    pub(super) dynamic: bool,
}

/// A target as written: a literal, a word the shell expands, or a default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Target {
    /// A literal `-t`/`-s` value.
    Literal(String),
    /// A value the shell expands, so its text is unknown.
    Dynamic(String),
    /// An omitted `-t`: the caller's current pane.
    Current,
    /// An omitted `-s` of a source verb: the marked pane, else the current one.
    Marked,
    /// Every session on the server (`kill-server`, `kill-session -a`).
    Server,
}

/// One tmux command in the deny set, or one this guard cannot read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Hit {
    /// The verb as resolved, `-C`, or empty for an unreadable invocation.
    pub(super) verb: String,
    /// The `-L`/`-S` words selecting the server.
    pub(super) server: Vec<String>,
    /// Every target the command can reach.
    pub(super) targets: Vec<Target>,
    /// Why the command cannot be read: an unparseable command naming tmux, an
    /// expanded option or verb, control mode, `source-file`, an unknown verb,
    /// an alias defined to a deny verb, or an unknown or valueless option.
    /// Such a hit denies while an Architect is live, whatever its targets.
    pub(super) opaque: Option<&'static str>,
}

impl Hit {
    fn opaque(verb: &str, server: &[String], why: &'static str) -> Self {
        Self {
            verb: verb.to_string(),
            server: server.to_vec(),
            targets: Vec::new(),
            opaque: Some(why),
        }
    }
}

/// Every deny-set or unreadable tmux command `command` runs.
///
/// Test: `every_deny_verb_is_denied_in_each_target_form`,
/// `a_tmux_command_reached_through_a_wrapper_or_argument_is_found`.
pub(super) fn tmux_hits(command: &str) -> Vec<Hit> {
    let mut out = Vec::new();
    shell(command, 0, &mut out);
    out
}

/// Whether shell text that does not parse could run a deny-set tmux command:
/// it names `tmux` and, below the top level, also a deny verb — so typed
/// prose with an apostrophe that mentions tmux is not refused.
fn may_run_tmux(text: &str, depth: usize) -> bool {
    let words = || text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'));
    words().any(|w| w == "tmux")
        && (depth == 0 || words().any(|w| DENY_VERBS.iter().any(|v| v.name == w || v.alias == w)))
}

/// `command` with each unquoted, unescaped `\;` spelled `';'`.
///
/// Why: the shared segment splitter reads the `;` of tmux's `\;` separator as
/// a shell separator, which leaves a segment ending in a lone `\` that does
/// not lex. Both spellings give tmux the same `;` argument.
fn quote_escaped_semicolons(command: &str) -> String {
    let scan = QuoteScan::new(command);
    let bytes = command.as_bytes();
    let mut out = String::with_capacity(command.len());
    let mut i = 0;
    while i < bytes.len() {
        let escapes = bytes[..i].iter().rev().take_while(|b| **b == b'\\').count();
        if bytes[i] == b'\\'
            && bytes.get(i + 1) == Some(&b';')
            && scan.is_unquoted(i)
            && escapes % 2 == 0
        {
            out.push_str("';'");
            i += 2;
            continue;
        }
        let len = command[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&command[i..i + len]);
        i += len;
    }
    out
}

/// Read `command` as shell text.
fn shell(command: &str, depth: usize, out: &mut Vec<Hit>) {
    let command = &quote_escaped_semicolons(command);
    // #8902: `$'\x74mux'` hides the program name itself, so at the top level
    // any command the guard cannot classify counts.
    if unclassifiable_command(command).is_some() && (depth == 0 || may_run_tmux(command, depth)) {
        out.push(Hit::opaque("", &[], "the command does not parse"));
        return;
    }
    for seg in segments(command) {
        let Some(argv) = shlex::split(seg.text.trim()) else {
            if may_run_tmux(&seg.text, depth) {
                out.push(Hit::opaque("", &[], "the command does not parse"));
            }
            continue;
        };
        let words = with_dynamics(&seg.text, argv);
        let texts: Vec<String> = words.iter().map(|w| w.text.clone()).collect();
        for (pos, base) in program_positions(&texts) {
            if base == "tmux" {
                invocation(&words[pos + 1..], depth, out);
            }
        }
    }
    if depth < MAX_DEPTH {
        for body in command_substitutions(command) {
            shell(body.text(), depth + 1, out);
        }
    }
}

/// Pair each shlex word with whether the shell expands it.
fn with_dynamics(segment: &str, argv: Vec<String>) -> Vec<Word> {
    let flags = word_dynamics(segment.trim());
    let aligned = flags.len() == argv.len();
    argv.into_iter()
        .enumerate()
        .map(|(i, text)| {
            let dynamic = if aligned {
                flags[i]
            } else {
                text.contains(['$', '`'])
            };
            Word { text, dynamic }
        })
        .collect()
}

/// For each unquoted-whitespace-separated word of `segment`, whether it holds
/// a `$` or backtick outside single quotes and not escaped, or starts with an
/// unquoted `~`.
fn word_dynamics(segment: &str) -> Vec<bool> {
    let (mut out, mut single, mut double, mut escaped) = (Vec::new(), false, false, false);
    let mut current: Option<bool> = None;
    for c in segment.chars() {
        if escaped {
            escaped = false;
            current.get_or_insert(false);
            continue;
        }
        let quoted = single || double;
        if !quoted && c.is_whitespace() {
            out.extend(current.take());
            continue;
        }
        let at_start = current.is_none();
        let live = current.get_or_insert(false);
        match c {
            '\\' if !single => escaped = true,
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '$' | '`' if !single => *live = true,
            '~' if !quoted && at_start => *live = true,
            _ => {}
        }
    }
    out.extend(current);
    out
}

/// Parse one `tmux` invocation: its global options, then its commands.
fn invocation(words: &[Word], depth: usize, out: &mut Vec<Hit>) {
    let mut server: Vec<String> = Vec::new();
    let mut i = 0;
    while let Some(word) = words.get(i) {
        let text = word.text.as_str();
        if text == "--" {
            i += 1;
            break;
        }
        if !text.starts_with('-') || text == "-" {
            break;
        }
        if word.dynamic {
            out.push(Hit::opaque("", &server, "an option the shell expands"));
            return;
        }
        for (k, c) in text.char_indices().skip(1) {
            if c == 'C' {
                out.push(Hit::opaque(
                    "-C",
                    &server,
                    "control mode reads its commands from stdin",
                ));
                return;
            }
            if !"cfLST".contains(c) {
                continue;
            }
            let inline = &text[k + 1..];
            let value = if inline.is_empty() {
                i += 1;
                words.get(i).cloned()
            } else {
                Some(Word {
                    text: inline.to_string(),
                    dynamic: false,
                })
            };
            let Some(value) = value else {
                out.push(Hit::opaque("", &server, "an option has no value"));
                return;
            };
            if matches!(c, 'L' | 'S') {
                if value.dynamic {
                    out.push(Hit::opaque("", &server, "a server the shell expands"));
                    return;
                }
                server.extend([format!("-{c}"), value.text]);
            } else if c == 'c' && depth < MAX_DEPTH {
                shell(&value.text, depth + 1, out);
            }
            break;
        }
        i += 1;
    }
    commands(&words[i..], &server, depth, false, out);
}

/// Split `words` on tmux's `;` separators and judge each command.
fn commands(words: &[Word], server: &[String], depth: usize, nested: bool, out: &mut Vec<Hit>) {
    let mut argv: Vec<Word> = Vec::new();
    for word in words {
        let text = word.text.as_str();
        if let Some(head) = text.strip_suffix(';').filter(|_| !text.ends_with("\\;")) {
            if !head.is_empty() {
                argv.push(Word {
                    text: head.to_string(),
                    dynamic: word.dynamic,
                });
            }
            command(&std::mem::take(&mut argv), server, depth, nested, out);
        } else {
            argv.push(word.clone());
        }
    }
    command(&argv, server, depth, nested, out);
}

/// Judge one tmux command; `nested` when it came from inside an argument.
fn command(argv: &[Word], server: &[String], depth: usize, nested: bool, out: &mut Vec<Hit>) {
    let Some((head, args)) = argv.split_first() else {
        return;
    };
    if head.dynamic {
        if !nested {
            out.push(Hit::opaque("", server, "a command the shell expands"));
        }
        return;
    }
    match resolve(&head.text, nested) {
        Resolved::Unknown if nested => {
            // #8902: `command-alias[N] name=kill-server` defines a deny verb.
            let aliased = head.text.split_once('=').map(|(_, v)| resolve(v, false));
            if matches!(aliased, Some(Resolved::Deny(_) | Resolved::Opaque)) {
                out.push(Hit::opaque(&head.text, server, "an alias for a deny verb"));
            }
            return;
        }
        Resolved::Unknown => {
            out.push(Hit::opaque(
                &head.text,
                server,
                "an unknown command, possibly an alias",
            ));
            return;
        }
        Resolved::Opaque => out.push(Hit::opaque(
            &head.text,
            server,
            "it runs commands the guard cannot read",
        )),
        Resolved::Deny(verb) => match deny_targets(verb, args) {
            Ok(Some(targets)) => out.push(Hit {
                verb: verb.name.to_string(),
                server: server.to_vec(),
                targets,
                opaque: None,
            }),
            Ok(None) => {}
            Err(why) => out.push(Hit::opaque(verb.name, server, why)),
        },
        Resolved::Known => {}
    }
    if depth >= MAX_DEPTH {
        return;
    }
    for word in args.iter().filter(|w| !w.dynamic) {
        shell(&word.text, depth + 1, out);
        if let Some(inner) = shlex::split(&word.text) {
            let inner: Vec<Word> = inner
                .into_iter()
                .map(|text| Word {
                    dynamic: text.contains(['$', '`']) || text.contains("#{"),
                    text,
                })
                .collect();
            commands(&inner, server, depth + 1, true, out);
        }
    }
    for start in 1..args.len().min(MAX_SUFFIXES) {
        commands(&args[start..], server, depth + 1, true, out);
    }
}

/// The targets a deny-verb command can reach; `Ok(None)` when it reaches none
/// (`new-window` without `-k`), `Err` when its options cannot be read.
fn deny_targets(verb: &Verb, args: &[Word]) -> Result<Option<Vec<Target>>, &'static str> {
    let flags = parse_flags(verb.opts, args)?;
    let has = |c: char| flags.iter().any(|(f, _)| *f == c);
    let values = |c: char| -> Vec<Target> {
        flags
            .iter()
            .filter(|(f, _)| *f == c)
            .filter_map(|(_, v)| v.as_ref())
            .map(|w| {
                if w.dynamic {
                    Target::Dynamic(w.text.clone())
                } else {
                    Target::Literal(w.text.clone())
                }
            })
            .collect()
    };
    if verb.name == "new-window" && !has('k') {
        return Ok(None);
    }
    if verb.name == "send-keys" && has('c') {
        return Err("`-c` sends keys through a client whose pane is unknown");
    }
    if verb.name == "kill-server" || (verb.name == "kill-session" && has('a')) {
        return Ok(Some(vec![Target::Server]));
    }
    let mut targets = values('t');
    if targets.is_empty() {
        targets.push(Target::Current);
    }
    if verb.src {
        let sources = values('s');
        if sources.is_empty() {
            targets.extend([Target::Current, Target::Marked]);
        }
        targets.extend(sources);
    }
    Ok(Some(targets))
}

/// A tmux getopt parse of `args` against `opts`: each flag and its value.
fn parse_flags(opts: &str, args: &[Word]) -> Result<Vec<(char, Option<Word>)>, &'static str> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(word) = args.get(i) {
        let text = word.text.as_str();
        if text == "--" || !text.starts_with('-') || text == "-" {
            break;
        }
        if word.dynamic {
            return Err("an option the shell expands");
        }
        for (k, c) in text.char_indices().skip(1) {
            let at = opts
                .find(c)
                .filter(|_| c != ':')
                .ok_or("an option this guard does not know")?;
            if opts[at + 1..].starts_with(':') {
                let inline = &text[k + 1..];
                let value = if inline.is_empty() {
                    i += 1;
                    args.get(i).cloned().ok_or("an option has no value")?
                } else {
                    Word {
                        text: inline.to_string(),
                        dynamic: false,
                    }
                };
                out.push((c, Some(value)));
                break;
            }
            out.push((c, None));
        }
        i += 1;
    }
    Ok(out)
}

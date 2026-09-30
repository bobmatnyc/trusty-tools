//! The tmux commands a Bash command runs, for the Architect pane floor (#8902).
//!
//! Why: the floor judges tmux verbs and their targets, so it needs every tmux
//! command a Bash command can run, and each target exactly as tmux reads it.
//! What: [`tmux_hits`] walks the command's segments (through `sh -c`, `env -S`,
//! `xargs` and `eval` wrappers) and its command substitutions, finds each
//! `tmux` program word past the shared wrappers (`program_word`), and parses
//! the global options and each `;`-separated command. A verb in [`DENY_VERBS`]
//! yields a [`Hit`] with its targets. Every argument of a known command is
//! also read as a shell command, as a tmux command string, and as the start of
//! a tmux argv — the forms `run-shell`, `if-shell`, `bind-key`, `new-window`
//! and typed keys carry. An omitted target there is read in its [`Context`],
//! and a nested `tmux` with no `-L`/`-S` reaches that context's server — in
//! typed keys, also the default one.
//! FAIL-CLOSED: see [`Hit::opaque`].
//! Test: `architect_pane_tests.rs`.

use super::architect_pane_env::{
    ENV_RESET, RELATIVE_SOCKET, SERVER_ENV, assigns_dynamic_name, moves_server_env, resets_env,
};
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
    /// An omitted `-t` of a `tmux` in typed keys that reaches a server other
    /// than the one the keys go to: a pane that server picks (#8902).
    Picked,
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
    /// The same command renames or creates a session: the hit denies when its
    /// server holds an Architect pane (`architect_pane_env::RETARGET`).
    pub(super) retargeted: bool,
}

impl Hit {
    fn opaque(verb: &str, server: &[String], why: &'static str) -> Self {
        Self {
            verb: verb.to_string(),
            server: server.to_vec(),
            targets: Vec::new(),
            opaque: Some(why),
            retargeted: false,
        }
    }
}

/// Where a tmux command runs, which decides what an omitted target means.
#[derive(Debug, Clone)]
enum Context {
    /// The caller's own shell: an omitted `-t` is the caller's pane.
    Caller,
    /// Keys `send-keys` types into these targets, on this server: an omitted
    /// `-t` is one of them.
    Keys(Vec<Target>, Vec<String>),
    /// A command this server runs in a pane it picks (`bind-key`, `set-hook`,
    /// `if-shell`, `run-shell`, a new window): an omitted `-t` is unknown.
    Deferred(Vec<String>),
}

impl Context {
    /// The server a nested `tmux` with no `-L`/`-S` reaches: its `TMUX` names
    /// the server that runs it (#8902). The caller's shell inherits none; in
    /// typed keys `options_then_commands` also judges the default server.
    fn server(&self) -> &[String] {
        match self {
            Self::Caller => &[],
            Self::Keys(_, server) | Self::Deferred(server) => server,
        }
    }
}

/// The walk's state: the hits so far, and what the rest of the walk needs.
struct Scan {
    hits: Vec<Hit>,
    /// A `rename-session` or `new-session` runs somewhere in the command.
    retargets: bool,
    /// An assignment builtin runs on a name the shell expands (#8902).
    dynamic_env: bool,
    context: Context,
}

/// Every deny-set or unreadable tmux command `command` runs.
///
/// What: see the module doc. #8902 review: the pane list is read before the
/// command runs, on the server the hook process sees. A command that changes
/// `TMUX`/`TMUX_TMPDIR` makes every hit opaque; one that renames or creates
/// a session marks every hit [`Hit::retargeted`].
/// Test: `every_deny_verb_is_denied_in_each_target_form`,
/// `a_tmux_command_reached_through_a_wrapper_or_argument_is_found`,
/// `a_server_the_command_selects_differently_denies`,
/// `a_target_the_same_command_retargets_denies`,
/// `a_nested_invocation_inherits_the_outer_server`,
/// `a_nested_tmux_in_typed_keys_is_judged_on_the_default_server_too`,
/// `a_tmux_assignment_inside_a_word_or_through_an_unread_name_denies`.
pub(super) fn tmux_hits(command: &str) -> Vec<Hit> {
    let mut scan = Scan {
        hits: Vec::new(),
        retargets: false,
        dynamic_env: false,
        context: Context::Caller,
    };
    shell(command, 0, &mut scan);
    let moved = scan.dynamic_env || moves_server_env(command);
    for hit in &mut scan.hits {
        if moved {
            hit.opaque.get_or_insert(SERVER_ENV);
        }
        hit.retargeted = scan.retargets;
    }
    scan.hits
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
fn shell(command: &str, depth: usize, out: &mut Scan) {
    let command = &quote_escaped_semicolons(command);
    // #8902: `$'\x74mux'` hides the program name itself, so at the top level
    // any command the guard cannot classify counts.
    if unclassifiable_command(command).is_some() && (depth == 0 || may_run_tmux(command, depth)) {
        out.hits
            .push(Hit::opaque("", &[], "the command does not parse"));
        return;
    }
    for seg in segments(command) {
        let Some(argv) = shlex::split(seg.text.trim()) else {
            if may_run_tmux(&seg.text, depth) {
                out.hits
                    .push(Hit::opaque("", &[], "the command does not parse"));
            }
            continue;
        };
        let words = with_dynamics(&seg.text, argv);
        let texts: Vec<String> = words.iter().map(|w| w.text.clone()).collect();
        for (pos, base) in program_positions(&texts) {
            out.dynamic_env |= assigns_dynamic_name(&texts[pos..]);
            if base == "tmux" {
                let reset = resets_env(&texts[..pos]);
                invocation(&words[pos + 1..], depth, reset, out);
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
/// `reset` when `sudo`, `doas` or `env -i` runs it: every hit is opaque.
fn invocation(words: &[Word], depth: usize, reset: bool, out: &mut Scan) {
    let start = out.hits.len();
    let unknown = options_then_commands(words, depth, out);
    if let Some(why) = reset.then_some(ENV_RESET).or(unknown) {
        for hit in &mut out.hits[start..] {
            hit.opaque.get_or_insert(why);
        }
    }
}

/// The body of [`invocation`]; `Some(why)` when the server the global options
/// select cannot be resolved.
fn options_then_commands(words: &[Word], depth: usize, out: &mut Scan) -> Option<&'static str> {
    let mut server: Vec<String> = out.context.server().to_vec();
    let mut own_server = false;
    let mut unknown = None;
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
            out.hits
                .push(Hit::opaque("", &server, "an option the shell expands"));
            return None;
        }
        for (k, c) in text.char_indices().skip(1) {
            if c == 'C' {
                out.hits.push(Hit::opaque(
                    "-C",
                    &server,
                    "control mode reads its commands from stdin",
                ));
                return None;
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
                out.hits
                    .push(Hit::opaque("", &server, "an option has no value"));
                return None;
            };
            if matches!(c, 'L' | 'S') {
                if value.dynamic {
                    out.hits
                        .push(Hit::opaque("", &server, "a server the shell expands"));
                    return None;
                }
                // #8902 review: the probe cannot know the directory it is in.
                if c == 'S' && !value.text.starts_with('/') {
                    unknown = Some(RELATIVE_SOCKET);
                }
                if !std::mem::replace(&mut own_server, true) {
                    server.clear();
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
    // #8902 follow-up: the pane typed into may run a process with no `TMUX`,
    // whose `tmux` reaches the default server, so judge it there too.
    if !own_server && !server.is_empty() && matches!(out.context, Context::Keys(..)) {
        commands(&words[i..], &[], depth, false, out);
    }
    unknown
}

/// Split `words` on tmux's `;` separators and judge each command.
fn commands(words: &[Word], server: &[String], depth: usize, nested: bool, out: &mut Scan) {
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
fn command(argv: &[Word], server: &[String], depth: usize, nested: bool, out: &mut Scan) {
    let Some((head, args)) = argv.split_first() else {
        return;
    };
    if head.dynamic {
        if !nested {
            out.hits
                .push(Hit::opaque("", server, "a command the shell expands"));
        }
        return;
    }
    if retargets(&head.text, args, &out.context) {
        out.retargets = true;
    }
    // Keys `send-keys` types run in its target panes; other arguments run
    // where tmux picks.
    let mut inner = Context::Deferred(server.to_vec());
    match resolve(&head.text, nested) {
        Resolved::Unknown if nested => {
            // #8902: `command-alias[N] name=kill-server` defines a deny verb.
            let aliased = head.text.split_once('=').map(|(_, v)| resolve(v, false));
            if matches!(aliased, Some(Resolved::Deny(_) | Resolved::Opaque)) {
                out.hits
                    .push(Hit::opaque(&head.text, server, "an alias for a deny verb"));
            }
            return;
        }
        Resolved::Unknown => {
            out.hits.push(Hit::opaque(
                &head.text,
                server,
                "an unknown command, possibly an alias",
            ));
            return;
        }
        Resolved::Opaque => out.hits.push(Hit::opaque(
            &head.text,
            server,
            "it runs commands the guard cannot read",
        )),
        Resolved::Deny(verb) => {
            let found = deny_targets(verb, args)
                .and_then(|t| t.map(|t| in_context(t, &out.context, server)).transpose());
            match found {
                Ok(Some(targets)) => {
                    if verb.name == "send-keys" {
                        inner = Context::Keys(targets.clone(), server.to_vec());
                    }
                    out.hits.push(Hit {
                        verb: verb.name.to_string(),
                        server: server.to_vec(),
                        targets,
                        opaque: None,
                        retargeted: false,
                    });
                }
                Ok(None) => {}
                Err(why) => out.hits.push(Hit::opaque(verb.name, server, why)),
            }
        }
        Resolved::Known => {}
    }
    if depth >= MAX_DEPTH {
        return;
    }
    let outer = std::mem::replace(&mut out.context, inner);
    arguments(args, server, depth, out);
    out.context = outer;
}

/// Read each argument of a known tmux command as shell text, as a tmux
/// command string, and as the start of a tmux argv.
fn arguments(args: &[Word], server: &[String], depth: usize, out: &mut Scan) {
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

/// Whether `head` runs `rename-session` or `new-session`, whose session the
/// pane list read before the command does not hold. In typed keys, only with
/// an option, so a message that starts with "new" or "rename" is not one.
fn retargets(head: &str, args: &[Word], context: &Context) -> bool {
    // tmux takes an alias or a unique prefix: `rename-s`, `new-s`.
    let named = |full: &str, alias: &str, unique: usize| {
        head == alias || (head.len() >= unique && full.starts_with(head))
    };
    let verb = named("rename-session", "rename", 8) || named("new-session", "new", 5);
    verb && (!matches!(context, Context::Keys(..)) || args.iter().any(|w| w.text.starts_with('-')))
}

/// `targets` with an omitted target read in `context`.
///
/// What: the caller's pane in the caller's shell; the `send-keys` targets in
/// typed keys on their own `server`, else [`Target::Picked`]; `Err` in a
/// command tmux runs later or in a pane it picks.
fn in_context(
    targets: Vec<Target>,
    context: &Context,
    server: &[String],
) -> Result<Vec<Target>, &'static str> {
    let defaulted = |t: &Target| matches!(t, Target::Current | Target::Marked);
    match context {
        Context::Caller => Ok(targets),
        Context::Deferred(_) if targets.iter().any(defaulted) => {
            Err("a nested command with no target acts on a pane tmux picks when it runs")
        }
        Context::Deferred(_) => Ok(targets),
        // #8902 follow-up LOW: the pane typed into is not on this server.
        Context::Keys(typed_into, keys) => Ok(targets
            .into_iter()
            .flat_map(|t| match t {
                // #8902 follow-up LOW: the pane typed into is not on this server.
                Target::Current if keys.as_slice() != server => vec![Target::Picked],
                Target::Current => typed_into.clone(),
                t => vec![t],
            })
            .collect()),
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

//! The tmux command table of the Architect pane floor (#8902).
//!
//! Why: the floor must resolve a command word exactly as tmux does — name or
//! alias, then a unique prefix — and must know an unknown word is unknown.
//! What: [`DENY_VERBS`] (the verb decisions), the other tmux 3.6 commands,
//! and [`resolve`].
//! Test: `every_deny_verb_is_denied_in_each_target_form`,
//! `a_non_architect_target_and_a_read_verb_pass`.

/// A tmux verb this floor judges, with its tmux(1) option letters.
#[derive(Debug)]
pub(super) struct Verb {
    /// The command name.
    pub(super) name: &'static str,
    /// Its tmux alias, or `""`.
    pub(super) alias: &'static str,
    /// getopt letters; a letter followed by `:` takes a value.
    pub(super) opts: &'static str,
    /// Whether `-s` names a source pane or window.
    pub(super) src: bool,
}

const fn verb(name: &'static str, alias: &'static str, opts: &'static str, src: bool) -> Verb {
    Verb {
        name,
        alias,
        opts,
        src,
    }
}

/// The verbs that type into, kill, respawn, swap, join, break, move, link or
/// clear a pane, window or session (#8902 verb decisions). `new-window`
/// counts only with `-k`, which kills the window at the target index.
/// Allowed, not listed: `select-layout`, `resize-pane` (`-Z` included) and
/// `resize-window` change geometry only, and tmux never sizes a pane below
/// one cell; `capture-pane` and the other read verbs change nothing.
pub(super) const DENY_VERBS: &[Verb] = &[
    verb("send-keys", "send", "c:FHKlMN:Rt:X", false),
    verb("send-prefix", "", "2t:", false),
    verb("paste-buffer", "pasteb", "db:prs:t:", false),
    verb("pipe-pane", "pipep", "IOot:", false),
    verb("kill-pane", "killp", "at:", false),
    verb("kill-window", "killw", "at:", false),
    verb("kill-session", "", "aCt:", false),
    verb("kill-server", "", "", false),
    verb("respawn-pane", "respawnp", "c:e:kt:", false),
    verb("respawn-window", "respawnw", "c:e:kt:", false),
    verb("swap-pane", "swapp", "dDs:t:UZ", true),
    verb("swap-window", "swapw", "ds:t:", true),
    verb("join-pane", "joinp", "bdfhvp:l:s:t:", true),
    verb("move-pane", "movep", "bdfhvp:l:s:t:", true),
    verb("break-pane", "breakp", "abdPF:n:s:t:", true),
    verb("move-window", "movew", "abdkrs:t:", true),
    verb("link-window", "linkw", "abdks:t:", true),
    verb("unlink-window", "unlinkw", "kt:", false),
    verb("clear-history", "clearhist", "Ht:", false),
    verb("new-window", "neww", "abc:de:F:kn:PSt:", false),
];

/// Every other tmux 3.6 command and alias, so an unknown word is known to be
/// unknown (a `command-alias`, which could name any verb).
#[rustfmt::skip]
const OTHER_VERBS: &[&str] = &[
    "attach-session",
    "attach",
    "bind-key",
    "bind",
    "capture-pane",
    "capturep",
    "choose-buffer",
    "choose-client",
    "choose-tree",
    "clear-prompt-history",
    "clearphist",
    "clock-mode",
    "command-prompt",
    "confirm-before",
    "confirm",
    "copy-mode",
    "customize-mode",
    "delete-buffer",
    "deleteb",
    "detach-client",
    "detach",
    "display-menu",
    "menu",
    "display-message",
    "display",
    "display-popup",
    "popup",
    "display-panes",
    "displayp",
    "find-window",
    "findw",
    "has-session",
    "has",
    "if-shell",
    "if",
    "last-pane",
    "lastp",
    "last-window",
    "last",
    "list-buffers",
    "lsb",
    "list-clients",
    "lsc",
    "list-commands",
    "lscm",
    "list-keys",
    "lsk",
    "list-panes",
    "lsp",
    "list-sessions",
    "ls",
    "list-windows",
    "lsw",
    "load-buffer",
    "loadb",
    "lock-client",
    "lockc",
    "lock-server",
    "lock",
    "lock-session",
    "locks",
    "new-session",
    "new",
    "next-layout",
    "nextl",
    "next-window",
    "next",
    "previous-layout",
    "prevl",
    "previous-window",
    "prev",
    "refresh-client",
    "refresh",
    "rename-session",
    "rename",
    "rename-window",
    "renamew",
    "resize-pane",
    "resizep",
    "resize-window",
    "resizew",
    "rotate-window",
    "rotatew",
    "run-shell",
    "run",
    "save-buffer",
    "saveb",
    "select-layout",
    "selectl",
    "select-pane",
    "selectp",
    "select-window",
    "selectw",
    "server-access",
    "set-buffer",
    "setb",
    "set-environment",
    "setenv",
    "set-hook",
    "set-option",
    "set",
    "set-window-option",
    "setw",
    "show-buffer",
    "showb",
    "show-environment",
    "showenv",
    "show-hooks",
    "show-messages",
    "showmsgs",
    "show-options",
    "show",
    "show-prompt-history",
    "showphist",
    "show-window-options",
    "showw",
    "split-window",
    "splitw",
    "start-server",
    "start",
    "suspend-client",
    "suspendc",
    "switch-client",
    "switchc",
    "unbind-key",
    "unbind",
    "wait-for",
    "wait",
    "split-pane",
    "splitp",
    "server-info",
    "info",
    "choose-window",
    "choose-session",
];

/// Commands that run tmux commands this guard cannot read.
const OPAQUE_VERBS: &[&str] = &["source-file", "source"];

/// What a command word resolves to, as tmux resolves it.
pub(super) enum Resolved {
    Deny(&'static Verb),
    Known,
    Opaque,
    Unknown,
}

/// Shortest unique prefix read as a verb inside an argument, where one-letter
/// keys (`n`, `j`) are typed text far more often than tmux abbreviations.
const MIN_NESTED_PREFIX: usize = 4;

/// Resolve `name` as tmux does: exact name or alias first, then a unique name
/// prefix. An ambiguous prefix is [`Resolved::Known`]: tmux refuses it.
pub(super) fn resolve(name: &str, nested: bool) -> Resolved {
    if let Some(v) = DENY_VERBS
        .iter()
        .find(|v| v.name == name || v.alias == name)
    {
        return Resolved::Deny(v);
    }
    if OPAQUE_VERBS.contains(&name) {
        return Resolved::Opaque;
    }
    if OTHER_VERBS.contains(&name) {
        return Resolved::Known;
    }
    if name.is_empty() || (nested && name.len() < MIN_NESTED_PREFIX) {
        return Resolved::Unknown;
    }
    let deny: Vec<&'static Verb> = DENY_VERBS
        .iter()
        .filter(|v| v.name.starts_with(name))
        .collect();
    let others = OTHER_VERBS
        .iter()
        .chain(OPAQUE_VERBS)
        .filter(|v| v.starts_with(name));
    match (deny.as_slice(), others.count()) {
        ([only], 0) => Resolved::Deny(only),
        ([], 0) => Resolved::Unknown,
        _ => Resolved::Known,
    }
}

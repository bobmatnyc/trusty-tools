//! Finding `tmux send-keys` relays in a Bash command (#9167).
//!
//! Why: a Bash call that types text into another session's pane is an
//! inter-session message, and the `PreToolUse` hook carries the command as
//! text, not as a parsed argv. A call that only names `send-keys` inside
//! another command's quoted argument must not count as a relay.
//! What: [`send_keys_calls`] splits the command into simple commands with the
//! worktree ledger's quote-aware splitter, finds each `tmux` invocation, skips
//! tmux's global options, splits tmux's own `;`-chained command list, and reads
//! each `send-keys` (alias `send`) call's `-t` target and key arguments. The
//! text is the arguments typed back to back, as tmux types them. Without `-l`,
//! an argument that is a tmux key name (`Enter`, `C-m`, `Escape`, ...) is a
//! key press, not text. A `-X` call (a copy-mode command) or a call that types
//! no text is not a message. Shell expansions are not evaluated: `$VAR` is
//! its literal text.
//! Test: `message_tests::send_keys_text_follows_tmux_argument_rules_9167`,
//! `message_tests::a_tmux_send_keys_relay_names_its_pane_9167`.

use std::ffi::OsStr;
use std::path::Path;

use crate::core::worktree_ledger::bash_worktree::{segments, strip_prefix_words};

/// tmux global options that take the next word as their value.
const TMUX_VALUE_OPTS: &[char] = &['L', 'S', 'f', 'c', 'T'];

/// `send-keys` options that take the next word as their value.
const SEND_VALUE_OPTS: &[char] = &['t', 'N', 'c'];

/// tmux key names (matched case-insensitively), besides `F1`..`F24`.
const KEY_NAMES: &[&str] = &[
    "enter", "escape", "tab", "btab", "space", "bspace", "up", "down", "left", "right", "home",
    "end", "ic", "dc", "insert", "delete", "pageup", "pgup", "pagedown", "pgdn", "npage", "ppage",
];

/// One `tmux send-keys` call that types text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendKeys {
    /// The `-t` target as written; `None` is tmux's current pane.
    pub target: Option<String>,
    /// The text the call types: its non-key arguments, back to back.
    pub text: String,
}

/// The `tmux send-keys` calls in `command` that type text, in command order.
///
/// Why: see the module doc.
/// What: an unbalanced quote leaves its segment unread rather than guessed.
/// Test: `message_tests::send_keys_text_follows_tmux_argument_rules_9167`.
pub fn send_keys_calls(command: &str) -> Vec<SendKeys> {
    let mut out = Vec::new();
    for segment in segments(command) {
        let Some(words) = shlex::split(&segment) else {
            continue;
        };
        let words = strip_prefix_words(&words);
        let Some((program, rest)) = words.split_first() else {
            continue;
        };
        if Path::new(program).file_name() != Some(OsStr::new("tmux")) {
            continue;
        }
        let (_, commands) = options(rest, TMUX_VALUE_OPTS);
        out.extend(commands.split(|w| w == ";").filter_map(send_keys));
    }
    out
}

/// One tmux command (its words after the global options), if it is a
/// `send-keys` call that types text.
fn send_keys(words: &[String]) -> Option<SendKeys> {
    let (name, args) = words.split_first()?;
    if name != "send-keys" && name != "send" {
        return None;
    }
    let (opts, keys) = options(args, SEND_VALUE_OPTS);
    if opts.flags.contains(&'X') {
        return None;
    }
    let literal = opts.flags.contains(&'l');
    let text: String = keys
        .iter()
        .filter(|k| literal || !is_key_name(k))
        .map(String::as_str)
        .collect();
    if text.is_empty() {
        return None;
    }
    let target = opts
        .values
        .iter()
        .rev()
        .find(|(flag, _)| *flag == 't')
        .map(|(_, value)| value.clone());
    Some(SendKeys { target, text })
}

/// Flags and valued options read getopt-style from the front of `args`.
#[derive(Default)]
struct Opts {
    flags: Vec<char>,
    values: Vec<(char, String)>,
}

/// Read getopt-style options from `args`; return them and the words after.
///
/// What: a word `-abc` is a cluster of flags; a flag in `takes_value` takes
/// the rest of its word, else the next word, as its value. `--` ends the
/// options; so does the first word that does not start with `-`.
fn options<'a>(args: &'a [String], takes_value: &[char]) -> (Opts, &'a [String]) {
    let mut opts = Opts::default();
    let mut i = 0;
    while let Some(word) = args.get(i) {
        if word == "--" {
            i += 1;
            break;
        }
        let Some(cluster) = word.strip_prefix('-').filter(|c| !c.is_empty()) else {
            break;
        };
        i += 1;
        for (at, flag) in cluster.char_indices() {
            if takes_value.contains(&flag) {
                let inline = &cluster[at + flag.len_utf8()..];
                if inline.is_empty() {
                    if let Some(value) = args.get(i) {
                        opts.values.push((flag, value.clone()));
                    }
                    i += 1;
                } else {
                    opts.values.push((flag, inline.to_string()));
                }
                break;
            }
            opts.flags.push(flag);
        }
    }
    (opts, args.get(i..).unwrap_or_default())
}

/// Whether tmux reads `arg` as a key press rather than as text.
///
/// What: a name from [`KEY_NAMES`] or `F1`..`F24`, optionally behind `C-`,
/// `M-`, `S-` or `^` modifiers; a modified single character is a key too.
fn is_key_name(arg: &str) -> bool {
    let mut rest = arg;
    let mut modified = false;
    while let Some(next) = ["C-", "M-", "S-", "^"]
        .iter()
        .find_map(|m| rest.strip_prefix(m))
        .filter(|n| !n.is_empty())
    {
        rest = next;
        modified = true;
    }
    let lower = rest.to_ascii_lowercase();
    let function_key = lower
        .strip_prefix('f')
        .and_then(|n| n.parse::<u8>().ok())
        .is_some_and(|n| (1..=24).contains(&n));
    KEY_NAMES.contains(&lower.as_str()) || function_key || (modified && rest.chars().count() == 1)
}

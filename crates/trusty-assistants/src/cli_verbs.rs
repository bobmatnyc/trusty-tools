//! Per-verb help and `ta list` for the `ta` command line (#11742).
//!
//! Why: `ta ask --help` and `ta handoff --help` failed as usage errors, and
//! no command named the instances `ta ask` accepts. Kept apart from `cli` so
//! that file stays under the size cap.
//! What: [`verb_usage`] holds each hand-parsed verb's usage text,
//! [`wants_help`] decides when a verb's argv asks for it, and [`run_list`]
//! prints the instance homes.
//! Test: `subcommand_help_exits_zero_and_prints_usage`
//! (`tests/help_exit_code.rs`), `list_names_only_instance_homes`
//! (`tests/instance_list.rs`).

use anyhow::{Context, Result};

/// `ta ask`'s usage text.
const ASK_USAGE: &str = concat!(
    "Usage: ",
    env!("TA_BIN_NAME"),
    " ask <instance> <question…>\n\n\
Ask an assistant instance one question and print its answer on stdout.\n\
The words after <instance> are joined into one question. The instance's\n\
daemon starts on demand; a cold start prints one progress line on stderr.\n\
The daemon exits when idle. Instances live in ~/trusty-agents/<instance>/;\n`",
    env!("TA_BIN_NAME"),
    " list` names them.\n\n\
Options:\n  \
-h, --help  Print this help. Only `ask --help` or `ask <instance> --help`\n              \
with nothing after it; anywhere else it is question text\n  \
--          Before the question: every later word is question text, so\n              \
`ask <instance> -- --help` asks \"--help\"\n",
);

/// `ta handoff`'s usage text.
const HANDOFF_USAGE: &str = concat!(
    "Usage: ",
    env!("TA_BIN_NAME"),
    " handoff <instance> --to tm|tc [--key <key>] [--] <task…>\n\n\
Hand one task from an assistant instance to trusty-mpm (tm) or trusty-code\n\
(tc). Prints one JSON line: the key, the target and the target's handle.\n\n\
Options:\n  \
--to tm|tc   Required; there is no default. tm sends into the instance's\n               \
durable `tm serve --stdio` session; tc runs `task.run` on the\n               \
shared tcode socket\n  \
--key <key>  Idempotency key; the same key again replays the first handoff.\n               \
Omitted, a new key is minted and printed\n  \
--           End of options; every later word is task text\n  \
-h, --help   Print this help\n",
);

/// `ta list`'s usage text.
const LIST_USAGE: &str = concat!(
    "Usage: ",
    env!("TA_BIN_NAME"),
    " list\n\n\
Print the assistant instances, one id per line. An instance is a directory\n\
under ~/trusty-agents/ (or $TA_ASSISTANTS_DIR) that holds instructions.md.\n\n\
Options:\n  -h, --help  Print this help\n",
);

/// `ta mcp-serve`'s usage text.
const MCP_SERVE_USAGE: &str = concat!(
    "Usage: ",
    env!("TA_BIN_NAME"),
    " mcp-serve\n\n\
Serve the dispatch_task tool as an MCP server on stdin and stdout.\n\n\
Options:\n  -h, --help  Print this help\n",
);

/// `ta ask-daemon`'s usage text.
const ASK_DAEMON_USAGE: &str = concat!(
    "Usage: ",
    env!("TA_BIN_NAME"),
    " ask-daemon <instance>\n\n\
Run one instance's ask daemon. `",
    env!("TA_BIN_NAME"),
    " ask` starts it on demand; it exits when idle.\n\n\
Options:\n  -h, --help  Print this help\n",
);

/// A verb whose argv `ta` parses by hand and so owns its help text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verb {
    Ask,
    AskDaemon,
    Handoff,
    List,
    McpServe,
}

/// The usage text `<verb> --help` prints.
/// Test: `subcommand_help_exits_zero_and_prints_usage` (`tests/help_exit_code.rs`).
pub(crate) fn verb_usage(verb: Verb) -> &'static str {
    match verb {
        Verb::Ask => ASK_USAGE,
        Verb::AskDaemon => ASK_DAEMON_USAGE,
        Verb::Handoff => HANDOFF_USAGE,
        Verb::List => LIST_USAGE,
        Verb::McpServe => MCP_SERVE_USAGE,
    }
}

/// Whether a verb's argv tail asks for help.
///
/// Why (#11742): a help word inside a question or task text is that text,
/// not a request for usage; only the flag region may ask for help.
/// What: `ask` — the first word, or the word after the instance when it is
/// the last word. `handoff` — the instance slot or a word in the flag
/// region, which ends at `--` or the first word that is not `--to`/`--key`
/// (with their values). Other verbs take no free text: any word before `--`.
/// Test: `cli::tests::help_words_before_the_terminator_ask_for_help`.
pub(crate) fn wants_help(verb: Verb, tail: &[String]) -> bool {
    let is_help = |w: &String| w == "--help" || w == "-h";
    match verb {
        Verb::Ask => match tail {
            [first, ..] if is_help(first) => true,
            [_, second] => is_help(second),
            _ => false,
        },
        Verb::Handoff => {
            let Some((instance, mut words)) = tail.split_first() else {
                return false;
            };
            if is_help(instance) {
                return true;
            }
            loop {
                match words {
                    [w, ..] if is_help(w) => return true,
                    [w, _, ..] if w == "--to" || w == "--key" => words = &words[2..],
                    [w, ..] if w.starts_with("--to=") || w.starts_with("--key=") => {
                        words = &words[1..];
                    }
                    _ => return false,
                }
            }
        }
        Verb::AskDaemon | Verb::List | Verb::McpServe => {
            tail.iter().take_while(|w| w.as_str() != "--").any(is_help)
        }
    }
}

/// `ta list`: print each instance id on stdout, one per line.
///
/// Why (#11742): the names `ta ask` accepts were visible nowhere.
/// What: lists [`crate::assistants::list_instances`] under
/// [`crate::assistants::assistants_root`]. With none, prints one line on
/// stderr naming the root and exits 0, so stdout stays a plain list.
/// Test: `list_names_only_instance_homes`, `list_on_an_empty_root_says_so`
/// (`tests/instance_list.rs`).
pub(crate) fn run_list() -> Result<()> {
    let root = crate::assistants::assistants_root()?;
    let ids = crate::assistants::list_instances(&root)
        .with_context(|| format!("cannot read the assistants directory {}", root.display()))?;
    if ids.is_empty() {
        eprintln!(
            "no assistant instances under {}; an instance is a directory there holding {}",
            root.display(),
            crate::assistants::INSTRUCTIONS_FILE
        );
    }
    for id in ids {
        println!("{id}");
    }
    Ok(())
}

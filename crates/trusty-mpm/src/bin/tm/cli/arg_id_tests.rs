//! Clap arg-id collision guard and `tm register` parse tests (#10569).
//!
//! Why: clap keys every parsed value by arg id. A subcommand arg sharing an
//! id with a `global = true` arg on an ancestor writes its value into the
//! global field. `Command::Register`'s positional was id `url`, so
//! `tm register owner/repo` filled `Cli.url` and the #6288 remote-URL refusal
//! fired on every register.
//! What: parses each `tm register` form through `Cli::try_parse_from` and
//! asserts `cli.url` stays untouched, then walks the whole command tree and
//! asserts no subcommand arg reuses an inherited global arg id, apart from
//! the listed same-flag re-declarations.
//! Test: this file; the binary-level proof is `tests/tm_register_url_id_10569.rs`.

use clap::{CommandFactory, Parser};

use crate::cli::{Cli, Command};

/// Parse `argv` and return `(cli.url, register repo spec, register alias)`.
fn parse_register(argv: &[&str]) -> (Option<String>, String, Option<String>) {
    let cli = Cli::try_parse_from(argv).expect("tm register parses");
    match cli.command {
        Some(Command::Register {
            url: repo_spec,
            alias,
            ..
        }) => (cli.url, repo_spec, alias),
        other => panic!("expected Command::Register, got {other:?}"),
    }
}

#[test]
fn register_owner_repo_does_not_fill_cli_url() {
    let (url, spec, alias) = parse_register(&["tm", "register", "bobmatnyc/trusty-tools"]);
    assert!(url.is_none(), "positional leaked into Cli.url: {url:?}");
    assert_eq!(spec, "bobmatnyc/trusty-tools");
    assert_eq!(alias, None);
}

#[test]
fn register_full_url_does_not_fill_cli_url() {
    let input = "https://github.com/bobmatnyc/trusty-tools";
    let (url, spec, alias) = parse_register(&["tm", "register", input]);
    assert!(url.is_none(), "positional leaked into Cli.url: {url:?}");
    assert_eq!(spec, input);
    assert_eq!(alias, None);
}

#[test]
fn register_owner_repo_with_alias_does_not_fill_cli_url() {
    let (url, spec, alias) =
        parse_register(&["tm", "register", "bobmatnyc/trusty-tools", "mytools"]);
    assert!(url.is_none(), "positional leaked into Cli.url: {url:?}");
    assert_eq!(spec, "bobmatnyc/trusty-tools");
    assert_eq!(alias.as_deref(), Some("mytools"));
}

#[test]
fn register_legacy_alias_then_url_does_not_fill_cli_url() {
    let input = "https://github.com/bobmatnyc/trusty-tools";
    let (url, spec, alias) = parse_register(&["tm", "register", "mytools", input]);
    assert!(url.is_none(), "positional leaked into Cli.url: {url:?}");
    // The legacy order is untangled later by `resolve_register_args`; parsing
    // keeps the positionals in the order given.
    assert_eq!(spec, "mytools");
    assert_eq!(alias.as_deref(), Some(input));
}

#[test]
fn register_keeps_an_explicit_global_url() {
    let (url, spec, _) =
        parse_register(&["tm", "--url", "http://127.0.0.1:7880", "register", "x/y"]);
    assert_eq!(url.as_deref(), Some("http://127.0.0.1:7880"));
    assert_eq!(spec, "x/y");
}

/// Subcommand flags that re-declare a global flag under its own `--long`.
///
/// clap cannot give these a separate id: the global is propagated into every
/// subcommand, and two args spelled `--url` in one subcommand fail
/// `debug_assert`. Each still writes the global field. `tui`, `slack start`
/// and `telegram start` take the same `--url`/`TRUSTY_MPM_URL` the global
/// does. `env set --account` is a Keychain account, not a `gh` login, and is
/// reported on #10569 rather than renamed here: the pm-guard allowlists that
/// exact command shape.
const SAME_FLAG_REDECLARATIONS: &[&str] = &[
    "tui: url",
    "session tui: url",
    "sessions tui: url",
    "telegram start: url",
    "slack start: url",
    "env set: account",
];

/// Collect every `"<subcommand path>: <arg id>"` where a subcommand arg reuses
/// the id of a global arg declared on one of its ancestors. A positional, or a
/// flag under a different `--long`, is always reported; a same-`--long`
/// re-declaration is reported unless it is in [`SAME_FLAG_REDECLARATIONS`].
fn global_id_collisions(
    cmd: &clap::Command,
    inherited: &[(String, Option<String>)],
    path: &str,
    out: &mut Vec<String>,
) {
    let mut globals = inherited.to_vec();
    for arg in cmd.get_arguments() {
        let id = arg.get_id().as_str().to_string();
        let long = arg.get_long().map(str::to_string);
        if let Some((_, global_long)) = inherited.iter().find(|(g, _)| *g == id) {
            let site = format!("{path}: {id}");
            let same_flag = long.is_some() && long == *global_long;
            if !(same_flag && SAME_FLAG_REDECLARATIONS.contains(&site.as_str())) {
                out.push(site);
            }
        }
        if arg.is_global_set() && !globals.iter().any(|(g, _)| *g == id) {
            globals.push((id, long));
        }
    }
    for sub in cmd.get_subcommands() {
        let sub_path = format!("{path} {}", sub.get_name()).trim().to_string();
        global_id_collisions(sub, &globals, &sub_path, out);
    }
}

#[test]
fn no_subcommand_arg_id_reuses_a_global_arg_id() {
    // Walk the UNBUILT tree: `build()` propagates globals into every
    // subcommand under the same id, which would make every subcommand look
    // like a collision.
    let mut collisions = Vec::new();
    global_id_collisions(&Cli::command(), &[], "", &mut collisions);
    assert!(
        collisions.is_empty(),
        "subcommand args share an id with a global arg, so their values \
         land in the global field (#10569): {collisions:#?}"
    );
}

#[test]
fn cli_command_tree_passes_clap_debug_assert() {
    Cli::command().debug_assert();
}

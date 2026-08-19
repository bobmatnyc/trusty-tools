//! Precedence tests for the index the `serve` subcommand pins its MCP session
//! to (issue #4181).
//!
//! Why: ADR-0042 wants one MCP server declaration, `["serve"]` with no baked
//! `--index`, shared across every project, with `TRUSTY_INDEX` in the
//! declaration's `env` block naming the per-project index. That works today,
//! but nothing proved it and the source reads as though it does not: `Serve`
//! declares `#[arg(long)] index` with no `env`, which looks like a second arg
//! shadowing the `global = true, env = "TRUSTY_INDEX"` field on `Cli`. It is
//! not — both spell the same clap id, so clap propagates the one arg, env value
//! included, into the subcommand's matches. Two readers have now misread that,
//! so the behaviour ADR-0042 rests on is pinned here rather than left to be
//! re-derived. Removing `global` from `Cli::index`, renaming either field, or
//! giving `Serve` a distinct id would silently break the argless declaration;
//! these tests turn that into a failure.
//!
//! What: parses real `Cli` argument vectors, feeds the parsed fields through
//! [`super::resolve_pinned_index`] — the same function `main`'s `Serve` arm
//! calls — and asserts the result across all four combinations of `--index`
//! present/absent and `TRUSTY_INDEX` set/unset. The rows that need a real
//! environment run in a child process: clap reads `env` from the live process
//! environment during `try_parse_from`, and mutating it in-process would race
//! the other tests in this binary. The child is this same test binary re-run
//! with a single-test filter, so no daemon is contacted, no port is bound, and
//! the machine's live `trusty-search` is never touched.
//!
//! These tests live here rather than in `main.rs` because that file sits near
//! its `check_line_cap` budget; `Cli` stays reachable because this module is a
//! descendant of the binary's crate root.
//!
//! Test: `cargo test -p trusty-search --bin trusty-search index_env`

use super::resolve_pinned_index;
use crate::{Cli, Commands};
use clap::Parser;
use std::process::Command;

/// Set on the child process to tell [`probe_child`] to actually run. Its value
/// is the argument vector (space-separated, without the `trusty-search`
/// argv[0]) that the child should parse.
const PROBE_ARGV: &str = "TRUSTY_SERVE_INDEX_PROBE_ARGV";

/// Filter naming [`probe_child`] to libtest, exactly.
const PROBE_TEST: &str = "commands::serve::index_env_tests::probe_child";

/// Prefix of the single line [`probe_child`] prints for the parent to read.
const PROBE_PREFIX: &str = "PROBE_RESOLVED=";

/// Stand-in for `None` in the probe line, so the encoding stays free of
/// quoting rules.
const NONE: &str = "-";

/// What the `serve` arm sees after parsing: the global `Cli::index`, the
/// subcommand's own `index`, and the pin those resolve to.
#[derive(Debug, PartialEq, Eq)]
struct Parsed {
    global: Option<String>,
    subcommand: Option<String>,
    pin: Option<String>,
}

/// Parse `args` as a full argv and report both `index` fields plus the pin
/// `main`'s `Serve` arm would compute from them.
fn parse(args: &[&str]) -> Parsed {
    let cli = Cli::try_parse_from(args).expect("argument vector must parse");
    let global = cli.index;
    match cli.command {
        Commands::Serve { index, project, .. } => Parsed {
            global,
            subcommand: index.clone(),
            // #5264: the pin now carries its source alongside the id; these
            // rows assert the id, and `serve_scope_tests` asserts the source.
            pin: resolve_pinned_index(index, project).map(|c| c.index_id),
        },
        _ => panic!("expected Commands::Serve"),
    }
}

fn encode(v: &Option<String>) -> &str {
    v.as_deref().unwrap_or(NONE)
}

fn decode(v: &str) -> Option<String> {
    (v != NONE).then(|| v.to_string())
}

/// Child half of the probe: parse the argv handed over in [`PROBE_ARGV`] and
/// print the outcome on one line. A no-op when the variable is absent, which is
/// every ordinary run of this test.
#[test]
fn probe_child() {
    let Ok(argv) = std::env::var(PROBE_ARGV) else {
        return;
    };
    let mut args = vec!["trusty-search"];
    args.extend(argv.split_whitespace());
    let p = parse(&args);
    println!(
        "{PROBE_PREFIX}{};{};{}",
        encode(&p.global),
        encode(&p.subcommand),
        encode(&p.pin)
    );
}

/// Parent half of the probe: re-run this test binary with `TRUSTY_INDEX` set to
/// `env_value` (or removed, when `None`) and `argv` as the arguments to parse,
/// then return what the child observed.
fn parse_with_env(env_value: Option<&str>, argv: &str) -> Parsed {
    let exe = std::env::current_exe().expect("locate this test binary");
    let mut cmd = Command::new(exe);
    cmd.args([PROBE_TEST, "--exact", "--nocapture"])
        .env(PROBE_ARGV, argv)
        .env_remove("TRUSTY_INDEX");
    if let Some(v) = env_value {
        cmd.env("TRUSTY_INDEX", v);
    }

    let out = cmd
        .output()
        .expect("re-run this test binary as a probe child");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "probe child failed (TRUSTY_INDEX={env_value:?}, argv {argv:?}):\n{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let line = stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix(PROBE_PREFIX))
        .unwrap_or_else(|| {
            panic!("probe child printed no {PROBE_PREFIX} line; stdout was:\n{stdout}")
        });
    let mut parts = line.split(';');
    let mut next = || decode(parts.next().expect("probe line has three fields"));
    Parsed {
        global: next(),
        subcommand: next(),
        pin: next(),
    }
}

/// Why (issue #4181): the row ADR-0042 rests on. An argless `["serve"]`
/// declaration carries the project's index only in `TRUSTY_INDEX`; if that
/// stopped reaching the pin, every tool call in the session would fall back to
/// fan-out across all 40-odd indexes on the machine — slower, and answering
/// from the wrong repo.
/// What: asserts an unset `--index` with `TRUSTY_INDEX` set pins the
/// environment value, and that it arrives via the propagated global arg rather
/// than a separate subcommand-local one.
/// Test: this function.
#[test]
fn env_alone_pins_the_index() {
    let p = parse_with_env(Some("env-index"), "serve");
    assert_eq!(
        p.pin,
        Some("env-index".to_string()),
        "`serve` must pin to TRUSTY_INDEX when no --index is given (#4181)"
    );
    assert_eq!(
        p.global, p.subcommand,
        "`Serve::index` and the global `Cli::index` are one clap arg; if these \
         ever diverge, the subcommand has gained an id of its own and the \
         argless MCP declaration ADR-0042 needs has stopped working"
    );
}

/// Why: an explicit flag is the operator's direct instruction and must outrank
/// an inherited environment, which an MCP client or a parent shell can set
/// without the operator ever seeing it.
/// What: asserts `--index` wins with `TRUSTY_INDEX` also set.
/// Test: this function.
#[test]
fn explicit_flag_beats_the_environment() {
    assert_eq!(
        parse_with_env(Some("env-index"), "serve --index flag-index").pin,
        Some("flag-index".to_string()),
        "an explicit --index must outrank TRUSTY_INDEX"
    );
}

/// Why: an explicit `--index` is what every existing baked MCP declaration
/// relies on, and it must keep working whether or not the environment is
/// involved.
/// What: asserts `--index` resolves with `TRUSTY_INDEX` unset, both in-process
/// and in a child whose environment is provably clean.
/// Test: this function.
#[test]
fn explicit_flag_alone_pins_the_index() {
    assert_eq!(
        parse(&["trusty-search", "serve", "--index", "flag-index"]).pin,
        Some("flag-index".to_string())
    );
    assert_eq!(
        parse_with_env(None, "serve --index flag-index").pin,
        Some("flag-index".to_string())
    );
}

/// Why (issue #1373): the regression guard. #1373 was a wrong-index bug, and
/// the fix for it is the `--project`-derived fallback in `main`'s `Serve` arm.
/// A source that resolved to anything other than `None` here would pin the
/// wrong index for every caller relying on that fallback — the same defect
/// class, reintroduced.
/// What: asserts neither source resolves anything when both are absent, so the
/// arm still falls through to the `--project` derivation, and that the
/// derivation itself still runs.
///
/// #5709 note on shape: both rows run in a child, because both describe an
/// absent `TRUSTY_INDEX` and neither passes `--index`. An in-process row cannot
/// state that — clap reads `env` from the live process environment, so the
/// first row asserted `None` against whatever the invoking shell exported, and
/// the second passed on the environment value rather than on the `--project`
/// derivation it names.
/// Test: this function.
#[test]
fn neither_source_leaves_the_project_fallback_intact() {
    assert_eq!(parse_with_env(None, "serve").pin, None);

    let derived = parse_with_env(None, "serve --project /tmp/some-project").pin;
    assert!(
        derived.is_some(),
        "--project must still derive an index id when nothing else pins one (#1373)"
    );
}

/// Why: `--project` is the fallback the arm applies only when no index
/// resolved, so an environment-sourced index must suppress it exactly as an
/// explicit `--index` does.
/// What: asserts `TRUSTY_INDEX` wins over a `--project` that would derive a
/// different id.
/// Test: this function.
#[test]
fn env_index_outranks_project() {
    assert_eq!(
        parse_with_env(Some("env-index"), "serve --project /tmp/some-project").pin,
        Some("env-index".to_string()),
        "an index from any source wins over --project"
    );
}

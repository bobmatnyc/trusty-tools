//! CLI contract snapshot of the `trusty-memory` clap command tree (#9277).
//!
//! Why: ADR-0066 D1.2 freezes the trusty-memory CLI for 1.x — binary names,
//! subcommand paths, flag long names and short aliases, positional arity, flag
//! value types and exit codes — and D4 names this snapshot as its guard. A 1.x
//! change is additive only, so a rename, a removal, or an optional argument
//! turning required must fail here, while a new subcommand or optional flag
//! must not.
//! What: [`snapshot`] walks the built `Cli::command()` tree into JSON Lines
//! records (one per binary, exit code, command and argument) and
//! [`check`] compares the committed snapshot against the live tree with the
//! additive-only rules. Help text and descriptions are out of contract
//! (ADR-0066 D2) and are never recorded. A subcommand or flag renamed with
//! its old name kept as an alias still resolves, so the ADR's deprecation
//! path passes.
//! Test: `cli_contract_matches_snapshot` (the real tree) and the synthetic
//! comparator cases below it.

use super::Cli;
use clap::{Arg, ArgAction, Command, CommandFactory};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;

/// The committed snapshot, relative to the workspace root.
const SNAPSHOT: &str = "crates/trusty-memory/tests/snapshots/cli_contract.jsonl";
/// Environment variable that rewrites the snapshot instead of comparing.
const REFRESH_ENV: &str = "UPDATE_CLI_CONTRACT";
const REFRESH_CMD: &str =
    "UPDATE_CLI_CONTRACT=1 cargo test -p trusty-memory --bin trusty-memory cli_contract";

/// One line of the snapshot file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Record {
    Binary {
        name: String,
    },
    ExitCode {
        condition: String,
        code: i32,
    },
    Command {
        path: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        aliases: Vec<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        subcommand_required: bool,
    },
    Arg(ArgRecord),
}

/// The contract attributes of one argument. `key` is `--long`, `-s` for a
/// short-only flag, or `#N` for the Nth positional (a positional's field name
/// is not user-visible, so it is not part of the key).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ArgRecord {
    command: String,
    key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    short: Option<char>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    short_aliases: Vec<char>,
    #[serde(default, skip_serializing_if = "is_false")]
    required: bool,
    action: String,
    min_values: usize,
    /// `None` is unbounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_values: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value_type: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    possible_values: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    global: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Breaking changes and additive drift found by [`check`].
#[derive(Debug, Default)]
struct Report {
    breaks: Vec<String>,
    additions: Vec<String>,
}

/// Records the contract surface of `root` plus the given binary names.
fn snapshot(root: &Command, binaries: &[String]) -> Vec<Record> {
    let mut built = root.clone();
    built.build();
    let mut out: Vec<Record> = binaries
        .iter()
        .map(|name| Record::Binary { name: name.clone() })
        .collect();
    let usage = built
        .clone()
        .try_get_matches_from([built.get_name(), "--no-such-flag-9277"])
        .expect_err("an unknown flag is a usage error");
    out.push(Record::ExitCode {
        condition: "usage_error".to_string(),
        code: usage.exit_code(),
    });
    walk(
        &built,
        built.get_name().to_string(),
        &BTreeSet::new(),
        &mut out,
    );
    out
}

fn walk(cmd: &Command, path: String, inherited: &BTreeSet<String>, out: &mut Vec<Record>) {
    let mut aliases: Vec<String> = cmd.get_all_aliases().map(str::to_string).collect();
    aliases.sort();
    out.push(Record::Command {
        path: path.clone(),
        aliases,
        subcommand_required: cmd.is_subcommand_required_set(),
    });
    let mut globals = inherited.clone();
    for arg in cmd.get_arguments() {
        // clap generates `--help` / `--version`; a propagated global is
        // recorded once, where it is declared.
        if matches!(
            arg.get_action(),
            ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
        ) {
            continue;
        }
        let record = arg_record(&path, arg);
        if record.global && !globals.insert(record.key.clone()) {
            continue;
        }
        out.push(Record::Arg(record));
    }
    for sub in cmd.get_subcommands() {
        // clap's generated `help` subcommand is not ours to freeze.
        if sub.get_name() == "help" {
            continue;
        }
        walk(sub, format!("{path} {}", sub.get_name()), &globals, out);
    }
}

fn arg_record(command: &str, arg: &Arg) -> ArgRecord {
    let key = if arg.is_positional() {
        format!(
            "#{}",
            arg.get_index().expect("a built positional has an index")
        )
    } else if let Some(long) = arg.get_long() {
        format!("--{long}")
    } else {
        format!(
            "-{}",
            arg.get_short().expect("a named flag has a long or short")
        )
    };
    let mut aliases: Vec<String> = arg
        .get_all_aliases()
        .unwrap_or_default()
        .into_iter()
        .map(str::to_string)
        .collect();
    aliases.sort();
    let mut short_aliases = arg.get_all_short_aliases().unwrap_or_default();
    short_aliases.sort();
    let range = arg.get_num_args().expect("a built arg has num_args");
    let takes_values = arg.get_action().takes_values();
    let possible_values: Vec<String> = if takes_values {
        let mut v: Vec<String> = arg
            .get_possible_values()
            .iter()
            .map(|p| p.get_name().to_string())
            .collect();
        v.sort();
        v
    } else {
        Vec::new()
    };
    let value_type = takes_values.then(|| {
        if possible_values.is_empty() {
            value_type_name(arg)
        } else {
            // A value enum's contract is its accepted strings, not the Rust type.
            "enum".to_string()
        }
    });
    ArgRecord {
        command: command.to_string(),
        key,
        short: arg.get_short(),
        aliases,
        short_aliases,
        required: arg.is_required_set(),
        action: format!("{:?}", arg.get_action()),
        min_values: range.min_values(),
        max_values: if range.max_values() == usize::MAX {
            None
        } else {
            Some(range.max_values())
        },
        value_type,
        possible_values,
        global: arg.is_global_set(),
    }
}

/// The value parser's type with module paths stripped (`std::path::PathBuf`
/// → `PathBuf`), so moving a type between modules is not a type change.
fn value_type_name(arg: &Arg) -> String {
    let full = format!("{:?}", arg.get_value_parser().type_id());
    assert!(
        !full.starts_with("TypeId"),
        "clap reports value type names only with debug assertions on; run the \
         CLI contract test in the dev/test profile, not --release"
    );
    let mut out = String::new();
    let mut segment_start = 0;
    let mut chars = full.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ':' && chars.peek() == Some(&':') {
            chars.next();
            out.truncate(segment_start);
        } else {
            out.push(c);
            if !(c.is_alphanumeric() || c == '_') {
                segment_start = out.len();
            }
        }
    }
    out
}

/// Compares the committed `golden` records with the live `root` tree under
/// ADR-0066's additive-only rules.
fn check(golden: &[Record], root: &Command, binaries: &[String]) -> Report {
    let mut built = root.clone();
    built.build();
    let current = snapshot(&built, binaries);
    let mut report = Report::default();
    // (canonical command path, live key) of every live arg a golden arg found.
    let mut matched: BTreeSet<(String, String)> = BTreeSet::new();
    for record in golden {
        match record {
            Record::Binary { name } if !binaries.contains(name) => {
                report.breaks.push(format!("binary `{name}` was removed"));
            }
            Record::ExitCode { condition, code } => {
                let live = current.iter().find_map(|r| match r {
                    Record::ExitCode { condition: c, code } if c == condition => Some(*code),
                    _ => None,
                });
                if live != Some(*code) {
                    report.breaks.push(format!(
                        "exit code for {condition} changed from {code} to {live:?}"
                    ));
                }
            }
            Record::Command {
                path,
                aliases,
                subcommand_required,
            } => match resolve(&built, path) {
                None => report
                    .breaks
                    .push(format!("subcommand `{path}` was removed or renamed")),
                Some(cmd) => {
                    let live: BTreeSet<&str> = cmd.get_all_aliases().collect();
                    for alias in aliases.iter().filter(|a| !live.contains(a.as_str())) {
                        report
                            .breaks
                            .push(format!("subcommand `{path}` lost its alias `{alias}`"));
                    }
                    if !subcommand_required && cmd.is_subcommand_required_set() {
                        report
                            .breaks
                            .push(format!("subcommand `{path}` now requires a subcommand"));
                    }
                }
            },
            Record::Arg(g) => {
                let Some(canonical) = canonical_path(&built, &g.command) else {
                    continue; // reported once, as the missing subcommand
                };
                let live: Vec<&ArgRecord> = current
                    .iter()
                    .filter_map(|r| match r {
                        Record::Arg(a) if a.command == canonical => Some(a),
                        _ => None,
                    })
                    .collect();
                match live.iter().find(|a| arg_matches(g, a)) {
                    None => report.breaks.push(format!(
                        "`{}` of `{}` was removed or renamed",
                        g.key, g.command
                    )),
                    Some(c) => {
                        matched.insert((c.command.clone(), c.key.clone()));
                        compare_arg(g, c, &mut report.breaks);
                    }
                }
            }
            Record::Binary { .. } => {}
        }
    }
    // #9277: canonical paths of the golden commands. A new subcommand is
    // additive, so only its pre-existing commands can gain a breaking arg.
    let golden_commands: BTreeSet<String> = golden
        .iter()
        .filter_map(|r| match r {
            Record::Command { path, .. } => canonical_path(&built, path),
            _ => None,
        })
        .collect();
    for record in current.iter().filter(|r| !golden.contains(r)) {
        if let Record::Arg(c) = record {
            let known = matched.contains(&(c.command.clone(), c.key.clone()));
            if !known && c.required && golden_commands.contains(&c.command) {
                report.breaks.push(format!(
                    "new argument `{}` of `{}` is required",
                    c.key, c.command
                ));
                continue;
            }
        }
        report
            .additions
            .push(serde_json::to_string(record).expect("record serializes"));
    }
    report
}

/// Walks `path` (binary name first) by subcommand name or alias.
fn resolve<'a>(root: &'a Command, path: &str) -> Option<&'a Command> {
    path.split(' ')
        .skip(1)
        .try_fold(root, |cmd, segment| cmd.find_subcommand(segment))
}

/// The live, canonical-name path of `path` (which may name an alias).
fn canonical_path(root: &Command, path: &str) -> Option<String> {
    let mut out = root.get_name().to_string();
    let mut cmd = root;
    for segment in path.split(' ').skip(1) {
        cmd = cmd.find_subcommand(segment)?;
        out.push(' ');
        out.push_str(cmd.get_name());
    }
    Some(out)
}

/// True when the live arg `c` still answers to the golden arg's key.
fn arg_matches(g: &ArgRecord, c: &ArgRecord) -> bool {
    if c.key == g.key {
        return true;
    }
    if let Some(long) = g.key.strip_prefix("--") {
        return c.aliases.iter().any(|a| a == long);
    }
    if let (Some(short), false) = (g.short, g.key.starts_with('#')) {
        return c.short == Some(short) || c.short_aliases.contains(&short);
    }
    false
}

fn compare_arg(g: &ArgRecord, c: &ArgRecord, breaks: &mut Vec<String>) {
    let at = format!("`{}` of `{}`", g.key, g.command);
    if let Some(short) = g.short {
        if c.short != Some(short) && !c.short_aliases.contains(&short) {
            breaks.push(format!("{at} lost its short alias `-{short}`"));
        }
    }
    let longs: BTreeSet<&str> = c
        .key
        .strip_prefix("--")
        .into_iter()
        .chain(c.aliases.iter().map(String::as_str))
        .collect();
    for alias in g.aliases.iter().filter(|a| !longs.contains(a.as_str())) {
        breaks.push(format!("{at} lost its alias `--{alias}`"));
    }
    for short in g
        .short_aliases
        .iter()
        .filter(|s| c.short != Some(**s) && !c.short_aliases.contains(s))
    {
        breaks.push(format!("{at} lost its short alias `-{short}`"));
    }
    if !g.required && c.required {
        breaks.push(format!("{at} changed from optional to required"));
    }
    if g.action != c.action {
        breaks.push(format!(
            "{at} action changed from {} to {}",
            g.action, c.action
        ));
    }
    let narrower_max = match (g.max_values, c.max_values) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(g_max), Some(c_max)) => c_max < g_max,
    };
    if c.min_values > g.min_values || narrower_max {
        breaks.push(format!("{at} accepts fewer values (arity narrowed)"));
    }
    if g.value_type != c.value_type {
        breaks.push(format!(
            "{at} value type changed from {:?} to {:?}",
            g.value_type, c.value_type
        ));
    }
    for value in g
        .possible_values
        .iter()
        .filter(|v| !c.possible_values.contains(v))
    {
        breaks.push(format!("{at} no longer accepts the value `{value}`"));
    }
    if g.global && !c.global {
        breaks.push(format!("{at} is no longer global"));
    }
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `[[bin]]` names from this crate's manifest — the frozen binary names.
fn binary_names() -> Vec<String> {
    let text = std::fs::read_to_string(manifest_dir().join("Cargo.toml")).expect("read Cargo.toml");
    let manifest: toml::Value = text.parse().expect("Cargo.toml parses");
    manifest
        .get("bin")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|bin| bin.get("name")?.as_str().map(str::to_string))
        .collect()
}

fn render(records: &[Record]) -> String {
    let mut text: String = records
        .iter()
        .map(|r| serde_json::to_string(r).expect("record serializes"))
        .collect::<Vec<_>>()
        .join("\n");
    text.push('\n');
    text
}

fn parse(text: &str) -> Vec<Record> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad snapshot line {l}: {e}")))
        .collect()
}

/// Why: the guard ADR-0066 D4 assigns to the CLI surface.
/// What: compares `tests/snapshots/cli_contract.jsonl` with the live tree;
/// with `UPDATE_CLI_CONTRACT=1` it rewrites the snapshot instead.
/// Test: this function.
#[test]
fn cli_contract_matches_snapshot() {
    let path = manifest_dir().join(SNAPSHOT.trim_start_matches("crates/trusty-memory/"));
    let binaries = binary_names();
    let root = Cli::command();
    if std::env::var(REFRESH_ENV).is_ok_and(|v| !v.is_empty() && v != "0") {
        std::fs::create_dir_all(path.parent().expect("snapshot has a parent")).expect("mkdir");
        std::fs::write(&path, render(&snapshot(&root, &binaries))).expect("write snapshot");
        eprintln!("rewrote {}", path.display());
        return;
    }
    let golden = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e}\ncreate it with: {REFRESH_CMD}",
            path.display()
        )
    });
    let report = check(&parse(&golden), &root, &binaries);
    if !report.additions.is_empty() {
        eprintln!(
            "note: the CLI gained {} item(s) the snapshot does not pin yet. Additive \
             changes are allowed in 1.x (ADR-0066 D3); pin them with `{REFRESH_CMD}` \
             together with a changelog fragment that names the addition (ADR-0066 D4):\n  {}",
            report.additions.len(),
            report.additions.join("\n  ")
        );
    }
    assert!(
        report.breaks.is_empty(),
        "trusty-memory CLI contract broken (ADR-0066 D1.2, guard #9277):\n  - {}\n\n\
         The 1.x CLI changes additively only. To retire a subcommand or flag, keep \
         the old name as an alias and print the D3 deprecation warning instead.\n\
         Refreshing the snapshot over a break IS a contract break: it needs a major \
         version and Bob's override (ADR-0066 D6), plus a `Breaking` changelog \
         fragment. Only then refresh with:\n    {REFRESH_CMD}\nand commit {SNAPSHOT}.",
        report.breaks.join("\n  - "),
    );
}

/// A small tree the comparator cases below mutate.
fn sample() -> Command {
    Command::new("tool")
        .arg(
            Arg::new("verbose")
                .short('v')
                .long("verbose")
                .action(ArgAction::Count)
                .global(true),
        )
        .subcommand(
            Command::new("list")
                .arg(
                    Arg::new("limit")
                        .short('n')
                        .long("limit")
                        .value_parser(clap::value_parser!(usize)),
                )
                .arg(Arg::new("name")),
        )
        .subcommand(Command::new("rm").arg(Arg::new("id").required(true)))
}

/// One mutation of [`sample`].
type Change = Box<dyn FnOnce(Command) -> Command>;

fn breaks_after(change: impl FnOnce(Command) -> Command) -> Vec<String> {
    let golden = snapshot(&sample(), &[]);
    check(&golden, &change(sample()), &[]).breaks
}

/// Why: criterion "a rename or removal fails" (#9277), proved on a tree the
/// test controls. What: renames and removals of a subcommand, a flag and a
/// short alias each produce a break. Test: this function.
#[test]
fn cli_contract_flags_renames_and_removals() {
    let cases: Vec<(&str, Change)> = vec![
        (
            "renamed subcommand",
            Box::new(|c| c.mut_subcommand("list", |s| s.name("ls"))),
        ),
        (
            "renamed flag",
            Box::new(|c| c.mut_subcommand("list", |s| s.mut_arg("limit", |a| a.long("max")))),
        ),
        (
            "removed short",
            Box::new(|c| c.mut_subcommand("list", |s| s.mut_arg("limit", |a| a.short(None)))),
        ),
        (
            "changed value type",
            Box::new(|c| {
                c.mut_subcommand("list", |s| {
                    s.mut_arg("limit", |a| a.value_parser(clap::value_parser!(String)))
                })
            }),
        ),
        (
            "optional positional made required",
            Box::new(|c| c.mut_subcommand("list", |s| s.mut_arg("name", |a| a.required(true)))),
        ),
        (
            "new required flag",
            Box::new(|c| {
                c.mut_subcommand("rm", |s| {
                    s.arg(Arg::new("force").long("force").required(true))
                })
            }),
        ),
        (
            "global flag made local",
            Box::new(|c| c.mut_arg("verbose", |a| a.global(false))),
        ),
    ];
    for (name, change) in cases {
        assert!(
            !breaks_after(change).is_empty(),
            "{name} must be reported as a break"
        );
    }
}

/// Why: ADR-0066 D3 allows additive change and the deprecation path in 1.x.
/// What: a new subcommand, a new optional flag, a new short, and a rename that
/// keeps the old name as an alias all pass. Test: this function.
#[test]
fn cli_contract_allows_additive_changes_and_aliased_renames() {
    let cases: Vec<(&str, Change)> = vec![
        ("unchanged", Box::new(|c| c)),
        (
            "new subcommand",
            Box::new(|c| c.subcommand(Command::new("stats"))),
        ),
        (
            "new subcommand with a required positional",
            Box::new(|c| c.subcommand(Command::new("forget").arg(Arg::new("id").required(true)))),
        ),
        (
            "new optional flag",
            Box::new(|c| {
                c.mut_subcommand("rm", |s| {
                    s.arg(
                        Arg::new("dry-run")
                            .long("dry-run")
                            .action(ArgAction::SetTrue),
                    )
                })
            }),
        ),
        (
            "required positional made optional",
            Box::new(|c| c.mut_subcommand("rm", |s| s.mut_arg("id", |a| a.required(false)))),
        ),
        (
            "subcommand renamed, old name kept as alias",
            Box::new(|c| c.mut_subcommand("list", |s| s.name("ls").alias("list"))),
        ),
        (
            "subcommand with a required arg renamed, old name kept as alias",
            Box::new(|c| c.mut_subcommand("rm", |s| s.name("remove").alias("rm"))),
        ),
        (
            "flag renamed, old name kept as alias",
            Box::new(|c| {
                c.mut_subcommand("list", |s| {
                    s.mut_arg("limit", |a| a.long("max").alias("limit"))
                })
            }),
        ),
    ];
    for (name, change) in cases {
        let breaks = breaks_after(change);
        assert!(
            breaks.is_empty(),
            "{name} must not break the contract: {breaks:?}"
        );
    }
}

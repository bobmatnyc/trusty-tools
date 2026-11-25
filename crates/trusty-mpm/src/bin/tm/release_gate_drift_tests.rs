//! The release-gate command table covers every `tm` command (#11470).
//!
//! Why: `scripts/release-gate.sh` runs the candidate binary command by
//! command, from `crates/trusty-mpm/release-gate/commands.tsv`. A command
//! added without a row would ship untested, so a missing row fails here.
//! What: walks the in-process clap tree, hidden commands included and clap's
//! `help` excluded, and requires a table row per leaf path. A row path
//! ending ` <verb>` covers every leaf under its prefix (the hidden `session`
//! alias tree). A row path that names no command fails too, so a removed
//! command leaves no stale row.

use clap::CommandFactory as _;

const TABLE: &str = include_str!("../../../release-gate/commands.tsv");

/// Rows that are invocation variants, not clap paths.
const VARIANTS: &[&str] = &[
    "wait --for check",
    "serve --stdio",
    "<url> (external subcommand)",
];

/// Every leaf command path of `cmd`, space-joined, hidden ones included.
fn leaves(cmd: &clap::Command, prefix: &str, out: &mut Vec<String>) {
    let subs: Vec<_> = cmd
        .get_subcommands()
        .filter(|c| c.get_name() != "help")
        .collect();
    if subs.is_empty() && !prefix.is_empty() {
        out.push(prefix.to_owned());
    }
    for sub in subs {
        let path = if prefix.is_empty() {
            sub.get_name().to_owned()
        } else {
            format!("{prefix} {}", sub.get_name())
        };
        leaves(sub, &path, out);
    }
}

fn table_paths(table: &str) -> Vec<String> {
    table
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| l.split('\t').next())
        .map(str::to_owned)
        .collect()
}

/// The clap leaves with no row in `table`.
fn unclassified(table: &str) -> Vec<String> {
    let rows = table_paths(table);
    let mut all = Vec::new();
    leaves(&crate::cli::Cli::command(), "", &mut all);
    all.into_iter()
        .filter(|leaf| {
            !rows.iter().any(|row| match row.strip_suffix(" <verb>") {
                Some(prefix) => leaf == prefix || leaf.starts_with(&format!("{prefix} ")),
                None => row == leaf,
            })
        })
        .collect()
}

/// #11470 rule 9: a `tm` command with no row in the table fails.
#[test]
fn every_tm_command_has_a_release_gate_row() {
    let missing = unclassified(TABLE);
    assert!(
        missing.is_empty(),
        "add a row to crates/trusty-mpm/release-gate/commands.tsv for: {missing:?}"
    );
}

/// The check catches a command with no row: dropping one row is reported.
#[test]
fn a_command_without_a_row_is_reported() {
    let pruned: String = TABLE
        .lines()
        .filter(|l| !l.starts_with("banner\t"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(unclassified(&pruned), vec!["banner".to_owned()]);
}

/// A row that names no command is stale.
#[test]
fn every_release_gate_row_names_a_command() {
    let mut all = Vec::new();
    leaves(&crate::cli::Cli::command(), "", &mut all);
    let stale: Vec<String> = table_paths(TABLE)
        .into_iter()
        .filter(|row| !VARIANTS.contains(&row.as_str()))
        .filter(|row| match row.strip_suffix(" <verb>") {
            Some(prefix) => !all.iter().any(|l| l.starts_with(&format!("{prefix} "))),
            None => !all.contains(row),
        })
        .collect();
    assert!(stale.is_empty(), "rows naming no tm command: {stale:?}");
}

/// The rows of `table` that break its format: not eight columns, or a
/// runnable row expecting a non-zero exit with no expected output.
fn malformed(table: &str) -> Vec<String> {
    table
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter(|l| {
            let cols: Vec<&str> = l.split('\t').collect();
            if cols.len() != 8 {
                return true;
            }
            let runnable = matches!(cols[1], "SAFE" | "SANDBOX" | "CONTENT");
            runnable && cols[5] != "0" && matches!(cols[6], "" | "-")
        })
        .map(|l| l.split('\t').next().unwrap_or_default().to_owned())
        .collect()
}

/// #11470 F3: a row expecting an error names the error text it expects.
#[test]
fn every_error_row_names_its_expected_output() {
    let bad = malformed(TABLE);
    assert!(bad.is_empty(), "malformed release-gate rows: {bad:?}");
}

/// The check catches an error row whose expected output is blanked.
#[test]
fn an_error_row_without_expected_output_is_reported() {
    let blanked = "sessions stop\tSANDBOX\t$H\tsessions stop nosuch\t-\t1\t-\tr\n";
    assert_eq!(malformed(blanked), vec!["sessions stop".to_owned()]);
}

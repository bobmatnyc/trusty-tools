//! Walks the `tm` CLI's clap `Command` tree in-process to render
//! `references/cli.md` (source #1 of the issue #2913 design-research brief).
//!
//! Why: no code anywhere in the workspace previously called clap's
//! introspection API (`Command::get_subcommands`) — the CLI surface (51
//! top-level commands, depth up to 3) had no machine-extractable catalog.
//! Parsing `--help` text would be strictly worse (unstructured, drifts
//! silently); `Cli::command()` (clap derives this for free via
//! `CommandFactory`) already returns the fully-built `clap::Command` tree, so
//! walking it in-process can never drift from what clap actually parses.
//! What: [`render`] recursively walks every subcommand from the root,
//! emitting one indented markdown bullet per command with its `about` text.
//! Test: `cli_tree_render_contains_known_command`,
//! `cli_tree_render_is_deterministic`.

use std::fmt::Write as _;

use clap::CommandFactory;

use crate::cli::Cli;

/// Render the full CLI command tree as nested markdown bullets.
///
/// Why: gives an operator or agent a single, exhaustive, always-current list
/// of every `tm <command>` (and nested subcommand) without running `--help`
/// once per group.
/// What: sorts subcommands alphabetically at every depth (not declaration
/// order) so the output is stable under harmless source reordering — a
/// requirement for the byte-reproducible drift check (issue #2913).
/// Test: `cli_tree_render_contains_known_command`,
/// `cli_tree_render_is_deterministic`.
pub(crate) fn render() -> String {
    let root = Cli::command();
    let mut out = String::new();
    out.push_str("# CLI Command Reference\n\n");
    out.push_str(
        "Generated from `Cli::command()` (clap's command-tree introspection) — \
         every `tm <command>` and its nested subcommands, verbatim. Source: \
         `crates/trusty-mpm/src/bin/tm/cli/mod.rs` (top-level `Command` enum) \
         plus one action enum per group under `cli/actions/*.rs`. Regenerate \
         with `tm generate capabilities`.\n\n",
    );

    let mut commands: Vec<&clap::Command> = root.get_subcommands().collect();
    commands.sort_by_key(|c| c.get_name().to_string());
    let _ = writeln!(out, "{} top-level commands.\n", commands.len());

    for cmd in commands {
        render_command(cmd, 0, &mut out);
    }
    out
}

/// Render one command and its subcommands, recursively, depth-first.
fn render_command(cmd: &clap::Command, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    let name = cmd.get_name();
    let about = cmd.get_about().map(|s| s.to_string()).unwrap_or_default();
    if about.is_empty() {
        let _ = writeln!(out, "{indent}- `{name}`");
    } else {
        let _ = writeln!(out, "{indent}- `{name}` — {about}");
    }

    let mut subs: Vec<&clap::Command> = cmd.get_subcommands().collect();
    subs.sort_by_key(|c| c.get_name().to_string());
    for sub in subs {
        render_command(sub, depth + 1, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_tree_render_contains_known_command() {
        let rendered = render();
        assert!(rendered.contains("`doctor`"), "{rendered}");
        assert!(rendered.contains("`session`"), "{rendered}");
        assert!(rendered.contains("`generate`"), "{rendered}");
        assert!(rendered.contains("`capabilities`"), "{rendered}");
    }

    #[test]
    fn cli_tree_render_is_deterministic() {
        assert_eq!(render(), render());
    }

    #[test]
    fn cli_tree_render_lists_top_level_command_count() {
        let root = Cli::command();
        let expected = root.get_subcommands().count();
        let rendered = render();
        assert!(
            rendered.contains(&format!("{expected} top-level commands.")),
            "{rendered}"
        );
    }
}

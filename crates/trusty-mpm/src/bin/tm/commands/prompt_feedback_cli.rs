//! `tm prompt-feedback` — read back the captured prompt critiques (#7688).
//!
//! Why: the ledger is JSON Lines, which is the right storage and the wrong
//! reading experience. This is the minimum surface that makes the capture
//! usable: newest-first listing with the three filters an operator actually
//! needs, plus a per-agent-type count that answers "whose prompt should I fix
//! first". The prompt-engineer optimization pass that consumes this is a
//! deliberate follow-up slice, not part of #7688.
//!
//! A TOP-LEVEL VERB rather than a subcommand of an existing one: the nearest
//! candidates are `tm session` (this is not scoped to a session — the default
//! read spans all of them) and `tm divert` (a different feature). Neither
//! contains it, so a new verb is the honest placement.
//!
//! What: [`run`] lists or summarises. Reads only; nothing here writes.
//! Test: the inline suite below.

use trusty_mpm::core::prompt_feedback::{FeedbackRow, ReadFilter, read_rows, summarize};

/// Arguments for `tm prompt-feedback`.
///
/// What: the three filters plus `--summary`. `--summary` changes the RENDERING,
/// not the selection, so it composes with every filter.
/// Test: `cli_parses_prompt_feedback`, `cli_parses_prompt_feedback_summary`.
#[derive(Debug, Clone, clap::Args)]
pub struct PromptFeedbackArgs {
    /// Show only feedback captured in this Claude Code session.
    #[arg(long)]
    pub session: Option<String>,
    /// Show only feedback from this agent type (`pm` for the session itself).
    #[arg(long)]
    pub agent: Option<String>,
    /// Show at most this many of the newest matching rows.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Group by agent type and print counts instead of the rows.
    #[arg(long)]
    pub summary: bool,
}

/// Run the read-back.
///
/// What: resolves the ledger under the ambient framework root, applies the
/// filters, and renders. An empty ledger prints one line saying so rather than
/// nothing, because silence is indistinguishable from a broken command.
/// Test: `renders_rows_newest_first`, `renders_a_summary`,
/// `renders_a_note_when_nothing_is_captured`.
pub(crate) async fn run(args: PromptFeedbackArgs) -> anyhow::Result<()> {
    let root = trusty_mpm::core::paths::FrameworkPaths::default().root;
    let filter = ReadFilter {
        session: args.session.clone(),
        agent: args.agent.clone(),
        // `--summary` counts across everything that matched, so the row cap
        // would silently understate it.
        limit: (!args.summary).then_some(args.limit),
    };
    let rows = read_rows(&root, &filter);
    print!("{}", render(&rows, args.summary));
    Ok(())
}

/// Render `rows` as text.
///
/// Why: split from [`run`] so the output is assertable without a framework root
/// or a filesystem.
/// What: the summary table when `summary`, else one block per row.
/// Test: `renders_rows_newest_first`, `renders_a_summary`,
/// `renders_a_note_when_nothing_is_captured`.
fn render(rows: &[FeedbackRow], summary: bool) -> String {
    if rows.is_empty() {
        return "no prompt feedback captured yet\n".to_string();
    }
    if summary {
        let mut out = String::new();
        for (agent, count) in summarize(rows) {
            out.push_str(&format!("{count:>5}  {agent}\n"));
        }
        out.push_str(&format!("{:>5}  total\n", rows.len()));
        return out;
    }

    let mut out = String::new();
    for row in rows {
        out.push_str(&format!(
            "{}  {}  session={}  prompt={}\n",
            row.ts,
            row.agent_type,
            row.session_id.as_deref().unwrap_or("-"),
            // The first 12 hex chars identify the prompt without wrapping the
            // line; the full digest is in the ledger for anyone who needs it.
            row.prompt_digest
                .as_deref()
                .map_or("-", |d| &d[..d.len().min(12)])
        ));
        for line in row.feedback.lines() {
            out.push_str(&format!("    {line}\n"));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
#[path = "prompt_feedback_cli_tests.rs"]
mod tests;

//! `tm commit-trailers` — the per-commit stats footer (#7074).
//!
//! Why: the owner observed that commits made in a tm session carry the
//! attribution line and nothing about what the session spent. This command is
//! what the bundled `prepare-commit-msg` hook calls to add that: the session's
//! tokens in and out, the savings percentage, and the model id, as git
//! trailers. It runs in a different process from the status bar, so every value
//! comes from a store the render already writes — this command adds no writer
//! of its own to the usage directory.
//!
//! What: gathers the four values, renders them with
//! [`trusty_mpm::core::commit_trailers`], and either prints them or splices
//! them into a commit-message file. Where each comes from:
//!
//! | Trailer | Source |
//! |---|---|
//! | `Tokens-In` / `Tokens-Out` | the session transcript, folded and deduped by message id |
//! | `Savings` | `usage/savings.jsonl`, folded for this session, as the `💸` segment's percent |
//! | `Model` | `usage/session-model/<session_id>`, written by the statusline render |
//!
//! Any value with no source is omitted rather than rendered as a zero. The
//! command exits 0 on every path, including one where it stamps nothing: a
//! commit must never fail over its own statistics.
//!
//! Which of those happens at all depends on git's commit SOURCE, passed through
//! as `--commit-source` (#7249): a merge or squash message belongs to no single
//! session and gets nothing, and a reused message — cherry-pick, revert, amend
//! — has the previous session's block stripped before this one's is written.
//!
//! Test: `stats_are_empty_without_a_session`, `stamps_a_message_file`,
//! `an_unstampable_message_is_left_alone`, `a_merge_source_writes_nothing`,
//! `a_reused_message_is_stripped_in_place`.

use std::path::Path;

use anyhow::Context as _;
use trusty_mpm::core::commit_trailers::{
    CommitStats, StampPolicy, append_trailers, render_trailers, stamp_policy, strip_trailers,
};
use trusty_mpm::core::savings::{claude_code_session_id, fold_session, savings_log_in};
use trusty_mpm::core::session_model::read_session_model;
use trusty_mpm::core::session_record::{KIND_TRANSCRIPT, read_session_record};
use trusty_mpm::core::transcript_usage::{TRANSCRIPT_TAIL_BYTES, fold_transcript};

use crate::cli::CommitTrailersArgs;

/// Gather this session's stats from the stores the statusline already writes.
///
/// Why: separated from [`run`] so the whole gather is testable against a temp
/// root with no environment mutation and no live session — which is also what
/// keeps `session_actual_tokens` a parameter rather than a read of the real
/// `~/.trusty-mpm/statusline/` store.
/// What: folds the recorded transcript for the token pair, folds the savings
/// ledger for the percentage, and reads the recorded model id. Each field is
/// `None` when its source said nothing. A transcript the fold's byte cap cut
/// short also sets `tokens_window_bytes`, so the footer states that the counts
/// cover that window rather than the session.
/// Test: `stats_are_empty_without_a_session`, `stats_carry_every_known_value`,
/// `a_truncated_fold_scopes_the_counts_to_its_window`.
pub(crate) fn gather_stats(
    root: &Path,
    session_id: &str,
    session_actual_tokens: Option<u64>,
) -> CommitStats {
    let usage = read_session_record(root, KIND_TRANSCRIPT, session_id)
        .map(|path| fold_transcript(Path::new(&path)))
        .unwrap_or_default();
    let savings = fold_session(&savings_log_in(root), session_id);

    CommitStats {
        tokens_in: (!usage.is_empty()).then_some(usage.tokens_in),
        tokens_out: (!usage.is_empty()).then_some(usage.tokens_out),
        tokens_window_bytes: usage.truncated.then_some(TRANSCRIPT_TAIL_BYTES),
        savings_percent: savings.percent_saved(session_actual_tokens),
        model_id: read_session_model(root, session_id),
    }
}

/// Print the trailers, or splice them into a commit-message file.
///
/// Why/What: see the module doc. `--commit-source` decides what happens before
/// anything is gathered (#7249): a merge or squash returns having written
/// nothing, and a reused message — cherry-pick, revert, amend — has its
/// inherited block stripped first, so a session with nothing of its own to say
/// still leaves no other session's figures behind. Exits 0 whether or not
/// anything was stamped — the caller is a `prepare-commit-msg` hook, and a
/// non-zero exit there aborts the operator's commit.
/// Test: `stamps_a_message_file`, `an_unstampable_message_is_left_alone`,
/// `a_merge_source_writes_nothing`.
pub(crate) fn run(args: &CommitTrailersArgs) -> anyhow::Result<()> {
    let policy = stamp_policy(args.commit_source.as_deref().unwrap_or_default());
    if policy == StampPolicy::Skip {
        return Ok(());
    }
    if policy == StampPolicy::Restamp
        && let Some(path) = args.message_file.as_deref()
    {
        rewrite_message_file(path, strip_trailers)?;
    }

    let Some(session_id) = claude_code_session_id() else {
        return Ok(());
    };
    let Ok(paths) = crate::commands::managed_root::resolve_managed_paths(None) else {
        return Ok(());
    };
    let actual_tokens =
        crate::commands::statusline::compaction::session_actual_tokens_for(&session_id);
    let stats = gather_stats(&paths.root, &session_id, actual_tokens);
    let Some(trailers) = render_trailers(&stats) else {
        return Ok(());
    };

    match args.message_file.as_deref() {
        Some(path) => stamp_message_file(path, &trailers)?,
        None => println!("{trailers}"),
    }
    Ok(())
}

/// Splice `trailers` into the commit message at `path`.
///
/// Why: the hook hands over git's own `COMMIT_EDITMSG`-shaped file, comments
/// and scissors included, so the placement rules live in [`append_trailers`]
/// rather than here.
/// Test: `stamps_a_message_file`, `an_unstampable_message_is_left_alone`.
fn stamp_message_file(path: &Path, trailers: &str) -> anyhow::Result<()> {
    rewrite_message_file(path, |message| append_trailers(message, trailers))
}

/// Apply `transform` to the commit message at `path`.
///
/// Why: the write is atomic because the hook enforces a wall-clock budget and
/// can kill this process mid-run — a half-written `COMMIT_EDITMSG` would cost
/// the operator their commit message, where a temp-file rename leaves either
/// the original or the new text.
/// What: reads, transforms, and writes back only when the text actually
/// changed, so an amend over an already-stamped message does no write at all.
/// Test: `stamps_a_message_file`, `a_reused_message_is_stripped_in_place`.
fn rewrite_message_file(path: &Path, transform: impl FnOnce(&str) -> String) -> anyhow::Result<()> {
    use std::io::Write as _;

    let original = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read the commit message file {}", path.display()))?;
    let stamped = transform(&original);
    if stamped == original {
        return Ok(());
    }

    let dir = path.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("cannot stage a commit message beside {}", path.display()))?;
    tmp.write_all(stamped.as_bytes())
        .with_context(|| format!("cannot write the commit message file {}", path.display()))?;
    tmp.persist(path)
        .with_context(|| format!("cannot replace the commit message file {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use trusty_mpm::core::savings::{SavingsRow, append_row, now_ts};
    use trusty_mpm::core::session_record::{KIND_MODEL, record_session_value};

    fn seed(root: &Path, session_id: &str) -> PathBuf {
        let transcript = root.join("transcript.jsonl");
        std::fs::write(
            &transcript,
            "{\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":90,\"output_tokens\":7}}}\n",
        )
        .expect("write transcript");
        record_session_value(
            root,
            KIND_TRANSCRIPT,
            session_id,
            &transcript.display().to_string(),
        );
        record_session_value(root, KIND_MODEL, session_id, "claude-opus-4-1-20250805");
        append_row(
            &savings_log_in(root),
            &SavingsRow {
                ts: now_ts(),
                session_id: session_id.to_string(),
                technique: "divert".to_string(),
                tokens_saved: 400,
                tokens_before: 1_000,
                cost_saved_usd: 0.02,
                basis: "fixture".to_string(),
                model_source: "statusline".to_string(),
            },
        )
        .expect("append row");
        transcript
    }

    /// Why (#7074): a root with nothing recorded must produce no footer at all,
    /// rather than a footer of zeros — the same rule the `💸` segment follows.
    /// Test: itself.
    #[test]
    fn stats_are_empty_without_a_session() {
        let dir = tempfile::tempdir().expect("temp dir");
        let stats = gather_stats(dir.path(), "sess-unknown", None);
        assert!(stats.is_empty());
        assert_eq!(render_trailers(&stats), None);
    }

    /// Why: pins each value to its own source, so a gather that read the model
    /// store for the transcript path (or folded the wrong session) fails here.
    /// Test: itself.
    #[test]
    fn stats_carry_every_known_value() {
        let dir = tempfile::tempdir().expect("temp dir");
        seed(dir.path(), "sess-a");
        let stats = gather_stats(dir.path(), "sess-a", None);
        assert_eq!(stats.tokens_in, Some(100));
        assert_eq!(stats.tokens_out, Some(7));
        assert_eq!(stats.savings_percent, Some(40));
        assert_eq!(stats.model_id.as_deref(), Some("claude-opus-4-1-20250805"));
        assert_eq!(
            stats.tokens_window_bytes, None,
            "a transcript inside the cap is a whole-session total"
        );
    }

    /// Why (#7074 round-2 review): the fold caps its read so a commit never
    /// waits on a huge transcript, and the counts then cover a tail window. The
    /// gather must carry that scope through to the footer rather than letting
    /// the window's figures pass as session totals.
    /// Test: itself.
    #[test]
    fn a_truncated_fold_scopes_the_counts_to_its_window() {
        let dir = tempfile::tempdir().expect("temp dir");
        let transcript = dir.path().join("big.jsonl");
        let filler = format!("{{\"type\":\"user\",\"pad\":\"{}\"}}", "a".repeat(1024));
        let mut text = String::new();
        while text.len() as u64 <= TRANSCRIPT_TAIL_BYTES {
            text.push_str(&filler);
            text.push('\n');
        }
        text.push_str("{\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"usage\":{\"input_tokens\":10,\"output_tokens\":7}}}\n");
        std::fs::write(&transcript, &text).expect("write transcript");
        record_session_value(
            dir.path(),
            KIND_TRANSCRIPT,
            "sess-big",
            &transcript.display().to_string(),
        );

        let stats = gather_stats(dir.path(), "sess-big", None);
        assert_eq!(stats.tokens_in, Some(10));
        assert_eq!(stats.tokens_out, Some(7));
        assert_eq!(stats.tokens_window_bytes, Some(TRANSCRIPT_TAIL_BYTES));

        let rendered = render_trailers(&stats).expect("trailers");
        assert!(
            rendered.contains("Tokens-Window: last 8 MiB of a larger transcript"),
            "{rendered}"
        );
    }

    /// Why: this is what the hook actually does — hand over a real
    /// `COMMIT_EDITMSG` and get it back stamped, with the block parseable by
    /// git.
    /// Test: itself.
    #[test]
    fn stamps_a_message_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        seed(dir.path(), "sess-a");
        let stats = gather_stats(dir.path(), "sess-a", None);
        let trailers = render_trailers(&stats).expect("trailers");

        let msg = dir.path().join("COMMIT_EDITMSG");
        std::fs::write(&msg, "feat: x (Refs #7074)\n\n# a git comment\n").expect("write");
        stamp_message_file(&msg, &trailers).expect("stamp");

        let stamped = std::fs::read_to_string(&msg).expect("read");
        assert!(stamped.contains("Tokens-In: 100"), "{stamped}");
        assert!(stamped.contains("Tokens-Out: 7"), "{stamped}");
        assert!(stamped.contains("Savings: 40%"), "{stamped}");
        assert!(
            stamped.contains("Model: claude-opus-4-1-20250805"),
            "{stamped}"
        );
    }

    /// Why (#7249): a merge commit's message is assembled from its parents, so
    /// the tokens of whoever ran `git merge` describe none of it. The command
    /// must return before it gathers or writes anything.
    /// Test: itself.
    #[test]
    fn a_merge_source_writes_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let msg = dir.path().join("MERGE_MSG");
        std::fs::write(&msg, "Merge branch 'topic'\n").expect("write");

        run(&CommitTrailersArgs {
            message_file: Some(msg.clone()),
            commit_source: Some("merge".to_string()),
        })
        .expect("run");

        assert_eq!(
            std::fs::read_to_string(&msg).expect("read"),
            "Merge branch 'topic'\n"
        );
    }

    /// Why (#7249): the `commit` source hands over a message from an EARLIER
    /// commit, whose block describes another session. The strip is what removes
    /// it, and it must survive the comment block git appends to the file.
    /// Test: itself.
    #[test]
    fn a_reused_message_is_stripped_in_place() {
        let dir = tempfile::tempdir().expect("temp dir");
        let msg = dir.path().join("COMMIT_EDITMSG");
        std::fs::write(
            &msg,
            "feat: x\n\nTokens-In: 999\nTokens-Out: 888\nModel: stale\n\n# a git comment\n",
        )
        .expect("write");

        rewrite_message_file(&msg, strip_trailers).expect("strip");

        let stripped = std::fs::read_to_string(&msg).expect("read");
        assert_eq!(stripped, "feat: x\n\n# a git comment\n", "{stripped}");
    }

    /// Why: an amend re-runs the hook over a message that already carries the
    /// block; the file must not be rewritten and the footer must not double.
    /// Test: itself.
    #[test]
    fn an_unstampable_message_is_left_alone() {
        let dir = tempfile::tempdir().expect("temp dir");
        let msg = dir.path().join("COMMIT_EDITMSG");
        std::fs::write(&msg, "feat: x\n\nTokens-In: 5\n").expect("write");
        stamp_message_file(&msg, "Tokens-In: 99").expect("stamp");
        assert_eq!(
            std::fs::read_to_string(&msg).expect("read"),
            "feat: x\n\nTokens-In: 5\n"
        );
    }
}

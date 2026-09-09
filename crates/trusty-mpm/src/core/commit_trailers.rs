//! The per-commit stats footer: `Tokens-In`, `Tokens-Out`, `Savings`, `Model`,
//! and the `Tokens-Window` line that scopes the counts when the transcript fold
//! read only a tail window (#7074).
//!
//! Why: the attribution footer Claude Code writes says a session produced the
//! commit but nothing about what it cost, and the owner's observation on #7074
//! was exactly that — "I don't see the token counts/model in the commit
//! messages". These four values answer it at the one place a reader is already
//! looking. They are **git trailers**, not prose, so `git interpret-trailers`
//! and `trusty-git-analytics`' existing attribution collector can read them
//! back without a parser of their own.
//!
//! What: [`render_trailers`] turns a [`CommitStats`] into the trailer lines,
//! and [`append_trailers`] puts them into a commit message where git will
//! recognise them. Both are pure functions of their arguments — every read of
//! the ledger, the transcript, and the model store happens in the caller
//! (`tm commit-trailers`), so the placement rules below are unit-testable with
//! no filesystem at all.
//!
//! Three placement facts, each verified against git 2.54:
//!
//! - **The trailer block must be the message's last paragraph, and its FIRST
//!   line must itself be a trailer.** The existing footer's first line
//!   (`🤖🤖🤖 Generated with trusty-mpm — …`) is not trailer-shaped, so
//!   appending our keys to that same paragraph makes git parse none of them —
//!   including the `Claude-Session:` line already there. The block therefore
//!   goes in its own paragraph, after a blank line.
//! - **A trailing comment block is not the end of the message.** `git commit`
//!   hands a `prepare-commit-msg` hook a file whose tail is `#`-prefixed help
//!   text; text appended after it parses as part of that comment run. The
//!   insert goes after the last real content line instead.
//! - **`git commit --verbose` appends a diff after a scissors line.** Anything
//!   below that line is discarded, so the scissors bound the insert.
//!
//! Whether a message gets a block at all depends on git's commit SOURCE, not
//! only on its text: [`stamp_policy`] maps the source to skip, restamp, or
//! stamp, and [`strip_trailers`] removes a block a reused message carried in
//! from another session (#7249).
//!
//! Test: the inline suite in `commit_trailers_tests.rs` —
//! `render_omits_absent_fields`, `render_is_none_when_nothing_is_known`,
//! `git_interpret_trailers_parses_the_appended_block`,
//! `append_goes_above_a_trailing_comment_block`,
//! `append_stays_above_the_scissors_line`, `append_is_idempotent`.

/// Trailer key carrying the session's input tokens.
///
/// Test: `git_interpret_trailers_parses_the_appended_block`.
pub const TRAILER_TOKENS_IN: &str = "Tokens-In";

/// Trailer key carrying the session's output tokens.
///
/// Test: `git_interpret_trailers_parses_the_appended_block`.
pub const TRAILER_TOKENS_OUT: &str = "Tokens-Out";

/// Trailer key stating that the token counts cover a tail window, not the
/// whole session.
///
/// Why: the transcript fold is capped so a commit never waits on a huge read,
/// and the store behind it holds only the transcript's path — there are no
/// running per-session totals that would make a tail read stand for the
/// session. When the cap cuts, `Tokens-In` and `Tokens-Out` describe the window
/// and a reader has to be told so. It is a separate key rather than a suffix on
/// the two numbers, so both stay plain integers for anything parsing them back.
/// What: present only when the fold was truncated, naming the window's size.
/// Test: `render_states_the_window_when_the_fold_was_truncated`,
/// `render_omits_the_window_when_the_fold_read_everything`.
pub const TRAILER_TOKENS_WINDOW: &str = "Tokens-Window";

/// Trailer key carrying the savings percentage (#7179's percent form).
///
/// Test: `git_interpret_trailers_parses_the_appended_block`.
pub const TRAILER_SAVINGS: &str = "Savings";

/// Trailer key carrying the harness model id the session ran under.
///
/// Test: `git_interpret_trailers_parses_the_appended_block`.
pub const TRAILER_MODEL: &str = "Model";

/// The line `git commit --verbose` puts above the diff it appends.
///
/// Why: everything from this line down is discarded by git's cleanup, so a
/// trailer inserted below it never reaches the commit object.
/// Test: `append_stays_above_the_scissors_line`.
const SCISSORS: &str = "# ------------------------ >8 ------------------------";

/// The five keys this footer owns, in render order.
///
/// Why: [`strip_trailers`] has to recognise a block it wrote in an earlier
/// session, and recognising it by key is the only thing available — the values
/// are exactly what changed.
/// Test: `strip_removes_an_inherited_block`.
const STATS_KEYS: [&str; 5] = [
    TRAILER_TOKENS_IN,
    TRAILER_TOKENS_OUT,
    TRAILER_TOKENS_WINDOW,
    TRAILER_SAVINGS,
    TRAILER_MODEL,
];

/// What to do with a commit message, given how git says the commit was made.
///
/// Why: git tells a `prepare-commit-msg` hook its second argument — the commit
/// SOURCE — and the footer means something different for each (#7249). Tokens
/// are a property of one session's work on one message; a source that assembles
/// a message from other commits, or carries one in from another session, has no
/// such figure to state.
/// What: three answers, one per source class. See [`stamp_policy`].
/// Test: `stamp_policy_skips_merge_and_squash`,
/// `stamp_policy_restamps_a_reused_message`, `stamp_policy_stamps_a_new_message`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StampPolicy {
    /// Write nothing: the message belongs to no single session.
    Skip,
    /// Remove any inherited block first, then stamp this session's figures.
    Restamp,
    /// Stamp, leaving anything already there alone.
    Stamp,
}

/// Which policy git's commit-source argument earns.
///
/// Why (#7249): the hook used to ignore the argument, so a merge or squash got
/// the committing session's token counts stamped onto a message assembled from
/// other people's commits, and a cherry-pick kept the ORIGINAL commit's counts —
/// [`already_stamped`] read the inherited block as this session's own work and
/// left it in place. The four rules below are what the sources actually mean:
///
/// - `merge` — git built the message from the merge's parents. The tokens of
///   whoever ran `git merge` describe none of it. **Skip.**
/// - `squash` — the message is assembled by git or GitHub from a branch's
///   commits, each with its own figures. **Skip.**
/// - `commit` — the message comes from an existing commit: a cherry-pick, a
///   revert, `--amend`, `-c`/`-C`. The new commit is this session's work, so any
///   inherited block is stale and misattributed. **Restamp** — strip what came
///   in, then stamp this session's figures. With no current session there is
///   nothing to stamp, and the strip still happens: no block at all is correct,
///   another session's block is not.
/// - `message` (`-m`/`-F`), `template`, and the empty source git passes for an
///   ordinary editor commit — the message is this commit's own. **Stamp.**
///
/// What: an unrecognised source stamps, matching the empty-source case, because
/// a source git adds later is far likelier to be message-shaped than
/// merge-shaped, and a wrong stamp is recoverable where a silently skipped one
/// is invisible.
/// Test: `stamp_policy_skips_merge_and_squash`,
/// `stamp_policy_restamps_a_reused_message`, `stamp_policy_stamps_a_new_message`.
pub fn stamp_policy(source: &str) -> StampPolicy {
    match source.trim() {
        "merge" | "squash" => StampPolicy::Skip,
        "commit" => StampPolicy::Restamp,
        _ => StampPolicy::Stamp,
    }
}

/// Remove a stats footer this code wrote, wherever the message came from.
///
/// Why (#7249): a cherry-picked or amended message arrives carrying the ORIGINAL
/// commit's figures. Those numbers are not wrong about anything the new commit
/// did — they describe a different session's work — so they are removed rather
/// than left beside a second block.
/// What: only the message's LAST content paragraph is considered, and only when
/// every one of its lines is one of [`STATS_KEYS`] — that is the exact shape
/// [`append_trailers`] writes, and confining the match to it keeps a `Model:`
/// line in someone's prose out of scope. The blank line that separated the block
/// from the paragraph above goes with it. A trailing comment block and anything
/// below a `--verbose` scissors line are left where they are. A message with no
/// such block is returned unchanged.
/// Test: `strip_removes_an_inherited_block`,
/// `strip_leaves_a_message_that_has_none`,
/// `strip_keeps_a_trailing_comment_block`,
/// `strip_leaves_a_paragraph_that_is_not_only_trailers`.
pub fn strip_trailers(message: &str) -> String {
    let lines: Vec<&str> = message.lines().collect();
    let scissors_at = lines.iter().position(|line| line.trim_end() == SCISSORS);
    let search_end = scissors_at.unwrap_or(lines.len());
    let Some(last) = lines[..search_end]
        .iter()
        .rposition(|line| is_content(line))
    else {
        return message.to_string();
    };

    let mut first = last;
    while first > 0 && is_content(lines[first - 1]) {
        first -= 1;
    }
    if !lines[first..=last]
        .iter()
        .all(|line| is_stats_trailer(line))
    {
        return message.to_string();
    }

    let mut cut_from = first;
    if cut_from > 0 && lines[cut_from - 1].trim().is_empty() {
        cut_from -= 1;
    }

    let mut kept: Vec<&str> = Vec::with_capacity(lines.len());
    kept.extend_from_slice(&lines[..cut_from]);
    kept.extend_from_slice(&lines[last + 1..]);

    let mut out = kept.join("\n");
    if message.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Is `line` one of this footer's own trailer lines?
///
/// Test: `strip_removes_an_inherited_block`.
fn is_stats_trailer(line: &str) -> bool {
    let trimmed = line.trim_start();
    STATS_KEYS.iter().any(|key| {
        trimmed
            .strip_prefix(key)
            .is_some_and(|rest| rest.starts_with(':'))
    })
}

/// What one commit's stats footer states, each field independently optional.
///
/// Why: the four values come from three different stores and any of them can be
/// absent — a session whose status bar has not rendered has no model record, a
/// session with no savings row has no percentage. An absent field is omitted
/// from the footer rather than rendered as a zero, for the same reason the
/// `💸` segment omits itself: a stated zero is a measurement that was never
/// made.
/// What: plain `Option`s; [`render_trailers`] renders exactly the present ones.
/// Test: `render_omits_absent_fields`, `render_is_none_when_nothing_is_known`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitStats {
    /// Input tokens the session sent, cache creation and reads included.
    pub tokens_in: Option<u64>,
    /// Output tokens the session received.
    pub tokens_out: Option<u64>,
    /// Size of the transcript tail the counts were folded from, when the fold's
    /// byte cap stopped it short of the whole file.
    ///
    /// Why: `None` is the ordinary case and means the two counts are
    /// whole-session totals. `Some` narrows what they claim to that window.
    /// Test: `render_states_the_window_when_the_fold_was_truncated`.
    pub tokens_window_bytes: Option<u64>,
    /// Whole-number percent of tokens the harness avoided sending.
    pub savings_percent: Option<u32>,
    /// The harness model id, e.g. `claude-opus-4-1-20250805`.
    pub model_id: Option<String>,
}

impl CommitStats {
    /// Whether every field is absent, so there is no footer to write.
    ///
    /// `tokens_window_bytes` is deliberately not consulted: it scopes the two
    /// counts rather than being a measurement of its own, and a window with no
    /// counts beside it says nothing.
    /// Test: `render_is_none_when_nothing_is_known`.
    pub fn is_empty(&self) -> bool {
        self.tokens_in.is_none()
            && self.tokens_out.is_none()
            && self.savings_percent.is_none()
            && self.model_id.is_none()
    }
}

/// Render the present fields as trailer lines, in a fixed order.
///
/// Why: a fixed order makes the footer diffable across commits, and rendering
/// only present fields is what keeps an unknown value from becoming a stated
/// zero.
/// What: `Tokens-In`, `Tokens-Out`, `Tokens-Window`, `Savings`, `Model`, one
/// `Key: value` line each, newline-separated with no trailing newline. The
/// window line follows the counts it scopes and appears only when there is a
/// count to scope. A model id carrying a newline (which would break the block
/// into two paragraphs) is rejected rather than sanitised — no store writes
/// one, and silently rewriting an id would make a wrong value look right.
/// `None` when nothing is known.
/// Test: `render_omits_absent_fields`, `render_is_none_when_nothing_is_known`,
/// `render_rejects_a_multiline_model_id`,
/// `render_states_the_window_when_the_fold_was_truncated`,
/// `render_omits_the_window_when_the_fold_read_everything`.
pub fn render_trailers(stats: &CommitStats) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    if let Some(tokens_in) = stats.tokens_in {
        lines.push(format!("{TRAILER_TOKENS_IN}: {tokens_in}"));
    }
    if let Some(tokens_out) = stats.tokens_out {
        lines.push(format!("{TRAILER_TOKENS_OUT}: {tokens_out}"));
    }
    if let Some(bytes) = stats.tokens_window_bytes
        && !lines.is_empty()
    {
        lines.push(format!("{TRAILER_TOKENS_WINDOW}: {}", window_label(bytes)));
    }
    if let Some(percent) = stats.savings_percent {
        lines.push(format!("{TRAILER_SAVINGS}: {percent}%"));
    }
    if let Some(model) = stats.model_id.as_deref() {
        let model = model.trim();
        if !model.is_empty() && !model.contains(['\n', '\r']) {
            lines.push(format!("{TRAILER_MODEL}: {model}"));
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// Describe the tail window the token counts were folded from.
///
/// Why: a commit footer is read by people, and `8388608` states the same fact
/// far worse than `8 MiB`. The byte form is kept for a cap that is not a whole
/// number of MiB, so the label never rounds a size into a lie.
/// What: `last <n> MiB of a larger transcript`, or the byte count when the size
/// is not a whole multiple of a MiB.
/// Test: `render_states_the_window_when_the_fold_was_truncated`,
/// `window_label_falls_back_to_bytes_off_a_mib_boundary`.
fn window_label(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("last {} MiB of a larger transcript", bytes / MIB)
    } else {
        format!("last {bytes} bytes of a larger transcript")
    }
}

/// Put `trailers` into `message` where git will parse them as a trailer block.
///
/// Why/What: see the module doc for the three placement facts. The insert lands
/// after the last content line above the scissors, separated by one blank line,
/// so the block is its own final paragraph of the cleaned-up message. Already
/// carrying a `Tokens-In:` line, `message` is returned unchanged — `git commit
/// --amend` re-runs `prepare-commit-msg` over a message the hook already
/// stamped, and a second block would be the one git then parses.
/// Test: `git_interpret_trailers_parses_the_appended_block`,
/// `append_goes_above_a_trailing_comment_block`,
/// `append_stays_above_the_scissors_line`, `append_is_idempotent`.
pub fn append_trailers(message: &str, trailers: &str) -> String {
    if already_stamped(message) {
        return message.to_string();
    }

    let mut lines: Vec<&str> = message.lines().collect();
    let scissors_at = lines.iter().position(|line| line.trim_end() == SCISSORS);
    let search_end = scissors_at.unwrap_or(lines.len());
    let insert_at = lines[..search_end]
        .iter()
        .rposition(|line| is_content(line))
        .map_or(0, |idx| idx + 1);

    let mut block: Vec<&str> = vec![""];
    block.extend(trailers.lines());
    lines.splice(insert_at..insert_at, block);

    let mut out = lines.join("\n");
    if message.ends_with('\n') || out.is_empty() {
        out.push('\n');
    }
    out
}

/// Does `message` already carry a stats footer?
///
/// Why: this is the idempotency guard WITHIN one commit source — `git commit
/// --amend` and a retried hook both re-run over a message this session already
/// stamped, and a second block would be the one git parses. It cannot tell that
/// block apart from one a cherry-pick carried in from another session, which is
/// why the `commit` source strips before it stamps rather than asking here
/// (#7249, [`stamp_policy`]).
/// Test: `append_is_idempotent`.
fn already_stamped(message: &str) -> bool {
    message.lines().any(|line| {
        line.trim_start()
            .starts_with(&format!("{TRAILER_TOKENS_IN}:"))
    })
}

/// Is `line` real message content, rather than a git comment or blank?
///
/// Test: `append_goes_above_a_trailing_comment_block`.
fn is_content(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty() && !trimmed.starts_with('#')
}

#[cfg(test)]
#[path = "commit_trailers_tests.rs"]
mod tests;

//! Recall-query shaping for `prompt-context` (#4972).
//!
//! Why: the hook sent the raw user prompt to `/recall` verbatim and the
//! embedder cut it at its 512-token window with no warning, no metric, and no
//! signal to the caller — so the vector represented a *prefix* of the query and
//! nothing said so. Over the logged corpus (17,163 real firings,
//! `~/Library/Application Support/trusty-memory/logs/enriched-prompts.*.jsonl`)
//! 52.0% of prompts exceed that window under the model's own WordPiece
//! tokenizer, and 65.3% of them open with a `<task-notification>` envelope
//! whose framing alone spends a *median 253 of the 512 tokens* before the
//! payload begins. Half the window was machine boilerplate.
//! What: [`shape_recall_query`] — strip the known envelope, then pack whole
//! units (lines, then words) up to a token budget, and report what it did via
//! [`RecallQueryShape`]. Pure and allocation-light; no I/O.
//! Test: `envelope_strip_recovers_the_payload`,
//! `over_window_query_is_reduced_to_whole_units`,
//! `short_query_passes_through_untouched`, `token_estimate_never_splits_a_unit`,
//! `configured_query_budget_clamps_to_bounds`.
//!
//! Why not chunk-and-pool (the third option #4972 lists): pooling needs either
//! N embedder round trips inside a 1.5 s hook budget ([`super::BODY_DEADLINE`])
//! or a new server-side recall surface, and the owner's ruling on #5037 steers
//! at *raising the ceiling*, not at fusing more retrieval machinery. Stripping
//! the envelope raises the effective ceiling for free — the same 512 tokens now
//! carry payload instead of framing.

use crate::prompt_log::RecallQueryShape;

/// Token budget applied to the recall query before it is sent.
///
/// Why: the recall endpoint embeds with fastembed's `all-MiniLM-L6-v2`, whose
/// truncation is `DEFAULT_MAX_LENGTH.min(model_max_length)` — 512 for this
/// model (`tokenizer_config.json: model_max_length = 512`). Confirmed
/// empirically against the live daemon rather than read off a config: recalling
/// with a fixed suffix behind a growing filler prefix, the top score varies
/// through 456 tokens (0.2995) and then *plateaus at exactly 0.2533* for 532,
/// 608, 760, 912, 1216, 1824 and 2432 tokens. Identical scores across a 4.5×
/// range of input length means the embedding stopped changing — everything past
/// the window is invisible.
/// What: `512`, overridable via [`ENV_QUERY_TOKEN_BUDGET`].
/// Test: `over_window_query_is_reduced_to_whole_units`.
pub(super) const QUERY_TOKEN_BUDGET: usize = 512;

/// Env override for [`QUERY_TOKEN_BUDGET`].
///
/// Why: the window is a property of whichever embedder the daemon happens to
/// load, and this process cannot ask it. An operator running a longer-context
/// embedder must be able to raise the budget; setting it far above the real
/// window restores the pre-#4972 behaviour (the embedder truncates, silently)
/// which is the escape hatch if the shaping ever over-cuts.
/// What: a `usize` parsed from the env, clamped to
/// `[MIN_QUERY_TOKENS, MAX_QUERY_TOKENS]`.
/// Test: `configured_query_budget_clamps_to_bounds`.
pub const ENV_QUERY_TOKEN_BUDGET: &str = "TRUSTY_MEMORY_PROMPT_QUERY_TOKENS";

/// Floor for the operator-supplied budget — below this no real prompt survives.
const MIN_QUERY_TOKENS: usize = 32;

/// Ceiling for the operator-supplied budget. Nothing in the embedder family we
/// support has a window near this; it exists to stop a mistyped value being
/// treated as unbounded.
const MAX_QUERY_TOKENS: usize = 8192;

/// Opening marker of the task-notification envelope.
const ENVELOPE_OPEN: &str = "<task-notification>";

/// Envelope elements worth embedding. Everything else in the envelope —
/// `<task-id>`, `<tool-use-id>`, `<output-file>` (an absolute path), `<status>`,
/// `<note>` — is machine framing: hashes and paths that tokenize badly and
/// carry no user intent.
const ENVELOPE_KEEP: [&str; 2] = ["summary", "result"];

/// Characters per WordPiece sub-token inside one alphanumeric run.
///
/// Why: calibrated, not guessed. Measured against the model's own
/// `tokenizer.json` vocabulary (a WordPiece greedy-longest-match reference
/// implementation) over 1,500 randomly sampled real prompts:
///
/// | divisor | est/true p01 | p50 | p95 | underestimates |
/// |---|---|---|---|---|
/// | 3 | 0.99 | 1.25 | 1.58 | **1.1%** |
/// | 4 | 0.84 | 1.08 | 1.33 | 15.6% |
/// | 5 | 0.79 | 1.00 | 1.22 | 50.5% |
///
/// An *under*-estimate is the failure that matters: it hands the embedder a
/// query it then cuts silently, which is the whole defect. `3` is the only
/// divisor that essentially never does that (p01 = 0.99, so even the worst
/// case is within 1%), at the price of a median 25% over-estimate — we spend
/// ~410 of the 512 tokens rather than risk overrunning them. Note the repo's
/// `inference::bedrock::cache::estimate_tokens` uses chars/4 for a *different*
/// question (is this prefix big enough to be worth caching), where an
/// over-estimate is the harmful direction; over this corpus chars/4
/// underestimates 94.5% of the time.
/// Test: `token_estimate_never_splits_a_unit`.
const CHARS_PER_SUBTOKEN: usize = 3;

/// `[CLS]` + `[SEP]`, which every BERT-family encoding pays.
const SPECIAL_TOKEN_OVERHEAD: usize = 2;

/// A recall query after shaping, plus the record of what shaping did.
///
/// Why: the text and the account of how it was produced must travel together,
/// or the caller can send a reduced query and forget to report the reduction —
/// which is the silent-truncation defect wearing a different hat.
/// What: `text` is what goes on the wire; `shape` is what gets logged.
/// Test: `over_window_query_is_reduced_to_whole_units`.
pub(super) struct ShapedQuery {
    /// The query to send to `/recall`.
    pub(super) text: String,
    /// What shaping did, for the log and the warn line.
    pub(super) shape: RecallQueryShape,
}

/// Shape a raw user prompt into a recall query that fits the embedder window.
///
/// Why (issue #4972): see the module doc. The owner's ruling on #5037 permits
/// withholding but not withholding *silently*, and requires whole units rather
/// than an arbitrary cut — an embedder truncating mid-word at token 512 is both
/// of the things the ruling forbids.
/// What: (1) strips the `<task-notification>` envelope down to `<summary>` +
/// `<result>` when present; (2) if the result still exceeds `budget_tokens`,
/// keeps whole leading lines, falling back to whole words when a single line
/// busts the budget on its own — never a partial unit. Returns the shaped text
/// alongside a [`RecallQueryShape`] recording original/sent token estimates,
/// whether the envelope was stripped, and how many units were dropped.
/// Failure isolation: total. There is no error path — a prompt that cannot be
/// reduced to even one whole unit yields an empty query, which
/// [`super::fetch::fetch_palace_recall`] already treats as "recall nothing",
/// and the shape says so.
/// Test: `envelope_strip_recovers_the_payload`,
/// `over_window_query_is_reduced_to_whole_units`,
/// `short_query_passes_through_untouched`.
pub(super) fn shape_recall_query(prompt: &str, budget_tokens: usize) -> ShapedQuery {
    let original_tokens = estimate_tokens(prompt);
    let (body, envelope_stripped) = match strip_notification_envelope(prompt) {
        Some(inner) => (inner, true),
        None => (prompt.to_string(), false),
    };
    let (text, units_dropped) = pack_whole_units(&body, budget_tokens);
    let sent_tokens = estimate_tokens(&text);
    ShapedQuery {
        shape: RecallQueryShape {
            original_tokens,
            sent_tokens,
            budget_tokens,
            envelope_stripped,
            units_dropped,
        },
        text,
    }
}

/// Emit the stderr record of a reshaped query.
///
/// Why (issue #4972): "no warning, no metric, and no signal to the caller" is
/// the defect as filed. The durable metric is [`RecallQueryShape`] on the
/// enriched-prompt log line — the same corpus the 52% figure was measured over,
/// so the truncation rate is now queryable with one `jq` filter. This warn is
/// the live counterpart for anyone watching the daemon's stderr.
/// What: one `tracing::warn!` carrying the shape's fields, only when shaping
/// actually changed the query. Silent on the pass-through path.
/// Test: side-effect only; the shape it reports is covered by
/// `over_window_query_is_reduced_to_whole_units`.
pub(super) fn warn_if_reshaped(shape: &RecallQueryShape) {
    if !shape.reshaped() {
        return;
    }
    tracing::warn!(
        original_tokens = shape.original_tokens,
        sent_tokens = shape.sent_tokens,
        budget_tokens = shape.budget_tokens,
        envelope_stripped = shape.envelope_stripped,
        units_dropped = shape.units_dropped,
        "prompt-context: recall query reshaped to fit the embedder window (#4972)"
    );
}

/// Read [`ENV_QUERY_TOKEN_BUDGET`], clamped.
///
/// Why: same operator-escape-hatch shape as `super::configured_top_k`.
/// What: delegates to [`clamp_query_budget`] so the arithmetic is testable
/// without mutating process env.
/// Test: `configured_query_budget_clamps_to_bounds` covers the clamp.
pub(super) fn configured_query_budget() -> usize {
    clamp_query_budget(std::env::var(ENV_QUERY_TOKEN_BUDGET).ok().as_deref())
}

/// Parse and clamp a raw [`ENV_QUERY_TOKEN_BUDGET`] value.
///
/// What: parses `raw` as a `usize` and clamps to
/// `[MIN_QUERY_TOKENS, MAX_QUERY_TOKENS]`. `None`, unparseable, and zero fall
/// back to [`QUERY_TOKEN_BUDGET`].
/// Test: `configured_query_budget_clamps_to_bounds`.
fn clamp_query_budget(raw: Option<&str>) -> usize {
    raw.and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .map(|n| n.clamp(MIN_QUERY_TOKENS, MAX_QUERY_TOKENS))
        .unwrap_or(QUERY_TOKEN_BUDGET)
}

/// Reduce a `<task-notification>` envelope to the parts worth embedding.
///
/// Why (issue #4972): 65.3% of logged hook prompts are these envelopes, and
/// their `<task-id>` / `<tool-use-id>` / `<output-file>` head is a median 253
/// tokens — 49% of the whole window — of hashes and absolute paths. Embedding
/// them means the vector describes the harness, not the question.
/// What: returns `Some(inner)` with the `<summary>` and `<result>` bodies
/// joined by a blank line when the prompt opens with the envelope and at least
/// one of those elements has content; `None` otherwise, so a plain prompt is
/// passed through untouched. An unterminated element (the hook's 64 KiB stdin
/// cap can cut the envelope mid-`<result>`) is taken to end of input.
/// Test: `envelope_strip_recovers_the_payload`,
/// `envelope_strip_tolerates_a_cut_envelope`,
/// `short_query_passes_through_untouched`.
fn strip_notification_envelope(prompt: &str) -> Option<String> {
    let trimmed = prompt.trim_start();
    if !trimmed.starts_with(ENVELOPE_OPEN) {
        return None;
    }
    let kept: Vec<&str> = ENVELOPE_KEEP
        .iter()
        .filter_map(|tag| element_text(trimmed, tag))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if kept.is_empty() {
        return None;
    }
    Some(kept.join("\n\n"))
}

/// Inner text of the first `<tag>…</tag>` in `haystack`.
///
/// What: returns the span between the open and close markers, or from the open
/// marker to end of input when the close marker is absent. `None` when the open
/// marker is absent.
/// Test: `envelope_strip_tolerates_a_cut_envelope`.
fn element_text<'a>(haystack: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let start = haystack.find(&open)? + open.len();
    let rest = &haystack[start..];
    let close = format!("</{tag}>");
    Some(match rest.find(&close) {
        Some(end) => &rest[..end],
        None => rest,
    })
}

/// Keep whole leading units of `text` that fit inside `budget_tokens`.
///
/// Why (the owner's ruling on #5037, part 3 — "handle whole units or nothing"):
/// the embedder's own cut lands wherever token 512 falls, which is routinely
/// mid-word and always unannounced. A cut on a unit boundary is a cut a reader
/// can reason about, and the count of dropped units is what makes it reportable.
/// What: no-ops when `text` already fits. Otherwise packs whole lines from the
/// head; when the first line alone busts the budget, packs whole words from that
/// line instead — the finest unit that still never splits a word into a token
/// fragment. Returns the kept text and the number of units dropped.
/// Head-first, because after the envelope strip the payload's opening is its
/// thesis: a `<result>` block leads with the agent's overview, a typed prompt
/// leads with the ask.
/// Test: `over_window_query_is_reduced_to_whole_units`,
/// `single_oversized_line_falls_back_to_whole_words`.
fn pack_whole_units(text: &str, budget_tokens: usize) -> (String, usize) {
    if estimate_tokens(text) <= budget_tokens {
        return (text.to_string(), 0);
    }
    let lines: Vec<&str> = text.lines().collect();
    let (kept, dropped) = pack_units(&lines, "\n", budget_tokens);
    if !kept.trim().is_empty() {
        return (kept, dropped);
    }
    // Not one whole line fits. Drop to words within the first line, and count
    // the lines we never even reached.
    let first = lines.first().copied().unwrap_or(text);
    let words: Vec<&str> = first.split_whitespace().collect();
    let (kept, dropped_words) = pack_units(&words, " ", budget_tokens);
    (kept, dropped_words + lines.len().saturating_sub(1))
}

/// Greedily take leading `units` whose combined token estimate fits `budget`.
///
/// What: accumulates [`piece_tokens`] per unit (whitespace separators cost
/// nothing, so the sum is exact under [`estimate_tokens`]) and stops at the
/// first unit that would overflow. Returns the joined survivors and the count
/// of units not taken.
/// Test: `over_window_query_is_reduced_to_whole_units`.
fn pack_units(units: &[&str], sep: &str, budget: usize) -> (String, usize) {
    let mut used = SPECIAL_TOKEN_OVERHEAD;
    let mut taken = 0usize;
    for unit in units {
        let cost = piece_tokens(unit);
        if used + cost > budget {
            break;
        }
        used += cost;
        taken += 1;
    }
    (units[..taken].join(sep), units.len() - taken)
}

/// Estimated WordPiece token count for `text`, including `[CLS]`/`[SEP]`.
///
/// Why: see [`CHARS_PER_SUBTOKEN`] for the calibration and why this estimator
/// deliberately errs high.
/// What: [`SPECIAL_TOKEN_OVERHEAD`] plus [`piece_tokens`].
/// Test: `token_estimate_never_splits_a_unit`.
pub(super) fn estimate_tokens(text: &str) -> usize {
    SPECIAL_TOKEN_OVERHEAD + piece_tokens(text)
}

/// Estimated token count of `text` excluding the special-token overhead.
///
/// Why: kept separate from [`estimate_tokens`] because it is additive over
/// whitespace-joined concatenation, which is what makes [`pack_units`] exact
/// rather than approximate.
/// What: mirrors BERT basic tokenization — each run of alphanumeric/`_`
/// characters costs `ceil(len / CHARS_PER_SUBTOKEN)` sub-tokens, each
/// non-whitespace punctuation character costs 1, whitespace costs 0.
/// Test: `token_estimate_never_splits_a_unit`.
fn piece_tokens(text: &str) -> usize {
    let mut total = 0usize;
    let mut run = 0usize;
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            run += 1;
            continue;
        }
        total += run.div_ceil(CHARS_PER_SUBTOKEN);
        run = 0;
        if !ch.is_whitespace() {
            total += 1;
        }
    }
    total + run.div_ceil(CHARS_PER_SUBTOKEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (#4972): the measured shape of the defect — 65.3% of hook prompts
    /// open with this envelope and its head burns a median 253 of 512 tokens on
    /// task ids and absolute paths before any payload appears.
    /// What: a realistic envelope through the strip; asserts the framing is gone
    /// and both payload elements survive.
    /// Test: itself.
    #[test]
    fn envelope_strip_recovers_the_payload() {
        let raw = concat!(
            "<task-notification>\n",
            "<task-id>a23c46a0439fa7881</task-id>\n",
            "<tool-use-id>toolu_01PvoC76SpHX65DVbJi7sPes</tool-use-id>\n",
            "<output-file>/private/tmp/claude-502/-Users-masa-projects/tasks/a23.output",
            "</output-file>\n",
            "<status>completed</status>\n",
            "<summary>Agent \"retrieval floor\" finished</summary>\n",
            "<note>A task-notification fires each time this agent stops.</note>\n",
            "<result>The relevance floor lands at 0.35.</result>\n",
            "</task-notification>"
        );
        let out = strip_notification_envelope(raw).expect("envelope must be recognised");
        assert!(
            out.contains("retrieval floor") && out.contains("lands at 0.35"),
            "summary and result must both survive; got:\n{out}"
        );
        for framing in [
            "task-id",
            "toolu_01PvoC76",
            "/private/tmp",
            "<status>",
            "fires each time",
        ] {
            assert!(
                !out.contains(framing),
                "envelope framing `{framing}` must not reach the embedder; got:\n{out}"
            );
        }
    }

    /// Why (#4972): `read_stdin_best_effort` caps stdin at 64 KiB, so a long
    /// agent report arrives with `</result>` sliced off. Bailing out there would
    /// send the full envelope — the exact case the fix exists for.
    /// What: an envelope with no closing tags; asserts the payload is still
    /// recovered to end of input.
    /// Test: itself.
    #[test]
    fn envelope_strip_tolerates_a_cut_envelope() {
        let raw = "<task-notification>\n<task-id>abc</task-id>\n<result>payload survives";
        let out = strip_notification_envelope(raw).expect("cut envelope must still strip");
        assert_eq!(out, "payload survives");
    }

    /// Why (#4972): the fix must be invisible to the 48% of prompts that
    /// already fit. A prompt that is not an envelope and is inside the budget
    /// has to arrive at `/recall` byte-identical.
    /// What: a plain short prompt through `shape_recall_query`.
    /// Test: itself.
    #[test]
    fn short_query_passes_through_untouched() {
        let prompt = "how does the relevance floor interact with top_k?";
        let shaped = shape_recall_query(prompt, QUERY_TOKEN_BUDGET);
        assert_eq!(shaped.text, prompt, "a fitting prompt must not be reshaped");
        assert!(
            !shaped.shape.reshaped(),
            "nothing to report on a clean pass"
        );
        assert_eq!(shaped.shape.units_dropped, 0);
        assert!(!shaped.shape.envelope_stripped);
    }

    /// Why (#4972, the defect): an over-window prompt used to go out whole and
    /// be cut mid-word at token 512 with nothing recorded. It must now leave as
    /// whole units, inside the budget, with the reduction on the record.
    /// What: a 400-line prompt far past the window; asserts the sent query fits
    /// the budget, ends on a line boundary (no partial unit), and that the shape
    /// reports both the original size and the dropped-unit count.
    /// Test: itself.
    #[test]
    fn over_window_query_is_reduced_to_whole_units() {
        let line = "explain how the retrieval relevance floor interacts with the top_k cap";
        let prompt = vec![line; 400].join("\n");
        let shaped = shape_recall_query(&prompt, QUERY_TOKEN_BUDGET);

        assert!(
            shaped.shape.original_tokens > QUERY_TOKEN_BUDGET,
            "fixture must exceed the window; got {} tokens",
            shaped.shape.original_tokens
        );
        assert!(
            shaped.shape.sent_tokens <= QUERY_TOKEN_BUDGET,
            "the sent query must fit the window; got {} tokens",
            shaped.shape.sent_tokens
        );
        assert!(
            shaped.shape.units_dropped > 0 && shaped.shape.reshaped(),
            "the reduction must be reported, not silent: {:?}",
            shaped.shape
        );
        // Whole units only: every retained line is intact.
        for kept in shaped.text.lines() {
            assert_eq!(kept, line, "a partial unit reached the wire: {kept:?}");
        }
        assert!(
            !shaped.text.is_empty(),
            "reduction must not empty the query"
        );
    }

    /// Why (the ruling's "whole units or nothing"): a single line longer than
    /// the whole budget has no line-sized unit that fits. Falling back to words
    /// keeps the guarantee instead of abandoning it.
    /// What: one very long single-line prompt; asserts the result fits, is a
    /// prefix of whole words, and reports the drop.
    /// Test: itself.
    #[test]
    fn single_oversized_line_falls_back_to_whole_words() {
        let prompt = vec!["retrieval"; 2000].join(" ");
        let shaped = shape_recall_query(&prompt, QUERY_TOKEN_BUDGET);
        assert!(shaped.shape.sent_tokens <= QUERY_TOKEN_BUDGET);
        assert!(shaped.shape.units_dropped > 0);
        assert!(
            shaped.text.split_whitespace().all(|w| w == "retrieval"),
            "word fallback must not split a word"
        );
    }

    /// Why (#4972): the estimator is the only thing standing between the packer
    /// and a silent overrun, so its conservatism is load-bearing. It must count
    /// punctuation and sub-word runs, never undercount a long identifier as one
    /// token, and stay additive across a whitespace join so `pack_units` is
    /// exact.
    /// What: pins the special-token overhead, the sub-token divisor on a long
    /// run, punctuation cost, and additivity.
    /// Test: itself.
    #[test]
    fn token_estimate_never_splits_a_unit() {
        assert_eq!(estimate_tokens(""), SPECIAL_TOKEN_OVERHEAD);
        // A 12-char run costs ceil(12/3) = 4 sub-tokens, not 1.
        assert_eq!(piece_tokens("abcdefghijkl"), 4);
        // Punctuation is its own token; whitespace is free.
        assert_eq!(piece_tokens("ab, cd"), 1 + 1 + 1);
        // Additive across a whitespace join — the property `pack_units` relies on.
        let units = ["alpha beta", "gamma-delta", "epsilon"];
        let joined = units.join(" ");
        assert_eq!(
            piece_tokens(&joined),
            units.iter().map(|u| piece_tokens(u)).sum::<usize>()
        );
    }

    /// Why: an operator typo in the budget must not disable recall or hand the
    /// embedder an unbounded query.
    /// What: exercises unset, valid, zero, below-floor, above-ceiling, and
    /// unparseable inputs.
    /// Test: itself.
    #[test]
    fn configured_query_budget_clamps_to_bounds() {
        assert_eq!(clamp_query_budget(None), QUERY_TOKEN_BUDGET);
        assert_eq!(clamp_query_budget(Some("256")), 256);
        assert_eq!(clamp_query_budget(Some(" 1024 ")), 1024);
        assert_eq!(clamp_query_budget(Some("0")), QUERY_TOKEN_BUDGET);
        assert_eq!(clamp_query_budget(Some("1")), MIN_QUERY_TOKENS);
        assert_eq!(clamp_query_budget(Some("999999")), MAX_QUERY_TOKENS);
        assert_eq!(clamp_query_budget(Some("nonsense")), QUERY_TOKEN_BUDGET);
    }
}

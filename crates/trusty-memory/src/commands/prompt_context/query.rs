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
//! `short_query_passes_through_untouched`,
//! `token_estimate_covers_the_measured_scripts`, `cjk_is_one_token_per_char`,
//! `latin_compounds_are_not_underestimated`,
//! `unbreakable_oversized_token_still_yields_a_query`,
//! `configured_query_budget_clamps_to_bounds`.
//!
//! Two token counts travel with every shaped query, because one is not enough
//! (#4972, round-3 review). [`estimate_tokens`] is what the packer spends. Four
//! of its five branches are arithmetic upper bounds, but ASCII-letter runs are
//! charged by a divisor calibrated on real prompts — and **no divisor above 1
//! token per character bounds them**: nine random letters cost nine tokens and
//! every divisor charges fewer. [`max_tokens`] closes that gap by charging those
//! runs 1 token per character, which *is* a bound.
//!
//! The packer spends the estimate; the shape reports both. That is what lets
//! [`RecallQueryShape::may_exceed_window`] say "this send is not provably inside
//! the window" instead of the shape asserting a clean pass it cannot support. An
//! under-estimate nothing flags is the fail-open shape the review named: the
//! embedder cuts the query silently *and* the metric reports no loss.
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

/// Characters per WordPiece sub-token inside a run of ASCII *letters*.
///
/// Why: this is the packer's charge, and it is a **calibration, not a bound** —
/// the one branch of [`piece_tokens`] not derivable from the tokenizer's own
/// rules. Measured against the model's `tokenizer.json` vocabulary (30,522
/// entries, greedy longest match), at the point where each class of input just
/// fills a 512-token budget:
///
/// | input class | est/true at divisor 3 | at divisor 2 |
/// |---|---|---|
/// | English prose | 1.70 | 2.57 |
/// | German compounds | 0.98 | 1.40 |
/// | Dutch compounds | 0.94 | 1.40 |
/// | Turkish agglutinative | 0.93 | 1.35 |
/// | Finnish compounds | 0.92 | 1.34 |
/// | Hungarian agglutinative | 0.89 | 1.27 |
/// | Welsh | 0.75 | 1.06 |
/// | nine random letters per word | 0.55 | 0.92 |
///
/// Divisor 3 was calibrated on an English corpus and underestimates every
/// compound-word language in that table — the round-3 HIGH. Divisor 2 is the
/// coarsest charge that clears all of them with margin.
///
/// It does not clear the last row, and no divisor can: `piece_tokens` charges a
/// nine-letter run 5 against a true 9, and only 1 token per character bounds it.
/// Adopting that as the packer's charge would deliver a measured 104 true tokens
/// of the 512-token window instead of 189. So the packer keeps the divisor and
/// [`max_tokens`] carries the bound separately — the residual becomes visible
/// instead of becoming a false guarantee.
///
/// The cost is real and paid by English: a prompt now delivers a measured 189
/// true tokens into the window rather than 291 under divisor 3.
/// [`ENV_QUERY_TOKEN_BUDGET`] raises it back for an operator who prefers the
/// old trade.
///
/// The repo's `inference::bedrock::cache::estimate_tokens` uses chars/4 for a
/// *different* question (is this prefix big enough to be worth caching), where
/// an over-estimate is the harmful direction.
/// Test: `token_estimate_covers_the_measured_scripts`,
/// `latin_compounds_are_not_underestimated`.
const CHARS_PER_SUBTOKEN: usize = 2;

/// Tokens charged per character of a non-ASCII, non-CJK run (#4972 review).
///
/// Why: the tokenizer's vocabulary is overwhelmingly English WordPiece, so a
/// Cyrillic, Greek, Arabic, Hebrew or Hangul run shatters into near-character
/// fragments — and `BertNormalizer` NFD-decomposes precomposed syllables first,
/// which multiplies Hangul further. Measured against the real tokenizer, the
/// worst case is Korean at ~2.4 tokens per character. Charging `3` rounds up
/// from that worst observed rate, the same round-up-from-the-worst discipline
/// `DEFAULT_RELEVANCE_FLOOR` uses. It over-charges Cyrillic ~3× and Greek ~3×,
/// which costs those prompts some window — the deliberate trade, because the
/// alternative is the embedder cutting them silently while
/// [`RecallQueryShape`] reports `units_dropped: 0`.
/// Test: `token_estimate_covers_the_measured_scripts`.
const NON_ASCII_TOKENS_PER_CHAR: usize = 3;

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
    // #4972: the bound travels with the estimate so the shape can distinguish
    // "this send fits the window" from "this send might not".
    let sent_tokens_max = max_tokens(&text);
    ShapedQuery {
        shape: RecallQueryShape {
            original_tokens,
            sent_tokens,
            sent_tokens_max,
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
///
/// `may_exceed_window` rides the warn but does not trigger one. A send whose
/// bound exceeds the budget is common — [`max_tokens`] runs ~2.5× the true count
/// on English, so most prompts near the budget qualify — and warning on all of
/// them is a warn nobody reads. It is a *log field*, queryable across the corpus
/// with `jq 'select(.recall_query.sent_tokens_max > .recall_query.budget_tokens)'`;
/// the warn stays reserved for a query this module actually changed.
/// Test: side-effect only; the shape it reports is covered by
/// `over_window_query_is_reduced_to_whole_units`.
pub(super) fn warn_if_reshaped(shape: &RecallQueryShape) {
    if !shape.reshaped() {
        return;
    }
    tracing::warn!(
        original_tokens = shape.original_tokens,
        sent_tokens = shape.sent_tokens,
        sent_tokens_max = shape.sent_tokens_max,
        budget_tokens = shape.budget_tokens,
        envelope_stripped = shape.envelope_stripped,
        units_dropped = shape.units_dropped,
        may_exceed_window = shape.may_exceed_window(),
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
/// What: returns `Some(inner)` with the `<summary>` and `<result>` bodies —
/// **plus anything the user typed after `</task-notification>`** — joined by
/// blank lines, when the prompt opens with the envelope and at least one of
/// those parts has content. Returns `None` otherwise, so a plain prompt and an
/// envelope with no payload at all both pass through untouched rather than
/// becoming an empty query. An unterminated element (the hook's 64 KiB stdin
/// cap can cut the envelope mid-`<result>`) is taken to end of input.
///
/// The trailing-text clause is the #4972 review's finding 2: a user who appends
/// an instruction after the notification block wrote the most intent-bearing
/// text in the prompt, and dropping it uncounted is the same defect class this
/// module closes. It is placed **first** in the joined result, not last:
/// [`pack_whole_units`] packs head-first, so appending it made the text this
/// clause exists to rescue the first thing the packer dropped on any over-budget
/// envelope.
/// Test: `envelope_strip_recovers_the_payload`,
/// `envelope_strip_keeps_text_after_the_envelope`,
/// `envelope_trailing_instruction_outranks_the_payload`,
/// `envelope_strip_tolerates_a_cut_envelope`,
/// `envelope_with_no_payload_passes_through_whole`,
/// `short_query_passes_through_untouched`.
fn strip_notification_envelope(prompt: &str) -> Option<String> {
    let trimmed = prompt.trim_start();
    if !trimmed.starts_with(ENVELOPE_OPEN) {
        return None;
    }
    let mut kept: Vec<&str> = ENVELOPE_KEEP
        .iter()
        .filter_map(|tag| element_text(trimmed, tag))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    // #4972: text following the closing tag is the user speaking, not the
    // harness. It outranks everything above it and must never be dropped.
    const ENVELOPE_CLOSE: &str = "</task-notification>";
    if let Some(idx) = trimmed.rfind(ENVELOPE_CLOSE) {
        let tail = trimmed[idx + ENVELOPE_CLOSE.len()..].trim();
        if !tail.is_empty() {
            // #4972: `pack_whole_units` packs head-first, so appending the tail
            // put the highest-ranked text first in line to be dropped. Ranking
            // it first in the text is what makes the ranking real.
            kept.insert(0, tail);
        }
    }
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
///
/// Last resort (#4972 review, finding 3): when even one whole *word* does not
/// fit — a base64 blob, a `data:` URI, a JWT, a minified bundle, all of which
/// are a single unbroken token far past the budget — the whole-unit rule cannot
/// be honoured at all. It falls back to a character prefix rather than sending
/// an empty query, because an empty query makes `fetch_palace_recall` return
/// zero drawers, which is strictly worse than the truncation this module
/// replaced. The reduction is still counted, so it is never silent.
/// Test: `over_window_query_is_reduced_to_whole_units`,
/// `single_oversized_line_falls_back_to_whole_words`,
/// `unbreakable_oversized_token_still_yields_a_query`.
fn pack_whole_units(text: &str, budget_tokens: usize) -> (String, usize) {
    if estimate_tokens(text) <= budget_tokens {
        return (text.to_string(), 0);
    }
    let lines: Vec<&str> = text.lines().collect();
    let (kept, dropped) = pack_units(&lines, "\n", budget_tokens);
    if !kept.trim().is_empty() {
        return (kept, dropped);
    }
    // Not one whole line fits. Drop to words within the first line that has
    // any content, and count the lines we never even reached.
    //
    // #4972: `lines.first()` reopened the empty-query regression this fallback
    // exists to close — one leading `\n` makes line 0 the empty string, which
    // packs zero words and zero characters, and `fetch_palace_recall` returns no
    // drawers at all for an empty query. Skip blank lines the way the packer
    // above already does when it checks `kept.trim()`.
    let first = lines
        .iter()
        .find(|l| !l.trim().is_empty())
        .copied()
        .unwrap_or(text);
    let words: Vec<&str> = first.split_whitespace().collect();
    let (kept, dropped_words) = pack_units(&words, " ", budget_tokens);
    if !kept.trim().is_empty() {
        return (kept, dropped_words + lines.len().saturating_sub(1));
    }
    // Last resort: the unit degrades to the character, so `units_dropped` counts
    // characters here — the ones actually dropped, not the whole input (#4972).
    let kept = pack_chars(first, budget_tokens);
    let dropped = text.chars().count().saturating_sub(kept.chars().count());
    (kept, dropped)
}

/// Fill `budget_tokens` with a character prefix of `text`.
///
/// Why: the only path that reaches this cannot honour the whole-unit rule — the
/// first unit is itself over budget and indivisible. Sending nothing would drop
/// recall entirely for that prompt; a budgeted prefix is what the embedder would
/// have produced anyway, except that here it is bounded by our own estimate and
/// reported by the caller.
/// What: walks `text` one character at a time carrying the same run state
/// [`piece_tokens`] uses, and stops at the last character whose inclusion keeps
/// the running estimate within budget. Single pass — the estimate is advanced
/// incrementally rather than recomputed per prefix. Cuts on a character
/// boundary, so the result is always valid UTF-8.
/// Test: `unbreakable_oversized_token_still_yields_a_query`,
/// `pack_chars_matches_the_estimator`.
fn pack_chars(text: &str, budget_tokens: usize) -> String {
    let mut kept_bytes = 0usize;
    let mut committed = SPECIAL_TOKEN_OVERHEAD;
    let mut run = Run::default();
    for (idx, ch) in text.char_indices() {
        let (next_committed, next_run) = if is_cjk(ch) {
            (committed + run.cost(Charge::Calibrated) + 1, Run::default())
        } else if ch.is_alphanumeric() {
            (committed, run.extended(ch))
        } else {
            (
                committed + run.cost(Charge::Calibrated) + usize::from(!ch.is_whitespace()),
                Run::default(),
            )
        };
        if next_committed + next_run.cost(Charge::Calibrated) > budget_tokens {
            break;
        }
        committed = next_committed;
        run = next_run;
        kept_bytes = idx + ch.len_utf8();
    }
    text[..kept_bytes].to_string()
}

/// Which of the two charging rules to apply to a run of ASCII letters.
///
/// Why (#4972, round-3 review): the ASCII-letters branch is the only one that is
/// not an arithmetic bound, so the module needs both numbers — the calibrated
/// one to pack against, and the bounded one so [`RecallQueryShape`] never claims
/// a fit it cannot prove. Every other branch is identical under both.
/// What: selects [`CHARS_PER_SUBTOKEN`] or 1 token per character.
/// Test: `max_tokens_bounds_the_real_tokenizer`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Charge {
    /// `ceil(len / CHARS_PER_SUBTOKEN)` — calibrated on real prompts, and the
    /// charge the packer spends. Underestimates high-entropy letter runs.
    Calibrated,
    /// One token per character. A true upper bound: every ASCII letter exists in
    /// the vocabulary as both `x` and `##x`, so an *n*-letter word is at most
    /// *n* pieces, and a word over `max_input_chars_per_word` (100) collapses to
    /// a single `[UNK]`.
    Bound,
}

/// One in-progress run of alphanumeric characters, and what it costs.
///
/// Why: [`piece_tokens`] and [`pack_chars`] must charge a run identically or the
/// packer can overshoot the estimate it is packing against. Sharing one type is
/// what keeps them in step.
/// What: the run's length plus the two flags that select its charging rule.
/// Test: `pack_chars_matches_the_estimator`.
#[derive(Default, Clone, Copy)]
struct Run {
    len: usize,
    non_ascii: bool,
    has_digit: bool,
}

impl Run {
    /// This run's token cost under `charge` — see [`piece_tokens`] for the table.
    fn cost(self, charge: Charge) -> usize {
        if self.len == 0 {
            0
        } else if self.non_ascii {
            self.len * NON_ASCII_TOKENS_PER_CHAR
        } else if self.has_digit || charge == Charge::Bound {
            self.len
        } else {
            self.len.div_ceil(CHARS_PER_SUBTOKEN)
        }
    }

    /// This run with `ch` appended.
    fn extended(self, ch: char) -> Self {
        Self {
            len: self.len + 1,
            non_ascii: self.non_ascii || !ch.is_ascii(),
            has_digit: self.has_digit || ch.is_ascii_digit(),
        }
    }
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
        let cost = piece_tokens(unit, Charge::Calibrated);
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
/// Why: this is the number the packer spends. See [`CHARS_PER_SUBTOKEN`] for the
/// calibration, and [`max_tokens`] for the bound it is deliberately not.
/// What: [`SPECIAL_TOKEN_OVERHEAD`] plus [`piece_tokens`] under
/// [`Charge::Calibrated`].
/// Test: `token_estimate_never_splits_a_unit`.
pub(super) fn estimate_tokens(text: &str) -> usize {
    SPECIAL_TOKEN_OVERHEAD + piece_tokens(text, Charge::Calibrated)
}

/// Upper bound on the true WordPiece token count of `text`.
///
/// Why (#4972, round-3 review): [`estimate_tokens`] charges ASCII-letter runs by
/// a calibrated divisor, and no divisor above 1 token per character bounds them
/// — nine random letters truly cost nine tokens. Packing against a number that
/// can be too low is tolerable; *reporting* it as though the send provably fits
/// the embedder window is not. That is the fail-open shape the review named: the
/// embedder cuts the query and the metric says nothing was lost. This function
/// is the honest ceiling that lets [`RecallQueryShape::may_exceed_window`]
/// distinguish "this send fits" from "this send might not".
/// What: [`piece_tokens`] under [`Charge::Bound`] — identical to
/// [`estimate_tokens`] except that ASCII-letter runs cost 1 token per character.
/// It is a bound, not an estimate: on English prose it runs ~2.5× the true count.
/// Test: `max_tokens_bounds_the_real_tokenizer`,
/// `shape_flags_a_send_it_cannot_prove_fits`.
pub(super) fn max_tokens(text: &str) -> usize {
    SPECIAL_TOKEN_OVERHEAD + piece_tokens(text, Charge::Bound)
}

/// Estimated token count of `text` excluding the special-token overhead.
///
/// Why: kept separate from [`estimate_tokens`] because it is additive over
/// whitespace-joined concatenation, which is what makes [`pack_units`] exact
/// rather than approximate. Runs never span whitespace, so splitting a string
/// on whitespace and summing the parts gives the same answer as measuring the
/// whole — the property [`pack_units`] relies on.
/// What: mirrors BERT basic tokenization, charging each run by what the
/// tokenizer actually does to that class of characters:
///
/// | run | charge | why |
/// |---|---|---|
/// | one CJK codepoint | 1 token | `handle_chinese_chars: true` pads every Han character with spaces before pre-tokenization, so it *is* one token — arithmetic, not estimate |
/// | any run containing non-ASCII | [`NON_ASCII_TOKENS_PER_CHAR`] per char | the vocabulary barely covers these scripts |
/// | ASCII run containing a digit | 1 token per char | hex digests, base64, UUIDs and JWTs measured 0.42–0.61 against `ceil(len/3)`, i.e. ≈1 token per char |
/// | ASCII letters only | [`CHARS_PER_SUBTOKEN`], or 1 per char under [`Charge::Bound`] | the one calibrated branch |
/// | any other non-whitespace char | 1 token | punctuation, including `_`, which `BertPreTokenizer` splits on rather than treating as a word character |
/// | whitespace | 0 | |
///
/// Four of those five branches are arithmetic bounds — they follow from what the
/// normalizer and pre-tokenizer do, not from a corpus. The ASCII-letters branch
/// is the exception, and calling it a bound was the round-3 HIGH: `ceil(len/2)`
/// charges a nine-letter run 5 against a true 9. [`Charge::Bound`] is that
/// branch made exact, which is what [`max_tokens`] uses so the shape can report
/// the gap rather than paper over it.
/// Test: `token_estimate_covers_the_measured_scripts`, `cjk_is_one_token_per_char`,
/// `max_tokens_bounds_the_real_tokenizer`.
fn piece_tokens(text: &str, charge: Charge) -> usize {
    let mut total = 0usize;
    let mut run = Run::default();
    for ch in text.chars() {
        if is_cjk(ch) {
            total += run.cost(charge) + 1;
            run = Run::default();
            continue;
        }
        if ch.is_alphanumeric() {
            run = run.extended(ch);
            continue;
        }
        total += run.cost(charge) + usize::from(!ch.is_whitespace());
        run = Run::default();
    }
    total + run.cost(charge)
}

/// Whether `ch` is one of the CJK blocks `BertNormalizer` space-pads.
///
/// Why: `handle_chinese_chars: true` in the model's `tokenizer.json` wraps each
/// of these codepoints in spaces before pre-tokenization, which makes it exactly
/// one token. Charging a shared `ceil(n/3)` across a Han run undercounts it 3×,
/// every time — the arithmetic core of the #4972 review's HIGH finding.
/// What: the ranges HuggingFace's `is_chinese_char` uses, verbatim.
/// Test: `cjk_is_one_token_per_char`.
fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x4E00..=0x9FFF
        | 0x3400..=0x4DBF
        | 0x20000..=0x2A6DF
        | 0x2A700..=0x2B73F
        | 0x2B740..=0x2B81F
        | 0x2B820..=0x2CEAF
        | 0xF900..=0xFAFF
        | 0x2F800..=0x2FA1F)
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
    /// run, punctuation cost (including `_`, which `BertPreTokenizer` splits on),
    /// and additivity.
    /// Test: itself.
    #[test]
    fn token_estimate_never_splits_a_unit() {
        assert_eq!(estimate_tokens(""), SPECIAL_TOKEN_OVERHEAD);
        // A 12-char run costs ceil(12/2) = 6 sub-tokens, not 1.
        assert_eq!(piece_tokens("abcdefghijkl", Charge::Calibrated), 6);
        // Punctuation is its own token; whitespace is free.
        assert_eq!(piece_tokens("ab, cd", Charge::Calibrated), 1 + 1 + 1);
        // `_` splits the run and costs 1 — it is punctuation to the tokenizer,
        // so `a_b` is three tokens, not one two-char run.
        assert_eq!(piece_tokens("a_b", Charge::Calibrated), 1 + 1 + 1);
        // Additive across a whitespace join under both charges — the property
        // `pack_units` relies on, and `max_tokens` must not break.
        let units = ["alpha beta", "gamma-delta", "epsilon"];
        let joined = units.join(" ");
        for charge in [Charge::Calibrated, Charge::Bound] {
            assert_eq!(
                piece_tokens(&joined, charge),
                units.iter().map(|u| piece_tokens(u, charge)).sum::<usize>()
            );
        }
    }

    /// Why (#4972 review, the HIGH): the previous estimator charged every
    /// alphanumeric run `ceil(len/3)` regardless of script, which undercounts
    /// the real tokenizer by 2–5× on CJK (0.34), Korean (0.18), Cyrillic (0.39),
    /// Greek (0.41), hex digests (0.42), base64 (0.50) and UUIDs (0.61). A
    /// 1520-character Chinese prompt estimated 509 tokens, passed through
    /// untouched, lost 66% of itself inside the embedder — and the new
    /// `recall_query` metric then reported `units_dropped: 0`, an alarm
    /// asserting there was no loss. Under-reporting a loss is worse than the
    /// loss.
    /// What: the safe direction, pinned across the script classes that were
    /// actually measured. Each case carries the true WordPiece count for that
    /// exact string, measured against the model's own `tokenizer.json`
    /// vocabulary (30,522 entries, `handle_chinese_chars: true`, NFD +
    /// lowercase) with a greedy-longest-match reference implementation.
    ///
    /// The name says `covers_the_measured_scripts`, not `never_underestimates`,
    /// and the distinction is the round-3 HIGH: a nine-case table cannot pin a
    /// universal property, and the estimator does not have one — see
    /// [`CHARS_PER_SUBTOKEN`] and `max_tokens_bounds_the_real_tokenizer`. Two
    /// figures here were wrong in the previous round and are re-measured: the
    /// JWT case (78, not 94) and the `snake_case` case (442, not 762).
    /// Test: itself.
    #[test]
    fn token_estimate_covers_the_measured_scripts() {
        // (label, input, true WordPiece token count including [CLS]/[SEP])
        let cases: [(&str, String, usize); 9] = [
            (
                "latin prose",
                "The relevance floor lands at 0.35 after measuring the retrieval corpus. "
                    .repeat(3),
                44,
            ),
            (
                "chinese han",
                "检索相关性下限设定为零点三五这是经过语料库测量后得到的结果".repeat(10),
                292,
            ),
            (
                "japanese",
                "検索の関連性の下限はコーパスを測定した結果です".repeat(12),
                278,
            ),
            (
                "korean hangul",
                "검색 관련성 하한은 코퍼스를 측정한 결과입니다 ".repeat(12),
                638,
            ),
            (
                "russian cyrillic",
                "Нижняя граница релевантности поиска установлена после измерения корпуса "
                    .repeat(8),
                458,
            ),
            (
                "greek",
                "Το κατώτατο όριο συνάφειας ανάκτησης ορίστηκε μετά τη μέτρηση ".repeat(8),
                410,
            ),
            (
                "hex digest",
                "9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c ".repeat(24),
                890,
            ),
            (
                "jwt",
                "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0\
                 .dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U"
                    .to_string(),
                78,
            ),
            (
                "snake_case identifiers",
                "filter_drawers_by_relevance_floor_configured ".repeat(40),
                442,
            ),
        ];
        for (label, input, true_tokens) in cases {
            let est = estimate_tokens(&input);
            assert!(
                est >= true_tokens,
                "`{label}` underestimates: estimate {est} < true {true_tokens}. \
                 An underestimate hands the embedder a query it cuts silently while \
                 RecallQueryShape reports no loss — always err high."
            );
        }
    }

    /// Why (#4972, round-3 HIGH-1): the divisor was calibrated on an English
    /// corpus, and every compound-word language underestimates against it —
    /// Hungarian 342 against a true 372, Finnish 362 against 392, Dutch 332
    /// against 362, German landing exactly on 442 with no margin at all. These
    /// are ordinary Latin-script prose, not an adversarial construction, and an
    /// under-estimate is the failure that matters: the embedder cuts the query
    /// and `RecallQueryShape` reports no loss.
    /// What: two halves of the same defect. First the counts, measured against
    /// the model's own `tokenizer.json` through the `tokenizers` crate's own
    /// reference. Then the consequence: a Hungarian prompt sized so the old
    /// divisor scored it at exactly the budget — passed through whole, reported
    /// clean, and cut inside the embedder — must now be seen as over-window and
    /// report its reduction.
    /// Test: itself.
    #[test]
    fn latin_compounds_are_not_underestimated() {
        // (label, unit, repeats, true WordPiece count including [CLS]/[SEP])
        let cases: [(&str, &str, usize, usize); 4] = [
            (
                "german compounds",
                "Die Rechtsschutzversicherungsgesellschaft veroeffentlichte \
                 Geschwindigkeitsbegrenzungen und Arbeiterunfallversicherungsgesetze. ",
                10,
                442,
            ),
            (
                "hungarian agglutinative",
                "Megszentsegtelenithetetlensegeskedeseitekert \
                 elkelkaposztastalanitottatok viszontelnezhetetlenseg. ",
                10,
                372,
            ),
            (
                "finnish compounds",
                "Lentokonesuihkuturbiinimoottoriapumekaanikkoaliupseerioppilas \
                 jarjestelmallistyttamattomyydellansakaan. ",
                10,
                392,
            ),
            (
                "dutch compounds",
                "Meervoudigepersoonlijkheidsstoornis levensverzekeringsmaatschappij \
                 aansprakelijkheidsverzekering. ",
                10,
                362,
            ),
        ];
        for (label, unit, repeats, true_tokens) in cases {
            let est = estimate_tokens(&unit.repeat(repeats));
            assert!(
                est >= true_tokens,
                "`{label}` underestimates: estimate {est} < true {true_tokens}. \
                 Ordinary Latin prose, not an edge case — the embedder cuts this \
                 query and RecallQueryShape reports units_dropped: 0."
            );
        }

        // The consequence, at the boundary. Under the old divisor this scored
        // exactly 512 against a 512 budget: passed through untouched, reported
        // clean, and lost 45 tokens inside the embedder.
        let hungarian = "Megszentsegtelenithetetlensegeskedeseitekert \
                         elkelkaposztastalanitottatok viszontelnezhetetlenseg. "
            .repeat(15);
        let shaped = shape_recall_query(&hungarian, QUERY_TOKEN_BUDGET);
        assert!(
            shaped.shape.original_tokens > QUERY_TOKEN_BUDGET,
            "a 557-token Hungarian prompt must be seen as over-window; got {} \
             against a budget of {QUERY_TOKEN_BUDGET}",
            shaped.shape.original_tokens
        );
        assert!(
            shaped.shape.reshaped() && shaped.shape.units_dropped > 0,
            "the reduction must reach the metric, not just happen; got {:?}",
            shaped.shape
        );
    }

    /// Why (#4972, round-3 HIGH-1, the reporting half): correcting the divisor
    /// narrows the gap but cannot close it — no charge above 1 token per
    /// character bounds a run of ASCII letters, so a high-entropy Latin prompt
    /// still estimates under its true cost. What must not survive is the shape
    /// *asserting* a clean pass on that query. `may_exceed_window` is the honest
    /// answer: false means the send provably fits, true means the shape declines
    /// to claim it.
    /// What: 800 characters of nine-letter nonsense words — estimated 402,
    /// truly 562, so the embedder cuts it while nothing is reshaped. Asserts the
    /// query passes through (the estimate cleared the budget), that the shape
    /// nonetheless flags it, and that an ordinary short prompt is *not* flagged
    /// so the signal means something.
    /// Test: itself.
    #[test]
    fn shape_flags_a_send_it_cannot_prove_fits() {
        let high_entropy = "qzjvxwkfy ".repeat(80);
        let shaped = shape_recall_query(&high_entropy, QUERY_TOKEN_BUDGET);
        assert!(
            !shaped.shape.reshaped(),
            "the estimate clears the budget here — that is the premise of the \
             test; got {:?}",
            shaped.shape
        );
        assert!(
            shaped.shape.may_exceed_window(),
            "the true cost is 562 against a 512 window: the shape must not \
             report a clean pass it cannot prove; got {:?}",
            shaped.shape
        );

        let ordinary = "how does the relevance floor interact with top_k?";
        let shaped = shape_recall_query(ordinary, QUERY_TOKEN_BUDGET);
        assert!(
            !shaped.shape.may_exceed_window(),
            "a short prompt provably fits — a flag that is always on says \
             nothing; got {:?}",
            shaped.shape
        );
    }

    /// Why: [`max_tokens`] is the only number in this module that claims to be a
    /// bound, so the claim has to hold on the inputs the calibrated estimator
    /// gets wrong, not just on the ones it gets right.
    /// What: the same measured corpus as
    /// `token_estimate_covers_the_measured_scripts`, plus the two classes where
    /// the divisor underestimates by construction — high-entropy letter runs and
    /// a short repeated-letter word. Asserts `max_tokens >= true` throughout,
    /// and that it really is looser than the estimate rather than an alias.
    /// Test: itself.
    #[test]
    fn max_tokens_bounds_the_real_tokenizer() {
        // (label, input, true WordPiece count including [CLS]/[SEP])
        let cases: [(&str, String, usize); 4] = [
            ("nine random letters", "qzjvxwkfy ".repeat(80), 562),
            ("repeated letter", "qqqqqqqqq".to_string(), 11),
            (
                "hungarian agglutinative",
                "Megszentsegtelenithetetlensegeskedeseitekert \
                 elkelkaposztastalanitottatok viszontelnezhetetlenseg. "
                    .repeat(10),
                372,
            ),
            (
                "hex digest",
                "9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c ".repeat(24),
                890,
            ),
        ];
        for (label, input, true_tokens) in &cases {
            assert!(
                max_tokens(input) >= *true_tokens,
                "`{label}` breaks the bound: max_tokens {} < true {true_tokens}",
                max_tokens(input)
            );
        }
        // The first two are exactly where the divisor fails, which is the whole
        // reason the bound exists as a second number.
        for (label, input, true_tokens) in &cases[..2] {
            assert!(
                estimate_tokens(input) < *true_tokens,
                "`{label}` was chosen because the calibrated estimate \
                 underestimates it; if that stopped being true this test no \
                 longer proves the bound is load-bearing"
            );
        }
    }

    /// Why (#4972 review): the CJK case is arithmetic, not sampling.
    /// `handle_chinese_chars: true` pads every Han codepoint with spaces before
    /// pre-tokenization, so *n* Han characters are *n* tokens; charging
    /// `ceil(n/3)` is a guaranteed 3× undercount on every Chinese prompt.
    /// What: asserts one token per Han character exactly, and that a CJK prompt
    /// long enough to overrun the window is actually reduced rather than waved
    /// through.
    /// Test: itself.
    #[test]
    fn cjk_is_one_token_per_char() {
        let han = "检索相关性下限";
        assert_eq!(han.chars().count(), 7);
        assert_eq!(
            piece_tokens(han, Charge::Calibrated),
            7,
            "each Han codepoint is its own token"
        );
        assert!(is_cjk('检') && is_cjk('検') && !is_cjk('a') && !is_cjk('я'));

        // The boundary case from the review: ~1520 Chinese characters used to
        // estimate 509 and sail through the 512 budget while the embedder kept
        // a third of it.
        let long_han = "检索相关性下限设定为零点三五".repeat(110);
        assert!(long_han.chars().count() > 1_500);
        let shaped = shape_recall_query(&long_han, QUERY_TOKEN_BUDGET);
        assert!(
            shaped.shape.original_tokens > QUERY_TOKEN_BUDGET,
            "a 1500-character Chinese prompt must be seen as over-window; got {} tokens",
            shaped.shape.original_tokens
        );
        assert!(
            shaped.shape.reshaped() && shaped.shape.units_dropped > 0,
            "it must be reduced and the reduction reported; got {:?}",
            shaped.shape
        );
        assert!(shaped.shape.sent_tokens <= QUERY_TOKEN_BUDGET);
    }

    /// Why (#4972 review, finding 3): a first line that is one unbroken token
    /// past the budget — a base64 blob, a `data:` URI, a JWT, a minified bundle
    /// — packs zero whole words, and an empty query makes `fetch_palace_recall`
    /// return zero drawers. Pre-fix that prompt still got prefix-based recall,
    /// so an empty query would be a regression this PR introduced.
    /// What: a 6,000-character unbroken token; asserts the query is non-empty,
    /// within budget, a genuine prefix of the input, and that the loss is
    /// counted rather than silent.
    ///
    /// The leading-whitespace variants are the round-3 HIGH-2 (#4972). The
    /// guard above had a one-character bypass: `lines.first()` on `"\n" + blob`
    /// is the empty string, which packs no words and no characters, so the
    /// query went out empty and `fetch_palace_recall` returned nothing — the
    /// exact regression the bare-blob case was added to close. The bare fixture
    /// could not catch it, so the case belongs in this test rather than beside
    /// it.
    /// Test: itself.
    #[test]
    fn unbreakable_oversized_token_still_yields_a_query() {
        let blob = "a".repeat(6_000);
        for (label, prompt) in [
            ("bare", blob.clone()),
            ("one leading newline", format!("\n{blob}")),
            ("several blank lines", format!("\n\n\n{blob}")),
            ("space-only first line", format!("   \n{blob}")),
            ("crlf", format!("\r\n{blob}")),
        ] {
            let shaped = shape_recall_query(&prompt, QUERY_TOKEN_BUDGET);
            assert!(
                !shaped.text.trim().is_empty(),
                "`{label}`: an unbreakable oversized token must not produce an \
                 empty query — that returns zero drawers, worse than the \
                 truncation it replaced"
            );
            assert!(
                shaped.text.len() < prompt.len(),
                "`{label}`: it must still be reduced"
            );
            assert!(
                blob.starts_with(shaped.text.trim()),
                "`{label}`: the query must be a prefix of the payload; got {:?}",
                shaped.text
            );
            assert!(
                shaped.shape.sent_tokens <= QUERY_TOKEN_BUDGET,
                "`{label}`: got {} tokens",
                shaped.shape.sent_tokens
            );
            assert!(
                shaped.shape.reshaped() && shaped.shape.units_dropped > 0,
                "`{label}`: the reduction must be reported; got {:?}",
                shaped.shape
            );
            // #4972 round-3 LOW-1: on this path the unit is the character, so
            // the count is characters *dropped* — not the size of the input.
            assert!(
                shaped.shape.units_dropped < prompt.chars().count(),
                "`{label}`: units_dropped {} must count what was dropped, not \
                 the whole input ({} chars)",
                shaped.shape.units_dropped,
                prompt.chars().count()
            );
        }
    }

    /// Why (#4972, round-3 MEDIUM-1): `strip_notification_envelope` documents
    /// the text after `</task-notification>` as outranking everything above it
    /// — it is the user speaking rather than the harness — and then appended it
    /// last. `pack_whole_units` packs head-first, so on any over-budget envelope
    /// the one line the rescue exists for was the first line dropped. The fix
    /// only worked when nothing needed dropping.
    /// What: an envelope whose `<result>` alone busts the budget, followed by a
    /// short user instruction. Asserts the instruction survives all the way
    /// through `shape_recall_query`, not merely through the strip.
    /// Test: itself.
    #[test]
    fn envelope_trailing_instruction_outranks_the_payload() {
        let instruction = "now open the PR and set the milestone";
        let bulky = "The agent swept the retrieval floor and reported back. ".repeat(400);
        let raw = format!(
            "<task-notification>\n\
             <task-id>abc123</task-id>\n\
             <result>{bulky}</result>\n\
             </task-notification>\n\n\
             {instruction}"
        );
        let shaped = shape_recall_query(&raw, QUERY_TOKEN_BUDGET);
        assert!(
            shaped.shape.units_dropped > 0,
            "fixture must actually overflow the budget for this to mean \
             anything; got {:?}",
            shaped.shape
        );
        assert!(
            shaped.text.contains(instruction),
            "the user's instruction outranks the agent report and must survive \
             the packer, not just the strip; got:\n{}",
            shaped.text
        );
    }

    /// Why: `pack_chars` re-implements the run accounting incrementally, so it
    /// can drift from `piece_tokens` and overshoot the budget it packs against.
    /// What: packs prefixes of mixed-script input at several budgets and asserts
    /// the independently-computed estimate of each result is within budget.
    /// Test: itself.
    #[test]
    fn pack_chars_matches_the_estimator() {
        let text = "abc检索_x9f8e7d6c Нижняя αβγ 0123456789abcdef!!  tail".repeat(20);
        for budget in [MIN_QUERY_TOKENS, 64, 128, 512] {
            let packed = pack_chars(&text, budget);
            assert!(
                estimate_tokens(&packed) <= budget,
                "pack_chars overshot at budget {budget}: {} tokens for {:?}",
                estimate_tokens(&packed),
                packed
            );
        }
    }

    /// Why (#4972 review, finding 2): a user who appends an instruction after
    /// the notification block wrote the most intent-bearing text in the prompt.
    /// Keeping only `<summary>`/`<result>` discarded it, and `units_dropped`
    /// stayed 0 — content going missing without the accounting noticing, the
    /// defect class this module exists to close.
    /// What: an envelope with trailing user text; asserts the trailing text
    /// survives the strip.
    /// Test: itself.
    #[test]
    fn envelope_strip_keeps_text_after_the_envelope() {
        let raw = "<task-notification>\n\
                   <task-id>abc123</task-id>\n\
                   <result>the agent finished the sweep</result>\n\
                   </task-notification>\n\n\
                   now open the PR and set the milestone";
        let out = strip_notification_envelope(raw).expect("envelope must be recognised");
        assert!(
            out.contains("now open the PR and set the milestone"),
            "user text after the envelope must survive; got:\n{out}"
        );
        assert!(
            out.contains("finished the sweep"),
            "payload must survive too"
        );
        assert!(!out.contains("abc123"), "framing must still go");
    }

    /// Why (#4972 review, finding 4): an envelope whose `<summary>` and
    /// `<result>` are both absent or blank is pure framing. The branch is
    /// reachable and was untested, and the deliberate choice it encodes needs
    /// pinning: strip nothing and pass the prompt through, because the
    /// alternative — an empty query — returns zero drawers, and noisy recall
    /// beats none. The budget packer still bounds whatever goes out.
    /// What: an envelope with no payload; asserts `None` (pass-through) and that
    /// the shaped query is non-empty.
    /// Test: itself.
    #[test]
    fn envelope_with_no_payload_passes_through_whole() {
        let raw = "<task-notification>\n\
                   <task-id>abc123</task-id>\n\
                   <output-file>/tmp/x.output</output-file>\n\
                   <status>completed</status>\n\
                   <summary>   </summary>\n\
                   </task-notification>";
        assert_eq!(
            strip_notification_envelope(raw),
            None,
            "a payload-free envelope must pass through, not become an empty query"
        );
        let shaped = shape_recall_query(raw, QUERY_TOKEN_BUDGET);
        assert!(!shaped.text.is_empty(), "the query must not be empty");
        assert!(!shaped.shape.envelope_stripped, "nothing was stripped");
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

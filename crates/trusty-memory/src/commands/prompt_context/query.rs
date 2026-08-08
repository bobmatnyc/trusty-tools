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
//! `token_estimate_never_underestimates`, `cjk_is_one_token_per_char`,
//! `unbreakable_oversized_token_still_yields_a_query`,
//! `configured_query_budget_clamps_to_bounds`.
//!
//! The estimator errs high by construction on every input class it cannot model
//! exactly. That is the load-bearing property: an over-estimate trims a little
//! more than necessary and reports it, while an under-estimate hands the
//! embedder a query it cuts silently *and* leaves [`RecallQueryShape`] reporting
//! no loss — a monitor that says healthy while the loss happens (#4972 review).
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
/// Why: an under-estimate is the only failure that matters here — it hands the
/// embedder a query it then cuts silently, which is the defect this module
/// exists to close. Calibrated against the model's own `tokenizer.json`
/// vocabulary (a WordPiece greedy-longest-match reference implementation) over
/// randomly sampled real prompts, which are **Latin-script prose and code**:
///
/// | divisor | est/true p01 | p50 | underestimates |
/// |---|---|---|---|
/// | 3 | 1.00 | 1.38 | **0.0%** |
/// | 4 | 0.84 | 1.08 | 15.6% |
/// | 5 | 0.79 | 1.00 | 50.5% |
///
/// The guarantee this constant carries is narrow and worth stating exactly: on
/// runs of ASCII letters, `ceil(len / 3)` was at or above the true WordPiece
/// count for every prompt in that sample. It is **not** a guarantee about
/// arbitrary input — the sample contains no CJK, no Cyrillic, and no hex
/// digests, and a divisor tuned on English words underestimates all three by
/// 2–5×. [`piece_tokens`] therefore does not apply this divisor to them; see
/// [`NON_ASCII_TOKENS_PER_CHAR`] and the entropy branch there.
///
/// The repo's `inference::bedrock::cache::estimate_tokens` uses chars/4 for a
/// *different* question (is this prefix big enough to be worth caching), where
/// an over-estimate is the harmful direction; over this corpus chars/4
/// underestimates 94.5% of the time.
/// Test: `token_estimate_never_underestimates`.
const CHARS_PER_SUBTOKEN: usize = 3;

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
/// Test: `token_estimate_never_underestimates`.
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
/// module closes.
/// Test: `envelope_strip_recovers_the_payload`,
/// `envelope_strip_keeps_text_after_the_envelope`,
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
            kept.push(tail);
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
    // Not one whole line fits. Drop to words within the first line, and count
    // the lines we never even reached.
    let first = lines.first().copied().unwrap_or(text);
    let words: Vec<&str> = first.split_whitespace().collect();
    let (kept, dropped_words) = pack_units(&words, " ", budget_tokens);
    if !kept.trim().is_empty() {
        return (kept, dropped_words + lines.len().saturating_sub(1));
    }
    (pack_chars(first, budget_tokens), text.chars().count())
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
            (committed + run.cost() + 1, Run::default())
        } else if ch.is_alphanumeric() {
            (committed, run.extended(ch))
        } else {
            (
                committed + run.cost() + usize::from(!ch.is_whitespace()),
                Run::default(),
            )
        };
        if next_committed + next_run.cost() > budget_tokens {
            break;
        }
        committed = next_committed;
        run = next_run;
        kept_bytes = idx + ch.len_utf8();
    }
    text[..kept_bytes].to_string()
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
    /// This run's token cost — see [`piece_tokens`] for the table.
    fn cost(self) -> usize {
        if self.len == 0 {
            0
        } else if self.non_ascii {
            self.len * NON_ASCII_TOKENS_PER_CHAR
        } else if self.has_digit {
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
/// | ASCII letters only | `ceil(len / CHARS_PER_SUBTOKEN)` | the calibrated Latin-prose case |
/// | any other non-whitespace char | 1 token | punctuation, including `_`, which `BertPreTokenizer` splits on rather than treating as a word character |
/// | whitespace | 0 | |
///
/// Every branch errs high by construction. That costs window on scripts the
/// estimator cannot model precisely, and it is the correct direction: an
/// over-estimate trims a little more than necessary and says so, while an
/// under-estimate re-creates the silent cut (#4972 review, HIGH).
/// Test: `token_estimate_never_underestimates`, `cjk_is_one_token_per_char`.
fn piece_tokens(text: &str) -> usize {
    let mut total = 0usize;
    let mut run = Run::default();
    for ch in text.chars() {
        if is_cjk(ch) {
            total += run.cost() + 1;
            run = Run::default();
            continue;
        }
        if ch.is_alphanumeric() {
            run = run.extended(ch);
            continue;
        }
        total += run.cost() + usize::from(!ch.is_whitespace());
        run = Run::default();
    }
    total + run.cost()
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
        // A 12-char run costs ceil(12/3) = 4 sub-tokens, not 1.
        assert_eq!(piece_tokens("abcdefghijkl"), 4);
        // Punctuation is its own token; whitespace is free.
        assert_eq!(piece_tokens("ab, cd"), 1 + 1 + 1);
        // `_` splits the run and costs 1 — it is punctuation to the tokenizer,
        // so `a_b` is three tokens, not one two-char run.
        assert_eq!(piece_tokens("a_b"), 1 + 1 + 1);
        // Additive across a whitespace join — the property `pack_units` relies on.
        let units = ["alpha beta", "gamma-delta", "epsilon"];
        let joined = units.join(" ");
        assert_eq!(
            piece_tokens(&joined),
            units.iter().map(|u| piece_tokens(u)).sum::<usize>()
        );
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
    /// What: the safe direction, pinned. Each case carries the true WordPiece
    /// count measured against the model's own `tokenizer.json` vocabulary
    /// (30,522 entries, `handle_chinese_chars: true`, NFD + lowercase
    /// normalization) with a greedy-longest-match reference implementation.
    /// The estimate must be greater than or equal to every one of them. The
    /// figures are upper-bounded rather than exact on purpose — this asserts a
    /// direction, not a calibration.
    /// Test: itself.
    #[test]
    fn token_estimate_never_underestimates() {
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
                695,
            ),
            (
                "jwt",
                "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0\
                 .dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U"
                    .to_string(),
                94,
            ),
            (
                "snake_case identifiers",
                "filter_drawers_by_relevance_floor_configured ".repeat(40),
                762,
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
        assert_eq!(piece_tokens(han), 7, "each Han codepoint is its own token");
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
    /// Test: itself.
    #[test]
    fn unbreakable_oversized_token_still_yields_a_query() {
        let blob = "a".repeat(6_000);
        let shaped = shape_recall_query(&blob, QUERY_TOKEN_BUDGET);
        assert!(
            !shaped.text.is_empty(),
            "an unbreakable oversized token must not produce an empty query — \
             that returns zero drawers, worse than the truncation it replaced"
        );
        assert!(shaped.text.len() < blob.len(), "it must still be reduced");
        assert!(blob.starts_with(&shaped.text), "the query must be a prefix");
        assert!(
            shaped.shape.sent_tokens <= QUERY_TOKEN_BUDGET,
            "got {} tokens",
            shaped.shape.sent_tokens
        );
        assert!(
            shaped.shape.reshaped() && shaped.shape.units_dropped > 0,
            "the reduction must be reported; got {:?}",
            shaped.shape
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

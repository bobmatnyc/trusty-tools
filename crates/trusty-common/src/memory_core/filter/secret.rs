//! Secret / credential detection for the `memory_core::filter` gate.
//!
//! Why: extracted from `filter.rs` (issue #6199) so the gate module stays under
//! the 500-SLOC production cap. This is the credential-detection domain —
//! `find_secret_token` and the `looks_like_secret` predicate tree it drives —
//! that accreted across issues #1481, #2442, #2800, #4312, #4739, #4898, #4977,
//! #5043 and #5513.
//! What: `check_secret` (the entry point `super::FilterConfig::apply` calls),
//! `find_secret_token`, and the structural-token / charset predicates that
//! separate real credentials from git SHAs, paths, URLs, and symbol paths.
//! Test: exercised through the `filter_tests.rs` suite wired into `super`.

use super::{FilterReject, is_git_sha_like};

/// Scan `content` for the first token that looks like a genuine high-entropy
/// secret (API key, access token, long base64/JWT-ish blob), explicitly
/// allowlisting git-SHA-shaped hex tokens.
///
/// Why (issue #1481): credentials must never be stored, but git SHAs (the most
/// common "high-entropy-looking" token in engineering prose) must be. A pure
/// detector keyed only on entropy/length would block both; this function adds
/// the SHA allowlist and keys "secret" on the character-class mix that real
/// credentials exhibit (mixed upper+lower+digit, or known credential prefixes,
/// or symbol-bearing base64) which a SHA never does.
///
/// Why the backtick is a delimiter (issue #4312, the FOURTH recurrence of one
/// false-positive shape after #1667, #2800, #4216): splitting on whitespace
/// alone made two adjacent Markdown inline-code spans joined by a bare `/` —
/// `` `foo`/`bar` `` — arrive as ONE token. This function trims punctuation
/// only from the OUTER boundary of each token, so the interior backticks
/// survived, every `/`-segment failed `is_word_segment`, and the token fell to
/// the base64 branch of [`looks_like_secret`] and was flagged. Splitting on the
/// backtick removes that whole class, and strictly tightens detection for a
/// credential written flush against a backtick, which previously polluted the
/// token and defeated [`is_plausible_credential_charset`].
///
/// Bound on the safety of that split, stated precisely because a reader will
/// rely on it: a backtick occurs in no *machine-generated* credential — not
/// base64, base64url, hex or base32, and not in any provider key format
/// (`sk-`, `ghp_`, `AKIA`, `xoxb-`, JWT). Verified exhaustively in
/// `real_secrets_still_blocked_after_4312_backtick_split`. It is NOT true of a
/// *user-chosen* password inside a connection-string URL, where the charset is
/// unbounded and a literal backtick is legal: `mongodb://user:pa` + backtick +
/// `ss@host` splits into two fragments and is missed. The adversarial delta is
/// nil — the detector has always split on whitespace, so deliberate evasion
/// costs a space either way — but this is an accidental-storage hygiene gate,
/// not an adversarial control, and the limitation is real.
///
/// What: returns `Some(<redacted preview>)` for the first secret-looking
/// token, else `None`. Tokens are delimited by whitespace or a backtick, then
/// stripped of surrounding punctuation before classification. The preview
/// shows the leading characters and masks the tail so the secret itself is not
/// echoed back verbatim.
/// Test: `secret_token_is_blocked`, `git_sha_prose_is_accepted`,
/// `base64_blob_is_blocked`, `known_key_prefixes_are_blocked`,
/// `backtick_joined_spans_are_not_flagged`,
/// `real_secrets_still_blocked_after_4312_backtick_split`.
pub fn find_secret_token(content: &str) -> Option<String> {
    // #4312: backticks delimit Markdown inline-code spans and occur in no
    // machine-generated credential, so split on them alongside whitespace.
    for raw in content.split(|c: char| c.is_whitespace() || c == '`') {
        let tok =
            raw.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_')));
        if looks_like_secret(tok) {
            return Some(redact_token(tok));
        }
    }
    None
}

/// Standalone secret gate: `Ok(())` when `content` carries no
/// [`find_secret_token`] hit, `Err(FilterReject::PotentialSecret)` otherwise.
///
/// Why (issue #2520, two-tier `force` design): `RememberOptions::force`
/// bypasses the QUALITY gates (noise patterns, short-content, non-alphabetic
/// ratio) but must never bypass secret detection — otherwise an automated
/// writer that always passes `force: true` (e.g. trusty-code's per-turn
/// recorder) would persist raw credentials with zero screening. Extracting
/// this single-purpose check out of
/// [`crate::memory_core::filter::FilterConfig::apply`] lets
/// `PalaceHandle::remember_with_options` call it on its own when `force` is
/// set but the caller has NOT also set the explicit `allow_secret_like`
/// opt-in.
/// What: thin wrapper around [`find_secret_token`] that packages a hit into
/// the same [`FilterReject::PotentialSecret`] variant `apply` returns, so
/// both call sites produce an identical, actionable error message.
/// Test: exercised via `FilterConfig::apply`'s existing secret-detection
/// tests (this function is `apply`'s sole secret-check code path) and
/// directly by the two-tier `force` tests in
/// `trusty-common::memory_core::retrieval::handle`.
pub fn check_secret(content: &str) -> Result<(), FilterReject> {
    if let Some(token) = find_secret_token(content) {
        return Err(FilterReject::PotentialSecret { token });
    }
    Ok(())
}

/// Known credential prefixes that should always be treated as secrets.
///
/// Why (issue #1481): provider-issued keys (OpenAI `sk-`, GitHub `ghp_`/`gho_`,
/// Slack `xoxb-`) have distinctive prefixes that make them unambiguously secret
/// regardless of their entropy profile. Matching the prefix is cheaper and more
/// precise than entropy alone.
///
/// Every entry here carries punctuation an English word never does, which is why
/// [`SECRET_MIN_LEN`] is sufficient protection for this list. AWS `AKIA`/`ASIA`
/// used to live here and is not (issue #4898) — it is four bare letters, so it
/// needs a shape check; see [`AWS_KEY_ID_PREFIXES`].
/// What: lowercased prefix list checked case-insensitively in
/// [`looks_like_secret`] (which lowercases the token before comparing), after
/// the [`SECRET_MIN_LEN`] floor.
/// Test: `known_key_prefixes_are_blocked`, `aws_access_key_ids_are_blocked`.
pub(crate) const SECRET_PREFIXES: &[&str] = &[
    "sk-",
    "ghp_",
    "gho_",
    "ghs_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
];

/// AWS access key ID prefixes — long-term (`AKIA…`) and STS temporary
/// credentials (`ASIA…`).
///
/// Why these are separated from [`SECRET_PREFIXES`] (issue #4898): every other
/// entry there carries punctuation (`-`, `_`) that no English word contains, so
/// a bare length floor is enough to keep prose out. `akia`/`asia` do not — they
/// were compared case-insensitively against a lowercased token, so `Asia` (the
/// continent, 4 characters) matched, and `Asia-Pacific` and
/// `asia-pacific-rollout-notes` matched at any length. Matching the AWS *shape*
/// instead of a lowercased prefix removes that whole class rather than pushing
/// it past a length threshold.
/// What: uppercase prefixes, compared against the token verbatim by
/// [`is_aws_access_key_id`].
/// Test: `aws_access_key_ids_are_blocked`,
/// `three_4898_reproductions_are_not_flagged`.
pub(crate) const AWS_KEY_ID_PREFIXES: &[&str] = &["AKIA", "ASIA"];

/// True when `token` OPENS with an AWS access key ID: an
/// [`AWS_KEY_ID_PREFIXES`] prefix followed by uppercase-alphanumeric characters
/// for the full [`SECRET_MIN_LEN`] of a key id. Whatever follows those 20
/// characters is not examined.
///
/// Why (issue #4898): AWS key IDs are all-uppercase base32 (`[A-Z2-7]`), exactly
/// 20 characters for `AKIA…`, so the mixed-case heuristic can never flag them
/// and this layer is their only detector (FN-1, issue #1481).
///
/// Why it is a PREFIX predicate and not a whole-token one (#4898 review round
/// 1): the first cut of this function required the ENTIRE token to be
/// uppercase-alphanumeric, which lost AWS detection completely for any key with
/// an adjacent character. `AKIAIOSFODNN7EXAMPLE-old` fell through to
/// [`is_structural_token`], whose segmented-identifier branch rescued it
/// (`AKIAIOSFODNN7EXAMPLE` and `old` are both case-uniform words), and the
/// mixed-case branch then could not fire because `has_lower` is false. Measured
/// FLAG -> MISS, including on AWS's own canonical key-id/secret-key pair
/// `AKIAIOSFODNN7EXAMPLE/wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY`. The
/// predicate this replaced (`lower.starts_with("akia")`) was a prefix test, and
/// converting it to a whole-token shape test is what the structural bypass
/// defeated. Looking at the first 20 bytes only restores prefix semantics while
/// keeping the shape check that excludes `Asia`, `Asia-Pacific` and
/// `ASIA-PACIFIC-ROLLOUT-NOTES` (whose fifth byte is `-`).
/// What: length floor, case-sensitive prefix match, then a charset check over
/// the first [`SECRET_MIN_LEN`] bytes only.
/// Test: `aws_access_key_ids_are_blocked`,
/// `aws_key_ids_with_adjacent_text_are_blocked`,
/// `real_secrets_still_blocked_after_4898_narrowing`,
/// `known_accepted_bounds_after_4898`.
pub(crate) fn is_aws_access_key_id(token: &str) -> bool {
    token.len() >= SECRET_MIN_LEN
        && AWS_KEY_ID_PREFIXES.iter().any(|p| token.starts_with(p))
        // #4898 review: `.take(SECRET_MIN_LEN)`, not `.all()` over the token —
        // one neighbouring character must not disable AWS detection.
        && token
            .bytes()
            .take(SECRET_MIN_LEN)
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// Minimum token length before [`looks_like_secret`] will call anything a
/// credential.
///
/// Why (issue #4898): this floor existed as a bare `20` literal placed AFTER the
/// prefix test, so it governed the entropy branches only. A four-character token
/// equal to a prefix entry was flagged regardless of length — four characters
/// cannot be a credential. Naming the constant and moving the check above the
/// prefix test makes the floor apply to every detection path.
///
/// Why 20 does not cost detection: no issuer behind a [`SECRET_PREFIXES`] entry
/// mints a token under 20 characters — `ghp_` is 40, `github_pat_` 93, `xoxb-`
/// 50+, `sk-` issuers 35+ — and AWS key ids are exactly 20. The one shape the
/// floor does drop is a self-hosted proxy key that copies a provider prefix
/// without its length, such as LiteLLM's documented `sk-1234` master key
/// (verified in review round 1).
/// What: inclusive minimum token length.
/// Test: `three_4898_reproductions_are_not_flagged`,
/// `real_secrets_still_blocked_after_4898_narrowing`.
pub(crate) const SECRET_MIN_LEN: usize = 20;

/// Characters that separate the words of a human-readable compound identifier.
///
/// Why (issue #4739): [`is_segmented_identifier`] split on `-` alone, so an
/// ordinary dotted filename (`Agents.app.bak-20260729-000028`) was segmented
/// into `["Agents.app.bak", "20260729", "000028"]` — and the first segment,
/// carrying two case runs across its dots, could not be a word. `.` and `_`
/// delimit identifier words exactly as `-` does; treating them as delimiters is
/// what lets the per-segment predicate see words instead of a blob.
///
/// Why this does not widen the base64 branch: [`is_structural_token`] returns
/// before reaching its segmented-identifier branch whenever the token contains
/// `=` or `/`, and a `+`-bearing token is diverted to
/// [`is_plus_joined_word_phrase`] — which applies this same delimiter set plus a
/// word-length floor. The segmented branch proper is therefore reachable only
/// for tokens the base64 branch can never fire on. (Before #4898 the `+` case
/// was a flat rejection; the divert is what closed the `PM+instructions+
/// subagents` false positive without loosening `=` or `/`.)
/// What: the delimiter set `-`, `_`, `.` used by both `contains` and `split`.
/// Test: `dotted_capitalised_filenames_are_not_flagged`.
pub(crate) const IDENTIFIER_DELIMITERS: [char; 3] = ['-', '_', '.'];

/// True when `seg` reads as a single human-readable word: all-lowercase,
/// ALL-UPPERCASE, or Capitalized. Digits are case-neutral, so a digit-only
/// segment (`20260729`) qualifies.
///
/// Why: this is the per-segment predicate used by [`is_segmented_identifier`]
/// to tell a compound human-readable identifier (`2-medium->REQUEST_CHANGES`,
/// `Agents.app.bak-20260729-000028`) from a random mixed-case credential blob
/// (`AbCd1234-EfGh5678`).
///
/// Why Capitalized was added (issue #4739, the FIFTH recurrence in this
/// detector after #1667, #2800, #4216 and #4312): admitting only all-lower and
/// all-upper meant a leading capital — the single most ordinary thing about an
/// English word or a macOS app bundle name — read as internal mixed case, so
/// `Agents.app.bak-20260729-000028` fell through to the credential heuristic.
///
/// The bound this predicate actually holds, stated exactly because a reader
/// will rely on it and because #4723 shipped an absolute here that a
/// measurement disproved: a segment that is not uniformly uppercase admits **at
/// most one uppercase letter, in any position**. That is what keeps the
/// run-of-two case alternation of an encoded blob (`AbCd`, `EfGh`) out — two
/// uppercase letters in a non-uppercase segment is disqualifying.
///
/// Why the position constraint was dropped (issue #4898, the SIXTH recurrence):
/// #4739 admitted a capital only in FIRST position, so the git branch name
/// `fix-3696-slice1-gapA-emit` failed on the segment `gapA` — a lowercase word
/// with one trailing capital, the ordinary way engineers label a variant
/// (`gapA`, `partB`, `phase2B`). The count is what does the discriminating work,
/// not the position: `AbCd` and `xYzAbc` carry two capitals either way and stay
/// out.
///
/// KNOWN RESIDUAL MISS, measured, carried deliberately (#4898 review round 1):
/// the earlier revision of this doc claimed the relaxation admits a shape "no
/// encoded alphabet produces at segment scale". That is false for base64url,
/// which encodes with `-` and `_` — so its tokens arrive already split into 4–8
/// character segments, exactly the scale at which "at most one capital" is cheap
/// to satisfy by chance. `9h6Nn6_ivJd6vmb-xEk4` is real
/// `base64url(urandom(15))` output and flipped FLAG -> MISS on this change. Cost
/// against `origin/main` over a fixed 300k-sample corpus: 453 additional misses
/// at 15 input bytes, 210 at 16, 48 at 20, 6 at 24, 0 at 32 — it decays to
/// nothing as tokens lengthen, because more segments means more chances for one
/// to carry two capitals.
///
/// Why it is not closed here: the narrowing that would close it — refusing a
/// non-first capital in any segment that also carries a digit — takes `phase2B`
/// with it, and `slice1`/`gapA` sit either side of that line in the very branch
/// name this issue is about. Closing it properly is the structural rewrite this
/// PR defers (see the PR body); a carve-out would be the seventh round of the
/// same mistake. Pinned as a ratchet in
/// `generated_encoder_corpus_stays_flagged` so the number cannot drift
/// unnoticed.
///
/// The ACCEPTED side of that bound, stated because the rejected side alone
/// would read as a stronger guarantee than this predicate gives: what
/// [`is_segmented_identifier`] admits is a token whose **every segment is
/// independently case-uniform**. At short segment lengths that includes shapes
/// which are not human words at all —
/// `Xy7-Kp2-Qm9-Rt4-Vw8-Nz3-Bc6-Fj0` and `A1b2-C3d4-E5f6-G7h8-I9j0-K1l2` are
/// both admitted, and neither is a filename. They are a real, measured miss,
/// not a hypothetical.
///
/// Why that is an acceptable price rather than a hole: a *generated* credential
/// essentially never satisfies per-segment case uniformity, because it is not
/// segmented into short groups in the first place, and if it is, every group
/// must independently land in one of three case shapes. Treating a short
/// alphanumeric segment as case-uniform with probability roughly ¼ (an
/// estimate, assuming uniform draws from a mixed alphabet — not a measured
/// constant), an eight-segment token clears the bar about `(1/4)^8` of the
/// time, order 1 in 10^4–10^5. Everything **generated** rather than
/// **composed** — bare blobs, base64, base64url, JWTs, provider-prefixed keys —
/// carries no such segmentation and still flags. The exposure is to a
/// credential a human deliberately composed to look like an identifier, which
/// is the accidental-storage hygiene threat model this module has always had,
/// not an adversarial one.
///
/// The earlier revision of this doc illustrated the accepted side with
/// `Abcd-1234-Efgh-5678`. That example was wrong: at 19 characters it is below
/// `looks_like_secret`'s own 20-char floor, so it never reaches this branch and
/// could not demonstrate anything about it. `Abcd-1234-Efgh-5678-Ijkl` (24) is
/// the corrected form and is genuinely admitted here.
///
/// Known bound, deliberately not fixed: CamelCase (`TrustyMemory`,
/// `MyDocument`) has two capitalized runs and is still not a word here, so
/// `TrustyMemory.app.bak-20260801-120000` remains a false positive. Admitting
/// it would require accepting multi-uppercase segments, which is the exact
/// signature of `AbCdEfGhIjKlMnOpQrSt-1234` — a credential miss. Measured, not
/// assumed; see the residue assertion in the test below.
/// What: strips leading/trailing non-alnum chars (arrow punctuation like `>`,
/// `<`), then accepts when the alphabetic characters are all-uppercase, or carry
/// at most one uppercase letter anywhere. Interior non-alphanumeric characters
/// are ignored rather than rejected — this predicate constrains case, not
/// charset.
/// Test: `structural_tokens_are_not_flagged`,
/// `dotted_capitalised_filenames_are_not_flagged`,
/// `real_secrets_still_blocked_after_4739_capitalised_segments`,
/// `per_segment_case_uniformity_is_a_known_accepted_bound`,
/// `three_4898_reproductions_are_not_flagged`,
/// `real_secrets_still_blocked_after_4898_narrowing`.
pub(crate) fn is_human_word_segment(seg: &str) -> bool {
    let s = seg.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    if s.is_empty() {
        return false;
    }
    let uppers = s.chars().filter(|c| c.is_ascii_uppercase()).count();
    let lowers = s.chars().filter(|c| c.is_ascii_lowercase()).count();
    // Digit-only segments are case-neutral (`20260729`, `000028`).
    if uppers == 0 {
        return true;
    }
    // #4739: ALL-UPPERCASE (`REQUEST_CHANGES`) is a word.
    if lowers == 0 {
        return true;
    }
    // #4898: one capital anywhere — `Capitalized`, `gapA`, `phase2B`. Two or
    // more in a mixed-case segment is run-of-two alternation (`AbCd`), the
    // signature of an encoded blob.
    uppers == 1
}

/// True when `token` looks like a compound identifier whose delimiter-separated
/// segments are each human-readable words (e.g. `2-medium->REQUEST_CHANGES`,
/// `trusty-review-v0.6.0`, `Agents.app.bak-20260729-000028`).
///
/// Why (issue #1667): the mixed-case-plus-digit heuristic in
/// [`looks_like_secret`] fires on `2-medium->REQUEST_CHANGES` because the
/// compound token contains lower (`medium`), upper (`REQUEST_CHANGES`), and
/// digit (`2`) when considered as a whole. But each segment is internally a
/// single word, which is the hallmark of a human-readable compound identifier,
/// not a random credential blob.
///
/// Why the delimiter set widened (issue #4739): splitting on `-` alone left
/// every dot- and underscore-joined filename as one unsplittable segment. See
/// [`IDENTIFIER_DELIMITERS`] for why that widening cannot reach the base64
/// branch.
/// What: requires at least two segments after splitting on
/// [`IDENTIFIER_DELIMITERS`], each passing [`is_human_word_segment`] (which
/// rejects the empty segment a trailing delimiter produces).
/// Test: `structural_tokens_are_not_flagged`,
/// `dotted_capitalised_filenames_are_not_flagged`.
/// Minimum alphabetic length one segment of a `+`-joined token must reach
/// before [`is_plus_joined_word_phrase`] will call the token prose.
///
/// Why (issue #4898): `+` is base64's own symbol, so per-segment case uniformity
/// alone is too weak a signal there — `A+DIA+DIA+DIA+…` is case-uniform in every
/// segment and is a base64 blob. Requiring one segment to reach word length is
/// what separates it from `PM+instructions+subagents` (longest segment 12).
/// Five is the shortest length at which an English word carries enough letters
/// to be one; the repro's shortest content word (`agents`, `skills`) is six.
/// What: inclusive minimum count of ASCII alphabetic characters in the longest
/// segment.
/// Test: `real_secrets_still_blocked_after_4898_narrowing` (the `base64 with +`
/// entry is what this constant keeps flagged).
pub(crate) const MIN_PHRASE_WORD_LEN: usize = 5;

/// True when `token` is a `+`-joined phrase of human-readable words, e.g.
/// `PM+instructions+subagents`.
///
/// Why (issue #4898): [`is_structural_token`] rejected every `+`-bearing token
/// outright on the reasoning that "`+` is unambiguously base64". It is not — `+`
/// is also how prose joins terms into a compound label, and the resulting token
/// went straight to the base64 branch of [`looks_like_secret`], where the
/// `has_upper || has_digit` entropy floor was satisfied by any capitalised word
/// in the phrase. The doc on [`is_plausible_b64_charset`] already named this as
/// an accepted residual gap; this predicate closes it.
///
/// Why every segment must be purely alphanumeric: [`is_human_word_segment`]
/// constrains case, not charset — it trims the ends of a segment and ignores
/// interior punctuation. Without a charset requirement here, `token=A+DIA+DIA+…`
/// yields the segment `token=A` (one capital, six letters) and
/// `mongodb+srv://svcuser:hunterhunter@cluster.mongodb.net` yields
/// `srv://svcuser:hunterhunter@cluster` (all lowercase), so a `key=`-prefixed
/// base64 blob and a mongo connection string both passed.
///
/// Why every segment must additionally be pure-alphabetic OR pure-digit (#4898
/// review round 1): the first cut argued that base64's `+` density of ~1.6%
/// makes its `+`-separated runs "average tens of characters and cannot be
/// case-uniform". That reasoning fails at the 20-char floor, where a token is
/// short enough for every run to be short. `base64::encode(urandom(15))` yields
/// shapes like `j1u7nJd+tvZers+wdZyr` whose segments each carry exactly one
/// capital, and `j1u7nJd` clears [`MIN_PHRASE_WORD_LEN`] on its own. Measured on
/// a generated corpus, that cost 152 misses per 300k at 15 input bytes, 72 at
/// 16, 11 at 20. Requiring each segment to be uniformly alphabetic or uniformly
/// numeric takes it to 1–2 per 300k, because encoder output interleaves letters
/// and digits inside a run while a `+`-joined phrase never does — `PM`,
/// `instructions`, `milestone`, `1`, `3`, `5` are each uniform.
///
/// Known accepted bound: a `+`-joined token whose segments are case-uniform,
/// character-class-uniform AND word-length is admitted without any dictionary
/// check — `Abcdefgh+Ijklmnop+Qrstuvwx` passes. This is the same class of bound
/// [`is_human_word_segment`] already documents.
/// What: splits on [`IDENTIFIER_DELIMITERS`] plus `+`, requires at least two
/// segments, every segment non-empty, ASCII-alphanumeric, uniformly alphabetic
/// or uniformly numeric, and a [`is_human_word_segment`]; plus one segment
/// holding at least [`MIN_PHRASE_WORD_LEN`] alphabetic characters.
/// Test: `three_4898_reproductions_are_not_flagged`,
/// `real_secrets_still_blocked_after_4898_narrowing`,
/// `generated_encoder_corpus_stays_flagged`,
/// `known_accepted_bounds_after_4898`.
pub(crate) fn is_plus_joined_word_phrase(token: &str) -> bool {
    let segments: Vec<&str> = token
        .split(|c: char| c == '+' || IDENTIFIER_DELIMITERS.contains(&c))
        .collect();
    if segments.len() < 2 {
        return false;
    }
    let segment_is_wordlike = |s: &&str| {
        // #4898: charset first — a segment carrying `=`, `:`, `/` or `@` is not
        // a word, and `is_human_word_segment` would not notice.
        !s.is_empty()
            && s.chars().all(|c| c.is_ascii_alphanumeric())
            // #4898 review: uniformly alphabetic or uniformly numeric. A run
            // that interleaves letters and digits is encoder output, not a word.
            && (s.chars().all(|c| c.is_ascii_alphabetic())
                || s.chars().all(|c| c.is_ascii_digit()))
            && is_human_word_segment(s)
    };
    if !segments.iter().all(segment_is_wordlike) {
        return false;
    }
    segments
        .iter()
        .any(|s| s.chars().filter(|c| c.is_ascii_alphabetic()).count() >= MIN_PHRASE_WORD_LEN)
}

/// Separator joining the segments of a Rust/C++ symbol path (`Type::method`).
///
/// Why (issue #5043) this is the one delimiter the CamelCase relaxation below can
/// safely key on: `::` appears in NO encoder alphabet. base64 is `A–Za–z0–9+/=`,
/// base64url is `A–Za–z0–9-_=`, a JWT is base64url plus `.`, hex and base32 are
/// subsets of alphanumerics. Every OTHER identifier delimiter is shared with an
/// encoder — `-` and `_` are base64url's own two symbols, `.` is the JWT
/// separator, `+`/`/`/`=` are base64 — which is why six previous rounds could
/// only widen those under a strict per-segment case rule. Decomposing on `::`
/// cannot loosen any encoded-blob branch, because no encoded blob can contain it.
/// What: the literal `"::"`, used by [`is_symbol_path`].
/// Test: `rust_symbol_paths_are_not_flagged`,
/// `symbol_path_keyhole_does_not_shelter_credentials`.
pub(crate) const SYMBOL_PATH_SEPARATOR: &str = "::";

/// Colon-segment length at or below which a segment is exempt from carrying a
/// [`MIN_PHRASE_WORD_LEN`] word.
///
/// Why (issue #5043): ordinary Rust path segments are often short function or
/// module names with no five-letter word in them — `to_str`, `as_ref`, `std`,
/// `iter`. Requiring a five-letter word in EVERY segment rejects those.
/// Requiring one in only SOME segment is the hole: `secretKey::<blob>` would
/// ride in on `secretKey`. A short segment therefore keeps a word requirement,
/// just a lower one ([`SYMBOL_SEGMENT_SHORT_WORD_LEN`]).
/// What: inclusive maximum byte length at which the lower word floor applies.
/// Test: `symbol_path_keyhole_does_not_shelter_credentials`.
pub(crate) const SYMBOL_SEGMENT_SHORT_LEN: usize = 8;

/// Word floor for a segment of at most [`SYMBOL_SEGMENT_SHORT_LEN`] bytes.
///
/// Why (issue #5043, review round 1, HIGH): the first cut of this PR EXEMPTED a
/// short segment from the word requirement entirely, leaving only the digit-run
/// and stray-letter shape checks. That let a blob chunked into short
/// `::`-joined groups take the exemption whole — `Ab12cdEf::Gh34ijKl::Mn56opQr`
/// went FLAG -> MISS, and a 24-character base64url blob so chunked missed
/// 4629/3282/2521 per 20k at chunk widths 4/6/8 against a baseline of 355. The
/// doc above already named the risk ("requiring one in only SOME segment is the
/// hole") and the length exemption then opened it a different way. Three is what
/// `to_str`, `as_ref`, `std` and `Sha1Hash` reach (their longest CamelCase word
/// is 3–4) while a chunked encoder group does not: same shapes rescued,
/// chunked misses back to 366/732/1273.
///
/// Note this is a floor, not an exemption — every segment answers the same
/// question, at a length-dependent threshold. Nothing waives the word check.
///
/// It doubles as the boundary below which a segment is too short to hold a word
/// at all (review round 2): `io`, `rc`, `rt` and `os` are ordinary Rust module
/// names whose longest word is 2, so the floor flagged every symbol path through
/// one. Those are decided on case uniformity instead — see
/// [`is_symbol_path_segment`]. Measured cost of that arm: 37 additional misses
/// per 20k at chunk width 11, the only width whose 24-character chunking leaves a
/// 2-character remainder, and zero at every other width from 2 to 12.
/// What: inclusive minimum longest-CamelCase-word length for a short segment,
/// and the exclusive length below which case uniformity decides instead.
/// Test: `symbol_path_keyhole_does_not_shelter_credentials`,
/// `rust_symbol_paths_are_not_flagged`.
pub(crate) const SYMBOL_SEGMENT_SHORT_WORD_LEN: usize = 3;

/// Longest alphabetic CamelCase word in `seg`, and how many of its words are a
/// single letter.
///
/// Why (issue #5043): [`is_human_word_segment`] asks whether a segment is
/// case-UNIFORM, which a CamelCase identifier never is — `Bm25Index` and
/// `queryTopK` each carry two capitals, so it declines them, and that (not the
/// delimiter set) is what flagged the four reproductions. Deciding a CamelCase
/// segment needs the word structure INSIDE it, which means splitting at case and
/// alpha/digit boundaries and looking at the resulting run lengths.
///
/// The two statistics are what separate a composed identifier from encoder
/// output at the same length. `Bm25Index` splits to `Bm`/`25`/`Index` and
/// `Utf8Error` to `Utf`/`8`/`Error` — a five-letter word and no stray letters. A
/// base64url run alternates case every one or two characters, so its longest word
/// is 2–3 and single letters are everywhere.
/// What: one pass, no allocation. Boundaries are lower-or-digit -> Upper
/// (`fooBar`), Upper -> Upper-followed-by-lower (`HTTPServer` -> `HTTP`/`Server`),
/// and any alphabetic <-> digit transition. Returns
/// `(longest_alphabetic_word, single_letter_word_count)`.
/// Test: `rust_symbol_paths_are_not_flagged`,
/// `symbol_path_keyhole_does_not_shelter_credentials`.
pub(crate) fn camel_word_stats(seg: &str) -> (usize, usize) {
    fn tally(word: &[u8], longest: &mut usize, strays: &mut usize) {
        let letters = word.iter().filter(|b| b.is_ascii_alphabetic()).count();
        *longest = (*longest).max(letters);
        if letters == 1 {
            *strays += 1;
        }
    }
    let b = seg.as_bytes();
    let (mut longest, mut strays, mut start) = (0usize, 0usize, 0usize);
    for i in 1..b.len() {
        let (prev, cur) = (b[i - 1], b[i]);
        let boundary = (cur.is_ascii_uppercase() && !prev.is_ascii_uppercase())
            || (cur.is_ascii_uppercase()
                && prev.is_ascii_uppercase()
                && b.get(i + 1).is_some_and(|n| n.is_ascii_lowercase()))
            || (cur.is_ascii_digit() != prev.is_ascii_digit());
        if boundary {
            tally(&b[start..i], &mut longest, &mut strays);
            start = i;
        }
    }
    if start < b.len() {
        tally(&b[start..], &mut longest, &mut strays);
    }
    (longest, strays)
}

/// Number of maximal ASCII-digit runs in `s`.
///
/// Why (issue #5043): a composed identifier carries its digits in ONE group —
/// `Bm25Index`, `Sha256Hasher`, `Utf8Error`, `OAuth2Client`, `Base64Decoder` all
/// have exactly one. Encoder output draws digits uniformly, so a 20-character
/// base64url run averages three digits scattered across it and almost always
/// shows two or more runs. This is the single cheapest discriminator measured:
/// adding it alone cut the colon-wrapped credential miss rate from 8552 to 3319
/// per 30k.
/// What: counts transitions into a digit run.
/// Test: `symbol_path_keyhole_does_not_shelter_credentials`.
pub(crate) fn digit_run_count(s: &str) -> usize {
    let mut runs = 0usize;
    let mut in_run = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            runs += usize::from(!in_run);
            in_run = true;
        } else {
            in_run = false;
        }
    }
    runs
}

/// True when `seg` reads as one segment of a symbol path.
///
/// Why (issue #5043) each clause exists, measured on 30k generated base64url
/// tokens wrapped as `secretKey::<blob>` at the seed
/// `symbol_path_keyhole_does_not_shelter_credentials` uses (baseline miss rate
/// 1017/30k): word floor alone 8298, plus the digit-run cap 3331, plus the
/// stray-letter cap 2022. All three are load-bearing; dropping any one widens the
/// exemption measurably.
/// What: charset is alphanumerics plus [`IDENTIFIER_DELIMITERS`]; every
/// delimiter-separated word must hold at most one digit run and at most one
/// single-letter CamelCase word; and the segment's longest CamelCase word must
/// reach [`MIN_PHRASE_WORD_LEN`], or [`SYMBOL_SEGMENT_SHORT_WORD_LEN`] when the
/// segment is at most [`SYMBOL_SEGMENT_SHORT_LEN`] bytes.
/// Test: `rust_symbol_paths_are_not_flagged`,
/// `symbol_path_keyhole_does_not_shelter_credentials`,
/// `recurrence_corpus_true_positives_stay_flagged`.
pub(crate) fn is_symbol_path_segment(seg: &str) -> bool {
    if seg.is_empty()
        || !seg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || IDENTIFIER_DELIMITERS.contains(&c))
    {
        return false;
    }
    // #5043 review round 2: a segment too short to hold a word is decided on
    // case uniformity instead. Two-letter module names — `io`, `rc`, `rt`, `os`
    // — cannot reach the word floor, and the round-1 fix flagged every path
    // through one. Requiring single-case ALPHABETIC is what keeps a two-character
    // encoder group out: `aB` mixes case, `9x` is not all alphabetic.
    if seg.len() < SYMBOL_SEGMENT_SHORT_WORD_LEN {
        return seg.chars().all(|c| c.is_ascii_lowercase())
            || seg.chars().all(|c| c.is_ascii_uppercase());
    }
    let mut saw_word = false;
    let mut longest_overall = 0usize;
    for w in seg.split(IDENTIFIER_DELIMITERS).filter(|w| !w.is_empty()) {
        saw_word = true;
        let (longest, strays) = camel_word_stats(w);
        if digit_run_count(w) > 1 || strays > 1 {
            return false;
        }
        longest_overall = longest_overall.max(longest);
    }
    // #5043 review round 1: a graduated floor, never an exemption. A short
    // segment answers the same question at a lower threshold.
    let word_floor = if seg.len() <= SYMBOL_SEGMENT_SHORT_LEN {
        SYMBOL_SEGMENT_SHORT_WORD_LEN
    } else {
        MIN_PHRASE_WORD_LEN
    };
    saw_word && longest_overall >= word_floor
}

/// True when `token` is a `::`-joined symbol path — `Bm25Index::queryTopK`,
/// `std::str::Utf8Error::to_str`, `PalaceHandle::remember_with_options`.
///
/// Why (issue #5043, the SEVENTH recurrence after #1667, #1676, #2800/#4216,
/// #4312, #4739 and #4898): a CamelCase segment carries two or more capitals, so
/// [`is_human_word_segment`] declines it and the token satisfies the mixed-case
/// branch of [`looks_like_secret`]. Four reproductions were confirmed by
/// execution. The issue proposed adding `:` to [`IDENTIFIER_DELIMITERS`]; that
/// alone fixes nothing, because the case rule rejects `Bm25Index` however the
/// token is split.
///
/// Why the relaxation is keyed on `::` and not applied generally — the finding
/// that decided this design: admitting CamelCase inside [`is_human_word_segment`]
/// for every delimiter roughly doubles base64url misses per 30k at 15 input bytes
/// and multiplies them several-fold at 20, on tokens with no colon at all, and
/// still does not fix this issue. `-` and `_` ARE base64url's alphabet, so
/// relaxing the case rule there relaxes it for encoder output too. See
/// [`SYMBOL_PATH_SEPARATOR`] for why `::` carries no such cost.
/// That generalises to a rule, not a carve-out: the case rule may be relaxed for
/// a delimiter absent from every encoder alphabet, and `::` is currently the only
/// such delimiter.
///
/// Known accepted bound, measured: a credential a human writes in path syntax
/// (`secretKey::<blob>`) is the one way encoder output reaches this predicate,
/// and this roughly doubles the miss rate there (1017 -> 2022 per 30k). Pinned in
/// `symbol_path_keyhole_does_not_shelter_credentials` rather than left implicit.
/// Provider-prefixed keys are unaffected — [`SECRET_PREFIXES`] and
/// [`is_aws_access_key_id`] are checked before [`is_structural_token`].
/// What: requires `::`, at least two non-empty `::`-separated segments, and every
/// segment passing [`is_symbol_path_segment`].
/// Test: `rust_symbol_paths_are_not_flagged`,
/// `symbol_path_keyhole_does_not_shelter_credentials`,
/// `recurrence_corpus_has_no_false_positives`.
pub(crate) fn is_symbol_path(token: &str) -> bool {
    if !token.contains(SYMBOL_PATH_SEPARATOR) {
        return false;
    }
    let segments: Vec<&str> = token
        .split(SYMBOL_PATH_SEPARATOR)
        .filter(|s| !s.is_empty())
        .collect();
    segments.len() >= 2 && segments.iter().all(|s| is_symbol_path_segment(s))
}

pub(crate) fn is_segmented_identifier(token: &str) -> bool {
    if !token.contains(IDENTIFIER_DELIMITERS) {
        return false;
    }
    let segments: Vec<&str> = token.split(IDENTIFIER_DELIMITERS).collect();
    if segments.len() < 2 {
        return false;
    }
    segments.iter().all(|s| is_human_word_segment(s))
}

/// Longest unbroken alphabetic run a `/`-segment may carry and still read as a
/// path segment rather than an encoded run.
///
/// Why (issue #4977): the charset a path segment is drawn from is the charset an
/// encoded blob is drawn from, so charset alone cannot separate them and neither
/// can case — an ALL-UPPERCASE segment is `REQUEST_CHANGES` and is also
/// `XXXXXXXXXXXXXXXXXXXXXXXX`, the tail of the webhook URL
/// `real_secrets_still_blocked_after_4312_charset_gate` requires to stay flagged.
/// Length is what separates them: a path segment's words are words, and words
/// end.
/// What: inclusive maximum; a segment carrying a longer alphabetic run is not a
/// path segment. Tied to [`SECRET_MIN_LEN`] because that is already this
/// module's statement of "shorter than this cannot be a credential" — the
/// longest ordinary English word a URL slug carries (`internationalization`, 20)
/// sits exactly at the boundary and is admitted.
/// Test: `slash_bearing_base64_blobs_are_blocked`,
/// `bare_github_urls_are_not_flagged`.
pub(crate) const MAX_PATH_WORD_LEN: usize = SECRET_MIN_LEN;

/// Length of the longest unbroken ASCII-alphabetic run in `s`.
///
/// Why: see [`MAX_PATH_WORD_LEN`] — the discriminator between a path segment and
/// an encoded run at the one point where case and charset both fail.
/// What: one pass, digits and punctuation break the run.
/// Test: `slash_bearing_base64_blobs_are_blocked`.
pub(crate) fn longest_alpha_run(s: &str) -> usize {
    let (mut longest, mut cur) = (0usize, 0usize);
    for c in s.chars() {
        if c.is_ascii_alphabetic() {
            cur += 1;
            longest = longest.max(cur);
        } else {
            cur = 0;
        }
    }
    longest
}

/// A "word segment" for slash/equals splitting: non-empty, containing only ASCII
/// alphanumerics plus the minimal punctuation set legitimate structural tokens
/// carry.
///
/// Why each admitted character is admitted:
///   `-`  hyphen in slug/version segments (`prose-summary`, `v0.6.0`)
///   `_`  underscore in snake_case identifiers
///   `.`  dot in file extensions and semver (`synthesis.rs`, `v0.6.0`)
///   `>`  the lone `>` that arrives as the LHS of `>=2-medium->REQUEST_CHANGES`
///   `:`  Rust/module path separator inside a slash-path segment, e.g.
///        `client/http_client/error.rs::response_or_body_error` (issue #2442).
/// All other characters (`<`, `!`, `@`, `#`, `~`) are excluded — none appears in
/// the paths, slugs, or `key=value` tokens this gate needs to admit.
/// What: charset predicate only; it constrains what characters a segment is made
/// of, never how they are arranged. [`is_readable_path_segment`] is what asks the
/// second question.
/// Test: `structural_tokens_are_not_flagged`, `key_equals_slashpath_not_flagged`.
pub(crate) fn is_word_segment(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '>' | ':'))
}

/// True when `seg` reads as one segment of a path or URL — a slug, a directory
/// name, a filename, an issue number — rather than as a run of encoded bytes.
///
/// Why (issue #4977, the EIGHTH recurrence, and the first in the losing
/// direction): [`is_word_segment`] alone decided this, and it is a charset test.
/// Every `/`-separated run of a standard-base64 blob is pure alphanumeric, so
/// every such blob satisfied it and branch (b) of [`is_structural_token`]
/// exempted the whole token before the base64 branch of [`looks_like_secret`]
/// could see it. Measured over a 300k-sample corpus: 70,736 misses, 96.4% of them
/// `/`-bearing base64 — a larger detection gap than every false-positive round in
/// this file's history combined. The absolute miss rate for standard base64 was
/// 20–25%.
///
/// What the four arms are for, and why none alone is enough:
/// - **pure digits** — `/issues/5511`, `/projects/19`. No case or word structure
///   to test, and a digits-only run carries no alphabet, so no encoder alphabet
///   can be expressed in it (the same argument [`is_issue_number_list`] makes).
/// - **[`is_human_word_segment`]** — case uniformity. Admits `verdict`,
///   `prose-summary`, `REQUEST_CHANGES`, `gpt-5.4-mini-20260317`. This is the
///   permissive arm: an all-lowercase run of any shape rides in on it, which is
///   why the all-lowercase URL-path secret pinned in
///   `real_secrets_still_blocked_after_4312_charset_gate` stays missed.
/// - **[`is_symbol_path_segment`]** — CamelCase word structure, for the ordinary
///   path segments case uniformity declines: `MyComponent.tsx`, `Bm25Index`,
///   `README.md`. Without it, tightening branch (b) would have made every
///   CamelCase filename in a path a false positive — the ninth recurrence,
///   shipped in the same change as the eighth.
/// - **[`is_symbol_path`]** — a `::`-joined symbol path standing as one segment,
///   `error.rs::response_or_body_error` (issue #2442).
///
/// Why the recursion into [`looks_like_secret`] is bounded, stated because a
/// reader will rely on it: a segment reaching this function was produced by
/// splitting on `/`, so it contains no `/`. Branch (b) requires one, and
/// [`is_ordinary_url`] requires `://`, so neither can fire on the way back down.
/// Depth is 2, always. What it buys: an ALL-UPPERCASE segment passes the case arm
/// (that arm cannot tell `REQUEST_CHANGES` from `AKIAIOSFODNN7EXAMPLE`), and the
/// recursion is what catches a provider key parked in a URL path.
/// Test: `slash_bearing_base64_blobs_are_blocked`,
/// `bare_github_urls_are_not_flagged`, `structural_tokens_are_not_flagged`,
/// `recurrence_corpus_has_no_false_positives`.
pub(crate) fn is_readable_path_segment(seg: &str) -> bool {
    if !is_word_segment(seg) || longest_alpha_run(seg) > MAX_PATH_WORD_LEN {
        return false;
    }
    if seg.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    // #4977: a segment that is itself a credential is never a path segment.
    if looks_like_secret(seg) {
        return false;
    }
    is_human_word_segment(seg) || is_symbol_path_segment(seg) || is_symbol_path(seg)
}

/// True when every `/`-separated segment of `s` reads as a path segment.
///
/// Why it is a named helper: the `=` branch of [`is_structural_token`] validates
/// a `key=path/to/value` RHS with it without re-entering that function, keeping
/// the check one level deep.
/// What: requires at least one `/` and every segment passing
/// [`is_readable_path_segment`] (which rejects the empty segment a doubled or
/// trailing `/` produces).
/// Test: `key_equals_slashpath_not_flagged`, `slash_bearing_base64_blobs_are_blocked`.
pub(crate) fn is_slash_path(s: &str) -> bool {
    s.contains('/') && s.split('/').all(is_readable_path_segment)
}

/// True when `token` is an ordinary URL — one that carries no credential in its
/// userinfo and whose every authority/path segment reads as a path segment.
///
/// Why (issue #5513): a bare GitHub issue or PR link is the single most common
/// URL shape in this project's checkpoints, and it was rejected on the write path
/// whenever its path carried a digit — which every issue and PR number is. The
/// `://` puts a `/` in the token, so [`looks_like_secret`]'s base64 branch owns
/// it; branch (b) of [`is_structural_token`] could not rescue it because the
/// empty segment between the two slashes of `://` fails every segment test; and
/// the `has_upper || has_digit` entropy floor was then satisfied by the issue
/// number. A rejected write is a durable fact that never exists, so this failed
/// closed on data, not just on ergonomics.
///
/// Why the exemption is decomposition rather than a blanket "a bare URL is not a
/// credential": #4312 recorded that blanket rule as unsafe and it still is — a
/// userinfo-free URL can carry the secret in its PATH, which is exactly the
/// webhook shape `real_secrets_still_blocked_after_4312_charset_gate` requires to
/// stay flagged. Decomposing the URL and asking the same question of every
/// segment separates the two: `…/issues/5511` is a path all the way down,
/// `…/services/T00000000/B00000000/XXXX…` is not.
///
/// What: requires a `scheme://`, no `user:pass@` userinfo
/// ([`is_url_credential_shaped`]), a non-empty remainder, and every non-empty
/// `/`-separated segment of that remainder passing [`is_readable_path_segment`].
/// Empty segments are skipped rather than rejected so `file:///Users/masa/x` and
/// a trailing slash both decompose.
/// Test: `bare_github_urls_are_not_flagged`,
/// `url_path_secrets_are_still_blocked`, `url_shaped_prose_is_not_flagged`.
pub(crate) fn is_ordinary_url(token: &str) -> bool {
    let Some((scheme, rest)) = token.split_once("://") else {
        return false;
    };
    let scheme_ok = !scheme.is_empty()
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !scheme_ok || rest.is_empty() || is_url_credential_shaped(token) {
        return false;
    }
    rest.split('/')
        .filter(|s| !s.is_empty())
        .all(is_readable_path_segment)
}

/// True when `token` is a structured path/slug/key=value/compound-identifier
/// that should NOT be treated as a base64 blob or mixed-case credential.
///
/// Why (issues #1667, #1676): `looks_like_secret`'s heuristics (b64-symbol
/// check and mixed-case+digit check) fire on legitimate technical tokens.
/// Examples: `verdict/grade/prose-summary` contains `/` (b64 sym) → false
/// positive; `>=2-medium->REQUEST_CHANGES` is lower+upper+digit → false
/// positive on the mixed-case branch; `reviewer_model=openrouter/openai/...`
/// was routed to the slash-path branch, where the first `/`-segment
/// `reviewer_model=openrouter` contains `=` and fails `is_word_segment`,
/// causing a false positive (#1676).
/// This function recognises those structural shapes and short-circuits the
/// two dangerous branches in `looks_like_secret`.
///
/// How the branches divide the work (issue #4739, amended #4898 and #5513): the
/// URL guard returns first, so a token carrying `://` is decomposed as a URL and
/// never reaches the rest. The `+` guard is next and decides `+`-bearing tokens
/// on its own via [`is_plus_joined_word_phrase`]. Branches (a) and (b) then fire
/// only on tokens carrying `=` or `/`, so they are the ones that can rescue a
/// token from the base64 branch. Branch (c) is reachable only after all of `+`,
/// `=` and `/` are ruled out, i.e. only for tokens on which `has_b64_sym` is
/// false, so it serves the mixed-case branch exclusively. That partition is why a
/// fix to (c) cannot loosen base64 detection.
///
/// What every branch that admits a `/` now shares (issue #4977): one per-segment
/// predicate, [`is_readable_path_segment`]. Branch (b) used to ask a charset
/// question, which every `/`-separated run of a base64 blob answers `yes`; the
/// URL guard asks the same question of the same segments, so the loosening #5513
/// asks for and the tightening #4977 asks for are one decision made in one place
/// rather than two exemptions drifting apart.
/// What: returns `true` for (0) a userinfo-free URL whose every authority/path
/// segment reads as a path segment ([`is_ordinary_url`], checked first);
/// `+`-bearing tokens that are `+`-joined word phrases (checked next, and the
/// only way a `+` token can be structural); (a) `=`-containing tokens where the
/// LHS is a word segment and the RHS is itself structural (a word segment OR a
/// slash-path), checked before the slash-path branch so that tokens like
/// `key=path/to/value` are decomposed at `=` first; (b) slash-path tokens where
/// every `/`-segment reads as a path segment; or (c) `-`/`_`/`.`-segmented
/// compound identifiers where each segment is a single human-readable word.
/// Test: `structural_tokens_are_not_flagged`, `base64_blob_is_blocked`,
/// `key_equals_slashpath_not_flagged` (issue #1676 regression tests),
/// `dotted_capitalised_filenames_are_not_flagged` (issue #4739),
/// `three_4898_reproductions_are_not_flagged` (issue #4898),
/// `slash_bearing_base64_blobs_are_blocked` (issue #4977),
/// `bare_github_urls_are_not_flagged`, `url_path_secrets_are_still_blocked`
/// (issue #5513).
pub(crate) fn is_structural_token(token: &str) -> bool {
    // #5513: a URL is decomposed at `://` and `/` and decided segment by
    // segment, before the `+` guard so `mongodb+srv://host/db` is read as a URL.
    // A `user:pass@` URL is not ordinary and falls through to the heuristics.
    if is_ordinary_url(token) {
        return true;
    }
    // #4898: a `+`-bearing token is decided here and nowhere else — it is
    // structural only when it reads as a `+`-joined word phrase. Keeping the
    // early return (rather than letting `+` tokens fall through to branches (a)
    // and (b)) confines the change to this one shape: branch (a)'s `is_word_
    // segment(lhs) || is_word_segment(rhs)` fallback would otherwise exempt
    // `foo=<base64-with-plus>` on the strength of the `foo` alone.
    if token.contains('+') {
        return is_plus_joined_word_phrase(token);
    }
    // (a) key=value / semver-operator shape — checked BEFORE slash-path so
    // that tokens containing BOTH `=` and `/` (e.g.
    // `reviewer_model=openrouter/openai/gpt-5.4-mini-20260317`) are
    // decomposed at the `=` boundary first, letting the RHS be validated as
    // a slash-path rather than having the whole token routed to branch (b)
    // where the first `/`-segment (`reviewer_model=openrouter`) contains `=`
    // and fails `is_word_segment`.
    //
    // Compositional rule (issue #1676): LHS must be a word segment AND the
    // RHS must be EITHER a word segment OR a slash-path (all its `/`-separated
    // segments are word-like). A RHS that is a high-entropy opaque blob — no
    // slashes, not a simple word — is not structural, so the token falls
    // through to the entropy heuristics and is correctly flagged.
    // Exception: RHS that is pure `=` padding is non-structural (base64).
    if token.contains('=') {
        let parts: Vec<&str> = token.splitn(2, '=').collect();
        if parts.len() == 2 {
            let lhs = parts[0];
            let rhs = parts[1];
            if rhs.chars().all(|c| c == '=') {
                return false; // pure base64 padding, not key=value
            }
            if is_word_segment(lhs) && (is_word_segment(rhs) || is_slash_path(rhs)) {
                return true;
            }
            // #4977: the OR fallback below must not rescue a `/`-bearing RHS
            // that `is_slash_path` just declined — `key=<base64-with-slashes>`
            // is branch (b)'s blob wearing a `key=` prefix, and a word-shaped
            // LHS was enough to exempt it.
            if rhs.contains('/') {
                return false;
            }
            // Fall back to the original OR for semver-operator tokens like
            // `>=value` where the LHS may be a bare `>` and the RHS drives
            // the structural signal.
            return is_word_segment(lhs) || is_word_segment(rhs);
        }
    }
    // (b) Path/slug shape: every `/`-separated segment reads as a path segment.
    // Covers `verdict/grade/prose-summary`, `org/repo`, file paths.
    //
    // #4977: the per-segment test is `is_readable_path_segment`, not the bare
    // charset test it used to be — every `/`-separated run of a standard-base64
    // blob is pure alphanumeric, so the charset test exempted one in five of
    // them before the base64 branch could see them.
    if token.contains('/') {
        return token.split('/').all(is_readable_path_segment);
    }
    // #5043: a `::`-joined symbol path is decided before branch (c), whose
    // case-uniformity rule a CamelCase segment can never satisfy.
    if is_symbol_path(token) {
        return true;
    }
    // (c) Hyphen-segmented compound identifier: no b64 syms remain at this
    // point; check whether every `-`-separated segment is uniform-case.
    // Covers `2-medium->REQUEST_CHANGES`, `trusty-review-v0.6.0`.
    is_segmented_identifier(token)
}

/// Heuristic: is `token` a likely secret/credential (and not a git SHA)?
///
/// Why (issue #1481): see [`find_secret_token`]. This is the core decision —
/// it must say "no" to git SHAs and "yes" to credentials.
/// What: returns `false` immediately for [`is_git_sha_like`] tokens (the
/// allowlist) and for anything below [`SECRET_MIN_LEN`], then returns `true`
/// when the token (a) carries a known [`SECRET_PREFIXES`] credential prefix
/// (e.g. `sk-`, `ghp_`) or has the AWS key-id shape ([`is_aws_access_key_id`]),
/// or (b) is not a structural token (see
/// [`is_structural_token`]) AND mixes character classes in a way SHAs cannot
/// — i.e. contains BOTH a lowercase and an uppercase letter plus a digit, or
/// contains a base64-indicator symbol (`+`) or an `=`/`/` that is NOT part of
/// a structural path/slug/key=value token. Pure lowercase-hex (SHA-shaped or
/// longer all-hex), all-uppercase tokens without a known prefix, ordinary words,
/// path-like tokens, and compound identifiers are not flagged.
///
/// **Known limitation (FN-2, issue #1484):** mixed-case-but-no-digit tokens
/// ≥ 20 chars (e.g. base58 key segments like `xPubKeySegmentAbCdEf`) are not
/// flagged by the (b) fallback because `has_digit` is `false`. The prefix list
/// (a) remains the authoritative gate for well-known credential formats. If
/// your deployment stores content that may contain bare mixed-case-alphabetic
/// high-entropy blobs without a known prefix, add a custom prefix or extend
/// the pattern list in `FilterConfig`.
/// Test: `secret_token_is_blocked`, `base64_blob_is_blocked`,
/// `git_sha_like_is_not_secret`, `ordinary_words_are_not_secret`,
/// `aws_access_key_ids_are_blocked`, `mixed_case_no_digit_limitation`,
/// `structural_tokens_are_not_flagged`.
pub(crate) fn looks_like_secret(token: &str) -> bool {
    // Allowlist git SHAs first — the whole point of issue #1481.
    if is_git_sha_like(token) {
        return false;
    }
    // Allowlist issue/PR-number lists — issue #2800, same spirit as the SHA
    // carve-out above.
    if is_issue_number_list(token) {
        return false;
    }
    // #4898: the length floor now runs BEFORE the prefix test. It used to run
    // after, so a 4-char token equal to a prefix (`Asia`) was flagged.
    if token.len() < SECRET_MIN_LEN {
        return false;
    }
    let lower = token.to_ascii_lowercase();
    if SECRET_PREFIXES.iter().any(|p| lower.starts_with(p)) || is_aws_access_key_id(token) {
        return true;
    }
    // Issue #1667: structural tokens (paths, slugs, key=value pairs, and
    // hyphen-segmented compound identifiers) must not be flagged as secrets
    // even when they contain `/`, `=`, or mixed case across segments.
    if is_structural_token(token) {
        return false;
    }
    let has_lower = token.chars().any(|c| c.is_ascii_lowercase());
    let has_upper = token.chars().any(|c| c.is_ascii_uppercase());
    let has_digit = token.chars().any(|c| c.is_ascii_digit());
    let has_b64_sym = token.chars().any(|c| matches!(c, '+' | '/' | '='));
    // base64/url-safe blob: long, not structural, and carries a base64 symbol.
    //
    // #4312: two gates, both narrowing an ≥20-char-plus-a-slash rule that four
    // rounds of false positives (#1667, #2800, #4216, #4312) all bottomed out
    // in. (1) [`is_plausible_b64_charset`] — the same treatment #2442 gave the
    // mixed-case branch below. (2) an entropy floor: base64 encodes bytes, so a
    // ≥20-char encoded run all but certainly carries an uppercase letter or a
    // digit; an all-lowercase run is English. URL-shaped credentials are
    // structured, not encoded, so they are exempted by shape — see
    // [`is_url_credential_shaped`].
    if has_b64_sym
        && is_plausible_b64_charset(token)
        && (is_url_credential_shaped(token) || has_upper || has_digit)
    {
        return true;
    }
    // Mixed-case alphanumeric of credential length: SHAs are single-case hex,
    // so requiring BOTH cases plus a digit excludes them while catching the
    // typical `AbCd12…` / JWT-segment shapes. NOTE (FN-1, issue #1481): this
    // does NOT catch AWS access key IDs — `AKIAIOSFODNN7EXAMPLE` is // pragma: allowlist secret
    // all-uppercase base32 (`has_lower == false`), so it relies entirely on
    // `is_aws_access_key_id` above (#4898 moved that out of SECRET_PREFIXES).
    //
    // Issue #2442: this fallback used to fire on ANY ≥20-char token that
    // mixed case + digit, regardless of what other punctuation it carried.
    // A real-world false positive: an issue/PR/SHA "ledger" reference like
    // `#2486→PR#2491(e993c18a)` mixes digits, uppercase (`PR`), and lowercase
    // hex — but no credential format ever contains `#`, arrows, or parens.
    // Gate the fallback on [`is_plausible_credential_charset`] so it only
    // fires for tokens shaped like an actual bare credential (alphanumeric
    // plus the `-`/`_`/`.` separators used by JWTs and slug-style API keys).
    //
    // #4739: that charset gate is necessary but not sufficient — an ordinary
    // dotted filename is built from exactly this charset. The shape gate is
    // `is_structural_token` branch (c), widened there rather than here so the
    // two branches keep sharing one notion of "human-readable token" instead of
    // each growing its own allowlist for the fifth time.
    has_lower && has_upper && has_digit && is_plausible_credential_charset(token)
}

/// True when `token` is a slash-separated issue/PR-number list — built only
/// from `#`, `/`, and ASCII digits, with at least one digit.
///
/// Why (issue #2800, observed live twice): PM session checkpoints routinely
/// enumerate tickets as `#2763/#2774/#2780/#2782/#2790`. Such a token carries
/// no letters at all, but the `/` separators set `has_b64_sym` and the digits
/// set `has_digit`, so the base64-blob branch of [`looks_like_secret`] fired
/// and the whole memory was rejected. The slash-path branch of
/// [`is_structural_token`] does not rescue it because `#` is deliberately
/// excluded from `is_word_segment` — and widening *that* set would loosen the
/// structural bypass for every path and `key=value` token, which is a much
/// larger blast radius than this false positive warrants. Agents worked around
/// the rejection by rewording or dropping detail, silently degrading
/// checkpoint fidelity.
///
/// Why this is safe as an allowlist: the `{#, /, 0-9}` charset has no
/// character-class spread — no letters, so no base64, hex, or base32 alphabet
/// can be expressed in it, and every known credential format contains letters
/// (and none contains `#`). A single alphabetic character takes a token out of
/// this exemption and back onto the normal heuristic path. The exemption is
/// therefore a keyhole, not a hole: it is strictly narrower than the git-SHA
/// carve-out above, which admits the full hex alphabet.
///
/// What: returns `true` iff `token` is non-empty, every char is `#`, `/`, or an
/// ASCII digit, and at least one char is a digit. Note that
/// [`find_secret_token`] trims a leading `#` before classification, so the
/// trimmed form (`2763/#2774/…`) must satisfy this predicate too — it does,
/// since the charset is closed under that trim.
/// Test: `slash_separated_pr_number_list_not_flagged`,
/// `real_secrets_still_blocked_after_2800_exemption`.
pub(crate) fn is_issue_number_list(token: &str) -> bool {
    !token.is_empty()
        && token.bytes().any(|b| b.is_ascii_digit())
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'#' | b'/'))
}

/// True when every character in `token` could plausibly appear in a base64,
/// base64url, or URL-embedded credential: ASCII alphanumeric, the base64
/// symbols `+` `/` `=`, the base64url symbols `-` `_`, and the `.` `:` `@` that
/// connection-string credentials (`postgres://user:pass@host/db`) carry.
///
/// Why (issue #4312): the base64 branch of [`looks_like_secret`] fired on ANY
/// ≥20-char token containing a `/` plus one letter that [`is_structural_token`]
/// declined to rescue. That is the root cause of four rounds of false positives
/// — #1667 (slash paths), #2800 / #4216 (issue-number lists), #4312 (backtick
/// spans) — each previously patched with another allowlist for one token shape.
/// The shapes kept recurring because `is_word_segment`'s charset is deliberately
/// narrow, and every character outside it (backtick, `*`, `"`, `'`, `[`, `(`,
/// `|`, `%`, `,`) routes an ordinary piece of Markdown prose into a credential
/// verdict. Gating the branch on the charset a real blob is *made of* attacks
/// the cause instead of enumerating the symptoms.
///
/// Why this is safe: every machine-generated credential format this module
/// targets is drawn from this charset by construction — standard base64
/// (`A–Za–z0–9+/=`), base64url (`A–Za–z0–9-_=`), JWTs (base64url plus `.`),
/// and connection-string URLs (`scheme://user:pass@host/db`). Prose punctuation
/// is what the gate excludes, and prose punctuation is exactly what no encoder
/// emits. The known-prefix layer (`sk-`, `ghp_`, `AKIA`, …) is checked earlier
/// and is unaffected, so provider keys never depend on this branch at all.
///
/// Known bound: a token built only from this charset still reaches the branch,
/// so a hyphen/plus-joined English phrase such as `ticker+shutdown-channel`
/// depends on a second gate. That gate is the `has_upper || has_digit` entropy
/// floor in [`looks_like_secret`], with [`is_url_credential_shaped`] exempting
/// connection strings from it. The floor covers the all-lowercase case only,
/// which left `PM+instructions+subagents` flagged — issue #4898 closed that
/// residue upstream instead, in [`is_plus_joined_word_phrase`], so a
/// `+`-joined phrase is now rescued before it reaches this branch at all.
/// What: returns `true` iff every char is ASCII alphanumeric or in
/// `{'+', '/', '=', '-', '_', '.', ':', '@'}`.
/// Test: `four_4312_acceptance_cases_are_not_flagged`,
/// `markdown_decorated_paths_are_not_flagged`,
/// `real_secrets_still_blocked_after_4312_charset_gate`.
pub(crate) fn is_plausible_b64_charset(token: &str) -> bool {
    token.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_' | '.' | ':' | '@')
    })
}

/// True when `token` is a URL that actually carries credentials — a `://`
/// scheme separator followed by a colon-bearing userinfo before the first `@`,
/// i.e. the `scheme://user:pass@host` shape.
///
/// Why (issue #4312, repro 4): the entropy floor added alongside this
/// (`has_upper || has_digit` in [`looks_like_secret`]'s base64 branch) is what
/// finally excludes `ticker+shutdown-channel` — a lowercase English phrase
/// whose every character is individually credential-plausible. That floor
/// would also drop an all-lowercase connection string
/// (`postgres://user:password@host/database`), which is a genuine credential
/// this module has always caught. A connection string is not a blob: it is a
/// structured URL whose password sits inside it, so it is exempted by shape
/// rather than by entropy.
///
/// Why the predicate is this narrow: the first cut exempted any token
/// containing `://` OR `@`, which held open 14 URL-shaped prose false
/// positives the floor would otherwise have fixed — bare `https://` doc links,
/// `git@github.com:…` remotes, plain `mailto`-ish addresses. Requiring the
/// `user:pass@` userinfo keeps every real connection string.
///
/// Cost of that narrowing, stated exactly because this is a security module:
/// this predicate is an OR term inside the branch's conjunction, so widening it
/// widens flagging and **narrowing it narrows flagging** — narrowing therefore
/// CAN turn a caught credential into a miss. The exposure is bounded to tokens
/// that reach this branch, pass the charset gate, and carry neither an
/// uppercase letter nor a digit, i.e. a userinfo-free URL whose path secret is
/// all-lowercase:
///
/// ```text
/// https://webhook.example.com/services/abcdefghij/klmnopqrst/uvwxyzabcdefghij
///     origin/main: flagged  ->  here: missed
/// ```
///
/// That is the entropy floor's already-accepted bound (an all-lowercase run is
/// read as English), not a new class of hole — the broad predicate shielded
/// these incidentally, never by design. Real webhook tokens are near-universally
/// mixed-case or digit-bearing and stay caught; see the known-miss assertion in
/// `real_secrets_still_blocked_after_4312_charset_gate`.
/// Second consumer since #5513: [`is_ordinary_url`] uses this predicate in the
/// opposite polarity — a URL that IS credential-shaped is not an ordinary URL and
/// gets no exemption. Widening this predicate therefore now widens flagging in
/// one place and narrows the URL exemption in another; both move the same way, so
/// a change here still cannot silently lose a connection string.
/// What: returns `true` iff `token` contains `://` and the text between it and
/// the first following `@` contains a `:`.
/// Test: `four_4312_acceptance_cases_are_not_flagged`,
/// `url_shaped_prose_is_not_flagged`,
/// `real_secrets_still_blocked_after_4312_charset_gate`,
/// `url_path_secrets_are_still_blocked`.
pub(crate) fn is_url_credential_shaped(token: &str) -> bool {
    // #4312: `scheme://user:pass@host` only — a bare URL carries no userinfo.
    let Some((_, after_scheme)) = token.split_once("://") else {
        return false;
    };
    let Some((userinfo, _)) = after_scheme.split_once('@') else {
        return false;
    };
    userinfo.contains(':')
}

/// True when every character in `token` could plausibly appear in a bare
/// credential/API-key string: ASCII alphanumeric, or one of `-`, `_`, `.`,
/// `:` (hyphen/underscore-delimited key segments, JWT `.`-separated parts,
/// colon-delimited `user:secret` / `Bearer:token` shapes).
///
/// Why (issue #2442): the mixed-case-plus-digit fallback in
/// [`looks_like_secret`] must not fire on prose punctuation that a real
/// credential never contains — issue/PR ledger markers (`#`), arrows (`→`,
/// `->`), parentheses, etc. Restricting the fallback to tokens built only
/// from this charset keeps it precise without weakening the base64-symbol
/// branch (which already requires `+`/`/`/`=` and is unaffected by this gate)
/// or the known-prefix layer (checked earlier, unaffected).
///
/// Why `:` is included (issue #2520 review, BLOCKER regression fix): the
/// first cut of this function omitted `:`, which meant ANY colon in a token
/// — including a bare colon-delimited credential like
/// `token:aBc123XyZ987uvW456QrS` — flunked the `.all()` check and silently
/// disabled the fallback (`find_secret_token` returned `None` where
/// `origin/main`'s pre-#2442 code, which had no charset gate at all,
/// correctly returned `Some`). The `path::fn` false positive this module
/// fixes is handled entirely by `is_structural_token`'s slash-path branch
/// (checked earlier in [`looks_like_secret`] and gated on `/` being
/// present), so admitting `:` here does not reopen it — a bare
/// `path::fn`-shaped token with no digit/mixed-case never reaches the
/// fallback in the first place, and one that DOES contain a `/` short-
/// circuits via `is_structural_token` before this function is ever called.
/// What: returns `true` iff every char is ASCII alphanumeric or in
/// `{'-', '_', '.', ':'}`.
/// Test: `ledger_reference_token_not_flagged`, `secret_token_is_blocked`,
/// `real_secrets_still_blocked_after_1667_fix`,
/// `colon_bearing_credential_is_flagged`.
pub(crate) fn is_plausible_credential_charset(token: &str) -> bool {
    token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

/// Produce a short, non-reversible preview of a flagged secret token.
///
/// Why (issue #1481): the rejection message must name *which* token tripped the
/// gate (so the caller can find and remove it) without echoing the full secret
/// back into logs or responses. Issue #2401 consolidated the masking logic
/// itself into `credentials::redact_secret` — the `config` clap
/// module (epic #2400) needs the identical shape, and duplicating it here
/// would violate the "one implementation per behaviour" rule. This function
/// is now a thin delegating wrapper kept for call-site stability and doc
/// continuity at this module's original issue (#1481).
/// What: returns the first up-to-4 characters followed by `…` and the token
/// length, e.g. `sk-A…(48 chars)`. Short tokens (≤ 4 chars never reach here
/// because the secret heuristic requires length) are returned verbatim.
/// Test: `redact_token_masks_tail` (format stability); the masking
/// implementation itself is tested in
/// `credentials::redact::tests`.
pub(crate) fn redact_token(token: &str) -> String {
    crate::credentials::redact_secret(token)
}

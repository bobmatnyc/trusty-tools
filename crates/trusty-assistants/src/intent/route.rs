//! `dispatch_task` backend router — decides `Tm` (orchestration) vs `Tcode`
//! (direct coding) for a black-boxed unit of work (epic #3052, PR B, lane 3).
//!
//! Why: `dispatch_task` (see `crate::tools::pm_bridge`) is intentionally
//! backend-agnostic from the caller's point of view — the LLM never names
//! `tm`/`tcode`. Something still has to pick a real backend deterministically
//! (same input -> same route, every time, no I/O, no LLM call) so the tool's
//! behavior is testable and auditable independent of the model that calls it.
//! This is a separate module rather than part of `intent::classify_intent`
//! because the two answer orthogonal questions: the classifier decides how much
//! machinery an input needs (conversational / research / implementation), while
//! `route` decides — once the answer is already "hand this off" — WHICH
//! black-boxed backend receives it.
//! What: `route_task` classifies a free-text task string into `BridgeRoute`
//! using ordered heuristics over two signal tables (Tcode: coding verbs,
//! repo-file tokens, error/stack-trace markers; Tm: orchestration/session/
//! ticket/roadmap vocabulary) — modeled on `intent::classify_intent`'s
//! constant-table + priority-order approach, reusing `intent::normalize` for
//! punctuation-insensitive matching.
//! Test: `route_tests` covers pure-Tcode, pure-Tm, the required ambiguous
//! cases (including the locked Tm default), and a determinism/no-panic
//! property sweep mirroring `intent::classifier_property_tests`.

use super::normalize;

/// Backend a `dispatch_task` call is routed to.
///
/// Why: the two black-boxed execution paths PR B bridges — `tm` (orchestration
/// / project / session / issue / PR / multi-agent work) and `tcode` (direct
/// coding in a repo). Never surfaced to the caller by name; see
/// `crate::tools::pm_bridge`'s module docs for the black-box contract.
/// What: `Tm` is the OWNER-LOCKED default for ambiguous/empty input —
/// orchestration is the home lane. `Tcode` is chosen only when the task text
/// carries a clear coding signal.
/// Test: every case in `route_tests`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeRoute {
    Tm,
    Tcode,
}

/// The four "hard" Tcode verbs — concrete enough to win over ANY Tm signal
/// in the same input (see `route_task`'s precedence rule 1).
///
/// Why: "fix the sprint backlog" reads like Tm vocabulary (sprint, backlog)
/// but "fix" names concrete repo work; a human handed that sentence would
/// reach for a code change, not a ticket. Keeping this list short and
/// deliberately verb-only (not `write`/`add`/`create`, which are softer —
/// see `GENERIC_CODE_VERBS`) avoids over-triggering the highest-precedence
/// rule.
///
/// `pub(crate)` (owner-approved #4319 follow-up, 2026-07-29) so
/// `intent::classify_intent` can reuse the SAME "these 4 verbs are
/// concrete enough to need no other evidence" judgment this module already
/// makes for `route_task`, instead of maintaining a second, independently
/// drifting "which verbs are unambiguously technical" list.
pub(crate) const TCODE_HARD_VERBS: &[&str] = &["fix", "debug", "implement", "refactor"];

/// Full Tcode single-token signal set (superset of `TCODE_HARD_VERBS`), used
/// for the fallback per-side match count (precedence rule 4).
const TCODE_WORDS: &[&str] = &[
    "write",
    "edit",
    "implement",
    "refactor",
    "fix",
    "bug",
    "patch",
    "debug",
    "compile",
    "rename",
];

/// Multi-word Tcode phrases, matched as substrings of the normalized string
/// (they contain no punctuation, so normalization leaves them intact).
const TCODE_PHRASES: &[&str] = &["unit test", "failing test", "stack trace"];

/// File-extension tokens that name a source file — checked against the RAW
/// (pre-normalization) lowercased input, because `intent::normalize` maps
/// `.`/`/` to whitespace and would destroy `main.rs` / `src/` as tokens.
const REPO_FILE_EXTENSIONS: &[&str] = &[".rs", ".ts", ".py"];

/// Path-shaped token that names a source tree, checked the same
/// pre-normalization way as `REPO_FILE_EXTENSIONS`.
const REPO_FILE_PATH_TOKEN: &str = "src/";

/// Raw (pre-normalization) substring markers — `error:` includes a colon,
/// which `intent::normalize` also strips, so it must be checked before
/// normalization like the repo-file tokens above.
const RAW_TCODE_MARKERS: &[&str] = &["error:"];

/// Generic code verbs that route to Tcode ONLY when no Tm signal is present
/// (precedence rule 3) — deliberately softer than `TCODE_HARD_VERBS` because
/// "write up the project roadmap" should NOT out-rank "project"/"roadmap".
///
/// `run`/`build` added (owner-approved #4319 follow-up, 2026-07-29): the
/// owner declared "run the tests"/"build the release" -> `Tcode` a
/// non-negotiable requirement, but without these two words in
/// `GENERIC_CODE_VERBS`, `route_task` had no Tcode signal for either
/// sentence at all and fell to the Tm default. See
/// `RUN_BUILD_TCODE_ARTIFACT_WORDS` below for the additional narrowing
/// `run`/`build` specifically need before this rule fires for them.
const GENERIC_CODE_VERBS: &[&str] = &["write", "add", "create", "run", "build"];

/// route.rs-LOCAL ONLY exception (code-critic HIGH correction, 2026-07-29):
/// code-artifact nouns that let `run`/`build` win rule 3 even without a
/// `has_tcode_lexical_signal` hit — the ONLY way `route_task` currently
/// resolves "run the tests"/"build the release" to `Tcode`.
///
/// Why this is safe HERE but was NOT safe as a shared
/// `intent::classify_intent` exception: `route_task` only ever runs after
/// the LLM has already decided to call `dispatch_task`, and it picks
/// between two backends that both execute safely (`Tm` orchestration vs
/// `Tcode` direct coding) — a wrong guess here is a misrouted backend, not
/// a crash. `classify_intent`'s equivalent exception was deleted because a
/// wrong guess THERE could reach `task_runner.rs`'s subprocess-spawn crash
/// path (`"check my blood tests"`, `"check the release date of the movie"`
/// both hit that shared exception and both crashed) — that specific risk
/// does not exist in this module. Do NOT re-share this list with
/// `intent::classify_intent`; that module's `Implementation` gate stays
/// zero-exception (see `intent::has_unambiguous_technical_signal`'s doc
/// comment).
/// What: matched via exact word-token equality against normalized input,
/// same as `GENERIC_CODE_VERBS`/`TCODE_WORDS`. Deliberately tiny — "tests"/
/// "release" are themselves imperfect (a "release" can mean a movie
/// release), but in "run/build the ARTIFACT" with no other signal, they
/// overwhelmingly mean a software test suite / build artifact in this
/// product's actual traffic, and any false positive here still lands on a
/// non-crashing backend choice.
/// Test: `route_tests::run_and_build_route_tcode_via_local_artifact_exception`,
/// `route_tests::run_and_build_require_a_real_technical_signal_not_just_the_bare_verb`.
const RUN_BUILD_TCODE_ARTIFACT_WORDS: &[&str] = &["tests", "release"];

/// Tm single-token signal set.
const TM_WORDS: &[&str] = &[
    "orchestrate",
    "session",
    "sessions",
    "delegate",
    "agents",
    "multi-agent",
    "project",
    "issue",
    "issues",
    "ticket",
    "pr",
    "backlog",
    "sprint",
    "prioritize",
    "roadmap",
    "spawn",
    "resume",
    "pause",
    "coordinate",
];

/// Multi-word Tm phrases, matched as substrings of the normalized string.
const TM_PHRASES: &[&str] = &["pull request", "status of", "across repos"];

/// Route a free-text task to a backend, deterministically and with no I/O.
///
/// Why: see the module docs — this is the single choke point every
/// `dispatch_task` call passes through before a backend is ever invoked.
/// What: applies the precedence order in the module docs: (1) a repo-file
/// token or a hard Tcode verb wins outright; (2) else any Tm signal wins;
/// (3) else a generic code verb (with no Tm signal already ruled out by (2))
/// AND a genuine Tcode lexical signal (code-critic MEDIUM, 2026-07-29 —
/// `run`/`build` alone are common non-coding verbs; see
/// `run_generic_verb_requires_tcode_signal` below) routes Tcode; (4) else the
/// side with more matched signals wins; (5) a tie (including zero signals on
/// both sides, and empty/whitespace-only input) falls back to the
/// OWNER-LOCKED default, `Tm`.
/// Test: `route_tests::*`.
pub fn route_task(task: &str) -> BridgeRoute {
    let raw_lower = task.to_lowercase();
    let normalized = normalize(task);
    if normalized.is_empty() {
        // Empty / whitespace-only / all-punctuation input carries no signal.
        return BridgeRoute::Tm;
    }
    let words: Vec<&str> = normalized.split_whitespace().collect();

    // Rule 1: repo-file token or a hard Tcode verb wins outright, even over
    // Tm vocabulary in the same sentence.
    let has_hard_tcode_verb = words.iter().any(|w| TCODE_HARD_VERBS.contains(w));
    if has_repo_file_token(task) || has_hard_tcode_verb {
        return BridgeRoute::Tcode;
    }

    // Rule 2: any Tm signal wins once rule 1 has been ruled out.
    let tm_count =
        count_word_matches(&words, TM_WORDS) + count_phrase_matches(&normalized, TM_PHRASES);
    if tm_count > 0 {
        return BridgeRoute::Tm;
    }

    // Rule 3: a generic code verb routes Tcode — only reachable here because
    // rule 2 already proved there is no competing Tm signal.
    //
    // Code-critic MEDIUM (2026-07-29): `run`/`build` are common non-coding
    // verbs ("run to the store", "build rapport with the client", "run point
    // on this deal") — unlike `write`/`add`/`create`, which stayed in the
    // original design because a Tm signal veto (rule 2) was judged
    // sufficient protection for them, `run`/`build` additionally require a
    // genuine Tcode-flavored co-occurrence in the SAME input before they
    // win: `has_tcode_lexical_signal` (a repo-file token, error/stack-trace
    // marker, `TCODE_PHRASES` substring, or `TCODE_WORDS` token), OR the
    // narrow `run`/`build`-specific exception below. `write`/`add`/`create`
    // keep the original, softer rule (Tm-signal veto only) — narrowing them
    // too is out of this fix's scope and unproven to be a problem today.
    //
    // #4319 code-critic CRITICAL THIRD follow-up (2026-07-29) THEN
    // code-critic HIGH correction (same day): this gate's "tests"/"release"
    // exception was briefly SHARED with `intent::classify_intent` (mirrored
    // via a `super::` reference into that module), then deleted from BOTH
    // places when the shared version was proven to reopen the #4319 crash
    // class one level down ("check my blood tests", "check the release date
    // of the movie" — see `intent::has_unambiguous_technical_signal`'s doc
    // comment). Deleting it here too was a mistake: `route_task` only ever
    // runs AFTER the LLM has already chosen to call `dispatch_task`, and it
    // picks between two SAFE, non-crashing backends (`Tm` vs `Tcode`) — the
    // "blood tests" ambiguity that forces zero-exception on
    // `classify_intent` (where a wrong guess reaches a real subprocess
    // crash) does not apply here; a wrong guess in THIS function costs a
    // misrouted backend, never a crash. The owner separately declared "run
    // the tests"/"build the release" -> Tcode non-negotiable, and that
    // requirement was never actually retracted. So the exception is
    // restored HERE, ROUTE.RS-LOCAL ONLY (`RUN_BUILD_TCODE_ARTIFACT_WORDS`
    // below) — NOT reinstated in `classify_intent`, which stays
    // zero-exception.
    let has_run_or_build = words.iter().any(|w| *w == "run" || *w == "build");
    if has_run_or_build {
        if has_tcode_lexical_signal(task)
            || words
                .iter()
                .any(|w| RUN_BUILD_TCODE_ARTIFACT_WORDS.contains(w))
        {
            return BridgeRoute::Tcode;
        }
    } else if words.iter().any(|w| GENERIC_CODE_VERBS.contains(w)) {
        return BridgeRoute::Tcode;
    }

    // Rule 4/5: fall back to a full per-side count (tm_count is provably 0
    // here, so this reduces to "any remaining Tcode signal -> Tcode, else
    // the locked Tm default" — kept as an explicit comparison rather than a
    // bare `> 0` check so the code reads as the documented "higher count
    // wins, tie -> Tm" rule instead of a special case).
    let tcode_count = count_word_matches(&words, TCODE_WORDS)
        + count_phrase_matches(&normalized, TCODE_PHRASES)
        + usize::from(RAW_TCODE_MARKERS.iter().any(|m| raw_lower.contains(m)));
    if tcode_count > tm_count {
        BridgeRoute::Tcode
    } else {
        BridgeRoute::Tm
    }
}

/// True when `input` carries a repo-file-shaped token: a known source
/// extension (`.rs`/`.ts`/`.py`) or the `src/` path token, checked against
/// the RAW (pre-normalization) lowercased input — `intent::normalize` maps
/// `.`/`/` to whitespace and would destroy these as tokens.
///
/// `pub(crate)` (code-critic direction, 2026-07-29) so
/// `intent::classify_intent`'s unambiguous-signal gate can reuse this exact
/// check instead of a second implementation.
/// Test: `route_tests::has_repo_file_token_*`.
pub(crate) fn has_repo_file_token(input: &str) -> bool {
    let raw_lower = input.to_lowercase();
    REPO_FILE_EXTENSIONS
        .iter()
        .any(|ext| raw_lower.contains(ext))
        || raw_lower.contains(REPO_FILE_PATH_TOKEN)
}

/// True when `input` carries an explicit error/stack-trace marker: the raw
/// `error:` substring, or one of `TCODE_PHRASES` ("unit test", "failing
/// test", "stack trace") as a substring of the normalized text.
///
/// Why: these are UNAMBIGUOUS — unlike a bare `TCODE_WORDS` token (e.g.
/// "bug", "patch"), a literal `error:` prefix or a "stack trace"/"failing
/// test" phrase essentially never appears in non-technical writing.
/// `pub(crate)` (code-critic direction, 2026-07-29) so
/// `intent::classify_intent`'s unambiguous-signal gate can reuse this exact
/// check — "reuse `route::stack_trace_and_error_marker`, do not write a
/// second detector" — instead of a second implementation.
/// Test: `route_tests::has_error_or_stack_trace_marker_*`.
pub(crate) fn has_error_or_stack_trace_marker(input: &str) -> bool {
    let raw_lower = input.to_lowercase();
    let normalized = normalize(input);
    RAW_TCODE_MARKERS.iter().any(|m| raw_lower.contains(m))
        || TCODE_PHRASES.iter().any(|p| normalized.contains(p))
}

/// Count how many words in `list` appear (exact token match) in `words`.
fn count_word_matches(words: &[&str], list: &[&str]) -> usize {
    list.iter().filter(|w| words.contains(w)).count()
}

/// Count how many phrases in `list` appear as substrings of `normalized`.
fn count_phrase_matches(normalized: &str, list: &[&str]) -> usize {
    list.iter().filter(|p| normalized.contains(*p)).count()
}

/// #4319 (code-critic HIGH-1): True when `input` carries any of the
/// Tcode-side lexical signals this module already detects for
/// `route_task` — a repo-file token, a raw `error:` marker, a
/// `TCODE_PHRASES` substring, or a `TCODE_WORDS` token — independent of
/// what `route_task`'s full Tm-vs-Tcode precedence order would ultimately
/// decide between the two.
///
/// Why: `intent::classify_intent`'s bug-report `Research` fallback needs
/// "does this look like it's actually about code" as one of its inputs.
/// Reusing these exact constants/checks avoids a second, independently
/// drifting implementation of the same detection — the same
/// common-entry-point principle `intent::normalize` already follows for
/// punctuation handling.
/// What: repo-file extension/path token check against the RAW lowercased
/// input (pre-normalization, matching `route_task`'s own precedent since
/// `normalize` maps `.`/`/` to whitespace), `error:` raw substring check,
/// `TCODE_PHRASES` substring check against normalized text, `TCODE_WORDS`
/// exact-token check against normalized words.
/// Test: `route_tests::has_tcode_lexical_signal_*`.
pub(crate) fn has_tcode_lexical_signal(input: &str) -> bool {
    let raw_lower = input.to_lowercase();
    let normalized = normalize(input);
    let words: Vec<&str> = normalized.split_whitespace().collect();

    let has_repo_file_token = REPO_FILE_EXTENSIONS
        .iter()
        .any(|ext| raw_lower.contains(ext))
        || raw_lower.contains(REPO_FILE_PATH_TOKEN);
    let has_raw_marker = RAW_TCODE_MARKERS.iter().any(|m| raw_lower.contains(m));
    let has_tcode_phrase = TCODE_PHRASES.iter().any(|p| normalized.contains(p));
    let has_tcode_word = words.iter().any(|w| TCODE_WORDS.contains(w));

    has_repo_file_token || has_raw_marker || has_tcode_phrase || has_tcode_word
}

#[cfg(test)]
#[path = "route_tests.rs"]
mod route_tests;

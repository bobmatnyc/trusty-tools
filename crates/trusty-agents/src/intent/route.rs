//! `dispatch_task` backend router — decides `Tm` (orchestration) vs `Tcode`
//! (direct coding) for a black-boxed unit of work (epic #3052, PR B, lane 3).
//!
//! Why: `dispatch_task` (see `crate::tools::pm_bridge`) is intentionally
//! backend-agnostic from the caller's point of view — the LLM never names
//! `tm`/`tcode`. Something still has to pick a real backend deterministically
//! (same input -> same route, every time, no I/O, no LLM call) so the tool's
//! behavior is testable and auditable independent of the model that calls it.
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
const TCODE_HARD_VERBS: &[&str] = &["fix", "debug", "implement", "refactor"];

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
const GENERIC_CODE_VERBS: &[&str] = &["write", "add", "create"];

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
/// routes Tcode; (4) else the side with more matched signals wins; (5) a tie
/// (including zero signals on both sides, and empty/whitespace-only input)
/// falls back to the OWNER-LOCKED default, `Tm`.
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
    let has_repo_file_token = REPO_FILE_EXTENSIONS
        .iter()
        .any(|ext| raw_lower.contains(ext))
        || raw_lower.contains(REPO_FILE_PATH_TOKEN);
    let has_hard_tcode_verb = words.iter().any(|w| TCODE_HARD_VERBS.contains(w));
    if has_repo_file_token || has_hard_tcode_verb {
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
    if words.iter().any(|w| GENERIC_CODE_VERBS.contains(w)) {
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

/// Count how many words in `list` appear (exact token match) in `words`.
fn count_word_matches(words: &[&str], list: &[&str]) -> usize {
    list.iter().filter(|w| words.contains(w)).count()
}

/// Count how many phrases in `list` appear as substrings of `normalized`.
fn count_phrase_matches(normalized: &str, list: &[&str]) -> usize {
    list.iter().filter(|p| normalized.contains(*p)).count()
}

#[cfg(test)]
#[path = "route_tests.rs"]
mod route_tests;

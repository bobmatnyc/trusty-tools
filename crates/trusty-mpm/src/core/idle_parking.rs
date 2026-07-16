//! Detection of agent "idle parking" — a delegated agent ending its turn to
//! "wait" for a self-spawned monitor/watcher/notification that can never
//! re-wake it (issues #2501, #2610).
//!
//! Why: prompt-level prohibitions in the bundled agent guidance reduced but did
//! NOT eliminate the failure mode where merge/CI/release agents stop mid-task
//! with "monitoring the checks" / "standing by" / "will report back once…"
//! language after spawning a background watcher. Nothing re-invokes a stopped
//! agent, so the task strands until a human resumes it. A cheap, mechanical
//! back-stop that flags a parking final message on turn-end lets the daemon /
//! dashboard surface the stall (and, later, auto-nudge it) instead of relying
//! solely on the agent obeying its instructions.
//! What: two pure, side-effect-free helpers with no I/O. [`detect_idle_parking`]
//! scans an agent's final assistant text for high-signal parking idioms and
//! returns the first phrase matched. [`last_assistant_text_from_transcript`]
//! extracts the last assistant message's concatenated text blocks from a
//! Claude Code JSONL transcript so a Stop/SubagentStop hook can feed it to the
//! detector. Both are deliberately advisory (high recall, warning-only): a
//! false positive costs a spurious warning, never a blocked or killed agent.
//! Test: the inline `tests` module below.

/// High-signal, lowercased phrases that indicate an agent is ending its turn to
/// wait on a monitor/watcher/notification that cannot re-invoke a stopped agent.
///
/// Why: these are the exact idioms observed across the #2501 (9 incidents) and
/// #2610 (6 incidents) parking episodes — merge agents "monitoring PR checks",
/// a release agent on a "background polling monitor", an engineer "waiting for
/// the monitor to report the quality gate run". The list is curated to the
/// turn-ending idiom rather than the bare words "waiting"/"monitoring" so that a
/// legitimate final message ("I ran `gh pr checks --watch` in the foreground and
/// it finished") does not trip it. It is intentionally advisory: matches drive a
/// warning, never a hard action, so erring toward recall is safe.
/// What: substrings compared against the lowercased haystack by
/// [`detect_idle_parking`]. Order is roughly most-specific first so the returned
/// phrase is the most informative match.
/// Test: `detects_each_parking_phrase`, `ignores_benign_final_message` below.
const PARKING_PHRASES: &[&str] = &[
    "background polling monitor",
    "background poll",
    "background monitor",
    "background watcher",
    "monitor in the background",
    "monitoring in the background",
    "monitoring the checks",
    "monitoring pr",
    "monitoring the pr",
    "waiting for the monitor",
    "the monitor to report",
    "monitor to report back",
    "standing by",
    "will report back",
    "will report when",
    "report back once",
    "report back when",
    "i'll report back",
    "check back later",
    "checking back later",
    "wait for the notification",
    "waiting for the notification",
    "waiting for a notification",
    "wait for a notification",
    "await the notification",
    "let me know once",
    "let me know when the",
    "notify me when",
    "ping me when",
    "i'll wait for the",
    "i will wait for the",
    "will resume once",
];

/// The subset of [`PARKING_PHRASES`] that address a HUMAN, not a machine watcher.
///
/// Why (#2621 code-critic finding, HIGH 1): "let me know once you approve the
/// deploy" and "I'll wait for the CI notification" both trip [`detect_idle_parking`]
/// — and should: both are worth a passive warning/log entry. But only the second
/// is a genuine idle-park (a stopped agent waiting on a self-spawned watcher that
/// can never re-wake it); the first is a legitimate, in-scope wait on a HUMAN
/// decision, which #2621 explicitly requires the auto-nudge to never interrupt.
/// These four phrases are the ones that name a person as the addressee ("let me
/// know", "notify me", "ping me") rather than a monitor/notification/CI system, so
/// they are excluded from nudge eligibility while remaining in the passive
/// detection/log list unchanged.
/// What: an exact-match subset of [`PARKING_PHRASES`]; see [`is_nudge_eligible`].
/// Test: `human_addressed_phrases_are_not_nudge_eligible`,
/// `machine_wait_phrases_are_nudge_eligible`.
const HUMAN_ADDRESSED_PHRASES: &[&str] = &[
    "let me know once",
    "let me know when the",
    "notify me when",
    "ping me when",
];

/// Is `phrase` eligible to trigger an auto-nudge, or only a passive warning/log?
///
/// Why: #2621's auto-nudge must "never nudge a legitimate user-wait". Detection
/// (this module) stays high-recall and advisory for every [`PARKING_PHRASES`]
/// entry — a false positive there costs only a spurious warning. Auto-injection
/// is a real action taken on a human's behalf, so it needs a STRICTER subset:
/// machine-wait phrases only, excluding [`HUMAN_ADDRESSED_PHRASES`]. Callers that
/// only log/warn (the `tm hook` stderr line, the daemon's surfacing log) should
/// use [`detect_idle_parking`]/[`detect_idle_parking_in_transcript`] directly and
/// ignore this gate; callers that actually inject text into a pane (the #2621
/// auto-nudge) must additionally require `is_nudge_eligible(phrase)`.
/// What: `true` for any phrase NOT in [`HUMAN_ADDRESSED_PHRASES`]. A phrase that
/// never matched [`PARKING_PHRASES`] at all is nonsensical input for this
/// function but still returns `true` (fail-open on the "is this human-addressed"
/// question) — callers only ever pass a phrase that `detect_idle_parking` itself
/// returned, so this case does not arise in practice.
/// Test: `human_addressed_phrases_are_not_nudge_eligible`,
/// `machine_wait_phrases_are_nudge_eligible`.
pub fn is_nudge_eligible(phrase: &str) -> bool {
    !HUMAN_ADDRESSED_PHRASES.contains(&phrase)
}

/// Scan an agent's final assistant text for idle-parking language.
///
/// Why: a delegated agent that ends its turn with "monitoring the checks" or
/// "standing by" after spawning a background watcher has stranded its task — the
/// watcher can never wake it. Detecting that phrase at turn-end (via the
/// Stop/SubagentStop hook) is a mechanical back-stop for when the bundled
/// prompt guidance fails to prevent the stall.
/// What: lowercases `final_message` once and returns the first [`PARKING_PHRASES`]
/// entry that appears as a substring, or `None` when the message is clean. Pure;
/// no allocation beyond the single lowercased copy; no I/O.
/// Test: `detects_each_parking_phrase`, `ignores_benign_final_message`,
/// `detection_is_case_insensitive`, `empty_message_is_clean`.
pub fn detect_idle_parking(final_message: &str) -> Option<&'static str> {
    if final_message.trim().is_empty() {
        return None;
    }
    let haystack = final_message.to_lowercase();
    PARKING_PHRASES
        .iter()
        .copied()
        .find(|phrase| haystack.contains(phrase))
}

/// Extract the last assistant message's text from a Claude Code JSONL transcript.
///
/// Why: the Stop / SubagentStop hook payload carries a `transcript_path`, not the
/// message text itself. To run [`detect_idle_parking`] against what the agent
/// actually said as its final turn, the hook must pull the last assistant
/// message out of the transcript. Keeping the parse here (pure, string-in /
/// string-out) makes it unit-testable without touching the filesystem.
/// What: iterates the JSONL lines from the END (the final assistant turn is near
/// the tail), skipping blank and unparseable lines. For each line whose
/// top-level `type` is `"assistant"`, concatenates the `message.content[*].text`
/// blocks (Claude Code's array-of-blocks shape) — or accepts a bare string
/// `content` for forward-compat — and returns the first non-empty result found
/// scanning backward. Returns `None` when no assistant text is present. Tolerant
/// of a truncated leading line (e.g. when only the file tail was read): a line
/// that fails to parse is simply skipped. Known tradeoff (code-critic, PR #2620):
/// the caller only hands this the last `MAX_TRANSCRIPT_TAIL` (256 KiB) bytes of
/// the transcript, so a single final JSONL line LARGER than that window (e.g. an
/// unusually long assistant turn) is itself truncated, fails to parse, and is
/// skipped like any other broken leading line — an advisory false negative by
/// design, not a bug: this detector is a best-effort warning back-stop, never a
/// hard gate, so silently missing an oversized final line is an acceptable
/// tradeoff for bounding the read cost on multi-MB session transcripts.
/// Test: `extracts_last_assistant_text`, `extracts_across_tool_use_blocks`,
/// `tolerates_truncated_and_blank_lines`, `returns_none_without_assistant`.
pub fn last_assistant_text_from_transcript(jsonl: &str) -> Option<String> {
    for line in jsonl.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if val.get("type").and_then(serde_json::Value::as_str) != Some("assistant") {
            continue;
        }
        let content = val.get("message").and_then(|m| m.get("content"));
        let text = extract_text_blocks(content);
        if !text.trim().is_empty() {
            return Some(text);
        }
    }
    None
}

/// Concatenate the `text` from a Claude Code message `content` value.
///
/// Why: assistant `content` is normally an array of typed blocks
/// (`{"type":"text","text":…}`, `{"type":"tool_use",…}`, …); only the text
/// blocks carry what the agent "said". A bare-string `content` is also accepted
/// for robustness against transcript-shape drift.
/// What: for an array, joins every element's `text` field (skipping non-text
/// blocks) with newlines; for a string, returns it directly; otherwise returns
/// an empty string.
/// Test: covered via `extracts_last_assistant_text`,
/// `extracts_across_tool_use_blocks`.
fn extract_text_blocks(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(serde_json::Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Convenience: detect idle-parking directly from a JSONL transcript.
///
/// Why: the hook call site wants a one-shot "does this agent's final turn park?"
/// answer without threading the intermediate last-message string through itself.
/// What: composes [`last_assistant_text_from_transcript`] with
/// [`detect_idle_parking`], returning the matched phrase (a `'static` slice) when
/// the last assistant turn parks, else `None`.
/// Test: `detects_parking_from_transcript`, `clean_transcript_yields_none`.
pub fn detect_idle_parking_in_transcript(jsonl: &str) -> Option<&'static str> {
    let last = last_assistant_text_from_transcript(jsonl)?;
    detect_idle_parking(&last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_each_parking_phrase() {
        for phrase in PARKING_PHRASES {
            let msg = format!("All set. {phrase} the CI completes and I'll continue.");
            assert_eq!(
                detect_idle_parking(&msg),
                Some(*phrase),
                "phrase {phrase:?} must be detected"
            );
        }
    }

    #[test]
    fn detects_observed_incident_messages() {
        // Verbatim-style samples from #2501 / #2610.
        let samples = [
            "I've kicked off a background polling monitor for the release and will \
             resume once it reports.",
            "Monitoring PR #2606 checks via a passive watcher; standing by.",
            "Waiting for the monitor to report the quality gate run.",
            "I'll report back once the checks go green.",
        ];
        for s in samples {
            assert!(
                detect_idle_parking(s).is_some(),
                "incident message must be flagged: {s:?}"
            );
        }
    }

    #[test]
    fn human_addressed_phrases_are_not_nudge_eligible() {
        // A wait on a HUMAN decision must still be detected (for the passive
        // warning/log) but must NEVER qualify for auto-nudge injection (#2621
        // code-critic HIGH 1: "never nudge a legitimate user-wait").
        let msg =
            "Deploy config is ready. Let me know once you approve the deploy and I'll ship it.";
        let phrase = detect_idle_parking(msg).expect("must still be flagged for logging");
        assert_eq!(phrase, "let me know once");
        assert!(
            !is_nudge_eligible(phrase),
            "a human-addressed phrase must not be nudge-eligible"
        );

        for phrase in HUMAN_ADDRESSED_PHRASES {
            assert!(
                !is_nudge_eligible(phrase),
                "{phrase:?} must be excluded from nudge eligibility"
            );
        }
    }

    #[test]
    fn machine_wait_phrases_are_nudge_eligible() {
        // A genuine machine/monitor wait — the actual idle-park anti-pattern —
        // must remain nudge-eligible.
        let msg = "I'll wait for the CI notification before continuing.";
        let phrase = detect_idle_parking(msg).expect("must be flagged");
        assert_eq!(phrase, "i'll wait for the");
        assert!(
            is_nudge_eligible(phrase),
            "a machine-wait phrase must remain nudge-eligible"
        );

        for phrase in PARKING_PHRASES {
            if HUMAN_ADDRESSED_PHRASES.contains(phrase) {
                continue;
            }
            assert!(
                is_nudge_eligible(phrase),
                "{phrase:?} is not human-addressed and must be nudge-eligible"
            );
        }
    }

    #[test]
    fn ignores_benign_final_message() {
        let clean = [
            "I ran `gh pr checks 2606 --watch --fail-fast` in the foreground; all \
             12 checks passed. PR is green.",
            "cargo test → test result: ok. 68 passed; 0 failed. Done.",
            "Fixed the race condition in the async handler and verified the build.",
            "",
            "   ",
        ];
        for c in clean {
            assert_eq!(detect_idle_parking(c), None, "must be clean: {c:?}");
        }
    }

    #[test]
    fn detection_is_case_insensitive() {
        assert_eq!(detect_idle_parking("STANDING BY."), Some("standing by"));
        assert_eq!(
            detect_idle_parking("Will Report Back when it finishes"),
            Some("will report back")
        );
    }

    #[test]
    fn empty_message_is_clean() {
        assert_eq!(detect_idle_parking(""), None);
        assert_eq!(detect_idle_parking("\n\t  "), None);
    }

    #[test]
    fn extracts_last_assistant_text() {
        let jsonl = concat!(
            r#"{"type":"user","message":{"content":[{"type":"text","text":"go"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"first"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"final answer"}]}}"#,
            "\n",
        );
        assert_eq!(
            last_assistant_text_from_transcript(jsonl).as_deref(),
            Some("final answer")
        );
    }

    #[test]
    fn extracts_across_tool_use_blocks() {
        // Last assistant turn mixes a text block and a tool_use block; only the
        // text must surface.
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"standing by"},{"type":"tool_use","name":"Bash","input":{}}]}}"#,
            "\n",
        );
        let text = last_assistant_text_from_transcript(jsonl).expect("has assistant text");
        assert_eq!(text, "standing by");
        assert_eq!(detect_idle_parking(&text), Some("standing by"));
    }

    #[test]
    fn tolerates_truncated_and_blank_lines() {
        // A tail-read transcript can start with a truncated (invalid-JSON) line
        // and contain blank lines; both must be skipped without error.
        let jsonl = concat!(
            r#"pe":"assistant","message":{"content":[{"type":"text","text":"truncated"}"#,
            "\n",
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"monitoring the checks"}]}}"#,
            "\n",
            "   \n",
        );
        assert_eq!(
            detect_idle_parking_in_transcript(jsonl),
            Some("monitoring the checks")
        );
    }

    #[test]
    fn returns_none_without_assistant() {
        let jsonl = concat!(
            r#"{"type":"user","message":{"content":[{"type":"text","text":"waiting for the notification"}]}}"#,
            "\n",
            r#"{"type":"system","message":{"content":"standing by"}}"#,
            "\n",
        );
        // No assistant turn → nothing to detect, even though user/system text
        // contains parking words (we must never flag the user's own words).
        assert_eq!(last_assistant_text_from_transcript(jsonl), None);
        assert_eq!(detect_idle_parking_in_transcript(jsonl), None);
    }

    #[test]
    fn detects_parking_from_transcript() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"I'll report back once green."}]}}"#,
            "\n",
        );
        assert_eq!(
            detect_idle_parking_in_transcript(jsonl),
            Some("report back once")
        );
    }

    #[test]
    fn clean_transcript_yields_none() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"All checks passed. Merged."}]}}"#,
            "\n",
        );
        assert_eq!(detect_idle_parking_in_transcript(jsonl), None);
    }

    #[test]
    fn bare_string_content_is_supported() {
        let jsonl = r#"{"type":"assistant","message":{"content":"standing by for CI"}}"#;
        assert_eq!(
            last_assistant_text_from_transcript(jsonl).as_deref(),
            Some("standing by for CI")
        );
    }
}

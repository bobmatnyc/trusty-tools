//! Structured skill ratings parsed from observe-agent output (#174).
//!
//! Why: After every workflow run, the observe-agent emits a `## Skill Ratings`
//! block where it scores each injected skill on its actual contribution to the
//! task outcome. The engine parses these structured ratings and feeds them
//! back into the persistent `SkillRegistry` via `update_effectiveness`,
//! closing the feedback loop so skill rankings improve based on real outcomes
//! rather than coarse pass/fail signals alone.
//! What: `SkillRating` is the JSON-deserializable record per rated skill;
//! `parse_skill_ratings` walks observe output, locates the `## Skill Ratings`
//! section, parses each subsequent line as JSON, and skips malformed lines
//! gracefully (logged at DEBUG).
//! Test: `parse_skill_ratings_extracts_json_lines`,
//! `parse_skill_ratings_ignores_malformed_lines`,
//! `parse_skill_ratings_returns_empty_when_no_section`.

use serde::Deserialize;

/// One observe-agent rating for a single skill that was injected into the run.
///
/// Why: The registry's `update_effectiveness` call needs `(name, score)`; the
/// `reason` field is captured for future audit/log surfaces but is not yet
/// persisted, keeping the schema stable while keeping the door open for
/// richer analytics.
/// What: `score` is expected in `[0.0, 1.0]` (0 = harmful/wrong,
/// 0.5 = neutral, 1 = exactly right). Out-of-range scores are clamped by
/// `update_effectiveness` downstream, so this struct stays permissive.
/// Test: `parse_skill_ratings_extracts_json_lines`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SkillRating {
    pub skill: String,
    pub score: f32,
    #[serde(default)]
    pub reason: String,
}

/// Coarse effectiveness signal derived from a run status string (#174 fallback).
///
/// Why: When the observe-agent did not emit a structured ratings block (older
/// agent prompts, observe phase skipped, parse failure) we still want to feed
/// *some* signal back into the EMA so persistence is never a no-op for runs
/// that injected skills. Centralizing the mapping keeps the fallback policy
/// in one testable place.
/// What: `success` → 0.8, `partial` → 0.5, anything else (including
/// `failed`) → 0.3. Scores are deliberately not 1.0/0.0 because a coarse
/// signal shouldn't push the score as hard as a per-skill rating.
/// Test: `coarse_fallback_applied_when_no_ratings_block`.
pub fn coarse_fallback_signal(status: &str) -> f32 {
    match status {
        "success" => 0.8,
        "partial" => 0.5,
        _ => 0.3,
    }
}

/// Parse the `## Skill Ratings` block from observe-agent output.
///
/// Why: Observe output is freeform Markdown plus an appended structured block;
/// callers want a typed list of ratings without writing their own parser. A
/// graceful, line-by-line parse means stray prose or trailing commentary
/// after the block can't break the feedback loop.
/// What: Locates the first line equal to `## Skill Ratings` (trimmed), then
/// reads each following line. JSON-object lines deserialize into `SkillRating`
/// records; lines that fail to parse are logged at DEBUG and skipped. Stops
/// at the next Markdown heading (line starting with `#`) or at EOF. Returns
/// an empty `Vec` when the heading is not present.
/// Test: `parse_skill_ratings_extracts_json_lines`,
/// `parse_skill_ratings_ignores_malformed_lines`,
/// `parse_skill_ratings_returns_empty_when_no_section`.
pub fn parse_skill_ratings(observe_output: &str) -> Vec<SkillRating> {
    let mut ratings = Vec::new();
    let mut in_section = false;

    for raw in observe_output.lines() {
        let line = raw.trim();
        if !in_section {
            if line == "## Skill Ratings" {
                in_section = true;
            }
            continue;
        }

        // Empty lines inside the block are harmless separators.
        if line.is_empty() {
            continue;
        }

        // A new heading terminates the ratings block.
        if line.starts_with('#') {
            break;
        }

        // Only attempt to parse lines that look like JSON objects.
        if !line.starts_with('{') {
            tracing::debug!(line = %line, "skill ratings: skipping non-JSON line");
            continue;
        }

        match serde_json::from_str::<SkillRating>(line) {
            Ok(rating) => ratings.push(rating),
            Err(e) => {
                tracing::debug!(
                    line = %line,
                    error = %e,
                    "skill ratings: skipping malformed JSON line"
                );
            }
        }
    }

    ratings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_skill_ratings_extracts_json_lines() {
        let output = r#"# Workflow Report

Some narrative here.

## Skill Ratings
{"skill":"frameworks/fastapi.md","score":0.9,"reason":"lifespan pattern accurate"}
{"skill":"frameworks/pytest.md","score":0.6,"reason":"async fixture guidance incomplete"}
"#;
        let ratings = parse_skill_ratings(output);
        assert_eq!(ratings.len(), 2);
        assert_eq!(ratings[0].skill, "frameworks/fastapi.md");
        assert!((ratings[0].score - 0.9).abs() < 1e-6);
        assert_eq!(ratings[0].reason, "lifespan pattern accurate");
        assert_eq!(ratings[1].skill, "frameworks/pytest.md");
        assert!((ratings[1].score - 0.6).abs() < 1e-6);
    }

    #[test]
    fn parse_skill_ratings_ignores_malformed_lines() {
        let output = r#"## Skill Ratings
{"skill":"a.md","score":0.8,"reason":"good"}
this is not json
{"skill": malformed json
{"skill":"b.md","score":0.4,"reason":"meh"}
"#;
        let ratings = parse_skill_ratings(output);
        assert_eq!(ratings.len(), 2, "only valid JSON lines should be kept");
        assert_eq!(ratings[0].skill, "a.md");
        assert_eq!(ratings[1].skill, "b.md");
    }

    #[test]
    fn parse_skill_ratings_returns_empty_when_no_section() {
        let output = "# Workflow Report\n\nNo ratings here at all.\n";
        let ratings = parse_skill_ratings(output);
        assert!(ratings.is_empty());
    }

    #[test]
    fn parse_skill_ratings_stops_at_next_heading() {
        let output = r#"## Skill Ratings
{"skill":"a.md","score":0.7,"reason":"ok"}

## Next Steps
- something else
{"skill":"should-not-parse.md","score":0.0,"reason":"after heading"}
"#;
        let ratings = parse_skill_ratings(output);
        assert_eq!(ratings.len(), 1);
        assert_eq!(ratings[0].skill, "a.md");
    }

    #[test]
    fn coarse_fallback_applied_when_no_ratings_block() {
        // Sanity: the coarse fallback maps statuses into the documented bands.
        // Why: Locks the policy so future refactors don't silently change the
        // signal magnitude and skew long-lived effectiveness scores.
        // What: success → 0.8, partial → 0.5, fail/unknown → 0.3.
        // Test: this test.
        assert!((coarse_fallback_signal("success") - 0.8).abs() < 1e-6);
        assert!((coarse_fallback_signal("partial") - 0.5).abs() < 1e-6);
        assert!((coarse_fallback_signal("failed") - 0.3).abs() < 1e-6);
        assert!((coarse_fallback_signal("anything else") - 0.3).abs() < 1e-6);

        // And: parsing observe output that contains no ratings block returns
        // an empty Vec — the call site uses `is_empty()` to choose the
        // fallback path, so this is the contract that drives that branch.
        let output = "# Workflow Report\n\nNarrative only.\n";
        assert!(parse_skill_ratings(output).is_empty());
    }

    #[test]
    fn parse_skill_ratings_handles_missing_reason() {
        let output = "## Skill Ratings\n{\"skill\":\"a.md\",\"score\":0.5}\n";
        let ratings = parse_skill_ratings(output);
        assert_eq!(ratings.len(), 1);
        assert_eq!(ratings[0].reason, "");
    }
}

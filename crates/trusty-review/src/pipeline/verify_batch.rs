//! Batched, capped verifier requests (#8904).
//!
//! Why: before #8904 the verifier saw only findings that could change the
//! verdict, and on trusty-review 0.36.1 it ran in 1 of 10 reviews, so 7 of 7
//! fabricated findings were posted unchecked. Verifying every finding costs one
//! diff-carrying call per finding; batching several findings into one call pays
//! for the diff once, and a per-review call cap bounds the total spend.
//! What: [`plan_batches`] orders the candidates by impact, chunks them
//! into batches of at most `batch_size`, and splits off every finding past
//! `max_calls` batches; [`build_batch_request`] and [`batch_response_schema`]
//! ask for one judgment per numbered finding; [`parse_batch_judgments`] reads
//! them back; [`file_diff_slice`] cuts one file's sections out of a diff for
//! the map-reduce path, whose raw diff is over the reviewer's size cap.
//! Test: `verify_batch_tests.rs`.

use std::collections::HashMap;

use serde::Deserialize;

use crate::{
    llm::{LlmRequest, ResponseSchema},
    models::{Effort, Finding},
    pipeline::{
        citation_check::normalize_path,
        verify_prompt::{
            VERIFY_MAX_TOKENS, VERIFY_TEMPERATURE, diff_and_rationale, finding_fields,
            verifier_request, verifier_system_prompt,
        },
    },
};

/// Heading that opens each finding in a batched request.
///
/// Why: the batched answer names findings by this number, and a test fake
/// splits a request on it to answer each finding separately.
pub const BATCH_FINDING_HEADING: &str = "### Finding ";

/// Name of the batched verifier's forced-output schema.
pub const VERIFY_BATCH_SCHEMA_NAME: &str = "verification_judgments";

/// The verifier requests one review will make, and the findings left over.
///
/// What: `batches` holds indices into the findings slice, highest-impact
/// batch first; `over_cap` holds every candidate that did not fit.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BatchPlan {
    /// One entry per verifier request.
    pub batches: Vec<Vec<usize>>,
    /// Candidates past the `max_calls` cap; the caller withholds them.
    pub over_cap: Vec<usize>,
}

/// Group `candidates` into at most `max_calls` batches (#8904).
///
/// Why: the cap must cut the findings whose loss costs least.
/// What: sorts candidates by effort (High first), then confidence, then
/// position, and chunks them into batches of `batch_size`. Batches past
/// `max_calls` move to `over_cap`. Both counts are clamped to ≥ 1.
/// Test: `plan_batches_chunks_by_impact_and_caps_calls`,
/// `plan_batches_cuts_the_lowest_impact_findings`.
pub fn plan_batches(
    findings: &[Finding],
    candidates: &[usize],
    batch_size: usize,
    max_calls: usize,
) -> BatchPlan {
    let (batch_size, max_calls) = (batch_size.max(1), max_calls.max(1));
    let mut ordered = candidates.to_vec();
    ordered.sort_by(|&a, &b| {
        let (fa, fb) = (&findings[a], &findings[b]);
        effort_rank(&fb.effort)
            .cmp(&effort_rank(&fa.effort))
            .then(fb.confidence.total_cmp(&fa.confidence))
            .then(a.cmp(&b))
    });
    let mut batches: Vec<Vec<usize>> = ordered.chunks(batch_size).map(<[usize]>::to_vec).collect();
    let over_cap = if batches.len() > max_calls {
        batches.split_off(max_calls).into_iter().flatten().collect()
    } else {
        Vec::new()
    };
    BatchPlan { batches, over_cap }
}

fn effort_rank(effort: &Effort) -> u8 {
    match effort {
        Effort::Low => 0,
        Effort::Medium => 1,
        Effort::High => 2,
    }
}

/// The sections of `diff` that change any of `files`, or `None` when one of
/// them matches no section.
///
/// Why: the map-reduce path reviews diffs over the reviewer's size cap, and
/// sending that whole diff with every batch would overrun the verifier's
/// context and multiply its cost; each map call saw only its own file.
/// What: keeps every `diff --git` section whose header names one of `files`
/// (after stripping `a/`/`b/`). `None` lets the caller fall back to the whole
/// diff rather than send a batch without its evidence.
/// Test: `file_diff_slice_keeps_only_the_named_files`.
pub fn file_diff_slice<'a>(diff: &str, files: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut want: HashMap<String, bool> = files
        .into_iter()
        .map(|f| (normalize_path(f), false))
        .collect();
    let mut out = String::new();
    let mut keep = false;
    for line in diff.split_inclusive('\n') {
        if let Some(header) = line.strip_prefix("diff --git ") {
            keep = false;
            for path in header.split_whitespace() {
                if let Some(seen) = want.get_mut(&normalize_path(path)) {
                    *seen = true;
                    keep = true;
                }
            }
        }
        if keep {
            out.push_str(line);
        }
    }
    want.values().all(|seen| *seen).then_some(out)
}

/// Build one verifier request that judges every finding in `batch` (#8904).
///
/// Why: one diff per call instead of one per finding.
/// What: the single-finding system prompt plus a batch section, the shared
/// diff/rationale block, each finding under `### Finding N`, and the
/// [`batch_response_schema`]. `max_tokens` is the per-finding budget times the
/// batch length, so a full batch is not truncated.
/// Test: `batch_request_numbers_every_finding`.
pub fn build_batch_request(
    verifier_model: &str,
    diff: &str,
    batch: &[&Finding],
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    author_rationale: Option<&str>,
) -> LlmRequest {
    let mut user = diff_and_rationale(diff, author_rationale);
    user.push_str("## Findings to verify\n\n");
    for (i, finding) in batch.iter().enumerate() {
        user.push_str(&format!(
            "{BATCH_FINDING_HEADING}{}\n{}\n",
            i + 1,
            finding_fields(finding)
        ));
    }
    user.push_str(
        "Decide CONFIRMED, REFUTED, or UNVERIFIABLE for EACH finding per the rules in the \
         system prompt. A finding whose file or line does not appear in the diff above is \
         REFUTED. A finding resting on evidence outside the diff is UNVERIFIABLE, not \
         CONFIRMED.",
    );
    let system = format!(
        "{}\n\n## Batched findings\nThis request carries {} numbered findings instead of \
         one. Judge EACH as if it were the only finding: a judgment on one never informs \
         another. Return one `judgments` entry per finding, with `finding` set to its number.",
        verifier_system_prompt(),
        batch.len()
    );
    let per_finding = max_tokens.unwrap_or(VERIFY_MAX_TOKENS);
    let budget = per_finding.saturating_mul(u32::try_from(batch.len()).unwrap_or(u32::MAX));
    verifier_request(
        verifier_model,
        system,
        user,
        temperature.unwrap_or(VERIFY_TEMPERATURE),
        budget,
        batch_response_schema(),
    )
}

/// Forced-output schema for a batched verifier call (#8904).
///
/// What: `{judgments: [{finding, judgment, reason}]}` with `judgment` in
/// CONFIRMED / REFUTED / UNVERIFIABLE; made strict by `ResponseSchema::new`.
/// Test: `every_sent_schema_is_openai_strict_compliant`.
pub fn batch_response_schema() -> ResponseSchema {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "judgments": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "finding": {"type": "integer", "description": "The finding's number"},
                        "judgment": {
                            "type": "string",
                            "enum": ["CONFIRMED", "REFUTED", "UNVERIFIABLE"]
                        },
                        "reason": {"type": "string", "description": "One short sentence"}
                    }
                }
            }
        }
    });
    ResponseSchema::new(VERIFY_BATCH_SCHEMA_NAME, schema)
}

#[derive(Deserialize)]
struct BatchReply {
    judgments: Vec<BatchEntry>,
}

#[derive(Deserialize)]
struct BatchEntry {
    finding: usize,
    judgment: String,
}

/// Read a batched answer as one judgment string per finding (#8904).
///
/// Why: a finding the answer does not judge must fail closed, not inherit a
/// neighbour's judgment.
/// What: returns `n` entries, 1-based `finding` N at index N-1. An entry is
/// `None` when the reply does not parse, names no such finding, or judges it
/// twice with different answers. The first judgment of a finding wins when
/// repeats agree.
/// Test: `parse_batch_judgments_fails_closed_per_missing_finding`.
pub fn parse_batch_judgments(text: &str, n: usize) -> Vec<Option<String>> {
    let mut out: Vec<Option<String>> = vec![None; n];
    let Ok(reply) = serde_json::from_str::<BatchReply>(text.trim()) else {
        return out;
    };
    let mut conflicted = vec![false; n];
    for entry in reply.judgments {
        let Some(slot) = entry.finding.checked_sub(1).filter(|i| *i < n) else {
            continue;
        };
        let judgment = entry.judgment.trim().to_uppercase();
        match &out[slot] {
            Some(prev) if *prev != judgment => conflicted[slot] = true,
            Some(_) => {}
            None => out[slot] = Some(judgment),
        }
    }
    for (slot, bad) in out.iter_mut().zip(conflicted) {
        if bad {
            *slot = None;
        }
    }
    out
}

/// Test fakes answer single and batched verifier requests through this (#8904).
#[cfg(test)]
pub(crate) mod test_support {
    use super::{BATCH_FINDING_HEADING, VERIFY_BATCH_SCHEMA_NAME};
    use crate::llm::LlmRequest;

    /// Answer `req` the way a verifier would, judging each finding's section
    /// of the request with `judge`.
    ///
    /// What: a batched request (its schema is the batch schema) gets
    /// `{"judgments":[…]}` with one entry per `### Finding N` section; a
    /// single-finding request gets `{"judgment":…}` judged on the whole message.
    pub(crate) fn answer(req: &LlmRequest, judge: impl Fn(&str) -> String) -> String {
        let user = req.messages.first().map_or("", |m| m.content.as_str());
        let batched = req
            .response_schema
            .as_ref()
            .is_some_and(|s| s.name == VERIFY_BATCH_SCHEMA_NAME);
        if !batched {
            return format!(r#"{{"judgment":"{}","reason":"test"}}"#, judge(user));
        }
        let entries: Vec<String> = user
            .split(BATCH_FINDING_HEADING)
            .skip(1)
            .enumerate()
            .map(|(i, section)| {
                format!(
                    r#"{{"finding":{},"judgment":"{}","reason":"test"}}"#,
                    i + 1,
                    judge(section)
                )
            })
            .collect();
        format!(r#"{{"judgments":[{}]}}"#, entries.join(","))
    }
}

#[cfg(test)]
#[path = "verify_batch_tests.rs"]
mod tests;

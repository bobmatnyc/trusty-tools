//! Prompt builder for the synthesiser LLM call.
//!
//! Why: extracted from `synthesizer.rs` to keep that file under the 500-line
//! cap without losing any public API.
//! What: provides `build_synthesizer_prompt`, the system prompt, and the user
//! message builder.  All constants are imported from the parent module.
//! Test: covered transitively by `synthesizer::tests`.

use std::cmp::Reverse;

use crate::llm::{ChatMessage, LlmRequest, strip_provider_prefix};
use crate::profile::types::ContributorProfile;

// Constants imported from parent.
use super::{SYNTHESIZER_MAX_TOKENS, SYNTHESIZER_TEMPERATURE};

/// Build the LLM request for the synthesiser (narrative + strengths/weaknesses).
///
/// Why: the narrative pass needs the full deduped finding list, frequency counts,
/// and quality score series to produce a coherent longitudinal summary.
/// What: assembles a system prompt (profiler role + JSON schema) and a user
/// message with the finding summary table and quality trend series.  `model`
/// may carry a `bedrock/` or `openrouter/` routing prefix; this function strips
/// it so the bare id reaches the provider API.
/// Test: `synthesizer::tests::synthesizer_applies_llm_result`,
/// `synthesizer::tests::synthesizer_prompt_strips_bedrock_prefix`.
pub fn build_synthesizer_prompt(profile: &ContributorProfile, model: &str) -> LlmRequest {
    let system = synthesizer_system_prompt();
    let user = build_synthesizer_user_message(profile);
    LlmRequest {
        model: strip_provider_prefix(model).to_string(),
        system: system.to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: user,
        }],
        temperature: SYNTHESIZER_TEMPERATURE,
        max_tokens: SYNTHESIZER_MAX_TOKENS,
    }
}

pub(super) fn synthesizer_system_prompt() -> &'static str {
    r#"You are a senior engineering lead producing a longitudinal code-quality profile for a contributor.

## Task
Given a list of recurring code-quality findings across multiple time periods and a quality score trend,
write a concise, actionable engineering profile. Be direct and specific.

## Output format (REQUIRED)
End your response with EXACTLY ONE JSON block in this schema.
Do NOT include any text after the JSON block.

```json
{
  "strengths": ["List of 2–4 specific strengths observed across the periods"],
  "recurring_weaknesses": ["List of 2–4 specific recurring weaknesses or areas for improvement"],
  "improvement_trajectory": "improving|stable|declining",
  "narrative": "2–4 paragraph engineering assessment suitable for a manager review."
}
```

`improvement_trajectory` must be one of: improving, stable, declining.
Emit the raw JSON block with no additional prose after it."#
}

pub(super) fn build_synthesizer_user_message(profile: &ContributorProfile) -> String {
    let mut msg = String::with_capacity(4096);

    msg.push_str(&format!(
        "## Contributor: {} <{}>\n",
        profile.canonical_name, profile.canonical_email
    ));
    msg.push_str(&format!(
        "Profile window: {} → {}\n",
        profile.profiled_since, profile.profiled_until
    ));
    if !profile.repositories.is_empty() {
        msg.push_str(&format!(
            "Repositories: {}\n",
            profile.repositories.join(", ")
        ));
    }
    msg.push('\n');

    // Quality trend table.
    msg.push_str("### Quality trend\n\n");
    msg.push_str("| Period | Score |\n|--------|-------|\n");
    for (label, score) in &profile.quality_trend {
        msg.push_str(&format!("| {label} | {score:.2} |\n"));
    }
    msg.push('\n');

    // Finding summary.
    if profile.all_findings.is_empty() {
        msg.push_str("### Findings\n*(no findings extracted)*\n\n");
    } else {
        msg.push_str("### Findings across all periods\n\n");

        let mut kind_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for lf in &profile.all_findings {
            *kind_counts.entry(lf.finding.kind.as_str()).or_default() += 1;
        }
        let mut kinds: Vec<(&str, usize)> = kind_counts.into_iter().collect();
        kinds.sort_by_key(|b| Reverse(b.1));

        msg.push_str("**Finding frequency by kind:**\n");
        for (kind, count) in &kinds {
            msg.push_str(&format!("- {kind}: {count}×\n"));
        }
        msg.push('\n');

        msg.push_str("**Sample findings with trend tags:**\n");
        for lf in profile.all_findings.iter().take(20) {
            let tag = lf
                .trend_tag
                .as_ref()
                .map(|t| format!("{t:?}"))
                .unwrap_or_else(|| "Unknown".to_string());
            msg.push_str(&format!(
                "- [{tag}] ({}) {}: {}\n",
                lf.period_label, lf.finding.kind, lf.finding.description
            ));
        }
        msg.push('\n');
    }

    msg.push_str(&format!(
        "Deterministic trajectory: {}\n\n",
        format!("{:?}", profile.improvement_trajectory).to_lowercase()
    ));

    msg.push_str(
        "Please synthesise the above data into a longitudinal engineering profile \
         and end your response with the structured JSON block.\n",
    );

    msg
}

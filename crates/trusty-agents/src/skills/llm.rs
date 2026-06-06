//! LLM-backed skill selection and its result cache (#363 split from
//! `skills/mod.rs`).
//!
//! Why: Keyword matching over-fires and under-recalls; a cheap one-shot LLM
//! call ranks skills by semantic relevance. Results are memoized on a hash of
//! `(task_prefix, skill_index)` so repeated phases skip the round-trip.
//! What: Defines the module-level selection cache, the `skill_llm_enabled`
//! feature flag, `select_skills_via_llm`, and the JSON-array response parser.
//! Test: `llm_skill_selection_parses_json_array`,
//! `llm_skill_cache_hit_skips_llm_call`, `compute_cache_key_is_stable`.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

/// Module-level cache for LLM skill selection results.
///
/// Why: LLM calls are expensive (latency + tokens). Many phases re-invoke skill
/// selection with the same task prefix and skill index, so memoizing on a hash
/// of `(task_prefix_512, skill_index)` saves repeated round-trips.
/// What: Lazy-initialized `Mutex<HashMap<u64, Vec<String>>>`; key is the hash of
/// the cache key string, value is the previously selected skill names.
/// Test: Indirect — `select_skills_via_llm` checks this map before any IO.
pub(super) fn llm_skill_cache() -> &'static std::sync::Mutex<HashMap<u64, Vec<String>>> {
    static CACHE: OnceLock<std::sync::Mutex<HashMap<u64, Vec<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Compute a stable cache key hash from `(task_prefix, skill_index)`.
pub(super) fn compute_cache_key(task: &str, skill_index: &str) -> u64 {
    let prefix_len = 512.min(task.len());
    // Slice on a UTF-8 boundary by walking back to the nearest char boundary.
    let mut boundary = prefix_len;
    while boundary > 0 && !task.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let task_prefix = &task[..boundary];
    let combined = format!("{task_prefix}{skill_index}");
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    combined.hash(&mut hasher);
    hasher.finish()
}

/// Check whether the LLM-based skill selector is enabled.
///
/// Why: The feature is opt-in until validated; default-off avoids unexpected
/// LLM spend and preserves existing keyword-matching behavior.
/// What: Returns true when env var `TAGENT_SKILL_LLM=1` is set.
/// Test: Set the var, call this, assert true; unset, call, assert false.
pub fn skill_llm_enabled() -> bool {
    crate::env_compat::env_var("TAGENT_SKILL_LLM", "OPEN_MPM_SKILL_LLM").unwrap_or_default() == "1"
}

/// Ask `claude-haiku-4-5` (via OpenRouter) which skills are relevant to `task`.
///
/// Why: Keyword matching is brittle — it both over-fires on incidental words
/// (e.g. "test" inside a non-testing context) and misses paraphrased task
/// descriptions. A cheap LLM call can rank skills by semantic relevance.
/// What: Builds a JSON-array-only prompt with task + available skill list,
/// calls the model at temperature 0.0 with max_tokens=200, parses the response
/// as a JSON array of strings, and filters down to names that exist in
/// `available_skills`.
/// Test: `llm_skill_selection_parses_json_array`.
pub async fn select_skills_via_llm(
    task: &str,
    available_skills: &[(String, String, Vec<String>)],
    client: &async_openai::Client<async_openai::config::OpenAIConfig>,
    max_skills: usize,
) -> anyhow::Result<Vec<String>> {
    let formatted_skill_list = available_skills
        .iter()
        .map(|(name, desc, tags)| {
            let desc_disp = if desc.is_empty() {
                "(no description)"
            } else {
                desc
            };
            format!("- **{name}**: {desc_disp} [tags: {}]", tags.join(", "))
        })
        .collect::<Vec<_>>()
        .join("\n");

    let system_prompt = format!(
        "You are a skill selector for an AI coding agent. Given a task description \
         and a list of available skills, return a JSON array of skill names most \
         relevant to completing the task. Respond with ONLY a valid JSON array of \
         strings, e.g. [\"rust\", \"tdd\"]. Select 0 to {max_skills} skills. Prefer \
         precision over recall — only include clearly relevant skills."
    );

    let user_message = format!(
        "TASK:\n{task}\n\nAVAILABLE SKILLS:\n{formatted_skill_list}\n\n\
         Select up to {max_skills} relevant skill names from the list above."
    );

    let response = crate::llm::chat(
        client,
        "anthropic/claude-haiku-4-5",
        &system_prompt,
        &user_message,
        0.0,
        200,
        Vec::new(),
    )
    .await?;

    let raw = response
        .content
        .ok_or_else(|| anyhow::anyhow!("LLM returned no content"))?;

    let parsed = parse_skill_selection_response(&raw)?;
    let valid_names: std::collections::HashSet<&str> = available_skills
        .iter()
        .map(|(n, _, _)| n.as_str())
        .collect();
    let filtered: Vec<String> = parsed
        .into_iter()
        .filter(|n| valid_names.contains(n.as_str()))
        .take(max_skills)
        .collect();

    Ok(filtered)
}

/// Parse the LLM response as a JSON array of strings.
///
/// Why: Models sometimes wrap JSON in code fences or prefix it with prose; we
/// scan for the first `[` and parse the JSON array starting there to be robust.
/// What: Strips ``` fences if present, finds the first `[` and last `]`, parses
/// as `Vec<String>`. Returns the parse error on failure.
/// Test: `llm_skill_selection_parses_json_array`,
/// `llm_skill_selection_strips_code_fences`.
pub(super) fn parse_skill_selection_response(raw: &str) -> anyhow::Result<Vec<String>> {
    let trimmed = raw.trim();
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim_start_matches('\n'))
        .and_then(|s| s.strip_suffix("```"))
        .map(|s| s.trim())
        .unwrap_or(trimmed);

    let start = stripped
        .find('[')
        .ok_or_else(|| anyhow::anyhow!("no JSON array found in LLM response: {stripped}"))?;
    let end = stripped
        .rfind(']')
        .ok_or_else(|| anyhow::anyhow!("unterminated JSON array in LLM response: {stripped}"))?;
    if end < start {
        anyhow::bail!("malformed JSON array in LLM response: {stripped}");
    }
    let array_text = &stripped[start..=end];
    let parsed: Vec<String> = serde_json::from_str(array_text).map_err(|e| {
        anyhow::anyhow!("failed to parse skill selection JSON: {e} — text: {array_text}")
    })?;
    Ok(parsed)
}
